use anyhow::Result;
use clap::Parser;
use kodegen_config_manager::ConfigManager;
use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
use rmcp::transport::streamable_http_server::session::local::{LocalSessionManager, SessionConfig};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub mod cli;
pub mod managers;
pub mod memory;
pub mod monitor;
pub mod registration;
pub mod server;
pub mod tool_history;
pub mod usage_tracker;

pub use cli::Cli;
pub use managers::{Managers, ShutdownHook};
pub use registration::{register_tool, register_tool_arc};
pub use server::{HttpServer, ServerHandle, ShutdownError};
pub use tool_history::ToolHistory;
pub use usage_tracker::{UsageTracker, UsageStats};

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

/// Type alias for tool registration closure
type ToolRegistrationFn = Box<
    dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<RouterSet<HttpServer>>> + Send>>
    + Send
>;

/// Builder for configuring and running an HTTP MCP server
///
/// This is the recommended API for category servers. It collects all configuration
/// via builder methods and executes via `.run()`.
///
/// # Example
/// ```no_run
/// use kodegen_server_http::{ServerBuilder, RouterSet, Managers};
/// use kodegen_config::CATEGORY_FILESYSTEM;
/// use rmcp::handler::server::router::{tool::ToolRouter, prompt::PromptRouter};
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     ServerBuilder::new()
///         .category(CATEGORY_FILESYSTEM)
///         .register_tools(|| async {
///             let tool_router = ToolRouter::new();
///             let prompt_router = PromptRouter::new();
///             let managers = Managers::new();
///             // Register tools here...
///             Ok(RouterSet::new(tool_router, prompt_router, managers))
///         })
///         .run()
///         .await
/// }
/// ```
pub struct ServerBuilder {
    category: Option<String>,
    register_tools_fn: Option<ToolRegistrationFn>,
    listener: Option<tokio::net::TcpListener>,
    tls_config: Option<(std::path::PathBuf, std::path::PathBuf)>,
}

impl ServerBuilder {
    /// Create a new ServerBuilder
    pub fn new() -> Self {
        Self {
            category: None,
            register_tools_fn: None,
            listener: None,
            tls_config: None,
        }
    }

    /// Set the category name for this server (required)
    ///
    /// Use canonical constants from `kodegen_config::CATEGORY_*`
    pub fn category(mut self, category: &str) -> Self {
        self.category = Some(category.to_string());
        self
    }

    /// Set the tool registration function (required)
    ///
    /// The closure takes no parameters and returns a RouterSet.
    pub fn register_tools<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<RouterSet<HttpServer>>> + Send + 'static,
    {
        self.register_tools_fn = Some(Box::new(move || Box::pin(f())));
        self
    }

    /// Set a pre-bound listener (optional, for kodegend TOCTOU-safe port binding)
    ///
    /// When a listener is provided, the server will use it instead of parsing
    /// CLI arguments and binding to a new port. This is used by kodegend to
    /// eliminate race conditions during port cleanup.
    pub fn with_listener(mut self, listener: tokio::net::TcpListener) -> Self {
        self.listener = Some(listener);
        self
    }

    /// Set TLS configuration (optional, for HTTPS)
    ///
    /// Provides paths to TLS certificate and private key files.
    /// Used by embedded servers (kodegend) to enable HTTPS.
    pub fn with_tls_config(mut self, cert_path: std::path::PathBuf, key_path: std::path::PathBuf) -> Self {
        self.tls_config = Some((cert_path, key_path));
        self
    }

