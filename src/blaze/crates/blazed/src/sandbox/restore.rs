// SPDX-License-Identifier: Apache-2.0
//! Recoverable replacement of a running sandbox from a committed checkpoint.

use std::path::PathBuf;
use std::sync::Arc;

use blaze_core::backend::{RestoreRequest, SnapshotKind};
use blaze_core::checkpoint::validate_checkpoint_id;
use blaze_core::lifecycle::{BackendOwnership, OperationPhase, SandboxInstance, SandboxState};
use blaze_core::storage::StorageRestoreTransaction;
use tokio::sync::OwnedMutexGuard;
use uuid::Uuid;

use crate::checkpoint_store::CheckpointStoreError;
use crate::error::{BlazeDaemonError, Result};
use crate::spawner::{BackendRestoreRequest, DynBackendInstance, restore_with_runtime_directory};

use super::manager::SandboxManager;

/// Inputs resolved from the current daemon configuration.
#[derive(Debug, Clone)]
pub struct RestoreSandbox {
    /// Committed checkpoint selected by the caller.
    pub checkpoint_id: String,
    /// Current executable for the checkpoint's backend.
    pub binary_path: PathBuf,
}

/// Result of one completed checkpoint restore.
#[derive(Debug, Clone)]
pub struct RestoreSandboxResult {
    /// Updated durable sandbox record.
    pub instance: SandboxInstance,
    /// Checkpoint now selected by the catalog HEAD.
    pub checkpoint_id: String,
}

impl SandboxManager {
    /// Replace a running backend and rootfs from one verified checkpoint.
    pub async fn restore(
        self: &Arc<Self>,
        id: Uuid,
        request: RestoreSandbox,
    ) -> Result<RestoreSandboxResult> {
        validate_checkpoint_id(&request.checkpoint_id)
            .map_err(|error| BlazeDaemonError::BadRequest(error.to_string()))?;
        let operation = self.operation_lock(id).lock_owned().await;
        let manager = Arc::clone(self);
        crate::failpoint::spawn(
            async move { manager.restore_supervised(id, request, operation).await },
        )
        .await
        .map_err(|error| {
            let recovery = self.mark_recovery(id).err();
            BlazeDaemonError::RecoveryRequired(format!(
                "restore supervisor stopped unexpectedly: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            ))
        })?
    }

    async fn restore_supervised(
        self: Arc<Self>,
        id: Uuid,
        request: RestoreSandbox,
        operation: OwnedMutexGuard<()>,
    ) -> Result<RestoreSandboxResult> {
        let manager = Arc::clone(&self);
        let result =
            match crate::failpoint::spawn(async move { manager.restore_worker(id, request).await })
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    let recovery = self.mark_recovery(id).err();
                    Err(BlazeDaemonError::RecoveryRequired(format!(
                        "restore worker stopped unexpectedly: {error}{}",
                        recovery
                            .map(|error| format!("; recovery state persistence failed: {error}"))
                            .unwrap_or_default()
                    )))
                }
            };
        drop(operation);
        result
    }

