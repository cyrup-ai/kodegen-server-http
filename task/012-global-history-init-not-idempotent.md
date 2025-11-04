# Global History Initialization May Not Be Idempotent

## Issue Type
Logic / Testing

## Severity
Low

## Location
`src/lib.rs:98`

## Description
The code calls `kodegen_mcp_tool::tool_history::init_global_history(instance_id).await` to initialize a global state. If this function is not idempotent, calling it multiple times (e.g., in tests, or if run_http_server is called twice) could cause issues.

```rust
// Initialize global tool history tracking
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

## Problems

1. **Testing difficulty**: Unit tests that call `run_http_server` multiple times will reinitialize global state, which may:
   - Cause panics if the initialization is not idempotent
   - Leak resources (if each init allocates resources)
   - Cause test pollution (state from one test affects another)

2. **Unclear behavior**: Without seeing the implementation, it's unclear what happens on subsequent calls:
   - Does it reset the existing history?
   - Does it panic?
   - Does it silently ignore subsequent calls?
   - Does it merge histories?

3. **Documentation gap**: The code doesn't document:
   - Whether calling this multiple times is safe
   - What happens if it's called with different instance_ids
   - Whether cleanup is needed on shutdown

4. **Lifecycle management**: No corresponding shutdown/cleanup for this global state. If the history allocates resources (files, memory, background tasks), these may leak on graceful shutdown.

## Without Access to Implementation

We can't see the implementation of `init_global_history`, but common issues include:

```rust
// Bad: Not idempotent - panics on second call
static HISTORY: OnceCell<History> = OnceCell::new();

pub async fn init_global_history(id: String) {
    HISTORY.set(History::new(id)).expect("Already initialized!");  // Panics!
}

// Better: Idempotent - silent on subsequent calls
pub async fn init_global_history(id: String) {
    HISTORY.get_or_init(|| History::new(id));
}

// Best: Warns on reinitialization
pub async fn init_global_history(id: String) {
    if HISTORY.set(History::new(id)).is_err() {
        log::warn!("Global history already initialized, ignoring");
    }
}
```

## Testing Impact

This makes integration testing difficult:

```rust
#[tokio::test]
async fn test_server_startup() {
    run_http_server("test", |_, _| { /* ... */ }).await.unwrap();
}

#[tokio::test]
async fn test_server_shutdown() {
    // If init_global_history is not idempotent, this may fail
    run_http_server("test", |_, _| { /* ... */ }).await.unwrap();
}
```

## Recommendation

1. **Document the behavior**:

```rust
// Initialize global tool history tracking
// Note: This function is idempotent - subsequent calls are ignored
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

2. **Add cleanup on shutdown**:

```rust
// At the end of run_http_server, before returning:
kodegen_mcp_tool::tool_history::cleanup_global_history().await;
```

3. **For testing, add a reset function**:

```rust
#[cfg(test)]
pub async fn reset_global_history() {
    // Reset for testing
}
```

4. **Better: Make it non-global**:

Instead of global state, pass the history instance through the server:

```rust
pub struct HttpServer {
    // ... existing fields ...
    tool_history: Arc<ToolHistory>,
}

// Then no global state is needed
```

This is cleaner architecture and easier to test.

## Impact
- Testing: Low to Medium (affects test reliability)
- Code clarity: Low (unclear lifecycle)
- Architecture: Low (global state is generally less ideal)
- Production: Very Low (unlikely to be called twice in production)

## Note

This is mainly a concern for:
- Test suites
- Future refactoring
- Code maintainability

In production use, `run_http_server` is typically called once, so this is unlikely to cause issues in practice.
