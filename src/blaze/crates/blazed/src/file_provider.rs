// SPDX-License-Identifier: Apache-2.0
//! File-based storage provider: creates per-instance directories with
//! rootfs and memory files on a local filesystem. Base images and mutable
//! instance slots use separate roots.

use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, fstat, fsync, open, openat, renameat_with, statat, unlinkat,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use blaze_core::error::{BlazeError, Result};
use blaze_core::storage::{
    AcquireOpts, PoolStatus, StorageAcquireError, StorageProvider, StorageRestoreTransaction,
    StorageSlot, TemplateArtifact, TemplateStorage, TemplateStorageSlot,
};

mod restore;

/// A filesystem-based provider that copies base artifacts when available and
/// otherwise creates sparse rootfs and memory files at configured sizes.
pub struct FileStorageProvider {
    images_dir: PathBuf,
    instances_dir: PathBuf,
    #[cfg(test)]
    artifact_sync_open_hook: Option<std::sync::Arc<ArtifactSyncOpenHook>>,
}

#[cfg(test)]
pub(crate) struct ArtifactSyncOpenHook {
    opened: tokio::sync::Notify,
    resume: tokio::sync::Notify,
    capture_finished: tokio::sync::Notify,
}

#[cfg(test)]
impl ArtifactSyncOpenHook {
    pub(crate) fn new() -> Self {
        Self {
            opened: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
            capture_finished: tokio::sync::Notify::new(),
        }
    }

    pub(crate) async fn wait_until_open(&self) {
        self.opened.notified().await;
    }

    pub(crate) fn resume(&self) {
        self.resume.notify_one();
    }

    #[cfg(feature = "test-failpoints")]
    pub(crate) async fn wait_until_capture_finished(&self) {
        self.capture_finished.notified().await;
    }
}

struct CaptureCompletion {
    #[cfg(test)]
    hook: Option<std::sync::Arc<ArtifactSyncOpenHook>>,
}

impl CaptureCompletion {
    fn finish(self) {
        #[cfg(test)]
        if let Some(hook) = self.hook {
            hook.capture_finished.notify_one();
        }
    }
}

impl FileStorageProvider {
    /// Create a provider with no separate image directory.
    ///
    /// This constructor is kept for focused tests. Daemon startup uses
    /// [`Self::with_images`] so immutable images and runtime slots cannot mix.
    #[cfg(test)]
    pub fn new(instances_dir: PathBuf) -> Self {
        Self {
            images_dir: instances_dir.clone(),
            instances_dir,
            artifact_sync_open_hook: None,
        }
    }

    /// Create a provider with distinct immutable image and runtime roots.
    pub fn with_images(images_dir: PathBuf, instances_dir: PathBuf) -> Self {
        Self {
            images_dir,
            instances_dir,
            #[cfg(test)]
            artifact_sync_open_hook: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_artifact_sync_open_hook(
        images_dir: PathBuf,
        instances_dir: PathBuf,
        hook: std::sync::Arc<ArtifactSyncOpenHook>,
    ) -> Self {
        Self {
            images_dir,
            instances_dir,
            artifact_sync_open_hook: Some(hook),
        }
    }

    fn slot_for_id(&self, instance_id: &str) -> Result<StorageSlot> {
        validate_instance_id(instance_id)?;
        let instance_dir = self.instances_dir.join(instance_id);
        if !instance_dir.starts_with(&self.instances_dir) || instance_dir == self.instances_dir {
            return Err(BlazeError::StorageError {
                msg: format!("slot '{instance_id}': path escapes instances_dir"),
            });
        }
        Ok(StorageSlot {
            id: instance_id.to_string(),
            rootfs_path: instance_dir.join("rootfs.ext4"),
            mem_path: instance_dir.join("mem.bin"),
            mem_diff_path: instance_dir.join("mem.diff"),
            rootfs_diff_path: instance_dir.join("rootfs.diff"),
            instance_dir,
        })
    }
}

#[derive(Clone, Copy)]
enum RequiredPathType {
    Directory,
    File,
}

impl RequiredPathType {
    fn description(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
        }
    }

    fn matches(self, metadata: &std::fs::Metadata) -> bool {
        match self {
            Self::Directory => metadata.is_dir(),
            Self::File => metadata.is_file(),
        }
    }
}

struct UnpublishedCheckpoint {
    parent: OwnedFd,
    temporary_file: std::fs::File,
    identity: Option<rustix::fs::Stat>,
    temporary: OsString,
    target: OsString,
    committed: bool,
}

impl UnpublishedCheckpoint {
    fn new(
        parent: OwnedFd,
        temporary_file: std::fs::File,
        temporary: OsString,
        target: OsString,
    ) -> Self {
        Self {
            parent,
            temporary_file,
            identity: None,
            temporary,
            target,
            committed: false,
        }
    }

    fn parent(&self) -> &OwnedFd {
        &self.parent
    }

    fn temporary_file(&self) -> &std::fs::File {
        &self.temporary_file
    }

    fn retain_identity(&mut self) -> std::io::Result<()> {
        let stat = fstat(&self.temporary_file).map_err(std::io::Error::from)?;
        self.identity = Some(stat);
        Ok(())
    }

    fn candidate_matches(&self, name: &std::ffi::OsStr) -> std::io::Result<bool> {
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| std::io::Error::other("checkpoint temporary identity is unavailable"))?;
        match statat(&self.parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(stat.st_dev == identity.st_dev && stat.st_ino == identity.st_ino),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(error) => Err(std::io::Error::from(error)),
        }
    }

    fn require_temporary_identity(&self) -> std::io::Result<()> {
        if self.candidate_matches(&self.temporary)? {
            return Ok(());
        }
        Err(std::io::Error::other(
            "checkpoint temporary file changed identity before publication",
        ))
    }

    fn publish_noreplace(&self) -> std::io::Result<()> {
        self.require_temporary_identity()?;
        let rename_error = renameat_with(
            &self.parent,
            &self.temporary,
            &self.parent,
            &self.target,
            RenameFlags::NOREPLACE,
        )
        .err()
        .map(std::io::Error::from);

        let temporary_matches = self.candidate_matches(&self.temporary)?;
        let target_matches = self.candidate_matches(&self.target)?;
        match (temporary_matches, target_matches, rename_error) {
            (false, true, _) => Ok(()),
            (true, false, Some(error)) => Err(error),
            (true, false, None) => Err(std::io::Error::other(
                "checkpoint rename reported success but retained the temporary name",
            )),
            (false, false, Some(error)) => Err(std::io::Error::other(format!(
                "checkpoint rename failed and the retained file lost both candidate names: {error}"
            ))),
            (false, false, None) => Err(std::io::Error::other(
                "checkpoint rename reported success but the retained file lost both candidate names",
            )),
            (true, true, Some(error)) => Err(std::io::Error::other(format!(
                "checkpoint rename failed with both candidate names linked to the retained file: {error}"
            ))),
            (true, true, None) => Err(std::io::Error::other(
                "checkpoint rename reported success with both candidate names linked to the retained file",
            )),
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for UnpublishedCheckpoint {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut removed = false;
        for name in [&self.temporary, &self.target] {
            if self.candidate_matches(name).unwrap_or(false)
                && unlinkat(&self.parent, name, AtFlags::empty()).is_ok()
            {
                removed = true;
            }
        }
        if removed {
            let _ = fsync(&self.parent);
        }
    }
}

async fn require_slot_path(
    instance_id: &str,
    path: &Path,
    required_type: RequiredPathType,
) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if required_type.matches(&metadata) => Ok(()),
        Ok(_) => Err(BlazeError::StorageIncomplete {
            instance_id: instance_id.to_string(),
            path: path.to_path_buf(),
            expected: required_type.description(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(BlazeError::StorageIncomplete {
                instance_id: instance_id.to_string(),
                path: path.to_path_buf(),
                expected: required_type.description(),
            })
        }
        Err(error) => Err(BlazeError::StorageError {
            msg: format!(
                "reconstruct '{instance_id}': inspect {}: {error}",
                path.display()
            ),
        }),
    }
}

#[async_trait]
impl StorageProvider for FileStorageProvider {
    async fn probe(&self) -> Result<bool> {
        Ok(self.images_dir.exists() && self.instances_dir.exists())
    }

    async fn acquire(
        &self,
        opts: &AcquireOpts,
    ) -> std::result::Result<StorageSlot, StorageAcquireError> {
        crate::failpoint::storage("storage-acquire")?;
        let slot = self.slot_for_id(&opts.instance_id)?;
        let instance_dir = slot.instance_dir.clone();

        // Atomic: create_dir fails with AlreadyExists if concurrent acquire races
        match tokio::fs::create_dir(&instance_dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(StorageAcquireError::clean(BlazeError::StorageError {
                    msg: format!(
                        "acquire '{}': instance directory already exists",
                        opts.instance_id
                    ),
                }));
            }
            Err(e) => {
                return Err(StorageAcquireError::clean(BlazeError::StorageError {
                    msg: format!("acquire '{}': create dir: {}", opts.instance_id, e),
                }));
            }
        }

        // Create rootfs + mem; rollback dir on failure
        let result = async {
            create_or_copy(
                &self.images_dir.join("rootfs.ext4"),
                &slot.rootfs_path,
                opts.rootfs_size,
            )
            .await?;
            create_or_copy(
                &self.images_dir.join("mem.bin"),
                &slot.mem_path,
                opts.mem_size,
            )
            .await?;
            tokio::fs::File::create(&slot.mem_diff_path).await?;
            tokio::fs::File::create(&slot.rootfs_diff_path).await?;
            crate::failpoint::storage("storage-acquire-artifacts")?;
            Ok::<(), BlazeError>(())
        }
        .await;

        if let Err(e) = result {
            let rollback = match crate::failpoint::storage("storage-acquire-rollback") {
                Ok(()) => tokio::fs::remove_dir_all(&instance_dir)
                    .await
                    .map_err(BlazeError::from),
                Err(error) => Err(error),
            };
            let source = match rollback {
                Ok(()) => BlazeError::StorageError {
                    msg: format!(
                        "acquire '{}': file setup failed, rolled back: {}",
                        opts.instance_id, e
                    ),
                },
                Err(cleanup) => {
                    return Err(StorageAcquireError::with_residual(
                        BlazeError::StorageError {
                            msg: format!(
                                "acquire '{}': file setup failed ({e}); rollback failed for {}: {cleanup}",
                                opts.instance_id,
                                instance_dir.display()
                            ),
                        },
                        slot,
                    ));
                }
            };
            return Err(StorageAcquireError::clean(source));
        }

        Ok(slot)
    }

    async fn acquire_template(
        &self,
        opts: &AcquireOpts,
        source: TemplateStorage,
    ) -> std::result::Result<TemplateStorageSlot, StorageAcquireError> {
        crate::failpoint::storage("storage-acquire-template")?;
        if opts.rootfs_size != source.rootfs.size_bytes || opts.mem_size != source.memory.size_bytes
        {
            return Err(StorageAcquireError::clean(BlazeError::StorageError {
                msg: format!(
                    "acquire template '{}': requested rootfs {} and memory {} do not match the \
                     template artifacts {} and {}",
                    opts.instance_id,
                    opts.rootfs_size,
                    opts.mem_size,
                    source.rootfs.size_bytes,
                    source.memory.size_bytes
                ),
            }));
        }
        let slot = self.slot_for_id(&opts.instance_id)?;
        let instance_dir = slot.instance_dir.clone();

        match tokio::fs::create_dir(&instance_dir).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(StorageAcquireError::clean(BlazeError::StorageError {
                    msg: format!(
                        "acquire template '{}': instance directory already exists",
                        opts.instance_id
                    ),
                }));
            }
            Err(error) => {
                return Err(StorageAcquireError::clean(BlazeError::StorageError {
                    msg: format!(
                        "acquire template '{}': create dir: {error}",
                        opts.instance_id
                    ),
                }));
            }
        }

