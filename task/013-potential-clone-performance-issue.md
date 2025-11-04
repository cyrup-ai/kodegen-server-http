# Issue: Potential Clone Performance Issue with HttpServer

## Location
`src/server.rs:21-28, 74`

## Severity
Low-Medium - Depends on ConfigManager implementation

## Description
`HttpServer` derives `Clone` and contains a `ConfigManager` that is NOT wrapped in `Arc`:

```rust
#[derive(Clone)]
pub struct HttpServer {
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
    usage_tracker: UsageTracker,
    config_manager: kodegen_tools_config::ConfigManager,  // ← Not Arc<>
    managers: std::sync::Arc<crate::managers::Managers>,
}
```

## Problem
The server is cloned on line 74:
```rust
let service_factory = {
    let server = self.clone();
    move || Ok::<_, std::io::Error>(server.clone())
};
```

And potentially multiple times per request by the HTTP service layer.

## Impact Depends on ConfigManager Implementation

### If ConfigManager Contains Heavy Data
```rust
// Hypothetical bad implementation
pub struct ConfigManager {
    config: HashMap<String, String>,  // Clones entire config each time
    cache: Vec<CacheEntry>,           // Clones cache
    file_paths: Vec<PathBuf>,         // Clones paths
}
```
Then each HTTP request clone would:
- Copy entire config HashMap
- Duplicate cache
- Memory overhead scales with request count

### If ConfigManager Is Already Arc-Wrapped Internally
```rust
// Good implementation (unknown if this is the case)
pub struct ConfigManager {
    inner: Arc<ConfigManagerInner>,
}
```
Then cloning is cheap and this is not an issue.

## Why Other Fields Are Fine
- `tool_router: ToolRouter<Self>` - Likely Arc-wrapped routes internally
- `prompt_router: PromptRouter<Self>` - Same
- `usage_tracker: UsageTracker` - Likely Arc-wrapped or cheaply cloneable
- `managers: Arc<Managers>` - Explicitly Arc-wrapped ✓

## Investigation Needed
Check `kodegen_tools_config::ConfigManager`:
1. Is it cheaply cloneable?
2. Does it use Arc internally?
3. How large is the config data it holds?

## Potential Issues

### Memory Overhead
```
1 request  → 2 clones  → 2× config data
10 requests → 20 clones → 20× config data (if not Arc'd)
```

### Cache Incoherence
If ConfigManager has mutable state:
```rust
impl ConfigManager {
    pub fn update_config(&mut self, key: String, value: String) {
        self.cache.insert(key, value);
    }
}
```
Then cloning creates separate copies that don't see each other's updates.

## How to Check
```bash
# Look at ConfigManager implementation
grep -A 20 "pub struct ConfigManager"
grep -A 10 "impl Clone for ConfigManager"
```

## Recommendation

### Option 1: Wrap in Arc (Safe)
```rust
#[derive(Clone)]
pub struct HttpServer {
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
    usage_tracker: UsageTracker,
    config_manager: Arc<kodegen_tools_config::ConfigManager>,  // ← Wrapped
    managers: Arc<crate::managers::Managers>,
}
```

### Option 2: Verify ConfigManager Is Already Cheap
Document the assumption:
```rust
/// config_manager is cheaply cloneable (internal Arc-wrapping)
config_manager: kodegen_tools_config::ConfigManager,
```

### Option 3: Stop Cloning Server
Instead of cloning the server for each request, use Arc:
```rust
let server = Arc::new(self);
let service_factory = {
    let server = server.clone();
    move || Ok::<_, std::io::Error>((*server).clone())
};
```

## Performance Test
Benchmark under load:
```rust
// Measure memory usage
for _ in 0..1000 {
    let cloned = server.clone();
    std::hint::black_box(cloned);
}
```

Check if memory usage spikes or stays flat.
