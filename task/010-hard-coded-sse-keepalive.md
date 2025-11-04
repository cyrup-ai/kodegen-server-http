# Hard-Coded SSE Keep-Alive Timeout

## Issue Type
Configuration / Performance

## Severity
Low

## Location
`src/server.rs:84`

## Description
The Server-Sent Events (SSE) keep-alive timeout is hard-coded to 15 seconds with no way to configure it.

```rust
StreamableHttpServerConfig {
    stateful_mode: true,
    sse_keep_alive: Some(Duration::from_secs(15)),
}
```

## Problems

1. **One size doesn't fit all**: Different deployments have different requirements:
   - Local development: Longer timeout OK (low bandwidth cost)
   - Cloud deployment: Shorter timeout may be better (reduce idle connection cost)
   - Mobile clients: May need more frequent keep-alives (NAT timeout)

2. **Network environment dependency**: The optimal keep-alive interval depends on:
   - Load balancer timeouts (often 30-60 seconds)
   - NAT/firewall timeouts (often 30-60 seconds)
   - Client preferences

3. **No way to disable**: Some deployments may want to disable keep-alive entirely and rely on client reconnection.

4. **Cost implications**: In cloud environments, unnecessary keep-alives waste bandwidth and connections, increasing costs.

## Background: Why Keep-Alive Matters

SSE connections are long-lived HTTP connections. Without keep-alive:
- Idle connections may be closed by intermediate proxies
- Clients can't distinguish between "no data" and "connection lost"
- NAT devices may drop the connection from their tables

The current 15-second interval may be:
- **Too frequent**: Wastes bandwidth if network is reliable
- **Too infrequent**: May not prevent proxy/NAT timeouts (often 30-60s)

## Common Configurations

Different scenarios need different settings:

| Scenario | Recommended Keep-Alive | Reason |
|----------|------------------------|--------|
| Behind AWS ALB | 30-45 seconds | ALB idle timeout is 60s |
| Behind nginx | 45-55 seconds | Default timeout is 60s |
| Direct to client | 20-30 seconds | Good balance |
| Mobile clients | 25-30 seconds | NAT traversal |
| Development | 60+ seconds or disabled | Reduce noise |

## Recommendation

Make this configurable via CLI or config file:

```rust
#[derive(Parser, Debug, Clone)]
pub struct Cli {
    // ... existing fields ...

    /// SSE keep-alive interval in seconds (0 to disable)
    #[arg(long, value_name = "SECONDS", default_value = "15")]
    pub sse_keepalive_secs: u64,
}

impl Cli {
    pub fn sse_keepalive(&self) -> Option<Duration> {
        if self.sse_keepalive_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(self.sse_keepalive_secs))
        }
    }
}

// Usage in server.rs:
StreamableHttpServerConfig {
    stateful_mode: true,
    sse_keep_alive: cli.sse_keepalive(),
}
```

Or make it part of `StreamableHttpServerConfig` passed to the library.

## Impact
- Performance: Low (minor bandwidth optimization possible)
- Flexibility: Low (nice to have for different deployments)
- Production readiness: Low (works fine as-is, but limits optimization)
