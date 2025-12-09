use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Update event for background processor
enum StatsUpdate {
    Success {
        connection_id: String,
        tool_name: String,
    },
    Failure {
        connection_id: String,
        tool_name: String,
    },
    RemoveConnection(String), // connection_id
    SaveToDisk, // Periodic flush to disk
    Shutdown, // Final flush and shutdown
}

// Session timeout: 30 minutes of inactivity = new session
const SESSION_TIMEOUT_SECS: i64 = 30 * 60;

// Periodic save interval: flush stats to disk every 5 minutes
const SAVE_INTERVAL_SECS: u64 = 5 * 60;

/// Statistics tracked for tool usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStats {
    // Tool category counters
    pub filesystem_operations: u64,
    pub terminal_operations: u64,
    pub edit_operations: u64,
    pub search_operations: u64,
    pub config_operations: u64,
    pub process_operations: u64,

    // Overall counters
    pub total_tool_calls: u64,
    pub successful_calls: u64,
    pub failed_calls: u64,

    // Tool-specific counters
    pub tool_counts: HashMap<String, u64>,

    // Timing information
    pub first_used: i64, // Unix timestamp
    pub last_used: i64,  // Unix timestamp
    pub total_sessions: u64,
}

impl Default for UsageStats {
    fn default() -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            filesystem_operations: 0,
            terminal_operations: 0,
            edit_operations: 0,
            search_operations: 0,
            config_operations: 0,
            process_operations: 0,
            total_tool_calls: 0,
            successful_calls: 0,
            failed_calls: 0,
            tool_counts: HashMap::new(),
            first_used: now,
            last_used: now,
            total_sessions: 1,
        }
    }
}

/// Usage tracker that manages per-connection statistics for all tool calls
#[derive(Clone)]
pub struct UsageTracker {
    /// Per-connection stats storage (connection_id -> UsageStats)
    stats_by_connection: Arc<DashMap<String, UsageStats>>,
    stats_file: PathBuf,
    session_start: std::time::Instant,
    /// Fire-and-forget channel for stat updates
    update_sender: tokio::sync::mpsc::UnboundedSender<StatsUpdate>,
}

impl UsageTracker {
    /// Create new `UsageTracker` with instance-specific stats file in ~/.kodegen/stats_{`instance_id}.json`
    #[must_use]
    pub fn new(instance_id: String) -> Self {
        let stats_file = Self::get_stats_file_path(&instance_id);

        // Load existing stats from disk (if available)
        let stats_by_connection = Self::load_from_disk(&stats_file);

        // Create unbounded channel for fire-and-forget updates
        let (update_sender, update_receiver) = tokio::sync::mpsc::unbounded_channel();

        let tracker = Self {
            stats_by_connection: Arc::new(stats_by_connection),
            stats_file: stats_file.clone(),
            session_start: std::time::Instant::now(),
            update_sender: update_sender.clone(),
        };

        // Start background processor
        tracker.start_background_processor(update_receiver, stats_file);

        // Start periodic save timer
        tracker.start_periodic_save_timer();

        tracker
    }

    /// Get server uptime since tracker creation
    #[must_use]
    pub fn uptime(&self) -> std::time::Duration {
        self.session_start.elapsed()
    }

    /// Get the path to the stats file on disk
    #[must_use]
    pub fn stats_file_path(&self) -> &std::path::Path {
        &self.stats_file
    }

    /// Get stats file path using kodegen_config (directory creation happens async)
    fn get_stats_file_path(instance_id: &str) -> PathBuf {
        kodegen_config::KodegenConfig::data_dir()
            .map(|dir| dir.join("stats").join(format!("stats_{instance_id}.json")))
            .unwrap_or_else(|_| PathBuf::from(format!("stats_{instance_id}.json")))
    }

    /// Check if this is a new session (30+ min since last activity)
    fn is_new_session(last_used: i64) -> bool {
        let now = chrono::Utc::now().timestamp();
        (now - last_used) > SESSION_TIMEOUT_SECS
    }