        let payload_dir = instance_dir.join("backend");
        let snapshot_path = payload_dir.join("vmstate.snap");
        let payload_memory_path = payload_dir.join("memory.snap");
        let result = async {
            tokio::fs::create_dir(&payload_dir).await?;
            copy_template_artifact(source.rootfs, &slot.rootfs_path).await?;
            copy_template_artifact(source.memory, &slot.mem_path).await?;
            // The storage slot and restore payload refer to the same private
            // memory image. A hard link gives the backend its payload name
            // without duplicating a potentially large sparse file.
            tokio::fs::hard_link(&slot.mem_path, &payload_memory_path).await?;
            copy_template_artifact(source.vmstate, &snapshot_path).await?;
            create_empty_durable_file(&slot.mem_diff_path).await?;
            create_empty_durable_file(&slot.rootfs_diff_path).await?;
            crate::failpoint::storage("storage-acquire-template-artifacts")?;
            tokio::fs::File::open(&payload_dir)
                .await?
                .sync_all()
                .await?;
            tokio::fs::File::open(&instance_dir)
                .await?
                .sync_all()
                .await?;
            Ok::<(), BlazeError>(())
        }
        .await;

        if let Err(error) = result {
            let rollback = match crate::failpoint::storage("storage-acquire-rollback") {
                Ok(()) => tokio::fs::remove_dir_all(&instance_dir)
                    .await
                    .map_err(BlazeError::from),
                Err(cleanup) => Err(cleanup),
            };
            return match rollback {
                Ok(()) => Err(StorageAcquireError::clean(BlazeError::StorageError {
                    msg: format!(
                        "acquire template '{}': artifact setup failed, rolled back: {error}",
                        opts.instance_id
                    ),
                })),
                Err(cleanup) => Err(StorageAcquireError::with_residual(
                    BlazeError::StorageError {
                        msg: format!(
                            "acquire template '{}': artifact setup failed ({error}); rollback \
                             failed for {}: {cleanup}",
                            opts.instance_id,
                            instance_dir.display()
                        ),
                    },
                    slot,
                )),
            };
        }

        Ok(TemplateStorageSlot {
            storage: slot,
            payload_dir,
        })
    }

    fn supports_templates(&self) -> bool {
        true
    }

    async fn release(&self, slot: StorageSlot) -> Result<()> {
        crate::failpoint::storage("storage-release")?;
        // Re-derive the canonical path from instances_dir + slot.id. Do not
        // trust path strings carried in a persisted or externally built slot.
        let canonical_dir = self.slot_for_id(&slot.id)?.instance_dir;
        if canonical_dir.exists() {
            tokio::fs::remove_dir_all(&canonical_dir)
                .await
                .map_err(|e| BlazeError::StorageError {
                    msg: format!("release '{}': {}", slot.id, e),
                })?;
        }
        Ok(())
    }

    async fn release_by_id(&self, instance_id: &str) -> Result<()> {
        let slot = self.slot_for_id(instance_id)?;
        self.release(slot).await
    }

    async fn reconstruct(&self, instance_id: &str) -> Result<StorageSlot> {
        let slot = self.slot_for_id(instance_id)?;
        require_slot_path(instance_id, &slot.instance_dir, RequiredPathType::Directory).await?;
        for path in [
            &slot.rootfs_path,
            &slot.mem_path,
            &slot.mem_diff_path,
            &slot.rootfs_diff_path,
        ] {
            require_slot_path(instance_id, path, RequiredPathType::File).await?;
        }
        Ok(slot)
    }

    async fn sync_artifacts(&self, slot: &StorageSlot) -> Result<()> {
        crate::failpoint::storage("sync-artifacts")?;
        // Never trust paths carried by a runtime or persisted slot. Rebuild
        // the complete provider-owned artifact set from the validated ID.
        let canonical = self.slot_for_id(&slot.id)?;
        let instance_dir = canonical.instance_dir.clone();
        let directory_fd = open_required_slot_path(
            &slot.id,
            &canonical.instance_dir,
            RequiredPathType::Directory,
            move || {
                open(
                    &instance_dir,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
            },
        )
        .await?;
        #[cfg(test)]
        if let Some(hook) = &self.artifact_sync_open_hook {
            hook.opened.notify_one();
            hook.resume.notified().await;
        }
        let directory_fd = Arc::new(directory_fd);
        for (name, path) in [
            ("rootfs.ext4", &canonical.rootfs_path),
            ("mem.bin", &canonical.mem_path),
            ("mem.diff", &canonical.mem_diff_path),
            ("rootfs.diff", &canonical.rootfs_diff_path),
        ] {
            let open_directory_fd = Arc::clone(&directory_fd);
            let file_fd =
                open_required_slot_path(&slot.id, path, RequiredPathType::File, move || {
                    openat(
                        &*open_directory_fd,
                        name,
                        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                        Mode::empty(),
                    )
                })
                .await?;
            let file = tokio::fs::File::from_std(std::fs::File::from(file_fd));
            file.sync_all()
                .await
                .map_err(|error| BlazeError::StorageError {
                    msg: format!(
                        "sync artifacts '{}': sync {}: {error}",
                        slot.id,
                        path.display()
                    ),
                })?;
        }
        let directory_fd = Arc::try_unwrap(directory_fd).map_err(|_| BlazeError::StorageError {
            msg: format!(
                "sync artifacts '{}': directory descriptor remained shared after opening artifacts",
                slot.id
            ),
        })?;
        let directory = tokio::fs::File::from_std(std::fs::File::from(directory_fd));
        directory
            .sync_all()
            .await
            .map_err(|error| BlazeError::StorageError {
                msg: format!(
                    "sync artifacts '{}': sync directory {}: {error}",
                    slot.id,
                    canonical.instance_dir.display()
                ),
            })?;
        Ok(())
    }

    fn supports_checkpoint_capture(&self) -> bool {
        true
    }

    async fn capture_checkpoint(&self, slot: &StorageSlot, target: &Path) -> Result<()> {
        let (source, source_path) = self.checkpoint_source(slot).await?;
        let (target_parent, target) = checkpoint_target(target).await?;
        let target_parent_owner = open_checkpoint_target_parent(&target_parent, &target).await?;
        #[cfg(test)]
        if let Some(hook) = &self.artifact_sync_open_hook {
            hook.opened.notify_one();
            hook.resume.notified().await;
        }

        let target_name = target
            .file_name()
            .expect("validated checkpoint target")
            .to_os_string();
        let temporary_name = checkpoint_temporary_name(&target);
        let completion = CaptureCompletion {
            #[cfg(test)]
            hook: self.artifact_sync_open_hook.clone(),
        };
        let result = capture_rootfs(
            source,
            target_parent_owner,
            temporary_name,
            target_name,
            completion,
        )
        .await;
        result.map_err(|error| BlazeError::StorageError {
            msg: format!(
                "capture checkpoint for '{}': copy {} to {}: {error}",
                slot.id,
                source_path.display(),
                target.display()
            ),
        })
    }

    fn supports_checkpoint_restore(&self) -> bool {
        true
    }

    async fn stage_checkpoint_restore(
        &self,
        slot: &StorageSlot,
        source: &Path,
    ) -> Result<StorageRestoreTransaction> {
        restore::stage(self, slot, source).await
    }

    async fn activate_checkpoint_restore(
        &self,
        transaction: &StorageRestoreTransaction,
    ) -> Result<()> {
        restore::activate(self, transaction).await
    }

    async fn commit_checkpoint_restore(
        &self,
        transaction: &StorageRestoreTransaction,
    ) -> Result<()> {
        restore::commit(self, transaction).await
    }

    async fn abort_checkpoint_restore(
        &self,
        transaction: &StorageRestoreTransaction,
    ) -> Result<()> {
        restore::abort(self, transaction).await
    }

    async fn reconcile_checkpoint_restore(&self, instance_id: &str) -> Result<()> {
        restore::reconcile(self, instance_id).await
    }

    fn pool_status(&self) -> PoolStatus {
        PoolStatus::default()
    }
}

