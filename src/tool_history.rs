use chrono::{DateTime, Utc};
use dashmap::DashMap;
use kodegen_config::KodegenConfig;
use kodegen_mcp_schema::tool::tool_history::ToolCallRecord;
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use termcolor::{BufferWriter, ColorChoice};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

const MAX_HISTORY_ENTRIES: usize = 1000;
const MAX_DISK_ENTRIES: usize = 5000;
const ROTATION_CHECK_INTERVAL: usize = 100;

/// Update event for background processor
enum HistoryUpdate {
    AddCall {
        connection_id: String,
        record: ToolCallRecord,
    },
    RemoveConnection(String), // connection_id
}

/// Tool call history manager with per-connection in-memory cache and disk persistence
#[derive(Clone)]
pub struct ToolHistory {
    /// Per-connection entries (connection_id -> VecDeque<ToolCallRecord>)
    entries_by_connection: Arc<DashMap<String, VecDeque<ToolCallRecord>>>,

    /// Path to JSONL history file
    history_file: PathBuf,

    /// Write queue for async batching (per-connection)
    write_queue: Arc<DashMap<String, Vec<ToolCallRecord>>>,

    /// Fire-and-forget channel for recording calls
    update_sender: tokio::sync::mpsc::UnboundedSender<HistoryUpdate>,

    /// Counter for rotation check
    writes_since_check: Arc<tokio::sync::RwLock<usize>>,
}

impl ToolHistory {
    /// Create new history manager and start background writer
    pub async fn new(instance_id: String) -> Self {
        // Determine history file location
        let history_dir = KodegenConfig::log_dir()
            .unwrap_or_else(|_| PathBuf::from("logs"));

        // Create directory if needed (async)
        if let Err(e) = tokio::fs::create_dir_all(&history_dir).await {
            let bufwtr = BufferWriter::stderr(ColorChoice::Auto);
            let mut buffer = bufwtr.buffer();
            let _ = writeln!(&mut buffer, "Failed to create history directory: {e}");
            let _ = bufwtr.print(&buffer);
        }

        let history_file = history_dir.join(format!("tool-history_{instance_id}.jsonl"));

        // Create unbounded channel for fire-and-forget recording
        let (update_sender, update_receiver) = tokio::sync::mpsc::unbounded_channel();

        let history = Self {
            entries_by_connection: Arc::new(DashMap::new()),
            history_file: history_file.clone(),
            write_queue: Arc::new(DashMap::new()),
            update_sender,
            writes_since_check: Arc::new(tokio::sync::RwLock::new(0)),
        };

        // Load existing history from disk (stored globally but distributed per-connection on load)
        history.load_from_disk().await;

        // Start background processor
        history.start_background_processor(update_receiver);

        history
    }

    /// Add a tool call to history for a specific connection (fire-and-forget, never blocks)
    pub fn track_call(
        &self,
        connection_id: &str,
        tool_name: String,
        arguments: serde_json::Value,
        output: serde_json::Value,
        duration_ms: Option<u64>,
    ) {
        // Serialize Value → String immediately (single allocation per field)
        let args_json = serde_json::to_string(&arguments)
            .unwrap_or_else(|_| "{}".to_string());
        let output_json = serde_json::to_string(&output)
            .unwrap_or_else(|_| "{}".to_string());

        let record = ToolCallRecord {
            timestamp: Utc::now().to_rfc3339(),
            tool_name,
            args_json,
            output_json,
            duration_ms,
        };

        // Fire-and-forget: send to background processor
        // If send fails (channel closed), silently ignore - history is best-effort
        let _ = self.update_sender.send(HistoryUpdate::AddCall {
            connection_id: connection_id.to_string(),
            record,
        });
    }

    /// Get history for a specific connection
    pub fn get_history_for_connection(&self, connection_id: &str) -> Option<Vec<ToolCallRecord>> {
        self.entries_by_connection
            .get(connection_id)
            .map(|entry| entry.value().iter().cloned().collect())
    }

