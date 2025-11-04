# Ignored Completion Channel Send Errors

## Issue Type
Error Handling / Logic

## Severity
Low

## Location
`src/server.rs:156`

## Description
The completion signal send is explicitly ignored using `let _ = completion_tx.send(());`. If the receiver has been dropped, this fails silently.

```rust
let _ = completion_tx.send(());
```

## Problems

1. **Indicates unexpected state**: If the receiver is dropped, it means `wait_for_completion()` was dropped or cancelled. This is an unusual state that might indicate a logic error.

2. **Silent failure**: The send failure is not logged, making it harder to debug unexpected shutdown behavior.

3. **Incomplete observability**: Operators can't see if the completion signaling mechanism is working correctly.

## When This Happens

The receiver is dropped if:
1. The caller drops the `ServerHandle` without calling `wait_for_completion()`
2. The `wait_for_completion()` future is cancelled (dropped mid-execution)
3. The `wait_for_completion()` timeout expires and drops the receiver

Case 3 is the most likely and somewhat expected. However, cases 1 and 2 indicate logic errors that should be logged.

## Recommendation

Log the error with context:

```rust
if completion_tx.send(()).is_err() {
    log::debug!(
        "Completion receiver was dropped - \
         wait_for_completion likely timed out or was cancelled"
    );
}
```

Or, distinguish between expected and unexpected cases:

```rust
// Store timeout value in the shutdown monitor task
if completion_tx.send(()).is_err() {
    if shutdown_timed_out {
        log::debug!("Completion receiver dropped due to timeout");
    } else {
        log::warn!("Completion receiver unexpectedly dropped");
    }
}
```

## Impact
- Observability: Low (minor debugging aid)
- Production readiness: Low (doesn't affect functionality)
- Code clarity: Low (improves understanding of shutdown flow)
