use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use crate::memory::{get_memory_used, format_bytes};

struct MemorySnapshot {
    memory: u64,
    requests: u64,
    timestamp: std::time::Instant,
}

pub fn spawn_memory_monitor(
    requests_processed: Arc<AtomicU64>,
    ct: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut last_snapshot: Option<MemorySnapshot> = None;

        loop {
            tokio::select! {
                _ = ct.cancelled() => break,
                _ = interval.tick() => {
                    let memory = match get_memory_used() {
                        Some(m) => m,
                        None => continue,
                    };
                    let requests = requests_processed.load(Ordering::SeqCst);
                    let now = std::time::Instant::now();

                    if let Some(ref prev) = last_snapshot {
                        let memory_growth = memory.saturating_sub(prev.memory);
                        let threshold = 100 * 1024 * 1024; // 100MB

                        if memory_growth >= threshold {
                            let elapsed = now.duration_since(prev.timestamp);
                            let requests_delta = requests.saturating_sub(prev.requests);

                            log::warn!(
                                "Memory growth detected: {} over {:?} ({} requests processed)",
                                format_bytes(memory_growth),
                                elapsed,
                                requests_delta
                            );
                        }
                    }

                    last_snapshot = Some(MemorySnapshot {
                        memory,
                        requests,
                        timestamp: now,
                    });
                }
            }
        }
    });
}
