// SPDX-License-Identifier: Apache-2.0
//! Periodic persistence of already-written provider-owned sandbox artifacts.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blaze_core::lifecycle::{BackendOwnership, SandboxState};
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[cfg(test)]
use tokio::sync::Notify;

use crate::error::{BlazeDaemonError, Result};

use super::manager::SandboxManager;

/// Supervised periodic storage-artifact synchronization task.
pub(crate) struct StorageSyncLoop {
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
    attempts: AttemptSupervisor,
    #[cfg(test)]
    started: Arc<Notify>,
}

impl StorageSyncLoop {
    /// Wait for an early worker exit and report it as a daemon failure.
    pub(crate) async fn observe_exit(&mut self) -> Result<()> {
        self.join().await?;
        Err(BlazeDaemonError::Internal(
            "storage artifact synchronization task exited unexpectedly".to_string(),
        ))
    }

    /// Request cooperative shutdown and join the worker before returning.
    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        self.cancellation.cancel();
        let worker_result = self.join().await;
        self.attempts.join_all().await;
        worker_result
    }

    async fn join(&mut self) -> Result<()> {
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        let result = task.await;
        self.task.take();
        result.map_err(join_error)
    }

    #[cfg(test)]
    async fn wait_started(&self) {
        self.started.notified().await;
    }
}

impl Drop for StorageSyncLoop {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

fn join_error(error: tokio::task::JoinError) -> BlazeDaemonError {
    BlazeDaemonError::Internal(format!(
        "storage artifact synchronization task join failed: {error}"
    ))
}

/// Counters emitted for one storage-artifact synchronization sweep.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StorageSyncSummary {
    /// Running records selected before any provider await.
    pub(crate) selected: usize,
    /// Provider calls that completed successfully.
    pub(crate) synced: usize,
    /// Records that are no longer Running when artifact sync owns their lock.
    pub(crate) skipped: usize,
    /// Records deferred behind another sandbox operation or occupied sync capacity.
    pub(crate) deferred: usize,
    /// Inconsistent Running records, invalid owners, and provider failures.
    pub(crate) failed: usize,
}

enum StorageSyncAttempt {
    Synced,
    Skipped,
    Deferred,
    Cancelled,
}

struct StorageSyncOwnership {
    id: Uuid,
    inflight: Arc<Mutex<HashSet<Uuid>>>,
    _permit: OwnedSemaphorePermit,
}

impl StorageSyncOwnership {
    fn begin(
        id: Uuid,
        inflight: Arc<Mutex<HashSet<Uuid>>>,
        permit: OwnedSemaphorePermit,
    ) -> Result<Option<Self>> {
        let mut active = inflight.lock().map_err(|_| {
            BlazeDaemonError::Internal(
                "storage artifact synchronization claims poisoned".to_string(),
            )
        })?;
        if !active.insert(id) {
            return Ok(None);
        }
        drop(active);
        Ok(Some(Self {
            id,
            inflight,
            _permit: permit,
        }))
    }
}

impl Drop for StorageSyncOwnership {
    fn drop(&mut self) {
        match self.inflight.lock() {
            Ok(mut active) => {
                active.remove(&self.id);
            }
            Err(poisoned) => {
                poisoned.into_inner().remove(&self.id);
            }
        }
    }
}

#[derive(Clone, Default)]
struct AttemptSupervisor {
    tasks: Arc<Mutex<Vec<LateAttempt>>>,
}

struct LateAttempt {
    id: Uuid,
    reason: &'static str,
    task: JoinHandle<Result<StorageSyncAttempt>>,
}

impl AttemptSupervisor {
    fn register(
        &self,
        id: Uuid,
        reason: &'static str,
        task: JoinHandle<Result<StorageSyncAttempt>>,
    ) {
        self.with_tasks(|tasks| tasks.push(LateAttempt { id, reason, task }));
    }

    async fn reap_finished(&self) {
        let finished = self.with_tasks(|tasks| {
            let mut finished = Vec::new();
            let mut pending = Vec::with_capacity(tasks.len());
            for attempt in tasks.drain(..) {
                if attempt.task.is_finished() {
                    finished.push(attempt);
                } else {
                    pending.push(attempt);
                }
            }
            *tasks = pending;
            finished
        });
        join_late_attempts(finished).await;
    }

