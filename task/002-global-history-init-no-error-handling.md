# Issue: Global Tool History Initialization Without Error Handling

## Location
`src/lib.rs:98`

## Severity
Medium - Hidden errors in global state initialization

## Description
Global tool history is initialized without any error handling:

```rust
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

## Problem
1. **No error handling**: If initialization fails, the error is silently ignored
2. **Global state**: This appears to initialize global/static state, which can be problematic if:
   - The function is called multiple times (e.g., in tests or multi-server scenarios)
   - Initialization fails partway through
   - There's no cleanup mechanism

## Impact
- Tool history tracking may silently fail, leading to incomplete or missing history data
- In test scenarios, repeated initialization could cause panics or data corruption
- Difficult to diagnose issues since failures are silent

## Recommendation
1. Check the return type of `init_global_history` and handle potential errors
2. Add logging to indicate successful initialization
3. Consider if this global state should be part of the server instance instead
4. Ensure idempotency or protect against multiple initializations

## Example Fix
```rust
// If it returns Result:
kodegen_mcp_tool::tool_history::init_global_history(instance_id)
    .await
    .context("Failed to initialize global tool history")?;

// Or at minimum:
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
log::info!("Initialized global tool history tracking");
```
