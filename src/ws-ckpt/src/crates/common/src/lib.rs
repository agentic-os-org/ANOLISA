use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;

pub mod backend;
pub mod migration;
pub mod persist;

use backend::BackendType;

// ── Constants ──

pub const DEFAULT_MOUNT_PATH: &str = "/mnt/btrfs-workspace";
pub const DEFAULT_SOCKET_PATH: &str = "/run/ws-ckpt/ws-ckpt.sock";
pub const SNAPSHOTS_DIR: &str = "snapshots";
pub const INDEX_FILE: &str = "index.json";
pub const BTRFS_IMG_PATH: &str = "/var/lib/ws-ckpt/btrfs-data.img";
pub const BTRFS_IMG_DIR: &str = "/var/lib/ws-ckpt";
/// Pre-FHS-migration location (kept for one-shot in-daemon migration on upgrade).
pub const LEGACY_BTRFS_IMG_PATH: &str = "/data/ws-ckpt/btrfs-data.img";
pub const CONFIG_FILE_PATH: &str = "/etc/ws-ckpt/config.toml";
pub const DEFAULT_IMG_SIZE_GB: u64 = 30;
pub const DEFAULT_IMG_MAX_PERCENT: f64 = 0.4; // 40% as fraction for calculation
pub const DEFAULT_STATE_DIR: &str = "/var/lib/ws-ckpt"; // systemd StateDirectory
pub const STATE_FILE: &str = "state.json"; // daemon state file
pub const INDEXES_DIR: &str = "indexes"; // snapshots indexes directory
pub const LOCKFILE_NAME: &str = "daemon.lock"; // daemon write lockfile
pub const POLICY_FILE: &str = "policy.toml";

/// Wire protocol version used by the guarded checkpoint API.
pub const GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2: u16 = 2;

/// Maximum UTF-8 byte length accepted for a guarded checkpoint identifier.
pub const GUARDED_CHECKPOINT_ID_MAX_BYTES_V2: usize = 128;

/// Maximum guarded-operation evidence records retained for one workspace.
pub const GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2: usize = 256;

/// Snapshot advisory threshold; strict-greater filter shared by daemon and CLI.
pub const ADVISORY_SNAPSHOT_LIMIT: u32 = 1000;

/// Sentinel in head snapshot's `child_ids` marking the writable subvolume.
pub const LIVE_CHILD: &str = "__live__";

// ── Error type ──

#[derive(Error, Debug)]
pub enum WsCkptError {
    #[error("bincode error: {0}")]
    Bincode(#[from] bincode::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: u64, max: u32 },
    #[error("config error: {0}")]
    Config(String),
}

// ── Request / Response ──

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Request {
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
    /// Query current daemon configuration
    Config,
    /// Reload configuration from file
    ReloadConfig,
    /// Reload only global config from file; skip the per-ws policy walk so a
    /// `config -g` view doesn't pay for a 500-ws policy.toml rescan.
    ReloadGlobalConfig,
    /// Reload a single workspace's `policy.toml`. Used by `config -w <ws>`
    /// view alignment so we don't rescan all 500 ws's just to view one.
    ReloadWorkspacePolicy {
        workspace: String,
    },
    /// `ws-ckpt config` (no scope): global cfg snapshot + ws override counts.
    ConfigOverview,
    /// Recover workspace to a normal directory (undo init)
    Recover {
        workspace: String,
    },
    /// Aggregated health metrics for post-command CLI advisories.
    HealthAdvisory,
    /// Read the per-workspace policy (absent ⇒ inherit-global).
    GetWorkspacePolicy {
        workspace: String,
    },
    /// Delete the per-workspace `policy.toml`, restoring inherit-global.
    /// The *only* whole-file operation; partial edits go through
    /// [`Request::PatchWorkspacePolicy`].
    ResetWorkspacePolicy {
        workspace: String,
    },
    /// Atomic field-level patch; daemon does read-modify-write under the
    /// per-ws write lock to avoid lost updates.
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
    /// Resolve the daemon's stable identity for a registered workspace path.
    WorkspaceIdentityV2 {
        registration_path: String,
    },
    /// Create a checkpoint guarded by workspace generation and operation identity.
    GuardedCheckpointV2 {
        ws_id: String,
        expected_generation: WorkspaceGenerationTokenV2,
        checkpoint_id: String,
        operation_digest: [u8; 32],
        message: Option<String>,
        metadata: Option<String>,
        pin: bool,
    },
    /// Query durable evidence for a previously guarded checkpoint operation.
    CheckpointEvidenceV2 {
        ws_id: String,
        expected_generation: WorkspaceGenerationTokenV2,
        checkpoint_id: String,
        operation_digest: [u8; 32],
    },
    /// Preview an exact rollback target and bind the result to workspace identity.
    GuardedRollbackPreviewV2 {
        registered_path: String,
        ws_id: String,
        expected_generation: WorkspaceGenerationTokenV2,
        target_snapshot_id: String,
    },
    /// Atomically revalidate a preview and roll back to its exact target.
    GuardedRollbackV2 {
        registered_path: String,
        ws_id: String,
        expected_generation: WorkspaceGenerationTokenV2,
        target_snapshot_id: String,
        expected_diff_digest: [u8; 32],
        operation_id: String,
        operation_digest: [u8; 32],
    },
    /// Query durable evidence for a guarded rollback operation.
    GuardedRollbackEvidenceV2 {
        ws_id: String,
        operation_id: String,
        operation_digest: [u8; 32],
    },
    /// Remove a registration only when its live subvolume is missing.
    Unregister {
        /// Registered workspace path or ID.
        workspace: String,
    },
    /// Resolve recovery's exact target and deletion scope before confirmation.
    RecoverPreview {
        workspace: String,
    },
    /// Recover only if the daemon's preview still describes the same target.
    RecoverConfirmed {
        preview: RecoveryPreview,
    },
    /// List snapshots recovered without their original metadata.
    ListOrphans {
        /// Omit to query all registered workspaces.
        workspace: Option<String>,
    },
    /// Read a page in ascending (created_at, workspace ID, snapshot ID) order.
    ListPage {
        /// Restrict the page to recovered orphan snapshots.
        orphans_only: bool,
        /// Workspace path or ID; absent means all registered workspaces.
        workspace: Option<String>,
        /// Maximum entries to return; must be greater than zero.
        limit: u32,
        /// Opaque continuation token returned by the previous page.
        cursor: Option<String>,
    },
}

/// Field-level patch op: `Unchanged` (default) / `Set(v)`.
///
/// No `Clear` variant — removing a per-ws override is whole-file, exposed
/// via [`Request::ResetWorkspacePolicy`]. Adding `Clear` later is a
/// non-breaking enum extension if a per-field clear UI ever lands.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub enum PolicyFieldOp<T> {
    #[default]
    Unchanged,
    Set(T),
}

impl<T> PolicyFieldOp<T> {
    /// Apply this op to a field's existing value. Returns the new value.
    pub fn apply(self, current: Option<T>) -> Option<T> {
        match self {
            Self::Unchanged => current,
            Self::Set(v) => Some(v),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Response {
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
        code: ErrorCode,
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
    /// Reply to any of the three reload IPCs. Includes the post-reload
    /// global config snapshot so callers that just wrote settings can
    /// confirm the landed state without a follow-up `Config` round-trip.
    ReloadConfigOk {
        config: ConfigReport,
        /// Global config the daemon now operates from, as parsed from
        /// [`CONFIG_FILE_PATH`] *in the daemon's own filesystem* — defaults
        /// when that file is absent. `None` when the reload did not consult
        /// the global file at all (per-workspace policy reload).
        ///
        /// `config` reports effective runtime values, which cannot expose a
        /// bootstrap-only setting the daemon has not applied yet; this field
        /// can. A client that just wrote the file compares it here to detect
        /// that the daemon reloaded a different file, or none — the case
        /// where CLI and daemon do not share `/etc/ws-ckpt`.
        file: Option<FileConfig>,
    },
    CheckpointSkipped {
        reason: String,
    },
    RecoverOk {
        workspace: String,
    },
    HealthAdvisoryOk {
        /// Count of workspaces exceeding `ADVISORY_SNAPSHOT_LIMIT`.
        over_limit_workspace_count: u32,
        /// Backend usage bytes; `fs_total_bytes == 0` sentinel means unavailable.
        fs_total_bytes: u64,
        fs_used_bytes: u64,
    },
    /// Reply to policy get/set/patch: effective (applied) + local (on-disk
    /// override) + global (daemon defaults snapshot).
    WorkspacePolicyOk {
        ws_id: String,
        effective: EffectivePolicy,
        local: WorkspacePolicy,
        global: GlobalPolicySnapshot,
    },
    /// Reply to `ConfigOverview`: full global cfg + ws roll-up.
    ConfigOverviewOk {
        config: ConfigReport,
        ws_total: usize,
        ws_with_override: usize,
    },
    RollbackPreviewOk {
        to: String,
        changes: Vec<DiffEntry>,
    },
    /// Stable identity returned for a registered workspace.
    WorkspaceIdentityV2Ok {
        /// Always [`GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2`].
        protocol_version: u16,
        ws_id: String,
        registered_path: String,
        generation: WorkspaceGenerationTokenV2,
    },
    /// Durable evidence that a guarded checkpoint request took effect or was skipped.
    GuardedCheckpointV2Ok {
        evidence: GuardedCheckpointEvidenceV2,
    },
    /// Durable evidence lookup; absence is represented by `None`, never a rejection.
    CheckpointEvidenceV2Ok {
        evidence: Option<GuardedCheckpointEvidenceV2>,
    },
    /// A request rejected before backend execution, with known no-checkpoint effect.
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
    /// Durable proof that a guarded rollback may have started but is not confirmed complete.
    GuardedRollbackV2Uncertain {
        evidence: GuardedRollbackEvidenceV2,
    },
    /// Durable evidence lookup for a guarded rollback operation.
    GuardedRollbackEvidenceV2Ok {
        evidence: Option<GuardedRollbackEvidenceV2>,
    },
    /// A guarded rollback rejected before backend execution, with known no-switch effect.
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
    /// Recovery identity and snapshot scope to display before confirmation.
    RecoverPreviewOk {
        preview: RecoveryPreview,
    },
    /// Byte-bounded snapshot page with an opaque continuation token.
    ListPageOk {
        snapshots: Vec<SnapshotListItem>,
        next_cursor: Option<String>,
    },
}

/// Daemon-resolved recovery target; execution revalidates it under lifecycle locks.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPreview {
    /// Registered workspace ID, or `None` for an interrupted, unregistered init.
    pub ws_id: Option<String>,
    /// User-visible restoration destination, never a managed storage alias.
    pub registration_path: String,
    /// Number of on-disk snapshot directories that recovery will delete.
    pub snapshot_count: u32,
    /// Fingerprint of the target and snapshot set; stale confirmation is refused.
    pub confirmation_digest: [u8; 32],
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
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

/// Opaque identity for one live writable-subvolume generation of a workspace.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkspaceGenerationTokenV2([u8; 32]);

impl WorkspaceGenerationTokenV2 {
    /// Constructs a generation token from its fixed-width wire representation.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrows the fixed-width wire representation.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Consumes the token and returns its fixed-width wire representation.
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl std::fmt::Debug for WorkspaceGenerationTokenV2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkspaceGenerationTokenV2(<opaque>)")
    }
}

/// Outcome durably bound to a guarded checkpoint operation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum GuardedCheckpointOutcomeV2 {
    /// The backend created this snapshot.
    Created { snapshot_id: String },
    /// The daemon intentionally skipped backend creation.
    Skipped { reason: String },
}

/// Durable proof binding a caller operation to its workspace and checkpoint outcome.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GuardedCheckpointEvidenceV2 {
    /// Exact daemon workspace identifier used for the operation.
    pub ws_id: String,
    /// Verbatim user-facing path stored in the daemon registration.
    pub registered_path: String,
    /// Live writable-subvolume generation checked before backend execution.
    pub generation: WorkspaceGenerationTokenV2,
    /// Snapshot identifier reserved while this evidence remains retained.
    pub checkpoint_id: String,
    /// Caller-defined digest binding the higher-level operation identity.
    pub operation_digest: [u8; 32],
    /// Effective UID obtained from the Unix peer credentials.
    pub caller_uid: u32,
    /// Durable checkpoint outcome.
    pub outcome: GuardedCheckpointOutcomeV2,
}

/// Reasons a guarded request can be rejected before backend execution.
///
/// Every variant guarantees that the daemon either did not invoke the backend or
/// otherwise knows that no checkpoint effect occurred.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum GuardedRollbackOutcomeV2 {
    /// The operation intent is durable and backend execution may have started.
    Started,
    /// The backend and index update both completed durably.
    Succeeded {
        /// Kernel-backed identity of the new writable workspace generation.
        resulting_generation: WorkspaceGenerationTokenV2,
    },
    /// Backend execution may have taken effect, but completion could not be proven.
    Unknown {
        /// Actionable diagnostic retained with the evidence.
        reason: String,
    },
}

/// Durable proof binding a caller operation to an exact guarded rollback.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GuardedRollbackEvidenceV2 {
    /// Exact daemon workspace identifier used for the operation.
    pub ws_id: String,
    /// Verbatim path stored in the daemon registration and supplied by the caller.
    pub registered_path: String,
    /// Live writable-subvolume generation that was previewed and revalidated.
    pub expected_generation: WorkspaceGenerationTokenV2,
    /// Full snapshot identifier; prefixes are never accepted by this API.
    pub target_snapshot_id: String,
    /// Digest of the live diff recomputed immediately before rollback.
    pub expected_diff_digest: [u8; 32],
    /// Caller-defined idempotency identifier.
    pub operation_id: String,
    /// Caller-defined digest binding the higher-level operation identity.
    pub operation_digest: [u8; 32],
    /// Effective UID obtained from Unix peer credentials.
    pub caller_uid: u32,
    /// Durable rollback outcome.
    pub outcome: GuardedRollbackOutcomeV2,
}

/// Reasons a guarded rollback can be rejected before backend execution.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
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

