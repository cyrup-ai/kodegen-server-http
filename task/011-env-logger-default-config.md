# Default env_logger Configuration May Be Insufficient

## Issue Type
Configuration / Observability

## Severity
Low

## Location
`src/lib.rs:82`

## Description
The logging system is initialized with default configuration using `env_logger::init()`. This provides minimal control over log formatting, output, and filtering, which may be insufficient for production use.

```rust
// Initialize logging
env_logger::init();
```

## Problems

1. **Limited formatting**: Default format is basic and may not include useful information:
   - No request IDs or correlation IDs
   - Timestamps use default format
   - No color coding in terminals (unless RUST_LOG_STYLE is set)
   - No structured logging support

2. **No log rotation**: All logs go to stderr with no rotation or size limits. In production, this can:
   - Fill up disks
   - Make log analysis difficult
   - Impact performance with very large log files

3. **Limited filtering**: Only `RUST_LOG` environment variable for filtering. No way to:
   - Set different levels for different modules via CLI
   - Override log levels at runtime
   - Configure per-tool or per-request logging

4. **Production needs**: Production environments often need:
   - JSON-formatted logs for parsing
   - Log shipping to external systems
   - Metrics integration
   - Sampling or rate limiting for high-volume logs

5. **No context**: Logs don't include important context like:
   - Client ID (from MCP handshake)
   - Request ID
   - Tool name being executed
   - Session ID

## Example: Default vs Enhanced Logging

**Default:**
```
[2024-11-04T18:36:45Z WARN kodegen_server_http] Failed to shutdown manager 0: timeout
```

**Enhanced:**
```json
{
  "timestamp": "2024-11-04T18:36:45.123Z",
  "level": "WARN",
  "module": "kodegen_server_http",
  "message": "Failed to shutdown manager 'BrowserManager': timeout",
  "manager": "BrowserManager",
  "shutdown_phase": "managers",
  "client_id": "claude-desktop-1.2.3",
  "instance_id": "20241104-183645"
}
```

## Recommendation

### Option 1: Enhanced env_logger configuration

```rust
use env_logger::Builder;
use std::io::Write;

fn init_logging() {
    let mut builder = Builder::from_default_env();

    builder.format(|buf, record| {
        writeln!(
            buf,
            "[{} {} {}:{}] {}",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            record.level(),
            record.file().unwrap_or("unknown"),
            record.line().unwrap_or(0),
            record.args()
        )
    });

    builder.init();
}
```

### Option 2: Use tracing crate (recommended for modern Rust)

```rust
use tracing_subscriber::{
    fmt,
    prelude::*,
    EnvFilter,
};

fn init_logging() {
    tracing_subscriber::registry()
        .with(fmt::layer().json())  // JSON formatting
        .with(EnvFilter::from_default_env())
        .init();
}

// Then use tracing::info! instead of log::info!
// This provides structured logging with spans and context
```

### Option 3: Add CLI configuration

```rust
#[derive(Parser)]
pub struct Cli {
    // ... existing fields ...

    /// Log format (text, json, compact)
    #[arg(long, default_value = "text")]
    pub log_format: String,

    /// Log level override
    #[arg(long)]
    pub log_level: Option<String>,
}

fn init_logging(cli: &Cli) {
    let mut builder = Builder::new();

    // Set level from CLI or RUST_LOG
    if let Some(level) = &cli.log_level {
        builder.parse_filters(level);
    } else {
        builder.parse_default_env();
    }

    // Set format
    match cli.log_format.as_str() {
        "json" => builder.format_json(),
        "compact" => builder.format_compact(),
        _ => builder.format_default(),
    };

    builder.init();
}
```

## Real-World Issues

1. **Debugging in production**: Without request/session context, it's hard to trace a specific client's issues in multi-client logs.

2. **Log parsing**: Default format is hard to parse programmatically for log analysis tools.

3. **Performance**: At high log volumes, the default logger can become a bottleneck.

4. **Cloud integration**: Cloud platforms (AWS, GCP, Azure) expect structured logs for proper indexing.

## Impact
- Observability: Low to Medium (affects production debugging)
- Production readiness: Low (works but suboptimal)
- Developer experience: Low (better logging helps development)
- Performance: Very Low (only matters at high volume)

## Note

This is not a critical issue for initial releases, but becomes important as the system matures and runs in production environments with multiple clients and high load.