    async fn join_all(&self) {
        let attempts = self.with_tasks(std::mem::take);
        join_late_attempts(attempts).await;
    }

    fn with_tasks<T>(&self, operation: impl FnOnce(&mut Vec<LateAttempt>) -> T) -> T {
        match self.tasks.lock() {
            Ok(mut tasks) => operation(&mut tasks),
            Err(poisoned) => operation(&mut poisoned.into_inner()),
        }
    }
}

async fn join_late_attempts(attempts: Vec<LateAttempt>) {
    for LateAttempt { id, reason, task } in attempts {
        match task.await {
            Ok(Ok(StorageSyncAttempt::Synced)) => {
                tracing::info!(sandbox_id = %id, %reason, "late storage artifact synchronization completed");
            }
            Ok(Ok(
                StorageSyncAttempt::Skipped
                | StorageSyncAttempt::Deferred
                | StorageSyncAttempt::Cancelled,
            )) => {
                tracing::warn!(sandbox_id = %id, %reason, "late storage artifact synchronization ended before artifact sync completed");
            }
            Ok(Err(error)) => {
                tracing::warn!(sandbox_id = %id, %reason, %error, "late storage artifact synchronization failed");
            }
            Err(error) => {
                tracing::warn!(sandbox_id = %id, %reason, %error, "late storage artifact synchronization task failed");
            }
        }
    }
}

