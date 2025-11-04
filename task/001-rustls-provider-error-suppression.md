# Issue: Silently Ignored rustls Provider Installation Error

## Location
`src/lib.rs:85`

## Severity
**Medium** - Could lead to runtime failures if TLS is used

---

## Core Objective

Fix error suppression in rustls CryptoProvider installation to provide clear diagnostics when TLS initialization fails.

The current code silently ignores the result of `install_default()`, which can cause confusing runtime failures when TLS/HTTPS is enabled.

---

## Problem Analysis

### Current Implementation

[`src/lib.rs:85`](../src/lib.rs#L85):
```rust
// Install rustls CryptoProvider (required for HTTPS)
let _ = rustls::crypto::ring::default_provider().install_default();
```

### What `install_default()` Does

Per the [rustls crypto provider documentation](../tmp/rustls/rustls/src/crypto/mod.rs#L231-L240):

```rust
/// Sets this `CryptoProvider` as the default for this process.
///
/// This can be called successfully at most once in any process execution.
///
/// Call this early in your process to configure which provider is used for
/// the provider. The configuration should happen before any use of
/// [`ClientConfig::builder()`] or [`ServerConfig::builder()`].
pub fn install_default(self) -> Result<(), Arc<Self>>
```

**Return Type:** `Result<(), Arc<Self>>`
- **Success (`Ok(())`)**: Provider installed successfully
- **Error (`Err(Arc<Self>)`)**: A provider is already installed. The `Arc<Self>` contains the previously-installed provider.

### The Error Case is Usually Benign

The **only** way `install_default()` fails is if a provider was already installed. This can happen in:
1. **Test environments**: Multiple tests in the same process
2. **Library usage**: If this library is used by a larger application that already installed a provider
3. **Repeated calls**: If `run_http_server()` is somehow called multiple times in the same process

**This is not typically a critical error** - it means cryptographic functionality is available.

### The Real Problem: Silent Failures

The issue is **not** that we ignore the "already installed" error. The issue is that we ignore **ALL** errors, including legitimate failures that would prevent TLS from working.

While `install_default()` currently only returns errors for "already installed", **future rustls versions** could add other failure modes, and we'd silently ignore those too.

---

## Impact

### When TLS is Enabled

If the crypto provider isn't actually available (though currently unlikely):

1. User runs: `./server --http 0.0.0.0:8080 --tls-cert cert.pem --tls-key key.pem`
2. Line 85 silently fails (hypothetically)
3. Server appears to start successfully
4. Line 102 attempts to load TLS: `RustlsConfig::from_pem_file()`
5. **Fails with cryptic error**: "no crypto provider available" or similar
6. Error message doesn't point to root cause (line 85)

### Debugging Experience

```
ERROR Failed to load TLS configuration: no default CryptoProvider available
```

User thinks the problem is with their certificate files, but the actual issue was much earlier in initialization.

---

## Solution: Idempotent Provider Installation

### What Needs to Change

**File:** [`src/lib.rs:85`](../src/lib.rs#L85)

**Replace:**
```rust
let _ = rustls::crypto::ring::default_provider().install_default();
```

**With:**
```rust
rustls::crypto::ring::default_provider()
    .install_default()
    .or_else(|_existing_provider| {
        log::debug!(
            "rustls crypto provider already installed (likely by parent application or test harness)"
        );
        Ok::<(), Arc<rustls::crypto::CryptoProvider>>(())
    })?;
```

### Why This Pattern?

1. **Idempotent**: Succeeds whether provider is already installed or not
2. **Informative**: Logs when provider was already present (useful for debugging)
3. **Fail-fast**: If `install_default()` ever returns other errors in future rustls versions, they'll be propagated
4. **Type-safe**: Explicitly handles the `Arc<CryptoProvider>` error type

### Alternative (Simpler) Approach

If you don't care about logging:

```rust
let _ = rustls::crypto::ring::default_provider()
    .install_default()
    .or(Ok(()));  // Treat "already installed" as success
```

However, this still silently ignores the error. The logging version is better for operations.

---

## Reference Implementation

From [rustls tests](../tmp/rustls/rustls-test/tests/process_provider.rs):

```rust
provider::DEFAULT_PROVIDER
    .install_default()
    .expect("cannot install");

// Later in same process:
provider::DEFAULT_PROVIDER
    .install_default()
    .expect_err("install succeeded a second time");  // This is expected to fail
```

From [rustls post-quantum example](../tmp/rustls/rustls-post-quantum/examples/client.rs):

```rust
rustls_post_quantum::provider()
    .install_default()
    .unwrap();  // Panics if already installed (not ideal for libraries)
```

---

## Implementation Steps

### Step 1: Update `src/lib.rs`

**Location:** Line 84-85

**Current:**
```rust
// Install rustls CryptoProvider (required for HTTPS)
let _ = rustls::crypto::ring::default_provider().install_default();
```

**New:**
```rust
// Install rustls CryptoProvider (required for HTTPS)
// This is idempotent: if a provider is already installed (e.g., by a parent
// application), we log and continue rather than failing.
rustls::crypto::ring::default_provider()
    .install_default()
    .or_else(|_existing_provider| {
        log::debug!(
            "rustls crypto provider already installed (likely by parent application or test harness)"
        );
        Ok::<(), Arc<rustls::crypto::CryptoProvider>>(())
    })?;
```

### Step 2: Verify No Other Changes Needed

**Check:** Are there other places that call `install_default()`?

```bash
$ grep -rn "install_default" src/
src/lib.rs:85:    let _ = rustls::crypto::ring::default_provider().install_default();
```

✅ **Only one location** - no other changes needed.

**Check:** Where is TLS actually used?

```bash
$ grep -rn "tls\|TLS\|rustls" src/
src/lib.rs:85:    let _ = rustls::crypto::ring::default_provider().install_default();
src/server.rs:102: axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path)
```

✅ TLS is only used in [`src/server.rs:102`](../src/server.rs#L102) when loading certificates. That code already has proper error handling.

---

## Definition of Done

- [ ] Line 85 in `src/lib.rs` handles the `install_default()` result instead of suppressing it
- [ ] "Already installed" case is explicitly handled and logged at debug level
- [ ] Genuine failures (if any) propagate as errors with context
- [ ] No other code changes required (verified by grep)

---

## Dependencies

**Crates:** No new dependencies required.

The code uses:
- `rustls = "0.23"` (already in [`Cargo.toml:50`](../Cargo.toml#L50))
- `log` (already in [`Cargo.toml:46`](../Cargo.toml#L46))
- `Arc` from `std::sync` (standard library)

---

## Context: How rustls Providers Work

From [`tmp/rustls/rustls/src/crypto/mod.rs`](../tmp/rustls/rustls/src/crypto/mod.rs#L74-L93):

> There is the concept of an implicit default provider, configured at run-time once in
> a given process.
>
> It is used for functions like `ClientConfig::builder()` and `ServerConfig::builder()`.
>
> The intention is that an application can specify the `CryptoProvider` they wish to use
> once, and have that apply to the variety of places where their application does TLS
> (which may be wrapped inside other libraries).
> They should do this by calling `CryptoProvider::install_default()` early on.

**Key Point:** It's a **process-wide singleton**. Once set, it cannot be changed. This is why the error case exists.

### The Implementation (OnceLock)

From [`tmp/rustls/rustls/src/crypto/mod.rs#L295-L310`](../tmp/rustls/rustls/src/crypto/mod.rs):

```rust
pub(crate) fn install_default(
    default_provider: CryptoProvider,
) -> Result<(), Arc<CryptoProvider>> {
    PROCESS_DEFAULT_PROVIDER.set(Arc::new(default_provider))
}

static PROCESS_DEFAULT_PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
```

`OnceLock::set()` returns `Err` if already set, containing the value that was passed in (wrapped in Arc).

---

## Notes

- This change makes the code **more robust**, not just "clearer"
- The pattern used (`or_else` with `Ok(())`) is **idempotent** and safe
- **No behavior change** in normal operation - provider installs successfully
- **Better diagnostics** if running in test environments or as a library
