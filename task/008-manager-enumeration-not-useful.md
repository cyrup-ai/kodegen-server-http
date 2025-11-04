# Manager Error Logging Uses Unhelpful Index

## Issue Type
Observability / Code Clarity

## Severity
Low

## Location
`src/managers.rs:55`

## Description
When a manager fails to shut down, the error is logged with only a numeric index (`i`), which provides no information about what manager failed or what it was responsible for.

```rust
if let Err(e) = hook.shutdown().await {
    log::warn!("Failed to shutdown manager {}: {}", i, e);
}
```

## Problems

1. **No identifying information**: The log says "manager 3 failed" but doesn't indicate what manager 3 does.

2. **Debugging difficulty**: When investigating logs, operators must:
   - Find the source code
   - Trace manager registration order
   - Count to the i-th registration
   - Identify what that manager is

3. **Registration order dependency**: The index changes if registration order changes, making historical logs harder to interpret.

4. **Multi-instance confusion**: If multiple server instances run, all will have "manager 0", "manager 1", etc., making it hard to identify which type of manager is failing across instances.

## Example Bad Log

```
[WARN] Failed to shutdown manager 0: Connection timeout
[WARN] Failed to shutdown manager 2: Process not found
```

What are managers 0 and 2? Browsers? Databases? Tunnels? Unknown from the log alone.

## Recommendation

### Option 1: Add manager names to the struct

```rust
pub struct Managers {
    shutdown_hooks: Vec<(String, Box<dyn ShutdownHook>)>,
}

pub fn register<H: ShutdownHook + 'static>(&mut self, name: impl Into<String>, hook: H) {
    self.shutdown_hooks.push((name.into(), Box::new(hook)));
}

// In shutdown:
for (name, hook) in &self.shutdown_hooks {
    if let Err(e) = hook.shutdown().await {
        log::warn!("Failed to shutdown manager '{}': {}", name, e);
    }
}
```

**Usage:**
```rust
managers.register("BrowserManager", browser_manager);
managers.register("TunnelManager", tunnel_manager);
```

### Option 2: Add type name method to trait

```rust
pub trait ShutdownHook: Send + Sync {
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

// In shutdown:
log::warn!("Failed to shutdown manager '{}': {}", hook.name(), e);
```

This automatically uses the type name (e.g., `my_crate::BrowserManager`).

### Comparison

**Option 1:**
- ✅ Clear, human-readable names
- ✅ No boilerplate in implementations
- ❌ Requires changes to registration API

**Option 2:**
- ✅ No registration API changes
- ✅ Works automatically
- ❌ Names are type paths (verbose)
- ✅ Can override for custom names

## Example Good Log (Option 1)

```
[WARN] Failed to shutdown manager 'BrowserManager': Connection timeout
[WARN] Failed to shutdown manager 'TunnelManager': Process not found
```

## Impact
- Observability: Low to Medium (helps with debugging)
- Developer experience: Low (minor quality of life improvement)
- Production operations: Low (helpful but not critical)
