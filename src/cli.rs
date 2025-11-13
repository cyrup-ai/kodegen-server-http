use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use anyhow::{Result, Context};

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// HTTP server bind address (e.g., 127.0.0.1:8080)
    #[arg(long, value_name = "ADDRESS")]
    pub http: Option<SocketAddr>,

    /// Path to TLS certificate file (enables HTTPS)
    #[arg(long, value_name = "PATH", requires = "tls_key")]
    pub tls_cert: Option<PathBuf>,

    /// Path to TLS private key file
    #[arg(long, value_name = "PATH", requires = "tls_cert")]
    pub tls_key: Option<PathBuf>,

    /// Graceful shutdown timeout in seconds
    ///
    /// Timeout budget allocation:
    /// - 70% allocated for HTTP connection draining (graceful close)
    /// - 30% reserved for request completion and manager cleanup
    ///
    /// Example with 30s timeout:
    /// - HTTP connections: 21 seconds to close gracefully
    /// - Cleanup phase: 9 seconds (request drain + manager shutdown)
    ///
    /// Example with 10s timeout:
    /// - HTTP connections: 7 seconds
    /// - Cleanup phase: 3 seconds
    ///
    /// Note: Request drain and manager timeouts are independent but must
    /// fit within the cleanup buffer. See server.rs for details.
    #[arg(long, value_name = "SECONDS", default_value = "30")]
    pub shutdown_timeout_secs: u64,

    /// Session keep-alive timeout in seconds (0 or omit = infinite, default: infinite)
    ///
    /// Controls how long idle HTTP sessions remain active before expiring.
    /// - 0 or omitted: Infinite - sessions never timeout (recommended)
    /// - Positive value: Sessions expire after N seconds of inactivity
    ///
    /// Examples:
    ///   --keep-alive 3600    # 1 hour timeout
    ///   --keep-alive 0       # Infinite (same as omitting flag)
    ///
    /// Note: Infinite keep-alive is recommended. Sessions are still cleaned up
    /// when clients disconnect or the server restarts.
    #[arg(long, value_name = "SECONDS")]
    pub keep_alive: Option<u64>,
}

impl Cli {
    /// Get HTTP address with validation
    pub fn http_address(&self) -> Result<SocketAddr> {
        let addr = self.http
            .context("--http flag is required for HTTP mode")?;

        // Validate privileged ports
        if addr.port() < 1024 {
            anyhow::bail!(
                "Port {} requires elevated privileges (root/sudo).\n\
                 Use ports >= 1024 for unprivileged operation, e.g., --http {}:30437",
                addr.port(),
                addr.ip()
            );
        }

        // Validate port 0 (OS-assigned ports break MCP client config)
        if addr.port() == 0 {
            anyhow::bail!(
                "Port 0 is not allowed (OS-assigned ports not supported).\n\
                 Specify an explicit port, e.g., --http {}:30437",
                addr.ip()
            );
        }

        // Warn about wildcard binding security implications
        if addr.ip().is_unspecified() {
            log::warn!(
                "Binding to {} exposes server on all network interfaces.",
                addr.ip()
            );
            log::warn!(
                "For local-only access, use: --http 127.0.0.1:{}",
                addr.port()
            );
        }

        Ok(addr)
    }

    /// Get TLS configuration if both cert and key provided
    pub fn tls_config(&self) -> Option<(PathBuf, PathBuf)> {
        match (&self.tls_cert, &self.tls_key) {
            (Some(cert), Some(key)) => Some((cert.clone(), key.clone())),
            _ => None,
        }
    }

    /// Get shutdown timeout duration
    pub fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.shutdown_timeout_secs)
    }

    /// Convert CLI keep-alive argument to SessionConfig duration
    ///
    /// - None or Some(0) → None (infinite keep-alive)
    /// - Some(n) → Some(Duration::from_secs(n))
    pub fn session_keep_alive(&self) -> Option<Duration> {
        match self.keep_alive {
            None => None,  // Not specified = infinite (default)
            Some(0) => None,  // Explicit 0 = infinite
            Some(n) => Some(Duration::from_secs(n)),  // Positive value = timeout
        }
    }
}
