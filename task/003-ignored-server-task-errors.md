# Ignored Server Task Errors

## Issue Type
Error Handling / Observability

## Severity
High

## Location
`src/server.rs:149`

## Description
The server task result is explicitly ignored using `let _ = server_task.await;`. If the HTTP/HTTPS server task panics or returns an error, it will be silently dropped without any notification.

```rust
let _ = server_task.await;
log::debug!("HTTP server shutdown complete");
```

## Problems

1. **Hidden panics**: If the server task panics, the panic is caught by tokio but the error is discarded. Operators have no visibility into what went wrong.

2. **False success indication**: The code logs "HTTP server shutdown complete" even if the server crashed or errored.

3. **Debugging difficulty**: In production, silent failures make it extremely difficult to diagnose issues.

4. **Incomplete shutdown**: If the server failed before shutdown was initiated, the shutdown sequence may be operating on incorrect assumptions.

## Real-World Scenarios

1. **TLS certificate expiry**: If the certificate expires during runtime and causes a fatal error, this would be silently ignored.

2. **Port binding issues**: If something causes the server to lose its port binding, the error would be invisible.

3. **Resource exhaustion**: If the server runs out of file descriptors or memory, the error would be dropped.

## Recommendation

Log the error appropriately:

```rust
match server_task.await {
    Ok(()) => {
        log::debug!("HTTP server shutdown complete");
    }
    Err(e) if e.is_panic() => {
        log::error!("HTTP server task panicked during shutdown: {:?}", e);
    }
    Err(e) => {
        log::error!("HTTP server task failed: {:?}", e);
    }
}
```

Better yet, propagate the error status so callers can detect failures:

```rust
pub struct ShutdownResult {
    pub server_ok: bool,
    pub managers_ok: bool,
}

// Return this from wait_for_completion
pub async fn wait_for_completion(mut self, timeout: Duration)
    -> Result<ShutdownResult, Duration>
```

## Impact
- Observability: High (hidden errors in production)
- Debugging: High (makes troubleshooting very difficult)
- Production readiness: High (critical for production operations)
