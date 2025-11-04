# Issue: Inconsistent Shutdown Timeout Handling

## Location
`src/lib.rs:147-158`

## Severity
Low - Inconsistent behavior but not critical

## Description
When waiting for server shutdown to complete, timeout errors are handled inconsistently:

```rust
match handle.wait_for_completion(timeout).await {
    Ok(()) => {
        log::info!("{} server shutdown completed successfully", category);
    }
    Err(_elapsed) => {
        log::warn!(
            "{} server shutdown timeout ({:?}) elapsed before completion",
            category,
            timeout
        );
    }
}
```

After the match, execution continues normally and returns `Ok(())` regardless of whether shutdown completed or timed out.

## Problem
1. **Inconsistent semantics**: The function returns `Ok(())` even if shutdown timed out
2. **Hidden failures**: Callers (if any) can't distinguish between successful and partial shutdown
3. **Resource leaks**: If shutdown times out, resources may not be fully cleaned up, but this fact is hidden from callers

## Impact
- In automated/orchestrated environments, the process may appear to have shut down cleanly when it actually timed out
- Monitoring systems can't detect partial shutdown failures
- Could lead to resource leaks or zombie processes

## Recommendation
1. Return an error if shutdown times out, making the failure explicit
2. Or document clearly that timeout is treated as success (which seems semantically incorrect)
3. Consider whether the process should exit with an error code in timeout scenarios

## Example Fix
```rust
match handle.wait_for_completion(timeout).await {
    Ok(()) => {
        log::info!("{} server shutdown completed successfully", category);
    }
    Err(elapsed) => {
        let error = anyhow::anyhow!(
            "{} server shutdown timeout ({:?}) elapsed before completion",
            category,
            timeout
        );
        log::error!("{}", error);
        return Err(error);
    }
}
```
