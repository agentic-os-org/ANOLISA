//! Types for workspace checkpoint operations (ws-ckpt daemon IPC).
//!
//! These types mirror the ws-ckpt daemon protocol exactly.
//! The enum variant order is critical — bincode serializes enums by index.

use serde::{Deserialize, Serialize};

/// Default socket path for ws-ckpt daemon.
pub const DEFAULT_SOCKET_PATH: &str = "/run/ws-ckpt/ws-ckpt.sock";
/// Exact protocol version implemented by the guarded checkpoint API.
pub const GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2: u16 = 2;

// ===========================================================================
// Wire protocol types (must match ws-ckpt-common exactly)
// ===========================================================================

/// Request sent to ws-ckpt daemon over Unix socket (bincode wire format).
/// CRITICAL: variant order must match ws-ckpt-common/src/lib.rs exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WsCkptRequest {
    Init {
        workspace: String,
    },
    Checkpoint {
        workspace: String,
        id: String,
        message: Option<String>,
        metadata: Option<String>,
        pin: bool,
    },
    Rollback {
        workspace: String,
        to: Option<String>,
        num_ancestors: Option<u32>,
    },
    Delete {
        workspace: Option<String>,
        snapshot: String,
        force: bool,
    },
    List {
        workspace: Option<String>,
        format: Option<String>,
    },
    Diff {
        workspace: String,
        from: String,
        to: Option<String>,
    },
    Status {
        workspace: Option<String>,
    },
    Cleanup {
        workspace: String,
        keep: Option<u32>,
    },
    Config,
    ReloadConfig,
    ReloadGlobalConfig,
    ReloadWorkspacePolicy {
        workspace: String,
    },
    ConfigOverview,
    Recover {
        workspace: String,
    },
    HealthAdvisory,
    GetWorkspacePolicy {
        workspace: String,
    },
    ResetWorkspacePolicy {
        workspace: String,
    },
    PatchWorkspacePolicy {
        workspace: String,
        auto_cleanup: PolicyFieldOp<bool>,
        auto_cleanup_keep: PolicyFieldOp<CleanupRetention>,
    },
    RollbackPreview {
        workspace: String,
        to: Option<String>,
        num_ancestors: Option<u32>,
    },
    /// Resolves the stable identity of an already-registered workspace.
    WorkspaceIdentityV2 {
        registration_path: String,
    },
    /// Creates a checkpoint under an atomic workspace-generation fence.
    GuardedCheckpointV2 {
        ws_id: String,
        expected_generation: WorkspaceGenerationTokenV2,
        checkpoint_id: String,
        operation_digest: [u8; 32],
        message: Option<String>,
        metadata: Option<String>,
        pin: bool,
    },
    /// Queries exact durable evidence for one guarded operation.
    CheckpointEvidenceV2 {
        ws_id: String,
        expected_generation: WorkspaceGenerationTokenV2,
        checkpoint_id: String,
        operation_digest: [u8; 32],
    },
    /// Previews an exact rollback target under workspace and generation fences.
    GuardedRollbackPreviewV2 {
        registered_path: String,
        ws_id: String,
        expected_generation: WorkspaceGenerationTokenV2,
        target_snapshot_id: String,
    },
    /// Atomically revalidates the preview and switches to its exact target.
    GuardedRollbackV2 {
        registered_path: String,
        ws_id: String,
        expected_generation: WorkspaceGenerationTokenV2,
        target_snapshot_id: String,
        expected_diff_digest: [u8; 32],
        operation_id: String,
        operation_digest: [u8; 32],
    },
    /// Queries durable evidence for one guarded rollback operation.
    GuardedRollbackEvidenceV2 {
        ws_id: String,
        operation_id: String,
        operation_digest: [u8; 32],
    },
    /// Removes a registration only when its live subvolume is missing.
    Unregister {
        /// Registered workspace path or ID.
        workspace: String,
    },
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum PolicyFieldOp<T> {
    #[default]
    Unchanged,
    Set(T),
}

