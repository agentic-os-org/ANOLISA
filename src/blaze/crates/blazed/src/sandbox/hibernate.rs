// SPDX-License-Identifier: Apache-2.0
//! Durable hibernation and restartable resume for managed sandboxes.

use std::collections::BTreeSet;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use blaze_core::backend::{BackendKind, RestoreRequest, SnapshotKind, SnapshotRequest};
use blaze_core::checkpoint::{CheckpointArtifact, PAYLOAD_BACKEND_DIR, validate_artifact_path};
use blaze_core::lifecycle::{BackendOwnership, OperationPhase, SandboxInstance, SandboxState};
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, fstat, fsync, mkdirat, openat,
    renameat_with, statat, unlinkat,
};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::OwnedMutexGuard;
use uuid::Uuid;

use crate::error::{BlazeDaemonError, Result};
use crate::spawner::{BackendRestoreRequest, DynBackendInstance, PinnedExecutable};
use crate::state_store::OwnedRunDir;

use super::manager::SandboxManager;

const HIBERNATE_FORMAT_VERSION: u32 = 1;
const HIBERNATE_DIRECTORY: &str = "hibernate";
const HIBERNATE_DIRECTORY_MODE: Mode = Mode::RWXU;
const HIBERNATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const MANIFEST_ARTIFACT: &str = "manifest.json";
/// Deepest payload nesting hibernation will walk, matching the pure
/// artifact-path bound so every publishable payload stays enumerable.
const MAX_PAYLOAD_DEPTH: usize = 16;
/// Deepest nesting removal will traverse. Deliberately far above
/// [`MAX_PAYLOAD_DEPTH`]: a payload rejected for being too deep must still
/// be removable by compensation and destroy, so this bound only protects
/// the daemon from descriptor exhaustion on a pathological tree.
const MAX_REMOVAL_DEPTH: usize = 256;

/// Inputs resolved from the current daemon configuration before hibernation.
#[derive(Debug, Clone)]
pub struct HibernateSandbox {
    /// Current executable for the sandbox backend.
    pub binary_path: PathBuf,
}

/// Inputs resolved from the current daemon configuration before resume.
#[derive(Debug, Clone)]
pub struct ResumeSandbox {
    /// Current executable for the sandbox backend.
    pub binary_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HibernateManifest {
    format_version: u32,
    sandbox_id: Uuid,
    policy_name: String,
    image_digest: String,
    backend: BackendKind,
    backend_version: Option<String>,
    snapshot_kind: SnapshotKind,
    expose_guest_socket: bool,
    /// Host shape the captured runtime held, recorded because a resume may
    /// happen after a daemon restart. The generic restore transaction can probe
    /// the owner it is about to replace; hibernation has no such owner left, so
    /// the shape has to be frozen here while the runtime is still alive or the
    /// replacement would silently come back without a network slot or without
    /// recording console output.
    preserve_network: bool,
    record_console_log: bool,
    artifacts: Vec<CheckpointArtifact>,
}

impl SandboxManager {
    /// Stop a running backend after publishing durable hibernation artifacts.
    ///
    /// The work runs in a detached supervisor that retains the per-sandbox
    /// operation lock, matching checkpoint and restore: a client that
    /// cancels the request cannot drop the future mid-transaction and
    /// strand a quiesced backend without compensation.
    pub async fn hibernate(
        self: &Arc<Self>,
        id: Uuid,
        request: HibernateSandbox,
    ) -> Result<SandboxInstance> {
        let operation = self.operation_lock(id).lock_owned().await;
        let manager = Arc::clone(self);
        crate::failpoint::spawn(async move {
            manager.hibernate_supervised(id, request, operation).await
        })
        .await
        .map_err(|error| {
            let recovery = self.mark_recovery(id).err();
            BlazeDaemonError::RecoveryRequired(format!(
                "hibernate supervisor stopped unexpectedly: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            ))
        })?
    }

    async fn hibernate_supervised(
        self: Arc<Self>,
        id: Uuid,
        request: HibernateSandbox,
        operation: OwnedMutexGuard<()>,
    ) -> Result<SandboxInstance> {
        let manager = Arc::clone(&self);
        let result =
            match crate::failpoint::spawn(
                async move { manager.hibernate_worker(id, request).await },
            )
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    let recovery = self.mark_recovery(id).err();
                    Err(BlazeDaemonError::RecoveryRequired(format!(
                        "hibernate worker stopped unexpectedly: {error}{}",
                        recovery
                            .map(|error| format!("; recovery state persistence failed: {error}"))
                            .unwrap_or_default()
                    )))
                }
            };
        drop(operation);
        result
    }

    fn hibernate_worker(
        self: Arc<Self>,
        id: Uuid,
        request: HibernateSandbox,
    ) -> Pin<Box<dyn Future<Output = Result<SandboxInstance>> + Send>> {
        Box::pin(async move {
            let mut instance = self.get(id)?;
            require_quiescent_state(&instance, SandboxState::Running)?;

            let backend = self.backend_owner(id).ok_or_else(|| {
                BlazeDaemonError::Conflict(format!("instance {id} has no backend owner"))
            })?;
            if backend.instance_id() != id || backend.backend() != instance.backend {
                self.mark_instance_recovery(instance)?;
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} backend owner identity does not match durable state"
                )));
            }
            require_backend_live(id, &backend).await.map_err(|error| {
                let recovery = self.mark_instance_recovery(instance.clone()).err();
                with_recovery_error(error, recovery)
            })?;
            if !backend.supports_checkpoint_capture() {
                return Err(BlazeDaemonError::UnsupportedOperation(format!(
                    "instance {id} backend {} does not support hibernation",
                    backend.backend()
                )));
            }

            let spawner = self.spawner(instance.backend).ok_or_else(|| {
                BlazeDaemonError::UnsupportedOperation(format!(
                    "instance {id} has no resume adapter for {}",
                    instance.backend
                ))
            })?;
            let capability = spawner
                .restore_capability(pinned_executable(&request.binary_path)?.as_deref())
                .await?
                .ok_or_else(|| {
                    BlazeDaemonError::UnsupportedOperation(format!(
                        "instance {id} backend {} does not support resume",
                        instance.backend
                    ))
                })?;
            let backend_version = backend.version().map(str::to_string);
            if capability.backend != instance.backend
                || capability.version != backend_version
                || capability.snapshot_kind != SnapshotKind::Full
            {
                return Err(BlazeDaemonError::UnsupportedOperation(format!(
                    "instance {id} backend capture identity does not match its resume adapter"
                )));
            }
            let storage = self.storage.reconstruct(&id.to_string()).await?;
            // Freeze the host shape while the runtime that owns it is still
            // alive: stopping it removes the devices a resume has to rebind, and
            // a resume after a daemon restart has nothing left to ask.
            let host_shape = CapturedHostShape {
                expose_guest_socket: !backend.guest_socket_path().as_os_str().is_empty(),
                preserve_network: backend.holds_network_slot(),
                record_console_log: backend.records_console_log(),
            };
            let sandbox_dir = match self.hibernate_root(id) {
                Ok(sandbox_dir) => sandbox_dir,
                Err(error) => {
                    let recovery = self.mark_instance_recovery(instance).err();
                    return Err(with_recovery_error(error, recovery));
                }
            };
            if let Err(error) = prepare_hibernate_directory(&sandbox_dir) {
                let recovery = self.mark_instance_recovery(instance).err();
                return Err(with_recovery_error(error, recovery));
            }

            instance.begin_hibernate_operation()?;
            instance.transition(SandboxState::Hibernating)?;
            if let Err(error) = crate::failpoint::state("hibernate-begin-state")
                .and_then(|_| self.persist_and_retain(instance.clone()))
            {
                // The durable record may already say `Hibernating` (rename
                // succeeded) even though the fsync failed; the backend is
                // still running and unpauseable from durable state alone.
                // Mark recovery so startup does not interpret this as an
                // interrupted hibernate with a live backend.
                let recovery = self.mark_instance_recovery(instance.clone()).err();
                return Err(with_recovery_error(error, recovery));
            }
            crate::failpoint::pause("hibernate-after-begin").await;

            let paused = match crate::failpoint::backend("hibernate-pause") {
                Ok(()) => backend.quiesce_for_capture().await,
                Err(error) => Err(error),
            };
            if let Err(error) = paused {
                return Err(self
                    .compensate_hibernate(
                        instance,
                        &backend,
                        None,
                        format!("backend pause failed: {error}"),
                    )
                    .await);
            }
            if let Err(error) = instance
                .advance_hibernate_phase(OperationPhase::HibernatePaused)
                .map_err(BlazeDaemonError::from)
                .and_then(|_| {
                    crate::failpoint::state("hibernate-paused-state")?;
                    self.persist_and_retain(instance.clone())
                })
            {
                return Err(self
                    .compensate_hibernate(
                        instance,
                        &backend,
                        None,
                        format!("paused-state commit failed: {error}"),
                    )
                    .await);
            }

