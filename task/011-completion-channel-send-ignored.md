# Issue: Completion Channel Send Error Ignored

## Location
`src/server.rs:156`

## Severity
Low - Unlikely but indicates logical issue

## Description
The completion signal send error is ignored:

```rust
let _ = completion_tx.send(());
```

## Problem
`oneshot::Sender::send()` returns `Result<(), T>` where the error contains the sent value if the receiver was dropped.

Error case means:
- The receiver (`completion_rx`) was dropped before shutdown completed
- The `wait_for_completion()` call was dropped or cancelled
- Nobody is waiting for shutdown to complete

## When This Happens

### Scenario 1: User Code Drops Handle
```rust
let handle = server.serve_with_tls(addr, None).await?;
// ... handle is dropped without calling wait_for_completion()
```

### Scenario 2: Timeout Already Fired
```rust
// In lib.rs:147
handle.wait_for_completion(timeout).await  // Times out first

// Then shutdown completes and tries to send
let _ = completion_tx.send(());  // Error: receiver gone
```

### Scenario 3: Panic in Main
```rust
let handle = server.serve_with_tls(addr, None).await?;
panic!("something went wrong");  // Handle dropped
```

## Why This Matters (Subtle Issue)
While the error is usually benign, ignoring it means:
1. **No visibility**: Can't tell if anyone was waiting for shutdown
2. **Logic smell**: If nobody's waiting, why are we doing graceful shutdown?
3. **Testing gaps**: Tests might not wait for completion properly

## Current Behavior
```
Shutdown monitor: "I've finished cleanup, sending completion signal"
Channel: *crickets* (receiver was dropped)
Shutdown monitor: "Whatever, I'm done" (exits)
```

## Impact
- **Low runtime impact**: Shutdown completes regardless
- **Observability gap**: Can't detect "orphaned" shutdowns
- **Code hygiene**: Ignoring errors that shouldn't happen

## Recommendation

### Option 1: Log if Receiver Dropped (Best)
```rust
if let Err(_) = completion_tx.send(()) {
    log::debug!(
        "Shutdown completion signal not delivered (receiver was dropped). \
         This is expected if wait_for_completion() timed out or was cancelled."
    );
}
```

### Option 2: Debug Assert
For catching issues in development:
```rust
#[cfg(debug_assertions)]
{
    if completion_tx.send(()).is_err() {
        log::warn!("Completion receiver was dropped before shutdown completed");
    }
}
#[cfg(not(debug_assertions))]
{
    let _ = completion_tx.send(());
}
```

### Option 3: Track State
More sophisticated approach for production monitoring:
```rust
match completion_tx.send(()) {
    Ok(()) => {
        log::debug!("Shutdown completion signal delivered");
    }
    Err(_) => {
        log::warn!("Shutdown completed but nobody was waiting (receiver dropped)");
        // Could increment metric for monitoring
    }
}
```

## Why Log This?
In production, seeing this warning could indicate:
- Shutdown timeout is too short
- Application shutdown logic has bugs
- Orchestrator is killing the process too aggressively
