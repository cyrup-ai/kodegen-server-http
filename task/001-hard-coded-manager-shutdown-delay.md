# Hard-Coded Manager Shutdown Delay

## Issue Type
Performance / Configuration

## Severity
Medium

## Location
`src/server.rs:139`

## Description
The manager shutdown delay is hard-coded to 2000 milliseconds (2 seconds). This delay is intended to allow in-flight HTTP requests to complete before manager cleanup begins, but the fixed duration may not be appropriate for all use cases.

```rust
tokio::time::sleep(Duration::from_millis(2000)).await;
```

## Problems

1. **Inflexible timing**: Different applications may have different request completion times. Some may need less than 2 seconds, others may need more.

2. **No relationship to shutdown timeout**: The user can configure `--shutdown-timeout-secs` but this 2-second delay is independent of that setting.

3. **Performance impact**: If the application typically has short requests (e.g., < 500ms), a 2-second delay unnecessarily extends shutdown time.

4. **Reliability risk**: If requests typically take > 2 seconds, managers may start shutting down while requests are still being processed, potentially causing errors.

## Recommendation

Make this delay configurable, either:
- As a CLI parameter (e.g., `--manager-shutdown-delay-millis`)
- As a percentage of the overall shutdown timeout
- As part of the `StreamableHttpServerConfig` or a new `ShutdownConfig` struct

## Example Fix

```rust
pub struct ShutdownConfig {
    pub manager_delay: Duration,
    pub http_timeout: Duration,
}

// In serve_with_tls signature:
pub async fn serve_with_tls(
    self,
    addr: SocketAddr,
    tls_config: Option<(PathBuf, PathBuf)>,
    shutdown_config: ShutdownConfig,
) -> Result<ServerHandle>
```

## Impact
- Runtime performance: Medium (affects shutdown latency)
- Production readiness: Medium (could cause issues in production with long-running requests)
