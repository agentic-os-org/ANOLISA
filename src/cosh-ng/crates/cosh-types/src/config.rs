use serde::{Deserialize, Serialize};

/// Runtime configuration for the cosh CLI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoshConfig {
    /// Override the detected package manager backend.
    pub pkg_backend: Option<PkgBackendOverride>,
    /// Path to the ws-ckpt daemon socket.
    pub checkpoint_socket: Option<String>,
    /// Security audit policy level.
    pub audit_policy: Option<AuditPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PkgBackendOverride {
    Dnf,
    Apt,
    Zypper,
    Yum,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditPolicy {
    Strict,
    Permissive,
}