async fn open_required_slot_path<F>(
    instance_id: &str,
    path: &Path,
    required_type: RequiredPathType,
    open_path: F,
) -> Result<std::os::fd::OwnedFd>
where
    F: FnOnce() -> rustix::io::Result<std::os::fd::OwnedFd> + Send + 'static,
{
    let task_instance_id = instance_id.to_string();
    let task_path = path.to_path_buf();
    let join_instance_id = task_instance_id.clone();
    let join_path = task_path.clone();
    tokio::task::spawn_blocking(move || {
        let file = open_path().map_err(|error| {
            if matches!(
                error,
                rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP
            ) {
                BlazeError::StorageIncomplete {
                    instance_id: task_instance_id.clone(),
                    path: task_path.clone(),
                    expected: required_type.description(),
                }
            } else {
                BlazeError::StorageError {
                    msg: format!(
                        "sync artifacts '{task_instance_id}': open {}: {error}",
                        task_path.display()
                    ),
                }
            }
        })?;
        let file = std::fs::File::from(file);
        let metadata = file.metadata().map_err(|error| BlazeError::StorageError {
            msg: format!(
                "sync artifacts '{task_instance_id}': inspect {}: {error}",
                task_path.display()
            ),
        })?;
        if !required_type.matches(&metadata) {
            return Err(BlazeError::StorageIncomplete {
                instance_id: task_instance_id,
                path: task_path,
                expected: required_type.description(),
            });
        }
        Ok(file.into())
    })
    .await
    .map_err(|error| BlazeError::StorageError {
        msg: format!(
            "sync artifacts '{join_instance_id}': open task for {} failed: {error}",
            join_path.display()
        ),
    })?
}

impl FileStorageProvider {
    async fn checkpoint_source(&self, slot: &StorageSlot) -> Result<(tokio::fs::File, PathBuf)> {
        let canonical = self.slot_for_id(&slot.id)?;
        let instance_path = canonical.instance_dir.clone();
        let directory = open_required_slot_path(
            &slot.id,
            &canonical.instance_dir,
            RequiredPathType::Directory,
            move || {
                open(
                    &instance_path,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
            },
        )
        .await?;
        let directory = Arc::new(directory);
        let open_directory = Arc::clone(&directory);
        let source = open_required_slot_path(
            &slot.id,
            &canonical.rootfs_path,
            RequiredPathType::File,
            move || {
                openat(
                    &*open_directory,
                    "rootfs.ext4",
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                    Mode::empty(),
                )
            },
        )
        .await?;
        Ok((
            tokio::fs::File::from_std(std::fs::File::from(source)),
            canonical.rootfs_path,
        ))
    }
}

async fn create_or_copy(
    source: &std::path::Path,
    target: &std::path::Path,
    size: u64,
) -> std::io::Result<()> {
    if source.is_file() && source != target {
        tokio::fs::copy(source, target).await?;
        return Ok(());
    }
    let file = tokio::fs::File::create(target).await?;
    if size > 0 {
        file.set_len(size).await?;
    }
    Ok(())
}

/// Copy one template artifact into provider-owned storage and revalidate it.
///
/// The source is an already-open object, so the copy cannot be redirected by
/// replacing a catalog path after validation. Size and digest are checked
/// again against the provider-owned destination after the sparse copy. Hashing
/// the copied object verifies the exact bytes the sandbox will use without
/// expanding holes in rootfs or guest-memory artifacts.
async fn copy_template_artifact(source: TemplateArtifact, target: &Path) -> Result<()> {
    let metadata = source
        .file
        .metadata()
        .map_err(|error| BlazeError::StorageError {
            msg: format!("inspect template artifact: {error}"),
        })?;
    if !metadata.is_file() || metadata.len() != source.size_bytes {
        return Err(BlazeError::StorageError {
            msg: format!(
                "template artifact has size {}; expected {}",
                metadata.len(),
                source.size_bytes
            ),
        });
    }

    let target = target.to_path_buf();
    let expected_size = source.size_bytes;
    let expected_digest = source.sha256;
    crate::failpoint::spawn_blocking(move || {
        let mut destination = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&target)?;
        copy_sparse_file(&source.file, &destination)?;

        let copied = destination.metadata()?.len();
        if copied != expected_size {
            return Err(BlazeError::StorageError {
                msg: format!("template artifact has {copied} bytes; expected {expected_size}"),
            });
        }
        destination.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 1024 * 1024];
        let mut hashed = 0_u64;
        loop {
            let read = destination.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hashed = hashed
                .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
                .ok_or_else(|| BlazeError::StorageError {
                    msg: "template artifact size overflow".to_string(),
                })?;
            digest.update(&buffer[..read]);
        }
        if hashed != expected_size {
            return Err(BlazeError::StorageError {
                msg: format!("template artifact has {hashed} bytes; expected {expected_size}"),
            });
        }
        let actual = format!("{:x}", digest.finalize());
        if actual != expected_digest {
            return Err(BlazeError::StorageError {
                msg: format!(
                    "template artifact digest mismatch: expected {expected_digest}, got {actual}"
                ),
            });
        }
        destination.sync_all()?;
        Ok(())
    })
    .await
    .map_err(|error| BlazeError::StorageError {
        msg: format!("copy template artifact task failed: {error}"),
    })?
}

/// Create one empty writable-diff file and persist its directory entry.
async fn create_empty_durable_file(path: &Path) -> Result<()> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?
        .sync_all()
        .await?;
    Ok(())
}

async fn canonical_plain_path(path: &Path, required_type: RequiredPathType) -> Result<PathBuf> {
    let metadata =
        tokio::fs::symlink_metadata(path)
            .await
            .map_err(|error| BlazeError::StorageError {
                msg: format!("inspect checkpoint path {}: {error}", path.display()),
            })?;
    if !required_type.matches(&metadata) || metadata.file_type().is_symlink() {
        return Err(BlazeError::StorageError {
            msg: format!(
                "checkpoint path {} is not a plain {}",
                path.display(),
                required_type.description()
            ),
        });
    }
    tokio::fs::canonicalize(path)
        .await
        .map_err(|error| BlazeError::StorageError {
            msg: format!("canonicalize checkpoint path {}: {error}", path.display()),
        })
}

async fn checkpoint_target(target: &Path) -> Result<(PathBuf, PathBuf)> {
    if !matches!(target.components().next_back(), Some(Component::Normal(_))) {
        return Err(BlazeError::StorageError {
            msg: format!(
                "checkpoint target {} must end in a file name",
                target.display()
            ),
        });
    }
    let parent = target.parent().ok_or_else(|| BlazeError::StorageError {
        msg: format!(
            "checkpoint target {} has no parent directory",
            target.display()
        ),
    })?;
    let parent = if is_retained_directory_adapter(parent) {
        let metadata =
            tokio::fs::metadata(parent)
                .await
                .map_err(|error| BlazeError::StorageError {
                    msg: format!(
                        "inspect retained checkpoint directory {}: {error}",
                        parent.display()
                    ),
                })?;
        if !metadata.is_dir() {
            return Err(BlazeError::StorageError {
                msg: format!(
                    "retained checkpoint path {} is not a directory",
                    parent.display()
                ),
            });
        }
        parent.to_path_buf()
    } else {
        canonical_plain_path(parent, RequiredPathType::Directory).await?
    };
    let file_name = target.file_name().ok_or_else(|| BlazeError::StorageError {
        msg: format!("checkpoint target {} has no file name", target.display()),
    })?;
    let target = parent.join(file_name);
    Ok((parent, target))
}

