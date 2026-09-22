//! IPC protocol types and security validation for the ANOLISA system-helper.
//!
//! This module defines the request/response envelope exchanged over the
//! Unix socket between the unprivileged CLI and the privileged helper daemon,
//! along with operation-type extraction, white-list validation, and a simple
//! per-UID rate limiter.

use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

// ─── Request / Response envelopes ───────────────────────────────────────────

/// CLI → Helper request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum HelperRequest {
    /// Version handshake — must be the first message after connecting.
    Handshake { cli_version: String },

    /// Install a scenario image via osbase.
    OsbaseInstall {
        scenario: String,
        register_handler: String, // "containerd" | "none"
        register_runtimeclass: bool,
        config_override: Option<String>,
        set_default: bool,
        force: bool,
        skip_verify: bool,
        dry_run: bool,
    },
    /// Remove a scenario.
    OsbaseRemove { scenario: String, purge: bool },
    /// Uninstall scenario packages (dnf remove).
    OsbaseUninstall { scenario: String, dry_run: bool },
    /// List scenarios (filter: "available" | "installed" | None).
    OsbaseList { filter: Option<String> },
    /// Query status of scenario(s).
    OsbaseStatus { scenario: Option<String> },
    /// Set a scenario as the default.
    OsbaseSetDefault { scenario: String },
    /// Run diagnostics on scenario(s), optionally auto-fix.
    OsbaseDoctor { scenario: Option<String>, fix: bool },

    /// ws-ckpt: take a workspace snapshot (reserved).
    WsCkptSnapshot { workspace: String },
    /// ws-ckpt: restore a workspace checkpoint (reserved).
    WsCkptRestore {
        workspace: String,
        checkpoint_id: String,
    },

    /// Query the helper's running status.
    SystemStatus,
    /// Gracefully shut down the helper.
    Shutdown,
}

/// Helper → CLI response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum HelperResponse {
    /// Handshake result.
    HandshakeOk {
        helper_version: String,
        compatible: bool,
    },

    /// Operation completed successfully.
    Success { message: String, exit_code: i32 },

    /// Intermediate progress (streaming, one per phase).
    Progress {
        phase: String,
        status: String,
        message: Option<String>,
    },

    /// Operation failed.
    Error { code: String, message: String },

    /// System status report.
    Status {
        running: bool,
        version: String,
        uptime_secs: u64,
        last_operation: Option<String>,
        last_operation_time: Option<String>,
    },
}

// ─── Operation classification ───────────────────────────────────────────────

/// Discrete operation types derived from [`HelperRequest`] — used for
/// white-list validation and rate limiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationType {
    Handshake,
    OsbaseInstall,
    OsbaseRemove,
    OsbaseUninstall,
    OsbaseList,
    OsbaseStatus,
    OsbaseSetDefault,
    OsbaseDoctor,
    WsCkptSnapshot,
    WsCkptRestore,
    SystemStatus,
    Shutdown,
}

/// Extract the [`OperationType`] from a request.
pub fn operation_type(req: &HelperRequest) -> OperationType {
    match req {
        HelperRequest::Handshake { .. } => OperationType::Handshake,
        HelperRequest::OsbaseInstall { .. } => OperationType::OsbaseInstall,
        HelperRequest::OsbaseRemove { .. } => OperationType::OsbaseRemove,
        HelperRequest::OsbaseUninstall { .. } => OperationType::OsbaseUninstall,
        HelperRequest::OsbaseList { .. } => OperationType::OsbaseList,
        HelperRequest::OsbaseStatus { .. } => OperationType::OsbaseStatus,
        HelperRequest::OsbaseSetDefault { .. } => OperationType::OsbaseSetDefault,
        HelperRequest::OsbaseDoctor { .. } => OperationType::OsbaseDoctor,
        HelperRequest::WsCkptSnapshot { .. } => OperationType::WsCkptSnapshot,
        HelperRequest::WsCkptRestore { .. } => OperationType::WsCkptRestore,
        HelperRequest::SystemStatus => OperationType::SystemStatus,
        HelperRequest::Shutdown => OperationType::Shutdown,
    }
}