            let staging_name = hibernate_staging_name();
            let staging_dir = match create_child_directory(&sandbox_dir, &staging_name) {
                Ok(staging_dir) => staging_dir,
                Err(error) => {
                    return Err(self
                        .compensate_hibernate(
                            instance,
                            &backend,
                            Some((&sandbox_dir, &staging_name)),
                            format!("staging directory creation failed: {error}"),
                        )
                        .await);
                }
            };
            // The backend owns one payload subtree and chooses its internal
            // layout; the manifest step below inventories whatever regular
            // files landed there and hashes them through the staging
            // descriptor before the image is published.
            let payload_dir = match staging_dir.create_subdirectory(PAYLOAD_BACKEND_DIR) {
                Ok(payload_dir) => payload_dir,
                Err(error) => {
                    return Err(self
                        .compensate_hibernate(
                            instance,
                            &backend,
                            Some((&sandbox_dir, &staging_name)),
                            format!("payload directory creation failed: {error}"),
                        )
                        .await);
                }
            };
            let snapshot = match crate::failpoint::backend("hibernate-snapshot") {
                Ok(()) => {
                    backend
                        .snapshot(SnapshotRequest {
                            // The configured pathname is handed out because
                            // capture may exec an external backend process
                            // that cannot resolve this daemon's
                            // /proc/self/fd names; the manifest step hashes
                            // and revalidates through retained descriptors.
                            payload_dir: payload_dir.configured_path().to_path_buf(),
                            kind: SnapshotKind::Full,
                        })
                        .await
                }
                Err(error) => Err(error),
            };
            drop(payload_dir);
            if let Err(error) = snapshot {
                return Err(self
                    .compensate_hibernate(
                        instance,
                        &backend,
                        Some((&sandbox_dir, &staging_name)),
                        format!("snapshot capture failed: {error}"),
                    )
                    .await);
            }
            let flushed = match crate::failpoint::storage("hibernate-storage-flush") {
                Ok(()) => self.storage.sync_artifacts(&storage).await,
                Err(error) => Err(error),
            };
            if let Err(error) = flushed {
                return Err(self
                    .compensate_hibernate(
                        instance,
                        &backend,
                        Some((&sandbox_dir, &staging_name)),
                        format!("storage flush failed: {error}"),
                    )
                    .await);
            }

            let manifest = match build_hibernate_manifest(
                &staging_dir,
                &instance,
                capability.version,
                host_shape,
            )
            .await
            {
                Ok(manifest) => manifest,
                Err(error) => {
                    return Err(self
                        .compensate_hibernate(
                            instance,
                            &backend,
                            Some((&sandbox_dir, &staging_name)),
                            format!("artifact hashing failed: {error}"),
                        )
                        .await);
                }
            };
            if let Err(error) = write_and_sync_manifest(&staging_dir, &manifest).await {
                return Err(self
                    .compensate_hibernate(
                        instance,
                        &backend,
                        Some((&sandbox_dir, &staging_name)),
                        format!("artifact publication failed: {error}"),
                    )
                    .await);
            }
            // The staging-root check only sees the staging directory. A capture
            // that escaped its configured payload path (for example through
            // `payload_dir.join("../../hibernate")`) can instead have dropped a
            // stray entry directly in the sandbox run directory, which would
            // survive until publication tries to rename onto it after the
            // backend is already stopped. Reject it here, while the backend is
            // still compensable, so a misbehaving adapter cannot cost the
            // running sandbox.
            if let Err(error) = require_publishable_sandbox_root(&sandbox_dir, &staging_name) {
                return Err(self
                    .compensate_hibernate(
                        instance,
                        &backend,
                        Some((&sandbox_dir, &staging_name)),
                        format!(
                            "sandbox directory holds an unpublishable hibernation entry: {error}"
                        ),
                    )
                    .await);
            }
            if let Err(error) = instance
                .advance_hibernate_phase(OperationPhase::HibernateArtifactsSynced)
                .map_err(BlazeDaemonError::from)
                .and_then(|_| {
                    crate::failpoint::state("hibernate-artifacts-state")?;
                    self.persist_and_retain(instance.clone())
                })
            {
                return Err(self
                    .compensate_hibernate(
                        instance,
                        &backend,
                        Some((&sandbox_dir, &staging_name)),
                        format!("artifact-state commit failed: {error}"),
                    )
                    .await);
            }
            // The staging descriptor is no longer needed: publication renames
            // the name, and every later step reopens through the sandbox
            // descriptor.
            drop(staging_dir);

            let stopped = match crate::failpoint::backend("hibernate-backend-stop") {
                Ok(()) => backend.kill().await,
                Err(error) => Err(error),
            };
            if let Err(error) = stopped {
                instance.backend_ownership = BackendOwnership::Unknown;
                return Err(self.fail_hibernate_after_stop(
                    instance,
                    format!("backend termination failed: {error}"),
                ));
            }
            instance.backend_ownership = BackendOwnership::Stopped;
            if let Err(error) = instance
                .advance_hibernate_phase(OperationPhase::HibernateBackendStopped)
                .map_err(BlazeDaemonError::from)
                .and_then(|_| {
                    crate::failpoint::state("hibernate-stopped-state")?;
                    self.persist_and_retain(instance.clone())
                })
            {
                return Err(self.fail_hibernate_after_stop(
                    instance,
                    format!("backend stopped but lifecycle commit failed: {error}"),
                ));
            }
            self.remove_backend_owner(id);
            crate::failpoint::pause("hibernate-after-stop").await;

            let backup_name = hibernate_backup_name();
            let previous = match optional_child_directory(&sandbox_dir, hibernate_dir_name()) {
                Ok(previous) => previous,
                Err(error) => {
                    return Err(self.fail_hibernate_after_stop(
                        instance,
                        format!("previous hibernation lookup failed: {error}"),
                    ));
                }
            };
            let had_previous = previous.is_some();
            drop(previous);
            if had_previous {
                if let Err(error) = renameat_with(
                    sandbox_dir.descriptor(),
                    hibernate_dir_name(),
                    sandbox_dir.descriptor(),
                    backup_name.as_str(),
                    RenameFlags::NOREPLACE,
                ) {
                    return Err(self.fail_hibernate_after_stop(
                        instance,
                        format!("previous hibernation backup failed: {error}"),
                    ));
                }
                if let Err(error) = sync_run_dir(&sandbox_dir) {
                    return Err(self.fail_hibernate_after_stop(
                        instance,
                        format!("previous hibernation backup sync failed: {error}"),
                    ));
                }
            }
            let published = match crate::failpoint::storage("hibernate-publish") {
                Ok(()) => renameat_with(
                    sandbox_dir.descriptor(),
                    staging_name.as_str(),
                    sandbox_dir.descriptor(),
                    hibernate_dir_name(),
                    RenameFlags::NOREPLACE,
                )
                .map_err(|source| {
                    hibernate_io_error(
                        "publish hibernation directory",
                        sandbox_dir.configured_path().join(hibernate_dir_name()),
                        std::io::Error::from(source),
                    )
                }),
                Err(error) => Err(error.into()),
            };
            if let Err(error) = published {
                return Err(self.fail_hibernate_after_stop(
                    instance,
                    format!("hibernate directory publication failed: {error}"),
                ));
            }
            if let Err(error) = sync_run_dir(&sandbox_dir) {
                return Err(self.fail_hibernate_after_stop(
                    instance,
                    format!("hibernate directory sync failed: {error}"),
                ));
            }
            if let Err(error) = instance
                .advance_hibernate_phase(OperationPhase::HibernatePublished)
                .map_err(BlazeDaemonError::from)
                .and_then(|_| {
                    crate::failpoint::state("hibernate-published-state")?;
                    self.persist_and_retain(instance.clone())
                })
            {
                return Err(self.fail_hibernate_after_stop(
                    instance,
                    format!("published-state commit failed: {error}"),
                ));
            }

