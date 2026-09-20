//! Byte-level golden lock for the ws-ckpt IPC wire protocol.
//!
//! The golden fixture was captured while these types were verified
//! byte-identical to the authoritative `ws-ckpt-common` crate, so any
//! local edit that changes the bincode layout (variant reorder, field
//! reorder, type change) fails against the recorded bytes without
//! needing a cross-workspace dev-dependency.
//!
//! Regenerate only for intentional protocol changes coordinated with
//! the ws-ckpt daemon:
//!
//! ```text
//! UPDATE_WIRE_GOLDENS=1 cargo test -p cosh-types --test checkpoint_wire_conformance
//! ```

use chrono::{TimeZone, Utc};
use cosh_types::checkpoint as local;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

const PROTOCOL_VERSION: u64 = 2;
const REGEN_ENV: &str = "UPDATE_WIRE_GOLDENS";

#[derive(Serialize, Deserialize)]
struct Fixture {
    protocol_version: u64,
    cases: Vec<FixtureCase>,
}

#[derive(Serialize, Deserialize)]
struct FixtureCase {
    name: String,
    hex: String,
}

fn fixture_path() -> PathBuf {
    // Derive the file name from PROTOCOL_VERSION so a version bump cannot
    // leave the constant and the fixture file out of sync.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "tests/fixtures/checkpoint-wire-v{PROTOCOL_VERSION}.json"
    ))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(name: &str, hex: &str) -> Vec<u8> {
    assert!(
        hex.len().is_multiple_of(2),
        "case {name}: odd-length hex string"
    );
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .unwrap_or_else(|e| panic!("case {name}: invalid hex byte at offset {i}: {e}"))
        })
        .collect()
}

// The exhaustive matches below are the compile-time drift guard: adding,
// removing, or renaming a variant on any wire enum forces an update here,
// which in turn forces a deliberate fixture regeneration.

fn request_case_name(value: &local::WsCkptRequest) -> &'static str {
    match value {
        local::WsCkptRequest::Init { .. } => "request/init",
        local::WsCkptRequest::Checkpoint { .. } => "request/checkpoint",
        local::WsCkptRequest::Rollback { .. } => "request/rollback",
        local::WsCkptRequest::Delete { .. } => "request/delete",
        local::WsCkptRequest::List { .. } => "request/list",
        local::WsCkptRequest::Diff { .. } => "request/diff",
        local::WsCkptRequest::Status { .. } => "request/status",
        local::WsCkptRequest::Cleanup { .. } => "request/cleanup",
        local::WsCkptRequest::Config => "request/config",
        local::WsCkptRequest::ReloadConfig => "request/reload_config",
        local::WsCkptRequest::ReloadGlobalConfig => "request/reload_global_config",
        local::WsCkptRequest::ReloadWorkspacePolicy { .. } => "request/reload_workspace_policy",
        local::WsCkptRequest::ConfigOverview => "request/config_overview",
        local::WsCkptRequest::Recover { .. } => "request/recover",
        local::WsCkptRequest::HealthAdvisory => "request/health_advisory",
        local::WsCkptRequest::GetWorkspacePolicy { .. } => "request/get_workspace_policy",
        local::WsCkptRequest::ResetWorkspacePolicy { .. } => "request/reset_workspace_policy",
        local::WsCkptRequest::PatchWorkspacePolicy { .. } => "request/patch_workspace_policy",
        local::WsCkptRequest::RollbackPreview { .. } => "request/rollback_preview",
        local::WsCkptRequest::WorkspaceIdentityV2 { .. } => "request/workspace_identity_v2",
        local::WsCkptRequest::GuardedCheckpointV2 { .. } => "request/guarded_checkpoint_v2",
        local::WsCkptRequest::CheckpointEvidenceV2 { .. } => "request/checkpoint_evidence_v2",
        local::WsCkptRequest::GuardedRollbackPreviewV2 { .. } => {
            "request/guarded_rollback_preview_v2"
        }
        local::WsCkptRequest::GuardedRollbackV2 { .. } => "request/guarded_rollback_v2",
        local::WsCkptRequest::GuardedRollbackEvidenceV2 { .. } => {
            "request/guarded_rollback_evidence_v2"
        }
        local::WsCkptRequest::Unregister { .. } => "request/unregister",
    }
}

