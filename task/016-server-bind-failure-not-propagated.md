# Issue: Server Bind Failures Not Propagated to Caller

## Location
`src/server.rs:106-125`

## Severity
High - Server may fail to start silently

## Description
The HTTP server binding happens inside a spawned task, so bind failures are only logged, not returned to the caller:

```rust
pub async fn serve_with_tls(
    self,
    addr: SocketAddr,
    tls_config: Option<(PathBuf, PathBuf)>,
) -> Result<ServerHandle> {
    // ... setup code ...

    // This spawns immediately and returns
    let server_task = tokio::spawn(async move {
        if let Err(e) = axum_server::bind(addr)
            .handle(axum_handle)
            .serve(router.into_make_service())
            .await
        {
            log::error!("HTTP server error: {e}");
        }
    });

    // Function returns here - binding hasn't actually happened yet!
    Ok(ServerHandle::new(ct, completion_rx))
}
```

## Problem
The function returns `Ok(ServerHandle)` **before** the server actually tries to bind to the port. Binding happens asynchronously in the background task.

## Race Condition Flow

```
T+0ms:  serve_with_tls() called
T+1ms:  Spawns background task
T+2ms:  Returns Ok(ServerHandle) ← Caller thinks server is running!
T+5ms:  Background task tries to bind to port
T+6ms:  Bind fails (port already in use)
T+7ms:  log::error("HTTP server error: address already in use")
        But caller already proceeded thinking server started!
```

## Real-World Failure Scenarios

### Scenario 1: Port Already in Use
```rust
let handle = server.serve_with_tls(addr, None).await?;
log::info!("Server running on {}", addr);  // ← LIE! Bind hasn't happened yet

// Wait for shutdown
wait_for_shutdown_signal().await?;
handle.cancel();
handle.wait_for_completion(timeout).await?;  // ← Waits for nothing (server never started)
```

Actual behavior:
```
INFO: Starting HTTP server on http://0.0.0.0:8080
INFO: Server running on http://0.0.0.0:8080    ← False claim
INFO: Press Ctrl+C to shutdown
ERROR: HTTP server error: Address already in use  ← Delayed error
(Server never actually started but appears to be running)
```

### Scenario 2: Permission Denied
```rust
// Try to bind to privileged port without permissions
let handle = server.serve_with_tls("0.0.0.0:80".parse()?, None).await?;
// Returns success!

// Later...
// ERROR: HTTP server error: Permission denied
```

### Scenario 3: Invalid TLS Configuration
```rust
let handle = server.serve_with_tls(
    addr,
    Some(("bad_cert.pem".into(), "bad_key.pem".into()))
).await?;  // Returns Ok!

// Later...
// ERROR: Failed to load TLS configuration: No such file or directory
```

## Impact

### For Users
- Confusing error messages ("Server says it's running but I can't connect")
- Delayed failure detection (errors show up seconds after "success")
- Misleading logs (says "running" when it's not)

### For Automated Systems
```rust
// Health check immediately after start
let handle = server.serve_with_tls(addr, None).await?;
check_health(addr).await?;  // ← Fails! Server hasn't bound yet
```

### For Integration Tests
```rust
#[tokio::test]
async fn test_server() {
    let handle = server.serve_with_tls(addr, None).await.unwrap();
    // Test assumes server is ready
    let response = client.get("http://localhost:8080").await.unwrap();
    // ← Might fail due to race condition
}
```

## Current Behavior in lib.rs
```rust
// lib.rs:127-129
let handle = server.serve_with_tls(addr, cli.tls_config()).await?;

log::info!("{} server running on {}://{}", category, protocol, addr);
// ← This log is printed BEFORE server actually binds!
```

## Root Cause
The pattern of spawning the server task immediately without waiting for bind completion:
```rust
tokio::spawn(async move {
    axum_server::bind(addr)  // ← This hasn't happened yet when function returns
        .serve(...)
        .await
})
```

## Recommendation

### Option 1: Wait for Server Ready (Best)
Use a channel to signal when bind succeeds:

```rust
pub async fn serve_with_tls(
    self,
    addr: SocketAddr,
    tls_config: Option<(PathBuf, PathBuf)>,
) -> Result<ServerHandle> {
    let (ready_tx, ready_rx) = oneshot::channel();

    let server_task = tokio::spawn(async move {
        // Build server
        let server = axum_server::bind(addr)
            .handle(axum_handle)
            .serve(router.into_make_service());

        // Signal that bind succeeded
        let _ = ready_tx.send(());

        // Run server
        if let Err(e) = server.await {
            log::error!("HTTP server error: {e}");
        }
    });

    // Wait for bind to succeed or fail
    match tokio::time::timeout(Duration::from_secs(5), ready_rx).await {
        Ok(Ok(())) => {
            log::debug!("Server bind successful");
            Ok(ServerHandle::new(ct, completion_rx))
        }
        Ok(Err(_)) => {
            Err(anyhow::anyhow!("Server task ended before bind completed"))
        }
        Err(_) => {
            server_task.abort();
            Err(anyhow::anyhow!("Server bind timeout (5 seconds)"))
        }
    }
}
```

### Option 2: Pre-bind Check
Check if port is available before spawning:
```rust
// Try to bind synchronously first
let listener = tokio::net::TcpListener::bind(addr).await
    .context("Failed to bind to address")?;

// Now spawn with pre-bound listener
let server_task = tokio::spawn(async move {
    axum_server::from_tcp(listener)
        .serve(...)
        .await
});
```

### Option 3: Return Future
Don't spawn immediately - let caller control when to start:
```rust
pub fn serve_with_tls(self, ...) -> impl Future<Output = Result<()>> {
    async move {
        // Bind and serve
        axum_server::bind(addr)
            .serve(...)
            .await?;
        Ok(())
    }
}

// Caller spawns when ready
let server_fut = server.serve_with_tls(addr, None);
tokio::spawn(server_fut);
```

### Option 4: Add Health Check Method
```rust
impl ServerHandle {
    pub async fn wait_until_ready(&self, timeout: Duration) -> Result<()> {
        // Poll the port until it responds
        // ...
    }
}

// Usage
let handle = server.serve_with_tls(addr, None).await?;
handle.wait_until_ready(Duration::from_secs(5)).await?;
log::info!("Server is now ready");
```

## Testing Recommendation
Add integration test that verifies bind failures are caught:
```rust
#[tokio::test]
async fn test_bind_failure_propagated() {
    let addr = "0.0.0.0:8080";

    // Occupy the port
    let _listener = TcpListener::bind(addr).await.unwrap();

    // Try to start server on same port
    let result = server.serve_with_tls(addr.parse()?, None).await;

    // Should fail, not succeed!
    assert!(result.is_err());
}
```