            let recovery = instance.clone();
            instance.transition(SandboxState::Hibernated)?;
            instance.finish_operation();
            if let Err(error) = crate::failpoint::state("hibernate-final-state")
                .and_then(|_| self.persist_and_retain(instance.clone()))
            {
                return Err(self.fail_hibernate_after_stop(
                    recovery,
                    format!("final hibernated-state commit failed: {error}"),
                ));
            }
            if had_previous && let Err(error) = remove_child_directory(&sandbox_dir, &backup_name) {
                tracing::warn!(
                    instance = %id,
                    %error,
                    "obsolete hibernation backup retained for later cleanup"
                );
            }
            Ok(instance)
        })
    }

    /// Start a backend from verified hibernation artifacts.
    ///
    /// The work runs in a detached supervisor that retains the per-sandbox
    /// operation lock, matching checkpoint and hibernate: a client that
    /// cancels the request cannot drop the future mid-transaction and
    /// strand a starting backend without compensation.
    pub async fn resume(
        self: &Arc<Self>,
        id: Uuid,
        request: ResumeSandbox,
    ) -> Result<SandboxInstance> {
        let operation = self.operation_lock(id).lock_owned().await;
        let manager = Arc::clone(self);
        crate::failpoint::spawn(
            async move { manager.resume_supervised(id, request, operation).await },
        )
        .await
        .map_err(|error| {
            let recovery = self.mark_recovery(id).err();
            BlazeDaemonError::RecoveryRequired(format!(
                "resume supervisor stopped unexpectedly: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            ))
        })?
    }

    async fn resume_supervised(
        self: Arc<Self>,
        id: Uuid,
        request: ResumeSandbox,
        operation: OwnedMutexGuard<()>,
    ) -> Result<SandboxInstance> {
        let manager = Arc::clone(&self);
        let result =
            match crate::failpoint::spawn(async move { manager.resume_worker(id, request).await })
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    let recovery = self.mark_recovery(id).err();
                    Err(BlazeDaemonError::RecoveryRequired(format!(
                        "resume worker stopped unexpectedly: {error}{}",
                        recovery
                            .map(|error| format!("; recovery state persistence failed: {error}"))
                            .unwrap_or_default()
                    )))
                }
            };
        drop(operation);
        result
    }

    fn resume_worker(
        self: Arc<Self>,
        id: Uuid,
        request: ResumeSandbox,
    ) -> Pin<Box<dyn Future<Output = Result<SandboxInstance>> + Send>> {
        Box::pin(async move {
            let mut instance = self.get(id)?;
            require_quiescent_state(&instance, SandboxState::Hibernated)?;
            if instance.backend_ownership != BackendOwnership::Stopped {
                self.mark_instance_recovery(instance)?;
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} is hibernated with unresolved backend ownership"
                )));
            }
            if self.backend_owner(id).is_some() {
                self.mark_instance_recovery(instance)?;
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} is hibernated but still retains a backend owner"
                )));
            }

            let sandbox_dir = match self.hibernate_root(id) {
                Ok(sandbox_dir) => sandbox_dir,
                Err(error) => {
                    let recovery = self.mark_instance_recovery(instance).err();
                    return Err(with_recovery_error(error, recovery));
                }
            };
            let hibernate_dir = match open_child_directory(&sandbox_dir, hibernate_dir_name()) {
                Ok(hibernate_dir) => hibernate_dir,
                Err(error) => {
                    let recovery = self.mark_instance_recovery(instance).err();
                    return Err(with_recovery_error(
                        BlazeDaemonError::RecoveryRequired(format!(
                            "instance {id} hibernation image is unavailable: {error}"
                        )),
                        recovery,
                    ));
                }
            };
            let (manifest, retained_artifacts) = load_and_verify_manifest(&hibernate_dir)
                .await
                .map_err(|error| {
                    let recovery = self.mark_instance_recovery(instance.clone()).err();
                    with_recovery_error(
                        BlazeDaemonError::RecoveryRequired(format!(
                            "instance {id} hibernation artifacts are invalid: {error}"
                        )),
                        recovery,
                    )
                })?;
            if let Err(error) = validate_manifest_identity(&manifest, &instance) {
                let recovery = self.mark_instance_recovery(instance).err();
                return Err(with_recovery_error(error, recovery));
            }
            // Retain the payload subtree descriptor for the whole restore so
            // path replacement cannot swap the image between verification and
            // consumption; the backend re-reads its own capture layout below.
            let payload_dir = match hibernate_dir.open_subdirectory(PAYLOAD_BACKEND_DIR) {
                Ok(payload_dir) => payload_dir,
                Err(error) => {
                    let recovery = self.mark_instance_recovery(instance).err();
                    return Err(with_recovery_error(
                        BlazeDaemonError::RecoveryRequired(format!(
                            "instance {id} hibernation payload is unavailable: {error}"
                        )),
                        recovery,
                    ));
                }
            };
            let spawner = self.spawner(instance.backend).ok_or_else(|| {
                BlazeDaemonError::UnsupportedOperation(format!(
                    "instance {id} has no resume adapter for {}",
                    instance.backend
                ))
            })?;
            // Pin the executable once. The capability check below and the launch
            // that happens after the resume transaction has begun must name the
            // same file: an executable replaced in between would otherwise only
            // be noticed once the sandbox could no longer be left hibernated.
            let executable = pinned_executable(&request.binary_path)?;
            let capability = spawner
                .restore_capability(executable.as_deref())
                .await?
                .ok_or_else(|| {
                    BlazeDaemonError::UnsupportedOperation(format!(
                        "instance {id} backend {} does not support resume",
                        instance.backend
                    ))
                })?;
            if capability.backend != manifest.backend
                || capability.version != manifest.backend_version
                || capability.snapshot_kind != manifest.snapshot_kind
            {
                return Err(BlazeDaemonError::UnsupportedOperation(format!(
                    "instance {id} hibernation image is incompatible with the current resume adapter"
                )));
            }
            let storage = self.storage.reconstruct(&id.to_string()).await?;

            instance.begin_resume_operation()?;
            instance.transition(SandboxState::Resuming)?;
            if let Err(error) = crate::failpoint::state("resume-begin-state")
                .and_then(|_| self.persist_and_retain(instance.clone()))
            {
                // The durable record may already say `Resuming` because the
                // rename succeeded and only the directory sync failed, while
                // the retained instance is still `Hibernated`. Nothing has
                // been started yet, so converge both back to hibernated: a
                // restart that read a half-committed resume intent would find
                // no owner and demote an untouched, resumable sandbox to
                // recovery-required with destroy as its only exit.
                return Err(self.fail_resume_without_owner(
                    instance,
                    format!("resume intent commit failed: {error}"),
                ));
            }
            crate::failpoint::pause("resume-after-begin").await;

            let run_dir = match self.run_directory(id) {
                Ok(run_dir) => run_dir,
                Err(error) => {
                    return Err(self.fail_resume_without_owner(
                        instance,
                        format!("resume runtime directory lookup failed: {error}"),
                    ));
                }
            };
            if let Err(error) = spawner.prepare_spawn(&run_dir).await {
                return Err(self.fail_resume_without_owner(
                    instance,
                    format!("resume ownership preparation failed: {error}"),
                ));
            }
            instance.backend_ownership = BackendOwnership::Starting;
            if let Err(error) = instance
                .advance_resume_phase(OperationPhase::ResumeBackendStarting)
                .map_err(BlazeDaemonError::from)
                .and_then(|_| {
                    crate::failpoint::state("resume-starting-state")?;
                    self.persist_and_retain(instance.clone())
                })
            {
                return Err(self.fail_resume_without_owner(
                    instance,
                    format!("resume ownership intent commit failed: {error}"),
                ));
            }

            let restored = match crate::failpoint::backend("resume-backend-start") {
                Ok(()) => match BackendRestoreRequest::new(
                    RestoreRequest {
                        instance_id: id,
                        binary_path: request.binary_path,
                        storage,
                        // The configured pathname is handed out because the
                        // restore adapter may exec an external backend
                        // process; every artifact was hashed through the
                        // retained hibernate-directory descriptor just above.
                        payload_dir: payload_dir.configured_path().to_path_buf(),
                        checkpoint_backend: manifest.backend,
                        expected_version: manifest.backend_version.clone(),
                        snapshot_kind: manifest.snapshot_kind,
                        expose_guest_socket: manifest.expose_guest_socket,
                        // The captured host shape comes from the manifest rather
                        // than from a live owner: hibernation stopped that owner,
                        // and a resume may run after a daemon restart.
                        preserve_network: manifest.preserve_network,
                        record_console_log: manifest.record_console_log,
                        // Resume reloads this sandbox's own hibernation image.
                        snapshot_from_other_sandbox: false,
                    },
                    run_dir,
                    executable.clone(),
                ) {
                    Ok(request) => spawner.restore(request).await,
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
                        return Err(self.fail_resume_with_owner(
                            instance,
                            format!("resume backend start failed: {source}"),
                        ));
                    }
                    return Err(self.fail_resume_without_owner(
                        instance,
                        format!("resume backend start failed: {source}"),
                    ));
                }
            };
            instance.backend_ownership = BackendOwnership::Running;
            if let Some(error) = self.retain_backend(id, restored.clone()) {
                return Err(self.fail_resume_with_owner(instance, error));
            }
            if restored.instance_id() != id
                || restored.backend() != manifest.backend
                || restored.version().map(str::to_string) != manifest.backend_version
            {
                return Err(self
                    .abort_resumed_backend(
                        instance,
                        &restored,
                        "restored backend identity does not match the hibernation manifest"
                            .to_string(),
                    )
                    .await);
            }
            if let Err(error) = instance
                .advance_resume_phase(OperationPhase::ResumeBackendStarted)
                .map_err(BlazeDaemonError::from)
                .and_then(|_| {
                    crate::failpoint::state("resume-started-state")?;
                    self.persist_and_retain(instance.clone())
                })
            {
                return Err(self.fail_resume_with_owner(
                    instance,
                    format!("restored backend ownership commit failed: {error}"),
                ));
            }
            if let Err(error) = self
                .verify_resumed_backend(id, &restored, manifest.expose_guest_socket)
                .await
            {
                return Err(self
                    .abort_resumed_backend(
                        instance,
                        &restored,
                        format!("restored backend readiness failed: {error}"),
                    )
                    .await);
            }
            // The adapter consumed the payload by pathname; before the
            // sandbox publishes `running`, prove the consumed objects are
            // still the verified ones. A swap in between tears the
            // replacement down instead of running from unverified bytes.
            if let Err(error) =
                require_payload_identity(&sandbox_dir, &hibernate_dir, &retained_artifacts)
            {
                return Err(self
                    .abort_resumed_backend(
                        instance,
                        &restored,
                        format!("hibernation payload changed during resume: {error}"),
                    )
                    .await);
            }
            if let Err(error) = instance
                .advance_resume_phase(OperationPhase::ResumeBackendReady)
                .map_err(BlazeDaemonError::from)
                .and_then(|_| {
                    crate::failpoint::state("resume-ready-state")?;
                    self.persist_and_retain(instance.clone())
                })
            {
                return Err(self.fail_resume_with_owner(
                    instance,
                    format!("restored backend readiness commit failed: {error}"),
                ));
            }

            let recovery = instance.clone();
            instance.transition(SandboxState::Running)?;
            instance.finish_operation();
            if let Err(error) = crate::failpoint::state("resume-final-state")
                .and_then(|_| self.persist_and_retain(instance.clone()))
            {
                return Err(self.fail_resume_with_owner(
                    recovery,
                    format!("final running-state commit failed: {error}"),
                ));
            }
            Ok(instance)
        })
    }

    async fn compensate_hibernate(
        &self,
        mut instance: SandboxInstance,
        backend: &DynBackendInstance,
        staging: Option<(&OwnedRunDir, &str)>,
        cause: String,
    ) -> BlazeDaemonError {
        let resumed = match crate::failpoint::backend("hibernate-compensation-resume") {
            Ok(()) => backend.unquiesce_after_capture().await,
            Err(error) => Err(error),
        };
        if let Err(error) = resumed {
            instance.backend_ownership = BackendOwnership::Unknown;
            return self.fail_hibernate_after_stop(
                instance,
                format!("{cause}; backend resume compensation failed: {error}"),
            );
        }
        if let Err(error) = self
            .verify_resumed_backend(
                instance.id,
                backend,
                !backend.guest_socket_path().as_os_str().is_empty(),
            )
            .await
        {
            instance.backend_ownership = BackendOwnership::Unknown;
            return self.fail_hibernate_after_stop(
                instance,
                format!("{cause}; resumed backend readiness failed: {error}"),
            );
        }
        if let Some((sandbox_dir, staging_name)) = staging
            && let Err(error) = remove_child_directory(sandbox_dir, staging_name)
        {
            return self.fail_hibernate_after_stop(
                instance,
                format!("{cause}; staging cleanup failed: {error}"),
            );
        }
        let recovery = instance.clone();
        instance.backend_ownership = BackendOwnership::Running;
        if let Err(error) = instance.transition(SandboxState::Running) {
            return self.fail_hibernate_after_stop(
                recovery,
                format!("{cause}; running-state compensation failed: {error}"),
            );
        }
        instance.finish_operation();
        if let Err(error) = self.persist_and_retain(instance) {
            return self.fail_hibernate_after_stop(
                recovery,
                format!("{cause}; running-state compensation commit failed: {error}"),
            );
        }
        BlazeDaemonError::Internal(cause)
    }

    async fn verify_resumed_backend(
        &self,
        id: Uuid,
        backend: &DynBackendInstance,
        expose_guest_socket: bool,
    ) -> Result<()> {
        require_backend_live(id, backend).await?;
        if expose_guest_socket {
            // The captured runtime exposed a guest transport, so a restored
            // owner without one would publish `running` while every guest
            // operation fails; reject it so the replacement is cleaned up.
            if backend.guest_socket_path().as_os_str().is_empty() {
                return Err(BlazeDaemonError::Internal(format!(
                    "instance {id} restored backend exposes no guest socket \
                     but the hibernation image requires one"
                )));
            }
            self.wait_for_guest_ready(backend, "resume-guest-ready")
                .await?;
        }
        require_backend_live(id, backend).await
    }

    async fn abort_resumed_backend(
        &self,
        mut instance: SandboxInstance,
        backend: &DynBackendInstance,
        cause: String,
    ) -> BlazeDaemonError {
        let stopped = match crate::failpoint::backend("resume-backend-stop") {
            Ok(()) => backend.kill().await,
            Err(error) => Err(error),
        };
        if let Err(error) = stopped {
            instance.backend_ownership = BackendOwnership::Unknown;
            return self.fail_resume_with_owner(
                instance,
                format!("{cause}; restored backend termination failed: {error}"),
            );
        }
        self.remove_backend_owner(instance.id);
        instance.backend_ownership = BackendOwnership::Stopped;
        self.fail_resume_without_owner(instance, cause)
    }

    fn fail_resume_without_owner(
        &self,
        mut instance: SandboxInstance,
        cause: String,
    ) -> BlazeDaemonError {
        let recovery = instance.clone();
        instance.backend_ownership = BackendOwnership::Stopped;
        if let Err(error) = instance.transition(SandboxState::Hibernated) {
            return self.fail_resume_with_owner(
                recovery,
                format!("{cause}; hibernated-state compensation failed: {error}"),
            );
        }
        instance.finish_operation();
        if let Err(error) = self.persist_and_retain(instance) {
            return self.fail_resume_with_owner(
                recovery,
                format!("{cause}; hibernated-state compensation commit failed: {error}"),
            );
        }
        BlazeDaemonError::Internal(cause)
    }

    fn fail_hibernate_after_stop(
        &self,
        instance: SandboxInstance,
        cause: String,
    ) -> BlazeDaemonError {
        let id = instance.id;
        let recovery = self.mark_instance_recovery(instance).err();
        BlazeDaemonError::RecoveryRequired(format!(
            "hibernate {id}: {cause}; resources retained{}",
            recovery
                .map(|error| format!("; recovery state persistence failed: {error}"))
                .unwrap_or_default()
        ))
    }

    fn fail_resume_with_owner(&self, instance: SandboxInstance, cause: String) -> BlazeDaemonError {
        let id = instance.id;
        let recovery = self.mark_instance_recovery(instance).err();
        BlazeDaemonError::RecoveryRequired(format!(
            "resume {id}: {cause}; resources retained{}",
            recovery
                .map(|error| format!("; recovery state persistence failed: {error}"))
                .unwrap_or_default()
        ))
    }

    pub(super) async fn cleanup_hibernate_artifacts(&self, id: Uuid) -> Result<()> {
        // Destroy runs after the runtime directory may already be released, so
        // a missing sandbox directory simply means there is nothing to reclaim.
        let sandbox_dir = match self.hibernate_root(id) {
            Ok(sandbox_dir) => sandbox_dir,
            Err(BlazeDaemonError::NotFound(_)) => return Ok(()),
            Err(error) => return Err(error),
        };
        // The recursive scan, stat, unlink, and directory fsync below are
        // synchronous filesystem work whose cost scales with payload size, so
        // it runs on the blocking pool rather than an async worker; concurrent
        // destroys of large payloads must not stall unrelated API requests.
        // This mirrors the checkpoint-store removal the destroy path already
        // offloads just above this call.
        crate::failpoint::spawn_blocking(move || {
            for name in run_dir_names(&sandbox_dir)? {
                if name == HIBERNATE_DIRECTORY
                    || (name.starts_with(".hibernate.")
                        && (name.ends_with(".tmp") || name.ends_with(".bak")))
                {
                    remove_child_directory(&sandbox_dir, &name)?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|error| {
            BlazeDaemonError::Internal(format!("hibernation cleanup blocking task failed: {error}"))
        })?
    }

    /// Borrow the retained sandbox directory that owns hibernation state.
    ///
    /// Every hibernation object is resolved relative to this descriptor, so a
    /// replaced or symlinked instance directory cannot redirect the image.
    fn hibernate_root(&self, id: Uuid) -> Result<OwnedRunDir> {
        self.run_directory(id)
    }
}

/// Name of the published hibernation directory.
fn hibernate_dir_name() -> &'static str {
    HIBERNATE_DIRECTORY
}

/// Name of a private staging directory that only one hibernate call owns.
fn hibernate_staging_name() -> String {
    format!(".hibernate.{}.tmp", Uuid::new_v4())
}

/// Name of the backup that retains the previous image during publication.
fn hibernate_backup_name() -> String {
    format!(".hibernate.{}.bak", Uuid::new_v4())
}

fn hibernate_io_error(
    operation: &'static str,
    path: PathBuf,
    source: std::io::Error,
) -> BlazeDaemonError {
    BlazeDaemonError::Internal(format!("{operation} {}: {source}", path.display()))
}

/// Create one directory below the sandbox directory and return its owner.
fn create_child_directory(parent: &OwnedRunDir, name: &str) -> Result<OwnedHibernateDir> {
    mkdirat(parent.descriptor(), name, HIBERNATE_DIRECTORY_MODE).map_err(|source| {
        hibernate_io_error(
            "create hibernation directory",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )
    })?;
    open_child_directory(parent, name)
}

/// Open one existing directory below the sandbox directory.
fn open_child_directory(parent: &OwnedRunDir, name: &str) -> Result<OwnedHibernateDir> {
    let directory = openat(
        parent.descriptor(),
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| {
        hibernate_io_error(
            "open hibernation directory",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )
    })?;
    Ok(OwnedHibernateDir {
        configured_path: parent.configured_path().join(name),
        directory,
    })
}

/// Open one directory below the sandbox directory when it exists.
fn optional_child_directory(parent: &OwnedRunDir, name: &str) -> Result<Option<OwnedHibernateDir>> {
    match openat(
        parent.descriptor(),
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(directory) => Ok(Some(OwnedHibernateDir {
            configured_path: parent.configured_path().join(name),
            directory,
        })),
        Err(Errno::NOENT) => Ok(None),
        Err(source) => Err(hibernate_io_error(
            "open hibernation directory",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )),
    }
}

/// Owner of one hibernation directory resolved from the sandbox descriptor.
struct OwnedHibernateDir {
    configured_path: PathBuf,
    directory: OwnedFd,
}

impl OwnedHibernateDir {
    fn descriptor(&self) -> &OwnedFd {
        &self.directory
    }

    /// Report the configured pathname for diagnostics only.
    fn configured_path(&self) -> &Path {
        &self.configured_path
    }

    /// Open one regular file below this directory, refusing symbolic links.
    fn open_file(&self, name: &str, operation: &'static str) -> Result<std::fs::File> {
        let descriptor = openat(
            self.descriptor(),
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|source| {
            hibernate_io_error(
                operation,
                self.configured_path().join(name),
                std::io::Error::from(source),
            )
        })?;
        let file = std::fs::File::from(descriptor);
        let metadata = file.metadata().map_err(|source| {
            hibernate_io_error(operation, self.configured_path().join(name), source)
        })?;
        if !metadata.is_file() {
            return Err(BlazeDaemonError::Internal(format!(
                "hibernation object {} is not a regular file",
                self.configured_path().join(name).display()
            )));
        }
        Ok(file)
    }

    /// Create one new regular file below this directory.
    fn create_new_file(&self, name: &str, operation: &'static str) -> Result<std::fs::File> {
        let descriptor = openat(
            self.descriptor(),
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            HIBERNATE_FILE_MODE,
        )
        .map_err(|source| {
            hibernate_io_error(
                operation,
                self.configured_path().join(name),
                std::io::Error::from(source),
            )
        })?;
        Ok(std::fs::File::from(descriptor))
    }

    /// Open one existing directory below this directory.
    fn open_subdirectory(&self, name: &str) -> Result<OwnedHibernateDir> {
        let directory = openat(
            self.descriptor(),
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|source| {
            hibernate_io_error(
                "open hibernation payload directory",
                self.configured_path().join(name),
                std::io::Error::from(source),
            )
        })?;
        Ok(OwnedHibernateDir {
            configured_path: self.configured_path().join(name),
            directory,
        })
    }

    /// Create one new directory below this directory and return its owner.
    fn create_subdirectory(&self, name: &str) -> Result<OwnedHibernateDir> {
        mkdirat(self.descriptor(), name, HIBERNATE_DIRECTORY_MODE).map_err(|source| {
            hibernate_io_error(
                "create hibernation payload directory",
                self.configured_path().join(name),
                std::io::Error::from(source),
            )
        })?;
        self.open_subdirectory(name)
    }

    /// Open the regular file at a slash-separated path below this directory,
    /// walking each segment through directory descriptors so no component
    /// can be a symbolic link.
    fn open_relative_file(&self, rel_path: &str, operation: &'static str) -> Result<std::fs::File> {
        let mut current: Option<OwnedHibernateDir> = None;
        let mut segments = rel_path.split('/').peekable();
        while let Some(segment) = segments.next() {
            let parent = current.as_ref().unwrap_or(self);
            if segments.peek().is_none() {
                return parent.open_file(segment, operation);
            }
            current = Some(parent.open_subdirectory(segment)?);
        }
        Err(BlazeDaemonError::Internal(format!(
            "hibernation artifact path {rel_path:?} is empty"
        )))
    }

    /// Open one entry without following symbolic links, classifying it as a
    /// directory or a regular file and rejecting every other object kind.
    fn open_entry(&self, name: &str) -> Result<HibernateEntry> {
        let path = self.configured_path().join(name);
        let descriptor = openat(
            self.descriptor(),
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|source| {
            hibernate_io_error(
                "open hibernation payload entry",
                path.clone(),
                std::io::Error::from(source),
            )
        })?;
        let stat = fstat(&descriptor).map_err(|source| {
            hibernate_io_error(
                "inspect hibernation payload entry",
                path.clone(),
                std::io::Error::from(source),
            )
        })?;
        match FileType::from_raw_mode(stat.st_mode as _) {
            FileType::Directory => Ok(HibernateEntry::Directory(OwnedHibernateDir {
                configured_path: path,
                directory: descriptor,
            })),
            FileType::RegularFile => Ok(HibernateEntry::File),
            _ => Err(BlazeDaemonError::Internal(format!(
                "hibernation payload entry {} is neither a regular file nor a directory",
                path.display()
            ))),
        }
    }

    /// Report every name this directory currently links.
    fn names(&self) -> Result<BTreeSet<String>> {
        let directory = self.directory.try_clone().map_err(|source| {
            hibernate_io_error(
                "scan hibernation directory",
                self.configured_path().to_path_buf(),
                source,
            )
        })?;
        let mut names = BTreeSet::new();
        for entry in Dir::new(directory).map_err(|source| {
            hibernate_io_error(
                "scan hibernation directory",
                self.configured_path().to_path_buf(),
                std::io::Error::from(source),
            )
        })? {
            let entry = entry.map_err(|source| {
                hibernate_io_error(
                    "scan hibernation directory",
                    self.configured_path().to_path_buf(),
                    std::io::Error::from(source),
                )
            })?;
            let name = entry.file_name().to_str().map_err(|_| {
                BlazeDaemonError::Internal(format!(
                    "hibernation directory {} contains a non-UTF-8 name",
                    self.configured_path().display()
                ))
            })?;
            if name == "." || name == ".." {
                continue;
            }
            names.insert(name.to_string());
        }
        Ok(names)
    }

    /// Enumerate every entry by raw `OsString`, including non-UTF-8 names.
    ///
    /// Publication rejects non-UTF-8 names, but removal must clean up a
    /// directory a backend populated with arbitrary names; rejecting the
    /// scan would strand the sandbox with destroy as the only exit.
    fn names_os(&self) -> Result<Vec<std::ffi::OsString>> {
        let directory = self.directory.try_clone().map_err(|source| {
            hibernate_io_error(
                "scan hibernation directory",
                self.configured_path().to_path_buf(),
                source,
            )
        })?;
        let mut names = Vec::new();
        for entry in Dir::new(directory).map_err(|source| {
            hibernate_io_error(
                "scan hibernation directory",
                self.configured_path().to_path_buf(),
                std::io::Error::from(source),
            )
        })? {
            let entry = entry.map_err(|source| {
                hibernate_io_error(
                    "scan hibernation directory",
                    self.configured_path().to_path_buf(),
                    std::io::Error::from(source),
                )
            })?;
            let name = entry.file_name();
            let name_bytes = name.to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            names.push(std::ffi::OsStr::from_bytes(name_bytes).to_os_string());
        }
        Ok(names)
    }

    fn sync(&self) -> Result<()> {
        fsync(self.descriptor()).map_err(|source| {
            hibernate_io_error(
                "sync hibernation directory",
                self.configured_path().to_path_buf(),
                std::io::Error::from(source),
            )
        })
    }
}

/// Flush one directory so a rename or unlink survives a crash.
fn sync_run_dir(parent: &OwnedRunDir) -> Result<()> {
    fsync(parent.descriptor()).map_err(|source| {
        hibernate_io_error(
            "sync sandbox directory",
            parent.configured_path().to_path_buf(),
            std::io::Error::from(source),
        )
    })
}

/// One hibernation directory entry classified without following links.
enum HibernateEntry {
    Directory(OwnedHibernateDir),
    /// A regular file; consumers reopen it by relative path when needed.
    File,
}

/// Walk one payload subtree, collecting the slash-separated relative path of
/// every regular file plus every directory visited, so callers can hash,
/// sync, or revalidate the exact set the backend wrote.
fn collect_payload_files(
    directory: &OwnedHibernateDir,
    prefix: &str,
    depth: usize,
    files: &mut Vec<String>,
    dirs: &mut Vec<OwnedHibernateDir>,
) -> Result<usize> {
    if depth > MAX_PAYLOAD_DEPTH {
        return Err(BlazeDaemonError::Internal(format!(
            "hibernation payload at {} is nested too deeply",
            directory.configured_path().display()
        )));
    }
    let mut file_count: usize = 0;
    for name in directory.names()? {
        let rel_path = format!("{prefix}/{name}");
        // Reject a path the manifest format cannot carry while the backend
        // is still compensable: hibernation stops the backend only after the
        // manifest is built, and a path that fails validation on load would
        // otherwise trap the sandbox in recovery with destroy as the only
        // exit.
        validate_artifact_path(&rel_path).map_err(|error| {
            BlazeDaemonError::Internal(format!("hibernation payload is not publishable: {error}"))
        })?;
        match directory.open_entry(&name)? {
            HibernateEntry::Directory(child) => {
                let child_files = collect_payload_files(&child, &rel_path, depth + 1, files, dirs)?;
                if child_files == 0 {
                    return Err(BlazeDaemonError::Internal(format!(
                        "hibernation payload directory {rel_path} is empty and cannot be \
                         carried by the manifest inventory"
                    )));
                }
                file_count += child_files;
                dirs.push(child);
            }
            HibernateEntry::File => {
                files.push(rel_path);
                file_count += 1;
            }
        }
    }
    Ok(file_count)
}

/// Remove one hibernation-namespace entry below the sandbox directory and
/// flush the parent.
///
/// The entry is normally a directory, but a misbehaving adapter can escape its
/// configured payload path (for example `payload_dir.join("../../hibernate")`)
/// and leave a plain file under a hibernation name. Destroy must still be able
/// to reclaim it, so a non-directory entry is unlinked rather than opened as a
/// directory — otherwise the terminal transition would be permanently blocked
/// with the sandbox stranded in `RecoveryRequired`.
fn remove_child_directory(parent: &OwnedRunDir, name: &str) -> Result<()> {
    let stat = match statat(parent.descriptor(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        // A missing entry is already reclaimed.
        Err(Errno::NOENT) => return Ok(()),
        Err(source) => {
            return Err(hibernate_io_error(
                "inspect hibernation entry",
                parent.configured_path().join(name),
                std::io::Error::from(source),
            ));
        }
    };
    if FileType::from_raw_mode(stat.st_mode as _) != FileType::Directory {
        // A stray file (or other non-directory) left by an escaped payload
        // write is unlinked directly; opening it as a directory would fail and
        // permanently block the terminal transition.
        unlinkat(parent.descriptor(), name, AtFlags::empty()).map_err(|source| {
            hibernate_io_error(
                "remove stray hibernation entry",
                parent.configured_path().join(name),
                std::io::Error::from(source),
            )
        })?;
        return sync_run_dir(parent);
    }
    let directory = open_child_directory(parent, name)?;
    remove_directory_tree(directory, 0)?;
    unlinkat(parent.descriptor(), name, AtFlags::REMOVEDIR).map_err(|source| {
        hibernate_io_error(
            "remove hibernation directory",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )
    })?;
    sync_run_dir(parent)
}

/// Empty one owned directory, recursing into payload subtrees.
///
/// Hibernation images carry backend-private subtrees, so removal must
/// recurse; the depth bound keeps a corrupted tree from unbounded work.
/// Uses raw `OsString` enumeration so removal can clean up non-UTF-8 names
/// a backend may have created; publication rejects them, but removal must
/// not strand the sandbox.
fn remove_directory_tree(directory: OwnedHibernateDir, depth: usize) -> Result<()> {
    if depth > MAX_REMOVAL_DEPTH {
        return Err(BlazeDaemonError::Internal(format!(
            "hibernation directory {} is nested too deeply to remove",
            directory.configured_path().display()
        )));
    }
    for entry in directory.names_os()? {
        // Classification is deliberately laxer than payload validation:
        // publication rejects symlinks, FIFOs, sockets, and devices, and
        // removal must be able to delete exactly those rejected entries,
        // so anything that is not a directory is simply unlinked.
        let stat = statat(directory.descriptor(), &entry, AtFlags::SYMLINK_NOFOLLOW).map_err(
            |source| {
                hibernate_io_error(
                    "inspect hibernation entry",
                    directory.configured_path().join(&entry),
                    std::io::Error::from(source),
                )
            },
        )?;
        if FileType::from_raw_mode(stat.st_mode as _) == FileType::Directory {
            let child = if let Some(name_str) = entry.to_str() {
                directory.open_subdirectory(name_str)?
            } else {
                let fd = openat(
                    directory.descriptor(),
                    &entry,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|source| {
                    hibernate_io_error(
                        "open hibernation subdirectory with non-UTF-8 name",
                        directory.configured_path().join(&entry),
                        std::io::Error::from(source),
                    )
                })?;
                OwnedHibernateDir {
                    configured_path: directory.configured_path().join(&entry),
                    directory: fd,
                }
            };
            remove_directory_tree(child, depth + 1)?;
            unlinkat(directory.descriptor(), &entry, AtFlags::REMOVEDIR).map_err(|source| {
                hibernate_io_error(
                    "remove hibernation directory",
                    directory.configured_path().join(&entry),
                    std::io::Error::from(source),
                )
            })?;
        } else {
            unlinkat(directory.descriptor(), &entry, AtFlags::empty()).map_err(|source| {
                hibernate_io_error(
                    "remove hibernation file",
                    directory.configured_path().join(&entry),
                    std::io::Error::from(source),
                )
            })?;
        }
    }
    directory.sync()
}

fn require_quiescent_state(instance: &SandboxInstance, expected: SandboxState) -> Result<()> {
    if let Some(journal) = &instance.operation {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "instance {} has unfinished {} operation",
            instance.id, journal.kind
        )));
    }
    if instance.state != expected {
        return Err(BlazeDaemonError::Conflict(format!(
            "instance {} is {}, expected {expected}",
            instance.id, instance.state
        )));
    }
    Ok(())
}