fn error_case_name(value: &local::WsCkptErrorCode) -> &'static str {
    match value {
        local::WsCkptErrorCode::WorkspaceNotFound => "error/workspace_not_found",
        local::WsCkptErrorCode::SnapshotNotFound => "error/snapshot_not_found",
        local::WsCkptErrorCode::AlreadyInitialized => "error/already_initialized",
        local::WsCkptErrorCode::BtrfsError => "error/btrfs_error",
        local::WsCkptErrorCode::IoError => "error/io_error",
        local::WsCkptErrorCode::InvalidPath => "error/invalid_path",
        local::WsCkptErrorCode::ConfirmationRequired => "error/confirmation_required",
        local::WsCkptErrorCode::InternalError => "error/internal_error",
        local::WsCkptErrorCode::SnapshotAlreadyExists => "error/snapshot_already_exists",
        local::WsCkptErrorCode::WriteLockConflict => "error/write_lock_conflict",
        local::WsCkptErrorCode::DiskSpaceInsufficient => "error/disk_space_insufficient",
        local::WsCkptErrorCode::CwdOccupied => "error/cwd_occupied",
        local::WsCkptErrorCode::CwdScanFailed => "error/cwd_scan_failed",
    }
}

fn response_case_name(value: &local::WsCkptResponse) -> &'static str {
    match value {
        local::WsCkptResponse::InitOk { .. } => "response/init_ok",
        local::WsCkptResponse::CheckpointOk { .. } => "response/checkpoint_ok",
        local::WsCkptResponse::RollbackOk { .. } => "response/rollback_ok",
        local::WsCkptResponse::DeleteOk { .. } => "response/delete_ok",
        local::WsCkptResponse::Error { .. } => "response/error",
        local::WsCkptResponse::ListOk { .. } => "response/list_ok",
        local::WsCkptResponse::DiffOk { .. } => "response/diff_ok",
        local::WsCkptResponse::StatusOk { .. } => "response/status_ok",
        local::WsCkptResponse::CleanupOk { .. } => "response/cleanup_ok",
        local::WsCkptResponse::ConfigOk { .. } => "response/config_ok",
        local::WsCkptResponse::ReloadConfigOk { .. } => "response/reload_config_ok",
        local::WsCkptResponse::CheckpointSkipped { .. } => "response/checkpoint_skipped",
        local::WsCkptResponse::RecoverOk { .. } => "response/recover_ok",
        local::WsCkptResponse::HealthAdvisoryOk { .. } => "response/health_advisory_ok",
        local::WsCkptResponse::WorkspacePolicyOk { .. } => "response/workspace_policy_ok",
        local::WsCkptResponse::ConfigOverviewOk { .. } => "response/config_overview_ok",
        local::WsCkptResponse::RollbackPreviewOk { .. } => "response/rollback_preview_ok",
        local::WsCkptResponse::WorkspaceIdentityV2Ok { .. } => "response/workspace_identity_v2_ok",
        local::WsCkptResponse::GuardedCheckpointV2Ok { .. } => "response/guarded_checkpoint_v2_ok",
        local::WsCkptResponse::CheckpointEvidenceV2Ok { .. } => {
            "response/checkpoint_evidence_v2_ok"
        }
        local::WsCkptResponse::GuardedCheckpointV2Rejected { .. } => {
            "response/guarded_checkpoint_v2_rejected"
        }
        local::WsCkptResponse::GuardedRollbackPreviewV2Ok { .. } => {
            "response/guarded_rollback_preview_v2_ok"
        }
        local::WsCkptResponse::GuardedRollbackV2Ok { .. } => "response/guarded_rollback_v2_ok",
        local::WsCkptResponse::GuardedRollbackV2Uncertain { .. } => {
            "response/guarded_rollback_v2_uncertain"
        }
        local::WsCkptResponse::GuardedRollbackEvidenceV2Ok { .. } => {
            "response/guarded_rollback_evidence_v2_ok"
        }
        local::WsCkptResponse::GuardedRollbackV2Rejected { .. } => {
            "response/guarded_rollback_v2_rejected"
        }
        local::WsCkptResponse::RecoverWithWarning { .. } => "response/recover_with_warning",
        local::WsCkptResponse::UnregisterOk { .. } => "response/unregister_ok",
    }
}

