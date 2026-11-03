//! Shared process identity and discovery selection contracts.

use serde::{Deserialize, Serialize};

/// Instance identity in the daemon's procfs PID namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessIdentity {
    /// Host boot identity fences instances across reboot.
    pub boot_id: String,
    /// PID namespace inode link observed by the scanner.
    pub pid_namespace: String,
    /// Process ID in the scanner's procfs view, not necessarily a host PID.
    pub pid: u32,
    /// Linux stat field 22, in clock ticks since boot.
    pub start_time: u64,
}