    /// Get tool category for categorization using inventory system
    fn get_category(tool_name: &str) -> Option<&'static str> {
        inventory::iter::<kodegen_mcp_schema::ToolMetadata>()
            .find(|tool| tool.name == tool_name)
            .map(|tool| tool.category.name)
    }

    /// Track a successful tool call for a specific connection (fire-and-forget, never blocks)
    pub fn track_success(&self, connection_id: &str, tool_name: &str) {
        let _ = self.update_sender.send(StatsUpdate::Success {
            connection_id: connection_id.to_string(),
            tool_name: tool_name.to_string(),
        });
    }

    /// Track a failed tool call for a specific connection (fire-and-forget, never blocks)
    pub fn track_failure(&self, connection_id: &str, tool_name: &str) {
        let _ = self.update_sender.send(StatsUpdate::Failure {
            connection_id: connection_id.to_string(),
            tool_name: tool_name.to_string(),
        });
    }

    /// Get stats for a specific connection
    #[must_use]
    pub fn get_stats_for_connection(&self, connection_id: &str) -> Option<UsageStats> {
        self.stats_by_connection.get(connection_id).map(|entry| entry.value().clone())
    }

    /// Remove connection stats (called when connection is deleted)
    pub fn remove_connection(&self, connection_id: &str) {
        let _ = self
            .update_sender
            .send(StatsUpdate::RemoveConnection(connection_id.to_string()));
    }

    /// Trigger immediate save to disk (fire-and-forget)
    pub fn save(&self) {
        let _ = self.update_sender.send(StatsUpdate::SaveToDisk);
    }

    /// Trigger final save and shutdown (fire-and-forget)
    pub fn shutdown(&self) {
        let _ = self.update_sender.send(StatsUpdate::Shutdown);
    }

    /// Load stats from disk (atomic read with error recovery)
    fn load_from_disk(stats_file: &PathBuf) -> DashMap<String, UsageStats> {
        match std::fs::read_to_string(stats_file) {
            Ok(json) => match serde_json::from_str::<HashMap<String, UsageStats>>(&json) {
                Ok(map) => {
                    log::info!("Loaded {} connection stats from {}", map.len(), stats_file.display());
                    map.into_iter().collect()
                }
                Err(e) => {
                    log::warn!("Failed to parse stats file {}: {} - starting fresh", stats_file.display(), e);
                    DashMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::debug!("No existing stats file at {} - starting fresh", stats_file.display());
                DashMap::new()
            }
            Err(e) => {
                log::warn!("Failed to read stats file {}: {} - starting fresh", stats_file.display(), e);
                DashMap::new()
            }
        }
    }

    /// Save stats to disk (atomic write with temp file)
    fn save_to_disk(stats_by_connection: &DashMap<String, UsageStats>, stats_file: &PathBuf) {
        // Convert DashMap to HashMap for serialization
        let snapshot: HashMap<String, UsageStats> = stats_by_connection
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        // Serialize to JSON
        let json = match serde_json::to_string_pretty(&snapshot) {
            Ok(j) => j,
            Err(e) => {
                log::error!("Failed to serialize stats: {}", e);
                return;
            }
        };

        // Ensure parent directory exists
        if let Some(parent) = stats_file.parent()
            && let Err(e) = std::fs::create_dir_all(parent) {
                log::error!("Failed to create stats directory {}: {}", parent.display(), e);
                return;
            }

        // Atomic write: write to temp file, then rename
        let temp_file = stats_file.with_extension("json.tmp");

        if let Err(e) = std::fs::write(&temp_file, json) {
            log::error!("Failed to write temp stats file {}: {}", temp_file.display(), e);
            return;
        }

        if let Err(e) = std::fs::rename(&temp_file, stats_file) {
            log::error!("Failed to rename {} to {}: {}", temp_file.display(), stats_file.display(), e);
            let _ = std::fs::remove_file(&temp_file); // Clean up temp file
            return;
        }

        log::debug!("Saved {} connection stats to {}", snapshot.len(), stats_file.display());
    }

    /// Start periodic save timer (saves every 5 minutes)
    fn start_periodic_save_timer(&self) {
        let update_sender = self.update_sender.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(SAVE_INTERVAL_SECS));
            loop {
                interval.tick().await;
                let _ = update_sender.send(StatsUpdate::SaveToDisk);
            }
        });
    }

    /// Background task that processes per-connection stat updates
    fn start_background_processor(
        &self,
        mut update_receiver: tokio::sync::mpsc::UnboundedReceiver<StatsUpdate>,
        stats_file: PathBuf,
    ) {
        let stats_by_connection = Arc::clone(&self.stats_by_connection);

        tokio::spawn(async move {
            loop {
                match update_receiver.recv().await {
                    Some(update) => match update {
                        StatsUpdate::Success {
                            connection_id,
                            tool_name,
                        } => {
                            // Get or create stats for this connection
                            let mut stats = stats_by_connection
                                .entry(connection_id.clone())
                                .or_default();

                            let now = chrono::Utc::now().timestamp();

                            // Check if new session (30 min timeout)
                            if Self::is_new_session(stats.last_used) {
                                stats.total_sessions += 1;
                            }

                            // Update counters
                            stats.total_tool_calls += 1;
                            stats.successful_calls += 1;
                            stats.last_used = now;

                            // Update tool-specific counter
                            *stats.tool_counts.entry(tool_name.clone()).or_insert(0) += 1;

                            // Update category counter
                            if let Some(category) = Self::get_category(&tool_name) {
                                match category {
                                    name if name == kodegen_config::CATEGORY_FILESYSTEM.name => {
                                        stats.filesystem_operations += 1
                                    }
                                    name if name == kodegen_config::CATEGORY_TERMINAL.name => {
                                        stats.terminal_operations += 1
                                    }
                                    name if name == kodegen_config::CATEGORY_INTROSPECTION.name
                                        || name == kodegen_config::CATEGORY_CONFIG.name
                                        || name == kodegen_config::CATEGORY_PROMPT.name => {
                                        stats.config_operations += 1
                                    }
                                    name if name == kodegen_config::CATEGORY_PROCESS.name => {
                                        stats.process_operations += 1
                                    }
                                    _ => {}
                                }
                            }
                        }
                        StatsUpdate::Failure {
                            connection_id,
                            tool_name,
                        } => {
                            // Get or create stats for this connection
                            let mut stats = stats_by_connection
                                .entry(connection_id.clone())
                                .or_default();

                            let now = chrono::Utc::now().timestamp();

                            // Check if new session (30 min timeout)
                            if Self::is_new_session(stats.last_used) {
                                stats.total_sessions += 1;
                            }

                            // Update counters
                            stats.total_tool_calls += 1;
                            stats.failed_calls += 1;
                            stats.last_used = now;

                            // Update tool-specific counter
                            *stats.tool_counts.entry(tool_name.clone()).or_insert(0) += 1;

                            // Update category counter
                            if let Some(category) = Self::get_category(&tool_name) {
                                match category {
                                    name if name == kodegen_config::CATEGORY_FILESYSTEM.name => {
                                        stats.filesystem_operations += 1
                                    }
                                    name if name == kodegen_config::CATEGORY_TERMINAL.name => {
                                        stats.terminal_operations += 1
                                    }
                                    name if name == kodegen_config::CATEGORY_INTROSPECTION.name
                                        || name == kodegen_config::CATEGORY_CONFIG.name
                                        || name == kodegen_config::CATEGORY_PROMPT.name => {
                                        stats.config_operations += 1
                                    }
                                    name if name == kodegen_config::CATEGORY_PROCESS.name => {
                                        stats.process_operations += 1
                                    }
                                    _ => {}
                                }
                            }
                        }
                        StatsUpdate::RemoveConnection(connection_id) => {
                            // Remove stats for this connection
                            stats_by_connection.remove(&connection_id);
                        }
                        StatsUpdate::SaveToDisk => {
                            // Periodic flush to disk
                            Self::save_to_disk(&stats_by_connection, &stats_file);
                        }
                        StatsUpdate::Shutdown => {
                            // Final flush and shutdown
                            log::info!("UsageTracker shutting down - saving stats to disk");
                            Self::save_to_disk(&stats_by_connection, &stats_file);
                            break; // Exit the background processor
                        }
                    },
                    // Channel closed (server shutdown)
                    None => {
                        log::info!("UsageTracker channel closed - final save to disk");
                        Self::save_to_disk(&stats_by_connection, &stats_file);
                        break;
                    }
                }
            }
        });
    }

}
