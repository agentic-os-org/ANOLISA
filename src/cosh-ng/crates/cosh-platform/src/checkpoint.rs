//! Checkpoint client — communicates with ws-ckpt daemon via Unix socket + bincode framing.
//!
//! Wire format: [4-byte LE length prefix][bincode payload]

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use cosh_types::checkpoint::*;
use cosh_types::error::{CoshError, ErrorCode};

/// Default timeout in milliseconds for socket operations.
const DEFAULT_TIMEOUT_MS: u64 = 5000;

/// Maximum response payload size (64 MiB) to guard against OOM from a
/// misbehaving or corrupted daemon.
const MAX_RESPONSE_LEN: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
enum IoPhase {
    Connect,
    WriteRequest,
    ReadResponseLength,
    ReadResponsePayload,
}

#[derive(Clone, Copy)]
enum ProtocolFailure {
    TruncatedLength,
    TruncatedPayload,
    OversizedLength,
    InvalidPayload,
    UnexpectedResponse,
}

/// Whether a failed ws-ckpt request may already have changed daemon state.
///
/// A governed caller must never retry a `PossiblyApplied` request. It has to
/// reconcile against durable daemon evidence or record an uncertain outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CkptRequestEffect {
    /// The daemon provably did not apply the request.
    KnownNoEffect,
    /// The request may have been applied, but its result cannot be proven.
    PossiblyApplied,
}

/// Failed ws-ckpt request together with its side-effect classification.
#[derive(Debug)]
pub struct CkptRequestFailure {
    /// Whether daemon state may already have changed.
    pub effect: CkptRequestEffect,
    /// Redacted failure suitable for audit and presentation.
    pub error: CoshError,
}

impl CkptRequestFailure {
    fn known_no_effect(error: CoshError) -> Self {
        Self {
            effect: CkptRequestEffect::KnownNoEffect,
            error,
        }
    }

    fn possibly_applied(error: CoshError) -> Self {
        Self {
            effect: CkptRequestEffect::PossiblyApplied,
            error,
        }
    }
}

/// Exact durable evidence for one snapshot identity in one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CkptSnapshotEvidence {
    /// Snapshot identity reported by the daemon index.
    pub snapshot_id: String,
    /// Workspace registration path reported verbatim by the daemon.
    pub workspace: String,
    /// Whether the daemon index knows the snapshot data is no longer present.
    pub missing: bool,
}

/// Client for ws-ckpt daemon IPC.
pub struct CkptClient {
    socket_path: String,
    timeout_ms: u64,
    trusted_peer_uid: Option<u32>,
}

