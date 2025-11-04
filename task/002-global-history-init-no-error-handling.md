# Issue: Global Tool History Initialization Without Visibility

## Location
`src/lib.rs:104`

## Severity
**Low** - Best-effort feature with hidden initialization, not critical to server operation

---

## Core Objective

Add logging to confirm successful initialization of the global tool history tracking system, providing visibility into feature status.

The current code calls `init_global_history()` silently, with no indication in logs whether the feature initialized successfully or encountered issues.

---

## Problem Analysis

### Current Implementation

[`src/lib.rs:104`](../src/lib.rs#L104):
```rust
// Initialize global tool history tracking
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

### What `init_global_history()` Actually Does

From [`tmp/kodegen_mcp_tool/src/tool_history.rs:405-409`](../tmp/kodegen_mcp_tool/src/tool_history.rs#L405-L409):

```rust
/// Initialize the global tool history instance (call once in main.rs)
pub async fn init_global_history(instance_id: String) -> &'static ToolHistory {
    TOOL_HISTORY
        .get_or_init(|| async move { ToolHistory::new(instance_id).await })
        .await
}
```

**Return Type:** `&'static ToolHistory`
- **Always succeeds** - returns a reference, never an error
- Uses `OnceCell::get_or_init()` which is **idempotent** (safe to call multiple times)
- **Cannot fail** in the sense of returning an error

### But Internal Failures ARE Hidden

