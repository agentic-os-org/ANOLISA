//! Types for service management operations.

use serde::{Deserialize, Serialize};

/// Structured service status — replaces `systemctl status` text output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvcStatus {
    pub name: String,
    pub active: bool,
    pub enabled: bool,
    pub state: SvcState,
    pub pid: Option<u32>,
    pub uptime_secs: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub description: Option<String>,
    /// Last N lines from journal (saves Agent a follow-up journalctl call).
    pub recent_logs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SvcState {
    Running,
    Stopped,
    Failed,
    Activating,
    Deactivating,
    Unknown(String),
}

/// Result of a service action (start/stop/restart/enable/disable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvcActionResult {
    pub name: String,
    pub action: String,
    pub success: bool,
    pub previous_state: SvcState,
    pub new_state: SvcState,
}

/// Result of listing services.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvcListResult {
    pub services: Vec<SvcStatus>,
    pub total: usize,
}

/// Unified service operation enum for the backend trait.
#[derive(Debug, Clone)]
pub enum SvcAction {
    Status { name: String },
    Start { name: String, dry_run: bool },
    Stop { name: String, dry_run: bool },
    Restart { name: String, dry_run: bool },
    Enable { name: String },
    Disable { name: String },
    List { state_filter: Option<String> },
}
