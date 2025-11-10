use anyhow::Result;
use clap::Parser;
use kodegen_config_manager::ConfigManager;
use kodegen_utils::usage_tracker::UsageTracker;
use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
use rmcp::transport::streamable_http_server::session::local::{LocalSessionManager, SessionConfig};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub mod cli;
pub mod managers;
pub mod registration;
pub mod server;

pub use cli::Cli;
pub use managers::{Managers, ShutdownHook};
pub use registration::{register_tool, register_tool_arc};
pub use server::{HttpServer, ServerHandle, ShutdownError};

/// Container for routers and managers
///
/// Category servers build this and pass to run_http_server().
pub struct RouterSet<S>
where
    S: Send + Sync + 'static,
{
    pub tool_router: ToolRouter<S>,
    pub prompt_router: PromptRouter<S>,
    pub managers: Managers,
}

impl<S> RouterSet<S>
where
    S: Send + Sync + 'static,
{
    pub fn new(
        tool_router: ToolRouter<S>,
        prompt_router: PromptRouter<S>,
        managers: Managers,
    ) -> Self {
        Self {
            tool_router,
            prompt_router,
            managers,
        }
    }
}

/// Create HTTP server programmatically without CLI argument parsing
///
/// This is the library API for embedding servers in other applications.
/// Unlike run_http_server(), this does not parse CLI args or block on shutdown signals.
///
/// Returns a ServerHandle immediately - the server runs in background tasks.
/// Call handle.cancel() and handle.wait_for_completion() for graceful shutdown.
///
/// Example usage:
/// ```
/// use kodegen_server_http::{create_http_server, RouterSet, Managers, register_tool};
/// use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
/// use std::time::Duration;
///
/// let addr = "127.0.0.1:30437".parse()?;
/// let handle = create_http_server("filesystem", addr, None, Duration::from_secs(30), |config, tracker| {
///     Box::pin(async move {
///         let tool_router = ToolRouter::new();
///         let prompt_router = PromptRouter::new();
///         let managers = Managers::new();
///         // Register tools...
///         Ok(RouterSet::new(tool_router, prompt_router, managers))
///     })
/// }).await?;
///
/// // Server is now running in background tasks
/// // handle.cancel() to shutdown
/// ```
pub async fn create_http_server<F>(
    category: &str,
    addr: std::net::SocketAddr,
    tls_config: Option<(std::path::PathBuf, std::path::PathBuf)>,
    shutdown_timeout: Duration,
    register_tools: F,
) -> Result<ServerHandle>
where
    F: FnOnce(&ConfigManager, &UsageTracker) -> Pin<Box<dyn Future<Output = Result<RouterSet<HttpServer>>> + Send>>,
{
    // Install rustls CryptoProvider (idempotent)
    if rustls::crypto::ring::default_provider().install_default().is_err() {
        log::debug!("rustls crypto provider already installed");
    }

    // Initialize shared components
    let config_manager = ConfigManager::new();
    config_manager.init().await?;

    let timestamp = chrono::Utc::now();
    let pid = std::process::id();
    let instance_id = format!("{}-{}", timestamp.format("%Y%m%d-%H%M%S-%9f"), pid);
    let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));

    // Initialize global tool history tracking
    log::debug!("Initializing global tool history tracking for instance: {}", instance_id);
    kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;

    // Build routers using provided async registration function
    let routers = register_tools(&config_manager, &usage_tracker).await?;

    // Create session manager with production configuration
    let session_config = SessionConfig {
        channel_capacity: 16,
        keep_alive: Some(Duration::from_secs(3600)),  // 1 hour timeout
    };
    let session_manager = Arc::new(LocalSessionManager {
        sessions: Default::default(),
        session_config,
    });

    // Create HTTP server
    let server = HttpServer::new(
        routers.tool_router,
        routers.prompt_router,
        usage_tracker,
        config_manager,
        routers.managers,
        session_manager,
    );

    let protocol = if tls_config.is_some() { "https" } else { "http" };
    log::info!("Starting {} HTTP server on {}://{}", category, protocol, addr);

    // Start server (non-blocking - returns ServerHandle immediately)
    let handle = server.serve_with_tls(addr, tls_config, shutdown_timeout).await?;

    log::info!("{} server running on {}://{}", category, protocol, addr);
    
    Ok(handle)
}

