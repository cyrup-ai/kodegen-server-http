# Misleading Comment About Shutdown Delay

## Issue Type
Code Clarity / Documentation

## Severity
Very Low

## Location
`src/server.rs:135`

## Description
The comment claims that the 2-second delay "allows in-flight HTTP requests to complete before manager cleanup," but this is not necessarily true.

```rust
// 2-second delay allows in-flight HTTP requests to complete before manager cleanup
let managers_shutdown = {
    let managers = managers.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2000)).await;
        // ...
    })
};
```

## Problems

1. **Not a guarantee**: The comment implies that 2 seconds is enough for requests to complete, but:
   - Some requests may take longer (e.g., file uploads, complex computations)
   - The delay is fixed regardless of actual request duration
   - There's no actual coordination with request completion

2. **Misleading intent**: The comment suggests the delay is based on request timing, but it's actually just an arbitrary grace period.

3. **Sets wrong expectations**: Developers reading this might assume:
   - All requests complete within 2 seconds
   - The system waits for actual request completion
   - Managers are safe to shut down after this delay

## Reality

The 2-second delay is a **best-effort grace period**, not a guarantee. It provides a window for short requests to finish, but:
- Long requests will be interrupted
- Managers may start shutting down while requests are still using them
- The HTTP server has a separate 20-second shutdown timeout (task 002)

## Better Comments

### More accurate:

```rust
// 2-second grace period before manager shutdown
// This gives short requests time to complete, but long-running requests
// may still be in flight when managers start shutting down.
// The HTTP server has a 20-second graceful shutdown timeout.
```

### Even better (with context):

```rust
// Shutdown sequence:
// 1. HTTP server begins graceful shutdown (20s timeout)
// 2. After 2s grace period, managers begin shutting down in parallel
// 3. Both complete within overall shutdown timeout (configurable)
//
// Note: The 2s delay is a best-effort to let short requests finish
// before managers (which requests may depend on) start shutting down.
// Long-running requests may be interrupted.
```

### Best (with reference to issue):

```rust
// TODO: Make this delay configurable (see task/001-hard-coded-manager-shutdown-delay.md)
// The 2-second delay provides a grace period for short requests to complete
// before managers start shutting down. This is not a guarantee - requests
// longer than 2 seconds may still be in flight when manager shutdown begins.
let manager_shutdown_delay = Duration::from_millis(2000);
```

## Related Issues

- Task 001: The delay itself should be configurable
- Task 006: The shutdown timing logic has deeper issues

## Recommendation

Update the comment to be more accurate and less misleading:

```rust
// Start manager shutdown after a grace period to allow short in-flight
// requests time to complete. This is not a guarantee - long-running requests
// may still be in flight. The HTTP layer has a separate 20s shutdown timeout.
tokio::time::sleep(Duration::from_millis(2000)).await;
```

## Impact
- Code clarity: Very Low (minor documentation improvement)
- Developer understanding: Low (prevents misunderstanding)
- Maintenance: Very Low (easier for future developers)