fn request_cases() -> Vec<(&'static str, local::WsCkptRequest)> {
    let samples = vec![
        local::WsCkptRequest::Init {
            workspace: "/ws".into(),
        },
        local::WsCkptRequest::Checkpoint {
            workspace: "/ws".into(),
            id: "s1".into(),
            message: Some("m".into()),
            metadata: Some("{\"k\":1}".into()),
            pin: true,
        },
        local::WsCkptRequest::Rollback {
            workspace: "/ws".into(),
            to: Some("s1".into()),
            num_ancestors: Some(2),
        },
        local::WsCkptRequest::Delete {
            workspace: Some("/ws".into()),
            snapshot: "s1".into(),
            force: true,
        },
        local::WsCkptRequest::List {
            workspace: Some("/ws".into()),
            format: Some("json".into()),
        },
        local::WsCkptRequest::Diff {
            workspace: "/ws".into(),
            from: "s1".into(),
            to: Some("s2".into()),
        },
        local::WsCkptRequest::Status {
            workspace: Some("/ws".into()),
        },
        local::WsCkptRequest::Cleanup {
            workspace: "/ws".into(),
            keep: Some(3),
        },
        local::WsCkptRequest::Config,
        local::WsCkptRequest::ReloadConfig,
        local::WsCkptRequest::ReloadGlobalConfig,
        local::WsCkptRequest::ReloadWorkspacePolicy {
            workspace: "/ws".into(),
        },
        local::WsCkptRequest::ConfigOverview,
        local::WsCkptRequest::Recover {
            workspace: "/ws".into(),
        },
        local::WsCkptRequest::HealthAdvisory,
        local::WsCkptRequest::GetWorkspacePolicy {
            workspace: "/ws".into(),
        },
        local::WsCkptRequest::ResetWorkspacePolicy {
            workspace: "/ws".into(),
        },
        local::WsCkptRequest::PatchWorkspacePolicy {
            workspace: "/ws".into(),
            auto_cleanup: local::PolicyFieldOp::Set(true),
            auto_cleanup_keep: local::PolicyFieldOp::Set(local::CleanupRetention::Count(7)),
        },
        local::WsCkptRequest::RollbackPreview {
            workspace: "/ws".into(),
            to: None,
            num_ancestors: Some(3),
        },
        local::WsCkptRequest::WorkspaceIdentityV2 {
            registration_path: "/ws".into(),
        },
        local::WsCkptRequest::GuardedCheckpointV2 {
            ws_id: "ws-abcdef".into(),
            expected_generation: local::WorkspaceGenerationTokenV2::from_bytes([0x11; 32]),
            checkpoint_id: "ckp_1".into(),
            operation_digest: [0x22; 32],
            message: Some("message".into()),
            metadata: Some("{\"k\":1}".into()),
            pin: true,
        },
        local::WsCkptRequest::CheckpointEvidenceV2 {
            ws_id: "ws-abcdef".into(),
            expected_generation: local::WorkspaceGenerationTokenV2::from_bytes([0x11; 32]),
            checkpoint_id: "ckp_1".into(),
            operation_digest: [0x22; 32],
        },
        local::WsCkptRequest::GuardedRollbackPreviewV2 {
            registered_path: "/ws".into(),
            ws_id: "ws-abcdef".into(),
            expected_generation: local::WorkspaceGenerationTokenV2::from_bytes([0x11; 32]),
            target_snapshot_id: "ckp_1".into(),
        },
        local::WsCkptRequest::GuardedRollbackV2 {
            registered_path: "/ws".into(),
            ws_id: "ws-abcdef".into(),
            expected_generation: local::WorkspaceGenerationTokenV2::from_bytes([0x11; 32]),
            target_snapshot_id: "ckp_1".into(),
            expected_diff_digest: [0x33; 32],
            operation_id: "ckp_2".into(),
            operation_digest: [0x44; 32],
        },
        local::WsCkptRequest::GuardedRollbackEvidenceV2 {
            ws_id: "ws-abcdef".into(),
            operation_id: "ckp_2".into(),
            operation_digest: [0x44; 32],
        },
        local::WsCkptRequest::Unregister {
            workspace: "/ws".into(),
        },
    ];
    samples
        .into_iter()
        .map(|value| (request_case_name(&value), value))
        .collect()
}

