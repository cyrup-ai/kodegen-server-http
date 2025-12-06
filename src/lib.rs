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
pub mod memory;
pub mod monitor;
pub mod registration;
pub mod server;

pub use cli::Cli;
pub use managers::{Managers, ShutdownHook};
pub use registration::{register_tool, register_tool_arc};
pub use server::{HttpServer, ServerHandle, ShutdownError};

/// Type alias for async connection cleanup callback
///
/// Called when a connection drops to cleanup connection-specific resources.
/// The callback receives the connection_id and performs async cleanup.
pub type ConnectionCleanupFn = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> 
    + Send 
    + Sync
>;

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
    /// Optional async cleanup callback invoked when connection drops
    pub connection_cleanup: Option<ConnectionCleanupFn>,
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
            connection_cleanup: None,
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
/// # Arguments
/// * `category` - Server category name for logging
/// * `addr` - Socket address to bind to
/// * `tls_config` - Optional TLS certificate and key paths
/// * `shutdown_timeout` - Graceful shutdown timeout duration
/// * `session_keep_alive` - Session keep-alive timeout (Duration::ZERO = infinite, recommended)
/// * `register_tools` - Async function to register tools and build routers
///
/// Example usage:
/// ```no_run
/// # use anyhow::Result;
/// # use kodegen_server_http::{create_http_server, RouterSet, Managers};
/// # use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
/// # use std::time::Duration;
/// #
/// # #[tokio::main]
/// # async fn main() -> Result<()> {
/// let addr = "127.0.0.1:30437".parse()?;
/// let handle = create_http_server(
///     "filesystem", addr, None,
///     Duration::from_secs(30), Duration::ZERO,
///     |config, tracker| {
///         Box::pin(async move {
///             let tool_router = ToolRouter::new();
///             let prompt_router = PromptRouter::new();
///             let managers = Managers::new();
///             Ok(RouterSet::new(tool_router, prompt_router, managers))
///         })
///     }
/// ).await?;
///
/// // Server is now running in background tasks
/// // handle.cancel() to shutdown
/// # Ok(())
/// # }
/// ```
pub async fn create_http_server<F>(
    category: &str,
    addr: std::net::SocketAddr,
    tls_config: Option<(std::path::PathBuf, std::path::PathBuf)>,
    shutdown_timeout: Duration,
    session_keep_alive: Duration,
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
    kodegen_mcp_schema::tool::tool_history::init_global_history(instance_id).await;

    // Build routers using provided async registration function
    let routers = register_tools(&config_manager, &usage_tracker).await?;

    // Create session manager with production configuration
    let session_config = SessionConfig {
        channel_capacity: 16,
        keep_alive: if session_keep_alive.is_zero() {
            None  // Zero duration = infinite keep-alive
        } else {
            Some(session_keep_alive)
        },
    };

    // Log configured keep-alive for observability
    if session_keep_alive.is_zero() {
        log::info!("Session keep-alive: infinite (no timeout)");
    } else {
        log::info!("Session keep-alive: {:?}", session_keep_alive);
    }

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
        routers.connection_cleanup,
    );

    let protocol = if tls_config.is_some() { "https" } else { "http" };
    log::info!("Starting {} HTTP server on {}://{}", category, protocol, addr);

    // Start server (non-blocking - returns ServerHandle immediately)
    let handle = server.serve_with_tls(addr, tls_config, shutdown_timeout).await?;

    log::info!("{} server running on {}://{}", category, protocol, addr);
    
    Ok(handle)
}

