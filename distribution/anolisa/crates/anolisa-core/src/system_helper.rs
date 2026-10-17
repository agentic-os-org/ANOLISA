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

/// Privilege required by one operation class.
///
/// Every [`OperationType`] is classified explicitly so that an operation added
/// to the enum tomorrow must pick a class here (the compiler enforces it)
/// instead of silently inheriting "allowed for every peer" through a catch-all
/// match arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationPrivilege {
    /// Read-only queries any connected peer may issue.
    AnyPeer,
    /// Operations that stop or reconfigure the service itself.
    RootOnly,
    /// System-mutating operations (package install/remove, service
    /// registration, snapshot/restore). Root may always issue them; other
    /// callers only when the deployment delegated that authority through the
    /// group that owns the service socket.
    RootOrDelegated,
}

/// Returns the privilege class of one operation.
#[must_use]
pub fn operation_privilege(op: OperationType) -> OperationPrivilege {
    match op {
        OperationType::Handshake
        | OperationType::SystemStatus
        | OperationType::OsbaseList
        | OperationType::OsbaseStatus => OperationPrivilege::AnyPeer,
        OperationType::Shutdown => OperationPrivilege::RootOnly,
        OperationType::OsbaseInstall
        | OperationType::OsbaseRemove
        | OperationType::OsbaseUninstall
        | OperationType::OsbaseSetDefault
        | OperationType::OsbaseDoctor
        | OperationType::WsCkptSnapshot
        | OperationType::WsCkptRestore => OperationPrivilege::RootOrDelegated,
    }
}

/// Check whether the given operation is allowed for the authenticated peer.
///
/// `delegated_gid` is the group that owns the service socket — the same group
/// the kernel enforces (including supplementary groups) at `connect()` time.
/// The daemon re-validates it here so that authorization does not depend solely
/// on the socket's filesystem mode, which any later privileged action could
/// relax; `None` disables delegation and leaves mutating operations root-only.
///
/// Read-only queries stay available to every connected peer, and `Shutdown`
/// stays root-only, exactly as before.
pub fn is_operation_allowed(
    op: OperationType,
    peer: &anolisa_platform::ipc::PeerCredential,
    delegated_gid: Option<u32>,
) -> bool {
    match operation_privilege(op) {
        OperationPrivilege::AnyPeer => true,
        OperationPrivilege::RootOnly => peer.uid == 0,
        OperationPrivilege::RootOrDelegated => {
            peer.uid == 0 || delegated_gid.is_some_and(|gid| peer_holds_delegated_group(peer, gid))
        }
    }
}

/// Whether the authenticated peer holds the delegated group.
///
/// `SO_PEERCRED` reports only the primary gid, while group membership granted
/// with `usermod -aG` is supplementary, so the remaining groups are read from
/// `/proc/<pid>/status` — tied back to the stored peer identity (see
/// [`supplementary_groups_matching_peer`]). The peer is alive and blocked on
/// its own request while this runs; an exited, recycled or unreadable peer
/// fails closed.
fn peer_holds_delegated_group(
    peer: &anolisa_platform::ipc::PeerCredential,
    delegated_gid: u32,
) -> bool {
    peer.gid == delegated_gid
        || supplementary_groups_matching_peer(peer).contains(&delegated_gid)
}

/// Supplementary group IDs of the peer's process from `/proc/<pid>/status`,
/// but only when that document still belongs to the authenticated peer.
///
/// `SO_PEERCRED` pins the peer's *numeric* PID, and Linux can recycle that
/// number while the connection stays open; reading the groups of whatever
/// process now owns the number would authorize an unrelated process. The
/// document is therefore tied back to the stored peer identity through the
/// `Uid:` line: the real uid from `SO_PEERCRED` cannot be recycled, so a
/// recycled PID whose replacement runs under a different user fails closed,
/// and a same-uid replacement cannot have gained the delegated group the
/// original caller lacked.
fn supplementary_groups_matching_peer(
    peer: &anolisa_platform::ipc::PeerCredential,
) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{}/status", peer.pid))
        .ok()
        .filter(|status| parse_status_real_uid(status) == Some(peer.uid))
        .map(|status| parse_supplementary_groups(&status))
        .unwrap_or_default()
}

/// Parses the real uid (first field) of the `Uid:` line of one
/// `/proc/<pid>/status` document.
fn parse_status_real_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|fields| fields.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

