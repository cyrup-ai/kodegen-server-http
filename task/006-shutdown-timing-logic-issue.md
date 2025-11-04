# Shutdown Timing Logic Issue

## Issue Type
Logic / Race Condition

## Severity
Medium

## Location
`src/server.rs:134-150`

## Description
The shutdown sequence has a potential timing issue where the total shutdown time is not properly bounded by the user-configured timeout.

## Current Shutdown Sequence

```rust
// t=0: Cancellation triggered
ct_clone.cancelled().await;

// t=0: Manager shutdown task spawned (waits 2s before starting)
let managers_shutdown = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(2000)).await;  // t=0 to t=2s
    managers.shutdown().await  // t=2s to t=???
});

// t=0: HTTP shutdown initiated (20s timeout)
shutdown_handle.graceful_shutdown(Some(Duration::from_secs(20)));

// t=0 to t=???: Wait for server task
let _ = server_task.await;

// t=??? to t=???: Wait for managers
let _ = managers_shutdown.await;

// t=???: Signal completion
let _ = completion_tx.send(());
```

## Problems

1. **Unbounded total time**:
   - HTTP shutdown: up to 20 seconds
   - Manager shutdown delay: 2 seconds
   - Manager shutdown: unbounded
   - Total: 22+ seconds minimum, possibly much longer

2. **User timeout not enforced**: The user's `--shutdown-timeout-secs` is only checked in `wait_for_completion()`, but the actual shutdown sequence doesn't respect it internally.

3. **Race condition**: If HTTP shutdown completes quickly (e.g., no active connections), we still wait 2 seconds before starting manager shutdown. This delays shutdown unnecessarily.

4. **No timeout on manager shutdown**: If a manager hangs, the entire shutdown hangs indefinitely (only bounded by the outer `wait_for_completion` timeout).

## Example Scenario

User sets `--shutdown-timeout-secs 10`:

1. HTTP shutdown completes in 1 second (no active connections)
2. Wait 2 seconds (hard-coded delay)
3. Manager shutdown takes 15 seconds (one manager is slow)
4. Total: 18 seconds - **exceeds user's 10-second timeout**

The outer `wait_for_completion(10s)` will timeout, but the shutdown tasks continue running in the background!

## Recommendation

1. **Apply timeout to entire shutdown sequence**:

```rust
tokio::select! {
    _ = async {
        // Manager shutdown with delay
        tokio::time::sleep(Duration::from_millis(2000)).await;
        if let Err(e) = managers.shutdown().await {
            log::error!("Manager shutdown failed: {}", e);
        }
    } => {}
    _ = tokio::time::sleep(shutdown_timeout) => {
        log::warn!("Manager shutdown timed out");
    }
}
```

2. **Make shutdown smarter**:

```rust
// Start HTTP shutdown
let http_shutdown_future = async {
    shutdown_handle.graceful_shutdown(Some(http_timeout));
    server_task.await
};

// Run HTTP and manager shutdowns with proper orchestration
tokio::select! {
    _ = http_shutdown_future => {
        // HTTP done, wait briefly then shutdown managers
        tokio::time::sleep(manager_delay).await;
        managers.shutdown().await
    }
    _ = tokio::time::sleep(total_timeout) => {
        log::warn!("Shutdown timeout exceeded");
    }
}
```

## Impact
- Reliability: Medium (shutdown can hang or exceed timeout)
- Production readiness: Medium (important for container orchestration)
- User experience: Medium (unexpected timeout behavior)