/// Response received from ws-ckpt daemon (bincode wire format).
/// CRITICAL: variant order must match ws-ckpt-common/src/lib.rs exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WsCkptResponse {
    InitOk {
        ws_id: String,
    },
    CheckpointOk {
        snapshot_id: String,
    },
    RollbackOk {
        from: String,
        to: String,
    },
    DeleteOk {
        target: String,
    },
    Error {
        code: WsCkptErrorCode,
        message: String,
    },
    ListOk {
        snapshots: Vec<SnapshotEntry>,
    },
    DiffOk {
        changes: Vec<DiffEntry>,
    },
    StatusOk {
        report: StatusReport,
    },
    CleanupOk {
        removed: Vec<String>,
    },
    ConfigOk {
        config: ConfigReport,
    },
    ReloadConfigOk {
        config: ConfigReport,
    },
    CheckpointSkipped {
        reason: String,
    },
    RecoverOk {
        workspace: String,
    },
    HealthAdvisoryOk {
        over_limit_workspace_count: u32,
        fs_total_bytes: u64,
        fs_used_bytes: u64,
    },
    WorkspacePolicyOk {
        ws_id: String,
        effective: EffectivePolicy,
        local: WorkspacePolicy,
        global: GlobalPolicySnapshot,
    },
    ConfigOverviewOk {
        config: ConfigReport,
        ws_total: usize,
        ws_with_override: usize,
    },
    RollbackPreviewOk {
        to: String,
        changes: Vec<DiffEntry>,
    },
    /// Stable identity returned for an already-registered workspace.
    WorkspaceIdentityV2Ok {
        protocol_version: u16,
        ws_id: String,
        registered_path: String,
        generation: WorkspaceGenerationTokenV2,
    },
    /// Durable evidence returned after a guarded checkpoint request.
    GuardedCheckpointV2Ok {
        evidence: GuardedCheckpointEvidenceV2,
    },
    /// Exact evidence lookup result; absence is represented by `None`.
    CheckpointEvidenceV2Ok {
        evidence: Option<GuardedCheckpointEvidenceV2>,
    },
    /// A guarded request rejected before checkpoint backend execution.
    GuardedCheckpointV2Rejected {
        code: GuardedCheckpointRejectionCodeV2,
        message: String,
    },
    /// Exact rollback preview produced for a guarded caller.
    GuardedRollbackPreviewV2Ok {
        protocol_version: u16,
        registered_path: String,
        ws_id: String,
        generation: WorkspaceGenerationTokenV2,
        target_snapshot_id: String,
        diff_digest: [u8; 32],
        changes: Vec<DiffEntry>,
        caller_uid: u32,
    },
    /// Durable proof that a guarded rollback completed.
    GuardedRollbackV2Ok {
        evidence: GuardedRollbackEvidenceV2,
    },
    /// Durable proof that a guarded rollback may have started.
    GuardedRollbackV2Uncertain {
        evidence: GuardedRollbackEvidenceV2,
    },
    /// Durable evidence lookup for one guarded rollback operation.
    GuardedRollbackEvidenceV2Ok {
        evidence: Option<GuardedRollbackEvidenceV2>,
    },
    /// A guarded rollback rejected before backend execution.
    GuardedRollbackV2Rejected {
        code: GuardedRollbackRejectionCodeV2,
        message: String,
    },
    /// Recovery succeeded while additional user data remains for inspection.
    RecoverWithWarning {
        /// Restored workspace path.
        workspace: String,
        /// Locations and reason for retaining additional data.
        warning: String,
    },
    /// Registration removed without restoring data or deleting snapshots.
    UnregisterOk {
        /// Original workspace path.
        workspace: String,
        /// Locations intentionally retained for manual recovery.
        retained_paths: Vec<String>,
    },
}

/// Error codes from ws-ckpt daemon.
/// CRITICAL: variant order must match ws-ckpt-common/src/lib.rs exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WsCkptErrorCode {
    WorkspaceNotFound,
    SnapshotNotFound,
    AlreadyInitialized,
    BtrfsError,
    IoError,
    InvalidPath,
    ConfirmationRequired,
    InternalError,
    SnapshotAlreadyExists,
    WriteLockConflict,
    DiskSpaceInsufficient,
    CwdOccupied,
    CwdScanFailed,
}

/// Opaque identity of one live writable-subvolume generation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceGenerationTokenV2([u8; 32]);

impl WorkspaceGenerationTokenV2 {
    /// Constructs a token from its exact fixed-width wire representation.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrows the exact fixed-width wire representation.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Consumes the token and returns its wire representation.
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl std::fmt::Debug for WorkspaceGenerationTokenV2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorkspaceGenerationTokenV2(<opaque>)")
    }
}