// ─── White-list validation ──────────────────────────────────────────────────

/// Check whether the given operation is allowed for the specified UID.
///
/// The white-list is the enum itself — any operation that can be deserialized
/// from the wire is considered valid.  `Shutdown` is restricted to root
/// (uid 0) only.
pub fn is_operation_allowed(op: OperationType, uid: u32) -> bool {
    match op {
        OperationType::Shutdown => uid == 0,
        _ => true,
    }
}

// ─── Rate limiter ───────────────────────────────────────────────────────────

/// Simple sliding-window rate limiter keyed by UID.
///
/// Tracks the most recent operation timestamps per user and rejects requests
/// that exceed `max_per_minute` within a rolling 60-second window.
#[derive(Debug)]
pub struct RateLimiter {
    records: HashMap<u32, Vec<Instant>>,
    max_per_minute: usize,
}

impl RateLimiter {
    /// Create a new rate limiter with the given per-minute cap.
    pub fn new(max_per_minute: usize) -> Self {
        Self {
            records: HashMap::new(),
            max_per_minute,
        }
    }

    /// Check whether `uid` is allowed to perform another operation.
    ///
    /// On success returns `Ok(())`.  On rejection returns an `Err` with a
    /// human-readable description.
    pub fn check(&mut self, uid: u32) -> Result<(), String> {
        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);

        let timestamps = self.records.entry(uid).or_default();

        // Evict entries outside the window.
        timestamps.retain(|t| now.duration_since(*t) < window);

        if timestamps.len() >= self.max_per_minute {
            return Err(format!(
                "rate limit exceeded for uid {uid}: max {}/min",
                self.max_per_minute
            ));
        }

        timestamps.push(now);
        Ok(())
    }

    /// Reset all tracked state (useful for testing).
    pub fn reset(&mut self) {
        self.records.clear();
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_type_extraction() {
        let req = HelperRequest::OsbaseInstall {
            scenario: "default".into(),
            register_handler: "containerd".into(),
            register_runtimeclass: true,
            config_override: None,
            set_default: false,
            force: false,
            skip_verify: false,
            dry_run: true,
        };
        assert_eq!(operation_type(&req), OperationType::OsbaseInstall);

        assert_eq!(
            operation_type(&HelperRequest::SystemStatus),
            OperationType::SystemStatus
        );
        assert_eq!(
            operation_type(&HelperRequest::Shutdown),
            OperationType::Shutdown
        );
    }

    #[test]
    fn whitelist_shutdown_requires_root() {
        assert!(!is_operation_allowed(OperationType::Shutdown, 1000));
        assert!(is_operation_allowed(OperationType::Shutdown, 0));
        // Non-shutdown operations allowed for any uid
        assert!(is_operation_allowed(OperationType::OsbaseList, 1000));
        assert!(is_operation_allowed(OperationType::SystemStatus, 65534));
    }

    #[test]
    fn rate_limiter_allows_within_limit() {
        let mut rl = RateLimiter::new(5);
        for _ in 0..5 {
            assert!(rl.check(1000).is_ok());
        }
        // 6th should fail
        assert!(rl.check(1000).is_err());
        // Different uid still ok
        assert!(rl.check(1001).is_ok());
    }

    #[test]
    fn rate_limiter_reset_clears_state() {
        let mut rl = RateLimiter::new(2);
        assert!(rl.check(1).is_ok());
        assert!(rl.check(1).is_ok());
        assert!(rl.check(1).is_err());
        rl.reset();
        assert!(rl.check(1).is_ok());
    }

    #[test]
    fn request_serialization_roundtrip() {
        let req = HelperRequest::OsbaseDoctor {
            scenario: Some("gpu".into()),
            fix: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: HelperRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, deserialized);
    }

    #[test]
    fn response_serialization_roundtrip() {
        let resp = HelperResponse::Progress {
            phase: "download".into(),
            status: "complete".into(),
            message: Some("256 MiB fetched".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: HelperResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, deserialized);
    }
}
