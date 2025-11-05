use anyhow::Result;
use kodegen_utils::usage_tracker::UsageTracker;
use thiserror::Error;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::router::{prompt::PromptRouter, tool::ToolRouter},
    model::*,
    service::RequestContext,
    transport::{
        common::server_side_http::SessionId,
        streamable_http_server::{
            SessionManager,
            StreamableHttpService, StreamableHttpServerConfig,
            session::local::LocalSessionManager,
        },
    },
};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use axum::Router;
use tower_http::cors::CorsLayer;

/// Wrapper for LocalSessionManager to enable graceful shutdown
/// 
/// Implements ShutdownHook to close all active HTTP sessions during server shutdown.
/// Each session runs a background tokio task that must be explicitly closed.
struct LocalSessionManagerHook {
    session_manager: Arc<LocalSessionManager>,
}

impl crate::managers::ShutdownHook for LocalSessionManagerHook {
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            log::info!("Shutting down LocalSessionManager");
            
            // Get all active session IDs (sessions field is public)
            let session_ids: Vec<SessionId> = {
                let sessions = self.session_manager.sessions.read().await;
                sessions.keys().cloned().collect()
            };
            
            log::debug!("Closing {} active HTTP sessions", session_ids.len());
            
            // Close each session gracefully (sends SessionEvent::Close to worker)
            for session_id in session_ids {
                match self.session_manager.close_session(&session_id).await {
                    Ok(_) => log::trace!("Closed session: {}", session_id),
                    Err(e) => log::warn!("Failed to close session {}: {}", session_id, e),
                }
            }
            
            log::info!("LocalSessionManager shutdown complete");
            Ok(())
        })
    }
}

/// MCP Server that serves tools via Streamable HTTP transport
///
/// Generic over `SessionManager` trait to enable pluggable session backends.
/// Defaults to `LocalSessionManager` for backward compatibility.
#[derive(Clone)]
pub struct HttpServer<SM = LocalSessionManager>
where
    SM: SessionManager + Clone,
{
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
    usage_tracker: UsageTracker,
    config_manager: kodegen_tools_config::ConfigManager,
    managers: std::sync::Arc<crate::managers::Managers>,
    active_requests: Arc<AtomicUsize>,
    session_manager: Arc<SM>,
}