/// Parses the `Groups:` line of a `/proc/<pid>/status` document.
///
/// Unparsable tokens are skipped rather than rejecting the whole list: the
/// kernel only writes decimal IDs there, so this only tolerates a corrupted
/// read, and one unreadable group must not grant or deny on its own.
fn parse_supplementary_groups(status: &str) -> Vec<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Groups:"))
        .map(|groups| {
            groups
                .split_whitespace()
                .filter_map(|value| value.parse().ok())
                .collect()
        })
        .unwrap_or_default()
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
        // A synthetic peer whose process cannot be inspected (pid -1) and that
        // holds no delegated group.
        let peer = |uid: u32| anolisa_platform::ipc::PeerCredential {
            uid,
            gid: 1000,
            pid: -1,
        };
        assert!(is_operation_allowed(
            OperationType::Shutdown,
            &peer(0),
            Some(4242)
        ));
        assert!(is_operation_allowed(
            OperationType::Shutdown,
            &peer(0),
            None
        ));
        assert!(!is_operation_allowed(
            OperationType::Shutdown,
            &peer(1000),
            Some(4242)
        ));
        assert!(!is_operation_allowed(
            OperationType::Shutdown,
            &peer(1000),
            None
        ));
        // Read-only operations allowed for any uid
        assert!(is_operation_allowed(
            OperationType::OsbaseList,
            &peer(1000),
            None
        ));
        assert!(is_operation_allowed(
            OperationType::SystemStatus,
            &peer(65534),
            None
        ));
        assert!(is_operation_allowed(
            OperationType::Handshake,
            &peer(65534),
            None
        ));
        assert!(is_operation_allowed(
            OperationType::OsbaseStatus,
            &peer(65534),
            None
        ));
    }

    #[test]
    fn whitelist_classifies_every_operation() {
        use OperationPrivilege::*;
        let cases = [
            (OperationType::Handshake, AnyPeer),
            (OperationType::SystemStatus, AnyPeer),
            (OperationType::OsbaseList, AnyPeer),
            (OperationType::OsbaseStatus, AnyPeer),
            (OperationType::Shutdown, RootOnly),
            (OperationType::OsbaseInstall, RootOrDelegated),
            (OperationType::OsbaseRemove, RootOrDelegated),
            (OperationType::OsbaseUninstall, RootOrDelegated),
            (OperationType::OsbaseSetDefault, RootOrDelegated),
            (OperationType::OsbaseDoctor, RootOrDelegated),
            (OperationType::WsCkptSnapshot, RootOrDelegated),
            (OperationType::WsCkptRestore, RootOrDelegated),
        ];
        for (op, expected) in cases {
            assert_eq!(operation_privilege(op), expected, "misclassified {op:?}");
        }
    }

    #[test]
    fn mutating_operations_require_root_or_the_delegated_group() {
        let root = anolisa_platform::ipc::PeerCredential {
            uid: 0,
            gid: 0,
            pid: -1,
        };
        let outsider = anolisa_platform::ipc::PeerCredential {
            uid: 1000,
            gid: 1000,
            // This test process does not hold the synthetic delegated group.
            pid: std::process::id() as i32,
        };
        let primary_member = anolisa_platform::ipc::PeerCredential {
            uid: 1000,
            gid: 4242,
            pid: -1,
        };
        let mutations = [
            OperationType::OsbaseInstall,
            OperationType::OsbaseRemove,
            OperationType::OsbaseUninstall,
            OperationType::OsbaseSetDefault,
            OperationType::OsbaseDoctor,
            OperationType::WsCkptSnapshot,
            OperationType::WsCkptRestore,
        ];
        for op in mutations {
            // Root is always authorized, with or without a delegated group.
            assert!(is_operation_allowed(op, &root, None), "root {op:?}");
            assert!(is_operation_allowed(op, &root, Some(4242)), "root {op:?}");
            // A non-root peer outside the delegated group is refused — the
            // previous catch-all white-list answered true here.
            assert!(
                !is_operation_allowed(op, &outsider, Some(4242)),
                "outsider {op:?}"
            );
            // With no delegation configured, only root may mutate.
            assert!(
                !is_operation_allowed(op, &primary_member, None),
                "undelegated member {op:?}"
            );
            // Membership through the primary gid delegates the operation.
            assert!(
                is_operation_allowed(op, &primary_member, Some(4242)),
                "primary-gid member {op:?}"
            );
        }
    }

    #[test]
    fn supplementary_groups_parse_proc_status_lines() {
        let status = "Name:\tanolisa\n\
                      Uid:\t1000\t1000\t1000\t1000\n\
                      Groups:\t1000 4242 27\n\
                      VmPeak:\t 1234 kB\n";
        assert_eq!(parse_supplementary_groups(status), vec![1000, 4242, 27]);
        // A document without a Groups line is empty, not an error.
        assert!(parse_supplementary_groups("Name:\tanolisa\n").is_empty());
        // A dead or recycled pid fails closed to an empty list.
        assert!(supplementary_groups_matching_peer(&ane_peer(-1, 1000)).is_empty());
        assert!(supplementary_groups_matching_peer(&ane_peer(i32::MAX, 1000)).is_empty());
    }

    fn ane_peer(pid: i32, uid: u32) -> anolisa_platform::ipc::PeerCredential {
        anolisa_platform::ipc::PeerCredential { uid, gid: uid, pid }
    }

    #[test]
    fn groups_require_the_status_document_to_belong_to_the_peer() {
        let pid = std::process::id() as i32;
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        let self_uid = parse_status_real_uid(&status).expect("status has a Uid line");

        // The document belongs to this process: the peer's uid matches, so
        // the groups are the process's own.
        let own = supplementary_groups_matching_peer(&ane_peer(pid, self_uid));
        assert_eq!(own, parse_supplementary_groups(&status));

        // A recycled PID: the peer authenticated as one uid, but the document
        // now belongs to a process running as another uid. The mismatch must
        // fail closed to an empty group list instead of authorizing the
        // replacement process's groups.
        let other_uid = if self_uid == 1000 { 1001 } else { 1000 };
        assert!(
            supplementary_groups_matching_peer(&ane_peer(pid, other_uid)).is_empty(),
            "a status document from another uid must not yield groups"
        );

        // A pid that never existed still yields nothing.
        assert!(supplementary_groups_matching_peer(&ane_peer(-1, self_uid)).is_empty());
    }

    #[test]
    fn status_real_uid_parses_the_first_uid_field() {
        let status = "Name:\tanolisa\n\
                      Uid:\t1000\t1001\t1002\t1003\n\
                      Groups:\t1000 27\n";
        assert_eq!(parse_status_real_uid(status), Some(1000));
        assert_eq!(parse_status_real_uid("Name:\tx\n"), None);
        assert_eq!(parse_status_real_uid("Uid:\tnotanumber\t0\t0\t0\n"), None);
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
