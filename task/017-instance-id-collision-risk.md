# Issue: Instance ID Collision Risk (Second-Level Precision)

## Location
`src/lib.rs:94`

## Severity
Low - Collision risk in specific scenarios

## Description
Instance IDs are generated using timestamp with only second-level precision:

```rust
let instance_id = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
```

## Problem
If multiple server instances start within the same second, they will have identical instance IDs.

## Example Collision Scenarios

### Scenario 1: Automated Testing
```bash
# Run 10 test instances in parallel
for i in {1..10}; do
    ./server --http 127.0.0.1:$((8000 + i)) &
done

# All 10 servers started in the same second
# All have instance_id: "20250104-143052"
```

### Scenario 2: Container Orchestration
```yaml
# Kubernetes deployment with 5 replicas
replicas: 5

# All 5 pods start simultaneously
# All have identical instance_ids
```

### Scenario 3: Rapid Restart
```bash
# Server crashes and auto-restarts quickly
# New instance has same ID as old instance
./server &  # instance_id: 20250104-143052
kill %1
./server &  # instance_id: 20250104-143052 (same second!)
```

### Scenario 4: Blue-Green Deployment
```bash
# Start new version while old still running
./server-v1 &  # instance_id: 20250104-143052
./server-v2 &  # instance_id: 20250104-143052 (collision!)
```

## Impact on Tool History
Line 98 uses this instance_id for global history tracking:
```rust
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

If multiple instances have the same ID:
- **History corruption**: Tool calls from different instances mixed together
- **Metrics confusion**: Usage tracking can't distinguish instances
- **Debugging difficulty**: Logs from multiple servers appear to be one instance
- **State conflicts**: If history is persisted, instances overwrite each other's data

## Real-World Consequences

### Development/Testing
```rust
// Test runner starts 3 servers simultaneously
Server A: instance_id = "20250104-120000"
Server B: instance_id = "20250104-120000"  // Same!
Server C: instance_id = "20250104-120000"  // Same!

// All three write to same history file/database
// Tool history becomes garbled
// Test assertions fail mysteriously
```

### Production Monitoring
```
Dashboard shows:
- instance_id: 20250104-120000
- tool_calls: 1,500
- active_sessions: 50

But actually:
- 3 instances with same ID
- Can't tell them apart
- Aggregated metrics are wrong
```

### Log Correlation
```
[20250104-120000] Tool call: read_file("/etc/passwd")
[20250104-120000] Tool call: delete_file("/data/important")
[20250104-120000] Error: Permission denied

# Which instance had the error? Can't tell!
```

## Current Format
```
Format: YYYYMMDD-HHMMSS
Example: 20250104-143052
Precision: 1 second
Collision probability:
- 2 instances in same second: 100%
- 2 instances in same minute: ~1.7%
```

## Recommendations

### Option 1: Add Millisecond/Microsecond Precision (Simple)
```rust
let instance_id = chrono::Utc::now()
    .format("%Y%m%d-%H%M%S-%3f")  // Adds milliseconds
    .to_string();

// Example: 20250104-143052-847
// Collision probability with 2 instances: ~0.1%
```

Or nanoseconds:
```rust
let instance_id = chrono::Utc::now()
    .format("%Y%m%d-%H%M%S-%6f")  // Adds microseconds
    .to_string();

// Example: 20250104-143052-847293
// Collision probability: ~0.0001%
```

### Option 2: Add Random Component (Better)
```rust
use rand::Rng;

let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let random_suffix: u32 = rand::thread_rng().gen_range(0..10000);
let instance_id = format!("{}-{:04}", timestamp, random_suffix);

// Example: 20250104-143052-8472
// Collision probability: ~0.01% (1 in 10,000)
```

### Option 3: Use UUID (Most Robust)
```rust
use uuid::Uuid;

let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let unique_id = Uuid::new_v4().to_string().split('-').next().unwrap();
let instance_id = format!("{}-{}", timestamp, unique_id);

// Example: 20250104-143052-a7f3c92d
// Collision probability: negligible (cryptographically random)
```

### Option 4: Hostname + PID + Timestamp
```rust
use std::process;

let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let hostname = hostname::get()
    .ok()
    .and_then(|h| h.into_string().ok())
    .unwrap_or_else(|| "unknown".to_string());
let pid = process::id();
let instance_id = format!("{}-{}-{}", hostname, pid, timestamp);

// Example: server01-12345-20250104-143052
// Collision probability: very low (needs same host + PID + time)
```

### Option 5: Monotonic Counter (Thread-Safe)
```rust
use std::sync::atomic::{AtomicU32, Ordering};

static INSTANCE_COUNTER: AtomicU32 = AtomicU32::new(0);

let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let counter = INSTANCE_COUNTER.fetch_add(1, Ordering::SeqCst);
let instance_id = format!("{}-{:04}", timestamp, counter);

// Example: 20250104-143052-0000, 20250104-143052-0001, ...
// No collisions within same process
// But collisions across processes still possible
```

## Recommendation: Hybrid Approach
Combine timestamp with process PID and random component:

```rust
use rand::Rng;
use std::process;

let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let pid = process::id();
let random: u16 = rand::thread_rng().gen();
let instance_id = format!("{}-{}-{:04x}", timestamp, pid, random);

// Example: 20250104-143052-12345-a7f3
// Benefits:
// - Human-readable timestamp
// - Process distinguishable (PID)
// - Random component for same-PID restarts
// - Compact format
```

## Testing
Add test to verify uniqueness:
```rust
#[test]
fn test_instance_id_uniqueness() {
    let mut ids = std::collections::HashSet::new();

    // Generate 1000 IDs rapidly
    for _ in 0..1000 {
        let id = generate_instance_id();
        assert!(ids.insert(id), "Instance ID collision detected");
    }
}
```

## Backward Compatibility
If instance_id format is stored/logged:
1. Document new format
2. Update parsers if needed
3. Consider migration path