impl<SM> HttpServer<SM>
where
    SM: SessionManager + Clone,
{
    /// Create a new HTTP server with pre-built routers and managers
    pub fn new(
        tool_router: ToolRouter<Self>,
        prompt_router: PromptRouter<Self>,
        usage_tracker: UsageTracker,
        config_manager: kodegen_tools_config::ConfigManager,
        managers: crate::managers::Managers,
        session_manager: Arc<SM>,
    ) -> Self {
        Self {
            tool_router,
            prompt_router,
            usage_tracker,
            config_manager,
            managers: std::sync::Arc::new(managers),
            active_requests: Arc::new(AtomicUsize::new(0)),
            session_manager,
        }
    }

    /// Create and serve HTTP server with optional TLS configuration
    ///
    /// Returns ServerHandle for graceful shutdown coordination.
    /// Spawns background tasks for HTTP/HTTPS server and shutdown monitoring.
    pub async fn serve_with_tls(
        self,
        addr: SocketAddr,
        tls_config: Option<(PathBuf, PathBuf)>,
        shutdown_timeout: Duration,
    ) -> Result<ServerHandle>
    where
        SM: std::any::Any + 'static,
    {
        use tokio::sync::oneshot;
        use tokio_util::sync::CancellationToken;

        let managers = self.managers.clone();
        let protocol = if tls_config.is_some() { "https" } else { "http" };

        log::info!("Starting HTTP server on {protocol}://{addr}");

        // Allocate timeout budget (70% HTTP drain, 30% cleanup)
        let http_drain_timeout = shutdown_timeout.mul_f32(0.7);
        let manager_buffer = shutdown_timeout.mul_f32(0.3);
        
        log::info!(
            "Shutdown timeout budget: total={:?}, HTTP drain={:?}, cleanup buffer={:?}",
            shutdown_timeout,
            http_drain_timeout,
            manager_buffer
        );

        // Create completion channel for graceful shutdown signaling
        let (completion_tx, completion_rx) = oneshot::channel();
        let ct = CancellationToken::new();

        // Register session manager for graceful shutdown (LocalSessionManager only)
        // Uses type downcast to check if session_manager is LocalSessionManager
        // Other SessionManager implementations would handle shutdown differently
        let session_manager = self.session_manager.clone();
        let session_manager_any: &dyn std::any::Any = &*session_manager;
        if session_manager_any.downcast_ref::<LocalSessionManager>().is_some() {
            // SAFETY: We just confirmed that SM is LocalSessionManager via downcast_ref.
            // Therefore Arc<SM> and Arc<LocalSessionManager> are the same type at runtime.
            let local_sm: Arc<LocalSessionManager> = unsafe {
                std::mem::transmute(session_manager.clone())
            };
            managers.register(LocalSessionManagerHook {
                session_manager: local_sm,
            }).await;
        }

        // Create service factory closure
        let service_factory = {
            let server = self.clone();
            move || Ok::<_, std::io::Error>(server.clone())
        };

        // Create StreamableHttpService
        let http_service = StreamableHttpService::new(
            service_factory,
            session_manager,
            StreamableHttpServerConfig {
                stateful_mode: true,
                sse_keep_alive: Some(Duration::from_secs(15)),
            },
        );

        // Build Axum router with CORS
        let router = Router::new()
            .nest_service("/mcp", http_service)
            .layer(CorsLayer::permissive());

        // Create axum-server handle for graceful shutdown
        let axum_handle = axum_server::Handle::new();
        let shutdown_handle = axum_handle.clone();

        // Spawn server with or without TLS
        let server_task = if let Some((cert_path, key_path)) = tls_config {
            log::info!("Loading TLS certificate from: {cert_path:?}");

            let rustls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load TLS configuration: {e}"))?;

            tokio::spawn(async move {
                if let Err(e) = axum_server::bind_rustls(addr, rustls_config)
                    .handle(axum_handle)
                    .serve(router.into_make_service())
                    .await
                {
                    log::error!("HTTP server error: {e}");
                }
            })
        } else {
            tokio::spawn(async move {
                if let Err(e) = axum_server::bind(addr)
                    .handle(axum_handle)
                    .serve(router.into_make_service())
                    .await
                {
                    log::error!("HTTP server error: {e}");
                }
            })
        };

        let ct_clone = ct.clone();
        let active_requests = self.active_requests.clone();

        // Spawn monitor task for graceful shutdown with immediate panic detection
        tokio::spawn(async move {
            // Pin server_task to allow polling in both select branches without moving
            tokio::pin!(server_task);
            
            // Race between cancellation signal and server task completion
            // This enables IMMEDIATE detection of panics during startup/operation
            let early_exit = tokio::select! {
                _ = ct_clone.cancelled() => {
                    log::debug!("Cancellation triggered, initiating graceful shutdown");
                    
                    // Trigger HTTP shutdown with 70% of timeout allocated for connection draining
                    shutdown_handle.graceful_shutdown(Some(http_drain_timeout));

                    // Wait for HTTP server to complete shutdown (with 5s safety buffer)
                    let server_shutdown_timeout = http_drain_timeout + Duration::from_secs(5);
                    match tokio::time::timeout(server_shutdown_timeout, &mut server_task).await {
                        Ok(Ok(_)) => {
                            log::debug!("HTTP server shutdown complete");
                        }
                        Ok(Err(e)) => {
                            log::error!("HTTP server task panicked during shutdown");
                            log::error!("  JoinError: {:?}", e);
                            if e.is_panic() {
                                if let Ok(panic_payload) = e.try_into_panic() {
                                    if let Some(msg) = panic_payload.downcast_ref::<&str>() {
                                        log::error!("  Panic message: {}", msg);
                                    } else if let Some(msg) = panic_payload.downcast_ref::<String>() {
                                        log::error!("  Panic message: {}", msg);
                                    } else {
                                        log::error!("  Panic payload: {:?}", panic_payload);
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            log::error!(
                                "HTTP server shutdown timeout ({:?}) - server task did not complete. Proceeding with manager shutdown.",
                                server_shutdown_timeout
                            );
                        }
                    }
                    
                    false  // Normal shutdown path
                }
                
                result = &mut server_task => {
                    // Server task completed BEFORE cancellation signal
                    // This is ALWAYS an error condition (panic or unexpected exit)
                    log::error!("╔═══════════════════════════════════════════════════════╗");
                    log::error!("║  HTTP SERVER TASK EXITED UNEXPECTEDLY                ║");
                    log::error!("║  Server terminated before shutdown signal received   ║");
                    log::error!("╚═══════════════════════════════════════════════════════╝");
                    
                    match result {
                        Ok(_) => {
                            log::error!("Server exited normally without cancellation signal");
                            log::error!("This indicates a bug in axum-server or misconfiguration");
                        }
                        Err(e) => {
                            log::error!("Server task PANICKED");
                            log::error!("  JoinError: {:?}", e);
                            
                            if e.is_panic() {
                                if let Ok(panic_payload) = e.try_into_panic() {
                                    if let Some(msg) = panic_payload.downcast_ref::<&str>() {
                                        log::error!("  Panic message: {}", msg);
                                    } else if let Some(msg) = panic_payload.downcast_ref::<String>() {
                                        log::error!("  Panic message: {}", msg);
                                    } else {
                                        log::error!("  Panic payload type: {:?}", panic_payload.type_id());
                                    }
                                }
                            } else if e.is_cancelled() {
                                log::error!("Server task was cancelled (unexpected)");
                            }
                        }
                    }
                    
                    log::error!("Proceeding with emergency cleanup (server already dead)");
                    true  // Early exit path - skip graceful shutdown
                }
            };

            // === Common cleanup path (executed for both normal and early exit) ===
            
            // Wait for all in-flight request handlers to complete
            // This is CRITICAL even after panic - prevents use-after-free in managers
            if early_exit {
                log::warn!("Server panicked - draining in-flight requests before manager cleanup");
            } else {
                log::info!("Draining in-flight request handlers before manager shutdown");
            }
            
            let drain_timeout = Duration::from_secs(30);
            let drain_start = std::time::Instant::now();
            
            loop {
                let active = active_requests.load(Ordering::SeqCst);
                
                if active == 0 {
                    log::info!("All request handlers completed successfully");
                    break;
                }
                
                if drain_start.elapsed() > drain_timeout {
                    log::warn!(
                        "Request drain timeout after {:?}, {} requests still active - proceeding with shutdown",
                        drain_timeout,
                        active
                    );
                    break;
                }
                
                log::debug!("Waiting for {} active request handlers to complete...", active);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            // Now shut down managers (safe - all request handlers finished or timeout expired)
            log::debug!("Starting manager shutdown");
            if let Err(e) = managers.shutdown().await {
                log::error!("Failed to shutdown managers: {e}");
            }
            log::debug!("Manager shutdown complete");

            // Signal shutdown complete (may fail if receiver timed out)
            if let Err(_) = completion_tx.send(()) {
                log::debug!(
                    "Shutdown completion signal not delivered (receiver dropped). \
                     This is expected if wait_for_completion() timed out or was cancelled."
                );
            }
        });

        Ok(ServerHandle::new(ct, completion_rx))
    }
}

impl<SM> ServerHandler for HttpServer<SM>
where
    SM: SessionManager,
{
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
            server_info: Implementation::from_build_env(),
            instructions: Some("KODEGEN HTTP Server".to_string()),
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool_name = request.name.clone();
        
        // Track this request handler (guard ensures decrement even on panic)
        let _guard = RequestGuard::new(self.active_requests.clone());
        
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);

        let result = self.tool_router.call(tcc).await;

        if result.is_ok() {
            self.usage_tracker.track_success(&tool_name);
        } else {
            self.usage_tracker.track_failure(&tool_name);
        }

        result
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let items = self.tool_router.list_all();
        Ok(ListToolsResult::with_all_items(items))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let pcc = rmcp::handler::server::prompt::PromptContext::new(
            self,
            request.name,
            request.arguments,
            context,
        );
        self.prompt_router.get_prompt(pcc).await
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let items = self.prompt_router.list_all();
        Ok(ListPromptsResult::with_all_items(items))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        Err(McpError::resource_not_found(
            "resource_not_found",
            Some(serde_json::json!({ "uri": request.uri })),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            next_cursor: None,
            resource_templates: Vec::new(),
        })
    }

    async fn initialize(
        &self,
        request: InitializeRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        // Store client info (fire-and-forget, errors logged in background task)
        let _ = self.config_manager.set_client_info(request.client_info).await;
        Ok(self.get_info())
    }
}

/// Errors that can occur during server shutdown
#[derive(Debug, Error)]
pub enum ShutdownError {
    /// Shutdown operations exceeded the timeout duration
    /// 
    /// This indicates that shutdown is taking longer than expected.
    /// Common causes:
    /// - Long-running request handlers still executing
    /// - Manager cleanup operations are slow or blocked
    /// - Database connections not closing promptly
    #[error("Shutdown timeout ({0:?}) - operations still in progress")]
    Timeout(Duration),

    /// Completion signal lost - monitor task may have panicked
    /// 
    /// This indicates the monitor task terminated unexpectedly before
    /// sending the completion signal. The actual shutdown may have
    /// completed successfully, but we cannot confirm.
    /// 
    /// Check logs for "HTTP SERVER TASK EXITED UNEXPECTEDLY" or panic messages.
    #[error("Shutdown completion signal lost - monitor task may have panicked")]
    SignalLost,
}

/// Handle for managing server lifecycle
///
/// Provides graceful shutdown with timeout support.
/// Zero-allocation, lock-free design using atomic CancellationToken.
pub struct ServerHandle {
    cancellation_token: tokio_util::sync::CancellationToken,
    completion_rx: tokio::sync::oneshot::Receiver<()>,
}

impl ServerHandle {
    pub fn new(
        cancellation_token: tokio_util::sync::CancellationToken,
        completion_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Self {
        Self {
            cancellation_token,
            completion_rx,
        }
    }

    /// Signal server to begin shutdown
    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    /// Wait for server shutdown to complete (with timeout)
    ///
    /// Returns Ok(()) if shutdown completes within timeout.
    /// Returns Err(ShutdownError::Timeout) if timeout expires.
    /// Returns Err(ShutdownError::SignalLost) if monitor task panicked.
    pub async fn wait_for_completion(mut self, timeout: Duration) -> Result<(), ShutdownError> {
        match tokio::time::timeout(timeout, &mut self.completion_rx).await {
            // Shutdown completed successfully
            Ok(Ok(())) => {
                log::debug!("Shutdown completed successfully");
                Ok(())
            }
            
            // Sender dropped - monitor task panicked or exited early
            Ok(Err(_recv_error)) => {
                log::error!("Shutdown completion signal lost (sender dropped)");
                Err(ShutdownError::SignalLost)
            }
            
            // Timeout expired - shutdown taking too long
            Err(_elapsed) => {
                log::error!("Shutdown timeout ({:?}) elapsed", timeout);
                Err(ShutdownError::Timeout(timeout))
            }
        }
    }
}

/// RAII guard for tracking active request handlers
///
/// Automatically increments the request counter on creation and decrements
/// on drop, ensuring proper cleanup even if the request handler panics.
///
/// This is the FIRST Drop implementation in kodegen-server-http/src,
/// establishing the RAII pattern for the codebase.
struct RequestGuard {
    counter: Arc<AtomicUsize>,
}

impl RequestGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self { counter }
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}