/// Terminal outcome durably bound to one guarded checkpoint operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuardedCheckpointOutcomeV2 {
    /// The backend created this snapshot.
    Created { snapshot_id: String },
    /// The daemon intentionally skipped backend creation.
    Skipped { reason: String },
}

/// Durable proof binding a caller operation to one workspace generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardedCheckpointEvidenceV2 {
    pub ws_id: String,
    pub registered_path: String,
    pub generation: WorkspaceGenerationTokenV2,
    pub checkpoint_id: String,
    pub operation_digest: [u8; 32],
    pub caller_uid: u32,
    pub outcome: GuardedCheckpointOutcomeV2,
}

/// Reasons a guarded request can be rejected before checkpoint backend execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuardedCheckpointRejectionCodeV2 {
    DaemonNotReady,
    PeerCredentialsUnavailable,
    InvalidRegistrationPath,
    InvalidWorkspaceId,
    InvalidCheckpointId,
    InvalidMetadata,
    WorkspaceNotFound,
    GenerationMismatch,
    OperationConflict,
    WriteLockConflict,
    CallerMismatch,
    EvidenceCapacityReached,
}

/// Outcome durably bound to one guarded rollback operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuardedRollbackOutcomeV2 {
    /// The operation intent is durable and backend execution may have started.
    Started,
    /// The backend and index update both completed durably.
    Succeeded {
        resulting_generation: WorkspaceGenerationTokenV2,
    },
    /// Backend execution may have taken effect, but completion is unproven.
    Unknown { reason: String },
}

/// Durable proof binding a caller operation to one exact guarded rollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardedRollbackEvidenceV2 {
    pub ws_id: String,
    pub registered_path: String,
    pub expected_generation: WorkspaceGenerationTokenV2,
    pub target_snapshot_id: String,
    pub expected_diff_digest: [u8; 32],
    pub operation_id: String,
    pub operation_digest: [u8; 32],
    pub caller_uid: u32,
    pub outcome: GuardedRollbackOutcomeV2,
}

/// Reasons a guarded rollback can be rejected before backend execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuardedRollbackRejectionCodeV2 {
    DaemonNotReady,
    PeerCredentialsUnavailable,
    InvalidRegistrationPath,
    InvalidWorkspaceId,
    InvalidSnapshotId,
    InvalidOperationId,
    WorkspaceNotFound,
    SnapshotNotFound,
    GenerationMismatch,
    DiffMismatch,
    OperationConflict,
    WriteLockConflict,
    CallerMismatch,
    EvidenceCapacityReached,
    CwdOccupied,
    CwdScanFailed,
}

/// Stable registered-workspace identity used to bind guarded requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceIdentityV2 {
    pub protocol_version: u16,
    pub ws_id: String,
    pub registered_path: String,
    pub generation: WorkspaceGenerationTokenV2,
}

// ===========================================================================
// Auxiliary types (match ws-ckpt-common)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub id: String,
    pub workspace: String,
    pub meta: SnapshotMeta,
}

