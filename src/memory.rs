use sysinfo::{Pid, ProcessesToUpdate, System};

/// Get current process memory usage in bytes (cross-platform)
/// Returns resident set size (RSS) on Linux/macOS/Unix or Working Set on Windows
pub fn get_memory_used() -> Option<u64> {
    // Get current process ID
    let pid = Pid::from_u32(std::process::id());

    // Create system and refresh only current process (most efficient)
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);

    // Get memory in bytes
    system.process(pid).map(|process| process.memory())
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.2} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{} bytes", bytes)
    }
}
