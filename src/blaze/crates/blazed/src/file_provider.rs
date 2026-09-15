// SPDX-License-Identifier: Apache-2.0
//! File-based storage provider: creates per-instance directories with
//! rootfs and memory files on a local filesystem. Base images and mutable
//! instance slots use separate roots.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rustix::fs::{
    AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags, Stat, flock, fstat, fsync,
    mkdirat, open, openat, renameat_with, statat, unlinkat,
};
use rustix::io::Errno;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use blaze_core::data_plane::DataPlaneLeaseState;
use blaze_core::error::{BlazeError, Result};
use blaze_core::storage::{
    AcquireOpts, OwnedStorageSlot, PoolStatus, StorageAcquireError, StorageOwnershipClaim,
    StorageOwnershipKey, StorageOwnershipLookup, StorageOwnershipPhase, StorageOwnershipRequest,
    StorageProvider, StorageRestoreTransaction, StorageSlot, StorageSlotIdentity, TemplateArtifact,
    TemplateStorage, TemplateStorageSlot,
};

mod restore;

const OWNERSHIP_LEDGER_DIRECTORY: &str = ".blaze-storage-ownership";
const OWNERSHIP_MANIFEST_FORMAT: u32 = 3;
const MAX_OWNERSHIP_MANIFEST_BYTES: u64 = 16 * 1024;
const MAX_SLOT_REMOVAL_DEPTH: usize = 64;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStorageOwnershipManifest {
    format: u32,
    claim: StorageOwnershipClaim,
}

struct StorageOwnershipLedger {
    root: OpenedStorageDirectory,
    directory: OpenedStorageDirectory,
    operations: Mutex<()>,
}

struct OpenedStorageDirectory {
    descriptor: OwnedFd,
    configured_path: PathBuf,
    identity: Stat,
}

/// A filesystem-based provider that copies base artifacts when available and
/// otherwise creates sparse rootfs and memory files at configured sizes.
pub struct FileStorageProvider {
    images_dir: PathBuf,
    instances_dir: PathBuf,
    ownership_ledger: Mutex<Option<Arc<StorageOwnershipLedger>>>,
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
            ownership_ledger: Mutex::new(None),
            artifact_sync_open_hook: None,
        }
    }

    /// Create a provider with distinct immutable image and runtime roots.
    pub fn with_images(images_dir: PathBuf, instances_dir: PathBuf) -> Self {
        Self {
            images_dir,
            instances_dir,
            ownership_ledger: Mutex::new(None),
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
            ownership_ledger: Mutex::new(None),
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

    fn ownership_ledger(&self) -> Result<Arc<StorageOwnershipLedger>> {
        let mut retained = self.ownership_ledger.lock().map_err(|_| {
            storage_error(
                "lock storage ownership ledger",
                &self.instances_dir,
                "the in-process ownership lock is poisoned",
            )
        })?;
        if let Some(ledger) = retained.as_ref() {
            return Ok(ledger.clone());
        }
        let ledger = Arc::new(open_ownership_ledger(&self.instances_dir)?);
        *retained = Some(ledger.clone());
        Ok(ledger)
    }

    fn reject_owned_legacy_release(&self, instance_id: &str) -> Result<()> {
        let Ok(instance_id) = Uuid::parse_str(instance_id) else {
            return Ok(());
        };
        let ledger = self.ownership_ledger()?;
        let _operation = ledger.operations.lock().map_err(|_| {
            ownership_error(
                &instance_id.to_string(),
                "the ownership operation lock is poisoned",
            )
        })?;
        if read_ownership_manifest(&ledger, instance_id)?.is_some() {
            return Err(ownership_error(
                &instance_id.to_string(),
                "request-scoped storage must be removed through its exact lease binding",
            ));
        }
        let slot = self.slot_for_id(&instance_id.to_string())?;
        if open_slot_directory(&ledger.root, &slot)?.is_some() {
            return Err(ownership_error(
                &instance_id.to_string(),
                "a request-scoped slot exists without an ownership record",
            ));
        }
        Ok(())
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

impl OpenedStorageDirectory {
    fn same_object(&self, stat: &Stat) -> bool {
        self.identity.st_dev == stat.st_dev && self.identity.st_ino == stat.st_ino
    }
}

struct UnpublishedOwnership<'a> {
    parent: &'a OwnedFd,
    file: std::fs::File,
    identity: Stat,
    temporary_name: OsString,
    published: bool,
}

impl UnpublishedOwnership<'_> {
    fn file_mut(&mut self) -> &mut std::fs::File {
        &mut self.file
    }

    fn mark_published(&mut self) {
        self.published = true;
    }

    fn linked_at(&self, name: &OsStr) -> std::io::Result<bool> {
        match statat(self.parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                Ok(stat.st_dev == self.identity.st_dev && stat.st_ino == self.identity.st_ino)
            }
            Err(Errno::NOENT) => Ok(false),
            Err(error) => Err(std::io::Error::from(error)),
        }
    }
}

impl Drop for UnpublishedOwnership<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        if self.linked_at(&self.temporary_name).unwrap_or(false)
            && unlinkat(self.parent, &self.temporary_name, AtFlags::empty()).is_ok()
        {
            let _ = fsync(self.parent);
        }
    }
}

fn storage_error(operation: &str, path: &Path, source: impl std::fmt::Display) -> BlazeError {
    BlazeError::StorageError {
        msg: format!("{operation} {}: {source}", path.display()),
    }
}

fn ownership_error(instance_id: &str, message: impl std::fmt::Display) -> BlazeError {
    BlazeError::StorageError {
        msg: format!("storage ownership for '{instance_id}' is not trustworthy: {message}"),
    }
}

fn same_identity(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn same_opened_file(left: &Stat, right: &Stat) -> bool {
    same_identity(left, right) && left.st_size == right.st_size
}

fn slot_identity(stat: &Stat) -> StorageSlotIdentity {
    #[cfg(target_os = "linux")]
    let device = stat.st_dev;
    #[cfg(not(target_os = "linux"))]
    let device = stat.st_dev as u64;
    StorageSlotIdentity {
        device,
        inode: stat.st_ino,
    }
}

fn provider_instance_id(root: &OpenedStorageDirectory) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(b"blaze.file-provider-instance.v1\0");
    hasher.update(storage_domain(root));
    let digest: [u8; 32] = hasher.finalize().into();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 9562 version 8 reserves the payload bits for application-defined
    // deterministic UUIDs. The remaining bits retain the storage-domain hash.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn open_storage_root(path: &Path) -> Result<OpenedStorageDirectory> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| storage_error("open storage root", path, source))?;
    let identity =
        fstat(&descriptor).map_err(|source| storage_error("inspect storage root", path, source))?;
    if identity.st_mode & 0o022 != 0 {
        return Err(storage_error(
            "verify storage root permissions",
            path,
            "group-writable or other-writable instances roots are not allowed",
        ));
    }
    let canonical_path = std::fs::canonicalize(path)
        .map_err(|source| storage_error("canonicalize storage root", path, source))?;
    let canonical_descriptor = open(
        &canonical_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| storage_error("open canonical storage root", &canonical_path, source))?;
    let canonical_identity = fstat(&canonical_descriptor).map_err(|source| {
        storage_error("inspect canonical storage root", &canonical_path, source)
    })?;
    if !same_identity(&identity, &canonical_identity) {
        return Err(storage_error(
            "verify storage root",
            path,
            "configured path and canonical path identify different directories",
        ));
    }
    Ok(OpenedStorageDirectory {
        descriptor,
        configured_path: canonical_path,
        identity,
    })
}