/// Create HTTP server using a pre-bound listener (TOCTOU-safe variant)
///
/// This is identical to `create_http_server()` except it accepts a pre-bound
/// TcpListener instead of a SocketAddr. Use this when port cleanup is required
/// to eliminate TOCTOU race conditions.
///
/// # Arguments
/// * `category` - Server category name (for logging and instance ID)
/// * `listener` - Pre-bound TcpListener (port already reserved)
/// * `tls_config` - Optional (cert_path, key_path) for HTTPS
/// * `shutdown_timeout` - Graceful shutdown timeout
/// * `session_keep_alive` - Session keep-alive duration (zero = infinite)
/// * `register_tools` - Async closure to register tools and prompts
///
/// # Returns
/// ServerHandle for graceful shutdown, or error if startup fails
///
/// # Example
/// ```rust
/// let listener = cleanup_and_reserve_port(30438).await?;
/// 
/// let handle = create_http_server_with_listener(
///     "filesystem",
///     listener,
///     None, // no TLS
///     Duration::from_secs(30),
///     Duration::ZERO,
///     |config, tracker| {
///         Box::pin(async move {
///             // Register tools...
///             Ok(RouterSet { tool_router, prompt_router, managers })
///         })
///     }
/// ).await?;
/// ```
pub async fn create_http_server_with_listener<F>(
    category: &str,
    listener: tokio::net::TcpListener,
    tls_config: Option<(std::path::PathBuf, std::path::PathBuf)>,
    shutdown_timeout: Duration,
    session_keep_alive: Duration,
    register_tools: F,
) -> Result<ServerHandle>
where
    F: FnOnce(&ConfigManager, &UsageTracker) -> Pin<Box<dyn Future<Output = Result<RouterSet<HttpServer>>> + Send>>,
{
    // Install rustls CryptoProvider (idempotent)
    if rustls::crypto::ring::default_provider().install_default().is_err() {
        log::debug!("rustls crypto provider already installed");
    }

    // Get address from pre-bound listener
    let addr = listener.local_addr()
        .map_err(|e| anyhow::anyhow!("Failed to get listener address: {}", e))?;

    // Initialize shared components
    let config_manager = ConfigManager::new();
    config_manager.init().await?;

    let timestamp = chrono::Utc::now();
    let pid = std::process::id();
    let instance_id = format!("{}-{}", timestamp.format("%Y%m%d-%H%M%S-%9f"), pid);
    let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));

    // Initialize global tool history tracking
    log::debug!("Initializing global tool history for instance: {}", instance_id);
    kodegen_mcp_schema::tool::tool_history::init_global_history(instance_id).await;

    // Build routers using provided async registration function
    let routers = register_tools(&config_manager, &usage_tracker).await?;

    // Create session manager with production configuration
    let session_config = SessionConfig {
        channel_capacity: 16,
        keep_alive: if session_keep_alive.is_zero() {
            None  // Zero duration = infinite keep-alive
        } else {
            Some(session_keep_alive)
        },
    };

    // Log configured keep-alive
    if session_keep_alive.is_zero() {
        log::info!("Session keep-alive: infinite (no timeout)");
    } else {
        log::info!("Session keep-alive: {:?}", session_keep_alive);
    }

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
        routers.connection_cleanup,
    );

    let protocol = if tls_config.is_some() { "https" } else { "http" };
    log::info!("Starting {} HTTP server on {}://{} (pre-bound listener)", category, protocol, addr);

    // Start server with pre-bound listener (eliminates TOCTOU race)
    let handle = server.serve_with_listener(listener, tls_config, shutdown_timeout).await?;

    log::info!("{} server started successfully on {}", category, addr);
    Ok(handle)
}