    async fn restore_worker(
        self: Arc<Self>,
        id: Uuid,
        request: RestoreSandbox,
    ) -> Result<RestoreSandboxResult> {
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

        let checkpoint_id = request.checkpoint_id.clone();
        let verify_manager = Arc::clone(&self);
        let selected_id = checkpoint_id.clone();
        let target = crate::failpoint::spawn_blocking(move || {
            verify_manager
                .checkpoints
                .verify_restore_target(id, &checkpoint_id)
                .map_err(|e| checkpoint_lookup_error(e, id, &selected_id))
        })
        .await
        .map_err(|error| {
            BlazeDaemonError::Internal(format!(
                "checkpoint restore verification blocking task: {error}"
            ))
        })??;
        let target_metadata = target.metadata().clone();
        // The backend adapter re-reads its own capture layout from its
        // payload subtree; the storage provider re-materializes the rootfs
        // from its own. Both paths pin the retained directory descriptors.
        let backend_payload_dir = target.backend_payload_dir();
        let rootfs_path = target.storage_payload_dir().join("rootfs.snap");
        if target_metadata.policy_name != instance.policy_name
            || target_metadata.image_digest != instance.image_digest
            || target_metadata.backend != instance.backend
        {
            return Err(BlazeDaemonError::Conflict(format!(
                "checkpoint {} runtime identity does not match instance {id}",
                request.checkpoint_id
            )));
        }
        if target_metadata.snapshot_kind != SnapshotKind::Full {
            return Err(BlazeDaemonError::UnsupportedOperation(format!(
                "checkpoint {} does not contain a full snapshot",
                request.checkpoint_id
            )));
        }

        let current_backend = self.backend_owner(id).ok_or_else(|| {
            BlazeDaemonError::Conflict(format!("instance {id} has no backend owner"))
        })?;
        if current_backend.instance_id() != id || current_backend.backend() != instance.backend {
            self.mark_recovery(id)?;
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "instance {id} backend owner identity does not match durable state"
            )));
        }
        self.require_restore_backend_live(id, &current_backend)
            .await?;
        if !self.storage.supports_checkpoint_restore() {
            return Err(BlazeDaemonError::UnsupportedOperation(format!(
                "instance {id} configured storage does not support checkpoint restore"
            )));
        }
        let spawner = self.spawner(target_metadata.backend).ok_or_else(|| {
            BlazeDaemonError::UnsupportedOperation(format!(
                "instance {id} has no restore adapter for {}",
                target_metadata.backend
            ))
        })?;
        // Pin the executable once. The capability check below and the launch
        // that happens after the running sandbox is stopped must name the same
        // file, or an executable replaced in between would only be noticed once
        // the original was already gone. A backend that needs no executable of
        // its own carries no configured path, and refusing it here would hide
        // the adapter's own answer about whether it supports restore at all.
        let executable = if request.binary_path.as_os_str().is_empty() {
            None
        } else {
            Some(std::sync::Arc::new(crate::spawner::PinnedExecutable::open(
                &request.binary_path,
            )?))
        };
        let capability = spawner
            .restore_capability(executable.as_deref())
            .await?
            .ok_or_else(|| {
                BlazeDaemonError::UnsupportedOperation(format!(
                    "instance {id} backend {} does not support checkpoint restore",
                    target_metadata.backend
                ))
            })?;
        if capability.backend != target_metadata.backend
            || capability.version != target_metadata.backend_version
            || capability.snapshot_kind != target_metadata.snapshot_kind
        {
            return Err(BlazeDaemonError::UnsupportedOperation(format!(
                "checkpoint {} requires {} version {:?} {:?}, but the current adapter provides \
                 {} version {:?} {:?}",
                request.checkpoint_id,
                target_metadata.backend,
                target_metadata.backend_version,
                target_metadata.snapshot_kind,
                capability.backend,
                capability.version,
                capability.snapshot_kind
            )));
        }
        let storage = self.storage.reconstruct(&id.to_string()).await?;
        let expose_guest_socket = !current_backend.guest_socket_path().as_os_str().is_empty();
        // Probe the network shape while the captured owner is still alive: its
        // cleanup removes the host device the snapshot names, so the replacement
        // has to be started with the same shape to rebind during load.
        let preserve_network = current_backend.holds_network_slot();
        let record_console_log = current_backend.records_console_log();

        instance.begin_restore_operation(request.checkpoint_id.clone())?;
        crate::failpoint::state("restore-begin-state")
            .and_then(|_| self.persist_and_retain(instance.clone()))?;
        crate::failpoint::pause("restore-after-begin").await;

        let transaction = match crate::failpoint::storage("restore-storage-stage") {
            Ok(()) => {
                self.storage
                    .stage_checkpoint_restore(&storage, &rootfs_path)
                    .await
            }
            Err(error) => Err(error),
        };
        let transaction = match transaction {
            Ok(transaction) => transaction,
            Err(error) => {
                return Err(self
                    .fail_before_restore_stop(instance, None, error.into())
                    .await);
            }
        };
        if let Err(error) = instance
            .advance_restore_phase(OperationPhase::RestoreStorageStaged)
            .map_err(BlazeDaemonError::from)
            .and_then(|_| {
                crate::failpoint::state("restore-staged-state")?;
                self.persist_and_retain(instance.clone())
            })
        {
            return Err(self
                .fail_before_restore_stop(instance, Some(&transaction), error)
                .await);
        }
        crate::failpoint::pause("restore-after-stage").await;

        let stopped = match crate::failpoint::backend("restore-backend-stop") {
            Ok(()) => current_backend.kill().await,
            Err(error) => Err(error),
        };
        if let Err(error) = stopped {
            instance.backend_ownership = BackendOwnership::Unknown;
            let abort = self
                .storage
                .abort_checkpoint_restore(&transaction)
                .await
                .err();
            return Err(self.fail_after_restore_stop(
                instance,
                format!(
                    "current backend termination failed: {error}{}",
                    abort
                        .map(|error| format!("; staged storage cleanup failed: {error}"))
                        .unwrap_or_default()
                ),
            ));
        }

        instance.backend_ownership = BackendOwnership::Stopped;
        let stopped_state = instance
            .advance_restore_phase(OperationPhase::RestoreBackendStopped)
            .and_then(|_| instance.transition(SandboxState::Restoring))
            .map_err(BlazeDaemonError::from)
            .and_then(|_| {
                crate::failpoint::state("restore-stopped-state")?;
                self.persist_and_retain(instance.clone())
            });
        if let Err(error) = stopped_state {
            self.remove_backend_owner(id);
            return Err(self.fail_after_restore_stop(
                instance,
                format!("backend stopped but lifecycle commit failed: {error}"),
            ));
        }
        self.remove_backend_owner(id);
        crate::failpoint::pause("restore-after-stop").await;

        let activated = match crate::failpoint::storage("restore-storage-activate") {
            Ok(()) => self.storage.activate_checkpoint_restore(&transaction).await,
            Err(error) => Err(error),
        };
        if let Err(error) = activated {
            let abort = self
                .storage
                .abort_checkpoint_restore(&transaction)
                .await
                .err();
            return Err(self.fail_after_restore_stop(
                instance,
                format!(
                    "replacement storage activation failed: {error}{}",
                    abort
                        .map(|error| format!("; predecessor restore failed: {error}"))
                        .unwrap_or_default()
                ),
            ));
        }
        if let Err(error) = instance
            .advance_restore_phase(OperationPhase::RestoreStorageActivated)
            .map_err(BlazeDaemonError::from)
            .and_then(|_| {
                crate::failpoint::state("restore-activated-state")?;
                self.persist_and_retain(instance.clone())
            })
        {
            return Err(self.fail_after_restore_stop(
                instance,
                format!("replacement storage activated but lifecycle commit failed: {error}"),
            ));
        }
        crate::failpoint::pause("restore-after-activate").await;

        let run_dir = match self.run_directory(id) {
            Ok(run_dir) => run_dir,
            Err(error) => {
                return Err(self.fail_after_restore_stop(
                    instance,
                    format!("runtime directory lookup after storage activation failed: {error}"),
                ));
            }
        };
        if let Err(error) = spawner.prepare_spawn(&run_dir).await {
            return Err(self.fail_after_restore_stop(
                instance,
                format!("prepare replacement backend ownership failed: {error}"),
            ));
        }
        instance.backend_ownership = BackendOwnership::Starting;
        if let Err(error) = crate::failpoint::state("restore-starting-state")
            .and_then(|_| self.persist_and_retain(instance.clone()))
        {
            return Err(self.fail_after_restore_stop(
                instance,
                format!("replacement backend intent commit failed: {error}"),
            ));
        }

        let restored = match crate::failpoint::backend("restore-backend-start") {
            Ok(()) => match BackendRestoreRequest::new(
                RestoreRequest {
                    instance_id: id,
                    binary_path: request.binary_path,
                    storage,
                    payload_dir: backend_payload_dir,
                    checkpoint_backend: target_metadata.backend,
                    expected_version: target_metadata.backend_version.clone(),
                    snapshot_kind: target_metadata.snapshot_kind,
                    expose_guest_socket,
                    preserve_network,
                    record_console_log,
                    // A rollback reloads this sandbox's own capture.
                    snapshot_from_other_sandbox: false,
                },
                run_dir,
                executable.clone(),
            ) {
                Ok(request) => restore_with_runtime_directory(spawner.as_ref(), request).await,
                Err(error) => Err(crate::spawner::SpawnFailure::clean(error)),
            },
            Err(error) => Err(crate::spawner::SpawnFailure::clean(error)),
        };
        let restored = match restored {
            Ok(owner) => owner,
            Err(error) => {
                let (source, owner) = error.into_parts();
                if let Some(owner) = owner {
                    let _ = self.retain_backend(id, owner);
                    instance.backend_ownership = BackendOwnership::Running;
                } else {
                    instance.backend_ownership = BackendOwnership::Stopped;
                }
                return Err(self.fail_after_restore_stop(
                    instance,
                    format!("replacement backend start failed: {source}"),
                ));
            }
        };
        if let Some(error) = self.retain_backend(id, restored.clone()) {
            instance.backend_ownership = BackendOwnership::Running;
            return Err(self.fail_after_restore_stop(instance, error));
        }
        instance.backend_ownership = BackendOwnership::Running;

        if restored.instance_id() != id
            || restored.backend() != target_metadata.backend
            || restored.version().map(str::to_string) != target_metadata.backend_version
        {
            return Err(self.fail_after_restore_stop(
                instance,
                format!(
                    "replacement backend identity ({}, {}, {:?}) does not match checkpoint \
                     identity ({id}, {}, {:?})",
                    restored.instance_id(),
                    restored.backend(),
                    restored.version(),
                    target_metadata.backend,
                    target_metadata.backend_version
                ),
            ));
        }
        if let Err(error) = instance
            .advance_restore_phase(OperationPhase::RestoreBackendStarted)
            .map_err(BlazeDaemonError::from)
            .and_then(|_| {
                crate::failpoint::state("restore-started-state")?;
                self.persist_and_retain(instance.clone())
            })
        {
            return Err(self.fail_after_restore_stop(
                instance,
                format!("replacement backend started but lifecycle commit failed: {error}"),
            ));
        }
        if let Err(error) = self
            .verify_restored_backend(id, &restored, expose_guest_socket)
            .await
        {
            return Err(self.fail_after_restore_stop(
                instance,
                format!("replacement backend readiness failed: {error}"),
            ));
        }

        let head_updated = match crate::failpoint::storage("restore-head-update") {
            Ok(()) => {
                let checkpoints = self.checkpoints.clone();
                crate::failpoint::spawn_blocking(move || {
                    checkpoints
                        .set_head_verified(&target)
                        .map_err(checkpoint_store_error)
                })
                .await
                .map_err(|error| {
                    BlazeDaemonError::RecoveryRequired(format!(
                        "checkpoint HEAD update blocking task stopped unexpectedly: {error}"
                    ))
                })?
            }
            Err(error) => Err(error.into()),
        };
        if let Err(error) = head_updated {
            let observed = self.observe_checkpoint_head(id).await;
            return Err(self.fail_after_restore_stop(
                instance,
                format!(
                    "checkpoint HEAD update failed: {error}; observed HEAD after failure: \
                     {observed:?}"
                ),
            ));
        }
        if let Err(error) = instance
            .advance_restore_phase(OperationPhase::RestoreHeadUpdated)
            .map_err(BlazeDaemonError::from)
            .and_then(|_| {
                crate::failpoint::state("restore-head-state")?;
                self.persist_and_retain(instance.clone())
            })
        {
            return Err(self.fail_after_restore_stop(
                instance,
                format!("checkpoint HEAD changed but lifecycle commit failed: {error}"),
            ));
        }
        crate::failpoint::pause("restore-after-head").await;

        let committed = match crate::failpoint::storage("restore-storage-commit") {
            Ok(()) => self.storage.commit_checkpoint_restore(&transaction).await,
            Err(error) => Err(error),
        };
        if let Err(error) = committed {
            return Err(self.fail_after_restore_stop(
                instance,
                format!("replacement storage commit failed: {error}"),
            ));
        }
        if let Err(error) = instance
            .advance_restore_phase(OperationPhase::RestoreStorageCommitted)
            .map_err(BlazeDaemonError::from)
            .and_then(|_| {
                crate::failpoint::state("restore-committed-state")?;
                self.persist_and_retain(instance.clone())
            })
        {
            return Err(self.fail_after_restore_stop(
                instance,
                format!("replacement storage committed but lifecycle commit failed: {error}"),
            ));
        }

        let recovery_instance = instance.clone();
        instance.transition(SandboxState::Running)?;
        instance.finish_operation();
        if let Err(error) = crate::failpoint::state("restore-final-state")
            .and_then(|_| self.persist_and_retain(instance.clone()))
        {
            return Err(self.fail_after_restore_stop(
                recovery_instance,
                format!("replacement is live but final lifecycle commit failed: {error}"),
            ));
        }
        Ok(RestoreSandboxResult {
            instance,
            checkpoint_id: request.checkpoint_id,
        })
    }

    /// Report which checkpoint HEAD names after a failed HEAD update.
    ///
    /// The observation stays on the blocking pool because it opens the catalog,
    /// and it deliberately skips artifact verification so the recorded
    /// identifier reaches the operator even when an artifact is unreadable.
    async fn observe_checkpoint_head(&self, id: Uuid) -> Result<Option<String>> {
        let checkpoints = self.checkpoints.clone();
        crate::failpoint::spawn_blocking(move || {
            checkpoints.read_head_id(id).map_err(checkpoint_store_error)
        })
        .await
        .map_err(|error| {
            BlazeDaemonError::Internal(format!(
                "checkpoint HEAD observation blocking task: {error}"
            ))
        })?
    }

    async fn require_restore_backend_live(
        &self,
        id: Uuid,
        backend: &DynBackendInstance,
    ) -> Result<()> {
        match backend.try_wait().await {
            Ok(None) => Ok(()),
            Ok(Some(result)) => {
                self.mark_recovery(id)?;
                Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} backend exited before restore \
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

    async fn verify_restored_backend(
        &self,
        id: Uuid,
        backend: &DynBackendInstance,
        expose_guest_socket: bool,
    ) -> Result<()> {
        self.require_restore_backend_live(id, backend).await?;
        if expose_guest_socket {
            // `wait_for_guest_ready` treats an empty socket path as immediately
            // ready, so an adapter that silently drops the guest transport would
            // otherwise pass readiness and commit a `Running` sandbox whose
            // exec, read, and write requests all fail with a conflict. Require
            // the replacement owner to expose the transport the captured runtime
            // had before publishing the restore.
            if backend.guest_socket_path().as_os_str().is_empty() {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} replacement backend does not expose the guest \
                     transport the checkpoint captured"
                )));
            }
            self.wait_for_guest_ready(backend, "restore-guest-ready")
                .await?;
        }
        self.require_restore_backend_live(id, backend).await
    }

    async fn fail_before_restore_stop(
        &self,
        mut instance: SandboxInstance,
        transaction: Option<&StorageRestoreTransaction>,
        original: BlazeDaemonError,
    ) -> BlazeDaemonError {
        let storage_cleanup = match transaction {
            Some(transaction) => self
                .storage
                .abort_checkpoint_restore(transaction)
                .await
                .map_err(BlazeDaemonError::from),
            None => self
                .storage
                .reconcile_checkpoint_restore(&instance.id.to_string())
                .await
                .map_err(BlazeDaemonError::from),
        };
        if let Err(cleanup) = storage_cleanup {
            return self.fail_after_restore_stop(
                instance,
                format!("{original}; staged storage cleanup failed: {cleanup}"),
            );
        }
        instance.finish_operation();
        if let Err(error) = self.persist_and_retain(instance.clone()) {
            return self.fail_after_restore_stop(
                instance,
                format!("{original}; restore journal cleanup failed: {error}"),
            );
        }
        original
    }

    fn fail_after_restore_stop(
        &self,
        instance: SandboxInstance,
        cause: impl std::fmt::Display,
    ) -> BlazeDaemonError {
        let id = instance.id;
        let recovery = self.mark_instance_recovery(instance).err();
        BlazeDaemonError::RecoveryRequired(format!(
            "restore {id}: {cause}; resources retained{}",
            recovery
                .map(|error| format!("; recovery state persistence failed: {error}"))
                .unwrap_or_default()
        ))
    }
}