async fn require_backend_live(id: Uuid, backend: &DynBackendInstance) -> Result<()> {
    match backend.try_wait().await {
        Ok(None) => Ok(()),
        Ok(Some(result)) => Err(BlazeDaemonError::RecoveryRequired(format!(
            "instance {id} backend exited (exit={:?}, signal={:?})",
            result.exit_code, result.signal
        ))),
        Err(error) => Err(BlazeDaemonError::RecoveryRequired(format!(
            "instance {id} backend liveness is unknown: {error}"
        ))),
    }
}

/// Pin the configured backend executable for a hibernate or resume operation.
///
/// A backend that needs no executable of its own carries no configured path,
/// and refusing that here would hide the adapter's own answer about whether it
/// supports resume at all, so an empty path stays `None`.
fn pinned_executable(binary_path: &Path) -> Result<Option<Arc<PinnedExecutable>>> {
    if binary_path.as_os_str().is_empty() {
        return Ok(None);
    }
    Ok(Some(Arc::new(PinnedExecutable::open(binary_path)?)))
}

/// Host shape a hibernation freezes so a resume can rebuild it.
///
/// The generic restore transaction probes the owner it is about to replace, but
/// a resume may run after a daemon restart with nothing left to ask, so these
/// answers are taken while the captured runtime is still alive and recorded in
/// the manifest.
#[derive(Debug, Clone, Copy)]
struct CapturedHostShape {
    /// Whether the captured runtime exposed the stable run-directory guest
    /// transport.
    expose_guest_socket: bool,
    /// Whether the captured runtime held a per-sandbox host network slot.
    preserve_network: bool,
    /// Whether the captured runtime recorded guest console output.
    record_console_log: bool,
}

