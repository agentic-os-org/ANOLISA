// SPDX-License-Identifier: Apache-2.0
//! Durable checkpoint capture and listing.

use std::sync::Arc;

use blaze_core::backend::{BackendKind, SnapshotKind, SnapshotRequest};
use blaze_core::checkpoint::{CheckpointInfo, CheckpointMetadata, CommitCheckpoint};
use blaze_core::lifecycle::{OperationKind, OperationPhase, SandboxInstance, SandboxState};
use tokio::sync::OwnedMutexGuard;
use uuid::Uuid;

use crate::checkpoint_store::{
    CheckpointHeadOutcome, CheckpointPublishOutcome, CheckpointStage, PruneOutcome,
    PublishedCheckpoint,
};
use crate::error::{BlazeDaemonError, Result};
use crate::spawner::DynBackendInstance;

use super::manager::SandboxManager;

enum PublishBoundaryResult {
    Published {
        instance: SandboxInstance,
        published: Box<PublishedCheckpoint>,
    },
    KnownUnpublished {
        stage: CheckpointStage,
        error: BlazeDaemonError,
    },
    RecoveryRequired {
        error: BlazeDaemonError,
    },
}

enum HeadBoundaryResult {
    Updated {
        instance: SandboxInstance,
        metadata: Box<CheckpointMetadata>,
    },
    KnownUnchanged {
        error: BlazeDaemonError,
    },
    RecoveryRequired {
        error: BlazeDaemonError,
    },
}

impl SandboxManager {
    /// Capture a self-contained checkpoint and resume the existing backend.
    pub async fn checkpoint(self: &Arc<Self>, id: Uuid) -> Result<CheckpointMetadata> {
        let operation = self.operation_lock(id).lock_owned().await;
        let manager = Arc::clone(self);
        crate::failpoint::spawn(async move { manager.checkpoint_supervised(id, operation).await })
            .await
            .map_err(|error| {
                let recovery = self.mark_recovery(id).err();
                BlazeDaemonError::RecoveryRequired(format!(
                    "checkpoint supervisor stopped unexpectedly: {error}{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                ))
            })?
    }

    async fn checkpoint_supervised(
        self: Arc<Self>,
        id: Uuid,
        operation: OwnedMutexGuard<()>,
    ) -> Result<CheckpointMetadata> {
        let manager = Arc::clone(&self);
        let result =
            match crate::failpoint::spawn(async move { manager.checkpoint_worker(id).await }).await
            {
                Ok(result) => result,
                Err(error) => {
                    let recovery = self.mark_recovery(id).err();
                    Err(BlazeDaemonError::RecoveryRequired(format!(
                        "checkpoint worker stopped unexpectedly: {error}{}",
                        recovery
                            .map(|error| format!("; recovery state persistence failed: {error}"))
                            .unwrap_or_default()
                    )))
                }
            };
        drop(operation);
        result
    }

    async fn checkpoint_worker(self: Arc<Self>, id: Uuid) -> Result<CheckpointMetadata> {
        let mut instance = self.get(id)?;
        if let Some(journal) = &instance.operation {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "instance {id} has unfinished {} operation",
                journal.kind
            )));
        }
        if instance.state != SandboxState::Running {
            return Err(BlazeDaemonError::Conflict(format!(
                "instance {id} is {}, expected running",
                instance.state
            )));
        }

