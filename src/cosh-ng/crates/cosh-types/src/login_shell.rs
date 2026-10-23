//! Explicit login-shell management results, independent of Agent integration.

use serde::Serialize;
use std::path::PathBuf;

/// Snapshot after an operation, or the unchanged snapshot for a dry run.
#[derive(Debug, Serialize)]
pub struct LoginShellReport {
    /// Operation requested by the caller.
    pub operation: String,
    /// Public entry path stored in shell registration and account records.
    pub shell: PathBuf,
    /// Resolved executable target; distinct from the public symlink entry.
    pub resolved_executable: Option<PathBuf>,
    /// Whether the entry resolves to a regular file with at least one execute bit.
    pub executable: bool,
    /// Whether the entry is listed in the available login shells.
    pub registered: bool,
    /// Explicit account inspected or changed, if requested.
    pub user: Option<String>,
    /// Current shell of that account; dry runs report the unchanged value.
    pub account_shell: Option<PathBuf>,
    /// Accounts matching this entry by path components, without resolving symlinks.
    pub using_accounts: Vec<String>,
    /// Whether this operation changed state, or would change it in a dry run.
    pub changed: bool,
    /// Previous account shell, suitable for an explicitly selected restoration.
    pub previous_shell: Option<PathBuf>,
    /// A missing-only removal retained an entry because its provider still exists.
    pub retained_provider: bool,
    /// Whether set/restore checked target access using the account's credentials.
    /// False for unprivileged previews and operations without account selection.
    pub account_access_checked: bool,
}