mod metadata_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    pub fn serialize<S: Serializer>(v: &Option<Value>, s: S) -> Result<S::Ok, S::Error> {
        let normalized = v.as_ref().filter(|value| !value.is_null());
        if s.is_human_readable() {
            normalized.serialize(s)
        } else {
            normalized.map(|value| value.to_string()).serialize(s)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
        if d.is_human_readable() {
            Option::<Value>::deserialize(d)
        } else {
            Option::<String>::deserialize(d)?
                .map(|value| serde_json::from_str(&value).map_err(serde::de::Error::custom))
                .transpose()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub message: Option<String>,
    #[serde(default, with = "metadata_serde")]
    pub metadata: Option<serde_json::Value>,
    pub pinned: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub missing: bool,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub child_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffEntry {
    pub path: String,
    pub change_type: ChangeType,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    pub uptime_secs: u64,
    pub workspaces: Vec<WorkspaceInfo>,
    pub fs_total_bytes: u64,
    pub fs_used_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub ws_id: String,
    pub path: String,
    pub snapshot_count: u32,
}

/// Cleanup retention policy — mirrors ws-ckpt-common exactly.
#[derive(Debug, Clone, PartialEq)]
pub enum CleanupRetention {
    Count(u32),
    Age { raw: String, secs: u64 },
}

impl CleanupRetention {
    pub fn age(raw: impl Into<String>) -> Result<Self, String> {
        let raw = raw.into();
        let secs = parse_duration_secs(&raw)?;
        Ok(Self::Age { raw, secs })
    }
}

#[derive(Serialize, Deserialize)]
enum CleanupRetentionWire {
    Count(u32),
    Age(String),
}

impl Serialize for CleanupRetention {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            match self {
                Self::Count(count) => serializer.serialize_u32(*count),
                Self::Age { raw, .. } => serializer.serialize_str(raw),
            }
        } else {
            match self {
                Self::Count(count) => CleanupRetentionWire::Count(*count),
                Self::Age { raw, .. } => CleanupRetentionWire::Age(raw.clone()),
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for CleanupRetention {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            struct CleanupRetentionVisitor;

            impl<'de> serde::de::Visitor<'de> for CleanupRetentionVisitor {
                type Value = CleanupRetention;

                fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    formatter.write_str("a non-negative count or a duration string")
                }

                fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                    u32::try_from(value)
                        .map(CleanupRetention::Count)
                        .map_err(E::custom)
                }

                fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                    u32::try_from(value)
                        .map(CleanupRetention::Count)
                        .map_err(E::custom)
                }

                fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                    CleanupRetention::age(value).map_err(E::custom)
                }

                fn visit_string<E: serde::de::Error>(
                    self,
                    value: String,
                ) -> Result<Self::Value, E> {
                    CleanupRetention::age(value).map_err(E::custom)
                }
            }

            deserializer.deserialize_any(CleanupRetentionVisitor)
        } else {
            match CleanupRetentionWire::deserialize(deserializer)? {
                CleanupRetentionWire::Count(count) => Ok(Self::Count(count)),
                CleanupRetentionWire::Age(raw) => Self::age(raw).map_err(serde::de::Error::custom),
            }
        }
    }
}

fn parse_duration_secs(value: &str) -> Result<u64, String> {
    let value = value.trim();
    let mut chars = value.chars();
    let unit = chars.next_back().ok_or("empty duration")?;
    let count: u64 = chars
        .as_str()
        .parse()
        .map_err(|_| format!("invalid duration: {value}"))?;
    let seconds = match unit.to_ascii_lowercase() {
        's' => count,
        'm' => count.saturating_mul(60),
        'h' => count.saturating_mul(3_600),
        'd' => count.saturating_mul(86_400),
        'w' => count.saturating_mul(604_800),
        _ => return Err(format!("invalid duration unit: {unit}")),
    };
    if seconds > i64::MAX as u64 {
        return Err("duration is too large".to_string());
    }
    Ok(seconds)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigReport {
    pub mount_path: String,
    pub socket_path: String,
    pub log_level: String,
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: CleanupRetention,
    pub auto_cleanup_interval_secs: u64,
    pub health_check_interval_secs: u64,
    pub img_size: u64,
    pub img_max_percent: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePolicy {
    pub auto_cleanup: Option<bool>,
    pub auto_cleanup_keep: Option<CleanupRetention>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: CleanupRetention,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalPolicySnapshot {
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: CleanupRetention,
}

// ===========================================================================
// CLI output types (used for CoshResponse mapping)
// ===========================================================================

/// Result of creating a checkpoint (CLI display layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptCreated {
    /// Daemon-assigned identifier, present exactly when a snapshot was created.
    pub snapshot_id: Option<String>,
    /// Original request path, preserved even when the daemon skips creation.
    pub workspace: String,
    /// Indicates the daemon accepted the request but created no snapshot.
    #[serde(default)]
    pub skipped: bool,
    /// Daemon explanation, present exactly when creation was skipped.
    pub reason: Option<String>,
}

/// A single checkpoint entry (CLI display layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptEntry {
    pub id: String,
    pub workspace: String,
    pub message: Option<String>,
    pub pinned: bool,
    pub created_at: String,
}

/// Result of listing checkpoints (CLI display layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptListResult {
    pub snapshots: Vec<CkptEntry>,
    pub total: usize,
}

/// Result of restoring a checkpoint (CLI display layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptRestored {
    pub from: String,
    pub to: String,
}

