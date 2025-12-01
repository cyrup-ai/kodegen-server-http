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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use axum::{extract::Path, response::Json, routing::{delete, get}, Router};
use serde::Serialize;
use tower::Service;
use tower_http::cors::CorsLayer;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio_rustls::{
    rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    TlsAcceptor,
};

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

/// Build rustls ServerConfig from PEM files
fn build_rustls_config(
    cert_path: PathBuf,
    key_path: PathBuf,
) -> Result<Arc<rustls::ServerConfig>> {
    let key = PrivateKeyDer::from_pem_file(key_path)
        .map_err(|e| anyhow::anyhow!("Failed to load private key: {e}"))?;
    
    let certs: Vec<CertificateDer> = CertificateDer::pem_file_iter(cert_path)
        .map_err(|e| anyhow::anyhow!("Failed to load certificates: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("Invalid certificate: {e}"))?;
    
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow::anyhow!("Failed to build TLS config: {e}"))?;
    
    // Enable HTTP/2 and HTTP/1.1
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    
    Ok(Arc::new(config))
}

/// Health check response returned by /mcp/health endpoint
#[derive(Serialize)]
struct HealthResponse {
    timestamp: String,
    status: HealthStatus,
    requests_processed: u64,
    memory_used: u64,
}

/// Health status enumeration
#[derive(Serialize)]
enum HealthStatus {
    #[serde(rename = "HEALTHY")]
    Healthy,
    #[serde(rename = "UNHEALTHY")]
    Unhealthy,
}

/// MCP Server that serves tools via Streamable HTTP transport
///
/// Generic over `SessionManager` trait to enable pluggable session backends.
/// Defaults to `LocalSessionManager` for backward compatibility.
pub struct HttpServer<SM = LocalSessionManager>
where
    SM: SessionManager,
{
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
    usage_tracker: UsageTracker,
    config_manager: kodegen_config_manager::ConfigManager,
    managers: std::sync::Arc<crate::managers::Managers>,
    active_requests: Arc<AtomicUsize>,
    requests_processed: Arc<AtomicU64>,
    session_manager: Arc<SM>,
    connection_cleanup: Option<crate::ConnectionCleanupFn>,
}

// Manual Clone implementation for HttpServer
// Arc<SM> is Clone regardless of whether SM is Clone
impl<SM> Clone for HttpServer<SM>
where
    SM: SessionManager,
{
    fn clone(&self) -> Self {
        Self {
            tool_router: self.tool_router.clone(),
            prompt_router: self.prompt_router.clone(),
            usage_tracker: self.usage_tracker.clone(),
            config_manager: self.config_manager.clone(),
            managers: self.managers.clone(),
            active_requests: self.active_requests.clone(),
            requests_processed: self.requests_processed.clone(),
            session_manager: self.session_manager.clone(),
            connection_cleanup: self.connection_cleanup.clone(),
        }
    }
}

impl<SM> HttpServer<SM>
where
    SM: SessionManager,
{
    /// Create a new HTTP server with pre-built routers and managers
    pub fn new(
        tool_router: ToolRouter<Self>,
        prompt_router: PromptRouter<Self>,
        usage_tracker: UsageTracker,
        config_manager: kodegen_config_manager::ConfigManager,
        managers: crate::managers::Managers,
        session_manager: Arc<SM>,
        connection_cleanup: Option<crate::ConnectionCleanupFn>,
    ) -> Self {
        Self {
            tool_router,
            prompt_router,
            usage_tracker,
            config_manager,
            managers: std::sync::Arc::new(managers),
            active_requests: Arc::new(AtomicUsize::new(0)),
            requests_processed: Arc::new(AtomicU64::new(0)),
            session_manager,
            connection_cleanup,
        }
    }

    /// Handle health check requests
    ///
    /// Returns JSON response with timestamp, status, requests processed count, and memory usage.
    async fn handle_health(&self) -> Json<HealthResponse> {
        use chrono::Utc;
        let memory_used = crate::memory::get_memory_used().unwrap_or(0);
        let status = if memory_used > 0 {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        };

        Json(HealthResponse {
            timestamp: Utc::now().to_rfc3339(),
            status,
            requests_processed: self.requests_processed.load(Ordering::SeqCst),
            memory_used,
        })
    }

    /// Handle connection cleanup notification
    ///
    /// Called when a connection drops to cleanup connection-specific resources.
    async fn handle_connection_delete(&self, connection_id: String) {
        use std::time::Instant;
        
        let start = Instant::now();
        
        log::info!("DELETE /mcp/connection/{}", connection_id);
        
        // Invoke cleanup handler if registered
        if let Some(cleanup) = &self.connection_cleanup {
            cleanup(connection_id.clone()).await;
        } else {
            log::debug!("No cleanup handler registered for this server");
        }
        
        let elapsed = start.elapsed();
        log::info!(
            "Connection {} cleanup completed in {:?}",
            connection_id,
            elapsed
        );
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

        // Pre-bind the socket with SO_REUSEADDR to allow immediate port reuse
        // This is CRITICAL for service manager integration - allows instant restarts
        log::debug!("Creating socket for {} with reuse options", addr);

        use tokio::net::TcpSocket;

        // Create socket (IPv4 or IPv6 based on address)
        let socket = if addr.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };

        // SO_REUSEADDR: Allows binding to port in TIME_WAIT state
        // Essential for fast restarts - without this, must wait 60+ seconds after shutdown
        socket.set_reuseaddr(true)
            .map_err(|e| anyhow::anyhow!("Failed to set SO_REUSEADDR: {}", e))?;

        // SO_REUSEPORT: (Unix only) Allows multiple processes to bind same port
        // Enables load balancing across multiple processes (advanced use case)
        #[cfg(unix)]
        socket.set_reuseport(true)
            .map_err(|e| anyhow::anyhow!("Failed to set SO_REUSEPORT: {}", e))?;

        log::debug!("Binding socket to {} with reuse flags enabled", addr);

        // Bind socket to address
        socket.bind(addr)
            .map_err(|e| anyhow::anyhow!("Failed to bind to {}: {}", addr, e))?;

        // Convert to listener with backlog of 1024 (standard for HTTP servers)
        let listener = socket.listen(1024)
            .map_err(|e| anyhow::anyhow!("Failed to listen on {}: {}", addr, e))?;

        log::info!("Successfully bound to {} with SO_REUSEADDR enabled", addr);

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

        // Spawn background memory monitor
        crate::monitor::spawn_memory_monitor(
            self.requests_processed.clone(),
            ct.clone(),
        );

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

        // Create health handler closure
        let health_handler = {
            let server = self.clone();
            move || {
                let server = server.clone();
                async move { server.handle_health().await }
            }
        };

        // Create connection delete handler closure
        let connection_delete_handler = {
            let server = self.clone();
            move |Path(connection_id): Path<String>| {
                let server = server.clone();
                async move {
                    server.handle_connection_delete(connection_id).await;
                    axum::http::StatusCode::NO_CONTENT
                }
            }
        };

        // Build Axum router with CORS
        let router = Router::new()
            .route("/mcp/health", get(health_handler))
            .route("/mcp/connection/{connection_id}", delete(connection_delete_handler))
            .nest_service("/mcp", http_service)
            .layer(CorsLayer::permissive());

        // Spawn server with or without TLS
        let server_task = if let Some((cert_path, key_path)) = tls_config {
            log::info!("Loading TLS certificate from: {cert_path:?}");
            
            let rustls_config = build_rustls_config(cert_path, key_path)?;
            let tls_acceptor = TlsAcceptor::from(rustls_config);
            let ct_for_tls = ct.clone();
            let active_requests = self.active_requests.clone();
            
            tokio::spawn(async move {
                loop {
                    // Accept TCP connection
                    let (tcp_stream, remote_addr) = tokio::select! {
                        _ = ct_for_tls.cancelled() => break,
                        result = listener.accept() => {
                            match result {
                                Ok(conn) => conn,
                                Err(e) => {
                                    log::error!("Failed to accept connection: {e}");
                                    continue;
                                }
                            }
                        }
                    };
                    
                    // Clone for task
                    let tls_acceptor = tls_acceptor.clone();
                    let router = router.clone();
                    let active_requests = active_requests.clone();
                    
                    // Spawn connection handler
                    tokio::spawn(async move {
                        // TLS handshake
                        let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                            Ok(stream) => stream,
                            Err(e) => {
                                log::error!("TLS handshake failed from {remote_addr}: {e}");
                                return;
                            }
                        };
                        
                        // Convert to hyper-compatible IO
                        let io = TokioIo::new(tls_stream);
                        
                        // Create hyper service from router
                        let tower_service = router.clone();
                        let hyper_service = hyper::service::service_fn(move |request| {
                            tower_service.clone().call(request)
                        });
                        
                        // Track active request
                        let _guard = RequestGuard::new(active_requests.clone());
                        
                        // Serve connection
                        if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                            .serve_connection_with_upgrades(io, hyper_service)
                            .await
                        {
                            log::debug!("Connection error from {remote_addr}: {e}");
                        }
                    });
                }
            })
        } else {
            // HTTP (no TLS) - use axum::serve directly
            let ct_for_http = ct.clone();
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        ct_for_http.cancelled().await;
                    })
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
                    
                    // Cancellation token already triggered shutdown via with_graceful_shutdown()
                    // Just wait for server task to complete
                    let server_shutdown_timeout = http_drain_timeout + Duration::from_secs(5);
                    match tokio::time::timeout(server_shutdown_timeout, &mut server_task).await {
                        Ok(Ok(_)) => {
                            log::debug!("HTTP server shutdown complete");
                        }
                        Ok(Err(e)) => {
                            log::error!("HTTP server task panicked during shutdown");
                            log::error!("  JoinError: {:?}", e);
                            if e.is_panic()
                                && let Ok(panic_payload) = e.try_into_panic() {
                                if let Some(msg) = panic_payload.downcast_ref::<&str>() {
                                    log::error!("  Panic message: {}", msg);
                                } else if let Some(msg) = panic_payload.downcast_ref::<String>() {
                                    log::error!("  Panic message: {}", msg);
                                } else {
                                    log::error!("  Panic payload: {:?}", panic_payload);
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
                            log::error!("This indicates a bug in the server implementation or misconfiguration");
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
            if completion_tx.send(()).is_err() {
                log::debug!(
                    "Shutdown completion signal not delivered (receiver dropped). \
                     This is expected if wait_for_completion() timed out or was cancelled."
                );
            }
        });

        Ok(ServerHandle::new(ct, completion_rx))
    }

    /// Create and serve HTTP server using a pre-bound listener (TOCTOU-safe)
    ///
    /// This variant accepts a TcpListener that's already bound to an address.
    /// Use this to eliminate TOCTOU races when port cleanup is required before startup.
    ///
    /// The listener is used directly for accept() calls, preventing any gap where
    /// another process could claim the port.
    ///
    /// # Arguments
    /// * `listener` - Pre-bound TcpListener (port already reserved)
    /// * `tls_config` - Optional (cert_path, key_path) for HTTPS
    /// * `shutdown_timeout` - Graceful shutdown timeout
    ///
    /// # Returns
    /// ServerHandle for graceful shutdown coordination
    ///
    /// # Example
    /// ```rust
    /// // Reserve port with cleanup
    /// let listener = cleanup_and_reserve_port(30438).await?;
    ///
    /// // Start server with pre-bound listener (no race window)
    /// let handle = server.serve_with_listener(listener, tls_config, timeout).await?;
    /// ```
    pub async fn serve_with_listener(
        self,
        listener: tokio::net::TcpListener,
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
        
        // Get the address the listener is bound to
        let addr = listener.local_addr()
            .map_err(|e| anyhow::anyhow!("Failed to get listener address: {}", e))?;

        log::info!("Starting HTTP server on {protocol}://{addr} (using pre-bound listener)");

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
        let session_manager = self.session_manager.clone();
        let session_manager_any: &dyn std::any::Any = &*session_manager;
        if session_manager_any.downcast_ref::<LocalSessionManager>().is_some() {
            let local_sm: Arc<LocalSessionManager> = unsafe {
                std::mem::transmute(session_manager.clone())
            };
            managers.register(LocalSessionManagerHook {
                session_manager: local_sm,
            }).await;
        }

        // Spawn background memory monitor
        crate::monitor::spawn_memory_monitor(
            self.requests_processed.clone(),
            ct.clone(),
        );

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

        // Create health handler closure
        let health_handler = {
            let server = self.clone();
            move || {
                let server = server.clone();
                async move { server.handle_health().await }
            }
        };

        // Create connection delete handler closure
        let connection_delete_handler = {
            let server = self.clone();
            move |Path(connection_id): Path<String>| {
                let server = server.clone();
                async move {
                    server.handle_connection_delete(connection_id).await;
                    axum::http::StatusCode::NO_CONTENT
                }
            }
        };

        // Build Axum router with CORS
        let router = Router::new()
            .route("/mcp/health", get(health_handler))
            .route("/mcp/connection/{connection_id}", delete(connection_delete_handler))
            .nest_service("/mcp", http_service)
            .layer(CorsLayer::permissive());

        // Spawn server with or without TLS
        let server_task = if let Some((cert_path, key_path)) = tls_config {
            log::info!("Loading TLS certificate from: {cert_path:?}");
            
            let rustls_config = build_rustls_config(cert_path, key_path)?;
            let tls_acceptor = TlsAcceptor::from(rustls_config);
            let ct_for_tls = ct.clone();
            let active_requests = self.active_requests.clone();
            
            tokio::spawn(async move {
                loop {
                    // Accept TCP connection from pre-bound listener
                    let (tcp_stream, remote_addr) = tokio::select! {
                        _ = ct_for_tls.cancelled() => break,
                        result = listener.accept() => {
                            match result {
                                Ok(conn) => conn,
                                Err(e) => {
                                    log::error!("Failed to accept connection: {e}");
                                    continue;
                                }
                            }
                        }
                    };
                    
                    // Clone for task
                    let tls_acceptor = tls_acceptor.clone();
                    let router = router.clone();
                    let active_requests = active_requests.clone();
                    
                    // Spawn connection handler (same as serve_with_tls)
                    tokio::spawn(async move {
                        let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                            Ok(stream) => stream,
                            Err(e) => {
                                log::error!("TLS handshake failed from {remote_addr}: {e}");
                                return;
                            }
                        };
                        
                        let io = TokioIo::new(tls_stream);
                        let tower_service = router.clone();
                        let hyper_service = hyper::service::service_fn(move |request| {
                            tower_service.clone().call(request)
                        });
                        
                        let _guard = RequestGuard::new(active_requests.clone());
                        
                        if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                            .serve_connection_with_upgrades(io, hyper_service)
                            .await
                        {
                            log::debug!("Connection error from {remote_addr}: {e}");
                        }
                    });
                }
            })
        } else {
            // HTTP (no TLS) - use axum::serve with pre-bound listener
            let ct_for_http = ct.clone();
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        ct_for_http.cancelled().await;
                    })
                    .await
                {
                    log::error!("HTTP server error: {e}");
                }
            })
        };

        // Spawn monitor task for graceful shutdown (identical pattern to serve_with_tls)
        let ct_clone = ct.clone();
        let active_requests = self.active_requests.clone();

        tokio::spawn(async move {
            tokio::pin!(server_task);
            
            let early_exit = tokio::select! {
                _ = ct_clone.cancelled() => {
                    log::debug!("Cancellation triggered, initiating graceful shutdown");
                    
                    let server_shutdown_timeout = http_drain_timeout + Duration::from_secs(5);
                    match tokio::time::timeout(server_shutdown_timeout, &mut server_task).await {
                        Ok(Ok(_)) => {
                            log::debug!("HTTP server shutdown complete");
                        }
                        Ok(Err(e)) => {
                            log::error!("HTTP server task panicked during shutdown: {:?}", e);
                        }
                        Err(_) => {
                            log::error!("HTTP server shutdown timeout ({:?})", server_shutdown_timeout);
                        }
                    }
                    
                    false
                }
                
                result = &mut server_task => {
                    log::error!("HTTP server task exited unexpectedly");
                    match result {
                        Ok(_) => log::error!("Server exited normally without cancellation"),
                        Err(e) => log::error!("Server task panicked: {:?}", e),
                    }
                    true
                }
            };

            // Wait for all in-flight request handlers to complete
            if early_exit {
                log::warn!("Server panicked - draining in-flight requests before cleanup");
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
                        "Request drain timeout after {:?}, {} requests still active",
                        drain_timeout,
                        active
                    );
                    break;
                }
                
                log::debug!("Waiting for {} active request handlers...", active);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            // Shut down managers
            log::debug!("Starting manager shutdown");
            if let Err(e) = managers.shutdown().await {
                log::error!("Manager shutdown error: {e}");
            }
            log::debug!("Manager shutdown complete");

            // Signal completion
            let _ = completion_tx.send(());
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

        // Increment total tool calls counter
        self.requests_processed.fetch_add(1, Ordering::SeqCst);

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