fn open_ownership_ledger(instances_dir: &Path) -> Result<StorageOwnershipLedger> {
    let root = open_storage_root(instances_dir)?;
    if let Err(source) = flock(&root.descriptor, FlockOperation::NonBlockingLockExclusive) {
        let message = if source == Errno::WOULDBLOCK {
            "another cooperating process already owns this instances root".to_string()
        } else {
            source.to_string()
        };
        return Err(storage_error(
            "lock storage ownership root",
            &root.configured_path,
            message,
        ));
    }
    match mkdirat(&root.descriptor, OWNERSHIP_LEDGER_DIRECTORY, Mode::RWXU) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(source) => {
            return Err(storage_error(
                "create storage ownership ledger",
                &root.configured_path.join(OWNERSHIP_LEDGER_DIRECTORY),
                source,
            ));
        }
    }
    let directory = open_child_storage_directory(&root, OsStr::new(OWNERSHIP_LEDGER_DIRECTORY))?;
    if directory.identity.st_mode & 0o077 != 0 {
        return Err(storage_error(
            "verify storage ownership ledger",
            &directory.configured_path,
            "group or other permissions are not allowed",
        ));
    }
    remove_stale_ownership_temporaries(&directory)?;
    fsync(&root.descriptor).map_err(|source| {
        storage_error(
            "sync storage ownership ledger parent",
            &root.configured_path,
            source,
        )
    })?;
    Ok(StorageOwnershipLedger {
        root,
        directory,
        operations: Mutex::new(()),
    })
}

fn storage_domain(root: &OpenedStorageDirectory) -> [u8; 32] {
    let path = root.configured_path.as_os_str().as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(b"blaze.file-storage-domain.v1\0");
    hasher.update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(path);
    hasher.update(root.identity.st_dev.to_be_bytes());
    hasher.update(root.identity.st_ino.to_be_bytes());
    hasher.finalize().into()
}

fn validate_ownership_request(instance_id: &str, request: StorageOwnershipRequest) -> Result<()> {
    let context = request.key.context;
    if request.key.provider_instance_id.is_nil()
        || context.instance_id.is_nil()
        || context.request_id.is_nil()
        || context.operation_id.is_nil()
        || context.lease_id.is_nil()
        || context.generation == 0
        || context.generation.checked_add(4).is_none()
        || request.root_filesystem_bytes == 0
        || request.guest_memory_bytes == 0
        || request.template_vmstate_bytes == Some(0)
        || context.instance_id.to_string() != instance_id
    {
        return Err(ownership_error(
            instance_id,
            "the published request identity or logical extent is invalid",
        ));
    }
    Ok(())
}

fn open_slot_directory(
    root: &OpenedStorageDirectory,
    slot: &StorageSlot,
) -> Result<Option<OpenedStorageDirectory>> {
    let name = OsStr::new(&slot.id);
    let linked = match statat(&root.descriptor, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(None),
        Err(source) => {
            return Err(storage_error(
                "inspect storage slot",
                &slot.instance_dir,
                source,
            ));
        }
    };
    if FileType::from_raw_mode(linked.st_mode as _) != FileType::Directory {
        return Err(BlazeError::StorageIncomplete {
            instance_id: slot.id.clone(),
            path: slot.instance_dir.clone(),
            expected: "plain directory",
        });
    }
    let descriptor = openat(
        &root.descriptor,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| storage_error("open storage slot", &slot.instance_dir, source))?;
    let identity = fstat(&descriptor).map_err(|source| {
        storage_error("inspect opened storage slot", &slot.instance_dir, source)
    })?;
    if !same_identity(&linked, &identity) {
        return Err(ownership_error(
            &slot.id,
            "the slot directory changed identity while it was opened",
        ));
    }
    Ok(Some(OpenedStorageDirectory {
        descriptor,
        configured_path: slot.instance_dir.clone(),
        identity,
    }))
}

fn require_regular_slot_entry(
    directory: &OpenedStorageDirectory,
    slot: &StorageSlot,
    name: &'static str,
    path: &Path,
) -> Result<Stat> {
    let stat =
        statat(&directory.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
            if source == Errno::NOENT {
                BlazeError::StorageIncomplete {
                    instance_id: slot.id.clone(),
                    path: path.to_path_buf(),
                    expected: "plain file",
                }
            } else {
                storage_error("inspect storage artifact", path, source)
            }
        })?;
    if FileType::from_raw_mode(stat.st_mode as _) != FileType::RegularFile {
        return Err(BlazeError::StorageIncomplete {
            instance_id: slot.id.clone(),
            path: path.to_path_buf(),
            expected: "plain file",
        });
    }
    Ok(stat)
}