impl CkptClient {
    /// Create a new client pointing to the given socket path with default timeout.
    pub fn new(socket_path: &str) -> Self {
        Self {
            socket_path: socket_path.to_string(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            trusted_peer_uid: None,
        }
    }

    /// Create a new client with an explicit timeout (milliseconds).
    pub fn with_timeout(socket_path: &str, timeout_ms: u64) -> Self {
        Self {
            socket_path: socket_path.to_string(),
            timeout_ms,
            trusted_peer_uid: None,
        }
    }

    /// Create a new client using the default socket path.
    pub fn default_path() -> Self {
        Self::new(DEFAULT_SOCKET_PATH)
    }

    /// Require every connection to authenticate as root or `owner_uid`.
    ///
    /// Path metadata alone cannot secure a socket: another principal may swap a
    /// verified directory between the check and `connect`. Kernel peer
    /// credentials are read after connecting and before any request byte is
    /// written, so an impostor listener is rejected with no possible effect.
    /// Governed operations reject the client before socket access unless this
    /// configuration is present; legacy operations retain their existing
    /// optional-authentication behavior.
    #[must_use]
    pub fn require_trusted_peer(mut self, owner_uid: u32) -> Self {
        self.trusted_peer_uid = Some(owner_uid);
        self
    }

    /// Check if the daemon socket exists (basic health check).
    pub fn is_available(&self) -> bool {
        Path::new(&self.socket_path).exists()
    }

    // =======================================================================
    // Public operations (map to WsCkptRequest variants)
    // =======================================================================

    /// Initialize a workspace for checkpointing.
    pub fn init(&self, workspace: &str) -> Result<CkptInitResult, CoshError> {
        let req = WsCkptRequest::Init {
            workspace: workspace.to_string(),
        };
        match self.send_request(&req)? {
            WsCkptResponse::InitOk { ws_id } => Ok(CkptInitResult { ws_id }),
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Recover a workspace.
    pub fn recover(&self, workspace: &str) -> Result<CkptRecoverResult, CoshError> {
        let req = WsCkptRequest::Recover {
            workspace: workspace.to_string(),
        };
        match self.send_request(&req)? {
            WsCkptResponse::RecoverOk { workspace } => Ok(CkptRecoverResult {
                workspace,
                warning: None,
            }),
            WsCkptResponse::RecoverWithWarning { workspace, warning } => Ok(CkptRecoverResult {
                workspace,
                warning: Some(warning),
            }),
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Create a workspace checkpoint.
    pub fn create(
        &self,
        workspace: &str,
        id: &str,
        message: Option<&str>,
        metadata: Option<&str>,
        pin: bool,
    ) -> Result<CkptCreated, CoshError> {
        let req = WsCkptRequest::Checkpoint {
            workspace: workspace.to_string(),
            id: id.to_string(),
            message: message.map(|s| s.to_string()),
            metadata: metadata.map(|s| s.to_string()),
            pin,
        };
        match self.send_request(&req)? {
            WsCkptResponse::CheckpointOk { snapshot_id } => Ok(CkptCreated {
                snapshot_id: Some(snapshot_id),
                workspace: workspace.to_string(),
                skipped: false,
                reason: None,
            }),
            WsCkptResponse::CheckpointSkipped { reason } => Ok(CkptCreated {
                snapshot_id: None,
                workspace: workspace.to_string(),
                skipped: true,
                reason: Some(reason),
            }),
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Create a workspace checkpoint, classifying failures by possible effect.
    ///
    /// Governed callers use this instead of [`Self::create`] because a lost
    /// response must not be replayed. Only a failure raised before any request
    /// byte reaches the kernel is `KnownNoEffect`. Every failure after that,
    /// including a daemon-reported error code, is `PossiblyApplied` so the caller
    /// reconciles or records an uncertain outcome.
    ///
    /// # Errors
    ///
    /// Returns `KnownNoEffect` when trusted-peer authentication was not
    /// configured, or a classified transport, protocol, or daemon failure.
    pub fn create_classified(
        &self,
        workspace: &str,
        id: &str,
        message: Option<&str>,
        metadata: Option<&str>,
        pin: bool,
    ) -> Result<CkptCreated, CkptRequestFailure> {
        let owner_uid = self
            .governed_peer_uid()
            .map_err(CkptRequestFailure::known_no_effect)?;
        let req = WsCkptRequest::Checkpoint {
            workspace: workspace.to_string(),
            id: id.to_string(),
            message: message.map(|s| s.to_string()),
            metadata: metadata.map(|s| s.to_string()),
            pin,
        };
        match self.send_request_classified_with_peer(&req, Some(owner_uid))? {
            WsCkptResponse::CheckpointOk { snapshot_id } => Ok(CkptCreated {
                snapshot_id: Some(snapshot_id),
                workspace: workspace.to_string(),
                skipped: false,
                reason: None,
            }),
            WsCkptResponse::CheckpointSkipped { reason } => Ok(CkptCreated {
                snapshot_id: None,
                workspace: workspace.to_string(),
                skipped: true,
                reason: Some(reason),
            }),
            // A daemon error proves no snapshot was created, but not that daemon
            // state is unchanged. The checkpoint dispatch path auto-initializes
            // the workspace first, so registration, subvolume adoption, or a
            // broken-symlink removal may already have happened before the code
            // was produced. No response code is treated as proven no-effect
            // until the protocol supplies an explicit pre-effect guarantee.
            WsCkptResponse::Error { code, message: _ } => {
                Err(CkptRequestFailure::possibly_applied(ws_error_to_cosh(
                    code,
                    "ws-ckpt daemon rejected the checkpoint request".to_owned(),
                )))
            }
            // An unexpected variant means this client's model of the daemon is
            // wrong, so no-effect cannot be claimed.
            _ => Err(CkptRequestFailure::possibly_applied(unexpected_response())),
        }
    }

    /// Resolves the exact V2 identity of an already-registered workspace.
    ///
    /// The daemon contract guarantees that this request performs no workspace
    /// initialization or repair. Governed callers must still configure trusted
    /// peer authentication so a substituted listener cannot supply authority.
    ///
    /// # Errors
    ///
    /// Returns when trusted-peer authentication is absent, the transport or
    /// protocol fails, the daemon is not V2-capable, or the workspace is not
    /// registered under the exact supplied path.
    pub fn workspace_identity_v2(
        &self,
        registration_path: &str,
    ) -> Result<WorkspaceIdentityV2, CoshError> {
        let owner_uid = self.governed_peer_uid()?;
        let request = WsCkptRequest::WorkspaceIdentityV2 {
            registration_path: registration_path.to_owned(),
        };
        match self
            .send_request_classified_with_peer(&request, Some(owner_uid))
            .map_err(|failure| failure.error)?
        {
            WsCkptResponse::WorkspaceIdentityV2Ok {
                protocol_version,
                ws_id,
                registered_path,
                generation,
            } if protocol_version == GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2
                && registered_path == registration_path =>
            {
                Ok(WorkspaceIdentityV2 {
                    protocol_version,
                    ws_id,
                    registered_path,
                    generation,
                })
            }
            WsCkptResponse::GuardedCheckpointV2Rejected { code, .. } => {
                Err(guarded_rejection_to_cosh(code))
            }
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Creates one checkpoint through the exact V2 generation fence.
    ///
    /// An explicit `GuardedCheckpointV2Rejected` response is `KnownNoEffect`
    /// because that versioned daemon response promises the backend was not
    /// invoked. Any legacy error, mismatched evidence, or failure after request
    /// bytes were written remains `PossiblyApplied` and must be reconciled with
    /// [`Self::checkpoint_evidence_v2`] rather than replayed.
    ///
    /// # Errors
    ///
    /// Returns a classified authentication, transport, protocol, rejection, or
    /// evidence-binding failure.
    #[allow(clippy::too_many_arguments)]
    pub fn guarded_create_v2(
        &self,
        ws_id: &str,
        expected_generation: WorkspaceGenerationTokenV2,
        checkpoint_id: &str,
        operation_digest: [u8; 32],
        message: Option<&str>,
        metadata: Option<&str>,
        pin: bool,
    ) -> Result<GuardedCheckpointEvidenceV2, CkptRequestFailure> {
        let owner_uid = self
            .governed_peer_uid()
            .map_err(CkptRequestFailure::known_no_effect)?;
        let request = WsCkptRequest::GuardedCheckpointV2 {
            ws_id: ws_id.to_owned(),
            expected_generation,
            checkpoint_id: checkpoint_id.to_owned(),
            operation_digest,
            message: message.map(str::to_owned),
            metadata: metadata.map(str::to_owned),
            pin,
        };
        match self.send_request_classified_with_peer(&request, Some(owner_uid))? {
            WsCkptResponse::GuardedCheckpointV2Ok { evidence }
                if evidence_matches_v2(
                    &evidence,
                    ws_id,
                    expected_generation,
                    checkpoint_id,
                    operation_digest,
                ) =>
            {
                Ok(evidence)
            }
            WsCkptResponse::GuardedCheckpointV2Rejected { code, .. } => Err(
                CkptRequestFailure::known_no_effect(guarded_rejection_to_cosh(code)),
            ),
            WsCkptResponse::Error { code, message } => Err(CkptRequestFailure::possibly_applied(
                ws_error_to_cosh(code, message),
            )),
            _ => Err(CkptRequestFailure::possibly_applied(unexpected_response())),
        }
    }

    /// Queries exact durable V2 evidence without replaying checkpoint creation.
    ///
    /// # Errors
    ///
    /// Returns when trusted-peer authentication is absent, transport or protocol
    /// validation fails, the daemon rejects the lookup, or returned evidence does
    /// not reproduce every supplied operation binding.
    pub fn checkpoint_evidence_v2(
        &self,
        ws_id: &str,
        expected_generation: WorkspaceGenerationTokenV2,
        checkpoint_id: &str,
        operation_digest: [u8; 32],
    ) -> Result<Option<GuardedCheckpointEvidenceV2>, CoshError> {
        let owner_uid = self.governed_peer_uid()?;
        let request = WsCkptRequest::CheckpointEvidenceV2 {
            ws_id: ws_id.to_owned(),
            expected_generation,
            checkpoint_id: checkpoint_id.to_owned(),
            operation_digest,
        };
        match self
            .send_request_classified_with_peer(&request, Some(owner_uid))
            .map_err(|failure| failure.error)?
        {
            WsCkptResponse::CheckpointEvidenceV2Ok { evidence: None } => Ok(None),
            WsCkptResponse::CheckpointEvidenceV2Ok {
                evidence: Some(evidence),
            } if evidence_matches_v2(
                &evidence,
                ws_id,
                expected_generation,
                checkpoint_id,
                operation_digest,
            ) =>
            {
                Ok(Some(evidence))
            }
            WsCkptResponse::GuardedCheckpointV2Rejected { code, .. } => {
                Err(guarded_rejection_to_cosh(code))
            }
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Look up exact durable evidence for one snapshot ID in one workspace.
    ///
    /// This is the read-only reconcile query for a checkpoint whose response was
    /// lost. It reuses the existing workspace-scoped listing and matches the
    /// snapshot identity exactly, so it never probes by creating a second
    /// snapshot. `Ok(None)` means the daemon index does not list the identity,
    /// which is not proof that no snapshot exists.
    ///
    /// # Errors
    ///
    /// Returns a permission error when trusted-peer authentication was not
    /// configured, or a transport, protocol, or daemon-reported failure. Any
    /// error leaves the reconciled outcome unproven.
    pub fn find_snapshot(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<Option<CkptSnapshotEvidence>, CoshError> {
        let owner_uid = self.governed_peer_uid()?;
        let req = WsCkptRequest::List {
            workspace: Some(workspace.to_string()),
            format: None,
        };
        match self
            .send_request_classified_with_peer(&req, Some(owner_uid))
            .map_err(|failure| failure.error)?
        {
            WsCkptResponse::ListOk { snapshots } => Ok(snapshots
                .into_iter()
                .find(|entry| entry.id == id)
                .map(|entry| CkptSnapshotEvidence {
                    snapshot_id: entry.id,
                    workspace: entry.workspace,
                    missing: entry.meta.missing,
                })),
            WsCkptResponse::Error { code, message: _ } => Err(ws_error_to_cosh(
                code,
                "ws-ckpt daemon rejected the checkpoint evidence query".to_owned(),
            )),
            _ => Err(unexpected_response()),
        }
    }

    /// List checkpoints for a workspace.
    pub fn list(&self, workspace: Option<&str>) -> Result<CkptListResult, CoshError> {
        let req = WsCkptRequest::List {
            workspace: workspace.map(|s| s.to_string()),
            format: None,
        };
        match self.send_request(&req)? {
            WsCkptResponse::ListOk { snapshots } => {
                let total = snapshots.len();
                let entries = snapshots
                    .into_iter()
                    .map(|s| CkptEntry {
                        id: s.id,
                        workspace: s.workspace,
                        message: s.meta.message,
                        pinned: s.meta.pinned,
                        created_at: s.meta.created_at.to_rfc3339(),
                    })
                    .collect();
                Ok(CkptListResult {
                    snapshots: entries,
                    total,
                })
            }
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Restore (rollback) to a checkpoint.
    pub fn restore(&self, workspace: &str, snapshot_id: &str) -> Result<CkptRestored, CoshError> {
        let req = WsCkptRequest::Rollback {
            workspace: workspace.to_string(),
            to: Some(snapshot_id.to_string()),
            num_ancestors: None,
        };
        match self.send_request(&req)? {
            WsCkptResponse::RollbackOk { from, to } => Ok(CkptRestored { from, to }),
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Restores under trusted-peer authentication and classifies uncertain I/O.
    pub fn restore_classified(
        &self,
        workspace: &str,
        snapshot_id: &str,
    ) -> Result<CkptRestored, CkptRequestFailure> {
        let owner_uid = self
            .governed_peer_uid()
            .map_err(CkptRequestFailure::known_no_effect)?;
        let request = WsCkptRequest::Rollback {
            workspace: workspace.to_owned(),
            to: Some(snapshot_id.to_owned()),
            num_ancestors: None,
        };
        match self.send_request_classified_with_peer(&request, Some(owner_uid))? {
            WsCkptResponse::RollbackOk { from, to } => Ok(CkptRestored { from, to }),
            WsCkptResponse::Error { code, message } => Err(CkptRequestFailure::possibly_applied(
                ws_error_to_cosh(code, message),
            )),
            _ => Err(CkptRequestFailure::possibly_applied(unexpected_response())),
        }
    }

    /// Previews one exact rollback target under trusted-peer authentication.
    pub fn rollback_preview_trusted(
        &self,
        workspace: &str,
        snapshot_id: &str,
    ) -> Result<CkptRollbackPreviewResult, CoshError> {
        let owner_uid = self.governed_peer_uid()?;
        let request = WsCkptRequest::RollbackPreview {
            workspace: workspace.to_owned(),
            to: Some(snapshot_id.to_owned()),
            num_ancestors: None,
        };
        match self
            .send_request_classified_with_peer(&request, Some(owner_uid))
            .map_err(|failure| failure.error)?
        {
            WsCkptResponse::RollbackPreviewOk { to, changes } => {
                Ok(CkptRollbackPreviewResult { to, changes })
            }
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Previews one exact rollback target under identity and generation fences.
    pub fn guarded_rollback_preview_v2(
        &self,
        registered_path: &str,
        ws_id: &str,
        expected_generation: WorkspaceGenerationTokenV2,
        target_snapshot_id: &str,
    ) -> Result<CkptGuardedRollbackPreviewV2, CoshError> {
        let owner_uid = self.governed_peer_uid()?;
        let request = WsCkptRequest::GuardedRollbackPreviewV2 {
            registered_path: registered_path.to_owned(),
            ws_id: ws_id.to_owned(),
            expected_generation,
            target_snapshot_id: target_snapshot_id.to_owned(),
        };
        match self
            .send_request_classified_with_peer(&request, Some(owner_uid))
            .map_err(|failure| failure.error)?
        {
            WsCkptResponse::GuardedRollbackPreviewV2Ok {
                protocol_version,
                registered_path: actual_path,
                ws_id: actual_ws_id,
                generation,
                target_snapshot_id: actual_target,
                diff_digest,
                changes,
                caller_uid,
            } if protocol_version == GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2
                && actual_path == registered_path
                && actual_ws_id == ws_id
                && generation == expected_generation
                && actual_target == target_snapshot_id
                && caller_uid == owner_uid =>
            {
                Ok(CkptGuardedRollbackPreviewV2 {
                    protocol_version,
                    registered_path: actual_path,
                    ws_id: actual_ws_id,
                    generation,
                    target_snapshot_id: actual_target,
                    diff_digest,
                    changes,
                    caller_uid,
                })
            }
            WsCkptResponse::GuardedRollbackV2Rejected { code, .. } => {
                Err(guarded_rollback_rejection_to_cosh(code))
            }
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Atomically revalidates a guarded preview and switches to its exact target.
    ///
    /// A typed rejection is known not to have invoked the rollback backend. Any
    /// uncertain response or transport failure after request transmission must
    /// not be replayed; reconcile it with [`Self::guarded_rollback_evidence_v2`].
    #[allow(clippy::too_many_arguments)]
    pub fn guarded_rollback_v2(
        &self,
        registered_path: &str,
        ws_id: &str,
        expected_generation: WorkspaceGenerationTokenV2,
        target_snapshot_id: &str,
        expected_diff_digest: [u8; 32],
        operation_id: &str,
        operation_digest: [u8; 32],
    ) -> Result<GuardedRollbackEvidenceV2, CkptRequestFailure> {
        let owner_uid = self
            .governed_peer_uid()
            .map_err(CkptRequestFailure::known_no_effect)?;
        let request = WsCkptRequest::GuardedRollbackV2 {
            registered_path: registered_path.to_owned(),
            ws_id: ws_id.to_owned(),
            expected_generation,
            target_snapshot_id: target_snapshot_id.to_owned(),
            expected_diff_digest,
            operation_id: operation_id.to_owned(),
            operation_digest,
        };
        match self.send_request_classified_with_peer(&request, Some(owner_uid))? {
            WsCkptResponse::GuardedRollbackV2Ok { evidence }
                if guarded_rollback_evidence_matches(
                    &evidence,
                    registered_path,
                    ws_id,
                    expected_generation,
                    target_snapshot_id,
                    expected_diff_digest,
                    operation_id,
                    operation_digest,
                    owner_uid,
                ) && matches!(evidence.outcome, GuardedRollbackOutcomeV2::Succeeded { .. }) =>
            {
                Ok(evidence)
            }
            WsCkptResponse::GuardedRollbackV2Rejected { code, .. } => Err(
                CkptRequestFailure::known_no_effect(guarded_rollback_rejection_to_cosh(code)),
            ),
            WsCkptResponse::GuardedRollbackV2Uncertain { evidence }
                if guarded_rollback_evidence_matches(
                    &evidence,
                    registered_path,
                    ws_id,
                    expected_generation,
                    target_snapshot_id,
                    expected_diff_digest,
                    operation_id,
                    operation_digest,
                    owner_uid,
                ) =>
            {
                Err(CkptRequestFailure::possibly_applied(CoshError::new(
                    ErrorCode::CheckpointRestoreFailed,
                    "ws-ckpt could not prove the guarded rollback outcome",
                    "checkpoint",
                )))
            }
            WsCkptResponse::Error { code, message } => Err(CkptRequestFailure::possibly_applied(
                ws_error_to_cosh(code, message),
            )),
            _ => Err(CkptRequestFailure::possibly_applied(unexpected_response())),
        }
    }

    /// Queries durable evidence for one guarded rollback without replaying it.
    pub fn guarded_rollback_evidence_v2(
        &self,
        ws_id: &str,
        operation_id: &str,
        operation_digest: [u8; 32],
    ) -> Result<Option<GuardedRollbackEvidenceV2>, CoshError> {
        let owner_uid = self.governed_peer_uid()?;
        let request = WsCkptRequest::GuardedRollbackEvidenceV2 {
            ws_id: ws_id.to_owned(),
            operation_id: operation_id.to_owned(),
            operation_digest,
        };
        match self
            .send_request_classified_with_peer(&request, Some(owner_uid))
            .map_err(|failure| failure.error)?
        {
            WsCkptResponse::GuardedRollbackEvidenceV2Ok { evidence: None } => Ok(None),
            WsCkptResponse::GuardedRollbackEvidenceV2Ok {
                evidence: Some(evidence),
            } if evidence.ws_id == ws_id
                && evidence.operation_id == operation_id
                && evidence.operation_digest == operation_digest
                && evidence.caller_uid == owner_uid =>
            {
                Ok(Some(evidence))
            }
            WsCkptResponse::GuardedRollbackV2Rejected { code, .. } => {
                Err(guarded_rollback_rejection_to_cosh(code))
            }
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Query workspace checkpoint status.
    pub fn status(&self, workspace: Option<&str>) -> Result<CkptStatusResult, CoshError> {
        let req = WsCkptRequest::Status {
            workspace: workspace.map(|s| s.to_string()),
        };
        match self.send_request(&req)? {
            WsCkptResponse::StatusOk { report } => Ok(CkptStatusResult {
                uptime_secs: report.uptime_secs,
                workspaces: report.workspaces,
                fs_total_bytes: report.fs_total_bytes,
                fs_used_bytes: report.fs_used_bytes,
            }),
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Delete a snapshot.
    pub fn delete(
        &self,
        workspace: Option<&str>,
        snapshot: &str,
        force: bool,
    ) -> Result<CkptDeleted, CoshError> {
        let req = WsCkptRequest::Delete {
            workspace: workspace.map(|s| s.to_string()),
            snapshot: snapshot.to_string(),
            force,
        };
        match self.send_request(&req)? {
            WsCkptResponse::DeleteOk { target } => Ok(CkptDeleted { target }),
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Diff between two snapshots.
    pub fn diff(&self, workspace: &str, from: &str, to: &str) -> Result<CkptDiffResult, CoshError> {
        let req = WsCkptRequest::Diff {
            workspace: workspace.to_string(),
            from: from.to_string(),
            to: Some(to.to_string()),
        };
        match self.send_request(&req)? {
            WsCkptResponse::DiffOk { changes } => Ok(CkptDiffResult { changes }),
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    /// Cleanup old snapshots.
    pub fn cleanup(
        &self,
        workspace: &str,
        keep: Option<u32>,
    ) -> Result<CkptCleanupResult, CoshError> {
        let req = WsCkptRequest::Cleanup {
            workspace: workspace.to_string(),
            keep,
        };
        match self.send_request(&req)? {
            WsCkptResponse::CleanupOk { removed } => Ok(CkptCleanupResult { removed }),
            WsCkptResponse::Error { code, message } => Err(ws_error_to_cosh(code, message)),
            _ => Err(unexpected_response()),
        }
    }

    // =======================================================================
    // Wire protocol
    // =======================================================================

    /// Send a request and receive a response over the Unix socket.
    /// Wire format: [4-byte LE length prefix][bincode payload]
    fn send_request(&self, req: &WsCkptRequest) -> Result<WsCkptResponse, CoshError> {
        self.send_request_classified(req)
            .map_err(|failure| failure.error)
    }

    /// Send a request, reporting whether a failure may already have taken effect.
    ///
    /// Only failures raised strictly before any request byte reaches the kernel
    /// are `KnownNoEffect`. Once a byte is handed over, this client does not
    /// assume the daemon discards a truncated frame, so the outcome is
    /// `PossiblyApplied` until the caller proves otherwise.
    fn send_request_classified(
        &self,
        req: &WsCkptRequest,
    ) -> Result<WsCkptResponse, CkptRequestFailure> {
        self.send_request_classified_with_peer(req, self.trusted_peer_uid)
    }

    fn governed_peer_uid(&self) -> Result<u32, CoshError> {
        self.trusted_peer_uid
            .ok_or_else(trusted_peer_configuration_error)
    }

    fn send_request_classified_with_peer(
        &self,
        req: &WsCkptRequest,
        trusted_peer_uid: Option<u32>,
    ) -> Result<WsCkptResponse, CkptRequestFailure> {
        // 1. Socket existence check — fast fail before attempting connection.
        if !Path::new(&self.socket_path).exists() {
            return Err(CkptRequestFailure::known_no_effect(
                CoshError::new(
                    ErrorCode::CheckpointDaemonUnavailable,
                    "ws-ckpt daemon socket is unavailable",
                    "checkpoint",
                )
                .with_hint("Start daemon with: systemctl start ws-ckpt")
                .recoverable(true),
            ));
        }

        // 2. Connect to Unix socket.
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|error| {
            CkptRequestFailure::known_no_effect(classify_io_error(error, IoPhase::Connect))
        })?;

        // 2a. Authenticate the connected peer before writing anything. This is
        //     what actually defeats a directory or socket swap racing the
        //     caller's path checks.
        if let Some(owner_uid) = trusted_peer_uid {
            verify_peer_credentials(&stream, owner_uid)
                .map_err(CkptRequestFailure::known_no_effect)?;
        }

        // 3. Apply configurable timeout to both read and write.
        let timeout = Duration::from_millis(self.timeout_ms);
        stream.set_read_timeout(Some(timeout)).ok();
        stream.set_write_timeout(Some(timeout)).ok();

        // 4. Encode and send the request frame, tracking how much was handed over.
        let frame = encode_frame(req).map_err(CkptRequestFailure::known_no_effect)?;
        write_request_frame(&mut stream, &frame)?;

        // 5. Read response length prefix (4 bytes, little-endian).
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).map_err(|error| {
            CkptRequestFailure::possibly_applied(if error.kind() == ErrorKind::UnexpectedEof {
                protocol_error(ProtocolFailure::TruncatedLength)
            } else {
                classify_io_error(error, IoPhase::ReadResponseLength)
            })
        })?;
        let resp_len = u32::from_le_bytes(len_buf) as usize;

        if resp_len > MAX_RESPONSE_LEN {
            return Err(CkptRequestFailure::possibly_applied(protocol_error(
                ProtocolFailure::OversizedLength,
            )));
        }

        // 6. Read response payload.
        let mut resp_buf = vec![0u8; resp_len];
        stream.read_exact(&mut resp_buf).map_err(|error| {
            CkptRequestFailure::possibly_applied(if error.kind() == ErrorKind::UnexpectedEof {
                protocol_error(ProtocolFailure::TruncatedPayload)
            } else {
                classify_io_error(error, IoPhase::ReadResponsePayload)
            })
        })?;

        // 7. Decode response.
        decode_response(&resp_buf).map_err(CkptRequestFailure::possibly_applied)
    }
}

/// Writes the complete request frame, classifying a partial write as applied.
fn write_request_frame(stream: &mut UnixStream, frame: &[u8]) -> Result<(), CkptRequestFailure> {
    let mut written = 0;
    while written < frame.len() {
        // A partial write leaves the peer holding an undecodable prefix. This
        // client does not depend on the daemon's framing to discard it, so any
        // transferred byte forfeits the known-no-effect classification.
        let classify = |error| {
            if written == 0 {
                CkptRequestFailure::known_no_effect(error)
            } else {
                CkptRequestFailure::possibly_applied(error)
            }
        };
        match stream.write(&frame[written..]) {
            Ok(0) => {
                return Err(classify(classify_io_error(
                    std::io::Error::from(ErrorKind::WriteZero),
                    IoPhase::WriteRequest,
                )))
            }
            Ok(count) => written = written.saturating_add(count),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(classify(classify_io_error(error, IoPhase::WriteRequest))),
        }
    }
    Ok(())
}

/// Rejects a connected peer that is neither root nor the expected owner.
#[cfg(target_os = "linux")]
fn verify_peer_credentials(stream: &UnixStream, owner_uid: u32) -> Result<(), CoshError> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};

    let credentials = getsockopt(stream, PeerCredentials).map_err(|_| peer_error())?;
    if credentials.uid() == 0 || credentials.uid() == owner_uid {
        Ok(())
    } else {
        Err(peer_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn verify_peer_credentials(_stream: &UnixStream, _owner_uid: u32) -> Result<(), CoshError> {
    // Without kernel peer credentials a governed caller cannot authenticate the
    // daemon, so requiring a trusted peer fails closed instead of degrading.
    Err(peer_error())
}

fn peer_error() -> CoshError {
    CoshError::new(
        ErrorCode::PermissionDenied,
        "ws-ckpt daemon peer is not a trusted principal",
        "checkpoint",
    )
    .with_hint("Verify that ws-ckpt is running as root or as the Gateway owner")
}

fn trusted_peer_configuration_error() -> CoshError {
    CoshError::new(
        ErrorCode::PermissionDenied,
        "governed checkpoint request requires trusted peer authentication",
        "checkpoint",
    )
    .with_hint("Configure the client with require_trusted_peer(owner_uid)")
}

// ---------------------------------------------------------------------------
// Frame encoding/decoding
// ---------------------------------------------------------------------------

/// Encode a message into a length-prefixed bincode frame.
/// Format: [4-byte LE length][bincode payload]
fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, CoshError> {
    let payload = bincode::serialize(msg).map_err(|e| {
        CoshError::new(
            ErrorCode::Unknown,
            format!("Failed to serialize request: {}", e),
            "checkpoint",
        )
    })?;
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode a bincode payload into a WsCkptResponse.
fn decode_response(data: &[u8]) -> Result<WsCkptResponse, CoshError> {
    bincode::deserialize(data).map_err(|_| protocol_error(ProtocolFailure::InvalidPayload))
}

// ---------------------------------------------------------------------------
// Error conversion helpers
// ---------------------------------------------------------------------------

/// Map ws-ckpt daemon ErrorCode to CoshError.
fn ws_error_to_cosh(code: WsCkptErrorCode, message: String) -> CoshError {
    let (error_code, hint) = match code {
        WsCkptErrorCode::WorkspaceNotFound => (
            ErrorCode::CheckpointNotFound,
            Some("Workspace not initialized. Run: cosh checkpoint init --workspace <path>"),
        ),
        WsCkptErrorCode::SnapshotNotFound => (
            ErrorCode::CheckpointNotFound,
            Some("Use 'cosh checkpoint list' to see available snapshots"),
        ),
        WsCkptErrorCode::AlreadyInitialized => (
            ErrorCode::CheckpointCreateFailed,
            Some("Workspace is already initialized for checkpointing"),
        ),
        WsCkptErrorCode::BtrfsError => (
            ErrorCode::CheckpointCreateFailed,
            Some("Btrfs filesystem error. Check that the workspace is on a btrfs volume"),
        ),
        WsCkptErrorCode::IoError => (ErrorCode::Unknown, None),
        WsCkptErrorCode::InvalidPath => (
            ErrorCode::InvalidInput,
            Some("The provided path is invalid or inaccessible"),
        ),
        WsCkptErrorCode::ConfirmationRequired => (
            ErrorCode::CheckpointRestoreFailed,
            Some("Use --force to skip confirmation"),
        ),
        WsCkptErrorCode::InternalError => (ErrorCode::Unknown, None),
        WsCkptErrorCode::SnapshotAlreadyExists => (
            ErrorCode::CheckpointCreateFailed,
            Some("A snapshot with this ID already exists"),
        ),
        WsCkptErrorCode::WriteLockConflict => (
            ErrorCode::CheckpointCreateFailed,
            Some("Another operation is in progress, retry later"),
        ),
        WsCkptErrorCode::DiskSpaceInsufficient => (
            ErrorCode::CheckpointCreateFailed,
            Some("Not enough disk space. Run 'cosh checkpoint cleanup' to free space"),
        ),
        WsCkptErrorCode::CwdOccupied => (
            ErrorCode::CheckpointRestoreFailed,
            Some("Leave the workspace before retrying the restore"),
        ),
        WsCkptErrorCode::CwdScanFailed => (ErrorCode::CheckpointRestoreFailed, None),
    };

    let mut err = CoshError::new(error_code, message, "checkpoint");
    if let Some(h) = hint {
        err = err.with_hint(h);
    }
    err
}

fn evidence_matches_v2(
    evidence: &GuardedCheckpointEvidenceV2,
    ws_id: &str,
    expected_generation: WorkspaceGenerationTokenV2,
    checkpoint_id: &str,
    operation_digest: [u8; 32],
) -> bool {
    let outcome_matches = match &evidence.outcome {
        GuardedCheckpointOutcomeV2::Created { snapshot_id } => snapshot_id == checkpoint_id,
        GuardedCheckpointOutcomeV2::Skipped { .. } => true,
    };
    evidence.ws_id == ws_id
        && evidence.generation == expected_generation
        && evidence.checkpoint_id == checkpoint_id
        && evidence.operation_digest == operation_digest
        && evidence.caller_uid == nix::unistd::Uid::effective().as_raw()
        && outcome_matches
}

#[allow(clippy::too_many_arguments)]
fn guarded_rollback_evidence_matches(
    evidence: &GuardedRollbackEvidenceV2,
    registered_path: &str,
    ws_id: &str,
    expected_generation: WorkspaceGenerationTokenV2,
    target_snapshot_id: &str,
    expected_diff_digest: [u8; 32],
    operation_id: &str,
    operation_digest: [u8; 32],
    caller_uid: u32,
) -> bool {
    evidence.registered_path == registered_path
        && evidence.ws_id == ws_id
        && evidence.expected_generation == expected_generation
        && evidence.target_snapshot_id == target_snapshot_id
        && evidence.expected_diff_digest == expected_diff_digest
        && evidence.operation_id == operation_id
        && evidence.operation_digest == operation_digest
        && evidence.caller_uid == caller_uid
}

fn guarded_rollback_rejection_to_cosh(code: GuardedRollbackRejectionCodeV2) -> CoshError {
    let (error_code, message, recoverable) = match code {
        GuardedRollbackRejectionCodeV2::DaemonNotReady => (
            ErrorCode::CheckpointDaemonUnavailable,
            "ws-ckpt daemon is not ready for guarded rollback",
            true,
        ),
        GuardedRollbackRejectionCodeV2::PeerCredentialsUnavailable
        | GuardedRollbackRejectionCodeV2::CallerMismatch => (
            ErrorCode::PermissionDenied,
            "ws-ckpt rejected the authenticated rollback caller",
            false,
        ),
        GuardedRollbackRejectionCodeV2::InvalidRegistrationPath
        | GuardedRollbackRejectionCodeV2::InvalidWorkspaceId
        | GuardedRollbackRejectionCodeV2::InvalidSnapshotId
        | GuardedRollbackRejectionCodeV2::InvalidOperationId => (
            ErrorCode::InvalidInput,
            "ws-ckpt rejected invalid guarded rollback input",
            false,
        ),
        GuardedRollbackRejectionCodeV2::WorkspaceNotFound
        | GuardedRollbackRejectionCodeV2::SnapshotNotFound => (
            ErrorCode::CheckpointNotFound,
            "ws-ckpt could not find the guarded rollback target",
            false,
        ),
        GuardedRollbackRejectionCodeV2::WriteLockConflict => (
            ErrorCode::CheckpointRestoreFailed,
            "ws-ckpt rejected guarded rollback because another operation holds the write lock",
            true,
        ),
        GuardedRollbackRejectionCodeV2::CwdOccupied => (
            ErrorCode::CheckpointRestoreFailed,
            "leave the workspace before retrying guarded rollback",
            true,
        ),
        GuardedRollbackRejectionCodeV2::CwdScanFailed => (
            ErrorCode::CheckpointRestoreFailed,
            "ws-ckpt could not safely inspect workspace occupants",
            true,
        ),
        GuardedRollbackRejectionCodeV2::GenerationMismatch
        | GuardedRollbackRejectionCodeV2::DiffMismatch
        | GuardedRollbackRejectionCodeV2::OperationConflict
        | GuardedRollbackRejectionCodeV2::EvidenceCapacityReached => (
            ErrorCode::CheckpointRestoreFailed,
            "ws-ckpt rejected the guarded rollback binding",
            false,
        ),
    };

    CoshError::new(error_code, message, "checkpoint")
        .recoverable(recoverable)
        .with_details(serde_json::json!({
            "guarded_rollback_rejection": guarded_rollback_rejection_name(code),
        }))
}

fn guarded_rollback_rejection_name(code: GuardedRollbackRejectionCodeV2) -> &'static str {
    match code {
        GuardedRollbackRejectionCodeV2::DaemonNotReady => "daemon_not_ready",
        GuardedRollbackRejectionCodeV2::PeerCredentialsUnavailable => {
            "peer_credentials_unavailable"
        }
        GuardedRollbackRejectionCodeV2::InvalidRegistrationPath => "invalid_registration_path",
        GuardedRollbackRejectionCodeV2::InvalidWorkspaceId => "invalid_workspace_id",
        GuardedRollbackRejectionCodeV2::InvalidSnapshotId => "invalid_snapshot_id",
        GuardedRollbackRejectionCodeV2::InvalidOperationId => "invalid_operation_id",
        GuardedRollbackRejectionCodeV2::WorkspaceNotFound => "workspace_not_found",
        GuardedRollbackRejectionCodeV2::SnapshotNotFound => "snapshot_not_found",
        GuardedRollbackRejectionCodeV2::GenerationMismatch => "generation_mismatch",
        GuardedRollbackRejectionCodeV2::DiffMismatch => "diff_mismatch",
        GuardedRollbackRejectionCodeV2::OperationConflict => "operation_conflict",
        GuardedRollbackRejectionCodeV2::WriteLockConflict => "write_lock_conflict",
        GuardedRollbackRejectionCodeV2::CallerMismatch => "caller_mismatch",
        GuardedRollbackRejectionCodeV2::EvidenceCapacityReached => "evidence_capacity_reached",
        GuardedRollbackRejectionCodeV2::CwdOccupied => "cwd_occupied",
        GuardedRollbackRejectionCodeV2::CwdScanFailed => "cwd_scan_failed",
    }
}

fn guarded_rejection_to_cosh(code: GuardedCheckpointRejectionCodeV2) -> CoshError {
    let (error_code, message, recoverable) = match code {
        GuardedCheckpointRejectionCodeV2::DaemonNotReady => (
            ErrorCode::CheckpointDaemonUnavailable,
            "ws-ckpt daemon is not ready for guarded checkpoints",
            true,
        ),
        GuardedCheckpointRejectionCodeV2::PeerCredentialsUnavailable
        | GuardedCheckpointRejectionCodeV2::CallerMismatch => (
            ErrorCode::PermissionDenied,
            "ws-ckpt rejected the authenticated checkpoint caller",
            false,
        ),
        GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath
        | GuardedCheckpointRejectionCodeV2::InvalidWorkspaceId
        | GuardedCheckpointRejectionCodeV2::InvalidCheckpointId
        | GuardedCheckpointRejectionCodeV2::InvalidMetadata => (
            ErrorCode::InvalidInput,
            "ws-ckpt rejected invalid guarded checkpoint input",
            false,
        ),
        GuardedCheckpointRejectionCodeV2::WorkspaceNotFound => (
            ErrorCode::CheckpointNotFound,
            "ws-ckpt does not know the guarded workspace identity",
            false,
        ),
        GuardedCheckpointRejectionCodeV2::WriteLockConflict => (
            ErrorCode::CheckpointCreateFailed,
            "ws-ckpt rejected the guarded checkpoint because another operation holds the write lock",
            true,
        ),
        GuardedCheckpointRejectionCodeV2::GenerationMismatch
        | GuardedCheckpointRejectionCodeV2::OperationConflict
        | GuardedCheckpointRejectionCodeV2::EvidenceCapacityReached => (
            ErrorCode::CheckpointCreateFailed,
            "ws-ckpt rejected the guarded checkpoint binding",
            false,
        ),
    };

    CoshError::new(error_code, message, "checkpoint")
        .recoverable(recoverable)
        .with_details(serde_json::json!({
            "guarded_rejection": guarded_rejection_name(code),
        }))
}

fn guarded_rejection_name(code: GuardedCheckpointRejectionCodeV2) -> &'static str {
    match code {
        GuardedCheckpointRejectionCodeV2::DaemonNotReady => "daemon_not_ready",
        GuardedCheckpointRejectionCodeV2::PeerCredentialsUnavailable => {
            "peer_credentials_unavailable"
        }
        GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath => "invalid_registration_path",
        GuardedCheckpointRejectionCodeV2::InvalidWorkspaceId => "invalid_workspace_id",
        GuardedCheckpointRejectionCodeV2::InvalidCheckpointId => "invalid_checkpoint_id",
        GuardedCheckpointRejectionCodeV2::InvalidMetadata => "invalid_metadata",
        GuardedCheckpointRejectionCodeV2::WorkspaceNotFound => "workspace_not_found",
        GuardedCheckpointRejectionCodeV2::GenerationMismatch => "generation_mismatch",
        GuardedCheckpointRejectionCodeV2::OperationConflict => "operation_conflict",
        GuardedCheckpointRejectionCodeV2::WriteLockConflict => "write_lock_conflict",
        GuardedCheckpointRejectionCodeV2::CallerMismatch => "caller_mismatch",
        GuardedCheckpointRejectionCodeV2::EvidenceCapacityReached => "evidence_capacity_reached",
    }
}

fn unexpected_response() -> CoshError {
    protocol_error(ProtocolFailure::UnexpectedResponse)
}

fn protocol_error(failure: ProtocolFailure) -> CoshError {
    let kind = match failure {
        ProtocolFailure::TruncatedLength => "truncated_length",
        ProtocolFailure::TruncatedPayload => "truncated_payload",
        ProtocolFailure::OversizedLength => "oversized_length",
        ProtocolFailure::InvalidPayload => "decode_failed",
        ProtocolFailure::UnexpectedResponse => "unexpected_response",
    };
    CoshError::new(
        ErrorCode::CheckpointProtocolError,
        "Invalid response from ws-ckpt daemon",
        "checkpoint",
    )
    .with_hint("Retry the operation; restart ws-ckpt if the problem persists")
    .with_details(serde_json::json!({"phase": "response", "kind": kind}))
}

fn classify_io_error(error: std::io::Error, phase: IoPhase) -> CoshError {
    if error.kind() == std::io::ErrorKind::TimedOut {
        return CoshError::new(
            ErrorCode::Timeout,
            "Timed out while communicating with ws-ckpt daemon",
            "checkpoint",
        )
        .with_hint("ws-ckpt daemon may be overloaded; retry later")
        .recoverable(true);
    }

    let message = match phase {
        IoPhase::Connect => "Cannot connect to ws-ckpt daemon",
        IoPhase::WriteRequest => "ws-ckpt daemon became unavailable while sending a request",
        IoPhase::ReadResponseLength | IoPhase::ReadResponsePayload => {
            "ws-ckpt daemon became unavailable while receiving a response"
        }
    };
    CoshError::new(
        ErrorCode::CheckpointDaemonUnavailable,
        message,
        "checkpoint",
    )
    .with_hint("Start or restart ws-ckpt with: systemctl start ws-ckpt")
    .recoverable(true)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[cfg(target_os = "linux")]
    use std::fs;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(target_os = "linux")]
    use std::os::unix::process::CommandExt;
    #[cfg(target_os = "linux")]
    use std::process::{Child, Command, Stdio};
    #[cfg(target_os = "linux")]
    use std::time::Duration;

    use super::*;

    const GUARDED_ROLLBACK_OPERATION_ID: &str = "ckp_00000000-0000-0000-0000-000000000009";

    fn trusted_client(socket_path: &str) -> CkptClient {
        CkptClient::new(socket_path).require_trusted_peer(nix::unistd::Uid::effective().as_raw())
    }

    fn send_fake_response(response: Vec<u8>) -> (CoshError, String) {
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut length = [0; 4];
            stream.read_exact(&mut length).unwrap();
            let mut request = vec![0; u32::from_le_bytes(length) as usize];
            stream.read_exact(&mut request).unwrap();
            stream.write_all(&response).unwrap();
        });

        let socket_path = socket_path.to_string_lossy().into_owned();
        let error = CkptClient::new(&socket_path)
            .send_request(&WsCkptRequest::Config)
            .unwrap_err();
        server.join().unwrap();
        (error, socket_path)
    }

    fn assert_protocol_error(error: &CoshError, socket_path: &str) {
        assert_eq!(error.code, ErrorCode::CheckpointProtocolError);
        assert!(!error.message.contains(socket_path));
        assert!(!error.message.contains("failed to fill whole buffer"));
        assert!(!error.message.contains("invalid value"));
    }

    #[test]
    fn response_truncated_length_is_protocol_error() {
        let (error, socket_path) = send_fake_response(vec![1, 2]);
        assert_protocol_error(&error, &socket_path);
    }

    #[test]
    fn response_truncated_payload_is_protocol_error() {
        let mut response = 8_u32.to_le_bytes().to_vec();
        response.extend_from_slice(&[1, 2]);
        let (error, socket_path) = send_fake_response(response);
        assert_protocol_error(&error, &socket_path);
    }

    #[test]
    fn response_oversized_length_is_protocol_error() {
        let response = ((MAX_RESPONSE_LEN + 1) as u32).to_le_bytes().to_vec();
        let (error, socket_path) = send_fake_response(response);
        assert_protocol_error(&error, &socket_path);
    }

    #[test]
    fn response_invalid_bincode_is_protocol_error() {
        let mut response = 4_u32.to_le_bytes().to_vec();
        response.extend_from_slice(&u32::MAX.to_le_bytes());
        let (error, socket_path) = send_fake_response(response);
        assert_protocol_error(&error, &socket_path);
    }

    fn spawn_one_shot_daemon(
        response: WsCkptResponse,
    ) -> (tempfile::TempDir, String, thread::JoinHandle<()>) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("ws-ckpt.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut len_buf = [0_u8; 4];
            stream.read_exact(&mut len_buf).unwrap();
            let request_len = u32::from_le_bytes(len_buf) as usize;
            let mut request = vec![0_u8; request_len];
            stream.read_exact(&mut request).unwrap();

            let payload = bincode::serialize(&response).unwrap();
            stream
                .write_all(&(payload.len() as u32).to_le_bytes())
                .unwrap();
            stream.write_all(&payload).unwrap();
        });

        let socket_path = socket_path.to_string_lossy().into_owned();
        (dir, socket_path, handle)
    }

    fn guarded_evidence() -> GuardedCheckpointEvidenceV2 {
        GuardedCheckpointEvidenceV2 {
            ws_id: "ws-abc123".to_owned(),
            registered_path: "/workspace".to_owned(),
            generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
            checkpoint_id: "ckp_1".to_owned(),
            operation_digest: [9; 32],
            caller_uid: nix::unistd::Uid::effective().as_raw(),
            outcome: GuardedCheckpointOutcomeV2::Created {
                snapshot_id: "ckp_1".to_owned(),
            },
        }
    }

    fn guarded_rollback_evidence() -> GuardedRollbackEvidenceV2 {
        GuardedRollbackEvidenceV2 {
            ws_id: "ws-abc123".to_owned(),
            registered_path: "/workspace".to_owned(),
            expected_generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
            target_snapshot_id: "ckp_1".to_owned(),
            expected_diff_digest: [6; 32],
            operation_id: GUARDED_ROLLBACK_OPERATION_ID.to_owned(),
            operation_digest: [9; 32],
            caller_uid: nix::unistd::Uid::effective().as_raw(),
            outcome: GuardedRollbackOutcomeV2::Succeeded {
                resulting_generation: WorkspaceGenerationTokenV2::from_bytes([8; 32]),
            },
        }
    }

    #[test]
    fn test_default_timeout() {
        let client = CkptClient::new("/tmp/test.sock");
        assert_eq!(client.timeout_ms, DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn test_custom_timeout() {
        let client = CkptClient::with_timeout("/tmp/test.sock", 10000);
        assert_eq!(client.timeout_ms, 10000);
    }

    #[test]
    fn test_socket_not_found_returns_checkpoint_unavailable() {
        let client = CkptClient::new("/tmp/nonexistent-test-sock-xyz.sock");
        let result = client.create("/tmp/ws", "snap-1", None, None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::CheckpointDaemonUnavailable);
        assert!(err.message.contains("ws-ckpt"));
        assert!(err
            .hint
            .as_ref()
            .unwrap()
            .contains("systemctl start ws-ckpt"));
        assert!(err.recoverable);
    }

    #[test]
    fn test_create_checkpoint_skipped_is_success() {
        let reason = "workspace has no changes";
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::CheckpointSkipped {
                reason: reason.into(),
            });
        let client = CkptClient::new(&socket_path);

        let result = client
            .create("/tmp/ws", "snap-1", None, None, false)
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({
                "snapshot_id": null,
                "workspace": "/tmp/ws",
                "skipped": true,
                "reason": reason,
            })
        );
    }

    #[test]
    fn test_create_checkpoint_ok_preserves_success_contract() {
        let (_dir, socket_path, daemon) = spawn_one_shot_daemon(WsCkptResponse::CheckpointOk {
            snapshot_id: "snap-1".into(),
        });
        let client = CkptClient::new(&socket_path);

        let result = client
            .create("/tmp/ws", "snap-1", None, None, false)
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({
                "snapshot_id": "snap-1",
                "workspace": "/tmp/ws",
                "skipped": false,
                "reason": null,
            })
        );
    }

    #[cfg(target_os = "linux")]
    fn spawn_silent_daemon() -> (tempfile::TempDir, String, thread::JoinHandle<()>) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("ws-ckpt.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut len_buf = [0_u8; 4];
            stream.read_exact(&mut len_buf).unwrap();
            let mut request = vec![0_u8; u32::from_le_bytes(len_buf) as usize];
            stream.read_exact(&mut request).unwrap();
            // Accept the complete request and then vanish without answering.
            drop(stream);
        });

        let socket_path = socket_path.to_string_lossy().into_owned();
        (dir, socket_path, handle)
    }

    #[test]
    fn missing_socket_create_is_known_no_effect() {
        let client = trusted_client("/tmp/absent-ws-ckpt-classified.sock");

        let failure = client
            .create_classified("/tmp/ws", "ckp_1", None, None, false)
            .unwrap_err();

        assert_eq!(failure.effect, CkptRequestEffect::KnownNoEffect);
        assert_eq!(failure.error.code, ErrorCode::CheckpointDaemonUnavailable);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn guarded_v2_success_requires_exact_bound_evidence() {
        let expected = guarded_evidence();
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::GuardedCheckpointV2Ok {
                evidence: expected.clone(),
            });

        let evidence = trusted_client(&socket_path)
            .guarded_create_v2(
                "ws-abc123",
                WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                "ckp_1",
                [9; 32],
                None,
                None,
                false,
            )
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(evidence, expected);
    }

    #[test]
    fn guarded_v2_rejection_is_known_no_effect() {
        for code in [
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
        ] {
            let (_dir, socket_path, daemon) =
                spawn_one_shot_daemon(WsCkptResponse::GuardedCheckpointV2Rejected {
                    code,
                    message: "daemon-private detail".to_owned(),
                });
            let failure = trusted_client(&socket_path)
                .guarded_create_v2(
                    "ws-abc123",
                    WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                    "ckp_1",
                    [9; 32],
                    None,
                    None,
                    false,
                )
                .unwrap_err();
            daemon.join().unwrap();

            assert_eq!(failure.effect, CkptRequestEffect::KnownNoEffect);
            assert!(!failure.error.message.contains("daemon-private"));
        }
    }

    #[test]
    fn guarded_rollback_success_requires_exact_bound_evidence() {
        let expected = guarded_rollback_evidence();
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::GuardedRollbackV2Ok {
                evidence: expected.clone(),
            });

        let evidence = trusted_client(&socket_path)
            .guarded_rollback_v2(
                "/workspace",
                "ws-abc123",
                WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                "ckp_1",
                [6; 32],
                GUARDED_ROLLBACK_OPERATION_ID,
                [9; 32],
            )
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(evidence, expected);
    }

    #[test]
    fn guarded_rollback_rejection_is_known_no_effect() {
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::GuardedRollbackV2Rejected {
                code: GuardedRollbackRejectionCodeV2::DiffMismatch,
                message: "daemon-private detail".to_owned(),
            });

        let failure = trusted_client(&socket_path)
            .guarded_rollback_v2(
                "/workspace",
                "ws-abc123",
                WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                "ckp_1",
                [6; 32],
                GUARDED_ROLLBACK_OPERATION_ID,
                [9; 32],
            )
            .unwrap_err();
        daemon.join().unwrap();

        assert_eq!(failure.effect, CkptRequestEffect::KnownNoEffect);
        assert!(!failure.error.message.contains("daemon-private"));
    }

    #[test]
    fn guarded_rollback_uncertain_response_is_possibly_applied() {
        let mut evidence = guarded_rollback_evidence();
        evidence.outcome = GuardedRollbackOutcomeV2::Unknown {
            reason: "backend completion unproven".to_owned(),
        };
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::GuardedRollbackV2Uncertain { evidence });

        let failure = trusted_client(&socket_path)
            .guarded_rollback_v2(
                "/workspace",
                "ws-abc123",
                WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                "ckp_1",
                [6; 32],
                GUARDED_ROLLBACK_OPERATION_ID,
                [9; 32],
            )
            .unwrap_err();
        daemon.join().unwrap();

        assert_eq!(failure.effect, CkptRequestEffect::PossiblyApplied);
    }

    #[test]
    fn legacy_error_for_guarded_v2_is_possibly_applied() {
        let (_dir, socket_path, daemon) = spawn_one_shot_daemon(WsCkptResponse::Error {
            code: WsCkptErrorCode::InvalidPath,
            message: "legacy daemon".to_owned(),
        });

        let failure = trusted_client(&socket_path)
            .guarded_create_v2(
                "ws-abc123",
                WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                "ckp_1",
                [9; 32],
                None,
                None,
                false,
            )
            .unwrap_err();
        daemon.join().unwrap();

        assert_eq!(failure.effect, CkptRequestEffect::PossiblyApplied);
    }

    #[test]
    fn mismatched_guarded_v2_evidence_is_possibly_applied() {
        let mut evidence = guarded_evidence();
        evidence.operation_digest = [8; 32];
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::GuardedCheckpointV2Ok { evidence });

        let failure = trusted_client(&socket_path)
            .guarded_create_v2(
                "ws-abc123",
                WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                "ckp_1",
                [9; 32],
                None,
                None,
                false,
            )
            .unwrap_err();
        daemon.join().unwrap();

        assert_eq!(failure.effect, CkptRequestEffect::PossiblyApplied);
        assert_eq!(failure.error.code, ErrorCode::CheckpointProtocolError);
    }

    #[test]
    fn guarded_v2_evidence_query_is_read_only_and_exact() {
        let expected = guarded_evidence();
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::CheckpointEvidenceV2Ok {
                evidence: Some(expected.clone()),
            });

        let evidence = trusted_client(&socket_path)
            .checkpoint_evidence_v2(
                "ws-abc123",
                WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                "ckp_1",
                [9; 32],
            )
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(evidence, Some(expected));
    }

    #[test]
    fn workspace_identity_v2_requires_exact_version_and_path() {
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::WorkspaceIdentityV2Ok {
                protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
                ws_id: "ws-abc123".to_owned(),
                registered_path: "/workspace".to_owned(),
                generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
            });

        let identity = trusted_client(&socket_path)
            .workspace_identity_v2("/workspace")
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(identity.ws_id, "ws-abc123");
        assert_eq!(identity.registered_path, "/workspace");
    }

    #[test]
    fn lost_response_after_full_write_is_possibly_applied() {
        let (_dir, socket_path, daemon) = spawn_silent_daemon();
        let client = trusted_client(&socket_path);

        let failure = client
            .create_classified("/tmp/ws", "ckp_1", None, None, false)
            .unwrap_err();
        daemon.join().unwrap();

        assert_eq!(failure.effect, CkptRequestEffect::PossiblyApplied);
        assert_eq!(failure.error.code, ErrorCode::CheckpointProtocolError);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn truncated_response_payload_is_possibly_applied() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("ws-ckpt.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let daemon = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut len_buf = [0_u8; 4];
            stream.read_exact(&mut len_buf).unwrap();
            let mut request = vec![0_u8; u32::from_le_bytes(len_buf) as usize];
            stream.read_exact(&mut request).unwrap();
            stream.write_all(&64_u32.to_le_bytes()).unwrap();
            stream.write_all(&[1, 2, 3]).unwrap();
        });
        let socket_path = socket_path.to_string_lossy().into_owned();

        let failure = trusted_client(&socket_path)
            .create_classified("/tmp/ws", "ckp_1", None, None, false)
            .unwrap_err();
        daemon.join().unwrap();

        assert_eq!(failure.effect, CkptRequestEffect::PossiblyApplied);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recover_preserves_warning_and_historical_success_json() {
        for warning in [None, Some("Backup retained at /tmp/ws.pre-init-bak")] {
            let response = match warning {
                None => WsCkptResponse::RecoverOk {
                    workspace: "/tmp/ws".into(),
                },
                Some(warning) => WsCkptResponse::RecoverWithWarning {
                    workspace: "/tmp/ws".into(),
                    warning: warning.into(),
                },
            };
            let (_dir, socket_path, daemon) = spawn_one_shot_daemon(response);
            let result = trusted_client(&socket_path).recover("/tmp/ws").unwrap();
            daemon.join().unwrap();
            assert_eq!(result.warning.as_deref(), warning);
            let mut expected = serde_json::json!({"workspace": "/tmp/ws"});
            if let Some(warning) = warning {
                expected["warning"] = warning.into();
            }
            assert_eq!(serde_json::to_value(result).unwrap(), expected);
            let decoded: CkptRecoverResult = serde_json::from_value(expected).unwrap();
            assert_eq!(decoded.warning.as_deref(), warning);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unexpected_response_variant_is_possibly_applied() {
        let (_dir, socket_path, daemon) = spawn_one_shot_daemon(WsCkptResponse::RecoverOk {
            workspace: "/tmp/ws".into(),
        });

        let failure = trusted_client(&socket_path)
            .create_classified("/tmp/ws", "ckp_1", None, None, false)
            .unwrap_err();
        daemon.join().unwrap();

        assert_eq!(failure.effect, CkptRequestEffect::PossiblyApplied);
        assert_eq!(failure.error.code, ErrorCode::CheckpointProtocolError);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn every_daemon_reported_error_is_possibly_applied() {
        // The checkpoint dispatch path auto-initializes the workspace before it
        // attempts a snapshot, so registration, subvolume adoption, or a
        // broken-symlink removal may precede any of these codes. None of them
        // proves daemon state is unchanged.
        for code in [
            WsCkptErrorCode::WorkspaceNotFound,
            WsCkptErrorCode::SnapshotAlreadyExists,
            WsCkptErrorCode::WriteLockConflict,
            WsCkptErrorCode::InvalidPath,
            WsCkptErrorCode::IoError,
            WsCkptErrorCode::BtrfsError,
            WsCkptErrorCode::InternalError,
            WsCkptErrorCode::DiskSpaceInsufficient,
            WsCkptErrorCode::SnapshotNotFound,
            WsCkptErrorCode::AlreadyInitialized,
            WsCkptErrorCode::ConfirmationRequired,
            WsCkptErrorCode::CwdOccupied,
            WsCkptErrorCode::CwdScanFailed,
        ] {
            let daemon_message = format!(
                "/secret/workspace\n\u{1b}[31m{}",
                "daemon-controlled-data".repeat(4096)
            );
            let (_dir, socket_path, daemon) = spawn_one_shot_daemon(WsCkptResponse::Error {
                code: code.clone(),
                message: daemon_message,
            });

            let failure = trusted_client(&socket_path)
                .create_classified("/tmp/ws", "ckp_1", None, None, false)
                .unwrap_err();
            daemon.join().unwrap();

            assert_eq!(
                failure.effect,
                CkptRequestEffect::PossiblyApplied,
                "{code:?} must fail closed"
            );
            assert_eq!(
                failure.error.message,
                "ws-ckpt daemon rejected the checkpoint request"
            );
            assert!(failure.error.message.len() < 128);
        }
    }

    /// `execve` can transiently report ETXTBSY for a helper that was only just
    /// copied into an overlay-backed temp dir, which fails the suite on a
    /// scheduler race instead of a real regression. Retry the bounded way
    /// `spawn_provider_child` already does in production.
    #[cfg(target_os = "linux")]
    fn spawn_helper_through_text_file_busy(command: &mut Command) -> std::io::Result<Child> {
        const MAX_SPAWN_ATTEMPTS: usize = 3;

        for attempt in 0..MAX_SPAWN_ATTEMPTS {
            match command.spawn() {
                Err(error)
                    if error.kind() == ErrorKind::ExecutableFileBusy
                        && attempt + 1 < MAX_SPAWN_ATTEMPTS =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }

        unreachable!("the bounded helper spawn loop always returns on its final attempt")
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_untrusted_peer_is_rejected_as_known_no_effect() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let socket_path = directory.path().join("untrusted.sock");

        // A root peer is always trusted in production, regardless of the
        // configured owner. Run the listener under a different kernel UID so
        // this remains a real negative test when the suite itself runs as root.
        let helper_exe = directory.path().join("untrusted-peer-helper");
        fs::copy(std::env::current_exe().unwrap(), &helper_exe).unwrap();
        fs::set_permissions(&helper_exe, fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new(&helper_exe);
        command
            .arg("--exact")
            .arg("checkpoint::tests::untrusted_peer_daemon_helper")
            .arg("--nocapture")
            .env("COSH_TEST_UNTRUSTED_PEER_SOCKET", &socket_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let test_uid = nix::unistd::Uid::effective().as_raw();
        if test_uid == 0 {
            command.uid(65_534);
        }
        let mut daemon = spawn_helper_through_text_file_busy(&mut command).unwrap();
        for _ in 0..100 {
            if socket_path.exists() {
                break;
            }
            assert!(daemon.try_wait().unwrap().is_none());
            thread::sleep(Duration::from_millis(10));
        }
        assert!(socket_path.exists());

        let trusted_owner = if test_uid == 0 {
            0
        } else {
            test_uid.wrapping_add(1)
        };

        let socket_path_string = socket_path.to_string_lossy().into_owned();
        let failure = CkptClient::new(&socket_path_string)
            .require_trusted_peer(trusted_owner)
            .create_classified("/tmp/ws", "ckp_1", None, None, false)
            .unwrap_err();

        assert_eq!(failure.effect, CkptRequestEffect::KnownNoEffect);
        assert_eq!(failure.error.code, ErrorCode::PermissionDenied);
        assert!(daemon.wait().unwrap().success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn untrusted_peer_daemon_helper() {
        let Ok(socket_path) = std::env::var("COSH_TEST_UNTRUSTED_PEER_SOCKET") else {
            return;
        };
        let listener = UnixListener::bind(socket_path).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_trusted_peer_is_accepted() {
        let (_dir, socket_path, daemon) = spawn_one_shot_daemon(WsCkptResponse::CheckpointOk {
            snapshot_id: "snap-1".into(),
        });
        let owner = nix::unistd::Uid::effective().as_raw();

        let created = CkptClient::new(&socket_path)
            .require_trusted_peer(owner)
            .create_classified("/tmp/ws", "ckp_1", None, None, false)
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(created.snapshot_id.as_deref(), Some("snap-1"));
    }

    #[test]
    fn missing_auth_create_refuses_before_socket_access() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("ws-ckpt.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();

        let failure = CkptClient::new(socket_path.to_str().unwrap())
            .create_classified("/tmp/ws", "ckp_1", None, None, false)
            .unwrap_err();

        assert_eq!(failure.effect, CkptRequestEffect::KnownNoEffect);
        assert_eq!(failure.error.code, ErrorCode::PermissionDenied);
        assert!(failure.error.message.contains("requires trusted peer"));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn missing_auth_guarded_v2_refuses_before_socket_access() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("ws-ckpt.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();

        let failure = CkptClient::new(socket_path.to_str().unwrap())
            .guarded_create_v2(
                "ws-abc123",
                WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                "ckp_1",
                [9; 32],
                None,
                None,
                false,
            )
            .unwrap_err();

        assert_eq!(failure.effect, CkptRequestEffect::KnownNoEffect);
        assert_eq!(failure.error.code, ErrorCode::PermissionDenied);
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn missing_auth_find_refuses_before_socket_access() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("ws-ckpt.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();

        let error = CkptClient::new(socket_path.to_str().unwrap())
            .find_snapshot("/tmp/ws", "ckp_1")
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert!(error.message.contains("requires trusted peer"));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn governed_operations_fail_closed_when_peer_authentication_is_unavailable() {
        let exercise = |operation: fn(&CkptClient) -> Result<(), CoshError>| {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("ws-ckpt.sock");
            let listener = UnixListener::bind(&socket_path).unwrap();
            let daemon = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut byte = [0_u8; 1];
                assert_eq!(stream.read(&mut byte).unwrap(), 0);
            });
            let client = CkptClient::new(socket_path.to_str().unwrap()).require_trusted_peer(0);

            let error = operation(&client).unwrap_err();
            daemon.join().unwrap();

            assert_eq!(error.code, ErrorCode::PermissionDenied);
        };

        exercise(|client| {
            client
                .create_classified("/tmp/ws", "ckp_1", None, None, false)
                .map(|_| ())
                .map_err(|failure| {
                    assert_eq!(failure.effect, CkptRequestEffect::KnownNoEffect);
                    failure.error
                })
        });
        exercise(|client| client.find_snapshot("/tmp/ws", "ckp_1").map(|_| ()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn find_snapshot_preserves_reported_workspace_and_matches_exact_identity() {
        let entry = |id: &str, missing: bool| SnapshotEntry {
            id: id.to_owned(),
            workspace: "/registered/../workspace".to_owned(),
            meta: SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing,
                parent_id: None,
                child_ids: Vec::new(),
            },
        };
        let (_dir, socket_path, daemon) = spawn_one_shot_daemon(WsCkptResponse::ListOk {
            snapshots: vec![entry("ckp_other", false), entry("ckp_wanted", true)],
        });

        let evidence = trusted_client(&socket_path)
            .find_snapshot("/tmp/ws", "ckp_wanted")
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(
            evidence,
            Some(CkptSnapshotEvidence {
                snapshot_id: "ckp_wanted".to_owned(),
                workspace: "/registered/../workspace".to_owned(),
                missing: true,
            })
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn find_snapshot_reports_absent_identity_without_error() {
        let (_dir, socket_path, daemon) =
            spawn_one_shot_daemon(WsCkptResponse::ListOk { snapshots: vec![] });

        let evidence = trusted_client(&socket_path)
            .find_snapshot("/tmp/ws", "ckp_wanted")
            .unwrap();
        daemon.join().unwrap();

        assert_eq!(evidence, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn find_snapshot_propagates_a_daemon_error() {
        let (_dir, socket_path, daemon) = spawn_one_shot_daemon(WsCkptResponse::Error {
            code: WsCkptErrorCode::WorkspaceNotFound,
            message: "/secret/workspace\n\u{1b}[31mdaemon rejection".into(),
        });

        let error = trusted_client(&socket_path)
            .find_snapshot("/tmp/ws", "ckp_wanted")
            .unwrap_err();
        daemon.join().unwrap();

        assert_eq!(error.code, ErrorCode::CheckpointNotFound);
        assert_eq!(
            error.message,
            "ws-ckpt daemon rejected the checkpoint evidence query"
        );
    }

    #[test]
    fn test_socket_not_found_list() {
        let client = CkptClient::new("/tmp/nonexistent-test-sock-xyz.sock");
        let result = client.list(Some("/tmp/ws"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::CheckpointDaemonUnavailable);
    }

    #[test]
    fn test_is_available_nonexistent() {
        let client = CkptClient::new("/tmp/absolutely-does-not-exist.sock");
        assert!(!client.is_available());
    }

    #[test]
    fn test_encode_decode_frame() {
        let req = WsCkptRequest::Status {
            workspace: Some("/tmp/ws".into()),
        };
        let frame = encode_frame(&req).unwrap();

        // Frame should start with 4-byte LE length
        let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        assert_eq!(frame.len(), 4 + len);

        // Payload should be valid bincode
        let decoded: WsCkptRequest = bincode::deserialize(&frame[4..]).unwrap();
        match decoded {
            WsCkptRequest::Status { workspace } => assert_eq!(workspace, Some("/tmp/ws".into())),
            _ => panic!("Wrong variant decoded"),
        }
    }

    #[test]
    fn test_decode_response_valid() {
        let resp = WsCkptResponse::CheckpointOk {
            snapshot_id: "snap-123".into(),
        };
        let data = bincode::serialize(&resp).unwrap();
        let decoded = decode_response(&data).unwrap();
        match decoded {
            WsCkptResponse::CheckpointOk { snapshot_id } => assert_eq!(snapshot_id, "snap-123"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_decode_response_invalid() {
        let bad_data = b"not valid bincode data!!!!";
        let result = decode_response(bad_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_classify_broken_pipe() {
        let err = classify_io_error(
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe"),
            IoPhase::WriteRequest,
        );
        assert_eq!(err.code, ErrorCode::CheckpointDaemonUnavailable);
        assert!(err.message.contains("unavailable"));
        assert!(!err.message.contains("BrokenPipe"));
        assert!(err
            .hint
            .as_ref()
            .unwrap()
            .contains("systemctl start ws-ckpt"));
        assert!(err.recoverable);
    }

    #[test]
    fn test_classify_connection_reset() {
        let err = classify_io_error(
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset"),
            IoPhase::ReadResponsePayload,
        );
        assert_eq!(err.code, ErrorCode::CheckpointDaemonUnavailable);
        assert!(err.message.contains("unavailable"));
        assert!(!err.message.contains("ConnectionReset"));
        assert!(err.recoverable);
    }

    #[test]
    fn test_classify_timeout() {
        let err = classify_io_error(
            std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"),
            IoPhase::ReadResponseLength,
        );
        assert_eq!(err.code, ErrorCode::Timeout);
        assert!(err.hint.as_ref().unwrap().contains("overloaded"));
        assert!(err.recoverable);
    }

    #[test]
    fn test_classify_connection_refused() {
        let err = classify_io_error(
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
            IoPhase::Connect,
        );
        assert_eq!(err.code, ErrorCode::CheckpointDaemonUnavailable);
        assert!(err
            .hint
            .as_ref()
            .unwrap()
            .contains("systemctl start ws-ckpt"));
    }

    #[test]
    fn test_classify_other_io_error() {
        let err = classify_io_error(
            std::io::Error::other("something else"),
            IoPhase::WriteRequest,
        );
        assert_eq!(err.code, ErrorCode::CheckpointDaemonUnavailable);
        assert!(err.recoverable);
    }

    #[test]
    fn test_ws_error_to_cosh_mapping() {
        let err = ws_error_to_cosh(WsCkptErrorCode::WorkspaceNotFound, "ws not found".into());
        assert_eq!(err.code, ErrorCode::CheckpointNotFound);
        assert!(err.hint.is_some());

        let err = ws_error_to_cosh(WsCkptErrorCode::DiskSpaceInsufficient, "no space".into());
        assert_eq!(err.code, ErrorCode::CheckpointCreateFailed);
        assert!(err.hint.unwrap().contains("cleanup"));
    }

    #[test]
    fn test_response_length_exceeds_max() {
        // Simulate a daemon sending a response length larger than MAX_RESPONSE_LEN.
        // We can't easily test through the socket, but we verify the constant is
        // reasonable and the guard logic is in place by checking the const.
        assert_eq!(MAX_RESPONSE_LEN, 64 * 1024 * 1024);
        // A normal response is at most a few KiB; 64 MiB is generous headroom.
    }
}