/// Main entry point for category HTTP servers
///
/// Handles all boilerplate: CLI parsing, config initialization,
/// tool registration via callback, HTTP server setup, graceful shutdown.
///
/// Example usage in category server main.rs:
/// ```
/// use kodegen_server_http::{run_http_server, RouterSet, Managers, register_tool};
/// use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
///
/// #[tokio::main]
/// async fn main() -> Result<()> {
///     run_http_server("filesystem", |config, tracker| {
///         let tool_router = ToolRouter::new();
///         let prompt_router = PromptRouter::new();
///         let managers = Managers::new();
///         
///         // Register tools
///         let (tool_router, prompt_router) = register_tool(
///             tool_router,
///             prompt_router,
///             ReadFileTool::new(config.clone()),
///         );
///         
///         Ok(RouterSet::new(tool_router, prompt_router, managers))
///     }).await
/// }
/// ```
pub async fn run_http_server<F>(
    category: &str,
    register_tools: F,
) -> Result<()>
where
    F: FnOnce(&ConfigManager, &UsageTracker) -> Pin<Box<dyn Future<Output = Result<RouterSet<HttpServer>>> + Send>>,
{
    // Initialize logging
    env_logger::init();

    // Install rustls CryptoProvider (required for HTTPS)
    // This is idempotent: if a provider is already installed (e.g., by a parent
    // application), we log and continue rather than failing.
    if rustls::crypto::ring::default_provider().install_default().is_err() {
        log::debug!(
            "rustls crypto provider already installed (likely by parent application or test harness)"
        );
    }

    // Parse CLI arguments
    let cli = Cli::parse();

    // Initialize shared components
    let config_manager = ConfigManager::new();
    config_manager.init().await?;

    let timestamp = chrono::Utc::now();
    let pid = std::process::id();
    let instance_id = format!("{}-{}", timestamp.format("%Y%m%d-%H%M%S-%9f"), pid);
    let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));

    // Initialize global tool history tracking
    log::debug!("Initializing global tool history tracking for instance: {}", instance_id);
    kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;

    // Build routers using provided async registration function
    let routers = register_tools(&config_manager, &usage_tracker).await?;

    // Create session manager for stateful HTTP with production-ready configuration
    // Sessions automatically expire after 1 hour of inactivity to prevent memory growth
    let session_config = SessionConfig {
        channel_capacity: 16,
        keep_alive: Some(Duration::from_secs(3600)),  // 1 hour timeout
    };
    let session_manager = Arc::new(LocalSessionManager {
        sessions: Default::default(),
        session_config,
    });

    // Create HTTP server
    let server = HttpServer::new(
        routers.tool_router,
        routers.prompt_router,
        usage_tracker,
        config_manager,
        routers.managers,
        session_manager,
    );

    // Start server
    let addr = cli.http_address()?;
    let protocol = if cli.tls_config().is_some() {
        "https"
    } else {
        "http"
    };

    log::info!(
        "Starting {} HTTP server on {}://{}",
        category,
        protocol,
        addr
    );

    // Get shutdown timeout configuration
    let timeout = cli.shutdown_timeout();
    let handle = server.serve_with_tls(addr, cli.tls_config(), timeout).await?;

    log::info!("{} server running on {}://{}", category, protocol, addr);
    if cli.tls_config().is_some() {
        log::info!("TLS/HTTPS enabled - using encrypted connections");
    }
    log::info!("Press Ctrl+C or send SIGTERM to initiate graceful shutdown");

    // Wait for shutdown signal
    wait_for_shutdown_signal().await?;

    // Graceful shutdown
    log::info!(
        "Shutdown signal received, initiating graceful shutdown (timeout: {:?})",
        timeout
    );

    handle.cancel();

    match handle.wait_for_completion(timeout).await {
        Ok(()) => {
            log::info!("{} server shutdown completed successfully", category);
            log::info!("{} server stopped", category);
            Ok(())
        }
        
        Err(ShutdownError::Timeout(elapsed)) => {
            let error = anyhow::anyhow!(
                "{} server shutdown timeout ({:?}) - operations still in progress",
                category,
                elapsed
            );
            log::error!("{}", error);
            log::error!("Possible causes: slow request handlers, blocked manager cleanup, or stuck database connections");
            Err(error)
        }
        
        Err(ShutdownError::SignalLost) => {
            let error = anyhow::anyhow!(
                "{} server shutdown completion signal lost - monitor task may have panicked",
                category
            );
            log::error!("{}", error);
            log::error!("Check logs above for 'HTTP SERVER TASK EXITED UNEXPECTEDLY' or panic messages");
            log::error!("Shutdown may have completed successfully but signal was lost");
            Err(error)
        }
    }
}

async fn wait_for_shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let ctrl_c = tokio::signal::ctrl_c();
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sighup = signal(SignalKind::hangup())?;

        tokio::select! {
            _ = ctrl_c => {
                log::debug!("Received SIGINT (Ctrl+C)");
            }
            _ = sigterm.recv() => {
                log::debug!("Received SIGTERM");
            }
            _ = sighup.recv() => {
                log::debug!("Received SIGHUP");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
    }

    Ok(())
}
