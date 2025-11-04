use anyhow::Result;
use clap::Parser;
use kodegen_tools_config::ConfigManager;
use kodegen_utils::usage_tracker::UsageTracker;
use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};

pub mod cli;
pub mod managers;
pub mod registration;
pub mod server;

pub use cli::Cli;
pub use managers::{Managers, ShutdownHook};
pub use registration::{register_tool, register_tool_arc};
pub use server::{ServerHandle, HttpServer};

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
    F: FnOnce(&ConfigManager, &UsageTracker) -> Result<RouterSet<HttpServer>>,
{
    // Initialize logging
    env_logger::init();

    // Install rustls CryptoProvider (required for HTTPS)
    // This is idempotent: if a provider is already installed (e.g., by a parent
    // application), we log and continue rather than failing.
    if let Err(_existing_provider) = rustls::crypto::ring::default_provider().install_default() {
        log::debug!(
            "rustls crypto provider already installed (likely by parent application or test harness)"
        );
    }

    // Parse CLI arguments
    let cli = Cli::parse();

    // Initialize shared components
    let config_manager = ConfigManager::new();
    config_manager.init().await?;

    let instance_id = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let usage_tracker = UsageTracker::new(format!("{}-{}", category, instance_id));

    // Initialize global tool history tracking
    log::debug!("Initializing global tool history tracking for instance: {}", instance_id);
    kodegen_mcp_tool::tool_history::init_global_history(instance_id).await;

    // Build routers using provided registration function
    let routers = register_tools(&config_manager, &usage_tracker)?;

    // Create HTTP server
    let server = HttpServer::new(
        routers.tool_router,
        routers.prompt_router,
        usage_tracker,
        config_manager,
        routers.managers,
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

    let handle = server.serve_with_tls(addr, cli.tls_config()).await?;

    log::info!("{} server running on {}://{}", category, protocol, addr);
    if cli.tls_config().is_some() {
        log::info!("TLS/HTTPS enabled - using encrypted connections");
    }
    log::info!("Press Ctrl+C or send SIGTERM to initiate graceful shutdown");

    // Wait for shutdown signal
    wait_for_shutdown_signal().await?;

    // Graceful shutdown
    let timeout = cli.shutdown_timeout();
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
        Err(elapsed) => {
            let error = anyhow::anyhow!(
                "{} server shutdown timeout ({:?}) elapsed before completion",
                category,
                elapsed
            );
            log::error!("{}", error);
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
