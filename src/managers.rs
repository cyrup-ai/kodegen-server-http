use anyhow::Result;
use futures::future::join_all;
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

    /// Shutdown all registered managers gracefully in parallel
    ///
    /// Called automatically by core server before exit.
    /// Continues shutdown for all managers even if some fail (fail-slow approach).
    /// Returns error if any manager shutdown failed.
    pub async fn shutdown(&self) -> Result<()> {
        log::info!("Shutting down {} managers in parallel", self.shutdown_hooks.len());

        let results: Vec<_> = join_all(
            self.shutdown_hooks
                .iter()
                .enumerate()
                .map(|(i, hook)| async move {
                    hook.shutdown().await.map_err(|e| (i, e))
                })
        )
        .await;

        let errors: Vec<_> = results.into_iter().filter_map(|r| r.err()).collect();

        if !errors.is_empty() {
            for (i, e) in &errors {
                log::error!("Failed to shutdown manager {}: {}", i, e);
            }
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
