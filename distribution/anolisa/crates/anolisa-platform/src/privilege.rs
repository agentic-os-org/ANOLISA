//! Privilege inspection helpers.

/// Check if the current process has root privileges.
pub fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Effective uid used for permission-gate decisions.
pub fn effective_uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}