    /// Get recent tool calls for a specific connection with optional filters and offset support
    pub fn get_recent_calls_for_connection(
        &self,
        connection_id: &str,
        max_results: usize,
        offset: i64,
        tool_name: Option<&str>,
        since: Option<&str>,
    ) -> Vec<ToolCallRecord> {
        // Get entries for this connection
        let entries = match self.entries_by_connection.get(connection_id) {
            Some(entry) => entry.value().clone(),
            None => return Vec::new(),
        };

        // Parse since timestamp if provided
        let since_dt = since.and_then(|s| DateTime::parse_from_rfc3339(s).ok());

        // Filter entries
        let filtered: Vec<_> = entries
            .iter()
            .filter(|record| {
                // Filter by tool name
                if let Some(name) = tool_name
                    && record.tool_name != name
                {
                    return false;
                }

                // Filter by timestamp
                if let Some(since_dt) = since_dt
                    && let Ok(record_dt) = DateTime::parse_from_rfc3339(&record.timestamp)
                    && record_dt < since_dt
                {
                    return false;
                }

                true
            })
            .cloned()
            .collect();

        // Apply offset-based pagination
        let total = filtered.len();

        let (start, end) = if offset < 0 {
            // Negative offset: tail behavior (max_results ignored)
            let tail_count = usize::try_from(-offset).unwrap_or(0).min(total);
            let start = total.saturating_sub(tail_count);
            (start, total)
        } else {
            // Positive offset: standard forward reading with max_results
            let limit = max_results.min(MAX_HISTORY_ENTRIES);
            let start = usize::try_from(offset).unwrap_or(0).min(total);
            let end = (start + limit).min(total);
            (start, end)
        };

        filtered[start..end].to_vec()
    }

    /// Remove connection history (called when connection is deleted)
    pub fn remove_connection(&self, connection_id: &str) {
        let _ = self
            .update_sender
            .send(HistoryUpdate::RemoveConnection(connection_id.to_string()));
    }

    /// Load history from disk (JSONL format) - stored globally but for backward compatibility
    async fn load_from_disk(&self) {
        if !tokio::fs::try_exists(&self.history_file)
            .await
            .unwrap_or(false)
        {
            return;
        }

        match tokio::fs::read_to_string(&self.history_file).await {
            Ok(content) => {
                let mut entries = VecDeque::new();

                // Parse each line as JSON
                for line in content.lines() {
                    if let Ok(record) = serde_json::from_str::<ToolCallRecord>(line) {
                        entries.push_back(record);
                    }
                }

                // Keep only last 1000 entries
                while entries.len() > MAX_HISTORY_ENTRIES {
                    entries.pop_front();
                }

                // For backward compatibility, store in a "__legacy__" connection_id
                // In practice, this data won't be visible to any specific connection
                if !entries.is_empty() {
                    self.entries_by_connection.insert("__legacy__".to_string(), entries);
                }
            }
            Err(e) => {
                let bufwtr = BufferWriter::stderr(ColorChoice::Auto);
                let mut buffer = bufwtr.buffer();
                let _ = writeln!(&mut buffer, "Failed to load tool history: {e}");
                let _ = bufwtr.print(&buffer);
            }
        }
    }