fn error_cases() -> Vec<(&'static str, local::WsCkptErrorCode)> {
    let samples = vec![
        local::WsCkptErrorCode::WorkspaceNotFound,
        local::WsCkptErrorCode::SnapshotNotFound,
        local::WsCkptErrorCode::AlreadyInitialized,
        local::WsCkptErrorCode::BtrfsError,
        local::WsCkptErrorCode::IoError,
        local::WsCkptErrorCode::InvalidPath,
        local::WsCkptErrorCode::ConfirmationRequired,
        local::WsCkptErrorCode::InternalError,
        local::WsCkptErrorCode::SnapshotAlreadyExists,
        local::WsCkptErrorCode::WriteLockConflict,
        local::WsCkptErrorCode::DiskSpaceInsufficient,
        local::WsCkptErrorCode::CwdOccupied,
        local::WsCkptErrorCode::CwdScanFailed,
    ];
    samples
        .into_iter()
        .map(|value| (error_case_name(&value), value))
        .collect()
}

fn local_snapshot() -> local::SnapshotEntry {
    local::SnapshotEntry {
        id: "s1".into(),
        workspace: "/ws".into(),
        meta: local::SnapshotMeta {
            message: Some("message".into()),
            metadata: Some(serde_json::json!({"k": 1})),
            pinned: true,
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            missing: true,
            parent_id: Some("s0".into()),
            child_ids: vec!["s2".into()],
        },
    }
}

fn local_change() -> local::DiffEntry {
    local::DiffEntry {
        path: "src/lib.rs".into(),
        change_type: local::ChangeType::Renamed,
        detail: Some("old.rs".into()),
    }
}

fn local_status() -> local::StatusReport {
    local::StatusReport {
        uptime_secs: 9,
        workspaces: vec![local::WorkspaceInfo {
            ws_id: "id".into(),
            path: "/ws".into(),
            snapshot_count: 2,
        }],
        fs_total_bytes: 100,
        fs_used_bytes: 30,
    }
}

fn local_config() -> local::ConfigReport {
    local::ConfigReport {
        mount_path: "/mnt".into(),
        socket_path: "/run/x".into(),
        log_level: "debug".into(),
        auto_cleanup: true,
        auto_cleanup_keep: local::CleanupRetention::Age {
            raw: "2d".into(),
            secs: 172_800,
        },
        auto_cleanup_interval_secs: 11,
        health_check_interval_secs: 12,
        img_size: 13,
        img_max_percent: 0.4,
    }
}

fn guarded_evidence() -> local::GuardedCheckpointEvidenceV2 {
    local::GuardedCheckpointEvidenceV2 {
        ws_id: "ws-abcdef".into(),
        registered_path: "/ws".into(),
        generation: local::WorkspaceGenerationTokenV2::from_bytes([0x33; 32]),
        checkpoint_id: "ckp_1".into(),
        operation_digest: [0x44; 32],
        caller_uid: 1000,
        outcome: local::GuardedCheckpointOutcomeV2::Created {
            snapshot_id: "ckp_1".into(),
        },
    }
}

