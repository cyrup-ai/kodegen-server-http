# Issue: Resource Handler Methods Are Stubs

## Location
`src/server.rs:227-258`

## Severity
Low - Feature not implemented (may be intentional)

## Description
Three MCP resource-related handler methods are stub implementations:

### 1. `list_resources` (lines 227-236)
```rust
async fn list_resources(
    &self,
    _request: Option<PaginatedRequestParam>,
    _: RequestContext<RoleServer>,
) -> Result<ListResourcesResult, McpError> {
    Ok(ListResourcesResult {
        resources: vec![],
        next_cursor: None,
    })
}
```
Always returns empty list.

### 2. `read_resource` (lines 238-247)
```rust
async fn read_resource(
    &self,
    request: ReadResourceRequestParam,
    _: RequestContext<RoleServer>,
) -> Result<ReadResourceResult, McpError> {
    Err(McpError::resource_not_found(
        "resource_not_found",
        Some(serde_json::json!({ "uri": request.uri })),
    ))
}
```
Always returns "not found" error.

### 3. `list_resource_templates` (lines 249-258)
```rust
async fn list_resource_templates(
    &self,
    _request: Option<PaginatedRequestParam>,
    _: RequestContext<RoleServer>,
) -> Result<ListResourceTemplatesResult, McpError> {
    Ok(ListResourceTemplatesResult {
        next_cursor: None,
        resource_templates: Vec::new(),
    })
}
```
Always returns empty list.

## Problem
If clients attempt to use MCP resources, they will:
1. See resources capability is NOT advertised (line 167-170 doesn't enable resources)
2. BUT the handler methods are still callable if a client tries
3. Get confusing "not found" errors instead of "not supported"

## Impact
- **Misleading errors**: Clients see "resource_not_found" instead of "resources not supported"
- **Incomplete feature**: Resources feature is partially implemented
- **Client confusion**: Some MCP features work (tools, prompts), others silently fail

## Potential Issues
If someone tries to enable resources without realizing these are stubs:
```rust
ServerCapabilities::builder()
    .enable_tools()
    .enable_prompts()
    .enable_resources()  // ← This would break!
    .build()
```

Clients would think resources are supported but get failures.

## Recommendation

### Option 1: Document as Intentional
If resources are not planned:
```rust
/// Resource handlers are not implemented - this server only supports tools and prompts.
/// Resources capability is not advertised, so clients should not call these methods.
/// These methods exist only to satisfy the ServerHandler trait.
async fn list_resources(...) -> Result<...> {
    // Not implemented - resources not supported
    Ok(ListResourcesResult {
        resources: vec![],
        next_cursor: None,
    })
}
```

### Option 2: Return Proper "Not Supported" Errors
```rust
async fn list_resources(...) -> Result<...> {
    Err(McpError::method_not_found(
        "resources_not_supported",
        Some(serde_json::json!({
            "message": "This server does not support MCP resources"
        }))
    ))
}
```

### Option 3: Implement Resources
If resources should be supported, implement them properly with:
- Router for resource handlers
- Registration mechanism like tools/prompts
- Enable capability in `get_info()`
