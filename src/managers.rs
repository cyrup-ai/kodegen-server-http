use anyhow::Result;
use std::future::Future;
use std::pin::Pin;

/// Container for managers that require explicit shutdown
///
/// Category servers populate this based on what managers their tools use.
/// The core server handles calling shutdown() on graceful termination.
#[derive(Default)]
pub struct Managers {
    shutdown_hooks: Vec<Box<dyn ShutdownHook>>,
}

/// Trait for components that need graceful shutdown
///
/// Example implementations:
/// - BrowserManager::shutdown() - closes Chrome processes
/// - TunnelGuard::shutdown() - closes SSH tunnels
/// - SearchManager::shutdown() - cancels background search tasks
pub trait ShutdownHook: Send + Sync {
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

impl Managers {
    pub fn new() -> Self {
        Self {
            shutdown_hooks: Vec::new(),
        }
    }

    /// Register a component that needs shutdown
    ///
    /// Example usage in category server:
    /// ```
    /// let browser_manager = Arc::new(BrowserManager::new());
    /// managers.register(browser_manager.clone());
    /// ```
    pub fn register<H: ShutdownHook + 'static>(&mut self, hook: H) {
        self.shutdown_hooks.push(Box::new(hook));
    }

    /// Shutdown all registered managers gracefully in reverse registration order (LIFO)
    ///
    /// Managers are shut down **sequentially** in reverse order of registration.
    /// This matches Rust's Drop trait convention and ensures that managers registered
    /// later (which may depend on earlier managers) shut down first.
    ///
    /// Example:
    /// ```
    /// managers.register(database_pool);  // Shuts down last
    /// managers.register(cache_manager);  // Shuts down first
    /// ```
    ///
    /// Called automatically by core server before exit.
    /// Continues shutdown for all managers even if some fail (fail-slow approach).
    /// Returns error if any manager shutdown failed.
    pub async fn shutdown(&self) -> Result<()> {
        log::info!(
            "Shutting down {} managers sequentially (LIFO order)",
            self.shutdown_hooks.len()
        );

        let mut errors = Vec::new();

        // Shut down in reverse order of registration (LIFO)
        for (i, hook) in self.shutdown_hooks.iter().enumerate().rev() {
            log::debug!("Shutting down manager {}", i);
            if let Err(e) = hook.shutdown().await {
                log::error!("Failed to shutdown manager {}: {}", i, e);
                errors.push((i, e));
            }
        }

        if !errors.is_empty() {
            return Err(anyhow::anyhow!(
                "{} out of {} managers failed to shutdown",
                errors.len(),
                self.shutdown_hooks.len()
            ));
        }

        log::info!("All managers shut down successfully");
        Ok(())
    }
}