fn require_logical_length(
    slot: &StorageSlot,
    path: &Path,
    stat: &Stat,
    expected: u64,
) -> Result<()> {
    let actual = u64::try_from(stat.st_size).map_err(|_| {
        ownership_error(
            &slot.id,
            format!("{} has a negative logical length", path.display()),
        )
    })?;
    if actual != expected {
        return Err(ownership_error(
            &slot.id,
            format!(
                "{} has logical length {actual}; expected {expected}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn verify_slot_artifacts(
    directory: &OpenedStorageDirectory,
    slot: &StorageSlot,
    claim: StorageOwnershipClaim,
) -> Result<()> {
    let rootfs = require_regular_slot_entry(directory, slot, "rootfs.ext4", &slot.rootfs_path)?;
    require_logical_length(
        slot,
        &slot.rootfs_path,
        &rootfs,
        claim.request.root_filesystem_bytes,
    )?;
    let memory = require_regular_slot_entry(directory, slot, "mem.bin", &slot.mem_path)?;
    require_logical_length(
        slot,
        &slot.mem_path,
        &memory,
        claim.request.guest_memory_bytes,
    )?;
    require_regular_slot_entry(directory, slot, "mem.diff", &slot.mem_diff_path)?;
    require_regular_slot_entry(directory, slot, "rootfs.diff", &slot.rootfs_diff_path)?;

    if let Some(vmstate_bytes) = claim.request.template_vmstate_bytes {
        let backend = open_child_storage_directory(directory, OsStr::new("backend"))?;
        let vmstate_path = backend.configured_path.join("vmstate.snap");
        let vmstate = require_regular_slot_entry(&backend, slot, "vmstate.snap", &vmstate_path)?;
        require_logical_length(slot, &vmstate_path, &vmstate, vmstate_bytes)?;
        let restore_memory_path = backend.configured_path.join("memory.snap");
        let restore_memory =
            require_regular_slot_entry(&backend, slot, "memory.snap", &restore_memory_path)?;
        require_logical_length(
            slot,
            &restore_memory_path,
            &restore_memory,
            claim.request.guest_memory_bytes,
        )?;
    }
    Ok(())
}

fn ownership_manifest_name(instance_id: Uuid) -> OsString {
    OsString::from(format!("{instance_id}.json"))
}

fn ownership_temporary_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(name) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let Some((instance_id, temporary_id)) = name.split_once(".json.") else {
        return false;
    };
    Uuid::parse_str(instance_id).is_ok_and(|parsed| parsed.to_string() == instance_id)
        && Uuid::parse_str(temporary_id).is_ok_and(|parsed| parsed.to_string() == temporary_id)
}

fn remove_stale_ownership_temporaries(directory: &OpenedStorageDirectory) -> Result<()> {
    let mut removed = false;
    for name in directory_entry_names(directory)? {
        if !ownership_temporary_name(&name) {
            continue;
        }
        let path = directory.configured_path.join(&name);
        let stat = statat(&directory.descriptor, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| storage_error("inspect stale ownership temporary", &path, source))?;
        if FileType::from_raw_mode(stat.st_mode as _) != FileType::RegularFile || stat.st_nlink != 1
        {
            return Err(storage_error(
                "remove stale ownership temporary",
                &path,
                "entry is not a singly linked plain file",
            ));
        }
        unlinkat(&directory.descriptor, &name, AtFlags::empty())
            .map_err(|source| storage_error("remove stale ownership temporary", &path, source))?;
        removed = true;
    }
    if removed {
        fsync(&directory.descriptor).map_err(|source| {
            storage_error(
                "sync stale ownership temporary removal",
                &directory.configured_path,
                source,
            )
        })?;
    }
    Ok(())
}

fn expected_lease_generation(initial: u64, state: DataPlaneLeaseState) -> Option<u64> {
    let offset = match state {
        DataPlaneLeaseState::Prepared => 0,
        DataPlaneLeaseState::Committed => 1,
        DataPlaneLeaseState::Finalized => 2,
        DataPlaneLeaseState::Stopped => 3,
        DataPlaneLeaseState::Released | DataPlaneLeaseState::Quarantined => return None,
    };
    initial.checked_add(offset)
}

fn read_ownership_manifest(
    ledger: &StorageOwnershipLedger,
    instance_id: Uuid,
) -> Result<Option<StorageOwnershipClaim>> {
    let name = ownership_manifest_name(instance_id);
    let path = ledger.directory.configured_path.join(&name);
    let linked = match statat(
        &ledger.directory.descriptor,
        &name,
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(None),
        Err(source) => return Err(storage_error("inspect ownership manifest", &path, source)),
    };
    if FileType::from_raw_mode(linked.st_mode as _) != FileType::RegularFile || linked.st_nlink != 1
    {
        return Err(ownership_error(
            &instance_id.to_string(),
            "the ownership manifest is not a singly linked plain file",
        ));
    }
    let size = u64::try_from(linked.st_size).map_err(|_| {
        ownership_error(
            &instance_id.to_string(),
            "the ownership manifest has an invalid size",
        )
    })?;
    if size == 0 || size > MAX_OWNERSHIP_MANIFEST_BYTES {
        return Err(ownership_error(
            &instance_id.to_string(),
            format!("the ownership manifest has unsupported size {size}"),
        ));
    }
    let descriptor = openat(
        &ledger.directory.descriptor,
        &name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|source| storage_error("open ownership manifest", &path, source))?;
    let opened = fstat(&descriptor)
        .map_err(|source| storage_error("inspect opened ownership manifest", &path, source))?;
    if !same_identity(&linked, &opened) {
        return Err(ownership_error(
            &instance_id.to_string(),
            "the ownership manifest changed identity while it was opened",
        ));
    }
    let mut file = std::fs::File::from(descriptor);
    let mut contents = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    (&mut file)
        .take(MAX_OWNERSHIP_MANIFEST_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|source| storage_error("read ownership manifest", &path, source))?;
    let bytes_read = u64::try_from(contents.len()).map_err(|_| {
        ownership_error(
            &instance_id.to_string(),
            "the ownership manifest is too large",
        )
    })?;
    if bytes_read != size || bytes_read > MAX_OWNERSHIP_MANIFEST_BYTES {
        return Err(ownership_error(
            &instance_id.to_string(),
            "the ownership manifest changed size while it was read",
        ));
    }
    let opened_after = fstat(&file)
        .map_err(|source| storage_error("reinspect opened ownership manifest", &path, source))?;
    if !same_opened_file(&opened, &opened_after) {
        return Err(ownership_error(
            &instance_id.to_string(),
            "the opened ownership manifest changed while it was read",
        ));
    }
    let current = statat(
        &ledger.directory.descriptor,
        &name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|source| storage_error("reinspect ownership manifest", &path, source))?;
    if !same_identity(&opened, &current) {
        return Err(ownership_error(
            &instance_id.to_string(),
            "the ownership manifest changed identity while it was read",
        ));
    }
    let manifest: FileStorageOwnershipManifest = serde_json::from_slice(&contents)
        .map_err(|source| storage_error("decode ownership manifest", &path, source))?;
    if manifest.format != OWNERSHIP_MANIFEST_FORMAT {
        return Err(ownership_error(
            &instance_id.to_string(),
            format!("unsupported manifest format {}", manifest.format),
        ));
    }
    validate_ownership_request(&instance_id.to_string(), manifest.claim.request)?;
    if manifest.claim.storage_domain != storage_domain(&ledger.root) {
        return Err(ownership_error(
            &instance_id.to_string(),
            "the ownership manifest belongs to a different storage domain",
        ));
    }
    if expected_lease_generation(
        manifest.claim.request.key.context.generation,
        manifest.claim.state,
    ) != Some(manifest.claim.generation)
        || (manifest.claim.phase == StorageOwnershipPhase::Preparing
            && (manifest.claim.state != DataPlaneLeaseState::Prepared
                || manifest.claim.generation != manifest.claim.request.key.context.generation))
        || (manifest.claim.phase == StorageOwnershipPhase::Ready
            && manifest.claim.slot_identity.is_none())
    {
        return Err(ownership_error(
            &instance_id.to_string(),
            "the ownership phase, state, or generation is invalid",
        ));
    }
    Ok(Some(manifest.claim))
}

fn verify_ownership_key(claim: StorageOwnershipClaim, key: StorageOwnershipKey) -> Result<()> {
    if claim.request.key == key {
        return Ok(());
    }
    Err(ownership_error(
        &key.context.instance_id.to_string(),
        "the ownership manifest belongs to a different provider request",
    ))
}

fn verify_slot_identity(
    claim: StorageOwnershipClaim,
    directory: &OpenedStorageDirectory,
) -> Result<()> {
    let instance_id = claim.request.key.context.instance_id.to_string();
    let Some(expected) = claim.slot_identity else {
        return Err(ownership_error(
            &instance_id,
            "the ownership record does not identify a concrete slot directory",
        ));
    };
    if slot_identity(&directory.identity) != expected {
        return Err(ownership_error(
            &instance_id,
            "the linked slot directory is not the object recorded by the ownership ledger",
        ));
    }
    Ok(())
}

fn write_ownership_manifest(
    ledger: &StorageOwnershipLedger,
    claim: StorageOwnershipClaim,
    replace: bool,
) -> Result<()> {
    let name = ownership_manifest_name(claim.request.key.context.instance_id);
    let temporary_name = OsString::from(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        Uuid::new_v4()
    ));
    let descriptor = openat(
        &ledger.directory.descriptor,
        &temporary_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|source| {
        storage_error(
            "create ownership manifest temporary file",
            &ledger.directory.configured_path.join(&temporary_name),
            source,
        )
    })?;
    let identity = fstat(&descriptor).map_err(|source| {
        storage_error(
            "inspect ownership manifest temporary file",
            &ledger.directory.configured_path.join(&temporary_name),
            source,
        )
    })?;
    let mut unpublished = UnpublishedOwnership {
        parent: &ledger.directory.descriptor,
        file: std::fs::File::from(descriptor),
        identity,
        temporary_name,
        published: false,
    };
    let mut encoded = serde_json::to_vec(&FileStorageOwnershipManifest {
        format: OWNERSHIP_MANIFEST_FORMAT,
        claim,
    })
    .map_err(|source| {
        ownership_error(
            &claim.request.key.context.instance_id.to_string(),
            format!("encode manifest: {source}"),
        )
    })?;
    encoded.push(b'\n');
    unpublished
        .file_mut()
        .write_all(&encoded)
        .map_err(|source| {
            storage_error(
                "write ownership manifest",
                &ledger.directory.configured_path,
                source,
            )
        })?;
    unpublished.file_mut().sync_all().map_err(|source| {
        storage_error(
            "sync ownership manifest",
            &ledger.directory.configured_path,
            source,
        )
    })?;
    renameat_with(
        &ledger.directory.descriptor,
        &unpublished.temporary_name,
        &ledger.directory.descriptor,
        &name,
        if replace {
            RenameFlags::empty()
        } else {
            RenameFlags::NOREPLACE
        },
    )
    .map_err(|source| {
        storage_error(
            "publish ownership manifest",
            &ledger.directory.configured_path,
            source,
        )
    })?;
    let published = statat(
        &ledger.directory.descriptor,
        &name,
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|source| {
        storage_error(
            "verify published ownership manifest",
            &ledger.directory.configured_path,
            source,
        )
    })?;
    if !same_identity(&unpublished.identity, &published)
        || unpublished
            .linked_at(&unpublished.temporary_name)
            .map_err(|source| {
                storage_error(
                    "verify ownership manifest temporary name",
                    &ledger.directory.configured_path,
                    source,
                )
            })?
    {
        return Err(ownership_error(
            &claim.request.key.context.instance_id.to_string(),
            "the ownership manifest publication result is ambiguous",
        ));
    }
    unpublished.mark_published();
    fsync(&ledger.directory.descriptor).map_err(|source| {
        storage_error(
            "sync storage ownership ledger",
            &ledger.directory.configured_path,
            source,
        )
    })
}

fn reserve_ownership_manifest(
    ledger: &StorageOwnershipLedger,
    request: StorageOwnershipRequest,
) -> Result<StorageOwnershipClaim> {
    let instance_id = request.key.context.instance_id;
    validate_ownership_request(&instance_id.to_string(), request)?;
    let _operation = ledger.operations.lock().map_err(|_| {
        ownership_error(
            &instance_id.to_string(),
            "the ownership operation lock is poisoned",
        )
    })?;
    if let Some(existing) = read_ownership_manifest(ledger, instance_id)? {
        if existing.request != request {
            return Err(ownership_error(
                &instance_id.to_string(),
                "an ownership record already exists for different immutable request facts",
            ));
        }
        if existing.phase == StorageOwnershipPhase::Deleting {
            return Err(ownership_error(
                &instance_id.to_string(),
                "the prior ownership record is still being deleted",
            ));
        }
        return Ok(existing);
    }
    let slot = StorageSlot {
        id: instance_id.to_string(),
        rootfs_path: PathBuf::new(),
        mem_path: PathBuf::new(),
        mem_diff_path: PathBuf::new(),
        rootfs_diff_path: PathBuf::new(),
        instance_dir: ledger.root.configured_path.join(instance_id.to_string()),
    };
    if open_slot_directory(&ledger.root, &slot)?.is_some() {
        return Err(ownership_error(
            &instance_id.to_string(),
            "a same-named slot exists without an ownership record",
        ));
    }
    let claim = StorageOwnershipClaim {
        request,
        storage_domain: storage_domain(&ledger.root),
        slot_identity: None,
        phase: StorageOwnershipPhase::Preparing,
        state: DataPlaneLeaseState::Prepared,
        generation: request.key.context.generation,
    };
    crate::failpoint::storage("storage-ownership-before-reserve")?;
    write_ownership_manifest(ledger, claim, false)?;
    crate::failpoint::storage("storage-ownership-after-reserve")?;
    Ok(claim)
}

struct SlotDirectoryAcquireFailure {
    source: Box<BlazeError>,
    created: bool,
}

impl SlotDirectoryAcquireFailure {
    fn into_acquire_error(self, slot: StorageSlot) -> StorageAcquireError {
        if self.created {
            StorageAcquireError::with_residual(*self.source, slot)
        } else {
            StorageAcquireError::clean(*self.source)
        }
    }
}

fn create_acquired_slot_directory(
    ledger: &StorageOwnershipLedger,
    slot: &StorageSlot,
    operation: &str,
) -> std::result::Result<bool, SlotDirectoryAcquireFailure> {
    let mut created = false;
    let result = (|| -> Result<bool> {
        let _operation = ledger
            .operations
            .lock()
            .map_err(|_| ownership_error(&slot.id, "the ownership operation lock is poisoned"))?;
        let durable_owner = match Uuid::parse_str(&slot.id) {
            Ok(instance_id) if instance_id.to_string() == slot.id => {
                read_ownership_manifest(ledger, instance_id)?
            }
            _ => None,
        };
        if let Some(claim) = durable_owner {
            if claim.phase != StorageOwnershipPhase::Preparing || claim.slot_identity.is_some() {
                return Err(ownership_error(
                    &slot.id,
                    "the ownership reservation does not authorize a new slot directory",
                ));
            }
        }

        crate::failpoint::storage("storage-acquire-before-mkdir")?;
        match mkdirat(&ledger.root.descriptor, OsStr::new(&slot.id), Mode::RWXU) {
            Ok(()) => created = true,
            Err(Errno::EXIST) => {
                return Err(BlazeError::StorageError {
                    msg: format!(
                        "{operation} '{}': instance directory already exists",
                        slot.id
                    ),
                });
            }
            Err(source) => {
                return Err(BlazeError::StorageError {
                    msg: format!("{operation} '{}': create dir: {source}", slot.id),
                });
            }
        }
        fsync(&ledger.root.descriptor).map_err(|source| {
            storage_error(
                "sync acquired storage directory",
                &ledger.root.configured_path,
                source,
            )
        })?;
        // This fault boundary intentionally precedes durable directory
        // identity publication. Recovery must retain the ambiguous directory,
        // never infer ownership from its pathname.
        crate::failpoint::storage("storage-acquire-after-mkdir")?;
        let directory = open_slot_directory(&ledger.root, slot)?.ok_or_else(|| {
            ownership_error(&slot.id, "the newly created slot directory disappeared")
        })?;
        linked_directory_is_current(&ledger.root, OsStr::new(&slot.id), &directory)?;
        if let Some(claim) = durable_owner {
            let identified = StorageOwnershipClaim {
                slot_identity: Some(slot_identity(&directory.identity)),
                ..claim
            };
            crate::failpoint::storage("storage-ownership-before-slot-identity")?;
            write_ownership_manifest(ledger, identified, true)?;
            crate::failpoint::storage("storage-ownership-after-slot-identity")?;
            linked_directory_is_current(&ledger.root, OsStr::new(&slot.id), &directory)?;
        }
        Ok(durable_owner.is_some())
    })();
    match result {
        Ok(owned) => Ok(owned),
        Err(source) => Err(SlotDirectoryAcquireFailure {
            source: Box::new(source),
            created,
        }),
    }
}

fn publish_ready_ownership_manifest(
    ledger: &StorageOwnershipLedger,
    slot: &StorageSlot,
    request: StorageOwnershipRequest,
) -> Result<StorageOwnershipClaim> {
    validate_ownership_request(&slot.id, request)?;
    let _operation = ledger
        .operations
        .lock()
        .map_err(|_| ownership_error(&slot.id, "the ownership operation lock is poisoned"))?;
    let current =
        read_ownership_manifest(ledger, request.key.context.instance_id)?.ok_or_else(|| {
            ownership_error(&slot.id, "the write-ahead ownership reservation is missing")
        })?;
    if current.request != request {
        return Err(ownership_error(
            &slot.id,
            "the write-ahead ownership reservation belongs to a different request",
        ));
    }
    if !matches!(
        current.phase,
        StorageOwnershipPhase::Preparing | StorageOwnershipPhase::Ready
    ) {
        return Err(ownership_error(
            &slot.id,
            "the ownership record is being deleted",
        ));
    }
    let directory =
        open_slot_directory(&ledger.root, slot)?.ok_or_else(|| BlazeError::StorageIncomplete {
            instance_id: slot.id.clone(),
            path: slot.instance_dir.clone(),
            expected: "plain directory",
        })?;
    verify_slot_identity(current, &directory)?;
    verify_slot_artifacts(&directory, slot, current)?;
    linked_directory_is_current(&ledger.root, OsStr::new(&slot.id), &directory)?;
    if current.phase == StorageOwnershipPhase::Ready {
        return Ok(current);
    }
    let ready = StorageOwnershipClaim {
        phase: StorageOwnershipPhase::Ready,
        ..current
    };
    crate::failpoint::storage("storage-ownership-before-ready")?;
    write_ownership_manifest(ledger, ready, true)?;
    crate::failpoint::storage("storage-ownership-after-ready")?;
    Ok(ready)
}

fn reconstruct_owned_slot(
    ledger: &StorageOwnershipLedger,
    slot: StorageSlot,
    key: StorageOwnershipKey,
) -> Result<Option<OwnedStorageSlot>> {
    let _operation = ledger
        .operations
        .lock()
        .map_err(|_| ownership_error(&slot.id, "the ownership operation lock is poisoned"))?;
    let Some(claim) = read_ownership_manifest(ledger, key.context.instance_id)? else {
        if open_slot_directory(&ledger.root, &slot)?.is_some() {
            return Err(ownership_error(
                &slot.id,
                "a same-named slot exists without an ownership record",
            ));
        }
        return Ok(None);
    };
    verify_ownership_key(claim, key)?;
    verify_claim_slot(ledger, &slot, claim)?;
    Ok(Some(OwnedStorageSlot {
        storage: slot,
        ownership: claim,
    }))
}

fn verify_claim_slot(
    ledger: &StorageOwnershipLedger,
    slot: &StorageSlot,
    claim: StorageOwnershipClaim,
) -> Result<()> {
    match open_slot_directory(&ledger.root, slot)? {
        Some(directory) => {
            verify_slot_identity(claim, &directory)?;
            linked_directory_is_current(&ledger.root, OsStr::new(&slot.id), &directory)?;
            if claim.phase == StorageOwnershipPhase::Ready {
                verify_slot_artifacts(&directory, slot, claim)?;
            }
            Ok(())
        }
        None if claim.phase != StorageOwnershipPhase::Ready => Ok(()),
        None => Err(BlazeError::StorageIncomplete {
            instance_id: slot.id.clone(),
            path: slot.instance_dir.clone(),
            expected: "owned slot directory",
        }),
    }
}

fn ownership_manifest_instance_id(name: &OsStr) -> Option<Uuid> {
    let text = name.to_str()?.strip_suffix(".json")?;
    Uuid::parse_str(text)
        .ok()
        .filter(|parsed| parsed.to_string() == text)
}

fn lookup_owned_slot(
    ledger: &StorageOwnershipLedger,
    slot: StorageSlot,
    key: StorageOwnershipKey,
) -> Result<StorageOwnershipLookup> {
    let _operation = ledger
        .operations
        .lock()
        .map_err(|_| ownership_error(&slot.id, "the ownership operation lock is poisoned"))?;
    let mut exact = None;
    let mut conflict = false;
    for name in directory_entry_names(&ledger.directory)? {
        let Some(instance_id) = ownership_manifest_instance_id(&name) else {
            continue;
        };
        let claim = read_ownership_manifest(ledger, instance_id)?.ok_or_else(|| {
            ownership_error(
                &slot.id,
                format!("ownership manifest {instance_id} vanished during the ledger scan"),
            )
        })?;
        let candidate = claim.request.key;
        if candidate == key {
            if exact.replace(claim).is_some() {
                return Err(ownership_error(
                    &slot.id,
                    "the ownership ledger contains duplicate exact records",
                ));
            }
        } else if candidate.context.instance_id == key.context.instance_id
            || candidate.context.lease_id == key.context.lease_id
        {
            conflict = true;
        }
    }
    if conflict {
        return Ok(StorageOwnershipLookup::Conflict);
    }
    if let Some(claim) = exact {
        verify_claim_slot(ledger, &slot, claim)?;
        return Ok(StorageOwnershipLookup::Owned(Box::new(OwnedStorageSlot {
            storage: slot,
            ownership: claim,
        })));
    }
    if open_slot_directory(&ledger.root, &slot)?.is_some() {
        return Err(ownership_error(
            &slot.id,
            "a same-named slot exists without an ownership record",
        ));
    }
    Ok(StorageOwnershipLookup::Absent)
}

fn lease_transition_is_valid(expected: DataPlaneLeaseState, next: DataPlaneLeaseState) -> bool {
    matches!(
        (expected, next),
        (
            DataPlaneLeaseState::Prepared,
            DataPlaneLeaseState::Committed
        ) | (
            DataPlaneLeaseState::Committed,
            DataPlaneLeaseState::Finalized
        ) | (DataPlaneLeaseState::Finalized, DataPlaneLeaseState::Stopped)
    )
}

fn advance_ownership_manifest(
    ledger: &StorageOwnershipLedger,
    key: StorageOwnershipKey,
    expected_state: DataPlaneLeaseState,
    expected_generation: u64,
    next_state: DataPlaneLeaseState,
    next_generation: u64,
) -> Result<StorageOwnershipClaim> {
    let instance_id = key.context.instance_id;
    let _operation = ledger.operations.lock().map_err(|_| {
        ownership_error(
            &instance_id.to_string(),
            "the ownership operation lock is poisoned",
        )
    })?;
    let current = read_ownership_manifest(ledger, instance_id)?.ok_or_else(|| {
        ownership_error(&instance_id.to_string(), "the ownership record is missing")
    })?;
    verify_ownership_key(current, key)?;
    if current.phase != StorageOwnershipPhase::Ready
        || current.state != expected_state
        || current.generation != expected_generation
        || !lease_transition_is_valid(expected_state, next_state)
        || expected_generation.checked_add(1) != Some(next_generation)
    {
        return Err(ownership_error(
            &instance_id.to_string(),
            "the durable lease state does not authorize the requested transition",
        ));
    }
    let next = StorageOwnershipClaim {
        state: next_state,
        generation: next_generation,
        ..current
    };
    crate::failpoint::storage("storage-ownership-before-state-update")?;
    write_ownership_manifest(ledger, next, true)?;
    crate::failpoint::storage("storage-ownership-after-state-update")?;
    Ok(next)
}

fn linked_directory_is_current(
    parent: &OpenedStorageDirectory,
    name: &OsStr,
    directory: &OpenedStorageDirectory,
) -> Result<()> {
    let linked = statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
        storage_error(
            "verify owned storage directory",
            &directory.configured_path,
            source,
        )
    })?;
    if FileType::from_raw_mode(linked.st_mode as _) != FileType::Directory
        || !directory.same_object(&linked)
    {
        return Err(storage_error(
            "verify owned storage directory",
            &directory.configured_path,
            "directory identity changed before removal",
        ));
    }
    Ok(())
}