fn guarded_rollback_evidence(
    outcome: local::GuardedRollbackOutcomeV2,
) -> local::GuardedRollbackEvidenceV2 {
    local::GuardedRollbackEvidenceV2 {
        ws_id: "ws-abcdef".into(),
        registered_path: "/ws".into(),
        expected_generation: local::WorkspaceGenerationTokenV2::from_bytes([0x11; 32]),
        target_snapshot_id: "ckp_1".into(),
        expected_diff_digest: [0x33; 32],
        operation_id: "ckp_2".into(),
        operation_digest: [0x44; 32],
        caller_uid: 1000,
        outcome,
    }
}

fn response_cases() -> Vec<(&'static str, local::WsCkptResponse)> {
    let samples = vec![
        local::WsCkptResponse::InitOk { ws_id: "id".into() },
        local::WsCkptResponse::CheckpointOk {
            snapshot_id: "s1".into(),
        },
        local::WsCkptResponse::RollbackOk {
            from: "s2".into(),
            to: "s1".into(),
        },
        local::WsCkptResponse::DeleteOk {
            target: "s1".into(),
        },
        local::WsCkptResponse::Error {
            code: local::WsCkptErrorCode::CwdScanFailed,
            message: "failed".into(),
        },
        local::WsCkptResponse::ListOk {
            snapshots: vec![local_snapshot()],
        },
        local::WsCkptResponse::DiffOk {
            changes: vec![local_change()],
        },
        local::WsCkptResponse::StatusOk {
            report: local_status(),
        },
        local::WsCkptResponse::CleanupOk {
            removed: vec!["s0".into()],
        },
        local::WsCkptResponse::ConfigOk {
            config: local_config(),
        },
        local::WsCkptResponse::ReloadConfigOk {
            config: local_config(),
        },
        local::WsCkptResponse::CheckpointSkipped {
            reason: "unchanged".into(),
        },
        local::WsCkptResponse::RecoverOk {
            workspace: "/ws".into(),
        },
        local::WsCkptResponse::HealthAdvisoryOk {
            over_limit_workspace_count: 2,
            fs_total_bytes: 100,
            fs_used_bytes: 20,
        },
        local::WsCkptResponse::WorkspacePolicyOk {
            ws_id: "id".into(),
            effective: local::EffectivePolicy {
                auto_cleanup: true,
                auto_cleanup_keep: local::CleanupRetention::Count(3),
            },
            local: local::WorkspacePolicy {
                auto_cleanup: Some(false),
                auto_cleanup_keep: Some(local::CleanupRetention::Count(2)),
            },
            global: local::GlobalPolicySnapshot {
                auto_cleanup: true,
                auto_cleanup_keep: local::CleanupRetention::Count(4),
            },
        },
        local::WsCkptResponse::ConfigOverviewOk {
            config: local_config(),
            ws_total: 10,
            ws_with_override: 2,
        },
        local::WsCkptResponse::RollbackPreviewOk {
            to: "s1".into(),
            changes: vec![local_change()],
        },
        local::WsCkptResponse::WorkspaceIdentityV2Ok {
            protocol_version: local::GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
            ws_id: "ws-abcdef".into(),
            registered_path: "/ws".into(),
            generation: local::WorkspaceGenerationTokenV2::from_bytes([0x33; 32]),
        },
        local::WsCkptResponse::GuardedCheckpointV2Ok {
            evidence: guarded_evidence(),
        },
        local::WsCkptResponse::CheckpointEvidenceV2Ok {
            evidence: Some(guarded_evidence()),
        },
        local::WsCkptResponse::GuardedCheckpointV2Rejected {
            code: local::GuardedCheckpointRejectionCodeV2::GenerationMismatch,
            message: "generation mismatch".into(),
        },
        local::WsCkptResponse::GuardedRollbackPreviewV2Ok {
            protocol_version: local::GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
            registered_path: "/ws".into(),
            ws_id: "ws-abcdef".into(),
            generation: local::WorkspaceGenerationTokenV2::from_bytes([0x11; 32]),
            target_snapshot_id: "ckp_1".into(),
            diff_digest: [0x33; 32],
            changes: vec![local_change()],
            caller_uid: 1000,
        },
        local::WsCkptResponse::GuardedRollbackV2Ok {
            evidence: guarded_rollback_evidence(local::GuardedRollbackOutcomeV2::Succeeded {
                resulting_generation: local::WorkspaceGenerationTokenV2::from_bytes([0x55; 32]),
            }),
        },
        local::WsCkptResponse::GuardedRollbackV2Uncertain {
            evidence: guarded_rollback_evidence(local::GuardedRollbackOutcomeV2::Unknown {
                reason: "completion unproven".into(),
            }),
        },
        local::WsCkptResponse::GuardedRollbackEvidenceV2Ok {
            evidence: Some(guarded_rollback_evidence(
                local::GuardedRollbackOutcomeV2::Started,
            )),
        },
        local::WsCkptResponse::GuardedRollbackV2Rejected {
            code: local::GuardedRollbackRejectionCodeV2::DiffMismatch,
            message: "live diff changed".into(),
        },
        local::WsCkptResponse::RecoverWithWarning {
            workspace: "/ws".into(),
            warning: "backup retained".into(),
        },
        local::WsCkptResponse::UnregisterOk {
            workspace: "/ws".into(),
            retained_paths: vec!["/snapshots/ws-id".into()],
        },
    ];
    samples
        .into_iter()
        .map(|value| (response_case_name(&value), value))
        .collect()
}

