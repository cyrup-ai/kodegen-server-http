use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Maximum time to wait for a single manager to shut down.
/// 
/// Chosen to be generous for most operations:
/// - Browser close: ~2-3 seconds
/// - DB pool drain: ~3-5 seconds  
/// - SSH tunnel disconnect: ~1-2 seconds
/// - File cleanup: <1 second
const PER_MANAGER_TIMEOUT: Duration = Duration::from_secs(10);

/// Container for managers that require explicit shutdown
///
/// Category servers populate this based on what managers their tools use.
/// The core server handles calling shutdown() on graceful termination.
#[derive(Default)]
pub struct Managers {
    shutdown_hooks: Mutex<Vec<Arc<dyn ShutdownHook>>>,
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
            shutdown_hooks: Mutex::new(Vec::new()),
        }
    }

    /// Register a component that needs shutdown
    ///
    /// Example usage in category server:
    /// ```no_run
    /// # use kodegen_server_http::{Managers, ShutdownHook};
    /// # use std::pin::Pin;
    /// # use std::future::Future;
    /// # use anyhow::Result;
    /// #
    /// # struct BrowserManager;
    /// # impl BrowserManager {
    /// #     fn global() -> Self { Self }
    /// # }
    /// # impl ShutdownHook for BrowserManager {
    /// #     fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
    /// #         Box::pin(async { Ok(()) })
    /// #     }
    /// # }
    /// #
    /// # #[tokio::main]
    /// # async fn main() -> anyhow::Result<()> {
    /// # let managers = Managers::new();
    /// let browser_manager = BrowserManager::global();
    /// managers.register(browser_manager).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn register<H: ShutdownHook + 'static>(&self, hook: H) {
        self.shutdown_hooks.lock().await.push(Arc::new(hook));
    }

    /// Shutdown all registered managers gracefully in reverse registration order (LIFO)
    ///
    /// Managers are shut down **sequentially** in reverse order of registration.
    /// This matches Rust's Drop trait convention and ensures that managers registered
    /// later (which may depend on earlier managers) shut down first.
    ///
    /// Example:
    /// ```no_run
    /// # use kodegen_server_http::{Managers, ShutdownHook};
    /// # use std::pin::Pin;
    /// # use std::future::Future;
    /// # use anyhow::Result;
    /// #
    /// # struct DatabasePool;
    /// # impl ShutdownHook for DatabasePool {
    /// #     fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
    /// #         Box::pin(async { Ok(()) })
    /// #     }
    /// # }
    /// #
    /// # struct CacheManager;
    /// # impl ShutdownHook for CacheManager {
    /// #     fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
    /// #         Box::pin(async { Ok(()) })
    /// #     }
    /// # }
    /// #
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let managers = Managers::new();
    /// # let database_pool = DatabasePool;
    /// # let cache_manager = CacheManager;
    /// // Managers shut down in LIFO order (reverse registration)
    /// managers.register(database_pool).await;  // Shuts down last
    /// managers.register(cache_manager).await;  // Shuts down first
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Called automatically by core server before exit.
    /// Continues shutdown for all managers even if some fail (fail-slow approach).
    /// Returns error if any manager shutdown failed.
    pub async fn shutdown(&self) -> Result<()> {
        let count = {
            let hooks = self.shutdown_hooks.lock().await;
            hooks.len()
        };
        
        log::info!(
            "Shutting down {} managers sequentially (LIFO order, {}s timeout each)",
            count,
            PER_MANAGER_TIMEOUT.as_secs()
        );

        let mut errors = Vec::new();

        // Shut down in reverse order of registration (LIFO)
        // We need to lock for each iteration to avoid holding the lock across await
        for i in (0..count).rev() {
            log::debug!("Shutting down manager {} (timeout: {:?})", i, PER_MANAGER_TIMEOUT);

            // Lock, clone the Arc, drop the lock, THEN await
            let hook = {
                let hooks = self.shutdown_hooks.lock().await;
                match hooks.get(i) {
                    Some(hook) => hook.clone(),  // Clone the Arc (cheap)
                    None => continue,
                }
            };  // Lock automatically dropped here

            // Now await without holding the lock
            let result = tokio::time::timeout(PER_MANAGER_TIMEOUT, hook.shutdown()).await;

            match result {
                Ok(Ok(_)) => {
                    log::debug!("Manager {} shutdown complete", i);
                }
                Ok(Err(e)) => {
                    log::error!("Manager {} shutdown failed: {}", i, e);
                    errors.push((i, e));
                    // Continue to next manager instead of stopping
                }
                Err(_) => {
                    let timeout_err = anyhow::anyhow!(
                        "Manager {} shutdown timeout after {:?}",
                        i,
                        PER_MANAGER_TIMEOUT
                    );
                    log::error!("{}", timeout_err);
                    errors.push((i, timeout_err));
                    // Continue to next manager instead of hanging forever
                }
            }
        }

        if !errors.is_empty() {
            return Err(anyhow::anyhow!(
                "{} out of {} managers failed to shutdown",
                errors.len(),
                count
            ));
        }

        log::info!("All managers shut down successfully");
        Ok(())
    }
}
