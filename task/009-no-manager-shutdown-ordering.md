# No Manager Shutdown Ordering or Dependencies

## Issue Type
Logic / Architecture

## Severity
Medium

## Location
`src/managers.rs:60`

## Description
All managers are shut down in parallel using `join_all`, with no way to specify shutdown order or dependencies between managers.

```rust
join_all(shutdown_futures).await;
```

## Problems

1. **Dependency conflicts**: Some managers may depend on others. For example:
   - A CacheManager may need to flush data to a DatabaseManager
   - A WorkerManager may need a MessageQueueManager to send final messages
   - A ProxyManager may need a TunnelManager for connectivity

2. **Race conditions**: If Manager A depends on Manager B, and they shut down in parallel, Manager A's shutdown may fail because Manager B is already gone.

3. **Resource ordering**: Some resources have natural ordering requirements:
   - Close HTTP clients before closing connection pools
   - Cancel background tasks before closing their communication channels
   - Flush buffers before closing file handles

4. **No way to express dependencies**: The API provides no mechanism to say "Manager A must shut down before Manager B".

## Real-World Scenarios

### Scenario 1: Database + Cache
```rust
// Cache writes to database on flush
managers.register(cache_manager);    // Depends on database
managers.register(database_manager);

// On shutdown (parallel):
// 1. cache_manager.shutdown() tries to flush to database
// 2. database_manager.shutdown() closes connections
// 3. Race: cache flush may fail if database closes first
```

### Scenario 2: Workers + Queue
```rust
// Workers send messages via queue
managers.register(worker_manager);   // Depends on queue
managers.register(queue_manager);

// On shutdown (parallel):
// 1. worker_manager.shutdown() tries to send final messages
// 2. queue_manager.shutdown() closes the queue
// 3. Race: final messages may be lost
```

### Scenario 3: Proxy + Tunnel
```rust
// Proxy uses tunnel for connectivity
managers.register(proxy_manager);    // Depends on tunnel
managers.register(tunnel_manager);

// On shutdown (parallel):
// 1. proxy_manager.shutdown() tries to close connections gracefully
// 2. tunnel_manager.shutdown() closes SSH tunnel
// 3. Race: proxy can't close gracefully if tunnel is gone
```

## Current Workaround

Developers must implement dependencies inside their managers:

```rust
impl ShutdownHook for CacheManager {
    async fn shutdown(&self) -> Result<()> {
        // Must manually wait for database to be ready
        // No API support for this
        self.flush().await?;
        Ok(())
    }
}
```

This is error-prone and requires tight coupling between managers.

## Recommendation

### Option 1: Add priority levels

```rust
pub enum ShutdownPriority {
    High,    // Shut down first (e.g., HTTP servers, workers)
    Normal,  // Shut down second (e.g., caches, proxies)
    Low,     // Shut down last (e.g., databases, queues, tunnels)
}

pub fn register_with_priority<H: ShutdownHook + 'static>(
    &mut self,
    hook: H,
    priority: ShutdownPriority,
) {
    // ...
}

// In shutdown:
// 1. Shut down all High priority managers (parallel)
// 2. Wait for all High to complete
// 3. Shut down all Normal priority managers (parallel)
// 4. Wait for all Normal to complete
// 5. Shut down all Low priority managers (parallel)
```

### Option 2: Explicit dependency graph

```rust
pub struct ManagerId(usize);

pub fn register<H: ShutdownHook + 'static>(&mut self, hook: H) -> ManagerId {
    let id = ManagerId(self.shutdown_hooks.len());
    self.shutdown_hooks.push(ManagerNode {
        hook: Box::new(hook),
        depends_on: vec![],
    });
    id
}

pub fn add_dependency(&mut self, manager: ManagerId, depends_on: ManagerId) {
    // ...
}

// Usage:
let db = managers.register(database_manager);
let cache = managers.register(cache_manager);
managers.add_dependency(cache, db);  // Cache depends on DB
```

Then use topological sort for shutdown order.

### Option 3: Shutdown phases (simpler)

```rust
pub enum ShutdownPhase {
    Phase1,  // User-facing services
    Phase2,  // Internal services
    Phase3,  // Infrastructure
}

// Shut down phase by phase
```

## Recommendation

**Start with Option 1 (priority levels)** because:
- ✅ Simple to understand and use
- ✅ Covers most real-world cases
- ✅ Easy to migrate from current API
- ✅ Low implementation complexity

Can add Option 2 later if needed.

## Impact
- Reliability: Medium (prevents race conditions)
- Code clarity: Medium (makes dependencies explicit)
- Production readiness: Medium (important for complex applications)
- Developer experience: Medium (easier to reason about shutdown)