        let backend = self.backend_owner(id).ok_or_else(|| {
            BlazeDaemonError::Conflict(format!("instance {id} has no backend owner"))
        })?;
        if !backend.supports_checkpoint_capture() || !self.storage.supports_checkpoint_capture() {
            return Err(BlazeDaemonError::UnsupportedOperation(format!(
                "instance {id} backend {} and configured storage do not support checkpoint capture",
                backend.backend()
            )));
        }
        if backend.instance_id() != id || backend.backend() != instance.backend {
            self.mark_recovery(id)?;
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "instance {id} backend owner identity does not match durable state"
            )));
        }
        let backend_version = backend.version().map(str::to_string);
        if backend_version
            .as_deref()
            .is_some_and(|version| version.trim().is_empty())
            || (backend.backend() == BackendKind::Firecracker && backend_version.is_none())
        {
            return Err(BlazeDaemonError::UnsupportedOperation(format!(
                "instance {id} backend {} does not report a usable checkpoint version",
                backend.backend()
            )));
        }
        self.require_live_backend(id, &backend).await?;
        let storage = self.storage.reconstruct(&id.to_string()).await?;

        // Validate the current parent before pausing the backend. The operation
        // guard keeps HEAD stable until publication, while the blocking pool
        // keeps full artifact verification off Tokio's async worker threads.
        let read_manager = Arc::clone(&self);
        let read_head = crate::failpoint::spawn_blocking(move || {
            crate::failpoint::pause_blocking("checkpoint-before-read-head");
            read_manager
                .checkpoints
                .read_head(id)
                .map_err(checkpoint_store_error)
        })
        .await
        .map_err(|error| {
            BlazeDaemonError::Internal(format!(
                "checkpoint parent validation blocking task: {error}"
            ))
        })?;
        let parent = read_head?;

        let begin_store = self.checkpoints.clone();
        let begin = crate::failpoint::spawn_blocking(move || {
            crate::failpoint::pause_blocking("checkpoint-before-stage-begin");
            begin_store.begin(id)
        })
        .await
        .map_err(|error| {
            let recovery = self.mark_recovery(id).err();
            BlazeDaemonError::RecoveryRequired(format!(
                "checkpoint stage creation blocking task stopped unexpectedly: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            ))
        })?;
        let stage = match begin {
            Ok(stage) => stage,
            Err(error) => {
                let Some(checkpoint_id) = error.recovery_checkpoint_id().map(str::to_owned) else {
                    return Err(checkpoint_store_error(error));
                };
                if let Err(journal) = instance.begin_checkpoint_operation(checkpoint_id) {
                    let recovery = self.mark_recovery(id).err();
                    return Err(BlazeDaemonError::RecoveryRequired(format!(
                        "checkpoint stage creation failed: {error}; recovery journal failed: \
                         {journal}{}",
                        recovery
                            .map(|error| format!("; recovery state persistence failed: {error}"))
                            .unwrap_or_default()
                    )));
                }
                let recovery = self.mark_instance_recovery(instance).err();
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "checkpoint stage creation failed and cleanup could not be confirmed: \
                     {error}{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )));
            }
        };
        let checkpoint_id = stage.id().to_string();
        // Each producer owns one payload subtree: the backend adapter writes
        // its private layout under backend/, the storage provider captures
        // the rootfs under storage/. Publication inventories both.
        let backend_payload_dir = stage.backend_payload_dir();
        let rootfs_path = stage.storage_payload_dir().join("rootfs.snap");
        if let Err(error) = crate::failpoint::state("checkpoint-begin-state") {
            let _ = self.abort_checkpoint_stage(stage).await;
            return Err(error);
        }
        if let Err(error) = instance.begin_checkpoint_operation(checkpoint_id.clone()) {
            let _ = self.abort_checkpoint_stage(stage).await;
            return Err(error.into());
        }
        if let Err(error) = crate::failpoint::state("checkpoint-begin-state-commit")
            .and_then(|_| self.persist_and_retain(instance.clone()))
        {
            if let Err(cleanup) = self.abort_checkpoint_stage(stage).await {
                let recovery = self.mark_instance_recovery(instance).err();
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "checkpoint intent state commit failed: {error}; checkpoint staging cleanup \
                     failed: {cleanup}{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )));
            }
            return Err(error);
        }
        crate::failpoint::pause("checkpoint-after-begin").await;

        let paused = match crate::failpoint::backend("checkpoint-pause") {
            Ok(()) => backend.quiesce_for_capture().await,
            Err(error) => Err(error),
        };
        if let Err(error) = paused {
            return self
                .finish_failed_unpublished_checkpoint(id, &backend, stage, error.into())
                .await;
        }

        if let Err(error) = instance
            .transition(SandboxState::Paused)
            .and_then(|_| instance.advance_checkpoint_phase(OperationPhase::CheckpointPaused))
        {
            return self
                .finish_failed_unpublished_checkpoint(id, &backend, stage, error.into())
                .await;
        }
        if let Err(error) = crate::failpoint::state("checkpoint-paused-state")
            .and_then(|_| self.persist_and_retain(instance.clone()))
        {
            return self
                .finish_failed_unpublished_checkpoint(id, &backend, stage, error)
                .await;
        }
        crate::failpoint::pause("checkpoint-after-pause").await;

        let snapshot = SnapshotRequest {
            payload_dir: backend_payload_dir,
            kind: SnapshotKind::Full,
        };
        let snapshot_result = match crate::failpoint::backend("checkpoint-snapshot") {
            Ok(()) => backend.snapshot(snapshot).await,
            Err(error) => Err(error),
        };
        if let Err(error) = snapshot_result {
            return self
                .finish_failed_unpublished_checkpoint(id, &backend, stage, error.into())
                .await;
        }

        let flushed = match crate::failpoint::storage("checkpoint-storage-flush") {
            Ok(()) => self.storage.sync_artifacts(&storage).await,
            Err(error) => Err(error),
        };
        if let Err(error) = flushed {
            return self
                .finish_failed_unpublished_checkpoint(id, &backend, stage, error.into())
                .await;
        }

        let captured = match crate::failpoint::storage("checkpoint-rootfs-capture") {
            Ok(()) => {
                self.storage
                    .capture_checkpoint(&storage, &rootfs_path)
                    .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = captured {
            return self
                .finish_failed_unpublished_checkpoint(id, &backend, stage, error.into())
                .await;
        }

        let publish_manager = Arc::clone(&self);
        let publish = crate::failpoint::spawn_blocking(move || {
            if let Err(error) = crate::failpoint::storage("checkpoint-publish") {
                return PublishBoundaryResult::KnownUnpublished {
                    stage,
                    error: error.into(),
                };
            }
            let published = publish_manager.checkpoints.publish_retained(
                &stage,
                CommitCheckpoint {
                    parent,
                    policy_name: instance.policy_name.clone(),
                    image_digest: instance.image_digest.clone(),
                    backend: instance.backend,
                    backend_version,
                    snapshot_kind: SnapshotKind::Full,
                },
            );
            let published = match published {
                Ok(published) => published,
                Err(error) if error.outcome() == CheckpointPublishOutcome::KnownUnpublished => {
                    return PublishBoundaryResult::KnownUnpublished {
                        stage,
                        error: checkpoint_store_error(error.into_store_error()),
                    };
                }
                Err(error) => {
                    let error = blocking_recovery_error(
                        &publish_manager,
                        &instance,
                        checkpoint_store_error(error.into_store_error()),
                        "publication with uncertain outcome",
                    );
                    return PublishBoundaryResult::RecoveryRequired { error };
                }
            };

            crate::failpoint::pause_blocking("checkpoint-after-store-publish-before-state");
            if let Err(error) =
                instance.advance_checkpoint_phase(OperationPhase::CheckpointPublished)
            {
                let error = blocking_recovery_error(
                    &publish_manager,
                    &instance,
                    error.into(),
                    "published journal update",
                );
                return PublishBoundaryResult::RecoveryRequired { error };
            }
            if let Err(error) = crate::failpoint::state("checkpoint-published-state")
                .and_then(|_| publish_manager.persist_and_retain(instance.clone()))
            {
                let error = blocking_recovery_error(
                    &publish_manager,
                    &instance,
                    error,
                    "published state commit",
                );
                return PublishBoundaryResult::RecoveryRequired { error };
            }
            PublishBoundaryResult::Published {
                instance,
                published: Box::new(published),
            }
        })
        .await;
        let (mut instance, published) = match publish {
            Ok(PublishBoundaryResult::Published {
                instance,
                published,
            }) => (instance, published),
            Ok(PublishBoundaryResult::KnownUnpublished { stage, error }) => {
                return self
                    .finish_failed_unpublished_checkpoint(id, &backend, stage, error)
                    .await;
            }
            Ok(PublishBoundaryResult::RecoveryRequired { error }) => {
                return self
                    .resume_after_blocking_boundary_failure(&backend, error)
                    .await;
            }
            Err(error) => {
                let error =
                    blocking_join_recovery_error(&self, id, "checkpoint publication", error);
                return self
                    .resume_after_blocking_boundary_failure(&backend, error)
                    .await;
            }
        };
        crate::failpoint::pause("checkpoint-after-publish-before-head").await;

        let head_manager = Arc::clone(&self);
        let head = crate::failpoint::spawn_blocking(move || {
            if let Err(error) = crate::failpoint::storage("checkpoint-head-update") {
                return HeadBoundaryResult::KnownUnchanged {
                    error: error.into(),
                };
            }
            let metadata = match head_manager.checkpoints.set_head_published(*published) {
                Ok(metadata) => metadata,
                Err(error) => {
                    return match error.outcome() {
                        CheckpointHeadOutcome::KnownUnchanged => {
                            HeadBoundaryResult::KnownUnchanged {
                                error: checkpoint_store_error(error.into_store_error()),
                            }
                        }
                        CheckpointHeadOutcome::Unknown => {
                            let error = blocking_recovery_error(
                                &head_manager,
                                &instance,
                                checkpoint_store_error(error.into_store_error()),
                                "HEAD update with uncertain outcome",
                            );
                            HeadBoundaryResult::RecoveryRequired { error }
                        }
                    };
                }
            };

            crate::failpoint::pause_blocking("checkpoint-after-store-head-before-state");
            if let Err(error) =
                instance.advance_checkpoint_phase(OperationPhase::CheckpointHeadUpdated)
            {
                let error = blocking_recovery_error(
                    &head_manager,
                    &instance,
                    error.into(),
                    "HEAD journal update",
                );
                return HeadBoundaryResult::RecoveryRequired { error };
            }
            if let Err(error) = crate::failpoint::state("checkpoint-head-state")
                .and_then(|_| head_manager.persist_and_retain(instance.clone()))
            {
                let error =
                    blocking_recovery_error(&head_manager, &instance, error, "HEAD state commit");
                return HeadBoundaryResult::RecoveryRequired { error };
            }
            HeadBoundaryResult::Updated {
                instance,
                metadata: Box::new(metadata),
            }
        })
        .await;
        let (mut instance, metadata) = match head {
            Ok(HeadBoundaryResult::Updated { instance, metadata }) => (instance, *metadata),
            Ok(HeadBoundaryResult::KnownUnchanged { error }) => {
                return self
                    .finish_failed_published_checkpoint(id, &backend, error)
                    .await;
            }
            Ok(HeadBoundaryResult::RecoveryRequired { error }) => {
                return self
                    .resume_after_blocking_boundary_failure(&backend, error)
                    .await;
            }
            Err(error) => {
                let error =
                    blocking_join_recovery_error(&self, id, "checkpoint HEAD update", error);
                return self
                    .resume_after_blocking_boundary_failure(&backend, error)
                    .await;
            }
        };
        crate::failpoint::pause("checkpoint-after-head").await;

        let resumed = match crate::failpoint::backend("checkpoint-resume") {
            Ok(()) => backend.unquiesce_after_capture().await,
            Err(error) => Err(error),
        };
        if let Err(error) = resumed {
            self.mark_recovery(id)?;
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "checkpoint {checkpoint_id} became HEAD, but backend resume failed: {error}"
            )));
        }
        if let Err(error) = self.verify_backend_ready(id, &backend).await {
            self.mark_recovery(id)?;
            return Err(error);
        }

        if let Err(error) = instance
            .transition(SandboxState::Checkpointed)
            .and_then(|_| instance.transition(SandboxState::Running))
        {
            self.mark_recovery(id)?;
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "checkpoint runtime resumed, but lifecycle transition failed: {error}"
            )));
        }
        instance.last_checkpoint = Some(checkpoint_id);
        instance.finish_operation();
        if let Err(error) = crate::failpoint::state("checkpoint-final-state")
            .and_then(|_| self.persist_and_retain(instance))
        {
            self.mark_recovery(id)?;
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "checkpoint completed, but final lifecycle state could not be committed: {error}"
            )));
        }
        Ok(metadata)
    }

    /// List every committed checkpoint and its HEAD reachability.
    pub async fn list_checkpoints(self: &Arc<Self>, id: Uuid) -> Result<Vec<CheckpointInfo>> {
        let operation = self.operation_lock(id).lock_owned().await;
        self.get(id)?;
        let manager = Arc::clone(self);
        crate::failpoint::spawn_blocking(move || {
            let _operation = operation;
            crate::failpoint::pause_blocking("checkpoint-before-store-list");
            manager.checkpoints.list(id).map_err(checkpoint_store_error)
        })
        .await
        .map_err(|error| {
            BlazeDaemonError::Internal(format!("checkpoint list blocking task: {error}"))
        })?
    }

    /// Remove checkpoint branches that are unreachable from HEAD.
    ///
    /// A detached supervisor retains the per-sandbox operation lock until
    /// deletion and lifecycle finalization finish, even if the caller leaves.
    pub async fn prune_checkpoints(self: &Arc<Self>, id: Uuid) -> Result<Vec<String>> {
        let operation = self.operation_lock(id).lock_owned().await;
        let manager = Arc::clone(self);
        crate::failpoint::spawn(
            async move { manager.prune_checkpoints_supervised(id, operation).await },
        )
        .await
        .map_err(|error| {
            let recovery = self.mark_recovery(id).err();
            BlazeDaemonError::RecoveryRequired(format!(
                "checkpoint prune supervisor stopped unexpectedly: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            ))
        })?
    }

    async fn prune_checkpoints_supervised(
        self: Arc<Self>,
        id: Uuid,
        operation: OwnedMutexGuard<()>,
    ) -> Result<Vec<String>> {
        let manager = Arc::clone(&self);
        let result = match crate::failpoint::spawn(async move {
            manager.prune_checkpoints_worker(id).await
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let recovery = self.mark_recovery(id).err();
                Err(BlazeDaemonError::RecoveryRequired(format!(
                    "checkpoint prune worker stopped unexpectedly: {error}{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )))
            }
        };
        drop(operation);
        result
    }

    async fn prune_checkpoints_worker(self: Arc<Self>, id: Uuid) -> Result<Vec<String>> {
        let mut instance = self.get(id)?;
        if instance.state != SandboxState::Running {
            return Err(BlazeDaemonError::Conflict(format!(
                "instance {id} must be running before checkpoint history can be pruned"
            )));
        }
        if let Some(journal) = &instance.operation {
            return Err(BlazeDaemonError::Conflict(format!(
                "instance {id} has unfinished {} operation",
                journal.kind
            )));
        }

        let checkpoint_history_expected = instance.last_checkpoint.is_some();
        instance.begin_operation(OperationKind::Prune);
        self.persist_and_retain(instance)?;

        let checkpoints = self.checkpoints.clone();
        let store_result = crate::failpoint::spawn_blocking(move || {
            crate::failpoint::pause_blocking("checkpoint-before-store-prune");
            checkpoints.prune_unreachable(id, checkpoint_history_expected)
        })
        .await;
        let outcome = match store_result {
            Ok(outcome) => outcome,
            Err(error) => {
                let recovery = self.mark_recovery(id).err();
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "checkpoint prune worker stopped unexpectedly: {error}{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )));
            }
        };

        match outcome {
            Ok(PruneOutcome::Complete { removed }) => {
                self.finish_prune_operation(id, None)?;
                Ok(removed)
            }
            Err(error) => {
                let original = checkpoint_store_error(error);
                self.finish_prune_operation(id, Some(&original))?;
                Err(original)
            }
            Ok(PruneOutcome::Incomplete {
                removed,
                uncertain,
                source,
            }) => {
                let recovery = self.mark_recovery(id).err();
                Err(BlazeDaemonError::RecoveryRequired(format!(
                    "checkpoint prune did not finish safely after removing {removed:?}{}: {source}{}",
                    uncertain
                        .map(|checkpoint_id| format!(
                            "; outcome for checkpoint {checkpoint_id} is uncertain"
                        ))
                        .unwrap_or_default(),
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )))
            }
        }
    }

    fn finish_prune_operation(&self, id: Uuid, original: Option<&BlazeDaemonError>) -> Result<()> {
        let mut instance = self.get(id)?;
        if !instance
            .operation
            .as_ref()
            .is_some_and(|operation| operation.kind == OperationKind::Prune)
        {
            self.mark_recovery(id)?;
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "instance {id} lost its checkpoint prune operation record"
            )));
        }
        instance.finish_operation();
        if let Err(error) = self.persist_and_retain(instance) {
            let recovery = self.mark_recovery(id).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "checkpoint prune could not commit its final lifecycle record{}: {error}{}",
                original
                    .map(|original| format!(" after {original}"))
                    .unwrap_or_default(),
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn finish_failed_unpublished_checkpoint<T>(
        &self,
        id: Uuid,
        backend: &DynBackendInstance,
        stage: CheckpointStage,
        original: BlazeDaemonError,
    ) -> Result<T> {
        let compensation = self
            .resume_and_clear_checkpoint(id, backend, Some(stage))
            .await;
        match compensation {
            Ok(()) => Err(original),
            Err(compensation) => Err(BlazeDaemonError::RecoveryRequired(format!(
                "{original}; checkpoint compensation failed: {compensation}"
            ))),
        }
    }

    async fn finish_failed_published_checkpoint<T>(
        &self,
        id: Uuid,
        backend: &DynBackendInstance,
        original: BlazeDaemonError,
    ) -> Result<T> {
        let compensation = self.resume_and_clear_checkpoint(id, backend, None).await;
        match compensation {
            Ok(()) => Err(original),
            Err(compensation) => Err(BlazeDaemonError::RecoveryRequired(format!(
                "{original}; checkpoint compensation failed: {compensation}"
            ))),
        }
    }

    async fn resume_after_blocking_boundary_failure<T>(
        &self,
        backend: &DynBackendInstance,
        original: BlazeDaemonError,
    ) -> Result<T> {
        let resume = self.resume_backend(backend).await;
        Err(BlazeDaemonError::RecoveryRequired(format!(
            "{original}{}",
            resume
                .err()
                .map(|error| format!("; backend resume failed: {error}"))
                .unwrap_or_default()
        )))
    }

    async fn resume_and_clear_checkpoint(
        &self,
        id: Uuid,
        backend: &DynBackendInstance,
        unpublished_stage: Option<CheckpointStage>,
    ) -> Result<()> {
        if let Err(error) = self.resume_backend(backend).await {
            let recovery = self.mark_recovery(id).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "backend resume failed: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        if let Some(stage) = unpublished_stage
            && let Err(error) = self.abort_checkpoint_stage(stage).await
        {
            let recovery = self.mark_recovery(id).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "checkpoint staging cleanup failed: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }

        let mut instance = self.get(id)?;
        if instance.state == SandboxState::Paused {
            instance.transition(SandboxState::Running)?;
        }
        instance.finish_operation();
        if let Err(error) = self.persist_and_retain(instance) {
            let recovery = self.mark_recovery(id).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "checkpoint compensation state commit failed: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn abort_checkpoint_stage(&self, stage: CheckpointStage) -> Result<()> {
        let checkpoints = self.checkpoints.clone();
        crate::failpoint::spawn_blocking(move || {
            crate::failpoint::pause_blocking("checkpoint-before-stage-abort");
            checkpoints.abort(stage).map_err(checkpoint_store_error)
        })
        .await
        .map_err(|error| {
            BlazeDaemonError::Internal(format!(
                "checkpoint staging cleanup blocking task stopped unexpectedly: {error}"
            ))
        })?
    }

    async fn resume_backend(&self, backend: &DynBackendInstance) -> Result<()> {
        match crate::failpoint::backend("checkpoint-compensation-resume") {
            Ok(()) => backend.unquiesce_after_capture().await?,
            Err(error) => return Err(error.into()),
        }
        self.verify_backend_ready(backend.instance_id(), backend)
            .await
    }

    async fn require_live_backend(&self, id: Uuid, backend: &DynBackendInstance) -> Result<()> {
        match backend.try_wait().await {
            Ok(None) => Ok(()),
            Ok(Some(result)) => {
                self.mark_recovery(id)?;
                Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} backend exited before checkpoint capture \
                     (exit={:?}, signal={:?})",
                    result.exit_code, result.signal
                )))
            }
            Err(error) => {
                self.mark_recovery(id)?;
                Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} backend liveness is unknown: {error}"
                )))
            }
        }
    }

    async fn verify_backend_ready(&self, id: Uuid, backend: &DynBackendInstance) -> Result<()> {
        self.require_live_backend(id, backend).await?;
        self.wait_for_guest_ready(backend, "checkpoint-guest-ready")
            .await?;
        self.require_live_backend(id, backend).await
    }
}

fn blocking_recovery_error(
    manager: &SandboxManager,
    instance: &SandboxInstance,
    original: BlazeDaemonError,
    boundary: &str,
) -> BlazeDaemonError {
    let recovery = manager.mark_instance_recovery(instance.clone());
    BlazeDaemonError::RecoveryRequired(format!(
        "checkpoint {boundary} failed: {original}{}",
        recovery
            .err()
            .map(|error| format!("; recovery state persistence failed: {error}"))
            .unwrap_or_default()
    ))
}

fn blocking_join_recovery_error(
    manager: &SandboxManager,
    id: Uuid,
    boundary: &str,
    join: tokio::task::JoinError,
) -> BlazeDaemonError {
    let recovery = manager.mark_recovery(id);
    BlazeDaemonError::RecoveryRequired(format!(
        "{boundary} blocking task failed: {join}{}",
        recovery
            .err()
            .map(|error| format!("; recovery state persistence failed: {error}"))
            .unwrap_or_default()
    ))
}

fn checkpoint_store_error(error: impl std::fmt::Display) -> BlazeDaemonError {
    BlazeDaemonError::Internal(format!("checkpoint store: {error}"))
}
