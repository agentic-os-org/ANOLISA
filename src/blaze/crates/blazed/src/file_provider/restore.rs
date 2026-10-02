// SPDX-License-Identifier: Apache-2.0
//! Recoverable rootfs replacement for the file storage provider.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use blaze_core::error::{BlazeError, Result};
use blaze_core::storage::{StorageRestoreTransaction, StorageSlot};

use super::{FileStorageProvider, RequiredPathType};

const JOURNAL_VERSION: u32 = 1;
const MAX_JOURNAL_SIZE: u64 = 16 * 1024;

#[derive(Debug)]
struct RestorePaths {
    instance_id: String,
    instance_dir: PathBuf,
    rootfs: PathBuf,
    copying: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    discard: PathBuf,
    journal: PathBuf,
    journal_temporary: PathBuf,
}

impl RestorePaths {
    fn transaction_artifacts(&self) -> [&Path; 6] {
        [
            &self.copying,
            &self.staged,
            &self.backup,
            &self.discard,
            &self.journal,
            &self.journal_temporary,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestoreState {
    Staged,
    Activated,
    Aborting,
    Committing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RestoreJournal {
    version: u32,
    instance_id: String,
    transaction_id: Uuid,
    state: RestoreState,
}

impl RestoreJournal {
    fn transaction(&self) -> StorageRestoreTransaction {
        StorageRestoreTransaction {
            instance_id: self.instance_id.clone(),
            transaction_id: self.transaction_id,
        }
    }
}

/// Removes files that have not yet become part of a durable transaction.
struct UnpublishedFiles {
    paths: Vec<PathBuf>,
}

impl UnpublishedFiles {
    fn new() -> Self {
        Self { paths: Vec::new() }
    }

    fn track(&mut self, path: &Path) {
        self.paths.push(path.to_path_buf());
    }

    fn untrack(&mut self, path: &Path) {
        self.paths.retain(|tracked| tracked != path);
    }

    fn commit(&mut self) {
        self.paths.clear();
    }
}

impl Drop for UnpublishedFiles {
    fn drop(&mut self) {
        for path in self.paths.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(super) async fn stage(
    provider: &FileStorageProvider,
    slot: &StorageSlot,
    source: &Path,
) -> Result<StorageRestoreTransaction> {
    let paths = restore_paths(provider, &slot.id).await?;
    require_plain_file(&paths.rootfs, "live rootfs").await?;
    ensure_no_transaction(&paths).await?;

    let source = canonical_plain_file(source, "restore source").await?;
    let rootfs = tokio::fs::canonicalize(&paths.rootfs)
        .await
        .map_err(|error| storage_error(format!("canonicalize live rootfs: {error}")))?;
    if same_file(&source, &rootfs).await? {
        return Err(storage_error(
            "restore source must be independent from the live rootfs",
        ));
    }

    let journal = RestoreJournal {
        version: JOURNAL_VERSION,
        instance_id: paths.instance_id.clone(),
        transaction_id: Uuid::new_v4(),
        state: RestoreState::Staged,
    };
    let mut unpublished = UnpublishedFiles::new();

    copy_for_restore(&source, &paths.copying, &mut unpublished).await?;
    crate::failpoint::pause("storage-restore-after-copy").await;
    rename_new_plain_file(&paths.copying, &paths.staged).await?;
    unpublished.untrack(&paths.copying);
    unpublished.track(&paths.staged);
    sync_directory(&paths.instance_dir).await?;
    crate::failpoint::pause("storage-restore-after-stage").await;

    publish_new_journal(&paths, &journal, &mut unpublished).await?;
    unpublished.commit();
    sync_directory(&paths.instance_dir).await?;
    Ok(journal.transaction())
}

pub(super) async fn activate(
    provider: &FileStorageProvider,
    transaction: &StorageRestoreTransaction,
) -> Result<()> {
    let paths = restore_paths(provider, &transaction.instance_id).await?;
    ensure_no_transient_files(&paths).await?;
    let mut journal = require_journal(&paths).await?;
    verify_transaction(&journal, transaction)?;

    match journal.state {
        RestoreState::Activated => return ensure_activated_layout(&paths).await,
        RestoreState::Staged => {}
        RestoreState::Aborting => {
            return Err(storage_error(format!(
                "restore transaction {} is aborting",
                transaction.transaction_id
            )));
        }
        RestoreState::Committing => {
            return Err(storage_error(format!(
                "restore transaction {} is committing",
                transaction.transaction_id
            )));
        }
    }

    let (live, staged, backup, discard) = inspect_layout(&paths).await?;
    if discard {
        return Err(invalid_layout(&paths, journal.state));
    }

    if live && staged && !backup {
        rename_new_plain_file(&paths.rootfs, &paths.backup).await?;
        sync_directory(&paths.instance_dir).await?;
        crate::failpoint::pause("storage-restore-after-backup").await;
    } else if !live && staged && backup {
        // Resume after the predecessor was retained.
    } else if live && !staged && backup {
        // Resume after the staged rootfs was selected.
    } else {
        return Err(invalid_layout(&paths, journal.state));
    }

    let (live, staged, backup, discard) = inspect_layout(&paths).await?;
    if !live && staged && backup && !discard {
        if let Err(selection) = select_staged_rootfs(&paths).await {
            let rollback = match crate::failpoint::storage("storage-restore-switch-rollback") {
                Ok(()) => match rename_new_plain_file(&paths.backup, &paths.rootfs).await {
                    Ok(()) => sync_directory(&paths.instance_dir).await,
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            return match rollback {
                Ok(()) => Err(storage_error(format!(
                    "select staged rootfs for '{}': {selection}; predecessor restored",
                    paths.instance_id
                ))),
                Err(rollback) => Err(storage_error(format!(
                    "select staged rootfs for '{}': {selection}; restoring predecessor failed: \
                     {rollback}",
                    paths.instance_id
                ))),
            };
        }
        crate::failpoint::pause("storage-restore-after-switch").await;
    }

    ensure_activated_layout(&paths).await?;
    journal.state = RestoreState::Activated;
    replace_journal(&paths, &journal).await
}

async fn select_staged_rootfs(paths: &RestorePaths) -> Result<()> {
    crate::failpoint::storage("storage-restore-switch")?;
    rename_new_plain_file(&paths.staged, &paths.rootfs).await?;
    sync_directory(&paths.instance_dir).await
}

pub(super) async fn commit(
    provider: &FileStorageProvider,
    transaction: &StorageRestoreTransaction,
) -> Result<()> {
    let paths = restore_paths(provider, &transaction.instance_id).await?;
    ensure_no_transient_files(&paths).await?;
    let Some(mut journal) = read_journal(&paths).await? else {
        return ensure_finalized_layout(&paths).await;
    };
    verify_transaction(&journal, transaction)?;

    match journal.state {
        RestoreState::Activated => {
            ensure_activated_layout(&paths).await?;
            journal.state = RestoreState::Committing;
            replace_journal(&paths, &journal).await?;
            crate::failpoint::pause("storage-restore-after-commit-intent").await;
        }
        RestoreState::Committing => {}
        RestoreState::Staged => {
            return Err(storage_error(format!(
                "restore transaction {} is not activated",
                transaction.transaction_id
            )));
        }
        RestoreState::Aborting => {
            return Err(storage_error(format!(
                "restore transaction {} is aborting",
                transaction.transaction_id
            )));
        }
    }
    finish_commit(&paths).await
}

pub(super) async fn abort(
    provider: &FileStorageProvider,
    transaction: &StorageRestoreTransaction,
) -> Result<()> {
    let paths = restore_paths(provider, &transaction.instance_id).await?;
    ensure_no_transient_files(&paths).await?;
    let Some(mut journal) = read_journal(&paths).await? else {
        return ensure_finalized_layout(&paths).await;
    };
    verify_transaction(&journal, transaction)?;

    if journal.state == RestoreState::Committing {
        return Err(storage_error(format!(
            "restore transaction {} has durable commit intent",
            transaction.transaction_id
        )));
    }
    if journal.state != RestoreState::Aborting {
        journal.state = RestoreState::Aborting;
        replace_journal(&paths, &journal).await?;
    }
    finish_abort(&paths).await
}

pub(super) async fn reconcile(provider: &FileStorageProvider, instance_id: &str) -> Result<()> {
    let paths = restore_paths(provider, instance_id).await?;
    remove_plain_file_if_present(&paths.copying, "restore copying file").await?;
    remove_plain_file_if_present(&paths.journal_temporary, "restore journal temporary").await?;

    let Some(mut journal) = read_journal(&paths).await? else {
        return reconcile_without_journal(&paths).await;
    };
    if journal.instance_id != paths.instance_id {
        return Err(storage_error(format!(
            "restore journal instance '{}' does not match slot '{}'",
            journal.instance_id, paths.instance_id
        )));
    }

    match journal.state {
        RestoreState::Committing => finish_commit(&paths).await,
        RestoreState::Staged | RestoreState::Activated => {
            journal.state = RestoreState::Aborting;
            replace_journal(&paths, &journal).await?;
            finish_abort(&paths).await
        }
        RestoreState::Aborting => finish_abort(&paths).await,
    }
}

async fn restore_paths(provider: &FileStorageProvider, instance_id: &str) -> Result<RestorePaths> {
    let slot = provider.slot_for_id(instance_id)?;
    let instances_dir = canonical_plain_path(
        &provider.instances_dir,
        RequiredPathType::Directory,
        "instances directory",
    )
    .await?;
    let instance_dir = canonical_plain_path(
        &slot.instance_dir,
        RequiredPathType::Directory,
        "slot directory",
    )
    .await?;
    if instance_dir.parent() != Some(instances_dir.as_path())
        || instance_dir.file_name() != Some(std::ffi::OsStr::new(instance_id))
    {
        return Err(storage_error(format!(
            "restore slot {} is not the direct '{}' child of instances directory {}",
            instance_dir.display(),
            instance_id,
            instances_dir.display()
        )));
    }

    Ok(RestorePaths {
        instance_id: instance_id.to_string(),
        rootfs: instance_dir.join("rootfs.ext4"),
        copying: instance_dir.join(".rootfs.restore-copying"),
        staged: instance_dir.join(".rootfs.restore-staged"),
        backup: instance_dir.join(".rootfs.restore-backup"),
        discard: instance_dir.join(".rootfs.restore-discard"),
        journal: instance_dir.join(".rootfs.restore.json"),
        journal_temporary: instance_dir.join(".rootfs.restore-journal.tmp"),
        instance_dir,
    })
}

async fn canonical_plain_path(
    path: &Path,
    required_type: RequiredPathType,
    description: &str,
) -> Result<PathBuf> {
    if matches!(required_type, RequiredPathType::File) && is_retained_descriptor_path(path) {
        let metadata = tokio::fs::metadata(path).await.map_err(|error| {
            storage_error(format!(
                "inspect retained {description} {}: {error}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(storage_error(format!(
                "{description} {} is not a retained plain file",
                path.display()
            )));
        }
        return Ok(path.to_path_buf());
    }
    let metadata = tokio::fs::symlink_metadata(path).await.map_err(|error| {
        storage_error(format!("inspect {description} {}: {error}", path.display()))
    })?;
    if !required_type.matches(&metadata) || metadata.file_type().is_symlink() {
        return Err(storage_error(format!(
            "{description} {} is not a plain {}",
            path.display(),
            required_type.description()
        )));
    }
    tokio::fs::canonicalize(path).await.map_err(|error| {
        storage_error(format!(
            "canonicalize {description} {}: {error}",
            path.display()
        ))
    })
}

async fn canonical_plain_file(path: &Path, description: &str) -> Result<PathBuf> {
    canonical_plain_path(path, RequiredPathType::File, description).await
}

async fn require_plain_file(path: &Path, description: &str) -> Result<()> {
    if plain_file_exists(path, description).await? {
        Ok(())
    } else {
        Err(storage_error(format!(
            "{description} {} does not exist",
            path.display()
        )))
    }
}

async fn plain_file_exists(path: &Path, description: &str) -> Result<bool> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(storage_error(format!(
            "{description} {} is not a plain file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage_error(format!(
            "inspect {description} {}: {error}",
            path.display()
        ))),
    }
}

async fn entry_exists(path: &Path) -> Result<bool> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage_error(format!(
            "inspect restore artifact {}: {error}",
            path.display()
        ))),
    }
}

async fn ensure_no_transaction(paths: &RestorePaths) -> Result<()> {
    for path in paths.transaction_artifacts() {
        if entry_exists(path).await? {
            return Err(storage_error(format!(
                "slot '{}' has unfinished restore artifact {}; reconcile it first",
                paths.instance_id,
                path.display()
            )));
        }
    }
    Ok(())
}

async fn ensure_no_transient_files(paths: &RestorePaths) -> Result<()> {
    for (path, description) in [
        (&paths.copying, "restore copying file"),
        (&paths.journal_temporary, "restore journal temporary"),
    ] {
        if entry_exists(path).await? {
            return Err(storage_error(format!(
                "slot '{}' has unfinished {description}; reconcile it first",
                paths.instance_id
            )));
        }
    }
    Ok(())
}

async fn copy_for_restore(
    source: &Path,
    destination: &Path,
    unpublished: &mut UnpublishedFiles,
) -> Result<()> {
    let mut source_options = tokio::fs::OpenOptions::new();
    source_options.read(true);
    #[cfg(unix)]
    if !is_retained_descriptor_path(source) {
        source_options.custom_flags(libc::O_NOFOLLOW);
    }
    let source_file = source_options.open(source).await.map_err(|error| {
        storage_error(format!("open restore source {}: {error}", source.display()))
    })?;
    if !source_file
        .metadata()
        .await
        .map_err(|error| storage_error(format!("inspect restore source: {error}")))?
        .is_file()
    {
        return Err(storage_error(format!(
            "restore source {} is not a regular file",
            source.display()
        )));
    }

    let mut destination_options = tokio::fs::OpenOptions::new();
    destination_options.write(true).create_new(true);
    #[cfg(unix)]
    destination_options.custom_flags(libc::O_NOFOLLOW);
    let destination_file = destination_options
        .open(destination)
        .await
        .map_err(|error| {
            storage_error(format!(
                "create restore stage {}: {error}",
                destination.display()
            ))
        })?;
    unpublished.track(destination);

    // Reuse the capture-side sparse copy so a mostly empty guest image does not
    // become fully allocated here. A dense copy would materialize every hole and
    // could exhaust the filesystem at the configured logical size even when the
    // live rootfs and the checkpoint both fit.
    let source_file = source_file.into_std().await;
    let destination_file = destination_file.into_std().await;
    crate::failpoint::spawn_blocking(move || {
        super::copy_sparse_file(&source_file, &destination_file)?;
        destination_file.sync_all()
    })
    .await
    .map_err(|error| storage_error(format!("restore stage copy task failed: {error}")))?
    .map_err(|error| storage_error(format!("copy restore source: {error}")))
}

fn is_retained_descriptor_path(path: &Path) -> bool {
    let parent = PathBuf::from(format!("/proc/{}/fd", std::process::id()));
    path.parent() == Some(parent.as_path())
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.parse::<u32>().is_ok())
}

async fn same_file(left: &Path, right: &Path) -> Result<bool> {
    let left = tokio::fs::metadata(left)
        .await
        .map_err(|error| storage_error(format!("inspect restore source: {error}")))?;
    let right = tokio::fs::metadata(right)
        .await
        .map_err(|error| storage_error(format!("inspect live rootfs: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(false)
    }
}

async fn publish_new_journal(
    paths: &RestorePaths,
    journal: &RestoreJournal,
    unpublished: &mut UnpublishedFiles,
) -> Result<()> {
    let bytes = encode_journal(journal)?;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(&paths.journal_temporary)
        .await
        .map_err(|error| storage_error(format!("create restore journal: {error}")))?;
    unpublished.track(&paths.journal_temporary);
    file.write_all(&bytes)
        .await
        .map_err(|error| storage_error(format!("write restore journal: {error}")))?;
    file.sync_all()
        .await
        .map_err(|error| storage_error(format!("sync restore journal: {error}")))?;
    drop(file);
    rename_new_plain_file(&paths.journal_temporary, &paths.journal).await?;
    unpublished.untrack(&paths.journal_temporary);
    // The journal now owns the staged rootfs, even if the directory sync fails.
    unpublished.commit();
    Ok(())
}

async fn replace_journal(paths: &RestorePaths, journal: &RestoreJournal) -> Result<()> {
    require_plain_file(&paths.journal, "restore journal").await?;
    if entry_exists(&paths.journal_temporary).await? {
        return Err(storage_error(format!(
            "slot '{}' has an unfinished journal update; reconcile it first",
            paths.instance_id
        )));
    }

    let bytes = encode_journal(journal)?;
    let mut cleanup = UnpublishedFiles::new();
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(&paths.journal_temporary)
        .await
        .map_err(|error| storage_error(format!("create restore journal update: {error}")))?;
    cleanup.track(&paths.journal_temporary);
    file.write_all(&bytes)
        .await
        .map_err(|error| storage_error(format!("write restore journal update: {error}")))?;
    file.sync_all()
        .await
        .map_err(|error| storage_error(format!("sync restore journal update: {error}")))?;
    drop(file);
    tokio::fs::rename(&paths.journal_temporary, &paths.journal)
        .await
        .map_err(|error| storage_error(format!("replace restore journal: {error}")))?;
    cleanup.untrack(&paths.journal_temporary);
    sync_directory(&paths.instance_dir).await
}

fn encode_journal(journal: &RestoreJournal) -> Result<Vec<u8>> {
    serde_json::to_vec(journal)
        .map_err(|error| storage_error(format!("encode restore journal: {error}")))
}

async fn read_journal(paths: &RestorePaths) -> Result<Option<RestoreJournal>> {
    if !plain_file_exists(&paths.journal, "restore journal").await? {
        return Ok(None);
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(&paths.journal)
        .await
        .map_err(|error| storage_error(format!("open restore journal: {error}")))?;
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL_SIZE + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| storage_error(format!("read restore journal: {error}")))?;
    if bytes.len() as u64 > MAX_JOURNAL_SIZE {
        return Err(storage_error("restore journal exceeds the size limit"));
    }
    let journal: RestoreJournal = serde_json::from_slice(&bytes)
        .map_err(|error| storage_error(format!("parse restore journal: {error}")))?;
    if journal.version != JOURNAL_VERSION {
        return Err(storage_error(format!(
            "unsupported restore journal version {}",
            journal.version
        )));
    }
    Ok(Some(journal))
}

async fn require_journal(paths: &RestorePaths) -> Result<RestoreJournal> {
    read_journal(paths).await?.ok_or_else(|| {
        storage_error(format!(
            "slot '{}' has no restore transaction",
            paths.instance_id
        ))
    })
}

fn verify_transaction(
    journal: &RestoreJournal,
    transaction: &StorageRestoreTransaction,
) -> Result<()> {
    if journal.instance_id != transaction.instance_id
        || journal.transaction_id != transaction.transaction_id
    {
        return Err(storage_error(format!(
            "restore transaction {} does not own slot '{}'",
            transaction.transaction_id, transaction.instance_id
        )));
    }
    Ok(())
}

async fn inspect_layout(paths: &RestorePaths) -> Result<(bool, bool, bool, bool)> {
    Ok((
        plain_file_exists(&paths.rootfs, "live rootfs").await?,
        plain_file_exists(&paths.staged, "staged rootfs").await?,
        plain_file_exists(&paths.backup, "retained rootfs").await?,
        plain_file_exists(&paths.discard, "discarded rootfs").await?,
    ))
}

async fn ensure_activated_layout(paths: &RestorePaths) -> Result<()> {
    if inspect_layout(paths).await? != (true, false, true, false) {
        return Err(invalid_layout(paths, RestoreState::Activated));
    }
    Ok(())
}

async fn ensure_finalized_layout(paths: &RestorePaths) -> Result<()> {
    require_plain_file(&paths.rootfs, "live rootfs").await?;
    for path in paths.transaction_artifacts() {
        if entry_exists(path).await? {
            return Err(storage_error(format!(
                "slot '{}' has restore artifact {}; reconcile it first",
                paths.instance_id,
                path.display()
            )));
        }
    }
    Ok(())
}

async fn finish_abort(paths: &RestorePaths) -> Result<()> {
    for _ in 0..6 {
        let layout = inspect_layout(paths).await?;
        match layout {
            (false, true, true, false) => {
                rename_new_plain_file(&paths.backup, &paths.rootfs).await?;
                sync_directory(&paths.instance_dir).await?;
                crate::failpoint::pause("storage-restore-after-rollback-rootfs").await;
            }
            (true, true, false, false) => {
                remove_plain_file(&paths.staged, "staged rootfs").await?;
                sync_directory(&paths.instance_dir).await?;
            }
            (true, false, true, false) => {
                rename_new_plain_file(&paths.rootfs, &paths.discard).await?;
                sync_directory(&paths.instance_dir).await?;
                crate::failpoint::pause("storage-restore-after-discard").await;
            }
            (false, false, true, true) => {
                rename_new_plain_file(&paths.backup, &paths.rootfs).await?;
                sync_directory(&paths.instance_dir).await?;
                crate::failpoint::pause("storage-restore-after-rollback-rootfs").await;
            }
            (true, false, false, true) => {
                remove_plain_file(&paths.discard, "discarded rootfs").await?;
                sync_directory(&paths.instance_dir).await?;
            }
            (true, false, false, false) => {
                remove_plain_file(&paths.journal, "restore journal").await?;
                sync_directory(&paths.instance_dir).await?;
                return Ok(());
            }
            _ => return Err(invalid_layout(paths, RestoreState::Aborting)),
        }
    }
    Err(storage_error(format!(
        "restore abort for '{}' did not converge",
        paths.instance_id
    )))
}

async fn finish_commit(paths: &RestorePaths) -> Result<()> {
    let (live, staged, _backup, discard) = inspect_layout(paths).await?;
    if !live || staged || discard {
        return Err(invalid_layout(paths, RestoreState::Committing));
    }
    remove_plain_file_if_present(&paths.backup, "retained rootfs").await?;
    sync_directory(&paths.instance_dir).await?;
    crate::failpoint::pause("storage-restore-after-backup-release").await;
    remove_plain_file(&paths.journal, "restore journal").await?;
    sync_directory(&paths.instance_dir).await
}

async fn reconcile_without_journal(paths: &RestorePaths) -> Result<()> {
    let (live, staged, backup, discard) = inspect_layout(paths).await?;
    if backup || discard || !live {
        return Err(storage_error(format!(
            "slot '{}' has ambiguous restore artifacts without a journal",
            paths.instance_id
        )));
    }
    if staged {
        remove_plain_file(&paths.staged, "unpublished staged rootfs").await?;
        sync_directory(&paths.instance_dir).await?;
    }
    Ok(())
}

async fn rename_new_plain_file(source: &Path, target: &Path) -> Result<()> {
    require_plain_file(source, "restore rename source").await?;
    if entry_exists(target).await? {
        return Err(storage_error(format!(
            "restore rename target {} already exists",
            target.display()
        )));
    }
    tokio::fs::rename(source, target).await.map_err(|error| {
        storage_error(format!(
            "rename restore file {} to {}: {error}",
            source.display(),
            target.display()
        ))
    })
}

async fn remove_plain_file(path: &Path, description: &str) -> Result<()> {
    require_plain_file(path, description).await?;
    tokio::fs::remove_file(path)
        .await
        .map_err(|error| storage_error(format!("remove {description} {}: {error}", path.display())))
}

async fn remove_plain_file_if_present(path: &Path, description: &str) -> Result<()> {
    if plain_file_exists(path, description).await? {
        remove_plain_file(path, description).await?;
    }
    Ok(())
}

async fn sync_directory(path: &Path) -> Result<()> {
    tokio::fs::File::open(path)
        .await
        .map_err(|error| {
            storage_error(format!(
                "open restore directory {}: {error}",
                path.display()
            ))
        })?
        .sync_all()
        .await
        .map_err(|error| {
            storage_error(format!(
                "sync restore directory {}: {error}",
                path.display()
            ))
        })
}

fn invalid_layout(paths: &RestorePaths, state: RestoreState) -> BlazeError {
    storage_error(format!(
        "slot '{}' has an invalid {:?} restore layout",
        paths.instance_id, state
    ))
}

fn storage_error(message: impl Into<String>) -> BlazeError {
    BlazeError::StorageError {
        msg: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use async_trait::async_trait;
    use blaze_core::storage::{
        AcquireOpts, PoolStatus, StorageAcquireError, StorageProvider, StorageSlot,
    };

    use super::*;

    /// Size used only to build test sources larger than one copy step.
    #[cfg(feature = "test-failpoints")]
    const COPY_BUFFER_SIZE: usize = 64 * 1024;

    struct UnsupportedStorage;

    #[async_trait]
    impl StorageProvider for UnsupportedStorage {
        async fn probe(&self) -> Result<bool> {
            Ok(true)
        }

        async fn acquire(
            &self,
            _opts: &AcquireOpts,
        ) -> std::result::Result<StorageSlot, StorageAcquireError> {
            Err(StorageAcquireError::clean(storage_error(
                "acquire unavailable",
            )))
        }

        async fn release(&self, _slot: StorageSlot) -> Result<()> {
            Ok(())
        }

        async fn reconstruct(&self, _instance_id: &str) -> Result<StorageSlot> {
            Err(storage_error("reconstruct unavailable"))
        }

        async fn sync_artifacts(&self, _slot: &StorageSlot) -> Result<()> {
            Ok(())
        }

        fn pool_status(&self) -> PoolStatus {
            PoolStatus::default()
        }
    }

    async fn fixture(
        instance_id: &str,
    ) -> (tempfile::TempDir, FileStorageProvider, StorageSlot, PathBuf) {
        let temp = tempfile::tempdir().expect("temporary storage");
        let instances = temp.path().join("instances");
        let checkpoints = temp.path().join("checkpoints");
        tokio::fs::create_dir(&instances)
            .await
            .expect("instances directory");
        tokio::fs::create_dir(&checkpoints)
            .await
            .expect("checkpoints directory");
        let provider = FileStorageProvider::new(instances);
        let slot = provider
            .acquire(&AcquireOpts {
                instance_id: instance_id.to_string(),
                rootfs_size: 64,
                mem_size: 32,
            })
            .await
            .expect("storage slot");
        tokio::fs::write(&slot.rootfs_path, b"live-rootfs")
            .await
            .expect("live rootfs");
        let source = checkpoints.join("rootfs.snap");
        tokio::fs::write(&source, b"checkpoint-rootfs")
            .await
            .expect("checkpoint rootfs");
        (temp, provider, slot, source)
    }

    async fn rootfs(path: &Path) -> Vec<u8> {
        tokio::fs::read(path).await.expect("read rootfs")
    }

    #[tokio::test]
    async fn restore_contract_is_opt_in_and_fail_closed() {
        let provider = UnsupportedStorage;
        let slot = StorageSlot {
            id: "unsupported".to_string(),
            rootfs_path: PathBuf::from("rootfs"),
            mem_path: PathBuf::from("memory"),
            mem_diff_path: PathBuf::from("memory-diff"),
            rootfs_diff_path: PathBuf::from("rootfs-diff"),
            instance_dir: PathBuf::from("instance"),
        };
        let transaction = StorageRestoreTransaction {
            instance_id: slot.id.clone(),
            transaction_id: Uuid::new_v4(),
        };

        assert!(!provider.supports_checkpoint_restore());
        assert!(
            provider
                .stage_checkpoint_restore(&slot, Path::new("checkpoint"))
                .await
                .is_err()
        );
        assert!(
            provider
                .activate_checkpoint_restore(&transaction)
                .await
                .is_err()
        );
        assert!(
            provider
                .commit_checkpoint_restore(&transaction)
                .await
                .is_err()
        );
        assert!(
            provider
                .abort_checkpoint_restore(&transaction)
                .await
                .is_err()
        );
        assert!(
            provider
                .reconcile_checkpoint_restore(&slot.id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stage_keeps_the_live_rootfs_running_image_unchanged() {
        let (_temp, provider, slot, source) = fixture("stage-independent").await;

        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage restore");

        assert!(provider.supports_checkpoint_restore());
        assert_eq!(transaction.instance_id, slot.id);
        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
        provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect_err("a second transaction must fail closed");
        provider
            .abort_checkpoint_restore(&transaction)
            .await
            .expect("abort staged restore");
    }

    /// Sparse allocation is filesystem dependent, so this assertion is limited
    /// to the platform the daemon targets, matching the capture-side test.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn staging_preserves_sparse_extents() {
        use std::io::{Seek, Write};
        use std::os::unix::fs::MetadataExt;

        const LOGICAL_LEN: u64 = 64 * 1024 * 1024;
        const FIRST_OFFSET: u64 = 4 * 1024;
        const LAST_OFFSET: u64 = 48 * 1024 * 1024 + 137;
        const FIRST_DATA: &[u8] = b"first-restore-extent";
        const LAST_DATA: &[u8] = b"last-restore-extent";

        let (_temp, provider, slot, source) = fixture("stage-sparse").await;
        let mut checkpoint = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .expect("open checkpoint rootfs");
        checkpoint.set_len(LOGICAL_LEN).expect("logical length");
        checkpoint
            .seek(std::io::SeekFrom::Start(FIRST_OFFSET))
            .expect("seek first extent");
        checkpoint.write_all(FIRST_DATA).expect("first extent");
        checkpoint
            .seek(std::io::SeekFrom::Start(LAST_OFFSET))
            .expect("seek last extent");
        checkpoint.write_all(LAST_DATA).expect("last extent");
        checkpoint.sync_all().expect("sync checkpoint");
        let source_blocks = checkpoint.metadata().expect("source metadata").blocks();
        drop(checkpoint);

        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage sparse restore");
        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");
        provider
            .commit_checkpoint_restore(&transaction)
            .await
            .expect("commit");

        let metadata = std::fs::metadata(&slot.rootfs_path).expect("restored metadata");
        assert_eq!(
            metadata.len(),
            LOGICAL_LEN,
            "logical length must be restored"
        );
        assert!(
            metadata.blocks().saturating_mul(512) < LOGICAL_LEN / 4,
            "restore allocated {} bytes for a {LOGICAL_LEN}-byte sparse checkpoint",
            metadata.blocks().saturating_mul(512)
        );
        assert!(
            metadata.blocks() <= source_blocks.saturating_add(32),
            "restore used {} blocks for a checkpoint using {source_blocks} blocks",
            metadata.blocks()
        );

        let restored = std::fs::read(&slot.rootfs_path).expect("restored rootfs");
        assert_eq!(
            &restored[FIRST_OFFSET as usize..FIRST_OFFSET as usize + FIRST_DATA.len()],
            FIRST_DATA
        );
        assert_eq!(
            &restored[LAST_OFFSET as usize..LAST_OFFSET as usize + LAST_DATA.len()],
            LAST_DATA
        );
    }

    #[tokio::test]
    async fn activated_restore_can_be_aborted_to_the_predecessor() {
        let (_temp, provider, slot, source) = fixture("activate-abort").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");

        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");
        assert_eq!(rootfs(&slot.rootfs_path).await, b"checkpoint-rootfs");

        provider
            .abort_checkpoint_restore(&transaction)
            .await
            .expect("abort");
        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
        ensure_finalized_layout(&restore_paths(&provider, &slot.id).await.unwrap())
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn activation_retains_the_original_rootfs_inode_until_finalization() {
        use std::os::unix::fs::MetadataExt;

        let (_temp, provider, slot, source) = fixture("retain-inode").await;
        let original_inode = tokio::fs::metadata(&slot.rootfs_path)
            .await
            .expect("live metadata")
            .ino();
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        assert_eq!(
            tokio::fs::metadata(&slot.rootfs_path)
                .await
                .expect("staged live metadata")
                .ino(),
            original_inode
        );

        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");
        let paths = restore_paths(&provider, &slot.id).await.expect("paths");
        assert_eq!(
            tokio::fs::metadata(&paths.backup)
                .await
                .expect("backup metadata")
                .ino(),
            original_inode
        );
        assert_ne!(
            tokio::fs::metadata(&paths.rootfs)
                .await
                .expect("selected metadata")
                .ino(),
            original_inode
        );

        provider
            .abort_checkpoint_restore(&transaction)
            .await
            .expect("abort");
        assert_eq!(
            tokio::fs::metadata(&slot.rootfs_path)
                .await
                .expect("restored metadata")
                .ino(),
            original_inode
        );
    }

    #[tokio::test]
    async fn committed_restore_releases_the_predecessor() {
        let (_temp, provider, slot, source) = fixture("activate-commit").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");

        provider
            .commit_checkpoint_restore(&transaction)
            .await
            .expect("commit");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"checkpoint-rootfs");
        ensure_finalized_layout(&restore_paths(&provider, &slot.id).await.unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stale_transaction_handle_cannot_select_a_rootfs() {
        let (_temp, provider, slot, source) = fixture("stale-handle").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        let stale = StorageRestoreTransaction {
            instance_id: transaction.instance_id.clone(),
            transaction_id: Uuid::new_v4(),
        };

        provider
            .activate_checkpoint_restore(&stale)
            .await
            .expect_err("stale transaction must be rejected");
        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
        provider
            .abort_checkpoint_restore(&transaction)
            .await
            .expect("abort");
    }

    #[tokio::test]
    async fn staging_rederives_provider_paths_from_the_slot_id() {
        let (temp, provider, slot, source) = fixture("canonical-slot").await;
        let external = temp.path().join("external-rootfs");
        tokio::fs::write(&external, b"external")
            .await
            .expect("external rootfs");
        let mut forged = slot.clone();
        forged.rootfs_path = external.clone();
        forged.instance_dir = temp.path().to_path_buf();

        let transaction = provider
            .stage_checkpoint_restore(&forged, &source)
            .await
            .expect("stage through canonical slot");
        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"checkpoint-rootfs");
        assert_eq!(rootfs(&external).await, b"external");
        provider
            .abort_checkpoint_restore(&transaction)
            .await
            .expect("abort");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staging_rejects_linked_sources_and_slot_paths() {
        use std::os::unix::fs::symlink;

        let (temp, provider, slot, source) = fixture("linked-paths").await;
        let linked_source = temp.path().join("linked-source");
        symlink(&source, &linked_source).expect("source link");
        provider
            .stage_checkpoint_restore(&slot, &linked_source)
            .await
            .expect_err("linked source must be rejected");

        tokio::fs::remove_file(&slot.rootfs_path)
            .await
            .expect("remove live rootfs");
        let external = temp.path().join("external-rootfs");
        tokio::fs::write(&external, b"external")
            .await
            .expect("external rootfs");
        symlink(&external, &slot.rootfs_path).expect("rootfs link");
        provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect_err("linked live rootfs must be rejected");
        assert_eq!(rootfs(&external).await, b"external");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staging_rejects_a_linked_slot_directory() {
        use std::os::unix::fs::symlink;

        let (temp, provider, slot, source) = fixture("linked-slot").await;
        tokio::fs::remove_dir_all(&slot.instance_dir)
            .await
            .expect("remove slot");
        let external = temp.path().join("external-slot");
        tokio::fs::create_dir(&external)
            .await
            .expect("external slot");
        tokio::fs::write(external.join("rootfs.ext4"), b"external")
            .await
            .expect("external rootfs");
        symlink(&external, &slot.instance_dir).expect("slot link");

        provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect_err("linked slot directory must be rejected");

        assert_eq!(rootfs(&external.join("rootfs.ext4")).await, b"external");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staging_rejects_linked_transaction_artifacts() {
        use std::os::unix::fs::symlink;

        let (temp, provider, slot, source) = fixture("linked-artifact").await;
        let external = temp.path().join("external");
        tokio::fs::write(&external, b"external")
            .await
            .expect("external");
        let paths = restore_paths(&provider, &slot.id).await.expect("paths");
        symlink(&external, &paths.staged).expect("stage link");

        provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect_err("linked transaction artifact must fail closed");
        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
        assert_eq!(rootfs(&external).await, b"external");
    }

    #[tokio::test]
    async fn staging_rejects_instance_id_path_components() {
        let (_temp, provider, mut slot, source) = fixture("valid-id").await;
        slot.id = "../escape".to_string();

        provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect_err("path component must be rejected");
    }

    #[tokio::test]
    async fn restart_aborts_a_staged_restore() {
        let (_temp, provider, slot, source) = fixture("restart-staged").await;
        provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");

        let restarted = FileStorageProvider::new(provider.instances_dir.clone());
        restarted
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
        ensure_finalized_layout(&restore_paths(&restarted, &slot.id).await.unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restart_aborts_an_activated_restore() {
        let (_temp, provider, slot, source) = fixture("restart-activated").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");

        let restarted = FileStorageProvider::new(provider.instances_dir.clone());
        restarted
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
    }

    #[tokio::test]
    async fn restart_finishes_a_durable_commit_intent() {
        let (_temp, provider, slot, source) = fixture("restart-commit").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");
        let paths = restore_paths(&provider, &slot.id).await.expect("paths");
        let mut journal = require_journal(&paths).await.expect("journal");
        journal.state = RestoreState::Committing;
        replace_journal(&paths, &journal)
            .await
            .expect("commit intent");

        let restarted = FileStorageProvider::new(provider.instances_dir.clone());
        restarted
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"checkpoint-rootfs");
        ensure_finalized_layout(&restore_paths(&restarted, &slot.id).await.unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restart_cleans_a_partial_copy_without_touching_the_live_rootfs() {
        let (_temp, provider, slot, _source) = fixture("restart-copying").await;
        let paths = restore_paths(&provider, &slot.id).await.expect("paths");
        tokio::fs::write(&paths.copying, b"partial")
            .await
            .expect("partial copy");

        let restarted = FileStorageProvider::new(provider.instances_dir.clone());
        restarted
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
        assert!(!paths.copying.exists());
    }

    #[tokio::test]
    async fn restart_recovers_after_retaining_the_predecessor() {
        let (_temp, provider, slot, source) = fixture("restart-after-backup").await;
        provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        let paths = restore_paths(&provider, &slot.id).await.expect("paths");
        rename_new_plain_file(&paths.rootfs, &paths.backup)
            .await
            .expect("retain predecessor");

        let restarted = FileStorageProvider::new(provider.instances_dir.clone());
        restarted
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
        assert!(!paths.staged.exists());
        assert!(!paths.backup.exists());
    }

    #[tokio::test]
    async fn restart_recovers_after_switching_before_journal_update() {
        let (_temp, provider, slot, source) = fixture("restart-after-switch").await;
        provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        let paths = restore_paths(&provider, &slot.id).await.expect("paths");
        rename_new_plain_file(&paths.rootfs, &paths.backup)
            .await
            .expect("retain predecessor");
        rename_new_plain_file(&paths.staged, &paths.rootfs)
            .await
            .expect("switch rootfs");

        let restarted = FileStorageProvider::new(provider.instances_dir.clone());
        restarted
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
    }

    #[tokio::test]
    async fn corrupt_journal_preserves_both_rootfs_versions() {
        let (_temp, provider, slot, source) = fixture("corrupt-journal").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");
        let paths = restore_paths(&provider, &slot.id).await.expect("paths");
        tokio::fs::write(&paths.journal, b"not-json")
            .await
            .expect("corrupt journal");

        provider
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect_err("corrupt journal must fail closed");

        assert_eq!(rootfs(&paths.rootfs).await, b"checkpoint-rootfs");
        assert_eq!(rootfs(&paths.backup).await, b"live-rootfs");
    }

    #[cfg(feature = "test-failpoints")]
    async fn cancel_at(
        provider: FileStorageProvider,
        slot: StorageSlot,
        source: PathBuf,
        failpoint: &'static str,
    ) {
        let hook = crate::failpoint::TestFailpoint::new(&[failpoint]);
        let operation_hook = hook.clone();
        let operation = tokio::spawn(async move {
            operation_hook
                .run(provider.stage_checkpoint_restore(&slot, &source))
                .await
        });
        hook.wait_until_paused().await;
        operation.abort();
        assert!(
            operation
                .await
                .expect_err("operation must be cancelled")
                .is_cancelled()
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_copy_is_removed_before_publication() {
        let (_temp, provider, slot, source) = fixture("cancel-copy").await;
        tokio::fs::write(&source, vec![42_u8; COPY_BUFFER_SIZE * 4])
            .await
            .expect("large checkpoint");
        let instances = provider.instances_dir.clone();
        let id = slot.id.clone();
        let rootfs_path = slot.rootfs_path.clone();

        cancel_at(provider, slot, source, "storage-restore-after-copy").await;

        let restarted = FileStorageProvider::new(instances);
        restarted
            .reconcile_checkpoint_restore(&id)
            .await
            .expect("reconcile cancellation");
        assert_eq!(rootfs(&rootfs_path).await, b"live-rootfs");
        ensure_finalized_layout(&restore_paths(&restarted, &id).await.unwrap())
            .await
            .unwrap();
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn failed_second_rename_immediately_restores_the_predecessor() {
        let (_temp, provider, slot, source) = fixture("failed-switch").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        let hook = crate::failpoint::TestFailpoint::new(&["storage-restore-switch"]);

        hook.run(provider.activate_checkpoint_restore(&transaction))
            .await
            .expect_err("selecting the staged rootfs must fail");

        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
        provider
            .abort_checkpoint_restore(&transaction)
            .await
            .expect("abort retained stage");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn failed_switch_compensation_remains_reconcilable() {
        let (_temp, provider, slot, source) = fixture("failed-switch-rollback").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        let hook = crate::failpoint::TestFailpoint::new(&[
            "storage-restore-switch",
            "storage-restore-switch-rollback",
        ]);

        hook.run(provider.activate_checkpoint_restore(&transaction))
            .await
            .expect_err("selection and immediate compensation must fail");

        let paths = restore_paths(&provider, &slot.id).await.expect("paths");
        assert!(!paths.rootfs.exists());
        assert_eq!(rootfs(&paths.backup).await, b"live-rootfs");
        provider
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile retained predecessor");
        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_activation_after_backup_is_reconciled() {
        let (_temp, provider, slot, source) = fixture("cancel-activation").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        let hook = crate::failpoint::TestFailpoint::new(&["storage-restore-after-backup"]);
        let operation_hook = hook.clone();
        let operation_provider = FileStorageProvider::new(provider.instances_dir.clone());
        let operation_transaction = transaction.clone();
        let operation = tokio::spawn(async move {
            operation_hook
                .run(operation_provider.activate_checkpoint_restore(&operation_transaction))
                .await
        });
        hook.wait_until_paused().await;
        operation.abort();
        assert!(
            operation
                .await
                .expect_err("activation must be cancelled")
                .is_cancelled()
        );

        provider
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");
        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_commit_intent_is_completed_on_restart() {
        let (_temp, provider, slot, source) = fixture("cancel-commit").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");
        let hook = crate::failpoint::TestFailpoint::new(&["storage-restore-after-commit-intent"]);
        let operation_hook = hook.clone();
        let operation_provider = FileStorageProvider::new(provider.instances_dir.clone());
        let operation_transaction = transaction.clone();
        let operation = tokio::spawn(async move {
            operation_hook
                .run(operation_provider.commit_checkpoint_restore(&operation_transaction))
                .await
        });
        hook.wait_until_paused().await;
        operation.abort();
        assert!(
            operation
                .await
                .expect_err("commit must be cancelled")
                .is_cancelled()
        );

        provider
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");
        assert_eq!(rootfs(&slot.rootfs_path).await, b"checkpoint-rootfs");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_abort_is_completed_on_restart() {
        let (_temp, provider, slot, source) = fixture("cancel-abort").await;
        let transaction = provider
            .stage_checkpoint_restore(&slot, &source)
            .await
            .expect("stage");
        provider
            .activate_checkpoint_restore(&transaction)
            .await
            .expect("activate");
        let hook = crate::failpoint::TestFailpoint::new(&["storage-restore-after-discard"]);
        let operation_hook = hook.clone();
        let operation_provider = FileStorageProvider::new(provider.instances_dir.clone());
        let operation_transaction = transaction.clone();
        let operation = tokio::spawn(async move {
            operation_hook
                .run(operation_provider.abort_checkpoint_restore(&operation_transaction))
                .await
        });
        hook.wait_until_paused().await;
        operation.abort();
        assert!(
            operation
                .await
                .expect_err("abort must be cancelled")
                .is_cancelled()
        );

        provider
            .reconcile_checkpoint_restore(&slot.id)
            .await
            .expect("reconcile");
        assert_eq!(rootfs(&slot.rootfs_path).await, b"live-rootfs");
    }
}