/// Reject a stray hibernation-namespace entry the capture may have created in
/// the sandbox run directory by escaping its configured payload path.
///
/// Only three hibernation names may legitimately exist in the run directory
/// while a capture is in flight: the active staging directory
/// (`staging_name`), a previously published `hibernate` directory, and a
/// `.hibernate.*.bak` backup directory — all directories. A capture that
/// escaped its payload path (for example through
/// `payload_dir.join("../../hibernate")` or `../../.hibernate.extra.tmp`) can
/// instead leave a plain file under one of these names, or an *extra*
/// `.hibernate.*.tmp` staging directory that the next hibernate would read as
/// an unfinished capture. Either strands the sandbox: the first defeats the
/// publication rename and directory-only cleanup, the second forces the next
/// hibernate into `RecoveryRequired`. Rejecting both here, before the backend
/// is stopped, keeps the failure on the compensation path with the original
/// runtime intact.
fn require_publishable_sandbox_root(parent: &OwnedRunDir, staging_name: &str) -> Result<()> {
    for name in run_dir_names(parent)? {
        if name == staging_name {
            continue;
        }
        let is_staging = name.starts_with(".hibernate.") && name.ends_with(".tmp");
        let is_backup = name.starts_with(".hibernate.") && name.ends_with(".bak");
        let is_hibernate_namespace = name == HIBERNATE_DIRECTORY || is_staging || is_backup;
        if !is_hibernate_namespace {
            continue;
        }
        // A second staging directory must never coexist with the active one:
        // the next hibernate would treat it as an unfinished capture.
        if is_staging {
            return Err(BlazeDaemonError::Internal(format!(
                "sandbox directory entry {} is an unexpected hibernation staging directory",
                parent.configured_path().join(&name).display()
            )));
        }
        let stat = statat(
            parent.descriptor(),
            name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|source| {
            hibernate_io_error(
                "inspect sandbox directory entry",
                parent.configured_path().join(&name),
                std::io::Error::from(source),
            )
        })?;
        if FileType::from_raw_mode(stat.st_mode as _) != FileType::Directory {
            return Err(BlazeDaemonError::Internal(format!(
                "sandbox directory entry {} occupies a hibernation name but is not a directory",
                parent.configured_path().join(&name).display()
            )));
        }
    }
    Ok(())
}