/// Main entry point for category HTTP servers
///
/// Handles all boilerplate: CLI parsing, config initialization,
/// tool registration via callback, HTTP server setup, graceful shutdown.
///
/// Example usage in category server main.rs:
/// ```no_run
/// # use anyhow::Result;
/// # use kodegen_server_http::{run_http_server, RouterSet, Managers, register_tool};
/// # use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
/// # use kodegen_mcp_schema::{Tool, ToolExecutionContext};
/// # use kodegen_config_manager::ConfigManager;
/// # use kodegen_mcp_schema::McpError;
/// # use rmcp::model::{Content, PromptArgument, PromptMessage};
/// # use serde_json::Value;
/// #
/// # #[derive(Clone)]
/// # struct ReadFileTool { config: ConfigManager }
/// # impl ReadFileTool {
/// #     fn new(_limit: usize, config: ConfigManager) -> Self { Self { config } }
/// # }
/// # impl Tool for ReadFileTool {
/// #     type Args = Value;
/// #     type PromptArgs = Value;
/// #     fn name() -> &'static str { "fs_read_file" }
/// #     fn description() -> &'static str { "Read file" }
/// #     async fn execute(&self, _args: Self::Args, _ctx: ToolExecutionContext) -> Result<Vec<Content>, McpError> {
/// #         Ok(vec![])
/// #     }
/// #     fn prompt_arguments() -> Vec<PromptArgument> { vec![] }
/// #     async fn prompt(&self, _args: Self::PromptArgs) -> Result<Vec<PromptMessage>, McpError> {
/// #         Ok(vec![])
/// #     }
/// # }
/// #
/// #[tokio::main]
/// async fn main() -> Result<()> {
///     run_http_server("filesystem", |config, tracker| {
///         let config = config.clone();
///         Box::pin(async move {
///             let tool_router = ToolRouter::new();
///             let prompt_router = PromptRouter::new();
///             let managers = Managers::new();
///
///             let (tool_router, prompt_router) = register_tool(
///                 tool_router, prompt_router,
///                 ReadFileTool::new(2000, config),
///             );
///
///             Ok(RouterSet::new(tool_router, prompt_router, managers))
///         })
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
    // Initialize logging with chromiumoxide CDP error filtering and tantivy spam reduction
    // Suppress internal chromiumoxide errors from outdated CDP definitions (Chromium 107)
    // while modern Chrome browsers send newer CDP messages causing benign deserialization failures
    // References:
    //   - https://github.com/mattsse/chromiumoxide/issues/229
    //   - https://github.com/mattsse/chromiumoxide/issues/167
    //
    // Suppress tantivy internal indexer INFO logs (commit, segment management, file watching)
    // These are noisy implementation details that spam logs during crawls (10+ messages per page)
    env_logger::Builder::from_default_env()
        .filter_module("chromiumoxide::handler", log::LevelFilter::Off)
        .filter_module("chromiumoxide::conn", log::LevelFilter::Off)
        .filter_module("tantivy::indexer::index_writer", log::LevelFilter::Warn)
        .filter_module("tantivy::indexer::prepared_commit", log::LevelFilter::Warn)
        .filter_module("tantivy::indexer::segment_updater", log::LevelFilter::Warn)
        .filter_module("tantivy::directory::managed_directory", log::LevelFilter::Warn)
        .filter_module("tantivy::directory::file_watcher", log::LevelFilter::Warn)
        .init();

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
    kodegen_mcp_schema::tool::tool_history::init_global_history(instance_id).await;

    // Build routers using provided async registration function
    let routers = register_tools(&config_manager, &usage_tracker).await?;

    // Create session manager for stateful HTTP with production-ready configuration
    let session_config = SessionConfig {
        channel_capacity: 16,
        keep_alive: cli.session_keep_alive(),  // Use CLI value or default (None)
    };

    // Log configured keep-alive for observability
    match session_config.keep_alive {
        None => log::info!("Session keep-alive: infinite (no timeout)"),
        Some(duration) => log::info!("Session keep-alive: {:?}", duration),
    }

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
        routers.connection_cleanup,
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
        
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        
        tokio::select! {
            _ = sigterm.recv() => {
                log::info!("Received SIGTERM, shutting down HTTP server");
            }
            _ = sigint.recv() => {
                log::info!("Received SIGINT, shutting down HTTP server");
            }
        }
    }
    
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        
        let mut ctrl_c = windows::ctrl_c()?;
        let mut ctrl_break = windows::ctrl_break()?;
        let mut ctrl_close = windows::ctrl_close()?;
        
        tokio::select! {
            _ = ctrl_c.recv() => {
                log::info!("Received CTRL+C, shutting down HTTP server");
            }
            _ = ctrl_break.recv() => {
                log::info!("Received CTRL+BREAK, shutting down HTTP server");
            }
            _ = ctrl_close.recv() => {
                log::info!("Received CTRL+CLOSE, shutting down HTTP server");
            }
        }
    }

    Ok(())
}
