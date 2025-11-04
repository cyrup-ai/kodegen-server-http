# Issue: Client Info Storage Error Suppressed During Initialize

## Location
`src/server.rs:265-267`

## Severity
Low-Medium - Depends on importance of client info

## Description
When storing client information fails during MCP initialization, only a warning is logged:

```rust
async fn initialize(
    &self,
    request: InitializeRequestParam,
    _context: RequestContext<RoleServer>,
) -> Result<InitializeResult, McpError> {
    if let Err(e) = self.config_manager.set_client_info(request.client_info).await {
        log::warn!("Failed to store client info: {e:?}");
    }
    Ok(self.get_info())
}
```

The initialization succeeds even if client info storage fails.

## Problem
Whether this is acceptable depends on what `client_info` is used for downstream:

### If Client Info Is Critical
- User/authentication tracking
- Feature flags based on client version
- Usage billing/quotas
- Audit logging

Then suppressing this error could cause:
- Incorrect billing
- Security audit gaps
- Feature compatibility issues

### If Client Info Is Optional
- Telemetry
- Diagnostics
- Analytics

Then warning is probably fine.

## Current Behavior
```
Client → initialize(client_info={...})
Server → Tries to store client info
Storage → ERROR: database connection failed
Server → "Failed to store client info: ..." (warn only)
Server → Returns successful initialization
Client → Thinks everything is fine
```

## Potential Issues

### Issue 1: Silent Failures Accumulate
```rust
// Later in tool execution
if self.config_manager.get_client_info().await?.is_none() {
    // Wait, we stored it during initialize... right?
    return Err("Client info required but not found");
}
```

### Issue 2: Audit/Compliance
```
Compliance Officer: "Show me all API access by client XYZ"
System: "Sorry, client info storage failed but we allowed the connection"
```

### Issue 3: Feature Flags
```rust
// Tool wants to check client capability
let client_version = config_manager.get_client_version().await?;
if client_version.supports_streaming() {
    // Returns None because storage failed
    // Falls back to non-streaming mode
}
```

## Impact Assessment Questions

To determine severity, need to understand:
1. What is `client_info` used for in the system?
2. Can the server operate correctly without it?
3. Are there downstream assumptions that it exists?
4. Is this for audit/compliance purposes?

## Recommendations

### Option 1: Fail Initialization (Conservative)
If client info is important:
```rust
self.config_manager
    .set_client_info(request.client_info)
    .await
    .map_err(|e| {
        McpError::internal_error(
            "failed_to_store_client_info",
            Some(serde_json::json!({ "error": e.to_string() }))
        )
    })?;

Ok(self.get_info())
```

### Option 2: Retry Logic
```rust
let mut retries = 3;
loop {
    match self.config_manager.set_client_info(request.client_info.clone()).await {
        Ok(()) => break,
        Err(e) if retries > 0 => {
            retries -= 1;
            log::warn!("Failed to store client info (retries left: {}): {}", retries, e);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(e) => {
            return Err(McpError::internal_error(
                "client_info_storage_failed",
                Some(serde_json::json!({ "error": e.to_string() }))
            ));
        }
    }
}
```

### Option 3: Keep Warning But Document
If client info is truly optional:
```rust
// Client info is optional telemetry data only.
// Initialization succeeds even if storage fails since no features depend on it.
if let Err(e) = self.config_manager.set_client_info(request.client_info).await {
    log::warn!("Failed to store client info (optional): {e:?}");
}
```

### Option 4: Increment Error Metric
For observability:
```rust
if let Err(e) = self.config_manager.set_client_info(request.client_info).await {
    log::warn!("Failed to store client info: {e:?}");
    metrics::counter!("client_info_storage_errors").increment(1);
}
```
