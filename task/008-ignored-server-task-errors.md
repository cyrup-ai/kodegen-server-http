# Issue: Ignored server_task Errors and Panics

## Location
`src/server.rs:149`

## Severity
Medium - Hidden failures in critical component

## Description
The HTTP server task result is ignored during shutdown:

```rust
let _ = server_task.await;
log::debug!("HTTP server shutdown complete");
```

## Problem
The `await` on a `JoinHandle` can return:
1. `Ok(Ok(()))` - Server shut down cleanly
2. `Ok(Err(e))` - Server returned an error
3. `Err(e)` - Server task **panicked**

By using `let _ =`, all three cases are treated the same way, including panics!

## Real Scenarios Where This Hides Failures

### Panic in HTTP Handler
```rust
// A bug in axum-server causes panic during shutdown
// Current code: Silently ignored
// Expected: Logged as critical error
```

### TLS Configuration Error
```rust
// TLS cert expired/invalid during runtime
// Server task errors out
// Current code: "HTTP server shutdown complete" (lie!)
// Expected: "HTTP server error: TLS failure"
```

### Port Binding Issue
```rust
// Rare: port becomes unavailable during operation
// Server task exits with error
// Current code: Appears to shut down normally
// Expected: Flagged as abnormal termination
```

## Impact
1. **Hidden panics**: Server crashes are logged as normal shutdowns
2. **Misleading logs**: "shutdown complete" when it actually failed/panicked
3. **Difficult debugging**: Root cause is hidden, only symptoms visible
4. **Monitoring blind spots**: Health checks can't detect these failures

## Example Log Confusion
### Current (incorrect):
```
INFO Cancellation triggered, initiating graceful shutdown
DEBUG HTTP server shutdown complete      ← LIE! It panicked
DEBUG Manager shutdown complete
```

### Should be:
```
INFO Cancellation triggered, initiating graceful shutdown
ERROR HTTP server task panicked: thread 'tokio-runtime-worker' panicked at...
DEBUG Manager shutdown complete
```

## Recommendation
Check the result and log appropriately:

```rust
match server_task.await {
    Ok(Ok(())) => {
        log::debug!("HTTP server shutdown complete");
    }
    Ok(Err(e)) => {
        log::error!("HTTP server returned error: {}", e);
    }
    Err(e) => {
        log::error!("HTTP server task panicked: {:?}", e);
    }
}
```

## Related Issue
Same problem exists for `managers_shutdown` on line 153:
```rust
let _ = managers_shutdown.await;
```

Should also check for panics there.
