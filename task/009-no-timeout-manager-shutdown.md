# Issue: No Timeout on Manager Shutdown

## Location
`src/server.rs:136-154`

## Severity
High - Can cause indefinite hangs

## Description
Manager shutdown has no timeout. If any manager hangs, the entire shutdown process hangs:

```rust
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(2000)).await;
    log::debug!("Starting manager shutdown");
    if let Err(e) = managers.shutdown().await {
        log::error!("Failed to shutdown managers: {e}");
    }
})
```

## Problem
If a `ShutdownHook::shutdown()` implementation hangs:
- The `managers.shutdown().await` never completes
- The `completion_tx.send(())` never fires
- `wait_for_completion()` in lib.rs:147 times out
- But the hung manager task continues running!

## Real-World Hang Scenarios

### Browser Manager
```rust
// BrowserManager tries to gracefully close Chrome
// Chrome is frozen/unresponsive
// close() hangs waiting for process to exit
// → Entire shutdown hangs
```

### Database Connection Pool
```rust
// DatabaseManager drains connection pool
// One connection has a long-running query
// drain() waits for query to complete
// → Shutdown hangs
```

### Network Tunnel
```rust
// TunnelGuard tries to close SSH tunnel
// SSH process is in uninterruptible sleep (D state)
// close() hangs in kernel
// → Shutdown hangs indefinitely
```

### External Service Call
```rust
// Manager makes shutdown notification to external service
// Service is down, TCP retries for minutes
// → Shutdown hangs
```

## Impact
1. **Zombie processes**: Server appears stopped but resources still held
2. **Container orchestration issues**: Kubernetes/Docker waits, then SIGKILL
3. **Resource exhaustion**: Ports/files/memory not released
4. **Cascading failures**: Dependent services can't start because ports are held

## Current Behavior
```
User: Ctrl+C
System: Shutdown signal received
System: Waits 30 seconds (user timeout)
System: "Shutdown timeout elapsed"
Result: Process exits BUT hung manager task still running in background
```

## Why This Is Serious
Tokio tasks outlive the "main" shutdown flow. Even though `wait_for_completion` times out, the spawned task with the hung manager continues running until:
- The process is SIGKILL'd
- The task eventually unblocks (could be never)

## Recommendation

### Option 1: Individual Manager Timeouts
```rust
pub async fn shutdown(&self) -> Result<()> {
    log::info!("Shutting down {} managers", self.shutdown_hooks.len());

    let timeout_per_manager = Duration::from_secs(10);

    for (i, hook) in self.shutdown_hooks.iter().enumerate() {
        match tokio::time::timeout(timeout_per_manager, hook.shutdown()).await {
            Ok(Ok(())) => {
                log::debug!("Manager {} shut down successfully", i);
            }
            Ok(Err(e)) => {
                log::error!("Manager {} shutdown failed: {}", i, e);
            }
            Err(_) => {
                log::error!("Manager {} shutdown timed out after {:?}", i, timeout_per_manager);
            }
        }
    }

    Ok(())
}
```

### Option 2: Total Manager Timeout
```rust
tokio::spawn(async move {
    ct_clone.cancelled().await;

    // ... HTTP shutdown ...

    let manager_timeout = Duration::from_secs(10);
    match tokio::time::timeout(manager_timeout, managers.shutdown()).await {
        Ok(Ok(())) => log::debug!("Managers shut down successfully"),
        Ok(Err(e)) => log::error!("Manager shutdown failed: {}", e),
        Err(_) => log::error!("Manager shutdown timed out after {:?}", manager_timeout),
    }

    let _ = completion_tx.send(());
});
```

### Option 3: Abort on Timeout
For critical systems, force-kill hung tasks:
```rust
let manager_task = tokio::spawn(async move {
    managers.shutdown().await
});

match tokio::time::timeout(Duration::from_secs(10), manager_task).await {
    Ok(Ok(Ok(()))) => log::debug!("Clean shutdown"),
    Ok(Ok(Err(e))) => log::error!("Shutdown error: {}", e),
    Ok(Err(_)) => log::error!("Manager task panicked"),
    Err(_) => {
        log::error!("Manager shutdown timed out - aborting task");
        manager_task.abort();
    }
}
```
