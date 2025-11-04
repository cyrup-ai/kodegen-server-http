# No Error Context When Registration Callback Fails

## Issue Type
Error Handling / Developer Experience

## Severity
Low

## Location
`src/lib.rs:101`

## Description
The registration callback is called without any additional error context. If it fails, the error message may not clearly indicate that the failure occurred during tool registration.

```rust
// Build routers using provided registration function
let routers = register_tools(&config_manager, &usage_tracker)?;
```

## Problems

1. **Unclear error location**: If `register_tools` returns an error, the error message might not indicate:
   - That it failed during registration
   - Which tool registration failed (if registering multiple tools)
   - What phase of initialization failed

2. **Debugging difficulty**: Users see an error from deep inside their tool initialization, but may not realize the issue is in their registration callback.

3. **No breadcrumbs**: The error propagates up with no context added at this layer.

## Example Error Without Context

```
Error: Failed to open configuration file: No such file or directory
```

Where did this happen? Was it:
- During config_manager initialization?
- During a tool registration?
- During something else?

The error doesn't say.

## Example Error With Context

```
Error: Failed to register tools

Caused by:
    0: Failed to initialize FilesystemTool
    1: Failed to open configuration file
    2: No such file or directory
```

Much clearer!

## Recommendation

Add error context using `anyhow::Context`:

```rust
// Build routers using provided registration function
let routers = register_tools(&config_manager, &usage_tracker)
    .context("Failed to register tools")?;
```

Even better, add more context:

```rust
let routers = register_tools(&config_manager, &usage_tracker)
    .with_context(|| {
        format!("Failed to register tools for category '{}'", category)
    })?;
```

## Additional Improvement

For even better developer experience, encourage registration functions to add their own context:

```rust
// In category server:
pub async fn register_my_tools(
    config: &ConfigManager,
    tracker: &UsageTracker,
) -> Result<RouterSet<HttpServer>> {
    let mut tool_router = ToolRouter::new();
    let mut prompt_router = PromptRouter::new();
    let mut managers = Managers::new();

    // Add context for each tool
    let fs_tool = FilesystemTool::new(config.clone())
        .context("Failed to create FilesystemTool")?;
    (tool_router, prompt_router) = register_tool(
        tool_router,
        prompt_router,
        fs_tool,
    );

    let browser_tool = BrowserTool::new(config.clone())
        .context("Failed to create BrowserTool")?;
    (tool_router, prompt_router) = register_tool(
        tool_router,
        prompt_router,
        browser_tool,
    );

    Ok(RouterSet::new(tool_router, prompt_router, managers))
}
```

## Real-World Impact

Without context, debugging initialization failures requires:
1. Looking at stack traces (if available)
2. Adding println/log statements
3. Binary search to find which tool is failing

With context:
1. Error message immediately shows what failed
2. Chain of errors shows exactly where the issue is
3. Fast debugging

## Impact
- Developer experience: Low to Medium (saves debugging time)
- Error reporting: Low (clearer errors)
- Production debugging: Low (helps operators identify issues faster)
