//! Linux login-shell registration and explicit account selection.
//!
//! Mutations require root and serialize with other invocations of this manager.
//! Account changes delegate to usermod; the expected-shell guard detects prior
//! administrator edits but is not an atomic compare-and-swap against other tools.

#[cfg(target_os = "linux")]
mod access;
#[cfg(target_os = "linux")]
mod storage;
#[cfg(target_os = "linux")]
mod system;
#[cfg(all(test, target_os = "linux"))]
mod tests;

use cosh_types::login_shell::LoginShellReport;
use std::path::{Path, PathBuf};

/// Failure at a configuration, privilege, filesystem, or account-tool boundary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The platform does not provide the supported Linux management interface.
    #[error("login-shell management is supported on Linux only")]
    Unsupported,
    /// An argument does not satisfy the management contract.
    #[error("invalid login-shell request: {0}")]
    Invalid(String),
    /// A mutation requires an explicitly privileged caller.
    #[error("login-shell mutations require root; status and --dry-run are unprivileged")]
    Permission,
    /// The destination cannot be executed with the selected account's credentials.
    #[error("login-shell target is not accessible to the account: {0}")]
    AccessDenied(String),
    /// Current state differs from the caller's expectation or is still in use.
    #[error("login-shell state conflict: {0}")]
    Conflict(String),
    /// A filesystem operation failed before or after the documented commit point.
    #[error("login-shell filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// A bounded operating-system account command failed.
    #[error("login-shell account operation failed: {0}")]
    Backend(String),
}

/// Independently selectable operations; no operation enables AW or starts Herdr.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Read executable, registration, and optional account state.
    Status,
    /// Add the shell entry without selecting it for any account.
    Register,
    /// Remove the entry only when no account uses it.
    Unregister,
    /// Select the installed cosh entry for an explicit local account.
    Set,
    /// Select an explicit replacement shell for an explicit local account.
    Restore,
}

#[cfg(target_os = "linux")]
impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Register => "register",
            Self::Unregister => "unregister",
            Self::Set => "set",
            Self::Restore => "restore",
        }
    }
}

/// Validated inputs to one management operation.
#[derive(Debug)]
pub struct Request {
    /// Operation to perform.
    pub operation: Operation,
    /// Public cosh entry, not its canonicalized private executable target.
    pub shell: PathBuf,
    /// Explicit account; required for set and restore.
    pub user: Option<String>,
    /// Expected current account shell; required for set and restore.
    pub expect_shell: Option<PathBuf>,
    /// Explicit replacement shell; required for restore.
    pub restore_shell: Option<PathBuf>,
    /// Inspect and validate the proposed change without writing or invoking usermod.
    pub dry_run: bool,
    /// Keep registration if an executable replacement provider is still present.
    pub if_missing: bool,
}

/// Resolve the public entry beside this installation's cosh-cli unless overridden.
///
/// Does not search PATH or execute the selected entry. The registration keeps the
/// public entry spelling so RPM provider swaps retain their stable account path.
///
/// # Errors
/// Returns an error for a nonabsolute or shell-table-incompatible path.
pub fn entry_path(cli: &Path, selected: Option<&Path>) -> Result<PathBuf, Error> {
    let path = selected
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cli.parent().unwrap_or_else(|| Path::new(".")).join("cosh"));
    validate_path(&path)?;
    Ok(path)
}

fn validate_path(path: &Path) -> Result<(), Error> {
    let text = path
        .to_str()
        .ok_or_else(|| Error::Invalid("shell path must be UTF-8".into()))?;
    if !path.is_absolute()
        || text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, ':' | '#' | '\0'))
        || text.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(Error::Invalid(
            "shell path must be absolute without whitespace, #, colon, or dot components".into(),
        ));
    }
    Ok(())
}

/// Execute one request against the host configuration.
/// `cli` must be the trusted cosh-cli executable providing the internal access
/// probe; account selection re-executes it with a bounded lifetime.
///
/// # Errors
/// Reports unsupported platforms, invalid input, privilege failures, conflicts,
/// and filesystem/account-tool failures. No account is inferred from sudo or HOME.
pub fn execute(request: &Request, cli: &Path) -> Result<LoginShellReport, Error> {
    #[cfg(target_os = "linux")]
    {
        system::execute(request, cli)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (request, cli);
        Err(Error::Unsupported)
    }
}

/// Check a shell in a disposable CLI child before starting any worker threads.
///
/// A privileged child permanently adopts the requested UID, primary GID and NSS
/// supplementary groups. An unprivileged child can only check its own identity.
/// Never call this in the managing process: credentials are not restored.
/// The shell is inspected, never executed.
///
/// # Errors
/// Reports credential setup, filesystem, and unsupported-platform failures.
pub fn probe_access(user: &str, uid: u32, gid: u32, shell: &Path) -> Result<bool, Error> {
    #[cfg(target_os = "linux")]
    {
        access::probe(user, uid, gid, shell)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (user, uid, gid, shell);
        Err(Error::Unsupported)
    }
}