/// Require the hibernation staging root to hold exactly the backend subtree.
///
/// Called before the backend is stopped and before the manifest is written, so
/// a capture that wrote anything beside `backend/` is rejected onto the
/// compensation path rather than published as an image that resume would later
/// refuse.
fn require_staging_root_layout(directory: &OwnedHibernateDir) -> Result<()> {
    let staging_names = directory.names()?;
    let expected = [PAYLOAD_BACKEND_DIR.to_string()]
        .into_iter()
        .collect::<BTreeSet<_>>();
    if staging_names != expected {
        return Err(BlazeDaemonError::Internal(format!(
            "hibernation staging root at {} has an unexpected entry set: {staging_names:?}",
            directory.configured_path().display()
        )));
    }
    Ok(())
}

async fn build_hibernate_manifest(
    directory: &OwnedHibernateDir,
    instance: &SandboxInstance,
    backend_version: Option<String>,
    host_shape: CapturedHostShape,
) -> Result<HibernateManifest> {
    // The backend owns its payload layout, so the manifest is built by
    // walking whatever the adapter wrote rather than expecting fixed names.
    // An empty payload means the capture did not happen and must fail here,
    // before the live backend is stopped.
    //
    // Reject anything the adapter wrote beside `backend/` at the staging root
    // as well. The manifest inventories only the backend subtree, so a stray
    // sibling (for example one written through `payload_dir.join("../log")`)
    // would survive publication and only be caught by the resume-side
    // directory check, stranding a sandbox that already hibernated in
    // `RecoveryRequired`. The manifest does not exist yet at this point, so the
    // staging root must contain exactly the backend subtree.
    require_staging_root_layout(directory)?;
    let payload = directory.open_subdirectory(PAYLOAD_BACKEND_DIR)?;
    let mut rel_paths = Vec::new();
    let mut dirs = Vec::new();
    collect_payload_files(&payload, PAYLOAD_BACKEND_DIR, 1, &mut rel_paths, &mut dirs)?;
    rel_paths.sort();
    if rel_paths.is_empty() {
        return Err(BlazeDaemonError::Internal(format!(
            "hibernation payload at {} is empty",
            payload.configured_path().display()
        )));
    }
    let mut artifacts = Vec::with_capacity(rel_paths.len());
    for rel_path in rel_paths {
        let file = directory.open_relative_file(&rel_path, "read hibernation artifact")?;
        // Reject files the daemon does not exclusively own while the backend
        // is still compensable: another hard link would let the "durable"
        // image change after publication without failing its hash check
        // here, only at resume, when the original backend is already gone.
        validate_hibernate_artifact_owner(directory, &rel_path, &file)?;
        artifacts.push(hash_open_artifact(rel_path, file).await?);
    }
    Ok(HibernateManifest {
        format_version: HIBERNATE_FORMAT_VERSION,
        sandbox_id: instance.id,
        policy_name: instance.policy_name.clone(),
        image_digest: instance.image_digest.clone(),
        backend: instance.backend,
        backend_version,
        snapshot_kind: SnapshotKind::Full,
        expose_guest_socket: host_shape.expose_guest_socket,
        preserve_network: host_shape.preserve_network,
        record_console_log: host_shape.record_console_log,
        artifacts,
    })
}

async fn write_and_sync_manifest(
    directory: &OwnedHibernateDir,
    manifest: &HibernateManifest,
) -> Result<()> {
    // Every payload file and directory becomes durable before the manifest
    // that describes them, so a crash never publishes an inventory of bytes
    // that were still in flight.
    for artifact in &manifest.artifacts {
        let file = directory.open_relative_file(&artifact.name, "sync hibernation artifact")?;
        crate::failpoint::spawn_blocking(move || file.sync_all())
            .await
            .map_err(|error| {
                BlazeDaemonError::Internal(format!(
                    "hibernation artifact sync task failed: {error}"
                ))
            })??;
    }
    let payload = directory.open_subdirectory(PAYLOAD_BACKEND_DIR)?;
    let mut rel_paths = Vec::new();
    let mut dirs = Vec::new();
    collect_payload_files(&payload, PAYLOAD_BACKEND_DIR, 1, &mut rel_paths, &mut dirs)?;
    for nested in &dirs {
        nested.sync()?;
    }
    payload.sync()?;
    let mut encoded = serde_json::to_vec_pretty(manifest)?;
    encoded.push(b'\n');
    let file = directory.create_new_file(MANIFEST_ARTIFACT, "publish hibernation manifest")?;
    crate::failpoint::spawn_blocking(move || {
        use std::io::Write;

        let mut file = file;
        file.write_all(&encoded)?;
        file.sync_all()
    })
    .await
    .map_err(|error| {
        BlazeDaemonError::Internal(format!("hibernation manifest write task failed: {error}"))
    })??;
    directory.sync()
}

