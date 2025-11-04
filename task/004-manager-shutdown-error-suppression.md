# Issue: Manager Shutdown Errors Are Suppressed

## Location
`src/managers.rs:54-56`

## Severity
Medium - Critical cleanup failures may be hidden

## Description
When shutting down managers, errors are only logged as warnings but not propagated:

```rust
.map(|(i, hook)| async move {
    if let Err(e) = hook.shutdown().await {
        log::warn!("Failed to shutdown manager {}: {}", i, e);
    }
})
```

## Problem
1. **Hidden failures**: Critical resource cleanup failures are suppressed
2. **No visibility**: Callers can't know if shutdown actually succeeded
3. **Resource leaks**: Failed shutdowns could leave resources allocated (browser processes, SSH tunnels, file handles, etc.)

## Impact
Examples of serious issues that would be hidden:
- `BrowserManager::shutdown()` fails to close Chrome processes → zombie processes
- `TunnelGuard::shutdown()` fails to close SSH tunnels → leaked connections
- `SearchManager::shutdown()` fails to cancel tasks → CPU/memory leak
- Database connection pools fail to drain → connection leaks

## Current Behavior
The `shutdown()` method always returns `Ok(())` regardless of how many individual manager shutdowns failed.

## Recommendation
1. **Track failures**: Collect all shutdown errors and return an aggregate error
2. **At minimum**: Log errors at ERROR level, not WARN level
3. **Consider**: Add telemetry/metrics for shutdown failures
4. **Document**: Make it clear that shutdown continues on failure (fail-fast vs fail-slow tradeoff)

## Example Fix
```rust
pub async fn shutdown(&self) -> Result<()> {
    log::info!("Shutting down {} managers in parallel", self.shutdown_hooks.len());

    let results: Vec<_> = join_all(
        self.shutdown_hooks
            .iter()
            .enumerate()
            .map(|(i, hook)| async move {
                hook.shutdown().await.map_err(|e| (i, e))
            })
    ).await;

    let errors: Vec<_> = results.into_iter().filter_map(|r| r.err()).collect();

    if !errors.is_empty() {
        for (i, e) in &errors {
            log::error!("Failed to shutdown manager {}: {}", i, e);
        }
        return Err(anyhow::anyhow!(
            "{} out of {} managers failed to shutdown",
            errors.len(),
            self.shutdown_hooks.len()
        ));
    }

    log::info!("All managers shut down successfully");
    Ok(())
}
```
