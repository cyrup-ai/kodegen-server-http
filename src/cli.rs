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
    #[arg(long, value_name = "SECONDS", default_value = "30")]
    pub shutdown_timeout_secs: u64,
}

impl Cli {
    /// Get HTTP address, error if not provided
    pub fn http_address(&self) -> Result<SocketAddr> {
        self.http
            .context("--http flag is required for HTTP mode")
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
}