fn rejection_cases() -> Vec<(&'static str, local::GuardedCheckpointRejectionCodeV2)> {
    use local::GuardedCheckpointRejectionCodeV2 as Code;
    vec![
        ("rejection/daemon_not_ready", Code::DaemonNotReady),
        (
            "rejection/peer_credentials_unavailable",
            Code::PeerCredentialsUnavailable,
        ),
        (
            "rejection/invalid_registration_path",
            Code::InvalidRegistrationPath,
        ),
        ("rejection/invalid_workspace_id", Code::InvalidWorkspaceId),
        ("rejection/invalid_checkpoint_id", Code::InvalidCheckpointId),
        ("rejection/invalid_metadata", Code::InvalidMetadata),
        ("rejection/workspace_not_found", Code::WorkspaceNotFound),
        ("rejection/generation_mismatch", Code::GenerationMismatch),
        ("rejection/operation_conflict", Code::OperationConflict),
        ("rejection/write_lock_conflict", Code::WriteLockConflict),
        ("rejection/caller_mismatch", Code::CallerMismatch),
        (
            "rejection/evidence_capacity_reached",
            Code::EvidenceCapacityReached,
        ),
    ]
}

fn rollback_rejection_cases() -> Vec<(&'static str, local::GuardedRollbackRejectionCodeV2)> {
    use local::GuardedRollbackRejectionCodeV2 as Code;
    vec![
        ("rollback_rejection/daemon_not_ready", Code::DaemonNotReady),
        (
            "rollback_rejection/peer_credentials_unavailable",
            Code::PeerCredentialsUnavailable,
        ),
        (
            "rollback_rejection/invalid_registration_path",
            Code::InvalidRegistrationPath,
        ),
        (
            "rollback_rejection/invalid_workspace_id",
            Code::InvalidWorkspaceId,
        ),
        (
            "rollback_rejection/invalid_snapshot_id",
            Code::InvalidSnapshotId,
        ),
        (
            "rollback_rejection/invalid_operation_id",
            Code::InvalidOperationId,
        ),
        (
            "rollback_rejection/workspace_not_found",
            Code::WorkspaceNotFound,
        ),
        (
            "rollback_rejection/snapshot_not_found",
            Code::SnapshotNotFound,
        ),
        (
            "rollback_rejection/generation_mismatch",
            Code::GenerationMismatch,
        ),
        ("rollback_rejection/diff_mismatch", Code::DiffMismatch),
        (
            "rollback_rejection/operation_conflict",
            Code::OperationConflict,
        ),
        (
            "rollback_rejection/write_lock_conflict",
            Code::WriteLockConflict,
        ),
        ("rollback_rejection/caller_mismatch", Code::CallerMismatch),
        (
            "rollback_rejection/evidence_capacity_reached",
            Code::EvidenceCapacityReached,
        ),
        ("rollback_rejection/cwd_occupied", Code::CwdOccupied),
        ("rollback_rejection/cwd_scan_failed", Code::CwdScanFailed),
    ]
}

