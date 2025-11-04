# Managers Shutdown Always Returns Success

## Issue Type
Error Handling / Observability

## Severity
High

## Location
`src/managers.rs:47-62`

## Description
The `Managers::shutdown()` function logs warnings for individual manager failures but always returns `Ok(())`, even if all managers fail to shut down properly.

```rust
pub async fn shutdown(&self) -> Result<()> {
    log::info!("Shutting down {} managers in parallel", self.shutdown_hooks.len());

    let shutdown_futures: Vec<_> = self.shutdown_hooks
        .iter()
        .enumerate()
        .map(|(i, hook)| async move {
            if let Err(e) = hook.shutdown().await {
                log::warn!("Failed to shutdown manager {}: {}", i, e);
            }
        })
        .collect();

    join_all(shutdown_futures).await;
    Ok(())  // <-- Always returns Ok!
}
```

## Problems

1. **Lost error information**: Callers cannot determine if shutdown was successful. They receive `Ok(())` even if critical managers failed.

2. **Resource leaks invisible**: Failed manager shutdowns often mean leaked resources (processes, connections, file handles), but callers have no way to detect this.

3. **Cannot implement retry logic**: Callers cannot retry failed shutdowns because they don't know which managers failed.

4. **Poor debugging**: The error log only shows manager index (`i`), not any identifying information about what the manager does.

5. **Compounds task 004**: The calling code in `server.rs` also ignores errors, compounding this problem.

## Real-World Impact

Consider a server with:
- BrowserManager (manages Chrome processes)
- TunnelManager (manages SSH tunnels)
- DatabaseManager (manages connection pool)

If BrowserManager fails to shut down:
- Chrome processes leak
- Memory usage remains high
- On next startup, Chrome may fail to start ("profile in use")

But the caller sees `Ok(())` and has no idea there's a problem.

## Recommendation

### Option 1: Return aggregate error

```rust
pub async fn shutdown(&self) -> Result<()> {
    log::info!("Shutting down {} managers in parallel", self.shutdown_hooks.len());

    let results: Vec<Result<()>> = join_all(
        self.shutdown_hooks
            .iter()
            .enumerate()
            .map(|(i, hook)| async move {
                hook.shutdown().await
                    .map_err(|e| anyhow::anyhow!("Manager {} failed: {}", i, e))
            })
    ).await;

    let errors: Vec<_> = results.into_iter()
        .filter_map(|r| r.err())
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} managers failed to shutdown: {:?}",
            errors.len(),
            errors
        ))
    }
}
```

### Option 2: Return detailed results

```rust
pub struct ShutdownResult {
    pub total: usize,
    pub succeeded: usize,
    pub failed: Vec<(usize, anyhow::Error)>,
}

pub async fn shutdown(&self) -> ShutdownResult {
    // ... collect all results ...
    ShutdownResult { total, succeeded, failed }
}
```

### Option 3: Add manager names

```rust
pub struct Managers {
    shutdown_hooks: Vec<(String, Box<dyn ShutdownHook>)>,
}

pub fn register<H: ShutdownHook + 'static>(&mut self, name: String, hook: H) {
    self.shutdown_hooks.push((name, Box::new(hook)));
}

// In shutdown:
log::warn!("Failed to shutdown manager '{}': {}", name, e);
```

## Impact
- Error handling: High (critical errors are lost)
- Observability: High (operators can't see failures)
- Resource management: High (can't detect leaks)
- Production readiness: High (essential for production operations)
- Debugging: High (makes troubleshooting very difficult)