fn directory_entry_names(directory: &OpenedStorageDirectory) -> Result<Vec<OsString>> {
    let scan = openat(
        &directory.descriptor,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| {
        storage_error(
            "open storage directory scan",
            &directory.configured_path,
            source,
        )
    })?;
    let entries = Dir::read_from(&scan).map_err(|source| {
        storage_error("scan storage directory", &directory.configured_path, source)
    })?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            storage_error("read storage directory", &directory.configured_path, source)
        })?;
        let bytes = entry.file_name().to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsStr::from_bytes(bytes).to_os_string());
        }
    }
    names.sort();
    Ok(names)
}

fn open_child_storage_directory(
    parent: &OpenedStorageDirectory,
    name: &OsStr,
) -> Result<OpenedStorageDirectory> {
    let path = parent.configured_path.join(name);
    let linked = statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|source| storage_error("inspect storage child directory", &path, source))?;
    if FileType::from_raw_mode(linked.st_mode as _) != FileType::Directory {
        return Err(storage_error(
            "open storage child directory",
            &path,
            "entry is no longer a directory",
        ));
    }
    let descriptor = openat(
        &parent.descriptor,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| storage_error("open storage child directory", &path, source))?;
    let identity = fstat(&descriptor)
        .map_err(|source| storage_error("inspect opened storage child directory", &path, source))?;
    if !same_identity(&linked, &identity) {
        return Err(storage_error(
            "open storage child directory",
            &path,
            "directory identity changed while it was opened",
        ));
    }
    Ok(OpenedStorageDirectory {
        descriptor,
        configured_path: path,
        identity,
    })
}