From [`tmp/kodegen_mcp_tool/src/tool_history.rs:60-72`](../tmp/kodegen_mcp_tool/src/tool_history.rs#L60-L72):

```rust
pub async fn new(instance_id: String) -> Self {
    // Determine history file location
    let history_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))  // ← Fallback to current dir
        .join("kodegen-mcp");

    // Create directory if needed (async)
    if let Err(e) = tokio::fs::create_dir_all(&history_dir).await {
        let bufwtr = BufferWriter::stderr(ColorChoice::Auto);
        let mut buffer = bufwtr.buffer();
        let _ = writeln!(&mut buffer, "Failed to create history directory: {e}");
        let _ = bufwtr.print(&buffer);  // ← Goes to stderr, not logs!
    }
    // ... continues regardless
}
```

And from [`tmp/kodegen_mcp_tool/src/tool_history.rs:185-216`](../tmp/kodegen_mcp_tool/src/tool_history.rs#L185-L216):

```rust
async fn load_from_disk(&self) {
    // ... file reading logic ...

    Err(e) => {
        let bufwtr = BufferWriter::stderr(ColorChoice::Auto);
        let mut buffer = bufwtr.buffer();
        let _ = writeln!(&mut buffer, "Failed to load tool history: {e}");
        let _ = bufwtr.print(&buffer);  // ← stderr, not logs!
    }
}
```

### The Real Issue: Silent Initialization

The function uses **best-effort** error handling:
1. Directory creation fails → fallback to current directory
2. File loading fails → start with empty history
3. All errors go to **stderr**, not the logging system
4. Server continues normally regardless

**Problem:** No visibility in application logs whether:
- History feature is working
- History file location
- Any initialization warnings

---

## Impact

### When Running Normally
```
$ ./server --http 0.0.0.0:8080
INFO Starting filesystem HTTP server on http://0.0.0.0:8080
INFO filesystem server running on http://0.0.0.0:8080
```

**Missing:** "Tool history initialized at /home/user/.config/kodegen-mcp/tool-history_20251104-143052.jsonl"

### When Directory Creation Fails
```
$ ./server --http 0.0.0.0:8080
Failed to create history directory: Permission denied  ← stderr only!
INFO Starting filesystem HTTP server on http://0.0.0.0:8080
```

User has no idea history is degraded unless they see stderr.

### In Production with Log Aggregation
- Stderr messages may be lost or in separate stream
- No correlation with structured logs
- Can't monitor history feature health
- Difficult to diagnose "missing history" issues

---

## Why We Can't "Handle Errors"

The task description says "Check the return type and handle potential errors" but this is **impossible** because:

**From the API:**
```rust
pub async fn init_global_history(instance_id: String) -> &'static ToolHistory
```

- Returns `&'static ToolHistory`, not `Result<&'static ToolHistory, Error>`
- Cannot return an error by design
- Best-effort architecture: always provides a `ToolHistory` instance, even if degraded

**This is intentional** - tool history is a **non-critical telemetry feature**, not a required component. The library design ensures the server never fails due to history issues.

---

## Solution: Add Observability Logging

### What Needs to Change

**File:** [`src/lib.rs:104`](../src/lib.rs#L104)

**Current:**
```rust
// Initialize global tool history tracking
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

**New:**
```rust
// Initialize global tool history tracking
let tool_history = kodegen_mcp_tool::tool_history::init_global_history(instance_id.clone()).await;
log::debug!(
    "Initialized global tool history tracking (instance: {}, file: {:?})",
    instance_id,
    tool_history.history_file()
);
```

**Note:** We need to check if `ToolHistory` exposes a method to get the history file path. Let me check...

From [`tmp/kodegen_mcp_tool/src/tool_history.rs`](../tmp/kodegen_mcp_tool/src/tool_history.rs), the struct has a `history_file` field but it's not public. We'll need a simpler approach.

### Revised Solution (Simple)

```rust
// Initialize global tool history tracking
kodegen_mcp_tool::tool_history::init_global_history(instance_id.clone()).await;
log::debug!("Initialized global tool history tracking for instance: {}", instance_id);
```

This provides:
1. **Confirmation** that initialization completed
2. **Instance ID** for correlation
3. **Timestamp** (from log system)
4. **Visibility** in structured logs

---

## Implementation Steps

### Step 1: Add Logging After Initialization

**Location:** [`src/lib.rs:103-104`](../src/lib.rs#L103-L104)

**Current:**
```rust
// Initialize global tool history tracking
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

**New:**
```rust
// Initialize global tool history tracking
kodegen_mcp_tool::tool_history::init_global_history(instance_id.clone()).await;
log::debug!("Initialized global tool history tracking for instance: {}", instance_id);
```

**Note:** We need to clone `instance_id` because it's moved into the function above, but we also use it below at line 101.

Actually, let me check the code flow...

From [`src/lib.rs:100-104`](../src/lib.rs#L100-L104):
```rust
let instance_id = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));

// Initialize global tool history tracking
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

The `instance_id` is moved into `init_global_history()`, so we need to either:
1. Clone before passing, or
2. Log using the moved value

**Correction - Check if it's actually moved:**

Let me verify by checking the function signature again. It takes `String` by value, so yes it's moved. But we can still log before the call, or clone.

### Revised Implementation

**Option 1: Log before (shows intent):**
```rust
let instance_id = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));

// Initialize global tool history tracking
log::debug!("Initializing global tool history tracking for instance: {}", instance_id);
kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

**Option 2: Clone and log after (confirms completion):**
```rust
let instance_id = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));

// Initialize global tool history tracking
kodegen_mcp_tool::tool_history::init_global_history(instance_id.clone()).await;
log::debug!("Global tool history initialized for instance: {}", instance_id);
```

**Recommendation:** Option 1 (log before) because:
- Still shows the instance ID
- No performance cost of cloning
- Sufficient for debugging/monitoring

### Step 2: Verify No Other Changes Needed

**Check:** Where else is tool history used?

```bash
$ grep -rn "tool_history\|init_global_history" src/
src/lib.rs:104:    kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;
```

✅ **Only one location** - no other changes needed.

---

## Definition of Done

- [ ] Line 104 in `src/lib.rs` has debug logging before calling `init_global_history()`
- [ ] Log message includes the instance ID for correlation
- [ ] No other code changes required (verified by grep)
- [ ] No cloning needed (log before the call)

---

## Dependencies

**Crates:** No new dependencies required.

The code uses:
- `kodegen_mcp_tool = { version = "0.1" }` (already in [`Cargo.toml:23`](../Cargo.toml#L23))
- `log` (already in [`Cargo.toml:46`](../Cargo.toml#L46))
- `chrono` (already in [`Cargo.toml:47`](../Cargo.toml#L47))

---

## Context: How Tool History Works

From [`tmp/kodegen_mcp_tool/src/tool_history.rs`](../tmp/kodegen_mcp_tool/src/tool_history.rs):

### Architecture

```rust
static TOOL_HISTORY: OnceCell<ToolHistory> = OnceCell::const_new();

pub async fn init_global_history(instance_id: String) -> &'static ToolHistory {
    TOOL_HISTORY
        .get_or_init(|| async move { ToolHistory::new(instance_id).await })
        .await
}
```

**Key Points:**
- **Process-wide singleton** stored in a static `OnceCell`
- **Idempotent:** Multiple calls return the same instance
- **Thread-safe:** `OnceCell` ensures single initialization
- **Best-effort:** Never fails, degrades gracefully

### Initialization Flow

1. Determines config directory (`~/.config/kodegen-mcp` or fallback to `.`)
2. Creates directory (logs to stderr if fails, continues)
3. Creates history file path: `tool-history_{instance_id}.jsonl`
4. Loads existing history from disk (logs to stderr if fails, continues)
5. Starts background writer task for async persistence
6. Returns reference to initialized history

### Error Handling Philosophy

The library uses **graceful degradation**:
- Directory creation fails → use current directory
- File loading fails → start with empty history
- File writing fails → keep working in-memory only

**This is appropriate** for a telemetry feature that should never break the main application.

---

## Why This Task is Low Severity

1. **Not a bug** - the code works as designed
2. **Feature is best-effort** - tool history is telemetry, not critical functionality
3. **No data loss risk** - worst case is missing history entries
4. **No security impact** - just visibility into a non-critical feature
5. **Easy fix** - single line of logging

The change improves **operational visibility** and **debugging experience**, not correctness.

---

## Notes

- This change makes initialization **observable**, not "handled"
- The function cannot return errors by design (returns `&'static ToolHistory`)
- Best-effort error handling is intentional and appropriate
- Log level should be `debug` not `info` (non-critical feature)
- No behavior change - just adds visibility
