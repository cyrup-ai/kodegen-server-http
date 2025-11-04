# Ignored Manager Shutdown Task Errors

## Issue Type
Error Handling / Observability

## Severity
High

## Location
`src/server.rs:153`

## Description
The manager shutdown task result is explicitly ignored using `let _ = managers_shutdown.await;`. If the task panics or if managers fail to shut down properly, this is silently dropped.

```rust
let _ = managers_shutdown.await;
log::debug!("Manager shutdown complete");
```

## Problems

1. **Hidden shutdown failures**: Managers are responsible for cleaning up critical resources like:
   - Browser processes (ChromeDriver, etc.)
   - SSH tunnels
   - Database connections
   - Background tasks

   If these fail to shut down properly, resources leak but the error is invisible.

2. **False success indication**: The code logs "Manager shutdown complete" even if shutdown failed.

3. **Resource leaks**: Failed manager shutdown often means leaked resources (processes, file handles, network connections).

4. **Cascading failures**: On the next startup, leaked resources might cause conflicts (e.g., ports still in use, lock files present).

## Real-World Scenarios

1. **Browser processes**: A BrowserManager fails to kill Chrome processes. These processes consume memory and may interfere with subsequent runs.

2. **SSH tunnels**: A TunnelManager fails to close SSH tunnels. Ports remain bound and the next startup fails with "port already in use".

3. **Database connections**: A DatabaseManager fails to close connections. The connection pool is exhausted and new connections fail.

## Current Behavior in managers.rs

The `Managers::shutdown()` function logs warnings but returns `Ok(())` even if all managers fail:

```rust
pub async fn shutdown(&self) -> Result<()> {
    // ... logs warnings for failures ...
    join_all(shutdown_futures).await;
    Ok(())  // Always returns Ok!
}
```

This compounds the problem - errors are logged but not propagated.

## Recommendation

1. **Capture and log errors in server.rs**:

```rust
match managers_shutdown.await {
    Ok(Ok(())) => {
        log::debug!("Manager shutdown complete");
    }
    Ok(Err(e)) => {
        log::error!("Manager shutdown failed: {}", e);
    }
    Err(e) if e.is_panic() => {
        log::error!("Manager shutdown task panicked: {:?}", e);
    }
    Err(e) => {
        log::error!("Manager shutdown task failed: {:?}", e);
    }
}
```

2. **Improve Managers::shutdown()** (see task 007):
   - Return actual errors instead of always `Ok(())`
   - Provide details about which managers failed

## Impact
- Resource management: High (resource leaks)
- Observability: High (hidden failures)
- Production readiness: High (can cause cascading failures)
- Debugging: High (makes troubleshooting difficult)