    /// Run the HTTP server (blocking until shutdown signal)
    ///
    /// This method:
    /// - Initializes logging
    /// - Parses CLI arguments
    /// - Creates ConfigManager, UsageTracker, ToolHistory
    /// - Calls the tool registration function
    /// - Starts the HTTP/HTTPS server
    /// - Waits for shutdown signal (SIGTERM, SIGINT, Ctrl+C)
    /// - Performs graceful shutdown
    pub async fn run(self) -> Result<()> {
        let category = self.category
            .ok_or_else(|| anyhow::anyhow!("category is required - call .category() before .run()"))?;
        let register_tools_fn = self.register_tools_fn
            .ok_or_else(|| anyhow::anyhow!("register_tools is required - call .register_tools() before .run()"))?;

        // Initialize logging with chromiumoxide CDP error filtering and tantivy spam reduction
        env_logger::Builder::from_default_env()
            .filter_module("chromiumoxide::handler", log::LevelFilter::Off)
            .filter_module("chromiumoxide::conn", log::LevelFilter::Off)
            .filter_module("tantivy::indexer::index_writer", log::LevelFilter::Warn)
            .filter_module("tantivy::indexer::prepared_commit", log::LevelFilter::Warn)
            .filter_module("tantivy::indexer::segment_updater", log::LevelFilter::Warn)
            .filter_module("tantivy::directory::managed_directory", log::LevelFilter::Warn)
            .filter_module("tantivy::directory::file_watcher", log::LevelFilter::Warn)
            .init();

        // Install rustls CryptoProvider (idempotent)
        if rustls::crypto::ring::default_provider().install_default().is_err() {
            log::debug!("rustls crypto provider already installed");
        }

        // Parse CLI arguments
        let cli = Cli::parse();

        // Initialize ConfigManager
        let config_manager = ConfigManager::new();
        config_manager.init().await?;

        // Create instance ID
        let timestamp = chrono::Utc::now();
        let pid = std::process::id();
        let instance_id = format!("{}-{}", timestamp.format("%Y%m%d-%H%M%S-%9f"), pid);

        // Create UsageTracker and ToolHistory
        let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));
        log::debug!("Initializing tool history tracking for instance: {}", instance_id);
        let tool_history = Arc::new(ToolHistory::new(format!("{}-{}", category, instance_id)).await);

        // Call tool registration function
        let routers = register_tools_fn().await?;

        // Create session manager
        let session_config = SessionConfig {
            channel_capacity: 16,
            keep_alive: cli.session_keep_alive(),
        };

        match session_config.keep_alive {
            None => log::info!("Session keep-alive: infinite (no timeout)"),
            Some(duration) => log::info!("Session keep-alive: {:?}", duration),
        }

        let session_manager = Arc::new(LocalSessionManager {
            sessions: Default::default(),
            session_config,
        });

        // Get listener and address (either from pre-bound listener or CLI)
        let (addr, listener) = if let Some(listener) = self.listener {
            let addr = listener.local_addr()
                .map_err(|e| anyhow::anyhow!("Failed to get listener address: {}", e))?;
            (addr, listener)
        } else {
            let addr = cli.http_address()?;
            let listener = tokio::net::TcpListener::bind(addr).await
                .map_err(|e| anyhow::anyhow!("Failed to bind to {}: {}", addr, e))?;
            (addr, listener)
        };

        // Create server identity
        let server_identity = server::ServerIdentity {
            category: category.clone(),
            instance_id: instance_id.clone(),
            port: addr.port(),
        };

        // Build HttpServer
        let mut builder = HttpServer::builder()
            .server_identity(server_identity)
            .tool_router(routers.tool_router)
            .prompt_router(routers.prompt_router)
            .usage_tracker(usage_tracker)
            .tool_history(tool_history)
            .config_manager(config_manager)
            .managers(routers.managers)
            .session_manager(session_manager);

        if let Some(cleanup) = routers.connection_cleanup {
            builder = builder.connection_cleanup(cleanup);
        }

        let server = builder.build()
            .expect("Failed to build HttpServer - all required fields provided");

        // Start server with pre-bound listener
        let protocol = if cli.tls_config().is_some() { "https" } else { "http" };
        log::info!("Starting {} HTTP server on {}://{}", category, protocol, addr);

        let timeout = cli.shutdown_timeout();
        let handle = server.serve_with_listener(listener, cli.tls_config(), timeout).await?;

        log::info!("{} server running on {}://{}", category, protocol, addr);
        if cli.tls_config().is_some() {
            log::info!("TLS/HTTPS enabled - using encrypted connections");
        }
        log::info!("Press Ctrl+C or send SIGTERM to initiate graceful shutdown");

        // Wait for shutdown signal
        wait_for_shutdown_signal().await?;

        // Graceful shutdown
        log::info!("Shutdown signal received, initiating graceful shutdown (timeout: {:?})", timeout);
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
                    category, elapsed
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
                Err(error)
            }
        }
    }

    /// Serve the HTTP server and return ServerHandle (for embedded servers)
    ///
    /// This method is designed for embedded servers (kodegend) that need programmatic
    /// shutdown control. Unlike `.run()`, it returns immediately with a ServerHandle
    /// instead of waiting for shutdown signals.
    ///
    /// # Returns
    /// ServerHandle for graceful shutdown control
    pub async fn serve(self) -> Result<ServerHandle> {
        let category = self.category
            .ok_or_else(|| anyhow::anyhow!("category is required - call .category() before .serve()"))?;
        let register_tools_fn = self.register_tools_fn
            .ok_or_else(|| anyhow::anyhow!("register_tools is required - call .register_tools() before .serve()"))?;

        // Initialize logging (may be called multiple times by different servers - idempotent)
        let _ = env_logger::Builder::from_default_env()
            .filter_module("chromiumoxide::handler", log::LevelFilter::Off)
            .filter_module("chromiumoxide::conn", log::LevelFilter::Off)
            .filter_module("tantivy::indexer::index_writer", log::LevelFilter::Warn)
            .filter_module("tantivy::indexer::prepared_commit", log::LevelFilter::Warn)
            .filter_module("tantivy::indexer::segment_updater", log::LevelFilter::Warn)
            .filter_module("tantivy::directory::managed_directory", log::LevelFilter::Warn)
            .filter_module("tantivy::directory::file_watcher", log::LevelFilter::Warn)
            .try_init();

        // Install rustls CryptoProvider (idempotent)
        if rustls::crypto::ring::default_provider().install_default().is_err() {
            log::debug!("rustls crypto provider already installed");
        }

        // Initialize ConfigManager
        let config_manager = ConfigManager::new();
        config_manager.init().await?;

        // Create instance ID
        let timestamp = chrono::Utc::now();
        let pid = std::process::id();
        let instance_id = format!("{}-{}", timestamp.format("%Y%m%d-%H%M%S-%9f"), pid);

        // Create UsageTracker and ToolHistory
        let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));
        log::debug!("Initializing tool history tracking for instance: {}", instance_id);
        let tool_history = Arc::new(ToolHistory::new(format!("{}-{}", category, instance_id)).await);

        // Call tool registration function
        let routers = register_tools_fn().await?;

        // Create session manager
        let session_config = SessionConfig {
            channel_capacity: 16,
            keep_alive: Some(std::time::Duration::from_secs(3600)),
        };

        let session_manager = Arc::new(LocalSessionManager {
            sessions: Default::default(),
            session_config,
        });

        // Get listener and address (must have pre-bound listener for embedded servers)
        let listener = self.listener
            .ok_or_else(|| anyhow::anyhow!("listener is required for .serve() - call .with_listener() before .serve()"))?;

        let addr = listener.local_addr()
            .map_err(|e| anyhow::anyhow!("Failed to get listener address: {}", e))?;

        // Create server identity
        let server_identity = server::ServerIdentity {
            category: category.clone(),
            instance_id: instance_id.clone(),
            port: addr.port(),
        };

        // Build HttpServer
        let mut builder = HttpServer::builder()
            .server_identity(server_identity)
            .tool_router(routers.tool_router)
            .prompt_router(routers.prompt_router)
            .usage_tracker(usage_tracker)
            .tool_history(tool_history)
            .config_manager(config_manager)
            .managers(routers.managers)
            .session_manager(session_manager);

        if let Some(cleanup) = routers.connection_cleanup {
            builder = builder.connection_cleanup(cleanup);
        }

        let server = builder.build()
            .expect("Failed to build HttpServer - all required fields provided");

        // Start server with pre-bound listener
        let tls_config = self.tls_config;
        let has_tls = tls_config.is_some();
        let protocol = if has_tls { "https" } else { "http" };
        log::info!("Starting {} HTTP server on {}://{}", category, protocol, addr);

        let shutdown_timeout = std::time::Duration::from_secs(30);
        let handle = server.serve_with_listener(listener, tls_config, shutdown_timeout).await?;

        log::info!("{} server running on {}://{}", category, protocol, addr);
        if has_tls {
            log::info!("TLS/HTTPS enabled - using encrypted connections");
        }

        Ok(handle)
    }
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self::new()
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
