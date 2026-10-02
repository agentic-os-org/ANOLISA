// SPDX-License-Identifier: Apache-2.0
//! Sandbox lifecycle state machine + JSON persistence.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::BackendKind;
use crate::error::{BlazeError, Result};
use crate::policy::WorkloadClass;

/// All known states. Transitions are enforced by [`SandboxInstance::transition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxState {
    Pending,
    Creating,
    Running,
    Paused,
    Checkpointed,
    /// The previous backend is stopped while replacement resources are owned.
    Restoring,
    /// A live backend is being converted into durable hibernation artifacts.
    Hibernating,
    /// Durable hibernation artifacts and storage are retained without a backend.
    Hibernated,
    /// A backend is being started from retained hibernation artifacts.
    Resuming,
    RecoveryRequired,
    Reset,
    Warm,
    Destroyed,
}

impl SandboxState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            SandboxState::Pending => "pending",
            SandboxState::Creating => "creating",
            SandboxState::Running => "running",
            SandboxState::Paused => "paused",
            SandboxState::Checkpointed => "checkpointed",
            SandboxState::Restoring => "restoring",
            SandboxState::Hibernating => "hibernating",
            SandboxState::Hibernated => "hibernated",
            SandboxState::Resuming => "resuming",
            SandboxState::RecoveryRequired => "recovery-required",
            SandboxState::Reset => "reset",
            SandboxState::Warm => "warm",
            SandboxState::Destroyed => "destroyed",
        }
    }
}

/// Persisted multi-step operation used for crash diagnosis and recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationKind {
    /// Sandbox creation is acquiring resources or starting a backend.
    Create,
    /// A point-in-time checkpoint is being captured and published.
    Checkpoint,
    /// Unreachable checkpoint branches are being removed.
    Prune,
    /// A running sandbox is being replaced from a selected checkpoint.
    Restore,
    /// A live backend is being stopped after durable artifacts are prepared.
    Hibernate,
    /// A backend is being started from retained hibernation artifacts.
    Resume,
    /// Runtime resources are being destroyed.
    Destroy,
}

impl OperationKind {
    const fn as_str(&self) -> &'static str {
        match self {
            OperationKind::Create => "create",
            OperationKind::Checkpoint => "checkpoint",
            OperationKind::Prune => "prune",
            OperationKind::Restore => "restore",
            OperationKind::Hibernate => "hibernate",
            OperationKind::Resume => "resume",
            OperationKind::Destroy => "destroy",
        }
    }
}

impl std::fmt::Display for OperationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Durable boundary reached by a multi-step lifecycle operation.
///
/// The journal keeps this separate from [`SandboxState`]: state describes
/// externally visible runtime availability, while the phase identifies the
/// last resource-ownership or catalog boundary committed before interruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationPhase {
    /// A staging directory exists, but the backend has not been paused.
    CheckpointPreparing,
    /// The backend is paused while snapshot artifacts are being written.
    CheckpointPaused,
    /// A complete checkpoint directory is visible, but HEAD is unchanged.
    CheckpointPublished,
    /// HEAD references the checkpoint; runtime resume is not yet committed.
    CheckpointHeadUpdated,
    /// Restore intent is durable, but the current runtime is still owned.
    RestorePreparing,
    /// Replacement storage is staged without changing the live rootfs.
    RestoreStorageStaged,
    /// The current backend has been confirmed stopped.
    RestoreBackendStopped,
    /// Staged storage is active while the predecessor remains recoverable.
    RestoreStorageActivated,
    /// A replacement backend has started and is owned by the runtime.
    RestoreBackendStarted,
    /// HEAD references the restored checkpoint; storage and lifecycle commits remain.
    RestoreHeadUpdated,
    /// The storage replacement is committed and can no longer be aborted.
    RestoreStorageCommitted,
    /// Hibernate intent is durable, but the backend is still running.
    HibernatePreparing,
    /// The backend is paused while hibernation artifacts are written.
    HibernatePaused,
    /// Hibernation artifacts are complete and durable.
    HibernateArtifactsSynced,
    /// The live backend has stopped and no longer owns runtime resources.
    HibernateBackendStopped,
    /// The replacement hibernation directory is durably visible.
    HibernatePublished,
    /// Resume intent is durable and no backend has started.
    ResumePreparing,
    /// Backend ownership intent is durable before restore starts.
    ResumeBackendStarting,
    /// A restored backend is owned, but readiness is not yet confirmed.
    ResumeBackendStarted,
    /// The restored backend and optional guest transport are ready.
    ResumeBackendReady,
}