/// Validates the canonical `ws-xxxxxx` identifier and optional `-N` collision suffix.
pub fn validate_workspace_id_v2(ws_id: &str) -> Result<(), String> {
    let Some(rest) = ws_id.strip_prefix("ws-") else {
        return Err("workspace id must start with 'ws-'".to_string());
    };
    let (base, suffix) = match rest.split_once('-') {
        Some((base, suffix)) => (base, Some(suffix)),
        None => (rest, None),
    };
    if base.len() != 6
        || !base
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(
            "workspace id hash must be exactly six lowercase hexadecimal characters".to_string(),
        );
    }
    if let Some(suffix) = suffix {
        if suffix.is_empty()
            || !suffix.bytes().all(|byte| byte.is_ascii_digit())
            || suffix.starts_with('0')
            || suffix.parse::<u64>().map_or(true, |value| value < 2)
        {
            return Err(
                "workspace id collision suffix must be a canonical integer of at least 2"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Validates a bounded, non-reserved checkpoint identifier safe as one path component.
pub fn validate_checkpoint_id_v2(checkpoint_id: &str) -> Result<(), String> {
    if checkpoint_id.is_empty() {
        return Err("checkpoint id must not be empty".to_string());
    }
    if checkpoint_id == LIVE_CHILD {
        return Err("checkpoint id is reserved for the live workspace marker".to_string());
    }
    if checkpoint_id.len() > GUARDED_CHECKPOINT_ID_MAX_BYTES_V2 {
        return Err(format!(
            "checkpoint id exceeds {} bytes",
            GUARDED_CHECKPOINT_ID_MAX_BYTES_V2
        ));
    }
    if checkpoint_id == "." || checkpoint_id == ".." {
        return Err("checkpoint id must not be '.' or '..'".to_string());
    }
    if !checkpoint_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(
            "checkpoint id may contain only ASCII letters, digits, '-', '_', and '.'".to_string(),
        );
    }
    Ok(())
}

// ── Snapshot types ──

/// Serde for `SnapshotMeta.metadata`: passes `Value` through for JSON,
/// encodes as `String` for bincode (which can't deserialize untagged enums).
mod metadata_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    pub fn serialize<S: Serializer>(v: &Option<Value>, s: S) -> Result<S::Ok, S::Error> {
        // Collapse `Some(Value::Null)` to `None` to match JSON round-trip.
        let normalized = v.as_ref().filter(|val| !val.is_null());
        if s.is_human_readable() {
            normalized.serialize(s)
        } else {
            normalized.map(|val| val.to_string()).serialize(s)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
        // No need to collapse `Some(Value::Null)` here: `serialize` already
        // normalizes it on the way out, so the wire never carries a null value.
        if d.is_human_readable() {
            Option::<Value>::deserialize(d)
        } else {
            Option::<String>::deserialize(d)?
                .map(|s| serde_json::from_str(&s).map_err(serde::de::Error::custom))
                .transpose()
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SnapshotMeta {
    pub message: Option<String>,
    #[serde(default, with = "metadata_serde")]
    pub metadata: Option<serde_json::Value>,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
    /// Is the subvolume missing in the filesystem (detected in reconcile)
    #[serde(default)]
    pub missing: bool,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub child_ids: Vec<String>,
}

/// A snapshot entry combining its ID with metadata.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SnapshotEntry {
    pub id: String,
    pub workspace: String,
    pub meta: SnapshotMeta,
}

/// One result in a paged snapshot listing.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SnapshotListItem {
    /// Complete snapshot metadata.
    Full(SnapshotEntry),
    /// Identity-only fallback when complete metadata cannot fit in one frame.
    Summary(SnapshotSummary),
}

/// Identity fields preserved when a snapshot's optional detail is too large.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SnapshotSummary {
    pub id: String,
    pub workspace: String,
    pub created_at: DateTime<Utc>,
    pub pinned: bool,
    pub missing: bool,
    pub omitted_fields: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SnapshotIndex {
    pub workspace_path: PathBuf,
    pub snapshots: HashMap<String, SnapshotMeta>,
    #[serde(default)]
    pub head: Option<String>,
    /// Durable guarded-operation evidence keyed by checkpoint identifier.
    #[serde(default)]
    pub governed_evidence: HashMap<String, GuardedCheckpointEvidenceV2>,
    /// Durable guarded rollback receipts keyed by caller idempotency identifier.
    #[serde(default)]
    pub guarded_rollbacks: HashMap<String, GuardedRollbackEvidenceV2>,
    /// Snapshot IDs adopted from disk without their original metadata.
    #[serde(default)]
    pub recovered_orphans: HashSet<String>,
}

impl SnapshotIndex {
    pub fn new(workspace_path: PathBuf) -> Self {
        Self {
            workspace_path,
            snapshots: HashMap::new(),
            head: None,
            governed_evidence: HashMap::new(),
            guarded_rollbacks: HashMap::new(),
            recovered_orphans: HashSet::new(),
        }
    }
}

impl SnapshotIndex {
    /// Remove a single node from the DAG, reparenting its children to its parent.
    pub fn unlink_node(&mut self, id: &str) {
        let parent = self.snapshots.get(id).and_then(|m| m.parent_id.clone());
        let children: Vec<String> = self
            .snapshots
            .get(id)
            .map(|m| m.child_ids.clone())
            .unwrap_or_default();

        for cid in &children {
            if cid == LIVE_CHILD {
                continue;
            }
            if let Some(cm) = self.snapshots.get_mut(cid) {
                cm.parent_id = parent.clone();
            }
        }
        if let Some(ref pid) = parent {
            if let Some(pm) = self.snapshots.get_mut(pid) {
                pm.child_ids.retain(|c| c != id);
                for cid in &children {
                    if cid != LIVE_CHILD && !pm.child_ids.contains(cid) {
                        pm.child_ids.push(cid.clone());
                    }
                }
            }
        }
        if self.head.as_deref() == Some(id) {
            self.head = parent.clone();
            if let Some(ref nh) = self.head {
                if let Some(m) = self.snapshots.get_mut(nh) {
                    if !m.child_ids.contains(&LIVE_CHILD.to_string()) {
                        m.child_ids.push(LIVE_CHILD.to_string());
                    }
                }
            }
        }
    }

    /// Remove a batch of nodes from the DAG, processing only boundary edges.
    pub fn prune_chain(&mut self, ids: &HashSet<String>) {
        let mut edges: Vec<(String, String)> = Vec::new();
        let mut surviving_parents: Vec<(String, String)> = Vec::new();
        for id in ids {
            if let Some(meta) = self.snapshots.get(id) {
                for cid in &meta.child_ids {
                    if cid != LIVE_CHILD && !ids.contains(cid) {
                        edges.push((cid.clone(), id.clone()));
                    }
                }
                if let Some(ref pid) = meta.parent_id {
                    if !ids.contains(pid) {
                        surviving_parents.push((pid.clone(), id.clone()));
                    }
                }
            }
        }

        for (parent_id, deleted_id) in &surviving_parents {
            if let Some(pm) = self.snapshots.get_mut(parent_id) {
                pm.child_ids.retain(|c| c != deleted_id);
            }
        }

        for (child_id, deleted_parent) in &edges {
            let mut sp = self
                .snapshots
                .get(deleted_parent)
                .and_then(|m| m.parent_id.clone());
            while let Some(ref pid) = sp {
                if !ids.contains(pid) {
                    break;
                }
                sp = self.snapshots.get(pid).and_then(|m| m.parent_id.clone());
            }
            if let Some(cm) = self.snapshots.get_mut(child_id) {
                cm.parent_id = sp.clone();
            }
            if let Some(ref spid) = sp {
                if let Some(pm) = self.snapshots.get_mut(spid) {
                    if !pm.child_ids.contains(child_id) {
                        pm.child_ids.push(child_id.clone());
                    }
                }
            }
        }

        if let Some(ref h) = self.head.clone() {
            if ids.contains(h) {
                let mut p = self.snapshots.get(h).and_then(|m| m.parent_id.clone());
                while let Some(ref pid) = p {
                    if !ids.contains(pid) {
                        break;
                    }
                    p = self.snapshots.get(pid).and_then(|m| m.parent_id.clone());
                }
                self.head = p;
                if let Some(ref nh) = self.head {
                    if let Some(m) = self.snapshots.get_mut(nh) {
                        if !m.child_ids.contains(&LIVE_CHILD.to_string()) {
                            m.child_ids.push(LIVE_CHILD.to_string());
                        }
                    }
                }
            }
        }
    }
}

/// Error type for ancestor traversal.
#[derive(Debug, PartialEq, Eq)]
pub enum AncestorError {
    InvalidAncestors,
    NoHead,
    BrokenChain { depth: usize },
    NotFound { at_id: String },
}

impl std::fmt::Display for AncestorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAncestors => write!(f, "num-ancestors must be >= 1"),
            Self::NoHead => write!(
                f,
                "no head set — no lineage available, use --snapshot instead"
            ),
            Self::BrokenChain { depth } => {
                write!(
                    f,
                    "lineage chain broken at depth {depth} (parent_id is None)"
                )
            }
            Self::NotFound { at_id } => {
                write!(f, "parent snapshot '{at_id}' not found in index")
            }
        }
    }
}

impl SnapshotIndex {
    /// Traverse the ancestor chain: n=1 returns head itself, n=2 returns head's parent, etc.
    pub fn ancestor(&self, n: usize) -> Result<(&String, &SnapshotMeta), AncestorError> {
        if n < 1 {
            return Err(AncestorError::InvalidAncestors);
        }
        let head_id = self.head.as_ref().ok_or(AncestorError::NoHead)?;
        let mut current_id = head_id;
        for depth in 1..n {
            let meta = self
                .snapshots
                .get(current_id)
                .ok_or_else(|| AncestorError::NotFound {
                    at_id: current_id.clone(),
                })?;
            current_id = meta
                .parent_id
                .as_ref()
                .ok_or(AncestorError::BrokenChain { depth })?;
        }
        let meta = self
            .snapshots
            .get(current_id)
            .ok_or_else(|| AncestorError::NotFound {
                at_id: current_id.clone(),
            })?;
        Ok((current_id, meta))
    }
}

/// Error type for snapshot prefix resolution.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    NotFound,
    Ambiguous(usize),
}

impl SnapshotIndex {
    /// Resolve a snapshot by exact ID or unique prefix.
    pub fn resolve_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<(&String, &SnapshotMeta), ResolveError> {
        // Exact match first
        if let Some((id, meta)) = self.snapshots.get_key_value(prefix) {
            return Ok((id, meta));
        }
        // Prefix match
        let matches: Vec<_> = self
            .snapshots
            .iter()
            .filter(|(id, _)| id.starts_with(prefix))
            .collect();
        match matches.len() {
            0 => Err(ResolveError::NotFound),
            1 => Ok((matches[0].0, matches[0].1)),
            n => Err(ResolveError::Ambiguous(n)),
        }
    }
}

// ── Phase 2 data types ──

/// Type of change detected in a diff between two snapshots.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// A single file change entry in a diff result.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DiffEntry {
    pub path: String,
    pub change_type: ChangeType,
    pub detail: Option<String>,
}

/// Summary information about a single workspace.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WorkspaceInfo {
    pub ws_id: String,
    pub path: String,
    pub snapshot_count: u32,
}

/// Status report for the daemon and its managed workspaces.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StatusReport {
    pub uptime_secs: u64,
    pub workspaces: Vec<WorkspaceInfo>,
    pub fs_total_bytes: u64,
    pub fs_used_bytes: u64,
}

/// Auto-cleanup retention policy (mutually-exclusive modes):
/// - `Count(N)` ← TOML integer: keep N most-recent non-pinned snapshots.
/// - `Age { raw, secs }` ← TOML string (`"30d"`, units `s/m/h/d/w`): purge non-pinned
///   snapshots older than `secs`. `raw` is the user's original string (round-trip +
///   display); `secs` is pre-parsed once at deserialize time. Strict — no count floor.
///
/// Invariant (Age): `parse_duration_secs(&raw) == Ok(secs)`, enforced by the only
/// public constructor [`CleanupRetention::age`] and by Deserialize.
///
/// Serde: bincode lacks `deserialize_any`, so we dispatch on `is_human_readable()` —
/// TOML/JSON use raw u32/String + Visitor; bincode uses a tagged wire enum carrying
/// only `raw` (secs re-derived on receive).
#[derive(Debug, Clone, PartialEq)]
pub enum CleanupRetention {
    /// Count mode: keep N most recent non-pinned snapshots (0 = disabled).
    Count(u32),
    /// Age mode: keep snapshots within `secs` seconds. `raw` is the user's original
    /// string (e.g. "30d", "2w") preserved for round-trip and display.
    Age { raw: String, secs: u64 },
}

impl CleanupRetention {
    /// Construct an [`Age`](Self::Age) variant from a duration string, parsing and
    /// caching the seconds value. Returns an error if `raw` is not a valid duration.
    pub fn age(raw: impl Into<String>) -> Result<Self, String> {
        let raw = raw.into();
        let secs = parse_duration_secs(&raw)?;
        Ok(Self::Age { raw, secs })
    }

    /// Whether this retention policy disables auto-cleanup entirely:
    /// `Count(0)` or `Age { secs: 0, .. }`.
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Count(0) | Self::Age { secs: 0, .. })
    }
}

/// Tagged wire representation used for binary (bincode) encoding only.
/// Only `raw` is transported; `secs` is re-derived from `raw` on the receiving side.
#[derive(Serialize, Deserialize)]
enum CleanupRetentionWire {
    Count(u32),
    Age(String),
}

impl Serialize for CleanupRetention {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        if ser.is_human_readable() {
            match self {
                Self::Count(n) => ser.serialize_u32(*n),
                Self::Age { raw, .. } => ser.serialize_str(raw),
            }
        } else {
            let wire = match self {
                Self::Count(n) => CleanupRetentionWire::Count(*n),
                Self::Age { raw, .. } => CleanupRetentionWire::Age(raw.clone()),
            };
            wire.serialize(ser)
        }
    }
}

