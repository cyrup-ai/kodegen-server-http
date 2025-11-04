# Signal Handling Order Is Non-Deterministic

## Issue Type
Logic / Clarity

## Severity
Very Low

## Location
`src/lib.rs:174`

## Description
The shutdown signal handling uses `tokio::select!` without `biased`, which means if multiple signals arrive simultaneously, the order in which they're checked is non-deterministic.

```rust
tokio::select! {
    _ = ctrl_c => {
        log::debug!("Received SIGINT (Ctrl+C)");
    }
    _ = sigterm.recv() => {
        log::debug!("Received SIGTERM");
    }
    _ = sighup.recv() => {
        log::debug!("Received SIGHUP");
    }
}
```

## Problems

1. **Non-deterministic logging**: If multiple signals arrive at once (rare but possible), the log will show whichever one the runtime happened to check first, not necessarily the one that actually triggered shutdown.

2. **Potential confusion**: If debugging shutdown behavior, the logs might not accurately reflect what signal was actually received.

3. **No prioritization**: Some signals might be considered more important than others (e.g., SIGTERM from orchestrator vs SIGHUP from terminal), but there's no way to prioritize.

## When This Matters

This is rarely a practical issue because:
- Multiple signals arriving simultaneously is uncommon
- All three signals trigger the same shutdown behavior
- The first signal usually cancels the other receivers

However, it can be confusing when:
- Debugging shutdown logs
- Understanding signal handling behavior
- Ensuring deterministic test behavior

## Example Confusion

```bash
# User presses Ctrl+C and system sends SIGTERM at almost the same time
# Log might show:
[DEBUG] Received SIGTERM
```

User thinks "but I pressed Ctrl+C?" - technically both signals arrived, but SIGTERM was checked first.

## Recommendation

### Option 1: Use biased select (deterministic order)

```rust
tokio::select! {
    biased;  // Check in order listed

    _ = ctrl_c => {
        log::debug!("Received SIGINT (Ctrl+C)");
    }
    _ = sigterm.recv() => {
        log::debug!("Received SIGTERM");
    }
    _ = sighup.recv() => {
        log::debug!("Received SIGHUP");
    }
}
```

This checks signals in order, so SIGINT is always handled before SIGTERM if both are ready.

### Option 2: Handle all signals that arrived

```rust
let signal = tokio::select! {
    _ = ctrl_c => "SIGINT",
    _ = sigterm.recv() => "SIGTERM",
    _ = sighup.recv() => "SIGHUP",
};
log::debug!("Received {} signal", signal);

// Check if other signals also arrived
if sigterm.try_recv().is_ok() {
    log::debug!("Also received SIGTERM");
}
if sighup.try_recv().is_ok() {
    log::debug!("Also received SIGHUP");
}
```

### Option 3: Log all signals (most complete)

```rust
use tokio::signal::unix::{signal, SignalKind};

let mut signals = vec![
    ("SIGINT", signal(SignalKind::interrupt())?),
    ("SIGTERM", signal(SignalKind::terminate())?),
    ("SIGHUP", signal(SignalKind::hangup())?),
];

loop {
    let received = tokio::select! {
        _ = signals[0].1.recv() => signals[0].0,
        _ = signals[1].1.recv() => signals[1].0,
        _ = signals[2].1.recv() => signals[2].0,
    };

    log::debug!("Received {} signal", received);
    break;
}
```

### Option 4: Keep as-is and document

```rust
// Note: Using unbiased select - if multiple signals arrive simultaneously,
// the order is non-deterministic. This is acceptable because all signals
// trigger the same shutdown behavior.
tokio::select! {
    _ = ctrl_c => {
        log::debug!("Received SIGINT (Ctrl+C)");
    }
    // ...
}
```

## Recommendation

**Option 4** (document and keep as-is) is probably best because:
- ✅ Simple
- ✅ No performance overhead
- ✅ No added complexity
- ✅ Clarifies intent

This is not a bug, just a characteristic of the implementation that should be documented.

## Alternative: Remove SIGHUP

SIGHUP traditionally means "hangup" (terminal closed) and was often ignored or used for config reload in daemons. For a modern server, it's questionable whether SIGHUP should trigger shutdown:

```rust
// Only handle the two standard shutdown signals
tokio::select! {
    _ = ctrl_c => {
        log::debug!("Received SIGINT (Ctrl+C)");
    }
    _ = sigterm.recv() => {
        log::debug!("Received SIGTERM");
    }
}
```

This is simpler and avoids confusion about SIGHUP behavior.

## Impact
- Behavior: Very Low (no practical difference)
- Debugging: Very Low (minor logging confusion possible)
- Code clarity: Very Low (documentation improvement)
