# Issue: Hardcoded 20-Second HTTP Shutdown Timeout Separate from User Config

## Location
`src/server.rs:148`

## Severity
Medium - Configuration inconsistency

## Description
The HTTP server shutdown uses a hardcoded 20-second timeout:

```rust
shutdown_handle.graceful_shutdown(Some(Duration::from_secs(20)));
```

This is independent from the user-configurable `--shutdown-timeout-secs` CLI parameter (default 30 seconds).

## Problem
There are now **two separate timeouts**:
1. User-configured overall shutdown timeout: 30 seconds (default)
2. Hardcoded HTTP shutdown timeout: 20 seconds

This creates confusion:
- Which timeout applies?
- What if user sets `--shutdown-timeout-secs=10`? HTTP timeout (20s) exceeds it!
- What if user sets `--shutdown-timeout-secs=60`? HTTP only waits 20s

## Current Flow
```
User sets --shutdown-timeout-secs=60

1. Main wait: 60 seconds (from lib.rs:147)
   └─> But internally:
       - HTTP shutdown: 20 seconds (hardcoded)
       - Manager shutdown: ~2-3 seconds after HTTP

Total actual shutdown time: ~23 seconds
User's 60-second config is ignored!
```

## Impact
- **Misleading configuration**: User thinks they control shutdown timeout, but they don't
- **Premature timeout**: HTTP connections might be killed while user expects more time
- **Wasted waiting**: If HTTP finishes in 23s but user set 10s timeout, the 23s is wasted
- **Inconsistent behavior**: Different timeout sources make debugging difficult

## Inconsistency Example
```bash
# User wants quick shutdown
./server --shutdown-timeout-secs=5

# But HTTP server ignores it and waits 20 seconds anyway!
# Manager shutdown adds another 2-3 seconds
# Total: ~23 seconds instead of 5
```

## Recommendation
1. **Pass user timeout through**: Use the CLI-configured timeout for HTTP shutdown
2. **Calculate allocation**: Allocate timeout budget between HTTP and manager shutdown
3. **Document**: Make it clear what the timeout applies to

## Example Fix

### Option 1: Pass timeout from CLI
```rust
// In lib.rs, pass timeout to server
let user_timeout = cli.shutdown_timeout();
let handle = server.serve_with_tls(addr, cli.tls_config(), user_timeout).await?;

// In server.rs
pub async fn serve_with_tls(
    self,
    addr: SocketAddr,
    tls_config: Option<(PathBuf, PathBuf)>,
    shutdown_timeout: Duration,
) -> Result<ServerHandle> {
    // ...

    // Use 80% of timeout for HTTP, 20% for managers
    let http_timeout = shutdown_timeout.mul_f32(0.8);
    let manager_timeout = shutdown_timeout.mul_f32(0.2);

    tokio::spawn(async move {
        ct_clone.cancelled().await;

        shutdown_handle.graceful_shutdown(Some(http_timeout));
        let _ = server_task.await;

        // Use timeout for manager shutdown too
        match tokio::time::timeout(manager_timeout, managers.shutdown()).await {
            Ok(Ok(())) => log::debug!("Managers shut down successfully"),
            Ok(Err(e)) => log::error!("Manager shutdown failed: {}", e),
            Err(_) => log::warn!("Manager shutdown timed out"),
        }

        let _ = completion_tx.send(());
    });
}
```

### Option 2: Document limitation
If keeping hardcoded timeout, document it clearly:
```rust
/// HTTP shutdown timeout (hardcoded to 20s)
///
/// Note: This is independent of the CLI --shutdown-timeout-secs flag.
/// The CLI timeout controls how long we wait for shutdown to complete,
/// but this controls how long the HTTP server waits for connections to close.
const HTTP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
```