/// Result of querying workspace checkpoint status (CLI display layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptStatusResult {
    pub uptime_secs: u64,
    pub workspaces: Vec<WorkspaceInfo>,
    pub fs_total_bytes: u64,
    pub fs_used_bytes: u64,
}

/// Result of deleting a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptDeleted {
    pub target: String,
}

/// Result of a diff operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptDiffResult {
    pub changes: Vec<DiffEntry>,
}

/// Result of previewing a rollback without changing the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptRollbackPreviewResult {
    /// Exact snapshot selected as the rollback target.
    pub to: String,
    /// Ordered file changes that rollback would apply.
    pub changes: Vec<DiffEntry>,
}

/// Exact guarded rollback preview bound to one workspace generation and caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptGuardedRollbackPreviewV2 {
    pub protocol_version: u16,
    pub registered_path: String,
    pub ws_id: String,
    pub generation: WorkspaceGenerationTokenV2,
    pub target_snapshot_id: String,
    pub diff_digest: [u8; 32],
    pub changes: Vec<DiffEntry>,
    pub caller_uid: u32,
}

/// Result of a cleanup operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptCleanupResult {
    pub removed: Vec<String>,
}

/// Result of init operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptInitResult {
    pub ws_id: String,
}

/// Result of recover operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CkptRecoverResult {
    pub workspace: String,
    /// Additional data retained for inspection after successful recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ckpt_created_deserializes_historical_success_json() {
        let created: CkptCreated = serde_json::from_value(serde_json::json!({
            "snapshot_id": "snap-1",
            "workspace": "/tmp/ws",
        }))
        .unwrap();

        assert_eq!(created.snapshot_id.as_deref(), Some("snap-1"));
        assert_eq!(created.workspace, "/tmp/ws");
        assert!(!created.skipped);
        assert!(created.reason.is_none());
    }

    #[test]
    fn test_request_bincode_roundtrip() {
        let requests = vec![
            WsCkptRequest::Init {
                workspace: "/home/user/project".into(),
            },
            WsCkptRequest::Checkpoint {
                workspace: "/home/user/project".into(),
                id: "snap-001".into(),
                message: Some("initial checkpoint".into()),
                metadata: Some(r#"{"key":"val"}"#.into()),
                pin: true,
            },
            WsCkptRequest::Rollback {
                workspace: "/tmp/ws".into(),
                to: Some("snap-001".into()),
                num_ancestors: None,
            },
            WsCkptRequest::Delete {
                workspace: Some("/tmp/ws".into()),
                snapshot: "snap-001".into(),
                force: false,
            },
            WsCkptRequest::List {
                workspace: Some("/tmp/ws".into()),
                format: None,
            },
            WsCkptRequest::Diff {
                workspace: "/tmp/ws".into(),
                from: "snap-001".into(),
                to: Some("snap-002".into()),
            },
            WsCkptRequest::Status { workspace: None },
            WsCkptRequest::Cleanup {
                workspace: "/tmp/ws".into(),
                keep: Some(5),
            },
            WsCkptRequest::Config,
            WsCkptRequest::ReloadConfig,
            WsCkptRequest::Recover {
                workspace: "/home/user/project".into(),
            },
            WsCkptRequest::HealthAdvisory,
        ];

        for req in &requests {
            let encoded = bincode::serialize(req).unwrap();
            let decoded: WsCkptRequest = bincode::deserialize(&encoded).unwrap();
            // Verify roundtrip by re-encoding
            let re_encoded = bincode::serialize(&decoded).unwrap();
            assert_eq!(encoded, re_encoded);
        }
    }

    #[test]
    fn test_response_bincode_roundtrip() {
        let responses = vec![
            WsCkptResponse::InitOk {
                ws_id: "ws-abc".into(),
            },
            WsCkptResponse::CheckpointOk {
                snapshot_id: "snap-001".into(),
            },
            WsCkptResponse::RollbackOk {
                from: "snap-003".into(),
                to: "snap-001".into(),
            },
            WsCkptResponse::DeleteOk {
                target: "snap-001".into(),
            },
            WsCkptResponse::Error {
                code: WsCkptErrorCode::WorkspaceNotFound,
                message: "not found".into(),
            },
            WsCkptResponse::ListOk { snapshots: vec![] },
            WsCkptResponse::DiffOk {
                changes: vec![DiffEntry {
                    path: "src/main.rs".into(),
                    change_type: ChangeType::Modified,
                    detail: None,
                }],
            },
            WsCkptResponse::StatusOk {
                report: StatusReport {
                    uptime_secs: 3600,
                    workspaces: vec![WorkspaceInfo {
                        ws_id: "ws-1".into(),
                        path: "/tmp".into(),
                        snapshot_count: 3,
                    }],
                    fs_total_bytes: 100_000_000,
                    fs_used_bytes: 50_000_000,
                },
            },
            WsCkptResponse::CleanupOk {
                removed: vec!["snap-old".into()],
            },
            WsCkptResponse::ConfigOk {
                config: ConfigReport {
                    mount_path: "/mnt/snapshots".into(),
                    socket_path: "/run/ws-ckpt/ws-ckpt.sock".into(),
                    log_level: "info".into(),
                    auto_cleanup: true,
                    auto_cleanup_keep: CleanupRetention::Count(5),
                    auto_cleanup_interval_secs: 3600,
                    health_check_interval_secs: 60,
                    img_size: 10_737_418_240,
                    img_max_percent: 80.0,
                },
            },
            WsCkptResponse::ReloadConfigOk {
                config: ConfigReport {
                    mount_path: "/mnt/snapshots".into(),
                    socket_path: "/run/ws-ckpt/ws-ckpt.sock".into(),
                    log_level: "info".into(),
                    auto_cleanup: true,
                    auto_cleanup_keep: CleanupRetention::Count(5),
                    auto_cleanup_interval_secs: 3600,
                    health_check_interval_secs: 60,
                    img_size: 10_737_418_240,
                    img_max_percent: 80.0,
                },
            },
            WsCkptResponse::CheckpointSkipped {
                reason: "no changes".into(),
            },
            WsCkptResponse::RecoverOk {
                workspace: "/tmp/ws".into(),
            },
            WsCkptResponse::HealthAdvisoryOk {
                over_limit_workspace_count: 2,
                fs_total_bytes: 1_000_000,
                fs_used_bytes: 800_000,
            },
        ];

        for resp in &responses {
            let encoded = bincode::serialize(resp).unwrap();
            let decoded: WsCkptResponse = bincode::deserialize(&encoded).unwrap();
            let re_encoded = bincode::serialize(&decoded).unwrap();
            assert_eq!(encoded, re_encoded);
        }
    }

    #[test]
    fn test_request_bincode_variant_index() {
        // Verify that each WsCkptRequest variant is serialized with the correct
        // bincode index — this is the wire contract with ws-ckpt daemon.
        let variants: Vec<(u32, WsCkptRequest)> = vec![
            (
                0,
                WsCkptRequest::Init {
                    workspace: "/ws".into(),
                },
            ),
            (
                1,
                WsCkptRequest::Checkpoint {
                    workspace: "/ws".into(),
                    id: "snap".into(),
                    message: None,
                    metadata: None,
                    pin: false,
                },
            ),
            (
                2,
                WsCkptRequest::Rollback {
                    workspace: "/ws".into(),
                    to: Some("snap".into()),
                    num_ancestors: None,
                },
            ),
            (
                3,
                WsCkptRequest::Delete {
                    workspace: None,
                    snapshot: "snap".into(),
                    force: false,
                },
            ),
            (
                4,
                WsCkptRequest::List {
                    workspace: None,
                    format: None,
                },
            ),
            (
                5,
                WsCkptRequest::Diff {
                    workspace: "/ws".into(),
                    from: "a".into(),
                    to: Some("b".into()),
                },
            ),
            (6, WsCkptRequest::Status { workspace: None }),
            (
                7,
                WsCkptRequest::Cleanup {
                    workspace: "/ws".into(),
                    keep: None,
                },
            ),
            (8, WsCkptRequest::Config),
            (9, WsCkptRequest::ReloadConfig),
            (10, WsCkptRequest::ReloadGlobalConfig),
            (
                11,
                WsCkptRequest::ReloadWorkspacePolicy {
                    workspace: "/ws".into(),
                },
            ),
            (12, WsCkptRequest::ConfigOverview),
            (
                13,
                WsCkptRequest::Recover {
                    workspace: "/ws".into(),
                },
            ),
            (14, WsCkptRequest::HealthAdvisory),
            (
                15,
                WsCkptRequest::GetWorkspacePolicy {
                    workspace: "/ws".into(),
                },
            ),
            (
                16,
                WsCkptRequest::ResetWorkspacePolicy {
                    workspace: "/ws".into(),
                },
            ),
            (
                17,
                WsCkptRequest::PatchWorkspacePolicy {
                    workspace: "/ws".into(),
                    auto_cleanup: PolicyFieldOp::Set(true),
                    auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(5)),
                },
            ),
            (
                18,
                WsCkptRequest::RollbackPreview {
                    workspace: "/ws".into(),
                    to: Some("snap".into()),
                    num_ancestors: None,
                },
            ),
            (
                19,
                WsCkptRequest::WorkspaceIdentityV2 {
                    registration_path: "/ws".into(),
                },
            ),
            (
                20,
                WsCkptRequest::GuardedCheckpointV2 {
                    ws_id: "ws-abc123".into(),
                    expected_generation: WorkspaceGenerationTokenV2::from_bytes([1; 32]),
                    checkpoint_id: "ckp_1".into(),
                    operation_digest: [2; 32],
                    message: None,
                    metadata: None,
                    pin: true,
                },
            ),
            (
                21,
                WsCkptRequest::CheckpointEvidenceV2 {
                    ws_id: "ws-abc123".into(),
                    expected_generation: WorkspaceGenerationTokenV2::from_bytes([1; 32]),
                    checkpoint_id: "ckp_1".into(),
                    operation_digest: [2; 32],
                },
            ),
            (
                22,
                WsCkptRequest::GuardedRollbackPreviewV2 {
                    registered_path: "/ws".into(),
                    ws_id: "ws-abc123".into(),
                    expected_generation: WorkspaceGenerationTokenV2::from_bytes([1; 32]),
                    target_snapshot_id: "ckp_1".into(),
                },
            ),
            (
                23,
                WsCkptRequest::GuardedRollbackV2 {
                    registered_path: "/ws".into(),
                    ws_id: "ws-abc123".into(),
                    expected_generation: WorkspaceGenerationTokenV2::from_bytes([1; 32]),
                    target_snapshot_id: "ckp_1".into(),
                    expected_diff_digest: [3; 32],
                    operation_id: "ckp_2".into(),
                    operation_digest: [4; 32],
                },
            ),
            (
                24,
                WsCkptRequest::GuardedRollbackEvidenceV2 {
                    ws_id: "ws-abc123".into(),
                    operation_id: "ckp_2".into(),
                    operation_digest: [4; 32],
                },
            ),
        ];

        for (expected_idx, req) in &variants {
            let encoded = bincode::serialize(req).unwrap();
            let variant_idx = u32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
            assert_eq!(
                variant_idx, *expected_idx,
                "WsCkptRequest variant index mismatch: expected {}, got {}",
                expected_idx, variant_idx
            );
        }
    }

    #[test]
    fn guarded_rollback_response_variants_keep_append_only_wire_indices() {
        let evidence = GuardedRollbackEvidenceV2 {
            ws_id: "ws-abc123".into(),
            registered_path: "/ws".into(),
            expected_generation: WorkspaceGenerationTokenV2::from_bytes([1; 32]),
            target_snapshot_id: "ckp_1".into(),
            expected_diff_digest: [2; 32],
            operation_id: "ckp_2".into(),
            operation_digest: [3; 32],
            caller_uid: 1000,
            outcome: GuardedRollbackOutcomeV2::Started,
        };
        let responses = [
            (
                21,
                WsCkptResponse::GuardedRollbackPreviewV2Ok {
                    protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
                    registered_path: "/ws".into(),
                    ws_id: "ws-abc123".into(),
                    generation: WorkspaceGenerationTokenV2::from_bytes([1; 32]),
                    target_snapshot_id: "ckp_1".into(),
                    diff_digest: [2; 32],
                    changes: Vec::new(),
                    caller_uid: 1000,
                },
            ),
            (
                22,
                WsCkptResponse::GuardedRollbackV2Ok {
                    evidence: evidence.clone(),
                },
            ),
            (
                23,
                WsCkptResponse::GuardedRollbackV2Uncertain {
                    evidence: evidence.clone(),
                },
            ),
            (
                24,
                WsCkptResponse::GuardedRollbackEvidenceV2Ok {
                    evidence: Some(evidence),
                },
            ),
            (
                25,
                WsCkptResponse::GuardedRollbackV2Rejected {
                    code: GuardedRollbackRejectionCodeV2::DiffMismatch,
                    message: "changed".into(),
                },
            ),
        ];

        for (expected_idx, response) in responses {
            let encoded = bincode::serialize(&response).unwrap();
            let actual = u32::from_le_bytes(encoded[..4].try_into().unwrap());
            assert_eq!(actual, expected_idx);
        }
    }

    #[test]
    fn test_error_code_bincode_index_order() {
        // Verify that bincode serializes enum variants by index (0, 1, 2, ...)
        let codes = vec![
            WsCkptErrorCode::WorkspaceNotFound,
            WsCkptErrorCode::SnapshotNotFound,
            WsCkptErrorCode::AlreadyInitialized,
            WsCkptErrorCode::BtrfsError,
            WsCkptErrorCode::IoError,
            WsCkptErrorCode::InvalidPath,
            WsCkptErrorCode::ConfirmationRequired,
            WsCkptErrorCode::InternalError,
            WsCkptErrorCode::SnapshotAlreadyExists,
            WsCkptErrorCode::WriteLockConflict,
            WsCkptErrorCode::DiskSpaceInsufficient,
            WsCkptErrorCode::CwdOccupied,
            WsCkptErrorCode::CwdScanFailed,
        ];

        for (idx, code) in codes.iter().enumerate() {
            let encoded = bincode::serialize(code).unwrap();
            // bincode 1.x encodes enums as u32 index
            let variant_idx = u32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
            assert_eq!(variant_idx, idx as u32, "ErrorCode variant index mismatch");
        }
    }

    #[test]
    fn test_default_socket_path() {
        assert_eq!(DEFAULT_SOCKET_PATH, "/run/ws-ckpt/ws-ckpt.sock");
    }

    #[test]
    fn test_cleanup_retention_bincode_roundtrip() {
        let count = CleanupRetention::Count(5);
        let bytes = bincode::serialize(&count).unwrap();
        let decoded: CleanupRetention = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded, CleanupRetention::Count(5));

        let age = CleanupRetention::Age {
            raw: "7d".into(),
            secs: 604800,
        };
        let bytes = bincode::serialize(&age).unwrap();
        let decoded: CleanupRetention = bincode::deserialize(&bytes).unwrap();
        assert_eq!(
            decoded,
            CleanupRetention::Age {
                raw: "7d".into(),
                secs: 604800
            }
        );
    }

    #[test]
    fn test_cleanup_retention_rejects_invalid_human_readable_values() {
        for input in [
            r#""""#,
            r#""d""#,
            r#""9223372036854775808s""#,
            r#""30天""#,
            r#""天""#,
        ] {
            assert!(
                serde_json::from_str::<CleanupRetention>(input).is_err(),
                "invalid retention value should fail: {input}"
            );
        }
    }

    #[test]
    fn test_config_report_bincode_roundtrip() {
        let report = ConfigReport {
            mount_path: "/mnt/ws-ckpt".into(),
            socket_path: "/run/ws-ckpt/ws-ckpt.sock".into(),
            log_level: "info".into(),
            auto_cleanup: true,
            auto_cleanup_keep: CleanupRetention::Count(10),
            auto_cleanup_interval_secs: 3600,
            health_check_interval_secs: 60,
            img_size: 536870912,
            img_max_percent: 80.0,
        };
        let bytes = bincode::serialize(&report).unwrap();
        let decoded: ConfigReport = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded.mount_path, "/mnt/ws-ckpt");
        assert_eq!(decoded.socket_path, "/run/ws-ckpt/ws-ckpt.sock");
        assert_eq!(decoded.log_level, "info");
        assert!(decoded.auto_cleanup);
        assert_eq!(decoded.auto_cleanup_keep, CleanupRetention::Count(10));
        assert_eq!(decoded.auto_cleanup_interval_secs, 3600);
        assert_eq!(decoded.health_check_interval_secs, 60);
        assert_eq!(decoded.img_size, 536870912);
        assert_eq!(decoded.img_max_percent, 80.0);
    }
}