impl OperationPhase {
    const fn as_str(self) -> &'static str {
        match self {
            OperationPhase::CheckpointPreparing => "checkpoint-preparing",
            OperationPhase::CheckpointPaused => "checkpoint-paused",
            OperationPhase::CheckpointPublished => "checkpoint-published",
            OperationPhase::CheckpointHeadUpdated => "checkpoint-head-updated",
            OperationPhase::RestorePreparing => "restore-preparing",
            OperationPhase::RestoreStorageStaged => "restore-storage-staged",
            OperationPhase::RestoreBackendStopped => "restore-backend-stopped",
            OperationPhase::RestoreStorageActivated => "restore-storage-activated",
            OperationPhase::RestoreBackendStarted => "restore-backend-started",
            OperationPhase::RestoreHeadUpdated => "restore-head-updated",
            OperationPhase::RestoreStorageCommitted => "restore-storage-committed",
            OperationPhase::HibernatePreparing => "hibernate-preparing",
            OperationPhase::HibernatePaused => "hibernate-paused",
            OperationPhase::HibernateArtifactsSynced => "hibernate-artifacts-synced",
            OperationPhase::HibernateBackendStopped => "hibernate-backend-stopped",
            OperationPhase::HibernatePublished => "hibernate-published",
            OperationPhase::ResumePreparing => "resume-preparing",
            OperationPhase::ResumeBackendStarting => "resume-backend-starting",
            OperationPhase::ResumeBackendStarted => "resume-backend-started",
            OperationPhase::ResumeBackendReady => "resume-backend-ready",
        }
    }

    const fn operation_kind(self) -> OperationKind {
        match self {
            OperationPhase::CheckpointPreparing
            | OperationPhase::CheckpointPaused
            | OperationPhase::CheckpointPublished
            | OperationPhase::CheckpointHeadUpdated => OperationKind::Checkpoint,
            OperationPhase::RestorePreparing
            | OperationPhase::RestoreStorageStaged
            | OperationPhase::RestoreBackendStopped
            | OperationPhase::RestoreStorageActivated
            | OperationPhase::RestoreBackendStarted
            | OperationPhase::RestoreHeadUpdated
            | OperationPhase::RestoreStorageCommitted => OperationKind::Restore,
            OperationPhase::HibernatePreparing
            | OperationPhase::HibernatePaused
            | OperationPhase::HibernateArtifactsSynced
            | OperationPhase::HibernateBackendStopped
            | OperationPhase::HibernatePublished => OperationKind::Hibernate,
            OperationPhase::ResumePreparing
            | OperationPhase::ResumeBackendStarting
            | OperationPhase::ResumeBackendStarted
            | OperationPhase::ResumeBackendReady => OperationKind::Resume,
        }
    }

    const fn rank(self) -> u8 {
        match self {
            OperationPhase::CheckpointPreparing => 0,
            OperationPhase::CheckpointPaused => 1,
            OperationPhase::CheckpointPublished => 2,
            OperationPhase::CheckpointHeadUpdated => 3,
            OperationPhase::RestorePreparing => 0,
            OperationPhase::RestoreStorageStaged => 1,
            OperationPhase::RestoreBackendStopped => 2,
            OperationPhase::RestoreStorageActivated => 3,
            OperationPhase::RestoreBackendStarted => 4,
            OperationPhase::RestoreHeadUpdated => 5,
            OperationPhase::RestoreStorageCommitted => 6,
            OperationPhase::HibernatePreparing => 0,
            OperationPhase::HibernatePaused => 1,
            OperationPhase::HibernateArtifactsSynced => 2,
            OperationPhase::HibernateBackendStopped => 3,
            OperationPhase::HibernatePublished => 4,
            OperationPhase::ResumePreparing => 0,
            OperationPhase::ResumeBackendStarting => 1,
            OperationPhase::ResumeBackendStarted => 2,
            OperationPhase::ResumeBackendReady => 3,
        }
    }
}

/// Durable journal entry for one active lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationJournal {
    /// Operation being performed.
    pub kind: OperationKind,
    /// UTC time at which the operation became externally visible.
    pub started_at: DateTime<Utc>,
    /// Checkpoint selected by this operation, when applicable.
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    /// Last durably committed operation boundary.
    #[serde(default)]
    pub phase: Option<OperationPhase>,
}

impl std::fmt::Display for SandboxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Persisted record of whether sandbox startup used a reusable instance.
///
/// New instances always use [`StartPath::Cold`]. [`StartPath::Warm`] remains
/// readable so startup reconciliation can clean records written by older releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartPath {
    /// Sandbox creation started without a reusable instance.
    Cold,
    /// Legacy reusable-instance start retained only for persisted-state compatibility.
    Warm,
}