fn remove_owned_slot_directory(
    parent: &OpenedStorageDirectory,
    name: &OsStr,
    directory: OpenedStorageDirectory,
    depth: usize,
) -> Result<()> {
    if depth > MAX_SLOT_REMOVAL_DEPTH {
        return Err(storage_error(
            "remove owned storage directory",
            &directory.configured_path,
            "directory nesting exceeds the safety limit",
        ));
    }
    linked_directory_is_current(parent, name, &directory)?;
    for entry in directory_entry_names(&directory)? {
        let path = directory.configured_path.join(&entry);
        let stat = statat(&directory.descriptor, &entry, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| storage_error("inspect owned storage entry", &path, source))?;
        crate::failpoint::storage("storage-release-during-slot-remove")?;
        if FileType::from_raw_mode(stat.st_mode as _) == FileType::Directory {
            let child = open_child_storage_directory(&directory, &entry)?;
            remove_owned_slot_directory(&directory, &entry, child, depth + 1)?;
        } else {
            unlinkat(&directory.descriptor, &entry, AtFlags::empty())
                .map_err(|source| storage_error("remove owned storage entry", &path, source))?;
        }
    }
    fsync(&directory.descriptor).map_err(|source| {
        storage_error(
            "sync removed storage directory",
            &directory.configured_path,
            source,
        )
    })?;
    linked_directory_is_current(parent, name, &directory)?;
    unlinkat(&parent.descriptor, name, AtFlags::REMOVEDIR).map_err(|source| {
        storage_error(
            "remove owned storage directory",
            &directory.configured_path,
            source,
        )
    })?;
    fsync(&parent.descriptor).map_err(|source| {
        storage_error(
            "sync storage directory removal",
            &parent.configured_path,
            source,
        )
    })
}