impl<'de> Deserialize<'de> for CleanupRetention {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        if de.is_human_readable() {
            struct V;
            impl<'de> serde::de::Visitor<'de> for V {
                type Value = CleanupRetention;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    write!(
                        f,
                        "a non-negative integer (count mode) or a duration string like \"30d\" (age mode)"
                    )
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                    if v > u32::MAX as u64 {
                        return Err(E::custom(format!(
                            "auto_cleanup_keep count {} exceeds u32::MAX",
                            v
                        )));
                    }
                    Ok(CleanupRetention::Count(v as u32))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                    if !(0..=u32::MAX as i64).contains(&v) {
                        return Err(E::custom(format!(
                            "auto_cleanup_keep count {} out of u32 range",
                            v
                        )));
                    }
                    Ok(CleanupRetention::Count(v as u32))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                    // Pre-validate + cache the seconds value so the value is rejected
                    // at config load / reload time and the runtime path avoids re-parsing.
                    CleanupRetention::age(v)
                        .map_err(|e| E::custom(format!("auto_cleanup_keep age mode: {}", e)))
                }
                fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                    CleanupRetention::age(v)
                        .map_err(|e| E::custom(format!("auto_cleanup_keep age mode: {}", e)))
                }
            }
            de.deserialize_any(V)
        } else {
            let wire = CleanupRetentionWire::deserialize(de)?;
            match wire {
                CleanupRetentionWire::Count(n) => Ok(CleanupRetention::Count(n)),
                CleanupRetentionWire::Age(raw) => CleanupRetention::age(raw).map_err(|e| {
                    serde::de::Error::custom(format!("auto_cleanup_keep age mode: {}", e))
                }),
            }
        }
    }
}

impl std::fmt::Display for CleanupRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Count(n) => write!(f, "{}", n),
            Self::Age { raw, .. } => write!(f, "\"{}\"", raw),
        }
    }
}

/// Parse a duration string like `30d`, `2w`, `3600s`, `5m`, `12h` into seconds.
/// Bare numbers without a unit are rejected to force explicit semantics.
pub fn parse_duration_secs(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".to_string());
    }
    let bytes = s.as_bytes();
    let last = bytes[bytes.len() - 1];
    if !last.is_ascii_alphabetic() {
        return Err(format!("duration '{}' missing unit suffix (s/m/h/d/w)", s));
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let n: u64 = num_str
        .parse()
        .map_err(|_| format!("duration '{}': invalid number '{}'", s, num_str))?;
    let secs = match unit.to_ascii_lowercase().as_str() {
        "s" => n,
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" => n.saturating_mul(86400),
        "w" => n.saturating_mul(604800),
        u => {
            return Err(format!(
                "duration '{}': invalid unit '{}' (expected s/m/h/d/w)",
                s, u
            ))
        }
    };
    // Reject values that would overflow i64 when downstream uses chrono::Duration::seconds.
    if secs > i64::MAX as u64 {
        return Err(format!(
            "duration '{}' too large (max supported is {} seconds \u{2248} {} years)",
            s,
            i64::MAX,
            i64::MAX / (365 * 86400),
        ));
    }
    Ok(secs)
}

/// Report of the current daemon configuration (returned by `Config` request).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
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

// ── Daemon config ──

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub mount_path: PathBuf,
    pub socket_path: PathBuf,
    pub log_level: String,
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: CleanupRetention,
    /// Interval in seconds between auto-cleanup runs
    pub auto_cleanup_interval_secs: u64,
    /// Interval in seconds between health checks
    pub health_check_interval_secs: u64,
    /// Backend type string from config: "auto" | "btrfs-base" | "btrfs-loop"
    pub backend_type: String,
    /// Target image size in GB. The on-disk image is grown/shrunk to match this at bootstrap.
    pub img_size: u64,
    /// Initial-creation cap as percentage of host partition capacity (0-100).
    /// Only consulted on the very first bootstrap when the image does not yet exist.
    pub img_max_percent: f64,
    /// Minimum free space in bytes (used by health-check reporting, does NOT block checkpoint)
    pub min_free_bytes: u64,
    /// Minimum free space percentage 0-100 (used by health-check reporting, does NOT block checkpoint)
    pub min_free_percent: f64,
}

impl DaemonConfig {
    /// Parse backend_type string into BackendType enum.
    /// Returns None for "auto" (caller should run auto-detect).
    pub fn parse_backend_type(&self) -> Option<BackendType> {
        match self.backend_type.as_str() {
            "btrfs-loop" => Some(BackendType::BtrfsLoop),
            "btrfs-base" => Some(BackendType::BtrfsBase),
            _ => None, // "auto" or unknown → auto-detect
        }
    }
}

pub const DEFAULT_AUTO_CLEANUP: bool = false;
/// Default count for `CleanupRetention::Count` when no retention is configured.
pub const DEFAULT_AUTO_CLEANUP_KEEP_COUNT: u32 = 20;
/// Factory for the default `CleanupRetention` (Count mode, 20 snapshots).
pub fn default_auto_cleanup_keep() -> CleanupRetention {
    CleanupRetention::Count(DEFAULT_AUTO_CLEANUP_KEEP_COUNT)
}
/// Default interval between auto-cleanup runs: 24 hours (86_400 seconds).
pub const DEFAULT_AUTO_CLEANUP_INTERVAL_SECS: u64 = 86_400;
pub const DEFAULT_HEALTH_CHECK_INTERVAL_SECS: u64 = 300;

// ── Config file ──

/// BtrfsLoop backend-specific configuration.
///
/// NOTE: These fields only take effect during daemon bootstrap.
/// Changing them via `ReloadConfig` will emit a warning and require a daemon restart.
/// The img file path is fixed to `BTRFS_IMG_PATH` and is not user-configurable.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BtrfsLoopConfig {
    /// Target image size in GB. Used as the reconcile target at bootstrap.
    pub img_size: Option<u64>,
    /// Initial-creation cap as percentage of host partition capacity (0-100).
    /// Only consulted on the very first bootstrap when the image does not yet exist.
    pub img_max_percent: Option<f64>,
}

/// Backend configuration section in config file.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BackendConfig {
    /// "auto" | "btrfs-base" | "btrfs-loop"
    #[serde(default = "default_backend_type")]
    pub r#type: String,
    /// BtrfsLoop backend-specific settings
    #[serde(default, rename = "btrfs-loop")]
    pub btrfs_loop: Option<BtrfsLoopConfig>,
}

fn default_backend_type() -> String {
    "auto".to_string()
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            r#type: default_backend_type(),
            btrfs_loop: None,
        }
    }
}

/// On-disk config file structure (all fields optional; missing = use defaults).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct FileConfig {
    pub auto_cleanup: Option<bool>,
    pub auto_cleanup_keep: Option<CleanupRetention>,
    pub auto_cleanup_interval_secs: Option<u64>,
    pub health_check_interval_secs: Option<u64>,
    /// Backend configuration section (optional; defaults to auto-detect)
    #[serde(default)]
    pub backend: BackendConfig,
}

/// Load config from a TOML file. Returns `FileConfig::default()` when the file
/// does not exist.
pub fn load_config_file(path: &Path) -> Result<FileConfig, WsCkptError> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let fc: FileConfig = toml::from_str(&content)
                .map_err(|e| WsCkptError::Config(format!("parse {}: {}", path.display(), e)))?;
            validate_file_config(&fc, path)?;
            Ok(fc)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileConfig::default()),
        Err(e) => Err(WsCkptError::Io(e)),
    }
}

/// Validate numeric ranges in a loaded `FileConfig` so downstream consumers
/// (e.g. bootstrap's `f64 -> u64` cast on `avail * img_max_percent / 100.0`)
/// never see NaN/Infinity/out-of-range values.
fn validate_file_config(fc: &FileConfig, path: &Path) -> Result<(), WsCkptError> {
    if let Some(loop_cfg) = &fc.backend.btrfs_loop {
        if let Some(pct) = loop_cfg.img_max_percent {
            if !pct.is_finite() || !(0.0..=100.0).contains(&pct) {
                return Err(WsCkptError::Config(format!(
                    "backend.btrfs-loop.img_max_percent in {}: expected a finite value in 0.0..=100.0 (got {})",
                    path.display(),
                    pct
                )));
            }
        }
    }
    Ok(())
}

/// Save config to a TOML file, creating parent directories as needed.
pub fn save_config_file(path: &Path, config: &FileConfig) -> Result<(), WsCkptError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)
        .map_err(|e| WsCkptError::Config(format!("serialize config: {}", e)))?;
    std::fs::write(path, content)?;
    Ok(())
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            mount_path: PathBuf::from(DEFAULT_MOUNT_PATH),
            socket_path: PathBuf::from(DEFAULT_SOCKET_PATH),
            log_level: "info".to_string(),
            auto_cleanup: DEFAULT_AUTO_CLEANUP,
            auto_cleanup_keep: default_auto_cleanup_keep(),
            auto_cleanup_interval_secs: DEFAULT_AUTO_CLEANUP_INTERVAL_SECS,
            health_check_interval_secs: DEFAULT_HEALTH_CHECK_INTERVAL_SECS,
            backend_type: "auto".to_string(),
            img_size: DEFAULT_IMG_SIZE_GB,
            img_max_percent: DEFAULT_IMG_MAX_PERCENT * 100.0, // stored as 0-100
            min_free_bytes: 512 * 1024 * 1024,                // 512 MB
            min_free_percent: 1.0,
        }
    }
}

// ── Frame encoding/decoding (sync, no tokio dependency) ──

/// Max IPC frame payload (excludes 4-byte length header). Enforced on both
/// sides to prevent OOM from a malformed length prefix.
pub const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024; // 16 MiB

/// Maximum number of records accepted in one list-page request.
pub const MAX_LIST_PAGE_ITEMS: u32 = 10_000;

/// Return the encoded payload size without allocating the payload.
pub fn encoded_size<T: Serialize>(msg: &T) -> Result<u64, WsCkptError> {
    Ok(bincode::serialized_size(msg)?)
}

/// Serialize a message into a length-prefixed frame: [4-byte LE length][bincode payload]
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, WsCkptError> {
    let len = encoded_size(msg)?;
    if len > u64::from(MAX_FRAME_SIZE) {
        return Err(WsCkptError::FrameTooLarge {
            size: len,
            max: MAX_FRAME_SIZE,
        });
    }
    let payload = bincode::serialize(msg)?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(len as u32).to_le_bytes());
    frame.extend(payload);
    Ok(frame)
}

/// Deserialize a bincode payload (caller is responsible for reading the length prefix
/// and then reading exactly N bytes before calling this function)
pub fn decode_payload<T: DeserializeOwned>(data: &[u8]) -> Result<T, WsCkptError> {
    Ok(bincode::deserialize(data)?)
}

// ── Per-workspace policy ──

/// On-disk per-workspace policy override (`indexes/<ws_id>/policy.toml`).
/// `Some(_)` overrides the global field; `None`/missing file ⇒ inherit global.
/// Daemon-wide fields (intervals, health, image sizing) are excluded and
/// rejected at the CLI/IPC boundary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePolicy {
    pub auto_cleanup: Option<bool>,
    pub auto_cleanup_keep: Option<CleanupRetention>,
}

impl WorkspacePolicy {
    /// True when no field is set — the workspace inherits everything from global.
    pub fn is_empty(&self) -> bool {
        self.auto_cleanup.is_none() && self.auto_cleanup_keep.is_none()
    }

    /// Overlay this local policy on global config, per-field `local.or(global)`.
    pub fn effective_for(&self, global: &DaemonConfig) -> EffectivePolicy {
        EffectivePolicy {
            auto_cleanup: self.auto_cleanup.unwrap_or(global.auto_cleanup),
            auto_cleanup_keep: self
                .auto_cleanup_keep
                .clone()
                .unwrap_or_else(|| global.auto_cleanup_keep.clone()),
        }
    }
}

/// Merged (local+global) auto-cleanup behavior applied to one workspace.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EffectivePolicy {
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: CleanupRetention,
}

impl EffectivePolicy {
    /// Whether scheduler should skip this workspace this tick.
    pub fn is_disabled(&self) -> bool {
        !self.auto_cleanup || self.auto_cleanup_keep.is_disabled()
    }
}

/// Snapshot of global policy fields at query time, for the CLI 3-column view.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GlobalPolicySnapshot {
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: CleanupRetention,
}

impl GlobalPolicySnapshot {
    pub fn from_config(cfg: &DaemonConfig) -> Self {
        Self {
            auto_cleanup: cfg.auto_cleanup,
            auto_cleanup_keep: cfg.auto_cleanup_keep.clone(),
        }
    }
}

// ── CLI `--format json` output shapes ──────────────────────────────────
//
// Stable, versioned JSON schemas for consumers (openclaw, hermes, CI).
// Kept in `common` (next to their source types, like `SnapshotEntry`) so
// the cli crate needs no own serde dep and Rust callers can reuse them.
//
// `is_disabled` is pre-computed on the wire so consumers don't re-derive
// daemon semantics — the only way `Count(0)` / `Count(N)` stay
// distinguishable to a plugin (the original openclaw bug).
//
// `RetentionJson` is a tagged enum (`{mode: "count"|"age", ...}`) rather
// than `CleanupRetention`'s bare-number/string serde, decoupling the
// plugin contract and sparing consumers a `typeof` check.

/// JSON shape for `ws-ckpt config -w ... --format json` and the
/// Patch/Reset responses under `--format json`. Versioned via `schema`.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct WorkspacePolicyJson {
    pub schema: &'static str,
    pub ws_id: String,
    pub effective: EffectivePolicyJson,
    pub local: LocalPolicyJson,
    pub global: GlobalPolicyValuesJson,
}

/// Versioned schema tag for [`WorkspacePolicyJson`]. Bump on any breaking
/// change so consumers can refuse unknown majors instead of silently
/// misreading.
pub const WORKSPACE_POLICY_JSON_SCHEMA: &str = "ws-ckpt-policy/v1";

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct EffectivePolicyJson {
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: RetentionJson,
    /// Pre-computed `!auto_cleanup || keep.is_disabled()`. Plugins MUST
    /// read this instead of re-deriving it consumer-side.
    pub is_disabled: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct LocalPolicyJson {
    pub auto_cleanup: Option<bool>,
    pub auto_cleanup_keep: Option<RetentionJson>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct GlobalPolicyValuesJson {
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: RetentionJson,
}

/// Tagged retention so consumers can match on a literal `mode` field
/// instead of discriminating number-vs-string. Mirrors
/// `CleanupRetention`'s two semantic modes 1:1.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum RetentionJson {
    Count { count: u32 },
    Age { raw: String, secs: u64 },
}

impl From<&CleanupRetention> for RetentionJson {
    fn from(r: &CleanupRetention) -> Self {
        match r {
            CleanupRetention::Count(n) => Self::Count { count: *n },
            CleanupRetention::Age { raw, secs } => Self::Age {
                raw: raw.clone(),
                secs: *secs,
            },
        }
    }
}

impl WorkspacePolicyJson {
    /// Convenience: build the JSON view from the three values returned in
    /// `Response::WorkspacePolicyOk`, pre-computing `is_disabled` for the
    /// effective layer so consumers don't have to.
    pub fn from_views(
        ws_id: String,
        effective: &EffectivePolicy,
        local: &WorkspacePolicy,
        global: &GlobalPolicySnapshot,
    ) -> Self {
        Self {
            schema: WORKSPACE_POLICY_JSON_SCHEMA,
            ws_id,
            effective: EffectivePolicyJson {
                auto_cleanup: effective.auto_cleanup,
                auto_cleanup_keep: (&effective.auto_cleanup_keep).into(),
                is_disabled: effective.is_disabled(),
            },
            local: LocalPolicyJson {
                auto_cleanup: local.auto_cleanup,
                auto_cleanup_keep: local.auto_cleanup_keep.as_ref().map(Into::into),
            },
            global: GlobalPolicyValuesJson {
                auto_cleanup: global.auto_cleanup,
                auto_cleanup_keep: (&global.auto_cleanup_keep).into(),
            },
        }
    }
}

/// JSON shape for `ws-ckpt config -g --format json`.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct GlobalConfigJson {
    pub schema: &'static str,
    pub config_file: String,
    pub mount_path: String,
    pub socket_path: String,
    pub auto_cleanup: bool,
    pub auto_cleanup_keep: RetentionJson,
    /// Pre-computed: `!auto_cleanup || auto_cleanup_keep.is_disabled()`.
    /// Same rationale as the per-ws effective view.
    pub auto_cleanup_is_disabled: bool,
    pub auto_cleanup_interval_secs: u64,
    pub health_check_interval_secs: u64,
    pub img_size_gb: u64,
    pub img_max_percent: f64,
}