async fn load_and_verify_manifest(
    directory: &OwnedHibernateDir,
) -> Result<(HibernateManifest, Vec<(String, std::fs::File)>)> {
    let manifest_file = directory.open_file(MANIFEST_ARTIFACT, "read hibernation manifest")?;
    let encoded = crate::failpoint::spawn_blocking(move || {
        use std::io::Read;

        let mut manifest_file = manifest_file;
        let mut encoded = Vec::new();
        manifest_file.read_to_end(&mut encoded).map(|_| encoded)
    })
    .await
    .map_err(|error| {
        BlazeDaemonError::Internal(format!("hibernation manifest read task failed: {error}"))
    })??;
    let manifest: HibernateManifest = serde_json::from_slice(&encoded)?;
    if manifest.format_version != HIBERNATE_FORMAT_VERSION {
        return Err(BlazeDaemonError::UnsupportedOperation(format!(
            "unsupported hibernation format {}",
            manifest.format_version
        )));
    }
    if manifest.snapshot_kind != SnapshotKind::Full {
        return Err(BlazeDaemonError::UnsupportedOperation(
            "hibernation image is not self-contained".to_string(),
        ));
    }
    // The manifest is the sole inventory of the backend-private payload:
    // its paths must be safe to resolve, canonically ordered, and account
    // for every file the payload subtree actually holds.
    if manifest.artifacts.is_empty() {
        return Err(BlazeDaemonError::Internal(
            "hibernation manifest has an empty artifact set".to_string(),
        ));
    }
    let mut previous: Option<&str> = None;
    for artifact in &manifest.artifacts {
        validate_artifact_path(&artifact.name).map_err(|error| {
            BlazeDaemonError::Internal(format!(
                "hibernation manifest has an invalid artifact set: {error}"
            ))
        })?;
        if artifact
            .name
            .strip_prefix(PAYLOAD_BACKEND_DIR)
            .is_none_or(|rest| !rest.starts_with('/'))
        {
            return Err(BlazeDaemonError::Internal(format!(
                "hibernation artifact {} is outside the payload subtree",
                artifact.name
            )));
        }
        if let Some(previous) = previous
            && previous >= artifact.name.as_str()
        {
            return Err(BlazeDaemonError::Internal(
                "hibernation manifest has an invalid artifact set".to_string(),
            ));
        }
        previous = Some(artifact.name.as_str());
    }
    let expected_directory_names = [
        MANIFEST_ARTIFACT.to_string(),
        PAYLOAD_BACKEND_DIR.to_string(),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    if directory.names()? != expected_directory_names {
        return Err(BlazeDaemonError::Internal(
            "hibernation directory has an unexpected file set".to_string(),
        ));
    }
    let payload = directory.open_subdirectory(PAYLOAD_BACKEND_DIR)?;
    let mut observed = Vec::new();
    let mut dirs = Vec::new();
    collect_payload_files(&payload, PAYLOAD_BACKEND_DIR, 1, &mut observed, &mut dirs)?;
    observed.sort();
    let recorded: Vec<&str> = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.name.as_str())
        .collect();
    if observed
        .iter()
        .map(String::as_str)
        .ne(recorded.iter().copied())
    {
        return Err(BlazeDaemonError::Internal(format!(
            "hibernation payload does not match its manifest: found {observed:?}, recorded {recorded:?}"
        )));
    }
    // Hash every artifact through a descriptor that stays retained: resume
    // hands the payload to the adapter by pathname, so these descriptors are
    // what the pre-publication identity revalidation compares against.
    let mut retained = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        let file = directory.open_relative_file(&artifact.name, "read hibernation artifact")?;
        let hashing = file.try_clone().map_err(|source| {
            hibernate_io_error(
                "clone hibernation artifact descriptor",
                directory.configured_path().join(&artifact.name),
                source,
            )
        })?;
        let observed = hash_open_artifact(artifact.name.clone(), hashing).await?;
        if &observed != artifact {
            return Err(BlazeDaemonError::Internal(format!(
                "hibernate artifact {} failed integrity verification",
                artifact.name
            )));
        }
        retained.push((artifact.name.clone(), file));
    }
    Ok((manifest, retained))
}

fn validate_manifest_identity(
    manifest: &HibernateManifest,
    instance: &SandboxInstance,
) -> Result<()> {
    if manifest.sandbox_id != instance.id
        || manifest.policy_name != instance.policy_name
        || manifest.image_digest != instance.image_digest
        || manifest.backend != instance.backend
    {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "instance {} hibernation identity does not match durable lifecycle state",
            instance.id
        )));
    }
    Ok(())
}

/// Hash one artifact through an already opened descriptor.
///
/// Hashing reads the whole artifact, so it runs on the blocking pool instead of
/// occupying an async worker.
async fn hash_open_artifact(name: String, file: std::fs::File) -> Result<CheckpointArtifact> {
    let diagnostic_name = name.clone();
    crate::failpoint::spawn_blocking(move || {
        use std::io::Read;

        let mut file = file;
        let mut hasher = Sha256::new();
        let mut size_bytes = 0_u64;
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            size_bytes = size_bytes.checked_add(read as u64).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "artifact size overflow")
            })?;
            hasher.update(&buffer[..read]);
        }
        Ok(CheckpointArtifact {
            name,
            size_bytes,
            sha256: format!("{:x}", hasher.finalize()),
        })
    })
    .await
    .map_err(|error| {
        BlazeDaemonError::Internal(format!("hibernation artifact hash task failed: {error}"))
    })?
    .map_err(|source: std::io::Error| {
        BlazeDaemonError::Internal(format!(
            "hash hibernation artifact {diagnostic_name}: {source}"
        ))
    })
}

/// Reject a payload file the daemon does not exclusively own.
fn validate_hibernate_artifact_owner(
    directory: &OwnedHibernateDir,
    rel_path: &str,
    file: &std::fs::File,
) -> Result<()> {
    let stat = fstat(file).map_err(|source| {
        hibernate_io_error(
            "inspect hibernation artifact",
            directory.configured_path().join(rel_path),
            std::io::Error::from(source),
        )
    })?;
    let expected_uid = unsafe { libc::geteuid() };
    if stat.st_uid != expected_uid {
        return Err(BlazeDaemonError::Internal(format!(
            "hibernation artifact {} is not owned by the daemon user",
            directory.configured_path().join(rel_path).display()
        )));
    }
    if stat.st_nlink != 1 {
        return Err(BlazeDaemonError::Internal(format!(
            "hibernation artifact {} must have exactly one hard link",
            directory.configured_path().join(rel_path).display()
        )));
    }
    Ok(())
}

/// Revalidate that every retained artifact descriptor is still the object
/// linked at its manifest path, that the payload holds nothing else, and
/// that the hibernate directory itself has not been renamed away.
///
/// Resume consumes the payload by pathname, so this runs after the restore
/// adapter finished reading and before the sandbox publishes `running`; a
/// payload entry replaced in between fails here and the replacement backend
/// is torn down instead of running from unverified bytes. The parent
/// linkage check proves the hibernate directory was not swapped wholesale
/// between manifest verification and this post-consume revalidation.
fn require_payload_identity(
    parent: &OwnedRunDir,
    directory: &OwnedHibernateDir,
    retained: &[(String, std::fs::File)],
) -> Result<()> {
    // Prove the hibernate directory is still the same object the parent
    // links at "hibernate": an attacker with write access to the run dir
    // could rename the verified directory away and substitute a different
    // one, so we re-open through the parent and compare dev/ino.
    let linkage = openat(
        parent.descriptor(),
        hibernate_dir_name(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| {
        hibernate_io_error(
            "re-link hibernation directory",
            parent.configured_path().join(hibernate_dir_name()),
            std::io::Error::from(source),
        )
    })?;
    let linkage_stat = fstat(&linkage).map_err(|source| {
        hibernate_io_error(
            "inspect hibernation directory linkage",
            parent.configured_path().join(hibernate_dir_name()),
            std::io::Error::from(source),
        )
    })?;
    let directory_stat = fstat(directory.descriptor()).map_err(|source| {
        hibernate_io_error(
            "inspect hibernation directory",
            directory.configured_path().to_path_buf(),
            std::io::Error::from(source),
        )
    })?;
    if linkage_stat.st_dev != directory_stat.st_dev || linkage_stat.st_ino != directory_stat.st_ino
    {
        return Err(BlazeDaemonError::Internal(
            "hibernation directory was replaced while it was being consumed".to_string(),
        ));
    }
    let payload = directory.open_subdirectory(PAYLOAD_BACKEND_DIR)?;
    let mut observed = Vec::new();
    let mut dirs = Vec::new();
    collect_payload_files(&payload, PAYLOAD_BACKEND_DIR, 1, &mut observed, &mut dirs)?;
    observed.sort();
    if observed
        .iter()
        .map(String::as_str)
        .ne(retained.iter().map(|(rel_path, _)| rel_path.as_str()))
    {
        return Err(BlazeDaemonError::Internal(
            "hibernation payload changed while it was being consumed".to_string(),
        ));
    }
    for (rel_path, expected) in retained {
        let current = directory.open_relative_file(rel_path, "revalidate hibernation artifact")?;
        let expected_stat = fstat(expected).map_err(|source| {
            hibernate_io_error(
                "inspect hibernation artifact",
                directory.configured_path().join(rel_path),
                std::io::Error::from(source),
            )
        })?;
        let current_stat = fstat(&current).map_err(|source| {
            hibernate_io_error(
                "inspect hibernation artifact",
                directory.configured_path().join(rel_path),
                std::io::Error::from(source),
            )
        })?;
        if expected_stat.st_dev != current_stat.st_dev
            || expected_stat.st_ino != current_stat.st_ino
        {
            return Err(BlazeDaemonError::Internal(format!(
                "hibernation artifact {} changed identity while it was being consumed",
                directory.configured_path().join(rel_path).display()
            )));
        }
    }
    Ok(())
}

/// Reject unfinished scratch and release obsolete backups before publication.
fn prepare_hibernate_directory(parent: &OwnedRunDir) -> Result<()> {
    let published = optional_child_directory(parent, hibernate_dir_name())?.is_some();
    let mut obsolete_backups = Vec::new();
    for name in run_dir_names(parent)? {
        if name.starts_with(".hibernate.") && name.ends_with(".tmp") {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "instance directory {} contains unfinished hibernation artifacts",
                parent.configured_path().display()
            )));
        }
        if name.starts_with(".hibernate.") && name.ends_with(".bak") {
            if !published {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance directory {} contains an unpaired hibernation backup",
                    parent.configured_path().display()
                )));
            }
            obsolete_backups.push(name);
        }
    }
    for backup in obsolete_backups {
        remove_child_directory(parent, &backup)?;
    }
    Ok(())
}