fn release_legacy_slot_by_id(ledger: &StorageOwnershipLedger, slot: StorageSlot) -> Result<()> {
    let _operation = ledger
        .operations
        .lock()
        .map_err(|_| ownership_error(&slot.id, "the ownership operation lock is poisoned"))?;
    if let Ok(instance_id) = Uuid::parse_str(&slot.id)
        && read_ownership_manifest(ledger, instance_id)?.is_some()
    {
        return Err(ownership_error(
            &slot.id,
            "request-scoped storage must be removed through its exact lease binding",
        ));
    }
    let Some(directory) = open_slot_directory(&ledger.root, &slot)? else {
        return Ok(());
    };
    // Records written before request-scoped ownership do not have a storage
    // manifest. Recovery is nevertheless authorized by the accepted lifecycle
    // record. Keep that compatibility path separate from `release`, and bind
    // recursive removal to the directory object opened under the configured
    // storage root so a path replacement cannot redirect cleanup.
    remove_owned_slot_directory(&ledger.root, OsStr::new(&slot.id), directory, 0)
}

fn release_owned_slot(
    ledger: &StorageOwnershipLedger,
    slot: StorageSlot,
    key: StorageOwnershipKey,
    expected_state: DataPlaneLeaseState,
    expected_generation: u64,
) -> Result<bool> {
    let _operation = ledger
        .operations
        .lock()
        .map_err(|_| ownership_error(&slot.id, "the ownership operation lock is poisoned"))?;
    let Some(current) = read_ownership_manifest(ledger, key.context.instance_id)? else {
        if open_slot_directory(&ledger.root, &slot)?.is_some() {
            return Err(ownership_error(
                &slot.id,
                "a same-named slot exists without an ownership record",
            ));
        }
        fsync(&ledger.root.descriptor).map_err(|source| {
            storage_error(
                "sync absent storage slot parent",
                &ledger.root.configured_path,
                source,
            )
        })?;
        fsync(&ledger.directory.descriptor).map_err(|source| {
            storage_error(
                "sync absent storage ownership record",
                &ledger.directory.configured_path,
                source,
            )
        })?;
        return Ok(false);
    };
    verify_ownership_key(current, key)?;
    if current.state != expected_state || current.generation != expected_generation {
        return Err(ownership_error(
            &slot.id,
            "the durable lease state does not authorize removal",
        ));
    }
    if let Some(directory) = open_slot_directory(&ledger.root, &slot)? {
        verify_slot_identity(current, &directory)?;
        linked_directory_is_current(&ledger.root, OsStr::new(&slot.id), &directory)?;
    }
    if current.phase != StorageOwnershipPhase::Deleting {
        let deleting = StorageOwnershipClaim {
            phase: StorageOwnershipPhase::Deleting,
            ..current
        };
        crate::failpoint::storage("storage-release-before-mark-deleting")?;
        write_ownership_manifest(ledger, deleting, true)?;
        crate::failpoint::storage("storage-release-after-mark-deleting")?;
    }

    if let Some(directory) = open_slot_directory(&ledger.root, &slot)? {
        verify_slot_identity(current, &directory)?;
        remove_owned_slot_directory(&ledger.root, OsStr::new(&slot.id), directory, 0)?;
    }
    if open_slot_directory(&ledger.root, &slot)?.is_some() {
        return Err(ownership_error(
            &slot.id,
            "the slot remained linked after recursive removal",
        ));
    }
    fsync(&ledger.root.descriptor).map_err(|source| {
        storage_error(
            "sync empty storage slot parent",
            &ledger.root.configured_path,
            source,
        )
    })?;
    crate::failpoint::storage("storage-release-after-slot-remove")?;

    let manifest_name = ownership_manifest_name(key.context.instance_id);
    unlinkat(
        &ledger.directory.descriptor,
        &manifest_name,
        AtFlags::empty(),
    )
    .map_err(|source| {
        storage_error(
            "remove storage ownership manifest",
            &ledger.directory.configured_path.join(&manifest_name),
            source,
        )
    })?;
    fsync(&ledger.directory.descriptor).map_err(|source| {
        storage_error(
            "sync removed storage ownership manifest",
            &ledger.directory.configured_path,
            source,
        )
    })?;
    crate::failpoint::storage("storage-release-after-ledger-remove")?;
    Ok(true)
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
    fn ownership_domain_id(&self) -> Result<Option<Uuid>> {
        let ledger = self.ownership_ledger()?;
        Ok(Some(provider_instance_id(&ledger.root)))
    }

    async fn probe(&self) -> Result<bool> {
        if !self.images_dir.exists() || !self.instances_dir.exists() {
            return Ok(false);
        }
        self.ownership_ledger()?;
        Ok(true)
    }

    async fn acquire(
        &self,
        opts: &AcquireOpts,
    ) -> std::result::Result<StorageSlot, StorageAcquireError> {
        crate::failpoint::storage("storage-acquire")?;
        let slot = self.slot_for_id(&opts.instance_id)?;
        let instance_dir = slot.instance_dir.clone();

        let ledger = self.ownership_ledger()?;
        let durably_owned = create_acquired_slot_directory(&ledger, &slot, "acquire")
            .map_err(|failure| failure.into_acquire_error(slot.clone()))?;

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
            if durably_owned {
                return Err(StorageAcquireError::with_residual(e, slot));
            }
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

        let ledger = self.ownership_ledger()?;
        let durably_owned = create_acquired_slot_directory(&ledger, &slot, "acquire template")
            .map_err(|failure| failure.into_acquire_error(slot.clone()))?;

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
            if durably_owned {
                return Err(StorageAcquireError::with_residual(error, slot));
            }
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
        self.reject_owned_legacy_release(&slot.id)?;
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
        crate::failpoint::storage("storage-release")?;
        let slot = self.slot_for_id(instance_id)?;
        let ledger = self.ownership_ledger()?;
        crate::failpoint::spawn_blocking(move || release_legacy_slot_by_id(&ledger, slot))
            .await
            .map_err(|source| BlazeError::StorageError {
                msg: format!("release legacy storage task failed: {source}"),
            })?
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

    async fn reserve_ownership(
        &self,
        request: StorageOwnershipRequest,
    ) -> Result<StorageOwnershipClaim> {
        let ledger = self.ownership_ledger()?;
        crate::failpoint::spawn_blocking(move || reserve_ownership_manifest(&ledger, request))
            .await
            .map_err(|source| BlazeError::StorageError {
                msg: format!("reserve storage ownership task failed: {source}"),
            })?
    }

    async fn publish_ownership(
        &self,
        slot: &StorageSlot,
        request: StorageOwnershipRequest,
    ) -> Result<StorageOwnershipClaim> {
        let canonical = self.slot_for_id(&slot.id)?;
        if canonical != *slot {
            return Err(ownership_error(
                &slot.id,
                "the runtime storage slot does not match the configured storage root",
            ));
        }
        self.sync_artifacts(&canonical).await?;
        let ledger = self.ownership_ledger()?;
        crate::failpoint::spawn_blocking(move || {
            publish_ready_ownership_manifest(&ledger, &canonical, request)
        })
        .await
        .map_err(|source| BlazeError::StorageError {
            msg: format!("publish storage ownership task failed: {source}"),
        })?
    }

    async fn advance_ownership(
        &self,
        key: StorageOwnershipKey,
        expected_state: DataPlaneLeaseState,
        expected_generation: u64,
        next_state: DataPlaneLeaseState,
        next_generation: u64,
    ) -> Result<StorageOwnershipClaim> {
        let ledger = self.ownership_ledger()?;
        crate::failpoint::spawn_blocking(move || {
            advance_ownership_manifest(
                &ledger,
                key,
                expected_state,
                expected_generation,
                next_state,
                next_generation,
            )
        })
        .await
        .map_err(|source| BlazeError::StorageError {
            msg: format!("advance storage ownership task failed: {source}"),
        })?
    }

    async fn reconstruct_owned(
        &self,
        key: StorageOwnershipKey,
    ) -> Result<Option<OwnedStorageSlot>> {
        let slot = self.slot_for_id(&key.context.instance_id.to_string())?;
        let ledger = self.ownership_ledger()?;
        crate::failpoint::spawn_blocking(move || reconstruct_owned_slot(&ledger, slot, key))
            .await
            .map_err(|source| BlazeError::StorageError {
                msg: format!("reconstruct owned storage task failed: {source}"),
            })?
    }

    async fn lookup_ownership(&self, key: StorageOwnershipKey) -> Result<StorageOwnershipLookup> {
        let slot = self.slot_for_id(&key.context.instance_id.to_string())?;
        let ledger = self.ownership_ledger()?;
        crate::failpoint::spawn_blocking(move || lookup_owned_slot(&ledger, slot, key))
            .await
            .map_err(|source| BlazeError::StorageError {
                msg: format!("lookup owned storage task failed: {source}"),
            })?
    }

    async fn release_owned(
        &self,
        key: StorageOwnershipKey,
        expected_state: DataPlaneLeaseState,
        expected_generation: u64,
    ) -> Result<bool> {
        crate::failpoint::storage("storage-release")?;
        let slot = self.slot_for_id(&key.context.instance_id.to_string())?;
        let ledger = self.ownership_ledger()?;
        crate::failpoint::spawn_blocking(move || {
            release_owned_slot(&ledger, slot, key, expected_state, expected_generation)
        })
        .await
        .map_err(|source| BlazeError::StorageError {
            msg: format!("release owned storage task failed: {source}"),
        })?
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
        let source_len = tokio::fs::metadata(source).await?.len();
        if source_len != size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "base image {} has logical length {source_len}; requested contract requires {size}",
                    source.display()
                ),
            ));
        }
        let copied = tokio::fs::copy(source, target).await?;
        let target_len = tokio::fs::metadata(target).await?.len();
        if copied != size || target_len != size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "base image copy {} -> {} produced {target_len} bytes; expected {size}",
                    source.display(),
                    target.display()
                ),
            ));
        }
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

    fn ownership_request(
        provider: &FileStorageProvider,
        instance_id: Uuid,
        template_vmstate_bytes: Option<u64>,
    ) -> StorageOwnershipRequest {
        StorageOwnershipRequest {
            key: StorageOwnershipKey {
                provider_instance_id: provider
                    .ownership_domain_id()
                    .expect("storage domain")
                    .expect("file storage domain"),
                context: blaze_core::data_plane::DataPlaneRequestContextRecord {
                    instance_id,
                    request_id: Uuid::new_v4(),
                    operation_id: Uuid::new_v4(),
                    lease_id: Uuid::new_v4(),
                    generation: 1,
                },
            },
            root_filesystem_bytes: 64,
            guest_memory_bytes: 32,
            source_fingerprint: [7; 32],
            template_vmstate_bytes,
        }
    }

    fn template_artifact(path: &Path, contents: &[u8]) -> TemplateArtifact {
        std::fs::write(path, contents).expect("template artifact");
        let mut digest = Sha256::new();
        digest.update(contents);
        TemplateArtifact {
            file: std::fs::File::open(path).expect("open template artifact"),
            size_bytes: contents.len() as u64,
            sha256: format!("{:x}", digest.finalize()),
        }
    }

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

    #[tokio::test]
    async fn acquire_rejects_a_base_image_whose_length_disagrees_with_the_contract() {
        let temp = tempfile::TempDir::new().unwrap();
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir(&images).expect("images");
        std::fs::create_dir(&instances).expect("instances");
        std::fs::write(images.join("rootfs.ext4"), vec![0_u8; 63]).expect("base rootfs");
        let provider = FileStorageProvider::with_images(images, instances.clone());

        let error = provider
            .acquire(&AcquireOpts {
                instance_id: "wrong-base-length".to_string(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .expect_err("mismatched base length");

        assert!(error.to_string().contains("requires 64"), "{error}");
        assert!(!instances.join("wrong-base-length").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_rejects_a_group_or_other_writable_instances_root() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::TempDir::new().unwrap();
        let instances = temp.path().join("instances");
        std::fs::create_dir(&instances).expect("instances");
        std::fs::set_permissions(&instances, std::fs::Permissions::from_mode(0o777))
            .expect("writable permissions");
        let provider = FileStorageProvider::new(instances.clone());

        let error = provider
            .probe()
            .await
            .expect_err("unsafe instances root permissions");

        assert!(error.to_string().contains("group-writable"), "{error}");
        assert!(!instances.join(OWNERSHIP_LEDGER_DIRECTORY).exists());
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

    #[tokio::test]
    async fn release_by_id_removes_a_legacy_uuid_slot_without_an_ownership_record() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        let instance_id = Uuid::new_v4().to_string();
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: instance_id.clone(),
                rootfs_size: 1024,
                mem_size: 512,
            })
            .await
            .expect("legacy storage slot");

        provider
            .release_by_id(&instance_id)
            .await
            .expect("legacy recovery release");

        assert!(!slot.instance_dir.exists());
    }

    #[tokio::test]
    async fn release_by_id_never_bypasses_request_scoped_ownership() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        let instance_id = Uuid::new_v4();
        let request = ownership_request(&provider, instance_id, None);
        provider
            .reserve_ownership(request)
            .await
            .expect("reserve owner");
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: instance_id.to_string(),
                rootfs_size: 1024,
                mem_size: 512,
            })
            .await
            .expect("request-scoped storage slot");

        let error = provider
            .release_by_id(&instance_id.to_string())
            .await
            .expect_err("request-scoped storage requires its exact lease binding");

        assert!(error.to_string().contains("exact lease binding"));
        assert!(slot.instance_dir.is_dir());
    }

    #[tokio::test]
    async fn ownership_ledger_never_deletes_an_unrecorded_request_scoped_slot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        let instance_id = Uuid::new_v4().to_string();
        let slot = provider.slot_for_id(&instance_id).expect("canonical slot");
        std::fs::create_dir(&slot.instance_dir).expect("unrecorded slot");

        let error = provider
            .release(slot.clone())
            .await
            .expect_err("an unrecorded request-scoped slot must fail closed");

        assert!(error.to_string().contains("without an ownership record"));
        assert!(slot.instance_dir.is_dir());
    }

    #[tokio::test]
    async fn preparing_owner_never_deletes_a_directory_that_won_the_name_before_mkdir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        let instance_id = Uuid::new_v4();
        let request = ownership_request(&provider, instance_id, None);
        provider
            .reserve_ownership(request)
            .await
            .expect("reserve owner");
        let foreign = tmp.path().join(instance_id.to_string());
        std::fs::create_dir(&foreign).expect("foreign colliding directory");
        std::fs::write(foreign.join("operator-data"), b"retain").expect("foreign data");

        let error = provider
            .acquire(&AcquireOpts {
                instance_id: instance_id.to_string(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .expect_err("directory collision");
        assert!(error.to_string().contains("already exists"), "{error}");
        let cleanup = provider
            .release_owned(
                request.key,
                DataPlaneLeaseState::Prepared,
                request.key.context.generation,
            )
            .await
            .expect_err("unidentified directory must not be removed");
        assert!(
            cleanup.to_string().contains("concrete slot directory"),
            "{cleanup}"
        );
        assert_eq!(
            std::fs::read(foreign.join("operator-data")).expect("retained data"),
            b"retain"
        );
    }

    #[tokio::test]
    async fn ready_owner_rejects_a_replacement_directory_before_inspection_or_deletion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let provider = FileStorageProvider::new(tmp.path().to_path_buf());
        let instance_id = Uuid::new_v4();
        let request = ownership_request(&provider, instance_id, None);
        provider
            .reserve_ownership(request)
            .await
            .expect("reserve owner");
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: instance_id.to_string(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .expect("acquire slot");
        let ready = provider
            .publish_ownership(&slot, request)
            .await
            .expect("publish owner");
        let retained_original = tmp.path().join("retained-original");
        std::fs::rename(&slot.instance_dir, &retained_original).expect("retain original");
        std::fs::create_dir(&slot.instance_dir).expect("replacement directory");
        std::fs::write(slot.instance_dir.join("foreign"), b"retain").expect("foreign data");

        assert!(provider.reconstruct_owned(request.key).await.is_err());
        assert!(
            provider
                .release_owned(request.key, ready.state, ready.generation)
                .await
                .is_err()
        );
        assert!(retained_original.is_dir());
        assert_eq!(
            std::fs::read(slot.instance_dir.join("foreign")).expect("foreign data retained"),
            b"retain"
        );
    }

    #[tokio::test]
    async fn ready_owner_validates_lengths_and_template_restore_payload() {
        let temp = tempfile::TempDir::new().unwrap();
        let instances = temp.path().join("instances");
        let sources = temp.path().join("sources");
        std::fs::create_dir(&instances).expect("instances");
        std::fs::create_dir(&sources).expect("sources");
        let provider = FileStorageProvider::new(instances);
        let instance_id = Uuid::new_v4();
        let vmstate = b"vm-state";
        let request = ownership_request(&provider, instance_id, Some(vmstate.len() as u64));
        provider
            .reserve_ownership(request)
            .await
            .expect("reserve owner");
        let materialized = provider
            .acquire_template(
                &AcquireOpts {
                    instance_id: instance_id.to_string(),
                    rootfs_size: 64,
                    mem_size: 32,
                },
                TemplateStorage {
                    vmstate: template_artifact(&sources.join("vmstate"), vmstate),
                    memory: template_artifact(&sources.join("memory"), &[2_u8; 32]),
                    rootfs: template_artifact(&sources.join("rootfs"), &[3_u8; 64]),
                },
            )
            .await
            .expect("materialize template");
        provider
            .publish_ownership(&materialized.storage, request)
            .await
            .expect("publish owner");

        std::fs::remove_file(materialized.payload_dir.join("vmstate.snap"))
            .expect("remove restore VM state");
        let missing_payload = provider
            .reconstruct_owned(request.key)
            .await
            .expect_err("missing restore payload");
        assert!(missing_payload.to_string().contains("vmstate.snap"));

        std::fs::write(materialized.payload_dir.join("vmstate.snap"), vmstate)
            .expect("restore VM state");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&materialized.storage.rootfs_path)
            .expect("open rootfs")
            .set_len(63)
            .expect("change rootfs length");
        let wrong_length = provider
            .reconstruct_owned(request.key)
            .await
            .expect_err("wrong logical length");
        assert!(wrong_length.to_string().contains("expected 64"));
    }

    #[test]
    fn ownership_ledger_removes_only_its_stale_temporary_records_after_restart() {
        let tmp = tempfile::TempDir::new().unwrap();
        {
            let provider = FileStorageProvider::new(tmp.path().to_path_buf());
            provider.ownership_ledger().expect("create ledger");
        }
        let ledger = tmp.path().join(OWNERSHIP_LEDGER_DIRECTORY);
        let stale = ledger.join(format!(".{}.json.{}.tmp", Uuid::new_v4(), Uuid::new_v4()));
        let unrelated = ledger.join("operator-note");
        std::fs::write(&stale, b"partial manifest").expect("stale temporary");
        std::fs::write(&unrelated, b"retain").expect("unrelated ledger entry");

        let restarted = FileStorageProvider::new(tmp.path().to_path_buf());
        restarted
            .ownership_ledger()
            .expect("recover ownership ledger");

        assert!(!stale.exists());
        assert!(unrelated.is_file());
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
