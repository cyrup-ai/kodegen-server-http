# Issue: Silently Ignored rustls Provider Installation Error

## Location
`src/lib.rs:85`

## Severity
Medium - Could lead to runtime failures

## Description
The code silently ignores errors from `rustls::crypto::ring::default_provider().install_default()`:

```rust
let _ = rustls::crypto::ring::default_provider().install_default();
```

## Problem
If the default crypto provider installation fails, the program continues execution without TLS support. This could lead to:
- Runtime panics when attempting to use TLS/HTTPS later
- Unclear error messages far from the root cause
- Difficult debugging experience

## Impact
When TLS is configured via CLI args (`--tls-cert` and `--tls-key`), the server will fail at line 102-104 when trying to load the TLS configuration, but the actual issue is that the crypto provider wasn't installed.

## Recommendation
Either:
1. Propagate the error and fail fast if installation fails
2. Check if a provider is already installed and only warn if the installation fails due to duplicate installation
3. At minimum, log the error so it's visible in debugging

## Example Fix
```rust
if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
    log::warn!("Failed to install default rustls crypto provider (may already be installed): {:?}", e);
}
```

Or fail fast:
```rust
rustls::crypto::ring::default_provider()
    .install_default()
    .context("Failed to install rustls crypto provider")?;
```
