# Issue: LocalSessionManager May Not Be Optimal for Production

## Location
`src/server.rs:70`

## Severity
Low-Medium - Scalability and reliability concern

## Description
The server uses `LocalSessionManager::default()` for managing stateful HTTP sessions:

```rust
let session_manager = Arc::new(LocalSessionManager::default());
```

## Problem
`LocalSessionManager` (based on its name) likely stores session state in local memory, which has several limitations.

## Potential Issues

### 1. No Persistence
```
Scenario:
- Server restarts
- All active sessions are lost
- Clients must re-initialize
- In-progress operations fail
```

### 2. Memory Growth
```rust
// Sessions accumulate in memory
// If session cleanup is not aggressive, memory usage grows
// Eventually could cause OOM under high load
```

### 3. Single-Process Only
```
Cannot scale horizontally:
- Process 1 has sessions A, B, C
- Process 2 has sessions D, E, F
- Load balancer sends client to different process → session not found
```

### 4. No Session Recovery
```
If server crashes:
- All session state is lost
- Clients see cryptic "session not found" errors
- No graceful degradation possible
```

## When LocalSessionManager Is Acceptable

**Good for:**
- Development/testing
- Single-instance deployments
- Low traffic scenarios
- Stateless or loosely-stateful APIs

**Not good for:**
- Multi-instance deployments
- High availability requirements
- Long-running sessions
- Critical production systems

## Investigation Needed

Check `LocalSessionManager` implementation:
1. Does it have session expiration/cleanup?
2. What is the memory overhead per session?
3. Is there a maximum session limit?
4. How does it handle session conflicts?

## Example Session Management Issues

### Issue 1: Session Leak
```rust
// Client creates session
let session = client.initialize().await?;

// Client disconnects without cleanup
// Session remains in memory forever?
// Memory leak accumulates over time
```

### Issue 2: Load Balancer Incompatibility
```
Request 1 → Server Instance A → Session ABC stored in A's memory
Request 2 → Server Instance B → "Session ABC not found"
```

### Issue 3: Restart During Active Session
```
Client:  POST /mcp/tools/call (session=ABC)
Server:  Processing... (crashes)
Server:  Restarts
Client:  POST /mcp/tools/call (session=ABC)
Server:  "Session not found" (lost in crash)
```

## Recommendations

### Option 1: Document Limitations
If LocalSessionManager is intentional for simple deployments:
```rust
/// Uses LocalSessionManager for session storage.
///
/// LIMITATIONS:
/// - Sessions are stored in memory (not persistent across restarts)
/// - Not suitable for multi-instance deployments
/// - Sessions lost on server crash
/// - For production HA deployments, consider implementing a distributed session store
let session_manager = Arc::new(LocalSessionManager::default());
```

### Option 2: Make Session Manager Configurable
```rust
pub async fn serve_with_tls<SM>(
    self,
    addr: SocketAddr,
    tls_config: Option<(PathBuf, PathBuf)>,
    session_manager: Arc<SM>,
) -> Result<ServerHandle>
where
    SM: SessionManager + Send + Sync + 'static,
{
    // Use provided session manager instead of hardcoded LocalSessionManager
}
```

### Option 3: Implement Persistent Session Manager
For production:
```rust
// Redis-backed session manager
pub struct RedisSessionManager {
    client: redis::Client,
    ttl: Duration,
}

// Database-backed session manager
pub struct DbSessionManager {
    pool: PgPool,
}

// Hybrid: Local cache + persistent backend
pub struct HybridSessionManager {
    local: LocalSessionManager,
    persistent: Box<dyn SessionManager>,
}
```

### Option 4: Add Session Cleanup
If keeping LocalSessionManager, ensure proper cleanup:
```rust
impl LocalSessionManager {
    pub fn new(max_sessions: usize, ttl: Duration) -> Self {
        // ...
    }

    pub async fn cleanup_expired(&self) {
        // Remove sessions older than TTL
    }
}

// In server startup
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        session_manager.cleanup_expired().await;
    }
});
```

## Questions to Answer

1. What is the expected session lifetime?
2. How many concurrent sessions are expected?
3. Is horizontal scaling required?
4. What happens on server restart during active sessions?
5. Are sessions critical or can they be lost?

## Related Considerations

- Session expiration policy
- Session storage size limits
- Concurrent session limit per client
- Session hijacking protection
- Session cleanup strategy
