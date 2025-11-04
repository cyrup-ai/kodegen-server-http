# Issue: Race Condition in Shutdown Sequence - Hardcoded 2-Second Delay

## Location
`src/server.rs:136-145`

## Severity
**HIGH** - Race condition that can cause real-world failures

## Description
The shutdown sequence uses a hardcoded 2-second delay before starting manager shutdown:

```rust
// Start manager shutdown concurrently with HTTP shutdown
// 2-second delay allows in-flight HTTP requests to complete before manager cleanup
let managers_shutdown = {
    let managers = managers.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2000)).await;
        log::debug!("Starting manager shutdown");
        if let Err(e) = managers.shutdown().await {
            log::error!("Failed to shutdown managers: {e}");
        }
    })
};
```

## Problem
This is a **time-based assumption** rather than proper coordination:

1. **No guarantee**: HTTP requests might take longer than 2 seconds
2. **Race condition**: If a request uses a manager resource after 2 seconds, it will fail
3. **Unpredictable**: Request duration depends on:
   - Network latency
   - Tool execution time
   - Client processing speed
   - System load

## Real-World Failure Scenarios

### Scenario 1: Long-running tool execution
```
T+0s:  Shutdown signal received
T+0s:  HTTP server starts graceful shutdown
T+0s:  Client calls expensive tool (e.g., browser automation, large file processing)
T+2s:  Managers start shutting down (BrowserManager closes Chrome)
T+3s:  Tool tries to interact with Chrome → crashes/errors
T+5s:  HTTP request finally completes with error
```

### Scenario 2: Slow client
```
T+0s:  Shutdown signal received
T+0s:  SSE streaming response starts
T+2s:  Manager shutdown closes resources
T+3s:  Client still consuming SSE stream → broken pipe / connection reset
```

### Scenario 3: Under load
```
T+0s:  Shutdown signal, 50 requests in flight
T+2s:  Managers shut down while 30 requests still processing
       → 30 failed requests with unclear error messages
```

## Impact
- **Data corruption**: Partial writes if database/storage managers close mid-operation
- **Client errors**: Requests fail with cryptic errors like "resource not available"
- **Poor UX**: Clients see failures instead of graceful "shutting down" responses
- **Debugging nightmare**: Errors occur far from root cause (timing race)

## Why This Is Wrong
HTTP graceful shutdown already has a 20-second timeout (line 148). The manager shutdown should wait for HTTP shutdown to **actually complete**, not guess with a fixed delay.

## Correct Approach
Wait for HTTP shutdown to complete, **then** shut down managers:

```rust
tokio::spawn(async move {
    ct_clone.cancelled().await;
    log::debug!("Cancellation triggered, initiating graceful shutdown");

    // Trigger HTTP shutdown with 20-second timeout
    shutdown_handle.graceful_shutdown(Some(Duration::from_secs(20)));

    // WAIT for HTTP server to actually finish
    match server_task.await {
        Ok(_) => log::debug!("HTTP server shutdown complete"),
        Err(e) => log::error!("HTTP server task panicked: {:?}", e),
    }

    // NOW it's safe to shut down managers
    log::debug!("Starting manager shutdown");
    if let Err(e) = managers.shutdown().await {
        log::error!("Failed to shutdown managers: {e}");
    }

    let _ = completion_tx.send(());
});
```

## Alternative: Refcount / Active Request Tracking
For even better control, track active requests and only shut down when count reaches zero.