    /// Start background processor task (receives updates, updates cache, writes to disk)
    fn start_background_processor(
        &self,
        mut update_receiver: tokio::sync::mpsc::UnboundedReceiver<HistoryUpdate>,
    ) {
        let entries_by_connection = Arc::clone(&self.entries_by_connection);
        let write_queue = Arc::clone(&self.write_queue);
        let writes_since_check = Arc::clone(&self.writes_since_check);
        let history_file = self.history_file.clone();

        tokio::spawn(async move {
            // Disk flush interval (1 second)
            let mut flush_interval = tokio::time::interval(std::time::Duration::from_secs(1));

            loop {
                tokio::select! {
                    // Receive new updates from channel
                    Some(update) = update_receiver.recv() => {
                        match update {
                            HistoryUpdate::AddCall { connection_id, record } => {
                                // Update in-memory cache for this connection
                                {
                                    let mut entries = entries_by_connection
                                        .entry(connection_id.clone())
                                        .or_default();

                                    entries.push_back(record.clone());

                                    // Keep only last 1000 in memory per connection
                                    if entries.len() > MAX_HISTORY_ENTRIES {
                                        entries.pop_front();
                                    }
                                }

                                // Queue for disk write
                                {
                                    write_queue
                                        .entry(connection_id)
                                        .or_default()
                                        .push(record);
                                }
                            }
                            HistoryUpdate::RemoveConnection(connection_id) => {
                                // Remove from memory
                                entries_by_connection.remove(&connection_id);

                                // Remove from write queue
                                write_queue.remove(&connection_id);
                            }
                        }
                    }

                    // Periodic disk flush
                    _ = flush_interval.tick() => {
                        // Collect all records from all connection queues
                        let mut all_records = Vec::new();

                        for mut entry in write_queue.iter_mut() {
                            let records = std::mem::take(entry.value_mut());
                            all_records.extend(records);
                        }

                        if all_records.is_empty() {
                            continue;
                        }

                        // Append to file (JSONL format)
                        match OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&history_file)
                            .await
                        {
                            Ok(mut file) => {
                                for record in &all_records {
                                    if let Ok(json) = serde_json::to_string(record) {
                                        let line = format!("{json}\n");
                                        let _ = file.write_all(line.as_bytes()).await;
                                    }
                                }
                            }
                            Err(e) => {
                                let bufwtr = BufferWriter::stderr(ColorChoice::Auto);
                                let mut buffer = bufwtr.buffer();
                                let _ = writeln!(&mut buffer, "Failed to write tool history: {e}");
                                let _ = bufwtr.print(&buffer);
                                continue;
                            }
                        }

                        // Check if rotation is needed
                        let should_rotate = {
                            let mut check_counter = writes_since_check.write().await;
                            *check_counter += all_records.len();

                            if *check_counter >= ROTATION_CHECK_INTERVAL {
                                *check_counter = 0;
                                true
                            } else {
                                false
                            }
                        };

                        if should_rotate {
                            // Perform rotation check
                            if let Err(e) = Self::rotate_if_needed(&history_file).await {
                                let bufwtr = BufferWriter::stderr(ColorChoice::Auto);
                                let mut buffer = bufwtr.buffer();
                                let _ = writeln!(&mut buffer, "Failed to rotate tool history: {e}");
                                let _ = bufwtr.print(&buffer);
                            }
                        }
                    }

                    // Channel closed (shutdown)
                    else => {
                        // Flush any remaining records before exiting
                        let mut all_records = Vec::new();

                        for entry in write_queue.iter() {
                            all_records.extend(entry.value().clone());
                        }

                        if !all_records.is_empty()
                            && let Ok(mut file) = OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&history_file)
                                .await
                        {
                            for record in &all_records {
                                if let Ok(json) = serde_json::to_string(record) {
                                    let line = format!("{json}\n");
                                    let _ = file.write_all(line.as_bytes()).await;
                                }
                            }
                        }

                        break;
                    }
                }
            }
        });
    }

    /// Check if file needs rotation and rotate if necessary
    ///
    /// This is called periodically by the background writer when the write counter
    /// reaches `ROTATION_CHECK_INTERVAL`. If the file has more than `MAX_DISK_ENTRIES`
    /// lines, it keeps only the last `MAX_DISK_ENTRIES` lines and atomically replaces
    /// the file using a temp file + rename strategy.
    async fn rotate_if_needed(history_file: &PathBuf) -> Result<(), std::io::Error> {
        // Read current file
        let content = match tokio::fs::read_to_string(history_file).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };

        // Count lines
        let line_count = content.lines().count();

        // Only rotate if exceeds limit
        if line_count <= MAX_DISK_ENTRIES {
            return Ok(());
        }

        // Keep only the last MAX_DISK_ENTRIES lines
        let keep_from = line_count.saturating_sub(MAX_DISK_ENTRIES);
        let kept_lines: Vec<&str> = content.lines().skip(keep_from).collect();

        // Write to temporary file (atomic operation step 1)
        let temp_file = history_file.with_extension("jsonl.tmp");
        {
            let mut file = tokio::fs::File::create(&temp_file).await?;
            for line in kept_lines {
                file.write_all(line.as_bytes()).await?;
                file.write_all(b"\n").await?;
            }
            file.sync_all().await?;
        }

        // Atomic rename (atomic operation step 2)
        // On Unix systems (including macOS), this is an atomic filesystem operation
        tokio::fs::rename(&temp_file, history_file).await?;

        Ok(())
    }
}