fn checkpoint_store_error(error: impl std::fmt::Display) -> BlazeDaemonError {
    BlazeDaemonError::Internal(format!("checkpoint store: {error}"))
}

/// Map a catalog lookup failure for a caller-supplied checkpoint identifier.
///
/// An absent sandbox checkpoint namespace (path leaf = sandbox_id) or an absent
/// entry for the selected checkpoint itself (path leaf = selected_id) classifies
/// as not-found. Ancestor entries resolved through `validated_chain_from` carry
/// different path leaves; their absence is catalog corruption rather than a
/// permanent client-selection error, so they keep the internal classification.
fn checkpoint_lookup_error(
    error: CheckpointStoreError,
    sandbox_id: Uuid,
    selected_id: &str,
) -> BlazeDaemonError {
    if let CheckpointStoreError::Io {
        ref source,
        ref path,
        ..
    } = error
        && source.kind() == std::io::ErrorKind::NotFound
    {
        let leaf = path.file_name().and_then(|name| name.to_str());
        let sandbox_name = sandbox_id.to_string();
        if leaf.is_some_and(|name| name == selected_id || name == sandbox_name.as_str()) {
            return BlazeDaemonError::NotFound(format!("checkpoint store: {error}"));
        }
    }
    checkpoint_store_error(error)
}
