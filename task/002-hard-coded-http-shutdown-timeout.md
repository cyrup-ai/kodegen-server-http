# Hard-Coded HTTP Shutdown Timeout

## Issue Type
Configuration / User Experience

## Severity
Medium

## Location
`src/server.rs:148`

## Description
The HTTP graceful shutdown timeout is hard-coded to 20 seconds, independent of the user-configurable `--shutdown-timeout-secs` parameter.

```rust
shutdown_handle.graceful_shutdown(Some(Duration::from_secs(20)));
```

## Problems

1. **Inconsistent with user expectations**: Users specify `--shutdown-timeout-secs` but the HTTP layer uses a different, hard-coded timeout.

2. **Potential timeout mismatch**:
   - If user sets `--shutdown-timeout-secs 10`, HTTP will continue trying for 20 seconds, exceeding the expected timeout
   - If user sets `--shutdown-timeout-secs 60`, HTTP will only try for 20 seconds, not utilizing the full timeout window

3. **Hidden behavior**: Users have no visibility or control over this 20-second timeout.

## Current Behavior

User sets `--shutdown-timeout-secs 30`:
- Overall shutdown timeout: 30 seconds
- Manager shutdown delay: 2 seconds (task 001)
- HTTP shutdown timeout: 20 seconds (hard-coded)
- Manager shutdown: remaining time (up to 28 seconds)

This can lead to confusing behavior where shutdown doesn't respect the configured timeout.

## Recommendation

Derive HTTP shutdown timeout from the user-configured shutdown timeout:

```rust
// Option 1: Use majority of timeout for HTTP
let http_timeout = shutdown_config.total_timeout.mul_f32(0.8);

// Option 2: Reserve fixed time for manager cleanup
let http_timeout = shutdown_config.total_timeout.saturating_sub(Duration::from_secs(5));

// Option 3: Make it explicitly configurable
shutdown_handle.graceful_shutdown(Some(shutdown_config.http_timeout));
```

## Impact
- User experience: Medium (confusing behavior)
- Runtime performance: Low (only affects shutdown)
- Production readiness: Medium (could cause issues with orchestration systems expecting specific timeouts)