/// Durable knowledge about whether a backend may still own a live process.
///
/// `Unknown` is the safe default for state written by older daemon versions.
/// Recovery must confirm termination for both `Unknown` and `Starting`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendOwnership {
    #[default]
    Unknown,
    NotStarted,
    Starting,
    Running,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxInstance {
    pub id: Uuid,
    pub state: SandboxState,
    pub backend: BackendKind,
    pub workload_class: WorkloadClass,
    pub image_digest: String,
    /// Catalog entry used to restore this sandbox, if it was template-backed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    pub start_path: StartPath,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub policy_name: String,
    /// Last durably known backend ownership state.
    #[serde(default)]
    pub backend_ownership: BackendOwnership,
    /// Active multi-step operation, if any.
    #[serde(default)]
    pub operation: Option<OperationJournal>,
    /// Last checkpoint whose capture completed and returned the sandbox to running.
    #[serde(default)]
    pub last_checkpoint: Option<String>,
}

impl SandboxInstance {
    /// Create a new instance in [`SandboxState::Pending`].
    pub fn new(
        backend: BackendKind,
        workload_class: WorkloadClass,
        image_digest: String,
        policy_name: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            state: SandboxState::Pending,
            backend,
            workload_class,
            image_digest,
            template: None,
            start_path: StartPath::Cold,
            created_at: now,
            updated_at: now,
            policy_name,
            backend_ownership: BackendOwnership::NotStarted,
            operation: None,
            last_checkpoint: None,
        }
    }

    /// Persist an operation before starting its first data-plane mutation.
    pub fn begin_operation(&mut self, kind: OperationKind) {
        self.operation = Some(OperationJournal {
            kind,
            started_at: Utc::now(),
            checkpoint_id: None,
            phase: None,
        });
        self.updated_at = Utc::now();
    }

    /// Record checkpoint intent before pausing the backend.
    pub fn begin_checkpoint_operation(&mut self, checkpoint_id: String) -> Result<()> {
        if let Some(active) = &self.operation {
            return Err(BlazeError::OperationInProgress {
                active: active.kind.to_string(),
                requested: OperationKind::Checkpoint.to_string(),
            });
        }
        let now = Utc::now();
        self.operation = Some(OperationJournal {
            kind: OperationKind::Checkpoint,
            started_at: now,
            checkpoint_id: Some(checkpoint_id),
            phase: Some(OperationPhase::CheckpointPreparing),
        });
        self.updated_at = now;
        Ok(())
    }

    /// Advance the active checkpoint journal without replacing its identity.
    pub fn advance_checkpoint_phase(&mut self, phase: OperationPhase) -> Result<()> {
        self.advance_operation_phase(OperationKind::Checkpoint, phase)
    }

    /// Record restore intent without changing the last completed checkpoint.
    pub fn begin_restore_operation(&mut self, checkpoint_id: String) -> Result<()> {
        if let Some(active) = &self.operation {
            return Err(BlazeError::OperationInProgress {
                active: active.kind.to_string(),
                requested: OperationKind::Restore.to_string(),
            });
        }
        let now = Utc::now();
        self.operation = Some(OperationJournal {
            kind: OperationKind::Restore,
            started_at: now,
            checkpoint_id: Some(checkpoint_id),
            phase: Some(OperationPhase::RestorePreparing),
        });
        self.updated_at = now;
        Ok(())
    }

    /// Advance the active restore journal without replacing its identity.
    pub fn advance_restore_phase(&mut self, phase: OperationPhase) -> Result<()> {
        self.advance_operation_phase(OperationKind::Restore, phase)
    }

    /// Record hibernation intent before pausing the backend.
    pub fn begin_hibernate_operation(&mut self) -> Result<()> {
        if let Some(active) = &self.operation {
            return Err(BlazeError::OperationInProgress {
                active: active.kind.to_string(),
                requested: OperationKind::Hibernate.to_string(),
            });
        }
        let now = Utc::now();
        self.operation = Some(OperationJournal {
            kind: OperationKind::Hibernate,
            started_at: now,
            checkpoint_id: None,
            phase: Some(OperationPhase::HibernatePreparing),
        });
        self.updated_at = now;
        Ok(())
    }

    /// Advance the active hibernation journal without replacing its identity.
    pub fn advance_hibernate_phase(&mut self, phase: OperationPhase) -> Result<()> {
        self.advance_operation_phase(OperationKind::Hibernate, phase)
    }

    /// Record resume intent before preparing a replacement backend.
    pub fn begin_resume_operation(&mut self) -> Result<()> {
        if let Some(active) = &self.operation {
            return Err(BlazeError::OperationInProgress {
                active: active.kind.to_string(),
                requested: OperationKind::Resume.to_string(),
            });
        }
        let now = Utc::now();
        self.operation = Some(OperationJournal {
            kind: OperationKind::Resume,
            started_at: now,
            checkpoint_id: None,
            phase: Some(OperationPhase::ResumePreparing),
        });
        self.updated_at = now;
        Ok(())
    }

    /// Advance the active resume journal without replacing its identity.
    pub fn advance_resume_phase(&mut self, phase: OperationPhase) -> Result<()> {
        self.advance_operation_phase(OperationKind::Resume, phase)
    }

    fn advance_operation_phase(
        &mut self,
        requested_kind: OperationKind,
        phase: OperationPhase,
    ) -> Result<()> {
        if phase.operation_kind() != requested_kind {
            return Err(BlazeError::InvalidStateTransition {
                from: requested_kind.to_string(),
                to: phase.as_str().to_string(),
            });
        }
        let operation = self
            .operation
            .as_mut()
            .ok_or_else(|| BlazeError::OperationInProgress {
                active: "none".to_string(),
                requested: requested_kind.to_string(),
            })?;
        if operation.kind != requested_kind {
            return Err(BlazeError::OperationInProgress {
                active: operation.kind.to_string(),
                requested: requested_kind.to_string(),
            });
        }
        if let Some(current) = operation.phase
            && (current.operation_kind() != requested_kind || phase.rank() < current.rank())
        {
            return Err(BlazeError::InvalidStateTransition {
                from: current.as_str().to_string(),
                to: phase.as_str().to_string(),
            });
        }
        operation.phase = Some(phase);
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Clear the marker before atomically persisting the final state.
    pub fn finish_operation(&mut self) {
        self.operation = None;
        self.updated_at = Utc::now();
    }

    /// Apply a state transition.
    ///
    /// Restore transitions additionally require the durable backend-stop and
    /// storage-commit boundaries before changing externally visible state.
    /// Returns
    /// [`BlazeError::InvalidStateTransition`] when the move is not part
    /// of the lifecycle state graph.
    pub fn transition(&mut self, target: SandboxState) -> Result<()> {
        let restore_boundary_reached = match (self.state, target) {
            (_, SandboxState::Restoring) => {
                self.restore_phase_reached(OperationPhase::RestoreBackendStopped)
            }
            (SandboxState::Restoring, SandboxState::Running) => {
                self.restore_phase_reached(OperationPhase::RestoreStorageCommitted)
            }
            _ => true,
        };
        let hibernate_boundary_reached = match (self.state, target) {
            (SandboxState::Running, SandboxState::Hibernating) => self.operation_phase_reached(
                OperationKind::Hibernate,
                OperationPhase::HibernatePreparing,
            ),
            (SandboxState::Hibernating, SandboxState::Hibernated) => {
                self.backend_ownership == BackendOwnership::Stopped
                    && self.operation_phase_reached(
                        OperationKind::Hibernate,
                        OperationPhase::HibernatePublished,
                    )
            }
            (SandboxState::Hibernating, SandboxState::Running) => {
                self.backend_ownership == BackendOwnership::Running
                    && self
                        .operation
                        .as_ref()
                        .is_some_and(|operation| operation.kind == OperationKind::Hibernate)
            }
            (SandboxState::Hibernated, SandboxState::Resuming) => {
                self.backend_ownership == BackendOwnership::Stopped
                    && self.operation_phase_reached(
                        OperationKind::Resume,
                        OperationPhase::ResumePreparing,
                    )
            }
            (SandboxState::Resuming, SandboxState::Running) => {
                self.backend_ownership == BackendOwnership::Running
                    && self.operation_phase_reached(
                        OperationKind::Resume,
                        OperationPhase::ResumeBackendReady,
                    )
            }
            (SandboxState::Resuming, SandboxState::Hibernated) => {
                self.backend_ownership == BackendOwnership::Stopped
                    && self
                        .operation
                        .as_ref()
                        .is_some_and(|operation| operation.kind == OperationKind::Resume)
            }
            _ => true,
        };
        if !restore_boundary_reached
            || !hibernate_boundary_reached
            || !is_valid_transition(self.state, target)
        {
            return Err(BlazeError::InvalidStateTransition {
                from: self.state.to_string(),
                to: target.to_string(),
            });
        }
        let prev = self.state;
        self.state = target;
        self.updated_at = Utc::now();
        tracing::info!(
            instance = %self.id,
            from = %prev,
            to = %target,
            backend = %self.backend,
            class = %self.workload_class,
            "sandbox state transition"
        );
        Ok(())
    }

    fn restore_phase_reached(&self, minimum: OperationPhase) -> bool {
        self.operation_phase_reached(OperationKind::Restore, minimum)
    }

    fn operation_phase_reached(
        &self,
        operation_kind: OperationKind,
        minimum: OperationPhase,
    ) -> bool {
        minimum.operation_kind() == operation_kind
            && self.operation.as_ref().is_some_and(|operation| {
                operation.kind == operation_kind
                    && operation.phase.is_some_and(|phase| {
                        phase.operation_kind() == operation_kind && phase.rank() >= minimum.rank()
                    })
            })
    }

    /// Persist this instance to `{state_dir}/{id}/state.json`. Atomic
    /// rename via `state.json.tmp` to avoid torn reads on daemon restart.
    pub fn persist(&self, state_dir: &Path) -> Result<()> {
        let dir = state_dir.join(self.id.to_string());
        fs::create_dir_all(&dir)?;
        let final_path = dir.join("state.json");
        let tmp_path = dir.join("state.json.tmp");
        let json = serde_json::to_vec_pretty(self)?;
        {
            let mut file = File::create(&tmp_path)?;
            file.write_all(&json)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        fs::rename(&tmp_path, &final_path)?;
        File::open(&dir)?.sync_all()?;
        Ok(())
    }

    /// Reload an instance previously persisted via [`Self::persist`].
    pub fn load(state_dir: &Path, id: Uuid) -> Result<Self> {
        let path: PathBuf = state_dir.join(id.to_string()).join("state.json");
        let raw = fs::read(&path)?;
        let instance: SandboxInstance = serde_json::from_slice(&raw)?;
        Ok(instance)
    }
}

fn is_valid_transition(from: SandboxState, to: SandboxState) -> bool {
    use SandboxState::{
        Checkpointed, Creating, Destroyed, Hibernated, Hibernating, Paused, Pending,
        RecoveryRequired, Restoring, Resuming, Running,
    };
    if to == Destroyed {
        // `* → destroyed` is always valid (terminal sink).
        return from != Destroyed;
    }
    if to == RecoveryRequired {
        return !matches!(from, Destroyed | RecoveryRequired);
    }
    match (from, to) {
        (Pending, Creating) => true,
        (Creating, Running) => true,
        (Running, Paused) => true,
        (Paused, Checkpointed) => true,
        (Paused, Running) => true, // resume
        (Checkpointed, Running) => true,
        (Running, Restoring) => true,
        (Restoring, Running) => true,
        (Running, Hibernating) => true,
        (Hibernating, Hibernated) => true,
        (Hibernating, Running) => true,
        (Hibernated, Resuming) => true,
        (Resuming, Running) => true,
        (Resuming, Hibernated) => true,
        // `Reset` and `Warm` are retained only so records from the removed
        // pool implementation still deserialize; they have no forward runtime
        // transition and may proceed only through cleanup to `Destroyed` or
        // `RecoveryRequired`, both handled above.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> SandboxInstance {
        SandboxInstance::new(
            BackendKind::KataFc,
            WorkloadClass::AgentRl,
            "sha256:deadbeef".into(),
            "agent-rl-default".into(),
        )
    }

    #[test]
    fn happy_path_cold() {
        let mut inst = fresh();
        for target in [
            SandboxState::Creating,
            SandboxState::Running,
            SandboxState::Paused,
            SandboxState::Checkpointed,
            SandboxState::Running,
            SandboxState::Destroyed,
        ] {
            inst.transition(target).expect("legal transition");
            assert_eq!(inst.state, target);
        }
    }

    #[test]
    fn restore_state_requires_owned_replacement_boundaries() {
        let mut inst = fresh();
        let error = inst
            .transition(SandboxState::Restoring)
            .expect_err("pending sandbox cannot restore");
        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));
        assert_eq!(inst.state, SandboxState::Pending);

        inst.transition(SandboxState::Creating).expect("creating");
        inst.transition(SandboxState::Running).expect("running");
        inst.begin_restore_operation("ckpt-00000000-0000-0000-0000-000000000001".to_string())
            .expect("begin restore");
        inst.advance_restore_phase(OperationPhase::RestoreStorageStaged)
            .expect("stage storage");
        let error = inst
            .transition(SandboxState::Restoring)
            .expect_err("running remains visible until the backend is stopped");
        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));
        assert_eq!(inst.state, SandboxState::Running);

        inst.advance_restore_phase(OperationPhase::RestoreBackendStopped)
            .expect("stop backend");
        inst.transition(SandboxState::Restoring)
            .expect("restore starts");
        inst.advance_restore_phase(OperationPhase::RestoreStorageActivated)
            .expect("activate storage");
        inst.advance_restore_phase(OperationPhase::RestoreBackendStarted)
            .expect("start backend");
        inst.advance_restore_phase(OperationPhase::RestoreHeadUpdated)
            .expect("update head");
        let error = inst
            .transition(SandboxState::Running)
            .expect_err("storage commit precedes the final running state");
        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));
        assert_eq!(inst.state, SandboxState::Restoring);

        inst.advance_restore_phase(OperationPhase::RestoreStorageCommitted)
            .expect("commit storage");
        inst.transition(SandboxState::Running)
            .expect("restore commits");
    }

    #[test]
    fn hibernation_state_requires_durable_ownership_boundaries() {
        let mut inst = fresh();
        inst.transition(SandboxState::Creating).expect("creating");
        inst.transition(SandboxState::Running).expect("running");
        inst.backend_ownership = BackendOwnership::Running;

        let error = inst
            .transition(SandboxState::Hibernating)
            .expect_err("hibernate requires a journal");
        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));

        inst.begin_hibernate_operation().expect("begin hibernate");
        inst.transition(SandboxState::Hibernating)
            .expect("hibernate starts");
        inst.advance_hibernate_phase(OperationPhase::HibernateArtifactsSynced)
            .expect("artifacts durable");
        let error = inst
            .transition(SandboxState::Hibernated)
            .expect_err("a live backend prevents hibernated state");
        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));

        inst.backend_ownership = BackendOwnership::Stopped;
        inst.advance_hibernate_phase(OperationPhase::HibernatePublished)
            .expect("publish hibernation");
        inst.transition(SandboxState::Hibernated)
            .expect("hibernate commits");
        inst.finish_operation();

        let error = inst
            .transition(SandboxState::Resuming)
            .expect_err("resume requires a journal");
        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));

        inst.begin_resume_operation().expect("begin resume");
        inst.transition(SandboxState::Resuming)
            .expect("resume starts");
        inst.advance_resume_phase(OperationPhase::ResumeBackendStarted)
            .expect("backend started");
        inst.backend_ownership = BackendOwnership::Running;
        let error = inst
            .transition(SandboxState::Running)
            .expect_err("readiness must precede running state");
        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));

        inst.advance_resume_phase(OperationPhase::ResumeBackendReady)
            .expect("backend ready");
        inst.transition(SandboxState::Running)
            .expect("resume commits");
    }

    #[test]
    fn destroy_is_always_legal_except_from_destroyed() {
        let mut inst = fresh();
        inst.transition(SandboxState::Destroyed).expect("ok");
        let again = inst.transition(SandboxState::Destroyed);
        assert!(matches!(
            again,
            Err(BlazeError::InvalidStateTransition { .. })
        ));
    }

    #[test]
    fn recovery_required_can_finish_but_cannot_be_reentered() {
        let mut inst = fresh();
        inst.transition(SandboxState::Creating).expect("creating");
        inst.transition(SandboxState::Running).expect("running");
        inst.transition(SandboxState::RecoveryRequired)
            .expect("recovery required");

        let repeated = inst.transition(SandboxState::RecoveryRequired);
        assert!(matches!(
            repeated,
            Err(BlazeError::InvalidStateTransition { .. })
        ));

        inst.transition(SandboxState::Destroyed)
            .expect("destroyed from recovery");
        let terminal = inst.transition(SandboxState::RecoveryRequired);
        assert!(matches!(
            terminal,
            Err(BlazeError::InvalidStateTransition { .. })
        ));
    }

    #[test]
    fn illegal_pending_to_running() {
        let mut inst = fresh();
        let err = inst.transition(SandboxState::Running).expect_err("illegal");
        assert!(matches!(err, BlazeError::InvalidStateTransition { .. }));
    }

    #[test]
    fn reset_and_warm_are_not_runtime_transition_targets() {
        let mut inst = fresh();
        inst.transition(SandboxState::Creating).expect("ok");
        inst.transition(SandboxState::Running).expect("ok");
        for target in [SandboxState::Reset, SandboxState::Warm] {
            let error = inst.transition(target).expect_err("legacy-only state");
            assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));
        }
    }

    #[test]
    fn legacy_pool_states_have_no_forward_runtime_transition() {
        use SandboxState::{Creating, Destroyed, RecoveryRequired, Reset, Warm};
        // `Reset` and `Warm` survive only for deserialization of records from
        // the removed pool implementation. They must not become eligible for
        // reuse: the only legal moves are cleanup to `Destroyed` or
        // `RecoveryRequired`.
        assert!(!is_valid_transition(Reset, Warm));
        assert!(!is_valid_transition(Warm, Creating));
        assert!(!is_valid_transition(Reset, Creating));
        for legacy in [Reset, Warm] {
            assert!(is_valid_transition(legacy, Destroyed));
            assert!(is_valid_transition(legacy, RecoveryRequired));
        }
    }

    #[test]
    fn persist_then_load_round_trip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut inst = fresh();
        inst.transition(SandboxState::Creating).expect("ok");
        inst.persist(tmp.path()).expect("persist");

        let loaded = SandboxInstance::load(tmp.path(), inst.id).expect("load");
        assert_eq!(loaded.id, inst.id);
        assert_eq!(loaded.state, SandboxState::Creating);
        assert_eq!(loaded.policy_name, inst.policy_name);
    }

    #[test]
    fn legacy_state_without_optional_fields_deserializes() {
        let inst = fresh();
        let value = serde_json::json!({
            "id": inst.id,
            "state": "running",
            "backend": "mock",
            "workload_class": "agent-rl",
            "image_digest": "sha256:old",
            "start_path": "cold",
            "created_at": inst.created_at,
            "updated_at": inst.updated_at,
            "policy_name": "legacy"
        });
        let loaded: SandboxInstance = serde_json::from_value(value).expect("legacy state");
        assert!(loaded.operation.is_none());
        assert!(loaded.last_checkpoint.is_none());
        assert!(loaded.template.is_none());
        assert_eq!(loaded.backend_ownership, BackendOwnership::Unknown);
    }

    #[test]
    fn template_identity_round_trips() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut instance = fresh();
        instance.template = Some("runtime-base".to_string());
        instance.persist(tmp.path()).expect("persist");

        let loaded = SandboxInstance::load(tmp.path(), instance.id).expect("load");

        assert_eq!(loaded.template.as_deref(), Some("runtime-base"));
    }

    #[test]
    fn legacy_reset_and_warm_states_deserialize() {
        let inst = fresh();
        for state in ["reset", "warm"] {
            let value = serde_json::json!({
                "id": inst.id,
                "state": state,
                "backend": "mock",
                "workload_class": "agent-rl",
                "image_digest": "sha256:old",
                "start_path": "warm",
                "created_at": inst.created_at,
                "updated_at": inst.updated_at,
                "policy_name": "legacy"
            });
            let loaded: SandboxInstance = serde_json::from_value(value).expect("legacy state");
            assert_eq!(loaded.state.as_str(), state);
            assert_eq!(loaded.start_path, StartPath::Warm);
        }
    }

    #[test]
    fn create_journal_round_trips() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut instance = fresh();
        instance.begin_operation(OperationKind::Create);
        instance.persist(tmp.path()).expect("persist");

        let mut loaded = SandboxInstance::load(tmp.path(), instance.id).expect("load");
        assert_eq!(
            loaded.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Create)
        );
        loaded.finish_operation();
        assert!(loaded.operation.is_none());
    }

    #[test]
    fn prune_journal_round_trips_without_a_checkpoint_selection() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut instance = fresh();
        instance.begin_operation(OperationKind::Prune);
        instance.persist(tmp.path()).expect("persist");

        let loaded = SandboxInstance::load(tmp.path(), instance.id).expect("load");
        let journal = loaded.operation.expect("prune journal");
        assert_eq!(journal.kind, OperationKind::Prune);
        assert!(journal.checkpoint_id.is_none());
        assert!(journal.phase.is_none());
    }

    #[test]
    fn checkpoint_journal_preserves_identity_and_phase() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut instance = fresh();
        instance
            .begin_checkpoint_operation("ckpt-00000000-0000-0000-0000-000000000001".into())
            .expect("begin checkpoint");
        instance
            .advance_checkpoint_phase(OperationPhase::CheckpointPublished)
            .expect("advance checkpoint");
        instance.persist(tmp.path()).expect("persist");

        let loaded = SandboxInstance::load(tmp.path(), instance.id).expect("load");
        let journal = loaded.operation.expect("checkpoint journal");
        assert_eq!(journal.kind, OperationKind::Checkpoint);
        assert_eq!(
            journal.checkpoint_id.as_deref(),
            Some("ckpt-00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(journal.phase, Some(OperationPhase::CheckpointPublished));
    }

    #[test]
    fn checkpoint_journal_rejects_phase_regression() {
        let mut instance = fresh();
        instance
            .begin_checkpoint_operation("ckpt-00000000-0000-0000-0000-000000000001".into())
            .expect("begin checkpoint");
        instance
            .advance_checkpoint_phase(OperationPhase::CheckpointPublished)
            .expect("advance checkpoint");

        let error = instance
            .advance_checkpoint_phase(OperationPhase::CheckpointPaused)
            .expect_err("checkpoint phase must remain a durable lower bound");

        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));
        assert_eq!(
            instance
                .operation
                .as_ref()
                .and_then(|journal| journal.phase),
            Some(OperationPhase::CheckpointPublished)
        );
    }

    #[test]
    fn checkpoint_journal_cannot_replace_an_active_operation() {
        let mut instance = fresh();
        instance.begin_operation(OperationKind::Create);
        let journal = instance.operation.clone();

        let error = instance
            .begin_checkpoint_operation("ckpt-00000000-0000-0000-0000-000000000001".into())
            .expect_err("checkpoint must not replace create");

        assert!(matches!(
            error,
            BlazeError::OperationInProgress { active, requested }
                if active == "create" && requested == "checkpoint"
        ));
        assert_eq!(instance.operation, journal);
    }

    #[test]
    fn restore_journal_round_trips_without_overwriting_last_checkpoint() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut instance = fresh();
        let completed = "ckpt-00000000-0000-0000-0000-000000000001".to_string();
        let selected = "ckpt-00000000-0000-0000-0000-000000000002".to_string();
        instance.last_checkpoint = Some(completed.clone());
        instance
            .transition(SandboxState::Creating)
            .expect("creating");
        instance.transition(SandboxState::Running).expect("running");
        instance
            .begin_restore_operation(selected.clone())
            .expect("begin restore");
        instance
            .advance_restore_phase(OperationPhase::RestoreStorageStaged)
            .expect("stage storage");
        instance
            .advance_restore_phase(OperationPhase::RestoreBackendStopped)
            .expect("stop backend");
        instance
            .transition(SandboxState::Restoring)
            .expect("restoring");

        for phase in [
            OperationPhase::RestoreStorageActivated,
            OperationPhase::RestoreBackendStarted,
            OperationPhase::RestoreHeadUpdated,
            OperationPhase::RestoreStorageCommitted,
        ] {
            instance
                .advance_restore_phase(phase)
                .expect("advance restore");
            assert_eq!(
                instance.last_checkpoint.as_deref(),
                Some(completed.as_str())
            );
        }
        instance.persist(tmp.path()).expect("persist");

        let loaded = SandboxInstance::load(tmp.path(), instance.id).expect("load");
        let journal = loaded.operation.expect("restore journal");
        assert_eq!(journal.kind, OperationKind::Restore);
        assert_eq!(journal.checkpoint_id.as_deref(), Some(selected.as_str()));
        assert_eq!(journal.phase, Some(OperationPhase::RestoreStorageCommitted));
        assert_eq!(loaded.last_checkpoint.as_deref(), Some(completed.as_str()));
        assert_eq!(
            serde_json::to_value(journal.kind).expect("serialize kind"),
            serde_json::json!("restore")
        );
        assert_eq!(
            serde_json::to_value(journal.phase).expect("serialize phase"),
            serde_json::json!("restore-storage-committed")
        );
    }

    #[test]
    fn restore_journal_rejects_phase_regression() {
        let mut instance = fresh();
        let completed = "ckpt-00000000-0000-0000-0000-000000000001".to_string();
        instance.last_checkpoint = Some(completed.clone());
        instance
            .begin_restore_operation("ckpt-00000000-0000-0000-0000-000000000002".to_string())
            .expect("begin restore");
        instance
            .advance_restore_phase(OperationPhase::RestoreStorageActivated)
            .expect("advance restore");

        let error = instance
            .advance_restore_phase(OperationPhase::RestoreStorageStaged)
            .expect_err("restore phase must remain a durable lower bound");

        assert!(matches!(error, BlazeError::InvalidStateTransition { .. }));
        assert_eq!(
            instance
                .operation
                .as_ref()
                .and_then(|journal| journal.phase),
            Some(OperationPhase::RestoreStorageActivated)
        );
        assert_eq!(instance.last_checkpoint, Some(completed));
    }

    #[test]
    fn operation_journals_reject_phases_from_the_other_operation() {
        let mut checkpoint = fresh();
        checkpoint
            .begin_checkpoint_operation("ckpt-00000000-0000-0000-0000-000000000001".to_string())
            .expect("begin checkpoint");
        let checkpoint_journal = checkpoint.operation.clone();
        let checkpoint_error = checkpoint
            .advance_checkpoint_phase(OperationPhase::RestoreBackendStopped)
            .expect_err("checkpoint cannot record restore progress");
        assert!(matches!(
            checkpoint_error,
            BlazeError::InvalidStateTransition { .. }
        ));
        assert_eq!(checkpoint.operation, checkpoint_journal);

        let mut restore = fresh();
        restore
            .begin_restore_operation("ckpt-00000000-0000-0000-0000-000000000002".to_string())
            .expect("begin restore");
        let restore_journal = restore.operation.clone();
        let restore_error = restore
            .advance_restore_phase(OperationPhase::CheckpointPublished)
            .expect_err("restore cannot record checkpoint progress");
        assert!(matches!(
            restore_error,
            BlazeError::InvalidStateTransition { .. }
        ));
        assert_eq!(restore.operation, restore_journal);
    }

    #[test]
    fn restore_journal_cannot_replace_an_active_operation() {
        let mut instance = fresh();
        instance.begin_operation(OperationKind::Create);
        let journal = instance.operation.clone();

        let error = instance
            .begin_restore_operation("ckpt-00000000-0000-0000-0000-000000000001".to_string())
            .expect_err("restore must not replace create");

        assert!(matches!(
            error,
            BlazeError::OperationInProgress { active, requested }
                if active == "create" && requested == "restore"
        ));
        assert_eq!(instance.operation, journal);
    }
}