fn encode_cases<T: Serialize>(cases: &[(&'static str, T)]) -> Vec<FixtureCase> {
    cases
        .iter()
        .map(|(name, value)| FixtureCase {
            name: (*name).to_string(),
            hex: to_hex(&bincode::serialize(value).unwrap()),
        })
        .collect()
}

fn assert_decode_reencode<T: Serialize + DeserializeOwned>(name: &str, hex: &str) {
    let bytes = from_hex(name, hex);
    let decoded: T = bincode::deserialize(&bytes)
        .unwrap_or_else(|e| panic!("case {name}: golden wire bytes no longer decode: {e}"));
    assert_eq!(
        to_hex(&bincode::serialize(&decoded).unwrap()),
        hex,
        "case {name}: decode/re-encode round trip diverged from golden bytes"
    );
}

fn load_fixture() -> Fixture {
    let path = fixture_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden fixture {}: {e}; regenerate with {REGEN_ENV}=1 \
             only for an intentional, daemon-coordinated protocol change",
            path.display()
        )
    });
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn checkpoint_wire_matches_golden_fixture() {
    let requests = request_cases();
    let errors = error_cases();
    let responses = response_cases();
    let rejections = rejection_cases();
    let rollback_rejections = rollback_rejection_cases();

    let mut expected = encode_cases(&requests);
    expected.extend(encode_cases(&errors));
    expected.extend(encode_cases(&responses));
    expected.extend(encode_cases(&rejections));
    expected.extend(encode_cases(&rollback_rejections));

    let unique: BTreeSet<&str> = expected.iter().map(|case| case.name.as_str()).collect();
    assert_eq!(unique.len(), expected.len(), "duplicate case names");

    if std::env::var_os(REGEN_ENV).is_some() {
        let fixture = Fixture {
            protocol_version: PROTOCOL_VERSION,
            cases: expected,
        };
        let serialized = serde_json::to_string_pretty(&fixture).unwrap();
        std::fs::write(fixture_path(), serialized + "\n").unwrap();
        return;
    }

    let fixture = load_fixture();
    assert_eq!(fixture.protocol_version, PROTOCOL_VERSION);

    let expected_names: Vec<&str> = expected.iter().map(|case| case.name.as_str()).collect();
    let fixture_names: Vec<&str> = fixture
        .cases
        .iter()
        .map(|case| case.name.as_str())
        .collect();
    assert_eq!(
        fixture_names, expected_names,
        "wire case set drifted from golden fixture; if the protocol change is \
         intentional and daemon-coordinated, regenerate with {REGEN_ENV}=1"
    );

    for (expected_case, fixture_case) in expected.iter().zip(fixture.cases.iter()) {
        assert_eq!(
            expected_case.hex, fixture_case.hex,
            "case {}: local bincode encoding no longer matches golden wire bytes",
            expected_case.name
        );
    }

    let fixture_hex = |name: &str| {
        fixture
            .cases
            .iter()
            .find(|case| case.name == name)
            .map(|case| case.hex.as_str())
            .unwrap()
    };
    for (name, _) in &requests {
        assert_decode_reencode::<local::WsCkptRequest>(name, fixture_hex(name));
    }
    for (name, _) in &errors {
        assert_decode_reencode::<local::WsCkptErrorCode>(name, fixture_hex(name));
    }
    for (name, _) in &responses {
        assert_decode_reencode::<local::WsCkptResponse>(name, fixture_hex(name));
    }
    for (name, _) in &rejections {
        assert_decode_reencode::<local::GuardedCheckpointRejectionCodeV2>(name, fixture_hex(name));
    }
    for (name, _) in &rollback_rejections {
        assert_decode_reencode::<local::GuardedRollbackRejectionCodeV2>(name, fixture_hex(name));
    }
}