/// Versioned schema tag for [`GlobalConfigJson`].
pub const GLOBAL_CONFIG_JSON_SCHEMA: &str = "ws-ckpt-config/v1";

/// Versioned schema tag for the `ws-ckpt config` (no-scope) overview JSON
/// emitted by the CLI: global config snapshot + workspace override counts.
/// Same naming convention as the policy/config tags (all `-` separated, /vN).
pub const OVERVIEW_JSON_SCHEMA: &str = "ws-ckpt-overview/v1";

/// Result of [`load_workspace_policy`]: distinguishes "no file" from "I/O
/// error" so a reload racing a half-done writer won't nuke the in-memory policy.
#[derive(Debug)]
pub enum LoadPolicyOutcome {
    /// No `policy.toml` — treat as inherit-global.
    Missing,
    /// Loaded successfully (may be `WorkspacePolicy::default()` if empty).
    Loaded(WorkspacePolicy),
}

/// Load `<index_dir>/policy.toml`: `Missing` if absent, `Loaded(p)` on
/// success, `Err` otherwise (parse/IO). See [`load_workspace_policy_or_default`]
/// to collapse `Missing`.
pub fn load_workspace_policy(index_dir: &Path) -> Result<LoadPolicyOutcome, WsCkptError> {
    let path = index_dir.join(POLICY_FILE);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let p: WorkspacePolicy = toml::from_str(&content)
                .map_err(|e| WsCkptError::Config(format!("parse {}: {}", path.display(), e)))?;
            Ok(LoadPolicyOutcome::Loaded(p))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LoadPolicyOutcome::Missing),
        Err(e) => Err(WsCkptError::Io(e)),
    }
}

/// Like [`load_workspace_policy`] but collapses `Missing` → default. Use only
/// when there's no in-memory state to preserve on transient errors (e.g. startup).
pub fn load_workspace_policy_or_default(index_dir: &Path) -> Result<WorkspacePolicy, WsCkptError> {
    match load_workspace_policy(index_dir)? {
        LoadPolicyOutcome::Missing => Ok(WorkspacePolicy::default()),
        LoadPolicyOutcome::Loaded(p) => Ok(p),
    }
}

/// Register-time policy loader with `auto_cleanup = false` fail-safe.
///
/// Used by every workspace register path (init / adopt_existing_subvol /
/// rebuild_from_persisted / rebuild_workspace_into_state). Three outcomes:
/// - `Missing` → `(default(), false)`; user never set a policy, inherit-global is correct
/// - `Loaded(p)` → `(p, false)`; on-disk truth
/// - `Err(e)` → `({auto_cleanup: Some(false), ..}, true)`; refuse to inherit-global
///   because the next scheduler tick under a globally-enabled cfg would delete
///   protected snapshots. The `true` marker propagates to `policy_failsafe`,
///   which PATCH refuses until reload/reset.
///
/// `entry_label` ("init" / "adopt" / "rebuild") is the only differing piece
/// across the 4 sites; it goes into the warn! message to keep the per-site
/// diagnostic.
pub fn load_workspace_policy_with_failsafe(
    index_dir: &Path,
    ws_id: &str,
    entry_label: &str,
) -> (WorkspacePolicy, bool) {
    match load_workspace_policy(index_dir) {
        Ok(LoadPolicyOutcome::Missing) => (WorkspacePolicy::default(), false),
        Ok(LoadPolicyOutcome::Loaded(p)) => (p, false),
        Err(e) => {
            tracing::warn!(
                "{} of {}: policy.toml at {:?} unreadable: {} — registering with in-memory \
                 `auto_cleanup = false` until next reload succeeds. Inherit-global would risk \
                 deleting protected snapshots, so we explicitly disable cleanup for this ws \
                 as a fail-safe.",
                entry_label,
                ws_id,
                index_dir,
                e
            );
            (
                WorkspacePolicy {
                    auto_cleanup: Some(false),
                    auto_cleanup_keep: None,
                },
                true,
            )
        }
    }
}

/// Atomically persist a per-workspace policy via [`persist::atomic_write`]
/// (0600, root-only daemon-internal data).
pub fn save_workspace_policy(
    index_dir: &Path,
    policy: &WorkspacePolicy,
) -> Result<(), WsCkptError> {
    let content = toml::to_string_pretty(policy)
        .map_err(|e| WsCkptError::Config(format!("serialize policy: {}", e)))?;
    persist::atomic_write(index_dir, POLICY_FILE, content.as_bytes(), Some(0o600))
        .map_err(|e| WsCkptError::Config(format!("save_workspace_policy: {:#}", e)))
}