#[cfg(target_os = "linux")]
fn is_retained_directory_adapter(path: &Path) -> bool {
    path.parent() == Some(Path::new("/proc/self/fd"))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()))
            .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn is_retained_directory_adapter(_path: &Path) -> bool {
    false
}

async fn open_checkpoint_target_parent(parent: &Path, target: &Path) -> Result<OwnedFd> {
    let parent_path = parent.to_path_buf();
    let target_path = target.to_path_buf();
    let target_name = target
        .file_name()
        .expect("validated checkpoint target")
        .to_os_string();
    let follow_retained_adapter = is_retained_directory_adapter(parent);
    crate::failpoint::spawn_blocking(move || {
        let base_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let flags = if follow_retained_adapter {
            base_flags
        } else {
            base_flags | OFlags::NOFOLLOW
        };
        let parent_owner =
            open(&parent_path, flags, Mode::empty()).map_err(|error| BlazeError::StorageError {
                msg: format!(
                    "open checkpoint target directory {}: {error}",
                    parent_path.display()
                ),
            })?;
        match statat(&parent_owner, &target_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Err(BlazeError::StorageError {
                msg: format!("checkpoint target {} already exists", target_path.display()),
            }),
            Err(rustix::io::Errno::NOENT) => Ok(parent_owner),
            Err(error) => Err(BlazeError::StorageError {
                msg: format!(
                    "inspect checkpoint target {}: {error}",
                    target_path.display()
                ),
            }),
        }
    })
    .await
    .map_err(|error| BlazeError::StorageError {
        msg: format!(
            "open checkpoint target directory {}: blocking task failed: {error}",
            parent.display()
        ),
    })?
}

fn checkpoint_temporary_name(target: &Path) -> OsString {
    let mut name = OsString::from(".");
    name.push(target.file_name().expect("validated checkpoint target"));
    name.push(format!(".capture-{}.tmp", Uuid::new_v4()));
    name
}

async fn capture_rootfs(
    source_file: tokio::fs::File,
    target_parent: OwnedFd,
    temporary_name: OsString,
    target_name: OsString,
    completion: CaptureCompletion,
) -> std::io::Result<()> {
    let source_file = source_file.into_std().await;
    crate::failpoint::spawn_blocking(move || {
        let result = (|| {
            if !source_file.metadata()?.is_file() {
                return Err(std::io::Error::other(
                    "checkpoint source owner is not a regular file",
                ));
            }
            let temporary_file = openat(
                &target_parent,
                &temporary_name,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::RUSR.union(Mode::WUSR),
            )
            .map(std::fs::File::from)
            .map_err(std::io::Error::from)?;
            let mut cleanup = UnpublishedCheckpoint::new(
                target_parent,
                temporary_file,
                temporary_name,
                target_name,
            );
            cleanup.retain_identity()?;
            copy_sparse_file(&source_file, cleanup.temporary_file())?;
            cleanup.temporary_file().sync_all()?;
            crate::failpoint::pause_blocking("storage-capture-before-publish");
            cleanup.publish_noreplace()?;
            crate::failpoint::storage("storage-capture-after-publish")
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            fsync(cleanup.parent()).map_err(std::io::Error::from)?;
            cleanup.commit();
            Ok(())
        })();
        completion.finish();
        result
    })
    .await
    .map_err(|error| std::io::Error::other(format!("checkpoint capture task failed: {error}")))?
}

fn copy_sparse_file(source: &std::fs::File, target: &std::fs::File) -> std::io::Result<()> {
    copy_sparse_file_with_seek(source, target, |file, position| {
        rustix::fs::seek(file, position)
    })
}

fn copy_sparse_file_with_seek<F>(
    source: &std::fs::File,
    target: &std::fs::File,
    mut seek: F,
) -> std::io::Result<()>
where
    F: FnMut(&std::fs::File, rustix::fs::SeekFrom) -> std::result::Result<u64, rustix::io::Errno>,
{
    const COPY_BUFFER_SIZE: usize = 64 * 1024;

    let logical_len = source.metadata()?.len();
    let mut position = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_SIZE];

    while position < logical_len {
        let data = match seek(source, rustix::fs::SeekFrom::Data(position)) {
            Ok(data) => data,
            Err(rustix::io::Errno::NXIO) => break,
            Err(error) if sparse_seek_is_unsupported(error) => {
                return copy_sparse_file_by_scanning(source, target, logical_len);
            }
            Err(error) => return Err(error.into()),
        };
        if data >= logical_len {
            break;
        }
        let hole = match seek(source, rustix::fs::SeekFrom::Hole(data)) {
            Ok(hole) => hole.min(logical_len),
            Err(error) if sparse_seek_is_unsupported(error) => {
                return copy_sparse_file_by_scanning(source, target, logical_len);
            }
            Err(error) => return Err(error.into()),
        };
        if hole <= data {
            return Err(std::io::Error::other(format!(
                "invalid sparse extent {data}..{hole} for file length {logical_len}"
            )));
        }

        let mut offset = data;
        while offset < hole {
            let remaining = hole - offset;
            let requested = usize::try_from(remaining.min(COPY_BUFFER_SIZE as u64))
                .map_err(|_| std::io::Error::other("sparse extent exceeds platform limits"))?;
            let read = rustix::io::pread(source, &mut buffer[..requested], offset)
                .map_err(std::io::Error::from)?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("sparse extent ended before offset {hole}"),
                ));
            }
            write_all_at(target, &buffer[..read], offset)?;
            offset += read as u64;
        }
        position = hole;
    }

    rustix::fs::ftruncate(target, logical_len).map_err(std::io::Error::from)
}

fn sparse_seek_is_unsupported(error: rustix::io::Errno) -> bool {
    error == rustix::io::Errno::INVAL || error == rustix::io::Errno::NOTSUP
}

fn copy_sparse_file_by_scanning(
    source: &std::fs::File,
    target: &std::fs::File,
    logical_len: u64,
) -> std::io::Result<()> {
    const COPY_BUFFER_SIZE: usize = 64 * 1024;

    // A seek implementation may report unsupported after earlier extents were
    // copied. Reset the private temporary file before rebuilding it so skipped
    // zero blocks cannot retain stale bytes from that partial attempt.
    rustix::fs::ftruncate(target, 0).map_err(std::io::Error::from)?;
    let mut buffer = [0_u8; COPY_BUFFER_SIZE];
    let mut offset = 0_u64;
    while offset < logical_len {
        let remaining = logical_len - offset;
        let requested = usize::try_from(remaining.min(COPY_BUFFER_SIZE as u64))
            .map_err(|_| std::io::Error::other("checkpoint file exceeds platform limits"))?;
        let read = rustix::io::pread(source, &mut buffer[..requested], offset)
            .map_err(std::io::Error::from)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("checkpoint source ended before offset {logical_len}"),
            ));
        }
        if buffer[..read].iter().any(|byte| *byte != 0) {
            write_all_at(target, &buffer[..read], offset)?;
        }
        offset += read as u64;
    }
    rustix::fs::ftruncate(target, logical_len).map_err(std::io::Error::from)
}

fn write_all_at(target: &std::fs::File, buffer: &[u8], offset: u64) -> std::io::Result<()> {
    let mut written = 0;
    while written < buffer.len() {
        let count = rustix::io::pwrite(target, &buffer[written..], offset + written as u64)
            .map_err(std::io::Error::from)?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "failed to write sparse checkpoint extent",
            ));
        }
        written += count;
    }
    Ok(())
}

