# Issue: No Ordering Control for Manager Shutdown

## Location
`src/managers.rs:50-61`

## Severity
Medium - Could cause cascading failures

## Description
All managers are shut down in parallel with no way to specify dependencies or ordering:

```rust
let shutdown_futures: Vec<_> = self.shutdown_hooks
    .iter()
    .enumerate()
    .map(|(i, hook)| async move { ... })
    .collect();

join_all(shutdown_futures).await;
```

## Problem
In real-world systems, managers often have dependencies:
- Manager A might use resources from Manager B
- Manager B should be shut down *after* Manager A

Parallel shutdown can cause:
- Use-after-close errors
- Panics or crashes
- Data corruption
- Unclear error messages

## Example Scenarios
1. **Database + Cache**:
   - Cache manager depends on database
   - If database shuts down first, cache flush fails

2. **HTTP Client + Connection Pool**:
   - HTTP client uses connection pool
   - If pool shuts down first, in-flight requests fail

3. **Metrics + Exporters**:
   - Metrics collector depends on exporters
   - If exporters shut down first, final metrics are lost

## Impact
- Difficult to debug cascading shutdown failures
- Resource cleanup may fail due to ordering issues
- Forces defensive programming in every shutdown hook
- No way to express "A depends on B" in the type system

## Current Limitations
The `Managers::register()` API accepts any `ShutdownHook` in any order, with no way to specify dependencies.

## Recommendation
Options:
1. **Sequential shutdown**: Shut down in reverse registration order (LIFO)
2. **Priority levels**: Allow registering managers with shutdown priority
3. **Dependency graph**: Build a DAG of dependencies and topological sort
4. **Document**: At minimum, document the current behavior and best practices

## Example Fix (Sequential LIFO approach)
```rust
pub async fn shutdown(&self) -> Result<()> {
    log::info!("Shutting down {} managers sequentially (LIFO)", self.shutdown_hooks.len());

    // Shut down in reverse order of registration
    for (i, hook) in self.shutdown_hooks.iter().enumerate().rev() {
        log::debug!("Shutting down manager {}", i);
        if let Err(e) = hook.shutdown().await {
            log::error!("Failed to shutdown manager {}: {}", i, e);
            // Continue or fail-fast depending on requirements
        }
    }

    Ok(())
}
```

## Alternative: Priority-based
```rust
pub struct Managers {
    shutdown_hooks: Vec<(u32, Box<dyn ShutdownHook>)>, // (priority, hook)
}

pub fn register_with_priority<H: ShutdownHook + 'static>(
    &mut self,
    hook: H,
    priority: u32
) {
    self.shutdown_hooks.push((priority, Box::new(hook)));
    // Sort by priority
    self.shutdown_hooks.sort_by_key(|(p, _)| *p);
}
```