impl SandboxManager {
    /// Start a cancellable periodic storage-artifact synchronization worker.
    ///
    /// The first sweep starts after one complete interval. Missed ticks are
    /// skipped instead of being queued behind a slow sweep.
    pub(crate) fn start_storage_sync_loop(
        self: &Arc<Self>,
        interval: Duration,
        attempt_timeout: Duration,
    ) -> StorageSyncLoop {
        let manager = self.clone();
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let attempts = AttemptSupervisor::default();
        let worker_attempts = attempts.clone();
        #[cfg(test)]
        let started = Arc::new(Notify::new());
        #[cfg(test)]
        let worker_started = started.clone();
        tracing::info!(
            interval_secs = interval.as_secs_f64(),
            attempt_timeout_secs = attempt_timeout.as_secs_f64(),
            "starting storage artifact synchronization task"
        );
        let task = tokio::spawn(async move {
            let first_tick = Instant::now() + interval;
            let mut ticker = tokio::time::interval_at(first_tick, interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            #[cfg(test)]
            worker_started.notify_one();
            loop {
                worker_attempts.reap_finished().await;
                tokio::select! {
                    biased;
                    _ = worker_cancellation.cancelled() => break,
                    _ = ticker.tick() => {
                        let summary = manager
                            .sync_all_artifacts_until(
                                &worker_cancellation,
                                attempt_timeout,
                                &worker_attempts,
                            )
                            .await;
                        let Some(summary) = summary else {
                            break;
                        };
                        tracing::debug!(
                            selected = summary.selected,
                            synced = summary.synced,
                            skipped = summary.skipped,
                            deferred = summary.deferred,
                            failed = summary.failed,
                            "storage artifact synchronization sweep completed"
                        );
                    }
                }
            }
            tracing::info!("storage artifact synchronization task stopped");
        });
        StorageSyncLoop {
            cancellation,
            task: Some(task),
            attempts,
            #[cfg(test)]
            started,
        }
    }

    #[cfg(test)]
    async fn sync_all_artifacts(self: &Arc<Self>, attempt_timeout: Duration) -> StorageSyncSummary {
        self.sync_all_artifacts_until(
            &CancellationToken::new(),
            attempt_timeout,
            &AttemptSupervisor::default(),
        )
        .await
        .expect("uncancelled sweep")
    }

    async fn sync_all_artifacts_until(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
        attempt_timeout: Duration,
        attempts: &AttemptSupervisor,
    ) -> Option<StorageSyncSummary> {
        let running_ids = match self.list() {
            Ok(instances) => instances
                .into_iter()
                .filter_map(|instance| {
                    (instance.state == SandboxState::Running).then_some(instance.id)
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                tracing::error!(%error, "cannot select storage artifact synchronization candidates");
                return Some(StorageSyncSummary {
                    failed: 1,
                    ..StorageSyncSummary::default()
                });
            }
        };
        let mut summary = StorageSyncSummary {
            selected: running_ids.len(),
            ..StorageSyncSummary::default()
        };
        for id in running_ids {
            match self
                .sync_artifacts_if_running(id, cancellation, attempt_timeout, attempts)
                .await
            {
                Ok(StorageSyncAttempt::Synced) => summary.synced += 1,
                Ok(StorageSyncAttempt::Skipped) => summary.skipped += 1,
                Ok(StorageSyncAttempt::Deferred) => summary.deferred += 1,
                Ok(StorageSyncAttempt::Cancelled) => return None,
                Err(error) => {
                    summary.failed += 1;
                    tracing::warn!(
                        sandbox_id = %id,
                        %error,
                        "sandbox storage artifact synchronization failed"
                    );
                }
            }
        }
        Some(summary)
    }

    async fn sync_artifacts_if_running(
        self: &Arc<Self>,
        id: Uuid,
        cancellation: &CancellationToken,
        attempt_timeout: Duration,
        attempts: &AttemptSupervisor,
    ) -> Result<StorageSyncAttempt> {
        let already_inflight = self
            .storage_sync_inflight
            .lock()
            .map_err(|_| {
                BlazeDaemonError::Internal(
                    "storage artifact synchronization claims poisoned".to_string(),
                )
            })?
            .contains(&id);
        if already_inflight {
            return Ok(StorageSyncAttempt::Deferred);
        }
        if cancellation.is_cancelled() {
            return Ok(StorageSyncAttempt::Cancelled);
        }
        let observed = self.get(id)?;
        if observed.state != SandboxState::Running {
            return Ok(StorageSyncAttempt::Skipped);
        }
        let operation_lock = self.operation_lock(id);
        let _operation = match operation_lock.try_lock_owned() {
            Ok(operation) => operation,
            Err(_) => return Ok(StorageSyncAttempt::Deferred),
        };
        let instance = self.get(id)?;
        if instance.state != SandboxState::Running {
            return Ok(StorageSyncAttempt::Skipped);
        }
        if let Some(operation) = instance.operation {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "instance {id} is Running with unfinished {:?} operation",
                operation.kind
            )));
        }
        if instance.backend_ownership != BackendOwnership::Running {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "instance {id} is Running with {} backend ownership",
                format!("{:?}", instance.backend_ownership).to_lowercase()
            )));
        }
        let backend = self.backend_owner(id).ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(format!(
                "instance {id} is Running without a backend owner"
            ))
        })?;
        let permit = match self.storage_sync_permits.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return Ok(StorageSyncAttempt::Deferred),
        };
        let Some(ownership) =
            StorageSyncOwnership::begin(id, self.storage_sync_inflight.clone(), permit)?
        else {
            return Ok(StorageSyncAttempt::Deferred);
        };
        let manager = self.clone();
        let mut attempt = tokio::spawn(async move {
            let _operation = _operation;
            let _ownership = ownership;
            if let Some(status) = backend.try_wait().await? {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} backend already exited: {status:?}"
                )));
            }
            let storage = manager.reconstruct_storage(id).await.map_err(|error| {
                BlazeDaemonError::RecoveryRequired(format!(
                    "instance {id} has no complete storage owner: {error}"
                ))
            })?;
            manager.sync_storage(&storage).await?;
            Ok(StorageSyncAttempt::Synced)
        });
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                attempts.register(id, "worker cancellation", attempt);
                Ok(StorageSyncAttempt::Cancelled)
            },
            result = tokio::time::timeout(attempt_timeout, &mut attempt) => {
                match result {
                    Ok(result) => result.map_err(join_error)?,
                    Err(_) => {
                        attempts.register(id, "attempt timeout", attempt);
                        Err(BlazeDaemonError::Internal(format!(
                            "storage artifact synchronization attempt for {id} timed out after {:.3} seconds",
                            attempt_timeout.as_secs_f64()
                        )))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use blaze_core::backend::{BackendKind, SpawnRequest};
    use blaze_core::config::TemplateSection;
    use blaze_core::error::{BlazeError, Result as CoreResult};
    use blaze_core::lifecycle::{BackendOwnership, OperationKind, SandboxInstance, SandboxState};
    use blaze_core::policy::{BackendConfigs, WorkloadClass};
    use blaze_core::storage::{
        AcquireOpts, PoolStatus, StorageAcquireError, StorageProvider, StorageSlot,
    };
    use tokio::sync::Notify;

    use crate::file_provider::{ArtifactSyncOpenHook, FileStorageProvider};
    use crate::sandbox::manager::{SandboxManagerInit, SandboxManagerResources};
    use crate::sandbox::template::TemplateCatalog;
    use crate::spawner::{
        BackendInstance, BackendSpawnRequest, MockSpawner, SpawnResult, SpawnerRegistry,
        spawn_with_runtime_directory,
    };
    use crate::state_store::{OwnedRunDir, StateStore};

    use super::*;

    struct RecordingStorage {
        inner: FileStorageProvider,
        instances: PathBuf,
        calls: Mutex<Vec<String>>,
        call_recorded: Notify,
        failures: Mutex<HashSet<String>>,
        block_next: AtomicBool,
        started: Notify,
        release_blocked: Notify,
        blocked_reconstructs: Mutex<HashSet<String>>,
        reconstruct_started: Notify,
        release_reconstruct: Notify,
    }

    struct BlockingBackend {
        block_next: AtomicBool,
        started: Notify,
        release: Notify,
    }

    impl BlockingBackend {
        fn new() -> Self {
            Self {
                block_next: AtomicBool::new(true),
                started: Notify::new(),
                release: Notify::new(),
            }
        }

        fn resume(&self) {
            self.release.notify_one();
        }
    }

    #[async_trait]
    impl BackendInstance for BlockingBackend {
        fn backend(&self) -> BackendKind {
            BackendKind::Mock
        }

        async fn try_wait(&self) -> CoreResult<Option<SpawnResult>> {
            if self.block_next.swap(false, Ordering::AcqRel) {
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(None)
        }

        async fn kill(&self) -> CoreResult<()> {
            Ok(())
        }
    }

    impl RecordingStorage {
        fn new(images: PathBuf, instances: PathBuf) -> Self {
            Self {
                inner: FileStorageProvider::with_images(images, instances.clone()),
                instances,
                calls: Mutex::new(Vec::new()),
                call_recorded: Notify::new(),
                failures: Mutex::new(HashSet::new()),
                block_next: AtomicBool::new(false),
                started: Notify::new(),
                release_blocked: Notify::new(),
                blocked_reconstructs: Mutex::new(HashSet::new()),
                reconstruct_started: Notify::new(),
                release_reconstruct: Notify::new(),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls").clone()
        }

        async fn wait_for_calls(&self, expected: usize) {
            loop {
                let call_recorded = self.call_recorded.notified();
                if self.calls.lock().expect("calls").len() >= expected {
                    return;
                }
                call_recorded.await;
            }
        }

        fn fail(&self, id: Uuid) {
            self.failures
                .lock()
                .expect("failures")
                .insert(id.to_string());
        }

        fn block_once(&self) {
            self.block_next.store(true, Ordering::Release);
        }

        fn resume_blocked(&self) {
            self.release_blocked.notify_one();
        }

        fn block_reconstruct(&self, id: Uuid) {
            self.blocked_reconstructs
                .lock()
                .expect("blocked reconstructs")
                .insert(id.to_string());
        }

        fn resume_reconstruct(&self) {
            self.release_reconstruct.notify_one();
        }
    }

    #[async_trait]
    impl StorageProvider for RecordingStorage {
        async fn probe(&self) -> CoreResult<bool> {
            self.inner.probe().await
        }

        async fn acquire(
            &self,
            opts: &AcquireOpts,
        ) -> std::result::Result<StorageSlot, StorageAcquireError> {
            self.inner.acquire(opts).await
        }

        async fn release(&self, slot: StorageSlot) -> CoreResult<()> {
            self.inner.release(slot).await
        }

        async fn release_by_id(&self, instance_id: &str) -> CoreResult<()> {
            self.inner.release_by_id(instance_id).await
        }

        async fn reconstruct(&self, instance_id: &str) -> CoreResult<StorageSlot> {
            if self
                .blocked_reconstructs
                .lock()
                .expect("blocked reconstructs")
                .remove(instance_id)
            {
                self.reconstruct_started.notify_one();
                self.release_reconstruct.notified().await;
            }
            self.inner.reconstruct(instance_id).await
        }

        async fn sync_artifacts(&self, slot: &StorageSlot) -> CoreResult<()> {
            self.calls.lock().expect("calls").push(slot.id.clone());
            self.call_recorded.notify_waiters();
            if self.block_next.swap(false, Ordering::AcqRel) {
                self.started.notify_one();
                self.release_blocked.notified().await;
            }
            if self.failures.lock().expect("failures").contains(&slot.id) {
                return Err(BlazeError::StorageError {
                    msg: format!(
                        "injected storage artifact synchronization failure for {}",
                        slot.id
                    ),
                });
            }
            Ok(())
        }

        fn pool_status(&self) -> PoolStatus {
            self.inner.pool_status()
        }
    }

    fn manager(
        temp: &Path,
        storage: Arc<dyn StorageProvider>,
    ) -> (Arc<SandboxManager>, SandboxManagerResources) {
        let state_dir = temp.join("state");
        let images = temp.join("images");
        let instances = temp.join("instances");
        let templates_dir = temp.join("templates");
        let runtime_template_imports = temp.join("runtime-template-imports");
        for directory in [&state_dir, &images, &instances, &runtime_template_imports] {
            std::fs::create_dir_all(directory).expect("test directory");
        }
        let template_catalog = TemplateCatalog::open(&TemplateSection {
            dir: templates_dir,
            import_root: Some(runtime_template_imports),
            ..TemplateSection::default()
        })
        .expect("test runtime template catalog");
        let mut spawners = SpawnerRegistry::new();
        spawners.insert(BackendKind::Mock, Arc::new(MockSpawner));
        let (manager, resources) = SandboxManager::new(SandboxManagerInit {
            instances: HashMap::new(),
            spawners,
            active_backend: BackendKind::Mock,
            storage,
            state_store: StateStore::new(state_dir),
            rootfs_size: 64,
            mem_size: 32,
            template_catalog,
        });
        (Arc::new(manager), resources)
    }

    async fn insert_running(
        manager: &SandboxManager,
        resources: &SandboxManagerResources,
        storage: &RecordingStorage,
        id: Uuid,
        acquire_storage: bool,
        insert_backend: bool,
        active_operation: bool,
    ) {
        let slot = if acquire_storage {
            Some(
                storage
                    .acquire(&AcquireOpts {
                        instance_id: id.to_string(),
                        rootfs_size: 64,
                        mem_size: 32,
                    })
                    .await
                    .expect("slot"),
            )
        } else {
            None
        };
        register_running(
            manager,
            resources,
            id,
            slot,
            storage.instances.join(id.to_string()).join("runtime"),
            insert_backend,
            active_operation,
        )
        .await;
    }

    async fn register_running(
        manager: &SandboxManager,
        resources: &SandboxManagerResources,
        id: Uuid,
        slot: Option<StorageSlot>,
        run_dir: PathBuf,
        insert_backend: bool,
        active_operation: bool,
    ) {
        let mut metadata = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:sync-test".into(),
            "sync-test".into(),
        );
        metadata.id = id;
        metadata
            .transition(SandboxState::Creating)
            .expect("pending to creating");
        metadata
            .transition(SandboxState::Running)
            .expect("creating to running");
        metadata.backend_ownership = BackendOwnership::Running;
        if active_operation {
            metadata.begin_operation(OperationKind::Create);
        }
        resources
            .instances
            .lock()
            .expect("instances")
            .insert(id, metadata);
        if insert_backend {
            let slot = slot.clone().unwrap_or_else(|| StorageSlot {
                id: id.to_string(),
                rootfs_path: PathBuf::new(),
                mem_path: PathBuf::new(),
                mem_diff_path: PathBuf::new(),
                rootfs_diff_path: PathBuf::new(),
                instance_dir: PathBuf::new(),
            });
            let run_dir = OwnedRunDir::for_test(id, run_dir);
            let owner = spawn_with_runtime_directory(
                &MockSpawner,
                BackendSpawnRequest::new(
                    SpawnRequest {
                        instance_id: id,
                        binary_path: PathBuf::new(),
                        storage: slot,
                        backend: BackendConfigs::default(),
                        vm: None,
                    },
                    run_dir.clone(),
                )
                .expect("matching backend request"),
            )
            .await
            .expect("mock owner");
            drop(run_dir);
            manager
                .insert_backend_owner(id, owner)
                .expect("register owner");
        }
    }

    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    async fn wait_for_operation_unlock(manager: &SandboxManager, id: Uuid) {
        let operation_lock = manager.operation_lock(id);
        let guard = tokio::time::timeout(Duration::from_secs(5), operation_lock.lock_owned())
            .await
            .unwrap_or_else(|_| panic!("operation lock for {id} remained held"));
        drop(guard);
    }

    async fn file_manager_with_artifact_sync_hook(
        temp: &Path,
    ) -> (
        Arc<SandboxManager>,
        Arc<ArtifactSyncOpenHook>,
        StorageSlot,
        PathBuf,
    ) {
        let images = temp.join("images");
        let instances = temp.join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let hook = Arc::new(ArtifactSyncOpenHook::new());
        let storage = Arc::new(FileStorageProvider::with_artifact_sync_open_hook(
            images,
            instances.clone(),
            hook.clone(),
        ));
        let id = Uuid::new_v4();
        let slot = storage
            .acquire(&AcquireOpts {
                instance_id: id.to_string(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .expect("slot");
        let (manager, resources) = manager(temp, storage);
        register_running(
            &manager,
            &resources,
            id,
            Some(slot.clone()),
            slot.instance_dir.join("runtime"),
            true,
            false,
        )
        .await;
        (manager, hook, slot, instances)
    }

    #[tokio::test]
    async fn sweep_syncs_running_records_and_isolates_failures() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        let failing = Uuid::new_v4();
        let succeeding = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, failing, true, true, false).await;
        insert_running(
            &manager, &resources, &storage, succeeding, true, true, false,
        )
        .await;
        storage.fail(failing);

        let summary = manager.sync_all_artifacts(Duration::from_secs(1)).await;

        assert_eq!(
            summary,
            StorageSyncSummary {
                selected: 2,
                synced: 1,
                skipped: 0,
                deferred: 0,
                failed: 1,
            }
        );
        assert_eq!(
            storage.calls().into_iter().collect::<HashSet<_>>(),
            HashSet::from([failing.to_string(), succeeding.to_string()])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sweep_keeps_sync_bound_to_the_open_slot_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp");
        let (manager, hook, slot, instances) =
            file_manager_with_artifact_sync_hook(temp.path()).await;

        let sweep = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.sync_all_artifacts(Duration::from_secs(5)).await })
        };
        hook.wait_until_open().await;

        let detached = instances.join(format!("{}.detached", slot.id));
        tokio::fs::rename(&slot.instance_dir, &detached)
            .await
            .expect("detach opened slot");
        let outside = temp.path().join("outside");
        tokio::fs::create_dir(&outside).await.expect("outside");
        for artifact in ["rootfs.ext4", "mem.bin", "mem.diff", "rootfs.diff"] {
            tokio::fs::write(outside.join(artifact), b"outside")
                .await
                .expect("outside artifact");
        }
        symlink(&outside, &slot.instance_dir).expect("replacement link");
        hook.resume();

        assert_eq!(
            sweep.await.expect("sweep"),
            StorageSyncSummary {
                selected: 1,
                synced: 1,
                skipped: 0,
                deferred: 0,
                failed: 0,
            }
        );
        for artifact in ["rootfs.ext4", "mem.bin", "mem.diff", "rootfs.diff"] {
            assert_eq!(
                tokio::fs::read(outside.join(artifact))
                    .await
                    .expect("outside artifact"),
                b"outside"
            );
        }
        assert!(detached.is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sweep_rejects_an_artifact_replaced_after_the_slot_is_open() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp");
        let (manager, hook, slot, _) = file_manager_with_artifact_sync_hook(temp.path()).await;
        let outside = temp.path().join("outside-memory-diff");
        tokio::fs::write(&outside, b"outside")
            .await
            .expect("outside artifact");
        let sweep = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.sync_all_artifacts(Duration::from_secs(5)).await })
        };
        hook.wait_until_open().await;

        tokio::fs::remove_file(&slot.mem_diff_path)
            .await
            .expect("remove slot artifact");
        symlink(&outside, &slot.mem_diff_path).expect("replacement link");
        hook.resume();

        assert_eq!(
            sweep.await.expect("sweep"),
            StorageSyncSummary {
                selected: 1,
                synced: 0,
                skipped: 0,
                deferred: 0,
                failed: 1,
            }
        );
        assert_eq!(
            tokio::fs::read(&outside).await.expect("outside artifact"),
            b"outside"
        );
        assert!(
            std::fs::symlink_metadata(&slot.mem_diff_path)
                .expect("replacement link")
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn sweep_reports_incomplete_running_owners_without_syncing() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        insert_running(
            &manager,
            &resources,
            &storage,
            Uuid::new_v4(),
            true,
            false,
            false,
        )
        .await;
        insert_running(
            &manager,
            &resources,
            &storage,
            Uuid::new_v4(),
            false,
            true,
            false,
        )
        .await;
        insert_running(
            &manager,
            &resources,
            &storage,
            Uuid::new_v4(),
            true,
            true,
            true,
        )
        .await;

        assert_eq!(
            manager.sync_all_artifacts(Duration::from_secs(1)).await,
            StorageSyncSummary {
                selected: 3,
                synced: 0,
                skipped: 0,
                deferred: 0,
                failed: 3,
            }
        );
        assert!(storage.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_provider_call_retains_ownership_and_bounds_late_work() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        let id = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, id, true, true, false).await;
        storage.block_once();

        let first = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.sync_all_artifacts(Duration::from_secs(5)).await })
        };
        storage.started.notified().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(first.await.expect("sweep").failed, 1);
        assert!(manager.operation_lock(id).try_lock().is_err());

        let second = manager.sync_all_artifacts(Duration::from_secs(5)).await;
        assert_eq!(second.deferred, 1);
        assert_eq!(second.synced, 0);
        assert_eq!(storage.calls(), vec![id.to_string()]);

        storage.resume_blocked();
        wait_for_operation_unlock(&manager, id).await;
        let retry = manager.sync_all_artifacts(Duration::from_secs(5)).await;
        assert_eq!(retry.synced, 1);
        assert_eq!(storage.calls(), vec![id.to_string(), id.to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn reconstruction_timeout_retains_lock_and_defers_more_provider_work() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        let blocked = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, blocked, true, true, false).await;
        storage.block_reconstruct(blocked);

        let sweep = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.sync_all_artifacts(Duration::from_secs(5)).await })
        };
        storage.reconstruct_started.notified().await;
        tokio::time::advance(Duration::from_secs(5)).await;

        assert_eq!(sweep.await.expect("sweep").failed, 1);
        assert!(manager.operation_lock(blocked).try_lock().is_err());

        let later = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, later, true, true, false).await;
        let deferred = manager.sync_all_artifacts(Duration::from_secs(5)).await;
        assert_eq!(deferred.selected, 2);
        assert_eq!(deferred.deferred, 2);
        assert!(storage.calls().is_empty());

        storage.resume_reconstruct();
        wait_for_operation_unlock(&manager, blocked).await;
        let retry = manager.sync_all_artifacts(Duration::from_secs(5)).await;
        assert_eq!(retry.synced, 2);
        assert!(storage.calls().contains(&blocked.to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn backend_liveness_uses_the_attempt_deadline_and_retains_ownership() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        let blocked = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, blocked, true, true, false).await;
        let backend = Arc::new(BlockingBackend::new());
        manager
            .insert_backend_owner(blocked, backend.clone())
            .expect("replace backend owner");

        let sweep = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.sync_all_artifacts(Duration::from_secs(5)).await })
        };
        backend.started.notified().await;
        tokio::time::advance(Duration::from_secs(5)).await;

        assert_eq!(sweep.await.expect("sweep").failed, 1);
        assert!(manager.operation_lock(blocked).try_lock().is_err());
        assert_eq!(manager.storage_sync_permits.available_permits(), 0);

        let later = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, later, true, true, false).await;
        let next = manager.sync_all_artifacts(Duration::from_secs(5)).await;
        assert_eq!(next.selected, 2);
        assert_eq!(next.deferred, 2);
        assert!(storage.calls().is_empty());

        backend.resume();
        wait_for_operation_unlock(&manager, blocked).await;
        let retry = manager.sync_all_artifacts(Duration::from_secs(5)).await;
        assert_eq!(retry.synced, 2);
    }

    #[tokio::test]
    async fn sweep_defers_active_operations_and_continues() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        let blocked = Uuid::new_v4();
        let ready = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, blocked, true, true, false).await;
        insert_running(&manager, &resources, &storage, ready, true, true, false).await;
        let lock = manager.operation_lock(blocked);
        let guard = lock.lock().await;
        resources
            .instances
            .lock()
            .expect("instances")
            .get_mut(&blocked)
            .expect("metadata")
            .begin_operation(OperationKind::Destroy);

        let summary = manager.sync_all_artifacts(Duration::from_secs(1)).await;

        assert_eq!(summary.selected, 2);
        assert_eq!(summary.synced, 1);
        assert_eq!(summary.deferred, 1);
        assert_eq!(storage.calls(), vec![ready.to_string()]);
        drop(guard);
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_worker_delays_first_tick_and_stops_before_next_sweep() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        let id = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, id, true, true, false).await;

        let mut worker =
            manager.start_storage_sync_loop(Duration::from_secs(10), Duration::from_secs(5));
        worker.wait_started().await;
        settle().await;
        assert!(storage.calls().is_empty());
        tokio::time::advance(Duration::from_secs(10)).await;
        storage.wait_for_calls(1).await;
        assert_eq!(storage.calls(), vec![id.to_string()]);

        worker.shutdown().await.expect("worker shutdown");
        tokio::time::advance(Duration::from_secs(30)).await;
        settle().await;
        assert_eq!(storage.calls(), vec![id.to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn worker_defers_a_busy_operation_lock_before_shutdown() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        let id = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, id, true, true, false).await;
        let lock = manager.operation_lock(id);
        let guard = lock.lock().await;
        let mut worker =
            manager.start_storage_sync_loop(Duration::from_secs(10), Duration::from_secs(5));
        worker.wait_started().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        settle().await;

        worker.shutdown().await.expect("worker shutdown");
        drop(guard);
        assert!(storage.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn worker_shutdown_joins_active_provider_attempts() {
        let temp = tempfile::tempdir().expect("temp");
        let storage = Arc::new(RecordingStorage::new(
            temp.path().join("images"),
            temp.path().join("instances"),
        ));
        let (manager, resources) = manager(temp.path(), storage.clone());
        let id = Uuid::new_v4();
        insert_running(&manager, &resources, &storage, id, true, true, false).await;
        storage.block_once();

        let mut worker =
            manager.start_storage_sync_loop(Duration::from_secs(10), Duration::from_secs(5));
        worker.wait_started().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        storage.started.notified().await;

        let shutdown = tokio::spawn(async move { worker.shutdown().await });
        settle().await;
        assert!(!shutdown.is_finished());
        assert!(manager.operation_lock(id).try_lock().is_err());
        assert_eq!(manager.storage_sync_permits.available_permits(), 0);

        storage.resume_blocked();
        shutdown
            .await
            .expect("shutdown task")
            .expect("worker shutdown");
        wait_for_operation_unlock(&manager, id).await;
        assert_eq!(manager.storage_sync_permits.available_permits(), 1);
    }

    #[tokio::test]
    async fn supervisor_reports_an_unexpected_worker_exit() {
        let cancellation = CancellationToken::new();
        let mut worker = StorageSyncLoop {
            cancellation,
            task: Some(tokio::spawn(async {})),
            attempts: AttemptSupervisor::default(),
            started: Arc::new(Notify::new()),
        };

        let error = worker
            .observe_exit()
            .await
            .expect_err("early worker exit must stop the daemon");

        assert!(error.to_string().contains("exited unexpectedly"));
        worker
            .shutdown()
            .await
            .expect("finished worker is already joined");
    }
}