fn validate_instance_id(instance_id: &str) -> Result<()> {
    if instance_id.is_empty()
        || instance_id.contains('/')
        || instance_id.contains('\\')
        || instance_id == ".."
        || instance_id == "."
        || std::path::Path::new(instance_id).is_absolute()
    {
        return Err(BlazeError::StorageError {
            msg: format!("invalid instance_id '{instance_id}': must be a single path component"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    fn sha256_file(path: &Path) -> String {
        use std::io::Read;

        let mut file = std::fs::File::open(path).expect("open digest source");
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).expect("read digest source");
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        format!("{:x}", digest.finalize())
    }

    async fn checkpoint_fixture(
        instance_id: &str,
    ) -> (tempfile::TempDir, FileStorageProvider, StorageSlot, PathBuf) {
        let temp = tempfile::TempDir::new().unwrap();
        let instances = temp.path().join("instances");
        let checkpoints = temp.path().join("checkpoints");
        tokio::fs::create_dir(&instances).await.unwrap();
        tokio::fs::create_dir(&checkpoints).await.unwrap();
        let provider = FileStorageProvider::new(instances);
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: instance_id.to_string(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        (temp, provider, slot, checkpoints)
    }

    #[tokio::test]
    async fn probe_existing_dir_returns_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        assert!(provider.probe().await.unwrap());
    }

    #[tokio::test]
    async fn probe_missing_dir_returns_false() {
        let provider =
            FileStorageProvider::new(PathBuf::from("/nonexistent/blaze-test-storage-probe"));
        assert!(!provider.probe().await.unwrap());
    }

    #[tokio::test]
    async fn acquire_creates_slot_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        let opts = AcquireOpts {
            instance_id: "test-inst-001".to_string(),
            rootfs_size: 1024,
            mem_size: 512,
        };
        let slot = provider.acquire(&opts).await.unwrap();
        assert_eq!(slot.id, "test-inst-001");
        assert!(slot.rootfs_path.exists());
        assert!(slot.mem_path.exists());
        assert!(slot.instance_dir.exists());
        // Verify sparse file lengths match requested sizes
        assert_eq!(
            tokio::fs::metadata(&slot.rootfs_path).await.unwrap().len(),
            1024
        );
        assert_eq!(
            tokio::fs::metadata(&slot.mem_path).await.unwrap().len(),
            512
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn template_artifact_copy_preserves_sparse_regions_and_revalidates_digest() {
        use std::io::{Read, Seek, Write};
        use std::os::unix::fs::MetadataExt;

        const LOGICAL_LEN: u64 = 64 * 1024 * 1024;
        const FIRST_OFFSET: u64 = 8 * 1024;
        const LAST_OFFSET: u64 = 48 * 1024 * 1024 + 91;
        const FIRST_DATA: &[u8] = b"template-first-extent";
        const LAST_DATA: &[u8] = b"template-last-extent";

        let temp = tempfile::tempdir().expect("temp");
        let source_path = temp.path().join("source.img");
        let mut source = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source_path)
            .expect("source");
        source.set_len(LOGICAL_LEN).expect("logical source length");
        source
            .seek(std::io::SeekFrom::Start(FIRST_OFFSET))
            .expect("first offset");
        source.write_all(FIRST_DATA).expect("first data");
        source
            .seek(std::io::SeekFrom::Start(LAST_OFFSET))
            .expect("last offset");
        source.write_all(LAST_DATA).expect("last data");
        source.sync_all().expect("source sync");
        let source_blocks = source.metadata().expect("source metadata").blocks();
        drop(source);
        let expected_digest = sha256_file(&source_path);

        let target_path = temp.path().join("target.img");
        copy_template_artifact(
            TemplateArtifact {
                file: std::fs::File::open(&source_path).expect("open source"),
                size_bytes: LOGICAL_LEN,
                sha256: expected_digest.clone(),
            },
            &target_path,
        )
        .await
        .expect("copy sparse template artifact");

        let metadata = std::fs::metadata(&target_path).expect("target metadata");
        assert_eq!(metadata.len(), LOGICAL_LEN);
        assert!(
            metadata.blocks().saturating_mul(512) < LOGICAL_LEN / 4,
            "template copy allocated {} bytes for a {LOGICAL_LEN}-byte sparse source",
            metadata.blocks().saturating_mul(512)
        );
        assert!(
            metadata.blocks() <= source_blocks.saturating_add(32),
            "template copy used {} blocks for a source using {source_blocks} blocks",
            metadata.blocks()
        );
        assert_eq!(sha256_file(&target_path), expected_digest);

        let mut target = std::fs::File::open(&target_path).expect("target");
        let mut first = vec![0; FIRST_DATA.len()];
        target
            .seek(std::io::SeekFrom::Start(FIRST_OFFSET))
            .expect("target first offset");
        target.read_exact(&mut first).expect("target first data");
        assert_eq!(first, FIRST_DATA);
        let mut last = vec![0; LAST_DATA.len()];
        target
            .seek(std::io::SeekFrom::Start(LAST_OFFSET))
            .expect("target last offset");
        target.read_exact(&mut last).expect("target last data");
        assert_eq!(last, LAST_DATA);
        let mut hole = [1_u8; 4096];
        target
            .seek(std::io::SeekFrom::Start(24 * 1024 * 1024))
            .expect("target hole offset");
        target.read_exact(&mut hole).expect("target hole");
        assert!(hole.iter().all(|byte| *byte == 0));

        let mismatch = copy_template_artifact(
            TemplateArtifact {
                file: std::fs::File::open(&source_path).expect("reopen source"),
                size_bytes: LOGICAL_LEN,
                sha256: "0".repeat(64),
            },
            &temp.path().join("digest-mismatch.img"),
        )
        .await
        .expect_err("digest mismatch");
        assert!(mismatch.to_string().contains("digest mismatch"));
    }

    #[tokio::test]
    async fn release_removes_instance_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        let opts = AcquireOpts {
            instance_id: "test-inst-release".to_string(),
            rootfs_size: 1024,
            mem_size: 512,
        };
        let slot = provider.acquire(&opts).await.unwrap();
        let dir = slot.instance_dir.clone();
        assert!(dir.exists());
        provider.release(slot).await.unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn pool_status_returns_current_capacity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        let status = provider.pool_status();
        assert_eq!(status.ready, 0);
        assert_eq!(status.capacity, 0);
        assert_eq!(status.pending, 0);
        assert_eq!(status.quarantined, 0);
    }

    #[tokio::test]
    async fn release_rejects_forged_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let fp = FileStorageProvider::new(dir.path().to_path_buf());
        let forged_slot = StorageSlot {
            id: "../../etc".into(),
            rootfs_path: PathBuf::from("/etc/passwd"),
            mem_path: PathBuf::from("/etc/shadow"),
            mem_diff_path: PathBuf::from("/etc/shadow"),
            rootfs_diff_path: PathBuf::from("/etc/passwd"),
            instance_dir: PathBuf::from("/etc"),
        };
        assert!(fp.release(forged_slot).await.is_err());
    }

    #[tokio::test]
    async fn acquire_rejects_duplicate_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let fp = FileStorageProvider::new(dir.path().to_path_buf());
        let opts = AcquireOpts {
            instance_id: "dup-1".into(),
            rootfs_size: 64,
            mem_size: 32,
        };

        // First acquire succeeds
        let _ = fp.acquire(&opts).await.unwrap();

        // Second acquire with same ID fails
        let r = fp.acquire(&opts).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn acquire_rejects_path_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());

        // Absolute path
        let r = provider
            .acquire(&AcquireOpts {
                instance_id: "/etc/passwd".into(),
                rootfs_size: 0,
                mem_size: 0,
            })
            .await;
        assert!(r.is_err());

        // Parent traversal
        let r = provider
            .acquire(&AcquireOpts {
                instance_id: "../escape".into(),
                rootfs_size: 0,
                mem_size: 0,
            })
            .await;
        assert!(r.is_err());

        // Slash in middle
        let r = provider
            .acquire(&AcquireOpts {
                instance_id: "foo/bar".into(),
                rootfs_size: 0,
                mem_size: 0,
            })
            .await;
        assert!(r.is_err());

        // Empty string
        let r = provider
            .acquire(&AcquireOpts {
                instance_id: "".into(),
                rootfs_size: 0,
                mem_size: 0,
            })
            .await;
        assert!(r.is_err());

        // Dot-dot
        let r = provider
            .acquire(&AcquireOpts {
                instance_id: "..".into(),
                rootfs_size: 0,
                mem_size: 0,
            })
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn reconstruct_derives_paths_from_id() {
        let temp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(temp.path().to_path_buf());
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "restore-me".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        let reconstructed = provider.reconstruct("restore-me").await.unwrap();
        assert_eq!(reconstructed, slot);
    }

    #[tokio::test]
    async fn reconstruct_classifies_missing_artifact_as_incomplete() {
        let temp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(temp.path().to_path_buf());
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "missing-artifact".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        tokio::fs::remove_file(&slot.mem_diff_path).await.unwrap();

        let error = provider
            .reconstruct("missing-artifact")
            .await
            .expect_err("missing artifact must invalidate the slot");

        assert!(matches!(
            error,
            BlazeError::StorageIncomplete {
                ref instance_id,
                ref path,
                expected: "file",
            } if instance_id == "missing-artifact" && path == &slot.mem_diff_path
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reconstruct_rejects_a_linked_slot_root() {
        use std::os::unix::fs::symlink;

        let storage = tempfile::TempDir::new().unwrap();
        let target = tempfile::TempDir::new().unwrap();
        for artifact in ["rootfs.ext4", "mem.bin", "mem.diff", "rootfs.diff"] {
            tokio::fs::write(target.path().join(artifact), b"external")
                .await
                .unwrap();
        }
        symlink(target.path(), storage.path().join("linked-slot")).unwrap();
        let provider = FileStorageProvider::new(storage.path().to_path_buf());

        let error = provider
            .reconstruct("linked-slot")
            .await
            .expect_err("linked slot root must be rejected");

        assert!(matches!(
            error,
            BlazeError::StorageIncomplete {
                ref instance_id,
                ref path,
                expected: "directory",
            } if instance_id == "linked-slot" && path == &storage.path().join("linked-slot")
        ));
        assert!(
            std::fs::symlink_metadata(storage.path().join("linked-slot"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(target.path().is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reconstruct_rejects_a_linked_slot_artifact() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(temp.path().to_path_buf());
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "linked-artifact".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        tokio::fs::remove_file(&slot.mem_diff_path).await.unwrap();
        let external = temp.path().join("external-memory-diff");
        tokio::fs::write(&external, b"external").await.unwrap();
        symlink(&external, &slot.mem_diff_path).unwrap();

        let error = provider
            .reconstruct("linked-artifact")
            .await
            .expect_err("linked artifact must be rejected");

        assert!(matches!(
            error,
            BlazeError::StorageIncomplete {
                ref instance_id,
                ref path,
                expected: "file",
            } if instance_id == "linked-artifact" && path == &slot.mem_diff_path
        ));
        assert!(
            std::fs::symlink_metadata(&slot.mem_diff_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(external.is_file());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slot_open_does_not_block_async_runtime() -> Result<()> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        use std::time::Duration;

        let temp = tempfile::tempdir()?;
        let path = temp.path().join("artifact");
        tokio::fs::write(&path, b"artifact").await?;

        let (opened_tx, opened_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let watchdog_release = release_tx.clone();
        let (watchdog_cancel_tx, watchdog_cancel_rx) = mpsc::channel();
        let watchdog_fired = Arc::new(AtomicBool::new(false));
        let watchdog_state = Arc::clone(&watchdog_fired);
        let watchdog = std::thread::spawn(move || {
            if watchdog_cancel_rx
                .recv_timeout(Duration::from_secs(2))
                .is_err()
            {
                watchdog_state.store(true, Ordering::SeqCst);
                let _ = watchdog_release.send(());
            }
        });

        let path_to_open = path.clone();
        let open_future = open_required_slot_path(
            "runtime-progress",
            &path,
            RequiredPathType::File,
            move || {
                let _ = opened_tx.send(());
                if release_rx.recv().is_err() {
                    return Err(rustix::io::Errno::INTR);
                }
                open(
                    &path_to_open,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
            },
        );
        let runtime_progress = async {
            let opened = tokio::time::timeout(Duration::from_secs(4), opened_rx).await;
            assert!(
                matches!(opened, Ok(Ok(()))),
                "blocking slot open did not start"
            );
            tokio::task::yield_now().await;
            assert!(
                !watchdog_fired.load(Ordering::SeqCst),
                "blocking slot open stalled the current-thread runtime"
            );
            assert!(release_tx.send(()).is_ok(), "release slot open");
            assert!(
                watchdog_cancel_tx.send(()).is_ok(),
                "cancel slot-open watchdog"
            );
        };

        let (open_result, ()) = tokio::join!(open_future, runtime_progress);
        assert!(watchdog.join().is_ok(), "slot-open watchdog panicked");
        assert!(
            !watchdog_fired.load(Ordering::SeqCst),
            "slot-open watchdog released a blocked runtime"
        );
        open_result?;
        Ok(())
    }

    #[tokio::test]
    async fn sync_artifacts_rederives_canonical_paths_from_slot_id() {
        let temp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(temp.path().to_path_buf());
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "sync-canonical".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        tokio::fs::write(&slot.mem_diff_path, b"dirty-memory")
            .await
            .unwrap();
        tokio::fs::write(&slot.rootfs_diff_path, b"dirty-rootfs")
            .await
            .unwrap();

        let mut forged = slot.clone();
        forged.rootfs_path = PathBuf::from("/must/not/be/opened/rootfs");
        forged.mem_path = PathBuf::from("/must/not/be/opened/memory");
        forged.mem_diff_path = PathBuf::from("/must/not/be/opened/memory-diff");
        forged.rootfs_diff_path = PathBuf::from("/must/not/be/opened/rootfs-diff");
        forged.instance_dir = PathBuf::from("/must/not/be/opened");

        provider
            .sync_artifacts(&forged)
            .await
            .expect("provider uses canonical paths");
    }

    #[tokio::test]
    async fn sync_artifacts_rejects_incomplete_provider_slot() {
        let temp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(temp.path().to_path_buf());
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "sync-incomplete".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        tokio::fs::remove_file(&slot.mem_diff_path).await.unwrap();

        let error = provider
            .sync_artifacts(&slot)
            .await
            .expect_err("missing artifact must fail the sweep item");
        assert!(error.to_string().contains("mem.diff"), "{error}");
    }

    #[tokio::test]
    async fn checkpoint_capture_is_explicit_and_independent() {
        let (_temp, provider, slot, checkpoints) = checkpoint_fixture("capture-independent").await;
        tokio::fs::write(&slot.rootfs_path, b"captured-rootfs")
            .await
            .unwrap();
        let target = checkpoints.join("rootfs.snap");

        assert!(provider.supports_checkpoint_capture());
        provider.capture_checkpoint(&slot, &target).await.unwrap();
        tokio::fs::write(&slot.rootfs_path, b"changed-live-rootfs")
            .await
            .unwrap();

        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"captured-rootfs");
    }

    #[tokio::test]
    async fn checkpoint_capture_does_not_replace_the_live_rootfs() {
        let (_temp, provider, slot, checkpoints) = checkpoint_fixture("capture-read-only").await;
        tokio::fs::write(&slot.rootfs_path, b"live-rootfs")
            .await
            .unwrap();

        provider
            .capture_checkpoint(&slot, &checkpoints.join("rootfs.snap"))
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read(&slot.rootfs_path).await.unwrap(),
            b"live-rootfs"
        );
    }

    #[tokio::test]
    async fn checkpoint_capture_ignores_forged_slot_paths() {
        let (temp, provider, slot, checkpoints) = checkpoint_fixture("capture-canonical").await;
        tokio::fs::write(&slot.rootfs_path, b"canonical-rootfs")
            .await
            .unwrap();
        let forged_source = temp.path().join("forged-rootfs");
        tokio::fs::write(&forged_source, b"forged-rootfs")
            .await
            .unwrap();
        let mut forged = slot.clone();
        forged.rootfs_path = forged_source;
        forged.mem_path = temp.path().join("forged-memory");
        forged.mem_diff_path = temp.path().join("forged-memory-diff");
        forged.rootfs_diff_path = temp.path().join("forged-rootfs-diff");
        forged.instance_dir = temp.path().to_path_buf();
        let target = checkpoints.join("rootfs.snap");

        provider.capture_checkpoint(&forged, &target).await.unwrap();

        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"canonical-rootfs");
    }

    #[tokio::test]
    async fn checkpoint_capture_retains_the_opened_source_artifact() {
        let temp = tempfile::TempDir::new().unwrap();
        let instances = temp.path().join("instances");
        let checkpoints = temp.path().join("checkpoints");
        tokio::fs::create_dir(&instances).await.unwrap();
        tokio::fs::create_dir(&checkpoints).await.unwrap();
        let hook = Arc::new(ArtifactSyncOpenHook::new());
        let provider = Arc::new(FileStorageProvider::with_artifact_sync_open_hook(
            instances.clone(),
            instances,
            Arc::clone(&hook),
        ));
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "capture-source-owner".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        tokio::fs::write(&slot.rootfs_path, b"opened-source")
            .await
            .unwrap();
        let target = checkpoints.join("rootfs.snap");

        let capture_provider = Arc::clone(&provider);
        let capture_slot = slot.clone();
        let capture_target = target.clone();
        let capture = tokio::spawn(async move {
            capture_provider
                .capture_checkpoint(&capture_slot, &capture_target)
                .await
        });
        hook.wait_until_open().await;
        let retained = slot.instance_dir.join("retained-rootfs.ext4");
        tokio::fs::rename(&slot.rootfs_path, &retained)
            .await
            .unwrap();
        tokio::fs::write(&slot.rootfs_path, b"replacement-source")
            .await
            .unwrap();
        hook.resume();

        capture.await.unwrap().unwrap();
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"opened-source");
        assert_eq!(
            tokio::fs::read(&slot.rootfs_path).await.unwrap(),
            b"replacement-source"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn checkpoint_sparse_copy_falls_back_when_extent_seeks_are_unsupported() {
        use std::io::{Read, Seek, Write};
        use std::os::unix::fs::MetadataExt;

        const LOGICAL_LEN: u64 = 64 * 1024 * 1024;
        const FIRST_OFFSET: u64 = 8 * 1024;
        const LAST_OFFSET: u64 = 48 * 1024 * 1024 + 91;
        const FIRST_DATA: &[u8] = b"portable-first-extent";
        const LAST_DATA: &[u8] = b"portable-last-extent";

        let temp = tempfile::tempdir().expect("temp");
        let source_path = temp.path().join("source.img");
        let mut source = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source_path)
            .expect("source");
        source.set_len(LOGICAL_LEN).expect("logical source length");
        source
            .seek(std::io::SeekFrom::Start(FIRST_OFFSET))
            .expect("first offset");
        source.write_all(FIRST_DATA).expect("first data");
        source
            .seek(std::io::SeekFrom::Start(LAST_OFFSET))
            .expect("last offset");
        source.write_all(LAST_DATA).expect("last data");
        source.sync_all().expect("source sync");

        for (name, unsupported) in [
            ("invalid", rustix::io::Errno::INVAL),
            ("not-supported", rustix::io::Errno::NOTSUP),
        ] {
            let target_path = temp.path().join(format!("target-{name}.img"));
            let target = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&target_path)
                .expect("target");
            copy_sparse_file_with_seek(&source, &target, |_, _| Err(unsupported))
                .expect("portable sparse copy");
            target.sync_all().expect("target sync");

            let metadata = target.metadata().expect("target metadata");
            assert_eq!(metadata.len(), LOGICAL_LEN);
            assert!(
                metadata.blocks().saturating_mul(512) < LOGICAL_LEN / 4,
                "fallback allocated {} bytes for a {LOGICAL_LEN}-byte sparse source",
                metadata.blocks().saturating_mul(512)
            );

            let mut captured = std::fs::File::open(&target_path).expect("captured target");
            let mut first = vec![0; FIRST_DATA.len()];
            captured
                .seek(std::io::SeekFrom::Start(FIRST_OFFSET))
                .expect("captured first offset");
            captured
                .read_exact(&mut first)
                .expect("captured first data");
            assert_eq!(first, FIRST_DATA);
            let mut last = vec![0; LAST_DATA.len()];
            captured
                .seek(std::io::SeekFrom::Start(LAST_OFFSET))
                .expect("captured last offset");
            captured.read_exact(&mut last).expect("captured last data");
            assert_eq!(last, LAST_DATA);
            let mut hole = [1_u8; 4096];
            captured
                .seek(std::io::SeekFrom::Start(24 * 1024 * 1024))
                .expect("captured hole offset");
            captured.read_exact(&mut hole).expect("captured hole");
            assert!(hole.iter().all(|byte| *byte == 0));
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn checkpoint_capture_preserves_sparse_extents() {
        use std::io::{Read, Seek, Write};
        use std::os::unix::fs::MetadataExt;

        const LOGICAL_LEN: u64 = 64 * 1024 * 1024;
        const FIRST_OFFSET: u64 = 4 * 1024;
        const LAST_OFFSET: u64 = 48 * 1024 * 1024 + 137;
        const FIRST_DATA: &[u8] = b"first-checkpoint-extent";
        const LAST_DATA: &[u8] = b"last-checkpoint-extent";

        let (_temp, provider, slot, checkpoints) = checkpoint_fixture("capture-sparse").await;
        let mut source = std::fs::OpenOptions::new()
            .write(true)
            .open(&slot.rootfs_path)
            .unwrap();
        source.set_len(LOGICAL_LEN).unwrap();
        source.seek(std::io::SeekFrom::Start(FIRST_OFFSET)).unwrap();
        source.write_all(FIRST_DATA).unwrap();
        source.seek(std::io::SeekFrom::Start(LAST_OFFSET)).unwrap();
        source.write_all(LAST_DATA).unwrap();
        source.sync_all().unwrap();
        let source_blocks = source.metadata().unwrap().blocks();
        drop(source);

        let target = checkpoints.join("rootfs.snap");
        provider.capture_checkpoint(&slot, &target).await.unwrap();

        let metadata = std::fs::metadata(&target).unwrap();
        assert_eq!(metadata.len(), LOGICAL_LEN);
        assert!(
            metadata.blocks().saturating_mul(512) < LOGICAL_LEN / 4,
            "checkpoint allocated {} bytes for a {LOGICAL_LEN}-byte sparse source",
            metadata.blocks().saturating_mul(512)
        );
        assert!(
            metadata.blocks() <= source_blocks.saturating_add(32),
            "checkpoint used {} blocks for a source using {source_blocks} blocks",
            metadata.blocks()
        );

        let mut live = std::fs::OpenOptions::new()
            .write(true)
            .open(&slot.rootfs_path)
            .unwrap();
        live.seek(std::io::SeekFrom::Start(FIRST_OFFSET)).unwrap();
        live.write_all(&[b'x'; FIRST_DATA.len()]).unwrap();
        live.sync_all().unwrap();

        let mut captured = std::fs::File::open(&target).unwrap();
        let mut first = vec![0; FIRST_DATA.len()];
        captured
            .seek(std::io::SeekFrom::Start(FIRST_OFFSET))
            .unwrap();
        captured.read_exact(&mut first).unwrap();
        assert_eq!(first, FIRST_DATA);
        let mut last = vec![0; LAST_DATA.len()];
        captured
            .seek(std::io::SeekFrom::Start(LAST_OFFSET))
            .unwrap();
        captured.read_exact(&mut last).unwrap();
        assert_eq!(last, LAST_DATA);
        let mut hole = [1_u8; 4096];
        captured
            .seek(std::io::SeekFrom::Start(16 * 1024 * 1024))
            .unwrap();
        captured.read_exact(&mut hole).unwrap();
        assert!(hole.iter().all(|byte| *byte == 0));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn checkpoint_capture_preserves_an_all_hole_rootfs() {
        use std::io::{Read, Seek};
        use std::os::unix::fs::MetadataExt;

        const LOGICAL_LEN: u64 = 64 * 1024 * 1024;

        let (_temp, provider, slot, checkpoints) = checkpoint_fixture("capture-all-hole").await;
        let source = std::fs::OpenOptions::new()
            .write(true)
            .open(&slot.rootfs_path)
            .unwrap();
        source.set_len(LOGICAL_LEN).unwrap();
        source.sync_all().unwrap();
        let source_blocks = source.metadata().unwrap().blocks();
        drop(source);

        let target = checkpoints.join("rootfs.snap");
        provider.capture_checkpoint(&slot, &target).await.unwrap();

        let metadata = std::fs::metadata(&target).unwrap();
        assert_eq!(metadata.len(), LOGICAL_LEN);
        assert!(
            metadata.blocks().saturating_mul(512) < LOGICAL_LEN / 16,
            "all-hole checkpoint allocated {} bytes",
            metadata.blocks().saturating_mul(512)
        );
        assert!(
            metadata.blocks() <= source_blocks.saturating_add(8),
            "all-hole checkpoint used {} blocks for a source using {source_blocks} blocks",
            metadata.blocks()
        );

        let mut captured = std::fs::File::open(&target).unwrap();
        let mut zeros = [1_u8; 4096];
        captured
            .seek(std::io::SeekFrom::Start(LOGICAL_LEN / 2))
            .unwrap();
        captured.read_exact(&mut zeros).unwrap();
        assert!(zeros.iter().all(|byte| *byte == 0));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn checkpoint_capture_retains_the_opened_target_directory() {
        use std::os::fd::AsRawFd;

        let temp = tempfile::TempDir::new().unwrap();
        let instances = temp.path().join("instances");
        let checkpoints = temp.path().join("checkpoints");
        tokio::fs::create_dir(&instances).await.unwrap();
        tokio::fs::create_dir(&checkpoints).await.unwrap();
        let checkpoint_owner = open(
            &checkpoints,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let stable_parent =
            PathBuf::from(format!("/proc/self/fd/{}", checkpoint_owner.as_raw_fd()));
        let hook = Arc::new(ArtifactSyncOpenHook::new());
        let provider = Arc::new(FileStorageProvider::with_artifact_sync_open_hook(
            instances.clone(),
            instances,
            Arc::clone(&hook),
        ));
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "capture-target-owner".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        tokio::fs::write(&slot.rootfs_path, b"retained-target")
            .await
            .unwrap();
        let target = stable_parent.join("rootfs.snap");

        let capture_provider = Arc::clone(&provider);
        let capture_slot = slot.clone();
        let capture = tokio::spawn(async move {
            capture_provider
                .capture_checkpoint(&capture_slot, &target)
                .await
        });
        hook.wait_until_open().await;
        let retained = temp.path().join("retained-checkpoints");
        tokio::fs::rename(&checkpoints, &retained).await.unwrap();
        tokio::fs::create_dir(&checkpoints).await.unwrap();
        hook.resume();

        capture.await.unwrap().unwrap();
        assert_eq!(
            tokio::fs::read(retained.join("rootfs.snap")).await.unwrap(),
            b"retained-target"
        );
        assert!(!checkpoints.join("rootfs.snap").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checkpoint_capture_rejects_a_linked_rootfs() {
        use std::os::unix::fs::symlink;

        let (temp, provider, slot, checkpoints) = checkpoint_fixture("capture-linked-source").await;
        tokio::fs::remove_file(&slot.rootfs_path).await.unwrap();
        let external = temp.path().join("external-rootfs");
        tokio::fs::write(&external, b"external").await.unwrap();
        symlink(&external, &slot.rootfs_path).unwrap();
        let target = checkpoints.join("rootfs.snap");

        provider
            .capture_checkpoint(&slot, &target)
            .await
            .expect_err("linked rootfs must not be captured");

        assert!(!target.exists());
        assert_eq!(tokio::fs::read(external).await.unwrap(), b"external");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checkpoint_capture_rejects_a_linked_slot_directory() {
        use std::os::unix::fs::symlink;

        let (temp, provider, slot, checkpoints) = checkpoint_fixture("capture-linked-slot").await;
        tokio::fs::remove_dir_all(&slot.instance_dir).await.unwrap();
        let external = temp.path().join("external-slot");
        tokio::fs::create_dir(&external).await.unwrap();
        tokio::fs::write(external.join("rootfs.ext4"), b"external")
            .await
            .unwrap();
        symlink(&external, &slot.instance_dir).unwrap();
        let target = checkpoints.join("rootfs.snap");

        provider
            .capture_checkpoint(&slot, &target)
            .await
            .expect_err("linked slot directory must be rejected");

        assert!(!target.exists());
        assert_eq!(
            tokio::fs::read(external.join("rootfs.ext4")).await.unwrap(),
            b"external"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checkpoint_capture_rejects_a_linked_target_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let instances = temp.path().join("instances");
        let external = temp.path().join("external-checkpoints");
        tokio::fs::create_dir(&instances).await.unwrap();
        tokio::fs::create_dir(&external).await.unwrap();
        let linked_parent = temp.path().join("linked-checkpoints");
        symlink(&external, &linked_parent).unwrap();
        let provider = FileStorageProvider::new(instances);
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "capture-linked-parent".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        let target = linked_parent.join("rootfs.snap");

        provider
            .capture_checkpoint(&slot, &target)
            .await
            .expect_err("linked target parent must be rejected");

        assert!(!external.join("rootfs.snap").exists());
    }

    #[tokio::test]
    async fn checkpoint_capture_preserves_an_existing_target() {
        let (_temp, provider, slot, checkpoints) =
            checkpoint_fixture("capture-existing-target").await;
        tokio::fs::write(&slot.rootfs_path, b"new-checkpoint")
            .await
            .unwrap();
        let target = checkpoints.join("rootfs.snap");
        tokio::fs::write(&target, b"existing-checkpoint")
            .await
            .unwrap();

        provider
            .capture_checkpoint(&slot, &target)
            .await
            .expect_err("capture must never replace an existing target");

        assert_eq!(
            tokio::fs::read(&target).await.unwrap(),
            b"existing-checkpoint"
        );
    }

    #[test]
    fn unpublished_checkpoint_cleans_target_after_an_unreported_rename() {
        let temp = tempfile::TempDir::new().unwrap();
        let temporary_name = OsString::from("temporary");
        let target_name = OsString::from("target");
        let parent = open(
            temp.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let temporary_file = openat(
            &parent,
            &temporary_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR.union(Mode::WUSR),
        )
        .map(std::fs::File::from)
        .unwrap();
        let mut cleanup = UnpublishedCheckpoint::new(
            parent,
            temporary_file,
            temporary_name.clone(),
            target_name.clone(),
        );
        cleanup.retain_identity().unwrap();
        std::fs::rename(
            temp.path().join(&temporary_name),
            temp.path().join(&target_name),
        )
        .unwrap();

        drop(cleanup);

        assert!(!temp.path().join(&temporary_name).exists());
        assert!(!temp.path().join(&target_name).exists());
    }

    #[test]
    fn unpublished_checkpoint_does_not_remove_a_replacement_target() {
        let temp = tempfile::TempDir::new().unwrap();
        let temporary_name = OsString::from("temporary");
        let target_name = OsString::from("target");
        let retained_name = OsString::from("retained");
        let parent = open(
            temp.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let temporary_file = openat(
            &parent,
            &temporary_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR.union(Mode::WUSR),
        )
        .map(std::fs::File::from)
        .unwrap();
        let mut cleanup = UnpublishedCheckpoint::new(
            parent,
            temporary_file,
            temporary_name.clone(),
            target_name.clone(),
        );
        cleanup.retain_identity().unwrap();
        std::fs::rename(
            temp.path().join(&temporary_name),
            temp.path().join(&target_name),
        )
        .unwrap();
        std::fs::rename(
            temp.path().join(&target_name),
            temp.path().join(&retained_name),
        )
        .unwrap();
        std::fs::write(temp.path().join(&target_name), b"replacement").unwrap();

        drop(cleanup);

        assert_eq!(
            std::fs::read(temp.path().join(&target_name)).unwrap(),
            b"replacement"
        );
        assert!(temp.path().join(&retained_name).exists());
    }

    #[tokio::test]
    async fn checkpoint_capture_does_not_replace_a_racing_target() {
        let temp = tempfile::TempDir::new().unwrap();
        let instances = temp.path().join("instances");
        let checkpoints = temp.path().join("checkpoints");
        tokio::fs::create_dir(&instances).await.unwrap();
        tokio::fs::create_dir(&checkpoints).await.unwrap();
        let hook = Arc::new(ArtifactSyncOpenHook::new());
        let provider = Arc::new(FileStorageProvider::with_artifact_sync_open_hook(
            instances.clone(),
            instances,
            Arc::clone(&hook),
        ));
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "capture-racing-target".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        tokio::fs::write(&slot.rootfs_path, b"new-checkpoint")
            .await
            .unwrap();
        let target = checkpoints.join("rootfs.snap");

        let capture_provider = Arc::clone(&provider);
        let capture_slot = slot.clone();
        let capture_target = target.clone();
        let capture = tokio::spawn(async move {
            capture_provider
                .capture_checkpoint(&capture_slot, &capture_target)
                .await
        });
        hook.wait_until_open().await;
        tokio::fs::write(&target, b"racing-checkpoint")
            .await
            .unwrap();
        hook.resume();

        capture
            .await
            .unwrap()
            .expect_err("capture must not replace a target created after validation");
        assert_eq!(
            tokio::fs::read(&target).await.unwrap(),
            b"racing-checkpoint"
        );
        let mut entries = tokio::fs::read_dir(&checkpoints).await.unwrap();
        assert_eq!(
            entries.next_entry().await.unwrap().unwrap().file_name(),
            "rootfs.snap"
        );
        assert!(entries.next_entry().await.unwrap().is_none());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_checkpoint_capture_finishes_its_blocking_publication() {
        let temp = tempfile::TempDir::new().unwrap();
        let instances = temp.path().join("instances");
        let checkpoints = temp.path().join("checkpoints");
        tokio::fs::create_dir(&instances).await.unwrap();
        tokio::fs::create_dir(&checkpoints).await.unwrap();
        let completion = Arc::new(ArtifactSyncOpenHook::new());
        let provider = Arc::new(FileStorageProvider::with_artifact_sync_open_hook(
            instances.clone(),
            instances,
            Arc::clone(&completion),
        ));
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: "capture-cancelled-publication".into(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .unwrap();
        tokio::fs::write(&slot.rootfs_path, b"complete-checkpoint")
            .await
            .unwrap();
        let target = checkpoints.join("rootfs.snap");
        let hook = crate::failpoint::TestFailpoint::new(&["storage-capture-before-publish"]);

        let capture = tokio::spawn({
            let hook = hook.clone();
            let provider = Arc::clone(&provider);
            let slot = slot.clone();
            let target = target.clone();
            async move { hook.run(provider.capture_checkpoint(&slot, &target)).await }
        });
        completion.wait_until_open().await;
        completion.resume();
        hook.wait_until_paused().await;
        capture.abort();
        assert!(capture.await.unwrap_err().is_cancelled());
        hook.release();

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            completion.wait_until_capture_finished(),
        )
        .await
        .expect("blocking publication transaction must finish after caller cancellation");
        assert_eq!(
            tokio::fs::read(&target).await.unwrap(),
            b"complete-checkpoint"
        );
        let mut entries = tokio::fs::read_dir(&checkpoints).await.unwrap();
        assert_eq!(
            entries.next_entry().await.unwrap().unwrap().file_name(),
            "rootfs.snap"
        );
        assert!(entries.next_entry().await.unwrap().is_none());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_capture_cleans_temporary_data_after_failure() {
        let (_temp, provider, slot, checkpoints) = checkpoint_fixture("capture-cleanup").await;
        tokio::fs::write(&slot.rootfs_path, b"complete-temporary-copy")
            .await
            .unwrap();
        let target = checkpoints.join("rootfs.snap");
        let hook = crate::failpoint::TestFailpoint::new(&["storage-capture-after-publish"]);

        hook.run(provider.capture_checkpoint(&slot, &target))
            .await
            .expect_err("armed capture must roll back its unpublished target");

        assert!(!target.exists());
        assert!(
            tokio::fs::read_dir(&checkpoints)
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .is_none(),
            "capture failure must remove its temporary file"
        );
    }
}