/// Remove `<index_dir>/policy.toml` (missing-file is OK; `--reset` is
/// idempotent), then fsync the parent dir. A post-unlink fsync failure is
/// logged, not propagated — the file is already gone.
pub fn delete_workspace_policy(index_dir: &Path) -> Result<(), WsCkptError> {
    let target = index_dir.join(POLICY_FILE);
    match std::fs::remove_file(&target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(WsCkptError::Io(e)),
    }
    if let Err(e) = persist::fsync_dir(index_dir) {
        tracing::warn!(
            "delete_workspace_policy: unlink of {:?} succeeded but parent dir fsync failed: {:#} \
             (file is gone; durability across a crash is best-effort on this fs)",
            target,
            e
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Helper: encode then decode round-trip ──
    fn round_trip_request(req: &Request) -> Request {
        let frame = encode_frame(req).expect("encode_frame failed");
        // Skip first 4 bytes (length prefix)
        let payload = &frame[4..];
        decode_payload::<Request>(payload).expect("decode_payload failed")
    }

    fn round_trip_response(resp: &Response) -> Response {
        let frame = encode_frame(resp).expect("encode_frame failed");
        let payload = &frame[4..];
        decode_payload::<Response>(payload).expect("decode_payload failed")
    }

    // ── Request round-trip tests ──

    #[test]
    fn request_init_round_trip() {
        let req = Request::Init {
            workspace: "/tmp/test-ws".to_string(),
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Init { workspace } => assert_eq!(workspace, "/tmp/test-ws"),
            _ => panic!("expected Init variant"),
        }
    }

    #[test]
    fn request_checkpoint_round_trip() {
        let req = Request::Checkpoint {
            workspace: "/tmp/ws".to_string(),
            id: "msg1-step0".to_string(),
            message: Some("save point".to_string()),
            metadata: None,
            pin: true,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Checkpoint {
                workspace,
                id,
                message,
                metadata,
                pin,
            } => {
                assert_eq!(workspace, "/tmp/ws");
                assert_eq!(id, "msg1-step0");
                assert_eq!(message.as_deref(), Some("save point"));
                assert!(metadata.is_none());
                assert!(pin);
            }
            _ => panic!("expected Checkpoint variant"),
        }
    }

    #[test]
    fn request_checkpoint_with_metadata_round_trip() {
        // metadata is now Option<String> (JSON string), which bincode handles fine.
        let json_str = r#"{"key":"value"}"#.to_string();
        let req = Request::Checkpoint {
            workspace: "/ws".to_string(),
            id: "msg2-step0".to_string(),
            message: None,
            metadata: Some(json_str.clone()),
            pin: false,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Checkpoint { metadata, .. } => {
                assert_eq!(metadata, Some(json_str));
            }
            _ => panic!("expected Checkpoint variant"),
        }
    }

    #[test]
    fn request_checkpoint_minimal_round_trip() {
        // Checkpoint with no optional fields
        let req = Request::Checkpoint {
            workspace: "/ws".to_string(),
            id: "msg1-step0".to_string(),
            message: None,
            metadata: None,
            pin: false,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Checkpoint {
                message,
                metadata,
                pin,
                ..
            } => {
                assert!(message.is_none());
                assert!(metadata.is_none());
                assert!(!pin);
            }
            _ => panic!("expected Checkpoint variant"),
        }
    }

    #[test]
    fn request_rollback_round_trip() {
        let req = Request::Rollback {
            workspace: "/tmp/ws".to_string(),
            to: Some("msg1-step2".to_string()),
            num_ancestors: None,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Rollback {
                workspace,
                to,
                num_ancestors,
            } => {
                assert_eq!(workspace, "/tmp/ws");
                assert_eq!(to.as_deref(), Some("msg1-step2"));
                assert_eq!(num_ancestors, None);
            }
            _ => panic!("expected Rollback variant"),
        }
    }

    #[test]
    fn request_delete_round_trip() {
        let req = Request::Delete {
            workspace: Some("/tmp/ws".to_string()),
            snapshot: "msg2-step0".to_string(),
            force: true,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Delete {
                workspace,
                snapshot,
                force,
            } => {
                assert_eq!(workspace.as_deref(), Some("/tmp/ws"));
                assert_eq!(snapshot, "msg2-step0");
                assert!(force);
            }
            _ => panic!("expected Delete variant"),
        }
    }

    #[test]
    fn request_delete_no_force_round_trip() {
        let req = Request::Delete {
            workspace: Some("/ws".to_string()),
            snapshot: "abc123".to_string(),
            force: false,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Delete {
                workspace,
                snapshot,
                force,
            } => {
                assert_eq!(workspace.as_deref(), Some("/ws"));
                assert_eq!(snapshot, "abc123");
                assert!(!force);
            }
            _ => panic!("expected Delete variant"),
        }
    }

    #[test]
    fn request_delete_no_workspace_round_trip() {
        let req = Request::Delete {
            workspace: None,
            snapshot: "msg1-step0".to_string(),
            force: false,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Delete {
                workspace,
                snapshot,
                force,
            } => {
                assert!(workspace.is_none());
                assert_eq!(snapshot, "msg1-step0");
                assert!(!force);
            }
            _ => panic!("expected Delete variant"),
        }
    }

    // ── Response round-trip tests ──

    #[test]
    fn response_init_ok_round_trip() {
        let resp = Response::InitOk {
            ws_id: "ws-a3f2b1".to_string(),
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::InitOk { ws_id } => assert_eq!(ws_id, "ws-a3f2b1"),
            _ => panic!("expected InitOk variant"),
        }
    }

    #[test]
    fn response_checkpoint_ok_round_trip() {
        let resp = Response::CheckpointOk {
            snapshot_id: "msg1-step2".to_string(),
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::CheckpointOk { snapshot_id } => assert_eq!(snapshot_id, "msg1-step2"),
            _ => panic!("expected CheckpointOk variant"),
        }
    }

    #[test]
    fn response_rollback_ok_round_trip() {
        let resp = Response::RollbackOk {
            from: "workspace-abc123".to_string(),
            to: "msg1-step0".to_string(),
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::RollbackOk { from, to } => {
                assert_eq!(from, "workspace-abc123");
                assert_eq!(to, "msg1-step0");
            }
            _ => panic!("expected RollbackOk variant"),
        }
    }

    #[test]
    fn response_delete_ok_round_trip() {
        let resp = Response::DeleteOk {
            target: "msg1-step2".to_string(),
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::DeleteOk { target } => assert_eq!(target, "msg1-step2"),
            _ => panic!("expected DeleteOk variant"),
        }
    }

    #[test]
    fn response_error_round_trip() {
        let resp = Response::Error {
            code: ErrorCode::WorkspaceNotFound,
            message: "workspace not found: /tmp/ws".to_string(),
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::WorkspaceNotFound);
                assert_eq!(message, "workspace not found: /tmp/ws");
            }
            _ => panic!("expected Error variant"),
        }
    }

    #[test]
    fn response_error_all_codes_round_trip() {
        // Verify every ErrorCode variant survives round-trip
        let codes = vec![
            ErrorCode::WorkspaceNotFound,
            ErrorCode::SnapshotNotFound,
            ErrorCode::AlreadyInitialized,
            ErrorCode::BtrfsError,
            ErrorCode::IoError,
            ErrorCode::InvalidPath,
            ErrorCode::ConfirmationRequired,
            ErrorCode::InternalError,
            ErrorCode::SnapshotAlreadyExists,
            ErrorCode::WriteLockConflict,
            ErrorCode::DiskSpaceInsufficient,
            ErrorCode::CwdOccupied,
            ErrorCode::CwdScanFailed,
        ];
        for code in codes {
            let resp = Response::Error {
                code: code.clone(),
                message: format!("test {:?}", code),
            };
            let decoded = round_trip_response(&resp);
            match decoded {
                Response::Error {
                    code: dc,
                    message: dm,
                } => {
                    assert_eq!(dc, code);
                    assert!(dm.starts_with("test "));
                }
                _ => panic!("expected Error variant"),
            }
        }
    }

    // ── Frame format tests ──

    #[test]
    fn encode_frame_length_prefix_is_le() {
        // Verify the first 4 bytes of encode_frame are LE-encoded payload length
        let req = Request::Init {
            workspace: "/tmp/test".to_string(),
        };
        let frame = encode_frame(&req).expect("encode_frame failed");
        let len_bytes: [u8; 4] = frame[..4].try_into().unwrap();
        let encoded_len = u32::from_le_bytes(len_bytes) as usize;
        // The rest of the frame should be exactly `encoded_len` bytes
        assert_eq!(frame.len() - 4, encoded_len);
    }

    #[test]
    fn encode_frame_payload_matches_bincode() {
        // Verify the payload portion matches direct bincode serialization
        let req = Request::Init {
            workspace: "/hello".to_string(),
        };
        let frame = encode_frame(&req).unwrap();
        let expected_payload = bincode::serialize(&req).unwrap();
        assert_eq!(&frame[4..], &expected_payload[..]);
    }

    // ── SnapshotIndex tests ──

    #[test]
    fn snapshot_index_new_is_empty() {
        let idx = SnapshotIndex::new(PathBuf::from("/tmp/ws"));
        assert_eq!(idx.workspace_path, PathBuf::from("/tmp/ws"));
        assert!(idx.snapshots.is_empty());
    }

    #[test]
    fn snapshot_index_resolve_by_prefix_exact_match() {
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert(
            "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: true,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        let result = idx.resolve_by_prefix("abcdef1234567890abcdef1234567890abcdef12");
        assert!(result.is_ok());
        let (id, _) = result.unwrap();
        assert_eq!(id, "abcdef1234567890abcdef1234567890abcdef12");
    }

    #[test]
    fn snapshot_index_resolve_by_prefix_unique_prefix() {
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert(
            "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        let result = idx.resolve_by_prefix("abcdef");
        assert!(result.is_ok());
    }

    #[test]
    fn snapshot_index_resolve_by_prefix_not_found() {
        let idx = SnapshotIndex::new(PathBuf::from("/ws"));
        let result = idx.resolve_by_prefix("nonexistent");
        assert_eq!(result.unwrap_err(), ResolveError::NotFound);
    }

    #[test]
    fn snapshot_index_resolve_by_prefix_ambiguous() {
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert(
            "abcdef1111111111111111111111111111111111".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        idx.snapshots.insert(
            "abcdef2222222222222222222222222222222222".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        let result = idx.resolve_by_prefix("abcdef");
        assert_eq!(result.unwrap_err(), ResolveError::Ambiguous(2));
    }

    // ── SnapshotIndex::ancestor() tests ──

    fn make_chain_index() -> SnapshotIndex {
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert(
            "snap-a".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        idx.snapshots.insert(
            "snap-b".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: Some("snap-a".to_string()),
                child_ids: vec![],
            },
        );
        idx.snapshots.insert(
            "snap-c".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: Some("snap-b".to_string()),
                child_ids: vec![],
            },
        );
        idx.head = Some("snap-c".to_string());
        idx
    }

    #[test]
    fn ancestor_zero_returns_error() {
        let idx = make_chain_index();
        assert_eq!(
            idx.ancestor(0).unwrap_err(),
            AncestorError::InvalidAncestors
        );
    }

    #[test]
    fn ancestor_one_returns_head() {
        let idx = make_chain_index();
        let (id, _) = idx.ancestor(1).unwrap();
        assert_eq!(id, "snap-c");
    }

    #[test]
    fn ancestor_two_returns_parent() {
        let idx = make_chain_index();
        let (id, _) = idx.ancestor(2).unwrap();
        assert_eq!(id, "snap-b");
    }

    #[test]
    fn ancestor_three_returns_grandparent() {
        let idx = make_chain_index();
        let (id, _) = idx.ancestor(3).unwrap();
        assert_eq!(id, "snap-a");
    }

    #[test]
    fn ancestor_exceeds_chain_returns_broken_chain() {
        let idx = make_chain_index();
        let err = idx.ancestor(4).unwrap_err();
        assert_eq!(err, AncestorError::BrokenChain { depth: 3 });
    }

    #[test]
    fn ancestor_no_head_returns_error() {
        let idx = SnapshotIndex::new(PathBuf::from("/ws"));
        assert_eq!(idx.ancestor(1).unwrap_err(), AncestorError::NoHead);
    }

    #[test]
    fn ancestor_broken_chain_mid_traversal() {
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert(
            "orphan".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        idx.snapshots.insert(
            "child".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: Some("orphan".to_string()),
                child_ids: vec![],
            },
        );
        idx.head = Some("child".to_string());
        let err = idx.ancestor(3).unwrap_err();
        assert_eq!(err, AncestorError::BrokenChain { depth: 2 });
    }

    // ── DaemonConfig::default() tests ──

    #[test]
    fn daemon_config_default_values() {
        let cfg = DaemonConfig::default();
        assert_eq!(cfg.mount_path, PathBuf::from(DEFAULT_MOUNT_PATH));
        assert_eq!(cfg.socket_path, PathBuf::from(DEFAULT_SOCKET_PATH));
        assert_eq!(cfg.log_level, "info");
    }

    // ── WsCkptError Display tests ──

    #[test]
    fn error_display_frame_too_large() {
        let err = WsCkptError::FrameTooLarge {
            size: 20_000_000,
            max: 16_777_216,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("frame too large"));
        assert!(msg.contains("20000000"));
        assert!(msg.contains("16777216"));
    }

    #[test]
    fn error_display_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = WsCkptError::Io(io_err);
        let msg = format!("{}", err);
        assert!(msg.contains("io error"));
    }

    #[test]
    fn error_display_json() {
        // Trigger a real serde_json error
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err = WsCkptError::Json(json_err);
        let msg = format!("{}", err);
        assert!(msg.contains("json error"));
    }

    #[test]
    fn error_display_bincode() {
        // Trigger a real bincode error (invalid data for a Request)
        let bad_data = vec![0xFF, 0xFF, 0xFF];
        let bincode_err = bincode::deserialize::<Request>(&bad_data).unwrap_err();
        let err = WsCkptError::Bincode(bincode_err);
        let msg = format!("{}", err);
        assert!(msg.contains("bincode error"));
    }

    // ── Phase 2 Request round-trip tests ──

    #[test]
    fn list_wire_compatibility_and_frame_boundary() {
        // Existing bincode discriminants and layouts must not change.
        let request = Request::List {
            workspace: None,
            format: None,
        };
        assert_eq!(
            bincode::serialize(&request).unwrap(),
            vec![4, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            bincode::serialize(&Response::ListOk { snapshots: vec![] }).unwrap(),
            vec![5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        let request = Request::ListPage {
            orphans_only: false,
            workspace: Some("/ws".into()),
            limit: 10,
            cursor: Some("opaque".into()),
        };
        assert_eq!(
            bincode::serialize(&request).unwrap(),
            bincode::serialize(&(29_u32, false, Some("/ws"), 10_u32, Some("opaque"))).unwrap()
        );
        assert!(matches!(round_trip_request(&request), Request::ListPage {
            orphans_only: false,
            workspace: Some(ws), limit: 10, cursor: Some(cursor),
        } if ws == "/ws" && cursor == "opaque"));

        // A bincode string has an eight-byte length prefix.
        let mut payload = "x".repeat(MAX_FRAME_SIZE as usize - 8);
        let frame = encode_frame(&payload).unwrap();
        assert_eq!(frame.len(), MAX_FRAME_SIZE as usize + 4);
        payload.push('x');
        assert!(
            matches!(encode_frame(&payload), Err(WsCkptError::FrameTooLarge {
            size, max: MAX_FRAME_SIZE,
        }) if size == u64::from(MAX_FRAME_SIZE) + 1)
        );
    }

    #[test]
    fn request_list_round_trip() {
        let req = Request::List {
            workspace: Some("/tmp/ws".to_string()),
            format: Some("json".to_string()),
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::List { workspace, format } => {
                assert_eq!(workspace.as_deref(), Some("/tmp/ws"));
                assert_eq!(format.as_deref(), Some("json"));
            }
            _ => panic!("expected List variant"),
        }
    }

    #[test]
    fn request_list_no_format_round_trip() {
        let req = Request::List {
            workspace: Some("/ws".to_string()),
            format: None,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::List { format, .. } => assert!(format.is_none()),
            _ => panic!("expected List variant"),
        }
    }

    #[test]
    fn request_diff_round_trip() {
        let req = Request::Diff {
            workspace: "/tmp/ws".to_string(),
            from: "msg1-step0".to_string(),
            to: Some("msg2-step0".to_string()),
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Diff {
                workspace,
                from,
                to,
            } => {
                assert_eq!(workspace, "/tmp/ws");
                assert_eq!(from, "msg1-step0");
                assert_eq!(to, Some("msg2-step0".to_string()));
            }
            _ => panic!("expected Diff variant"),
        }
    }

    #[test]
    fn request_rollback_preview_round_trip() {
        let req = Request::RollbackPreview {
            workspace: "/tmp/ws".to_string(),
            to: Some("msg1-step0".to_string()),
            num_ancestors: None,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::RollbackPreview {
                workspace,
                to,
                num_ancestors,
            } => {
                assert_eq!(workspace, "/tmp/ws");
                assert_eq!(to.as_deref(), Some("msg1-step0"));
                assert_eq!(num_ancestors, None);
            }
            _ => panic!("expected RollbackPreview variant"),
        }
    }

    #[test]
    fn request_status_round_trip() {
        let req = Request::Status {
            workspace: Some("/tmp/ws".to_string()),
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Status { workspace } => {
                assert_eq!(workspace.as_deref(), Some("/tmp/ws"));
            }
            _ => panic!("expected Status variant"),
        }
    }

    #[test]
    fn request_status_no_workspace_round_trip() {
        let req = Request::Status { workspace: None };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Status { workspace } => assert!(workspace.is_none()),
            _ => panic!("expected Status variant"),
        }
    }

    #[test]
    fn request_cleanup_round_trip() {
        let req = Request::Cleanup {
            workspace: "/tmp/ws".to_string(),
            keep: Some(10),
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Cleanup { workspace, keep } => {
                assert_eq!(workspace, "/tmp/ws");
                assert_eq!(keep, Some(10));
            }
            _ => panic!("expected Cleanup variant"),
        }
    }

    #[test]
    fn request_cleanup_no_keep_round_trip() {
        let req = Request::Cleanup {
            workspace: "/ws".to_string(),
            keep: None,
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Cleanup { keep, .. } => assert!(keep.is_none()),
            _ => panic!("expected Cleanup variant"),
        }
    }

    // ── Phase 2 Response round-trip tests ──

    #[test]
    fn response_list_ok_round_trip() {
        let resp = Response::ListOk {
            snapshots: vec![SnapshotEntry {
                id: "abc123def456".to_string(),
                workspace: "/home/user/ws".to_string(),
                meta: SnapshotMeta {
                    message: Some("first".to_string()),
                    metadata: None,
                    pinned: true,
                    created_at: chrono::Utc::now(),
                    missing: false,
                    parent_id: None,
                    child_ids: vec![],
                },
            }],
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::ListOk { snapshots } => {
                assert_eq!(snapshots.len(), 1);
                assert_eq!(snapshots[0].id, "abc123def456");
            }
            _ => panic!("expected ListOk variant"),
        }
    }

    #[test]
    fn snapshot_entry_round_trip() {
        let entry = SnapshotEntry {
            id: "abc123def456".to_string(),
            workspace: "/home/user/ws".to_string(),
            meta: SnapshotMeta {
                message: Some("test message".to_string()),
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        };
        let serialized = serde_json::to_string(&entry).unwrap();
        let deserialized: SnapshotEntry = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.id, "abc123def456");
        assert_eq!(deserialized.meta.message.as_deref(), Some("test message"));
        assert!(!deserialized.meta.pinned);
    }

    /// bincode round-trip with a non-trivial `metadata` Value.
    /// Regression: pre-fix, `Option<serde_json::Value>` would fail bincode
    /// deserialize because Value requires `deserialize_any`.
    #[test]
    fn response_list_ok_with_metadata_bincode_round_trip() {
        let metadata = serde_json::json!({"event": "init", "n": 42, "tags": ["a", "b"]});
        let resp = Response::ListOk {
            snapshots: vec![SnapshotEntry {
                id: "abc".to_string(),
                workspace: "/ws".to_string(),
                meta: SnapshotMeta {
                    message: Some("first".to_string()),
                    metadata: Some(metadata.clone()),
                    pinned: false,
                    created_at: chrono::Utc::now(),
                    missing: false,
                    parent_id: None,
                    child_ids: vec![],
                },
            }],
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::ListOk { snapshots } => {
                assert_eq!(snapshots.len(), 1);
                assert_eq!(snapshots[0].meta.metadata, Some(metadata));
            }
            _ => panic!("expected ListOk variant"),
        }
    }

    /// `Some(Value::Null)` collapses to `None` on both JSON and bincode paths,
    /// matching `Option<Value>`'s natural JSON round-trip behavior.
    #[test]
    fn snapshot_meta_metadata_null_collapses_to_none() {
        let meta = SnapshotMeta {
            message: None,
            metadata: Some(serde_json::Value::Null),
            pinned: false,
            created_at: chrono::Utc::now(),
            missing: false,
            parent_id: None,
            child_ids: vec![],
        };
        // JSON path
        let json = serde_json::to_string(&meta).unwrap();
        let from_json: SnapshotMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(from_json.metadata, None);
        // bincode path
        let bin = bincode::serialize(&meta).unwrap();
        let from_bin: SnapshotMeta = bincode::deserialize(&bin).unwrap();
        assert_eq!(from_bin.metadata, None);
    }

    /// JSON round-trip keeps `metadata` as a nested Value (not a quoted string).
    /// Verifies the `is_human_readable() == true` path of `metadata_serde`.
    #[test]
    fn snapshot_meta_metadata_json_round_trip_keeps_nested_object() {
        let metadata = serde_json::json!({"k": "v"});
        let meta = SnapshotMeta {
            message: None,
            metadata: Some(metadata.clone()),
            pinned: false,
            created_at: chrono::Utc::now(),
            missing: false,
            parent_id: None,
            child_ids: vec![],
        };
        let s = serde_json::to_string(&meta).unwrap();
        // metadata is rendered as an object, not as an escaped string
        assert!(s.contains(r#""metadata":{"k":"v"}"#), "got: {}", s);
        let parsed: SnapshotMeta = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.metadata, Some(metadata));
    }

    #[test]
    fn response_diff_ok_round_trip() {
        let resp = Response::DiffOk {
            changes: vec![
                DiffEntry {
                    path: "src/main.rs".to_string(),
                    change_type: ChangeType::Modified,
                    detail: Some("content changed".to_string()),
                },
                DiffEntry {
                    path: "new_file.txt".to_string(),
                    change_type: ChangeType::Added,
                    detail: None,
                },
            ],
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::DiffOk { changes } => {
                assert_eq!(changes.len(), 2);
                assert_eq!(changes[0].change_type, ChangeType::Modified);
                assert_eq!(changes[1].change_type, ChangeType::Added);
            }
            _ => panic!("expected DiffOk variant"),
        }
    }

    #[test]
    fn response_rollback_preview_ok_round_trip() {
        let resp = Response::RollbackPreviewOk {
            to: "msg1-step0".to_string(),
            changes: vec![DiffEntry {
                path: "src/main.rs".to_string(),
                change_type: ChangeType::Modified,
                detail: Some("content changed".to_string()),
            }],
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::RollbackPreviewOk { to, changes } => {
                assert_eq!(to, "msg1-step0");
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].change_type, ChangeType::Modified);
            }
            _ => panic!("expected RollbackPreviewOk variant"),
        }
    }

    #[test]
    fn response_status_ok_round_trip() {
        let resp = Response::StatusOk {
            report: StatusReport {
                uptime_secs: 3600,
                workspaces: vec![WorkspaceInfo {
                    ws_id: "ws-abc".to_string(),
                    path: "/home/user/ws".to_string(),
                    snapshot_count: 5,
                }],
                fs_total_bytes: 1_000_000_000,
                fs_used_bytes: 500_000_000,
            },
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::StatusOk { report } => {
                assert_eq!(report.uptime_secs, 3600);
                assert_eq!(report.workspaces.len(), 1);
                assert_eq!(report.workspaces[0].ws_id, "ws-abc");
                assert_eq!(report.fs_total_bytes, 1_000_000_000);
                assert_eq!(report.fs_used_bytes, 500_000_000);
            }
            _ => panic!("expected StatusOk variant"),
        }
    }

    #[test]
    fn response_cleanup_ok_round_trip() {
        let resp = Response::CleanupOk {
            removed: vec!["msg1-step0".to_string(), "msg1-step1".to_string()],
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::CleanupOk { removed } => {
                assert_eq!(removed.len(), 2);
                assert_eq!(removed[0], "msg1-step0");
                assert_eq!(removed[1], "msg1-step1");
            }
            _ => panic!("expected CleanupOk variant"),
        }
    }

    #[test]
    fn request_config_round_trip() {
        let req = Request::Config;
        let decoded = round_trip_request(&req);
        assert!(matches!(decoded, Request::Config));
    }

    #[test]
    fn response_config_ok_round_trip() {
        let resp = Response::ConfigOk {
            config: ConfigReport {
                mount_path: "/mnt/btrfs-workspace".to_string(),
                socket_path: "/run/ws-ckpt/ws-ckpt.sock".to_string(),
                log_level: "info".to_string(),
                auto_cleanup: false,
                auto_cleanup_keep: CleanupRetention::Count(20),
                auto_cleanup_interval_secs: 86_400,
                health_check_interval_secs: 300,
                img_size: 30,
                img_max_percent: 40.0,
            },
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::ConfigOk { config } => {
                assert_eq!(config.mount_path, "/mnt/btrfs-workspace");
                assert_eq!(config.auto_cleanup_keep, CleanupRetention::Count(20));
                assert_eq!(config.auto_cleanup_interval_secs, 86_400);
            }
            _ => panic!("expected ConfigOk variant"),
        }
    }

    #[test]
    fn request_reload_config_round_trip() {
        let req = Request::ReloadConfig;
        let decoded = round_trip_request(&req);
        assert!(matches!(decoded, Request::ReloadConfig));
    }

    #[test]
    fn response_reload_config_ok_round_trip() {
        let resp = Response::ReloadConfigOk {
            config: ConfigReport {
                mount_path: "/mnt/btrfs-workspace".to_string(),
                socket_path: "/run/ws-ckpt/ws-ckpt.sock".to_string(),
                log_level: "info".to_string(),
                auto_cleanup: true,
                auto_cleanup_keep: CleanupRetention::Count(5),
                auto_cleanup_interval_secs: 3_600,
                health_check_interval_secs: 60,
                img_size: 30,
                img_max_percent: 40.0,
            },
            file: Some(FileConfig {
                auto_cleanup: Some(true),
                auto_cleanup_keep: Some(CleanupRetention::age("30d").unwrap()),
                ..Default::default()
            }),
        };
        match round_trip_response(&resp) {
            Response::ReloadConfigOk { config, file } => {
                assert!(config.auto_cleanup);
                assert_eq!(config.auto_cleanup_keep, CleanupRetention::Count(5));
                assert_eq!(config.auto_cleanup_interval_secs, 3_600);
                // Age retention re-derives secs from `raw` over the wire; the
                // client compares this against the file it wrote, so the
                // round-trip must preserve it exactly.
                let file = file.expect("daemon-visible config file");
                assert_eq!(file.auto_cleanup, Some(true));
                assert_eq!(
                    file.auto_cleanup_keep,
                    Some(CleanupRetention::age("30d").unwrap())
                );
            }
            _ => panic!("expected ReloadConfigOk variant"),
        }
    }

    // ── FileConfig tests ──

    #[test]
    fn file_config_toml_round_trip() {
        let fc = FileConfig {
            auto_cleanup_keep: Some(CleanupRetention::Count(30)),
            auto_cleanup_interval_secs: Some(300),
            health_check_interval_secs: Some(180),
            ..Default::default()
        };
        let s = toml::to_string(&fc).unwrap();
        let parsed: FileConfig = toml::from_str(&s).unwrap();
        assert_eq!(parsed, fc);
    }

    #[test]
    fn file_config_toml_age_mode_round_trip() {
        let fc = FileConfig {
            auto_cleanup_keep: Some(CleanupRetention::age("30d").unwrap()),
            ..Default::default()
        };
        let s = toml::to_string(&fc).unwrap();
        let parsed: FileConfig = toml::from_str(&s).unwrap();
        assert_eq!(parsed, fc);
    }

    #[test]
    fn file_config_toml_rejects_invalid_age_string() {
        // Bare number (missing unit suffix)
        let err = toml::from_str::<FileConfig>("auto_cleanup_keep = \"10\"\n").unwrap_err();
        assert!(
            err.to_string().contains("missing unit suffix"),
            "expected missing-unit error, got: {}",
            err
        );
        // Unknown unit
        let err = toml::from_str::<FileConfig>("auto_cleanup_keep = \"30x\"\n").unwrap_err();
        assert!(
            err.to_string().contains("invalid unit"),
            "expected invalid-unit error, got: {}",
            err
        );
        // Garbage
        let err = toml::from_str::<FileConfig>("auto_cleanup_keep = \"abc\"\n").unwrap_err();
        assert!(
            err.to_string().contains("invalid number"),
            "expected invalid-number error, got: {}",
            err
        );
    }

    #[test]
    fn file_config_partial_toml() {
        let s = "auto_cleanup_keep = 50\n";
        let fc: FileConfig = toml::from_str(s).unwrap();
        assert_eq!(fc.auto_cleanup_keep, Some(CleanupRetention::Count(50)));
        assert_eq!(fc.auto_cleanup_interval_secs, None);
        assert_eq!(fc.health_check_interval_secs, None);
    }

    #[test]
    fn file_config_age_mode_toml() {
        let s = "auto_cleanup_keep = \"2w\"\n";
        let fc: FileConfig = toml::from_str(s).unwrap();
        assert_eq!(
            fc.auto_cleanup_keep,
            Some(CleanupRetention::age("2w").unwrap())
        );
    }

    #[test]
    fn parse_duration_accepts_units() {
        assert_eq!(parse_duration_secs("30s").unwrap(), 30);
        assert_eq!(parse_duration_secs("5m").unwrap(), 300);
        assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
        assert_eq!(parse_duration_secs("30d").unwrap(), 2_592_000);
        assert_eq!(parse_duration_secs("2w").unwrap(), 1_209_600);
    }

    #[test]
    fn parse_duration_rejects_bad_input() {
        assert!(parse_duration_secs("").is_err());
        assert!(parse_duration_secs("30").is_err()); // missing unit
        assert!(parse_duration_secs("abc").is_err());
        assert!(parse_duration_secs("30y").is_err()); // year not supported
    }

    #[test]
    fn parse_duration_rejects_i64_overflow() {
        // u64::MAX weeks clearly saturates past i64::MAX and must be rejected
        // so downstream `chrono::Duration::seconds(secs as i64)` stays safe.
        let huge = format!("{}w", u64::MAX);
        assert!(parse_duration_secs(&huge).is_err());
    }

    #[test]
    fn file_config_empty_toml() {
        let fc: FileConfig = toml::from_str("").unwrap();
        assert_eq!(fc, FileConfig::default());
    }

    #[test]
    fn load_config_file_nonexistent_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let fc = load_config_file(&dir.path().join("config.toml")).unwrap();
        assert_eq!(fc, FileConfig::default());
    }

    #[test]
    fn save_and_load_config_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let fc = FileConfig {
            auto_cleanup_keep: Some(CleanupRetention::Count(15)),
            auto_cleanup_interval_secs: Some(120),
            health_check_interval_secs: Some(60),
            ..Default::default()
        };
        save_config_file(&path, &fc).unwrap();
        let loaded = load_config_file(&path).unwrap();
        assert_eq!(loaded, fc);
    }

    #[test]
    fn save_config_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("dir").join("config.toml");
        let fc = FileConfig {
            auto_cleanup_keep: Some(CleanupRetention::Count(5)),
            auto_cleanup_interval_secs: None,
            health_check_interval_secs: None,
            ..Default::default()
        };
        save_config_file(&path, &fc).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn load_config_file_empty_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "").unwrap();
        let fc = load_config_file(&path).unwrap();
        assert_eq!(fc, FileConfig::default());
    }

    #[test]
    fn load_config_file_invalid_toml_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "not = [valid toml {{").unwrap();
        let result = load_config_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn load_config_file_rejects_img_max_percent_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["-1.0", "100.5", "nan", "inf"] {
            let path = dir.path().join(format!("bad_{}.toml", bad));
            let toml = format!("[backend.btrfs-loop]\nimg_max_percent = {}\n", bad);
            std::fs::write(&path, toml).unwrap();
            let result = load_config_file(&path);
            assert!(result.is_err(), "{} should be rejected", bad);
        }
    }

    #[test]
    fn load_config_file_accepts_img_max_percent_in_range() {
        let dir = tempfile::tempdir().unwrap();
        for good in ["0.0", "40.0", "100.0"] {
            let path = dir.path().join(format!("good_{}.toml", good));
            let toml = format!("[backend.btrfs-loop]\nimg_max_percent = {}\n", good);
            std::fs::write(&path, toml).unwrap();
            let result = load_config_file(&path);
            assert!(
                result.is_ok(),
                "{} should be accepted, got {:?}",
                good,
                result
            );
        }
    }

    // ── Recover round-trip tests ──

    #[test]
    fn request_recover_round_trip() {
        let req = Request::Recover {
            workspace: "/tmp/my-project".to_string(),
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::Recover { workspace } => assert_eq!(workspace, "/tmp/my-project"),
            _ => panic!("expected Recover variant"),
        }
    }

    #[test]
    fn response_recover_ok_round_trip() {
        let resp = Response::RecoverOk {
            workspace: "/home/user/project".to_string(),
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::RecoverOk { workspace, .. } => assert_eq!(workspace, "/home/user/project"),
            _ => panic!("expected RecoverOk variant"),
        }
    }

    // ── HealthAdvisory round-trip tests ──

    #[test]
    fn request_health_advisory_round_trip() {
        let req = Request::HealthAdvisory;
        let decoded = round_trip_request(&req);
        match decoded {
            Request::HealthAdvisory => {}
            _ => panic!("expected HealthAdvisory variant"),
        }
    }

    #[test]
    fn response_health_advisory_ok_round_trip() {
        let resp = Response::HealthAdvisoryOk {
            over_limit_workspace_count: 3,
            fs_total_bytes: 100 * 1024 * 1024 * 1024,
            fs_used_bytes: 94 * 1024 * 1024 * 1024,
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::HealthAdvisoryOk {
                over_limit_workspace_count,
                fs_total_bytes,
                fs_used_bytes,
            } => {
                assert_eq!(over_limit_workspace_count, 3);
                assert_eq!(fs_total_bytes, 100 * 1024 * 1024 * 1024);
                assert_eq!(fs_used_bytes, 94 * 1024 * 1024 * 1024);
            }
            _ => panic!("expected HealthAdvisoryOk variant"),
        }
    }

    // ── WorkspacePolicy tests ──

    fn global_count(n: u32) -> DaemonConfig {
        DaemonConfig {
            auto_cleanup: true,
            auto_cleanup_keep: CleanupRetention::Count(n),
            ..DaemonConfig::default()
        }
    }

    #[test]
    fn workspace_policy_empty_toml_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        save_workspace_policy(dir.path(), &WorkspacePolicy::default()).unwrap();
        match load_workspace_policy(dir.path()).unwrap() {
            LoadPolicyOutcome::Loaded(p) => {
                assert!(p.is_empty());
                assert_eq!(p, WorkspacePolicy::default());
            }
            LoadPolicyOutcome::Missing => panic!("file was just written, must be Loaded"),
        }
    }

    #[test]
    fn workspace_policy_partial_toml_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: None,
        };
        save_workspace_policy(dir.path(), &p).unwrap();
        let loaded = load_workspace_policy_or_default(dir.path()).unwrap();
        assert_eq!(loaded, p);
        assert!(!loaded.is_empty());
    }

    #[test]
    fn workspace_policy_age_mode_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: Some(CleanupRetention::age("30d").unwrap()),
        };
        save_workspace_policy(dir.path(), &p).unwrap();
        let loaded = load_workspace_policy_or_default(dir.path()).unwrap();
        assert_eq!(loaded, p);
    }

    #[test]
    fn workspace_policy_count_mode_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = WorkspacePolicy {
            auto_cleanup: Some(false),
            auto_cleanup_keep: Some(CleanupRetention::Count(5)),
        };
        save_workspace_policy(dir.path(), &p).unwrap();
        let loaded = load_workspace_policy_or_default(dir.path()).unwrap();
        assert_eq!(loaded, p);
    }

    #[test]
    fn load_workspace_policy_missing_file_returns_missing() {
        let dir = tempfile::tempdir().unwrap();
        match load_workspace_policy(dir.path()).unwrap() {
            LoadPolicyOutcome::Missing => {} // expected — fresh dir, no file
            LoadPolicyOutcome::Loaded(_) => panic!("empty dir must report Missing"),
        }
        // Convenience wrapper collapses Missing → default for fresh-startup
        // call sites that have no in-memory state to preserve.
        assert!(load_workspace_policy_or_default(dir.path())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn delete_workspace_policy_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        // No file yet — delete must succeed silently.
        delete_workspace_policy(dir.path()).unwrap();
        // Write, then delete.
        save_workspace_policy(
            dir.path(),
            &WorkspacePolicy {
                auto_cleanup: Some(true),
                auto_cleanup_keep: None,
            },
        )
        .unwrap();
        delete_workspace_policy(dir.path()).unwrap();
        // After delete the load must report Missing (not "Loaded(empty)") —
        // that distinction is what lets reload preserve in-memory state on
        // a transient EBUSY.
        assert!(matches!(
            load_workspace_policy(dir.path()).unwrap(),
            LoadPolicyOutcome::Missing
        ));
    }

    #[test]
    fn effective_for_local_some_overrides_global() {
        let global = global_count(20);
        let local = WorkspacePolicy {
            auto_cleanup: Some(false),
            auto_cleanup_keep: Some(CleanupRetention::Count(5)),
        };
        let eff = local.effective_for(&global);
        assert!(!eff.auto_cleanup);
        assert_eq!(eff.auto_cleanup_keep, CleanupRetention::Count(5));
    }

    #[test]
    fn effective_for_local_none_inherits_global() {
        let global = global_count(20);
        let local = WorkspacePolicy::default();
        let eff = local.effective_for(&global);
        assert_eq!(eff.auto_cleanup, global.auto_cleanup);
        assert_eq!(eff.auto_cleanup_keep, global.auto_cleanup_keep);
    }

    #[test]
    fn effective_for_local_partial_only_overrides_set_fields() {
        let global = global_count(20);
        let local = WorkspacePolicy {
            auto_cleanup: None,
            auto_cleanup_keep: Some(CleanupRetention::Count(7)),
        };
        let eff = local.effective_for(&global);
        // auto_cleanup is None ⇒ inherit
        assert_eq!(eff.auto_cleanup, global.auto_cleanup);
        // auto_cleanup_keep is Some ⇒ override
        assert_eq!(eff.auto_cleanup_keep, CleanupRetention::Count(7));
    }

    #[test]
    fn effective_policy_is_disabled_propagates() {
        let global = global_count(20);
        // Local sets auto_cleanup=false ⇒ disabled regardless of keep.
        let local = WorkspacePolicy {
            auto_cleanup: Some(false),
            auto_cleanup_keep: Some(CleanupRetention::Count(10)),
        };
        assert!(local.effective_for(&global).is_disabled());
        // Local sets keep=Count(0) ⇒ disabled even if auto_cleanup=true.
        let local = WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: Some(CleanupRetention::Count(0)),
        };
        assert!(local.effective_for(&global).is_disabled());
        // Local enables on top of disabled global.
        let mut g_off = global.clone();
        g_off.auto_cleanup = false;
        let local = WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: None,
        };
        assert!(!local.effective_for(&g_off).is_disabled());
    }

    #[test]
    fn workspace_policy_reset_request_round_trip() {
        let req = Request::ResetWorkspacePolicy {
            workspace: "/ws".to_string(),
        };
        match round_trip_request(&req) {
            Request::ResetWorkspacePolicy { workspace } => assert_eq!(workspace, "/ws"),
            _ => panic!("expected ResetWorkspacePolicy"),
        }
    }

    #[test]
    fn workspace_policy_get_request_round_trip() {
        let req = Request::GetWorkspacePolicy {
            workspace: "/ws".to_string(),
        };
        match round_trip_request(&req) {
            Request::GetWorkspacePolicy { workspace } => assert_eq!(workspace, "/ws"),
            _ => panic!("expected GetWorkspacePolicy"),
        }
    }

    #[test]
    fn policy_field_op_apply_semantics() {
        assert_eq!(
            PolicyFieldOp::<bool>::Unchanged.apply(Some(true)),
            Some(true)
        );
        assert_eq!(PolicyFieldOp::<bool>::Unchanged.apply(None), None);
        assert_eq!(
            PolicyFieldOp::<bool>::Set(false).apply(Some(true)),
            Some(false)
        );
        assert_eq!(PolicyFieldOp::<bool>::Set(true).apply(None), Some(true));
    }

    #[test]
    fn workspace_policy_patch_request_round_trip() {
        let req = Request::PatchWorkspacePolicy {
            workspace: "/ws".to_string(),
            auto_cleanup: PolicyFieldOp::Set(true),
            auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::age("7d").unwrap()),
        };
        let decoded = round_trip_request(&req);
        match decoded {
            Request::PatchWorkspacePolicy {
                workspace,
                auto_cleanup,
                auto_cleanup_keep,
            } => {
                assert_eq!(workspace, "/ws");
                assert_eq!(auto_cleanup, PolicyFieldOp::Set(true));
                assert!(matches!(
                    auto_cleanup_keep,
                    PolicyFieldOp::Set(CleanupRetention::Age { .. })
                ));
            }
            _ => panic!("expected PatchWorkspacePolicy"),
        }
    }

    #[test]
    fn workspace_policy_patch_request_mixed_round_trip() {
        // One field Set, one Unchanged — covers both variants on the wire.
        let req = Request::PatchWorkspacePolicy {
            workspace: "/ws".to_string(),
            auto_cleanup: PolicyFieldOp::Unchanged,
            auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(3)),
        };
        match round_trip_request(&req) {
            Request::PatchWorkspacePolicy {
                auto_cleanup,
                auto_cleanup_keep,
                ..
            } => {
                assert_eq!(auto_cleanup, PolicyFieldOp::<bool>::Unchanged);
                assert_eq!(
                    auto_cleanup_keep,
                    PolicyFieldOp::Set(CleanupRetention::Count(3))
                );
            }
            _ => panic!("expected PatchWorkspacePolicy"),
        }
    }

    #[test]
    fn workspace_policy_response_round_trip() {
        let resp = Response::WorkspacePolicyOk {
            ws_id: "ws-abc".to_string(),
            effective: EffectivePolicy {
                auto_cleanup: true,
                auto_cleanup_keep: CleanupRetention::Count(5),
            },
            local: WorkspacePolicy {
                auto_cleanup: None,
                auto_cleanup_keep: Some(CleanupRetention::Count(5)),
            },
            global: GlobalPolicySnapshot {
                auto_cleanup: true,
                auto_cleanup_keep: CleanupRetention::Count(20),
            },
        };
        match round_trip_response(&resp) {
            Response::WorkspacePolicyOk {
                ws_id,
                effective,
                local,
                global,
            } => {
                assert_eq!(ws_id, "ws-abc");
                assert!(effective.auto_cleanup);
                assert_eq!(effective.auto_cleanup_keep, CleanupRetention::Count(5));
                assert_eq!(local.auto_cleanup_keep, Some(CleanupRetention::Count(5)));
                assert_eq!(global.auto_cleanup_keep, CleanupRetention::Count(20));
            }
            _ => panic!("expected WorkspacePolicyOk"),
        }
    }

    #[test]
    fn response_health_advisory_ok_zero_round_trip() {
        // Backend query failed or no workspace over limit: all zeros.
        let resp = Response::HealthAdvisoryOk {
            over_limit_workspace_count: 0,
            fs_total_bytes: 0,
            fs_used_bytes: 0,
        };
        let decoded = round_trip_response(&resp);
        match decoded {
            Response::HealthAdvisoryOk {
                over_limit_workspace_count,
                fs_total_bytes,
                fs_used_bytes,
            } => {
                assert_eq!(over_limit_workspace_count, 0);
                assert_eq!(fs_total_bytes, 0);
                assert_eq!(fs_used_bytes, 0);
            }
            _ => panic!("expected HealthAdvisoryOk variant"),
        }
    }

    // ── unlink_node tests ──

    fn meta(parent: Option<&str>, children: Vec<&str>) -> SnapshotMeta {
        SnapshotMeta {
            message: None,
            metadata: None,
            pinned: false,
            created_at: chrono::Utc::now(),
            missing: false,
            parent_id: parent.map(|s| s.to_string()),
            child_ids: children.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn unlink_node_reparents_children() {
        // A←B←C(head+live), delete B → C.parent=A, A.child_ids=[C], head unchanged
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert("a".into(), meta(None, vec!["b"]));
        idx.snapshots.insert("b".into(), meta(Some("a"), vec!["c"]));
        idx.snapshots
            .insert("c".into(), meta(Some("b"), vec![LIVE_CHILD]));
        idx.head = Some("c".into());

        idx.unlink_node("b");

        assert_eq!(idx.snapshots["c"].parent_id.as_deref(), Some("a"));
        assert_eq!(idx.snapshots["a"].child_ids, vec!["c"]);
        assert_eq!(idx.head.as_deref(), Some("c"));
    }

    #[test]
    fn unlink_node_updates_head_and_migrates_live() {
        // A←B←C(head+live), delete C → head=B, B gets LIVE_CHILD
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert("a".into(), meta(None, vec!["b"]));
        idx.snapshots.insert("b".into(), meta(Some("a"), vec!["c"]));
        idx.snapshots
            .insert("c".into(), meta(Some("b"), vec![LIVE_CHILD]));
        idx.head = Some("c".into());

        idx.unlink_node("c");

        assert_eq!(idx.head.as_deref(), Some("b"));
        assert!(idx.snapshots["b"]
            .child_ids
            .contains(&LIVE_CHILD.to_string()));
    }

    #[test]
    fn unlink_node_branching() {
        // A←B, A←C, delete A → B.parent=None, C.parent=None (forest)
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert("a".into(), meta(None, vec!["b", "c"]));
        idx.snapshots.insert("b".into(), meta(Some("a"), vec![]));
        idx.snapshots.insert("c".into(), meta(Some("a"), vec![]));
        idx.head = Some("b".into());

        idx.unlink_node("a");

        assert_eq!(idx.snapshots["b"].parent_id, None);
        assert_eq!(idx.snapshots["c"].parent_id, None);
    }

    #[test]
    fn unlink_node_root() {
        // A(root)←B(head+live), delete A → B.parent=None, head unchanged
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert("a".into(), meta(None, vec!["b"]));
        idx.snapshots
            .insert("b".into(), meta(Some("a"), vec![LIVE_CHILD]));
        idx.head = Some("b".into());

        idx.unlink_node("a");

        assert_eq!(idx.snapshots["b"].parent_id, None);
        assert_eq!(idx.head.as_deref(), Some("b"));
    }

    // ── prune_chain tests ──

    #[test]
    fn prune_chain_tail() {
        // A←B←C←D←E(head+live), delete {A,B} → C.parent=None, head unchanged
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert("a".into(), meta(None, vec!["b"]));
        idx.snapshots.insert("b".into(), meta(Some("a"), vec!["c"]));
        idx.snapshots.insert("c".into(), meta(Some("b"), vec!["d"]));
        idx.snapshots.insert("d".into(), meta(Some("c"), vec!["e"]));
        idx.snapshots
            .insert("e".into(), meta(Some("d"), vec![LIVE_CHILD]));
        idx.head = Some("e".into());

        let ids: HashSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        idx.prune_chain(&ids);

        assert_eq!(idx.snapshots["c"].parent_id, None);
        assert_eq!(idx.head.as_deref(), Some("e"));
    }

    #[test]
    fn prune_chain_includes_head() {
        // A←B←C(head+live), delete {B,C} → head=A, A gets LIVE_CHILD
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert("a".into(), meta(None, vec!["b"]));
        idx.snapshots.insert("b".into(), meta(Some("a"), vec!["c"]));
        idx.snapshots
            .insert("c".into(), meta(Some("b"), vec![LIVE_CHILD]));
        idx.head = Some("c".into());

        let ids: HashSet<String> = ["b", "c"].iter().map(|s| s.to_string()).collect();
        idx.prune_chain(&ids);

        assert_eq!(idx.head.as_deref(), Some("a"));
        let a_children = &idx.snapshots["a"].child_ids;
        assert!(a_children.contains(&LIVE_CHILD.to_string()));
        assert!(!a_children.contains(&"b".to_string()));
        assert_eq!(a_children.len(), 1);
    }

    #[test]
    fn prune_chain_skip_middle() {
        // A←B←C←D(head+live), delete {B,C} → D.parent=A, A.child_ids=[D]
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert("a".into(), meta(None, vec!["b"]));
        idx.snapshots.insert("b".into(), meta(Some("a"), vec!["c"]));
        idx.snapshots.insert("c".into(), meta(Some("b"), vec!["d"]));
        idx.snapshots
            .insert("d".into(), meta(Some("c"), vec![LIVE_CHILD]));
        idx.head = Some("d".into());

        let ids: HashSet<String> = ["b", "c"].iter().map(|s| s.to_string()).collect();
        idx.prune_chain(&ids);

        assert_eq!(idx.snapshots["d"].parent_id.as_deref(), Some("a"));
        assert!(idx.snapshots["a"].child_ids.contains(&"d".to_string()));
        assert!(!idx.snapshots["a"].child_ids.iter().any(|c| ids.contains(c)));
    }

    #[test]
    fn prune_chain_creates_forest() {
        // a←b←c←d→{e←f, g←h←i, j←k→{l, m(head+live)←n}}
        // delete {a,b,c,d,e,g,j} → f,h,k parent=None (3 trees)
        let mut idx = SnapshotIndex::new(PathBuf::from("/ws"));
        idx.snapshots.insert("a".into(), meta(None, vec!["b"]));
        idx.snapshots.insert("b".into(), meta(Some("a"), vec!["c"]));
        idx.snapshots.insert("c".into(), meta(Some("b"), vec!["d"]));
        idx.snapshots
            .insert("d".into(), meta(Some("c"), vec!["e", "g", "j"]));
        idx.snapshots.insert("e".into(), meta(Some("d"), vec!["f"]));
        idx.snapshots.insert("f".into(), meta(Some("e"), vec![]));
        idx.snapshots.insert("g".into(), meta(Some("d"), vec!["h"]));
        idx.snapshots.insert("h".into(), meta(Some("g"), vec!["i"]));
        idx.snapshots.insert("i".into(), meta(Some("h"), vec![]));
        idx.snapshots.insert("j".into(), meta(Some("d"), vec!["k"]));
        idx.snapshots
            .insert("k".into(), meta(Some("j"), vec!["l", "m"]));
        idx.snapshots.insert("l".into(), meta(Some("k"), vec![]));
        idx.snapshots
            .insert("m".into(), meta(Some("k"), vec![LIVE_CHILD, "n"]));
        idx.snapshots.insert("n".into(), meta(Some("m"), vec![]));
        idx.head = Some("m".into());

        let ids: HashSet<String> = ["a", "b", "c", "d", "e", "g", "j"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        idx.prune_chain(&ids);

        assert_eq!(idx.snapshots["f"].parent_id, None);
        assert_eq!(idx.snapshots["h"].parent_id, None);
        assert_eq!(idx.snapshots["k"].parent_id, None);
        assert_eq!(idx.head.as_deref(), Some("m"));
        assert!(idx.snapshots["n"].parent_id.as_deref() == Some("m"));
    }

    #[test]
    fn orphan_query_preserves_legacy_list_wire_layout() {
        let old = Request::List {
            workspace: Some("/ws".into()),
            format: Some("json".into()),
        };
        assert_eq!(
            bincode::serialize(&old).unwrap(),
            bincode::serialize(&(4_u32, Some("/ws"), Some("json"))).unwrap()
        );
        let request = Request::ListOrphans {
            workspace: Some("/ws".into()),
        };
        let encoded = bincode::serialize(&request).unwrap();
        assert_eq!(encoded, bincode::serialize(&(28_u32, Some("/ws"))).unwrap());
        assert!(matches!(bincode::deserialize::<Request>(&encoded).unwrap(),
            Request::ListOrphans { workspace: Some(ws) } if ws == "/ws"));
    }

    #[test]
    fn recovery_protocol_extensions_preserve_existing_wire_layout() {
        let preview = RecoveryPreview {
            ws_id: Some("ws-preview".into()),
            registration_path: "/ws".into(),
            snapshot_count: 7,
            confirmation_digest: [9; 32],
        };
        let requests = [
            Request::Recover {
                workspace: "/ws".into(),
            },
            Request::Unregister {
                workspace: "/ws".into(),
            },
            Request::RecoverPreview {
                workspace: "/alias/ws".into(),
            },
            Request::RecoverConfirmed {
                preview: preview.clone(),
            },
        ];
        for (request, tag) in requests.iter().zip([13_u32, 25, 26, 27]) {
            let encoded = bincode::serialize(request).unwrap();
            assert_eq!(&encoded[..4], &tag.to_le_bytes());
            let decoded: Request = bincode::deserialize(&encoded).unwrap();
            assert_eq!(encoded, bincode::serialize(&decoded).unwrap());
        }
        let responses = [
            Response::RecoverOk {
                workspace: "/ws".into(),
            },
            Response::RecoverWithWarning {
                workspace: "/ws".into(),
                warning: "retained".into(),
            },
            Response::UnregisterOk {
                workspace: "/ws".into(),
                retained_paths: vec!["/backup".into()],
            },
            Response::RecoverPreviewOk { preview },
        ];
        for (response, tag) in responses.iter().zip([12_u32, 26, 27, 28]) {
            let encoded = bincode::serialize(response).unwrap();
            assert_eq!(&encoded[..4], &tag.to_le_bytes());
            let decoded: Response = bincode::deserialize(&encoded).unwrap();
            assert_eq!(encoded, bincode::serialize(&decoded).unwrap());
        }
        // A pre-extension RecoverOk payload is exactly its tag and string.
        assert_eq!(
            bincode::serialize(&responses[0]).unwrap(),
            bincode::serialize(&(12_u32, "/ws")).unwrap()
        );
    }

    #[test]
    fn v2_request_discriminants_are_append_only_and_round_trip() {
        let generation = WorkspaceGenerationTokenV2::from_bytes([0x11; 32]);
        let requests = [
            Request::WorkspaceIdentityV2 {
                registration_path: "/workspace".to_string(),
            },
            Request::GuardedCheckpointV2 {
                ws_id: "ws-abcdef-2".to_string(),
                expected_generation: generation,
                checkpoint_id: "turn-42".to_string(),
                operation_digest: [0x22; 32],
                message: Some("checkpoint".to_string()),
                metadata: Some(r#"{"turn":42}"#.to_string()),
                pin: true,
            },
            Request::CheckpointEvidenceV2 {
                ws_id: "ws-abcdef-2".to_string(),
                expected_generation: generation,
                checkpoint_id: "turn-42".to_string(),
                operation_digest: [0x22; 32],
            },
            Request::GuardedRollbackPreviewV2 {
                registered_path: "/workspace".to_string(),
                ws_id: "ws-abcdef-2".to_string(),
                expected_generation: generation,
                target_snapshot_id: "turn-41".to_string(),
            },
            Request::GuardedRollbackV2 {
                registered_path: "/workspace".to_string(),
                ws_id: "ws-abcdef-2".to_string(),
                expected_generation: generation,
                target_snapshot_id: "turn-41".to_string(),
                expected_diff_digest: [0x33; 32],
                operation_id: "switch-42".to_string(),
                operation_digest: [0x44; 32],
            },
            Request::GuardedRollbackEvidenceV2 {
                ws_id: "ws-abcdef-2".to_string(),
                operation_id: "switch-42".to_string(),
                operation_digest: [0x44; 32],
            },
        ];

        for (request, expected_discriminant) in requests.iter().zip(19_u32..=24) {
            let encoded = bincode::serialize(request).unwrap();
            assert_eq!(&encoded[..4], &expected_discriminant.to_le_bytes());
            let decoded: Request = bincode::deserialize(&encoded).unwrap();
            assert_eq!(bincode::serialize(&decoded).unwrap(), encoded);
        }

        let legacy = bincode::serialize(&Request::RollbackPreview {
            workspace: "/workspace".to_string(),
            to: None,
            num_ancestors: Some(1),
        })
        .unwrap();
        assert_eq!(&legacy[..4], &18_u32.to_le_bytes());
    }

    #[test]
    fn v2_response_discriminants_are_append_only_and_round_trip() {
        let evidence = GuardedCheckpointEvidenceV2 {
            ws_id: "ws-abcdef".to_string(),
            registered_path: "/workspace".to_string(),
            generation: WorkspaceGenerationTokenV2::from_bytes([0x33; 32]),
            checkpoint_id: "turn-42".to_string(),
            operation_digest: [0x44; 32],
            caller_uid: 1000,
            outcome: GuardedCheckpointOutcomeV2::Created {
                snapshot_id: "turn-42".to_string(),
            },
        };
        let rollback_evidence = GuardedRollbackEvidenceV2 {
            ws_id: "ws-abcdef".to_string(),
            registered_path: "/workspace".to_string(),
            expected_generation: evidence.generation,
            target_snapshot_id: "turn-41".to_string(),
            expected_diff_digest: [0x55; 32],
            operation_id: "switch-42".to_string(),
            operation_digest: [0x66; 32],
            caller_uid: 1000,
            outcome: GuardedRollbackOutcomeV2::Started,
        };
        let responses = [
            Response::WorkspaceIdentityV2Ok {
                protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
                ws_id: evidence.ws_id.clone(),
                registered_path: evidence.registered_path.clone(),
                generation: evidence.generation,
            },
            Response::GuardedCheckpointV2Ok {
                evidence: evidence.clone(),
            },
            Response::CheckpointEvidenceV2Ok {
                evidence: Some(evidence.clone()),
            },
            Response::GuardedCheckpointV2Rejected {
                code: GuardedCheckpointRejectionCodeV2::GenerationMismatch,
                message: "workspace generation changed".to_string(),
            },
            Response::GuardedRollbackPreviewV2Ok {
                protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
                registered_path: "/workspace".to_string(),
                ws_id: "ws-abcdef".to_string(),
                generation: evidence.generation,
                target_snapshot_id: "turn-41".to_string(),
                diff_digest: [0x55; 32],
                changes: vec![],
                caller_uid: 1000,
            },
            Response::GuardedRollbackV2Ok {
                evidence: GuardedRollbackEvidenceV2 {
                    outcome: GuardedRollbackOutcomeV2::Succeeded {
                        resulting_generation: WorkspaceGenerationTokenV2::from_bytes([0x77; 32]),
                    },
                    ..rollback_evidence.clone()
                },
            },
            Response::GuardedRollbackV2Uncertain {
                evidence: rollback_evidence.clone(),
            },
            Response::GuardedRollbackEvidenceV2Ok {
                evidence: Some(rollback_evidence),
            },
            Response::GuardedRollbackV2Rejected {
                code: GuardedRollbackRejectionCodeV2::DiffMismatch,
                message: "live diff changed".to_string(),
            },
        ];

        for (response, expected_discriminant) in responses.iter().zip(17_u32..=25) {
            let encoded = bincode::serialize(response).unwrap();
            assert_eq!(&encoded[..4], &expected_discriminant.to_le_bytes());
            let decoded: Response = bincode::deserialize(&encoded).unwrap();
            assert_eq!(bincode::serialize(&decoded).unwrap(), encoded);
        }

        let legacy = bincode::serialize(&Response::RollbackPreviewOk {
            to: "turn-41".to_string(),
            changes: vec![],
        })
        .unwrap();
        assert_eq!(&legacy[..4], &16_u32.to_le_bytes());
        assert_eq!(
            bincode::serialize(&ErrorCode::CwdScanFailed).unwrap(),
            12_u32.to_le_bytes()
        );
    }

    #[test]
    fn v2_supporting_enum_discriminants_are_frozen() {
        let rejection_codes = [
            GuardedCheckpointRejectionCodeV2::DaemonNotReady,
            GuardedCheckpointRejectionCodeV2::PeerCredentialsUnavailable,
            GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath,
            GuardedCheckpointRejectionCodeV2::InvalidWorkspaceId,
            GuardedCheckpointRejectionCodeV2::InvalidCheckpointId,
            GuardedCheckpointRejectionCodeV2::InvalidMetadata,
            GuardedCheckpointRejectionCodeV2::WorkspaceNotFound,
            GuardedCheckpointRejectionCodeV2::GenerationMismatch,
            GuardedCheckpointRejectionCodeV2::OperationConflict,
            GuardedCheckpointRejectionCodeV2::WriteLockConflict,
            GuardedCheckpointRejectionCodeV2::CallerMismatch,
            GuardedCheckpointRejectionCodeV2::EvidenceCapacityReached,
        ];
        for (code, expected_discriminant) in rejection_codes.iter().zip(0_u32..=11) {
            assert_eq!(
                bincode::serialize(code).unwrap(),
                expected_discriminant.to_le_bytes()
            );
        }

        let created = GuardedCheckpointOutcomeV2::Created {
            snapshot_id: "turn-42".to_string(),
        };
        let skipped = GuardedCheckpointOutcomeV2::Skipped {
            reason: "write lock active".to_string(),
        };
        assert_eq!(
            &bincode::serialize(&created).unwrap()[..4],
            &0_u32.to_le_bytes()
        );
        assert_eq!(
            &bincode::serialize(&skipped).unwrap()[..4],
            &1_u32.to_le_bytes()
        );

        let token = WorkspaceGenerationTokenV2::from_bytes([0x55; 32]);
        assert_eq!(bincode::serialize(&token).unwrap(), vec![0x55; 32]);
        assert_eq!(token.into_bytes(), [0x55; 32]);

        let rollback_rejections = [
            GuardedRollbackRejectionCodeV2::DaemonNotReady,
            GuardedRollbackRejectionCodeV2::PeerCredentialsUnavailable,
            GuardedRollbackRejectionCodeV2::InvalidRegistrationPath,
            GuardedRollbackRejectionCodeV2::InvalidWorkspaceId,
            GuardedRollbackRejectionCodeV2::InvalidSnapshotId,
            GuardedRollbackRejectionCodeV2::InvalidOperationId,
            GuardedRollbackRejectionCodeV2::WorkspaceNotFound,
            GuardedRollbackRejectionCodeV2::SnapshotNotFound,
            GuardedRollbackRejectionCodeV2::GenerationMismatch,
            GuardedRollbackRejectionCodeV2::DiffMismatch,
            GuardedRollbackRejectionCodeV2::OperationConflict,
            GuardedRollbackRejectionCodeV2::WriteLockConflict,
            GuardedRollbackRejectionCodeV2::CallerMismatch,
            GuardedRollbackRejectionCodeV2::EvidenceCapacityReached,
            GuardedRollbackRejectionCodeV2::CwdOccupied,
            GuardedRollbackRejectionCodeV2::CwdScanFailed,
        ];
        for (code, expected_discriminant) in rollback_rejections.iter().zip(0_u32..=15) {
            assert_eq!(
                bincode::serialize(code).unwrap(),
                expected_discriminant.to_le_bytes()
            );
        }
    }

    #[test]
    fn old_snapshot_index_json_loads_with_empty_governed_evidence() {
        let old_json = r#"{
            "workspace_path": "/workspace",
            "snapshots": {},
            "head": null
        }"#;

        let index: SnapshotIndex = serde_json::from_str(old_json).unwrap();
        assert_eq!(index.workspace_path, PathBuf::from("/workspace"));
        assert!(index.governed_evidence.is_empty());
        assert!(index.guarded_rollbacks.is_empty());
        assert!(index.recovered_orphans.is_empty());
    }

    #[test]
    fn v2_workspace_id_validator_requires_canonical_form() {
        for valid in ["ws-012abc", "ws-abcdef-2", "ws-abcdef-42"] {
            assert!(validate_workspace_id_v2(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "abcdef",
            "ws-abcde",
            "ws-ABCDEf",
            "ws-abcdef-0",
            "ws-abcdef-1",
            "ws-abcdef-02",
            "ws-abcdef-2-extra",
        ] {
            assert!(validate_workspace_id_v2(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn v2_checkpoint_id_validator_accepts_only_safe_non_reserved_components() {
        for valid in ["turn-42", "checkpoint_1", "ckpt.2026"] {
            assert!(validate_checkpoint_id_v2(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            ".",
            "..",
            LIVE_CHILD,
            "with space",
            "../escape",
            "slash/path",
        ] {
            assert!(validate_checkpoint_id_v2(invalid).is_err(), "{invalid}");
        }
        assert!(validate_checkpoint_id_v2(&"a".repeat(GUARDED_CHECKPOINT_ID_MAX_BYTES_V2)).is_ok());
        assert!(
            validate_checkpoint_id_v2(&"a".repeat(GUARDED_CHECKPOINT_ID_MAX_BYTES_V2 + 1)).is_err()
        );
    }
}