/// Report every name the sandbox directory currently links that is valid
/// UTF-8.
///
/// Both callers only match the ASCII `.hibernate.` namespace, so a name that
/// is not valid UTF-8 cannot be a hibernation artifact and is skipped rather
/// than failing the scan. A backend is free to leave arbitrary bytes in the
/// run directory, and destroy calls this during cleanup for every sandbox —
/// even ones that never hibernated — so a stray non-UTF-8 name must not block
/// the lifecycle's terminal transition and strand the sandbox in
/// `RecoveryRequired` with storage still held.
fn run_dir_names(parent: &OwnedRunDir) -> Result<BTreeSet<String>> {
    let directory = parent.descriptor().try_clone().map_err(|source| {
        hibernate_io_error(
            "scan sandbox directory",
            parent.configured_path().to_path_buf(),
            source,
        )
    })?;
    let mut names = BTreeSet::new();
    for entry in Dir::new(directory).map_err(|source| {
        hibernate_io_error(
            "scan sandbox directory",
            parent.configured_path().to_path_buf(),
            std::io::Error::from(source),
        )
    })? {
        let entry = entry.map_err(|source| {
            hibernate_io_error(
                "scan sandbox directory",
                parent.configured_path().to_path_buf(),
                std::io::Error::from(source),
            )
        })?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        names.insert(name.to_string());
    }
    Ok(names)
}

fn with_recovery_error(
    error: BlazeDaemonError,
    recovery: Option<BlazeDaemonError>,
) -> BlazeDaemonError {
    match recovery {
        Some(recovery) => BlazeDaemonError::RecoveryRequired(format!(
            "{error}; recovery state persistence failed: {recovery}"
        )),
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::state_store::OwnedRunDir;

    fn staging_root(temp: &std::path::Path) -> OwnedHibernateDir {
        let run_dir = OwnedRunDir::for_test(Uuid::new_v4(), temp.join("run"));
        create_child_directory(&run_dir, &hibernate_staging_name()).expect("staging dir")
    }

    #[test]
    fn staging_root_layout_accepts_only_the_backend_subtree() {
        let temp = tempfile::tempdir().expect("temp");
        let staging = staging_root(temp.path());
        staging
            .create_subdirectory(PAYLOAD_BACKEND_DIR)
            .expect("backend subtree");

        require_staging_root_layout(&staging).expect("a lone backend subtree is the valid layout");
    }

    #[test]
    fn staging_root_layout_rejects_a_sibling_beside_the_backend_subtree() {
        let temp = tempfile::tempdir().expect("temp");
        let staging = staging_root(temp.path());
        staging
            .create_subdirectory(PAYLOAD_BACKEND_DIR)
            .expect("backend subtree");
        // A snapshot adapter that writes outside its payload directory, e.g.
        // through `payload_dir.join("../capture.log")`, lands a sibling at the
        // staging root. Publication must refuse it while the backend is still
        // compensable rather than leave resume to reject the published image.
        staging
            .create_new_file("capture.log", "write stray sibling")
            .expect("stray sibling");

        let error = require_staging_root_layout(&staging)
            .expect_err("a stray staging-root entry must be rejected before the backend stops");
        assert!(
            matches!(error, BlazeDaemonError::Internal(message) if message.contains("unexpected entry set")),
            "unexpected error variant"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn run_dir_names_skips_non_utf8_entries_instead_of_failing() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().expect("temp");
        let run_dir = OwnedRunDir::for_test(Uuid::new_v4(), temp.path().join("run"));
        // A backend may leave a valid Unix filename whose bytes are not UTF-8.
        // Destroy scans this directory for every sandbox, so the scan must skip
        // such a name rather than fail and strand the sandbox in recovery. Only
        // Linux filesystems accept these bytes as a name; macOS rejects them,
        // so this is gated to Linux where the scenario can actually occur.
        let non_utf8 = std::ffi::OsStr::from_bytes(b"backend-\xff\xfe.log");
        std::fs::write(run_dir.path().join(non_utf8), b"opaque").expect("write non-utf8 entry");
        std::fs::create_dir(run_dir.path().join(HIBERNATE_DIRECTORY)).expect("hibernate dir");

        let names = run_dir_names(&run_dir).expect("scan must not fail on a non-UTF-8 name");
        assert!(
            names.contains(HIBERNATE_DIRECTORY),
            "the ASCII hibernation entry must still be reported"
        );
        assert_eq!(
            names.len(),
            1,
            "only the UTF-8 hibernation entry should be reported"
        );
    }

    #[test]
    fn publishable_sandbox_root_rejects_a_file_under_a_hibernation_name() {
        let temp = tempfile::tempdir().expect("temp");
        let run_dir = OwnedRunDir::for_test(Uuid::new_v4(), temp.path().join("run"));
        let staging_name = hibernate_staging_name();
        // The active staging directory is a legitimate hibernation-namespace
        // directory and must be tolerated.
        create_child_directory(&run_dir, &staging_name).expect("staging dir");
        // A capture that escaped its payload path through `../../hibernate`
        // lands a plain file at the publication target. It must be refused
        // before the backend stops rather than break the later rename.
        std::fs::write(run_dir.path().join(HIBERNATE_DIRECTORY), b"escaped").expect("stray file");

        let error = require_publishable_sandbox_root(&run_dir, &staging_name)
            .expect_err("a non-directory hibernation entry must be rejected before the stop");
        assert!(
            matches!(error, BlazeDaemonError::Internal(message) if message.contains("not a directory")),
            "unexpected error variant"
        );
    }

    #[test]
    fn publishable_sandbox_root_accepts_directories_and_the_active_staging_name() {
        let temp = tempfile::tempdir().expect("temp");
        let run_dir = OwnedRunDir::for_test(Uuid::new_v4(), temp.path().join("run"));
        let staging_name = hibernate_staging_name();
        create_child_directory(&run_dir, &staging_name).expect("staging dir");
        // A previously published image is a directory and stays valid.
        std::fs::create_dir(run_dir.path().join(HIBERNATE_DIRECTORY)).expect("published dir");

        require_publishable_sandbox_root(&run_dir, &staging_name)
            .expect("directory entries under hibernation names are publishable");
    }

    #[test]
    fn publishable_sandbox_root_rejects_an_extra_staging_directory() {
        let temp = tempfile::tempdir().expect("temp");
        let run_dir = OwnedRunDir::for_test(Uuid::new_v4(), temp.path().join("run"));
        let staging_name = hibernate_staging_name();
        create_child_directory(&run_dir, &staging_name).expect("active staging dir");
        // A capture that escaped through `../../.hibernate.extra.tmp` creates a
        // second staging directory. It is a directory, so the non-directory
        // check alone would accept it, but the next hibernate would read it as
        // an unfinished capture — reject it before the backend stops.
        let extra = hibernate_staging_name();
        assert_ne!(extra, staging_name);
        create_child_directory(&run_dir, &extra).expect("extra staging dir");

        let error = require_publishable_sandbox_root(&run_dir, &staging_name)
            .expect_err("a second staging directory must be rejected before the stop");
        assert!(
            matches!(error, BlazeDaemonError::Internal(message) if message.contains("staging directory")),
            "unexpected error variant"
        );
    }

    #[test]
    fn remove_child_directory_reclaims_a_stray_file() {
        let temp = tempfile::tempdir().expect("temp");
        let run_dir = OwnedRunDir::for_test(Uuid::new_v4(), temp.path().join("run"));
        // Destroy must be able to reclaim a plain file left under a hibernation
        // name by an escaped payload write, or the terminal transition would be
        // permanently blocked.
        std::fs::write(run_dir.path().join(HIBERNATE_DIRECTORY), b"escaped").expect("stray file");

        remove_child_directory(&run_dir, HIBERNATE_DIRECTORY)
            .expect("a stray file under a hibernation name must be removable");
        assert!(
            !run_dir.path().join(HIBERNATE_DIRECTORY).exists(),
            "the stray file should be gone"
        );
        // A second call is a no-op once the entry is gone.
        remove_child_directory(&run_dir, HIBERNATE_DIRECTORY)
            .expect("removing an absent entry is a no-op");
    }
}
