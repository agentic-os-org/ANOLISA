// SPDX-License-Identifier: Apache-2.0
//! Daemon-owned access to persisted sandbox state and runtime directories.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use blaze_core::lifecycle::{BackendOwnership, SandboxInstance, SandboxState};
use rustix::fs::{
    AtFlags, Dir, DirEntry, FileType, FlockOperation, Mode, OFlags, RenameFlags, Stat, flock,
    fstat, fsync, mkdirat, open, openat, renameat, renameat_with, statat, unlinkat,
};
use rustix::io::Errno;
use uuid::Uuid;

use crate::error::{BlazeDaemonError, Result};

const STATE_FILE: &str = "state.json";
const TEMP_STATE_FILE: &str = "state.json.tmp";
const CHECKPOINT_DIRECTORY: &str = "checkpoints";
const CHECKPOINT_DIRECTORY_MODE: Mode = Mode::RWXU;

/// Central access point for the daemon state directory.
///
/// The store holds the opened state-root object for its complete lifetime.
/// Record I/O and runtime-directory paths are derived from that object rather
/// than reopening the configured pathname.
#[derive(Clone)]
pub struct StateStore {
    inner: Arc<StateStoreInner>,
}

struct StateStoreInner {
    configured_root: PathBuf,
    root: OwnedFd,
    run_dirs: Mutex<HashMap<Uuid, RunDirEntry>>,
}

enum RunDirEntry {
    Owned(OwnedRunDir),
    Uncertain(OwnedRunDir),
    Released,
}

/// A cloneable owner of one sandbox runtime directory.
///
/// The handle keeps the opened directory object alive while lifecycle and
/// backend work use it. Its stable path resolves through that descriptor on
/// Linux, so replacing the configured pathname cannot redirect later work.
#[derive(Clone)]
pub(crate) struct OwnedRunDir {
    inner: Arc<OwnedRunDirInner>,
}

/// Cloneable owner of a directory derived from the retained state root.
///
/// Checkpoint catalog code uses this handle instead of reopening configured
/// pathnames after startup validation.
#[derive(Clone)]
pub(crate) struct OwnedStateDirectory {
    inner: Arc<OwnedStateDirectoryInner>,
}

struct OwnedStateDirectoryInner {
    configured_path: PathBuf,
    directory: OwnedFd,
}

struct OwnedRunDirInner {
    instance_id: Uuid,
    configured_path: PathBuf,
    stable_path: PathBuf,
    directory: OwnedFd,
    writer: Mutex<()>,
}

impl fmt::Debug for StateStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StateStore")
            .field("configured_root", &self.inner.configured_root)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for OwnedRunDir {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnedRunDir")
            .field("instance_id", &self.inner.instance_id)
            .field("configured_path", &self.inner.configured_path)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for OwnedStateDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnedStateDirectory")
            .field("configured_path", &self.inner.configured_path)
            .finish_non_exhaustive()
    }
}

impl StateStore {
    /// Open and exclusively own the configured state directory.
    pub fn open(root: PathBuf) -> Result<Self> {
        Self::open_with_lock(root, true)
    }

    #[cfg(test)]
    pub(crate) fn new(root: PathBuf) -> Self {
        Self::open_with_lock(root, false).expect("open test state store")
    }

    fn open_with_lock(root: PathBuf, lock: bool) -> Result<Self> {
        let directory = open(
            &root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        if lock && let Err(error) = flock(&directory, FlockOperation::NonBlockingLockExclusive) {
            return if error == Errno::WOULDBLOCK {
                Err(BlazeDaemonError::Conflict(format!(
                    "state directory {} is already owned by another daemon",
                    root.display()
                )))
            } else {
                Err(std::io::Error::from(error).into())
            };
        }
        Ok(Self {
            inner: Arc::new(StateStoreInner {
                configured_root: root,
                root: directory,
                run_dirs: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Create or open the daemon-owned checkpoint namespace relative to the
    /// retained state-root object.
    pub(crate) fn checkpoint_directory(&self) -> Result<OwnedStateDirectory> {
        match mkdirat(
            &self.inner.root,
            CHECKPOINT_DIRECTORY,
            CHECKPOINT_DIRECTORY_MODE,
        ) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        let directory = openat(
            &self.inner.root,
            CHECKPOINT_DIRECTORY,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        crate::failpoint::state("checkpoint-state-root-sync")?;
        fsync(&self.inner.root).map_err(std::io::Error::from)?;
        Ok(OwnedStateDirectory::new(
            self.inner.configured_root.join(CHECKPOINT_DIRECTORY),
            directory,
        ))
    }

    /// Return the retained owner for one known sandbox directory.
    pub(crate) fn run_dir(&self, id: Uuid) -> Result<OwnedRunDir> {
        self.cached_run_dir(id)?.ok_or_else(|| {
            BlazeDaemonError::NotFound(format!(
                "owned runtime directory for instance {id} is unavailable"
            ))
        })
    }

    /// Report whether a failed first publication left an object that must be
    /// completed or released through lifecycle recovery.
    pub(crate) fn has_run_dir_residual(&self, id: Uuid) -> Result<bool> {
        Ok(matches!(
            self.inner
                .run_dirs
                .lock()
                .map_err(|_| BlazeDaemonError::Internal(
                    "state run-directory lock poisoned".into()
                ))?
                .get(&id),
            Some(RunDirEntry::Owned(_) | RunDirEntry::Uncertain(_))
        ))
    }

    /// Persist one lifecycle record below the owned state root.
    pub fn persist(&self, instance: &SandboxInstance) -> Result<()> {
        let json = serde_json::to_vec_pretty(instance)?;
        let run_dir = {
            let mut run_dirs = self.inner.run_dirs.lock().map_err(|_| {
                BlazeDaemonError::Internal("state run-directory lock poisoned".into())
            })?;
            match run_dirs.get(&instance.id) {
                Some(RunDirEntry::Owned(run_dir)) => run_dir.clone(),
                Some(RunDirEntry::Uncertain(run_dir)) => {
                    let run_dir = run_dir.clone();
                    self.revalidate_uncertain(instance.id, &run_dir)?;
                    run_dirs.insert(instance.id, RunDirEntry::Owned(run_dir.clone()));
                    run_dir
                }
                Some(RunDirEntry::Released) => {
                    return Err(BlazeDaemonError::Conflict(format!(
                        "terminal lifecycle record for instance {} cannot be rewritten",
                        instance.id
                    )));
                }
                None => {
                    return self.publish_new_record(&mut run_dirs, instance, &json);
                }
            }
        };
        let _writer =
            run_dir.inner.writer.lock().map_err(|_| {
                BlazeDaemonError::Internal("state record writer lock poisoned".into())
            })?;
        {
            let run_dirs = self.inner.run_dirs.lock().map_err(|_| {
                BlazeDaemonError::Internal("state run-directory lock poisoned".into())
            })?;
            match run_dirs.get(&instance.id) {
                Some(RunDirEntry::Owned(retained)) if retained.same_object(&run_dir) => {}
                Some(RunDirEntry::Uncertain(_)) => {
                    return Err(BlazeDaemonError::RecoveryRequired(format!(
                        "runtime-directory publication for instance {} is unconfirmed",
                        instance.id
                    )));
                }
                Some(RunDirEntry::Released) => {
                    return Err(BlazeDaemonError::Conflict(format!(
                        "terminal lifecycle record for instance {} cannot be rewritten",
                        instance.id
                    )));
                }
                Some(RunDirEntry::Owned(_)) => {
                    return Err(BlazeDaemonError::Conflict(format!(
                        "runtime-directory ownership changed for instance {}",
                        instance.id
                    )));
                }
                None => {
                    return Err(BlazeDaemonError::Internal(format!(
                        "runtime-directory ownership disappeared for instance {}",
                        instance.id
                    )));
                }
            }
        }
        self.write_record_locked(&run_dir, &json)?;
        // Retrying a publication whose parent-directory sync previously
        // failed must cross that durability boundary before the owner can be
        // released. Syncing the root on every commit keeps that retry path
        // explicit without maintaining a second publication journal.
        fsync(&self.inner.root).map_err(std::io::Error::from)?;
        self.update_retention(instance, &run_dir)?;
        Ok(())
    }

    /// Load one lifecycle record from the owned sandbox directory.
    #[cfg(test)]
    pub fn load(&self, id: Uuid) -> Result<SandboxInstance> {
        let run_dir = match self.cached_run_dir(id)? {
            Some(run_dir) => run_dir,
            None => self.open_run_dir_object(id)?,
        };
        Self::load_from(&run_dir)
    }

    /// Validate and load every owned lifecycle record before request handling.
    ///
    /// The scan owns the run-directory map for its complete duration and must
    /// run before request handling starts. Supported lifecycle writers use the
    /// same store, so this lock prevents a stale scan result from restoring
    /// ownership after a concurrent terminal commit. The production store's
    /// state-root lock excludes other cooperating Blaze daemons.
    pub fn scan(&self) -> Result<HashMap<Uuid, SandboxInstance>> {
        self.scan_with_hooks(|_| Ok(()), || Ok(()), |_| Ok(()))
    }

    #[cfg(test)]
    fn scan_with_owner_hook<F>(&self, mut after_owner: F) -> Result<HashMap<Uuid, SandboxInstance>>
    where
        F: FnMut(Uuid) -> Result<()>,
    {
        self.scan_with_hooks(&mut after_owner, || Ok(()), |_| Ok(()))
    }

    #[cfg(test)]
    fn scan_with_prevalidation_hook<F>(
        &self,
        mut before_validation: F,
    ) -> Result<HashMap<Uuid, SandboxInstance>>
    where
        F: FnMut() -> Result<()>,
    {
        self.scan_with_hooks(|_| Ok(()), &mut before_validation, |_| Ok(()))
    }

    #[cfg(test)]
    fn scan_with_post_final_enumeration_hook<F>(
        &self,
        mut after_final_enumeration: F,
    ) -> Result<HashMap<Uuid, SandboxInstance>>
    where
        F: FnMut(&[Uuid]) -> Result<()>,
    {
        self.scan_with_hooks(|_| Ok(()), || Ok(()), &mut after_final_enumeration)
    }

    fn scan_with_hooks<F, G, H>(
        &self,
        mut after_owner: F,
        mut before_validation: G,
        mut after_final_enumeration: H,
    ) -> Result<HashMap<Uuid, SandboxInstance>>
    where
        F: FnMut(Uuid) -> Result<()>,
        G: FnMut() -> Result<()>,
        H: FnMut(&[Uuid]) -> Result<()>,
    {
        let mut instances = HashMap::new();
        let mut scanned_run_dirs = HashMap::new();
        let mut scanned_owners = Vec::new();
        let mut run_dirs =
            self.inner.run_dirs.lock().map_err(|_| {
                BlazeDaemonError::Internal("state run-directory lock poisoned".into())
            })?;
        if !run_dirs.is_empty() {
            return Err(BlazeDaemonError::Internal(
                "state scan must complete before lifecycle persistence starts".into(),
            ));
        }
        let entries = Dir::read_from(&self.inner.root).map_err(std::io::Error::from)?;
        for entry in entries {
            let entry = entry.map_err(std::io::Error::from)?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if is_state_staging_name(name) {
                if let Err(error) = self.remove_stale_staging(name) {
                    tracing::warn!(entry = name, %error, "failed to remove stale state staging");
                }
                continue;
            }
            let Ok(id) = Uuid::parse_str(name) else {
                continue;
            };
            let canonical_name = id.to_string();
            if name != canonical_name {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "owned state directory {name} is a non-canonical UUID alias for {canonical_name}"
                )));
            }
            let run_dir = self.open_scanned_run_dir(&entry, id).map_err(|error| {
                BlazeDaemonError::RecoveryRequired(format!(
                    "cannot open persisted instance directory {id}: {error}"
                ))
            })?;
            if let Err(error) = remove_file_if_exists(&run_dir.inner.directory, TEMP_STATE_FILE) {
                tracing::warn!(
                    instance = %id,
                    %error,
                    "failed to remove stale state record temporary file"
                );
            }
            let (instance, record_identity) =
                Self::load_from_with_identity(&run_dir).map_err(|error| {
                    BlazeDaemonError::RecoveryRequired(format!(
                        "cannot load persisted instance {id}: {error}"
                    ))
                })?;
            if instance.id != id {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "persisted instance id {} does not match owned directory {id}",
                    instance.id
                )));
            }
            validate_terminal_record(&instance)?;
            let ownership = if instance.state == SandboxState::Destroyed {
                RunDirEntry::Released
            } else {
                RunDirEntry::Owned(run_dir.clone())
            };
            scanned_owners.push((id, run_dir, record_identity));
            scanned_run_dirs.insert(id, ownership);
            instances.insert(id, instance);
            after_owner(id)?;
        }
        before_validation()?;
        self.revalidate_scanned_inventory(&scanned_owners, &mut after_final_enumeration)?;
        *run_dirs = scanned_run_dirs;
        tracing::info!(
            instances = instances.len(),
            "rehydrated instances from state_dir"
        );
        Ok(instances)
    }

    fn open_scanned_run_dir(&self, entry: &DirEntry, id: Uuid) -> Result<OwnedRunDir> {
        let name = entry.file_name().to_str().map_err(|_| {
            BlazeDaemonError::RecoveryRequired(format!(
                "owned state path for instance {id} has a non-UTF-8 name"
            ))
        })?;
        let inspected = statat(&self.inner.root, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        if !is_directory(&inspected) {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "owned state path {id} is not a directory"
            )));
        }
        if inspected.st_ino != entry.ino() {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "owned state path {id} is not the directory observed during the state scan"
            )));
        }
        let changed = format!("owned state path {id} changed while it was opened");
        let directory = open_inspected_object(
            &self.inner.root,
            name,
            &inspected,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            is_directory,
            &changed,
        )?;
        Ok(OwnedRunDir::new(
            id,
            self.inner.configured_root.join(id.to_string()),
            directory,
        ))
    }

    fn cached_run_dir(&self, id: Uuid) -> Result<Option<OwnedRunDir>> {
        let run_dirs =
            self.inner.run_dirs.lock().map_err(|_| {
                BlazeDaemonError::Internal("state run-directory lock poisoned".into())
            })?;
        match run_dirs.get(&id) {
            Some(RunDirEntry::Owned(run_dir)) => Ok(Some(run_dir.clone())),
            Some(RunDirEntry::Uncertain(_)) => Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime-directory publication for instance {id} is unconfirmed"
            ))),
            Some(RunDirEntry::Released) | None => Ok(None),
        }
    }

    #[cfg(test)]
    fn open_run_dir_object(&self, id: Uuid) -> Result<OwnedRunDir> {
        let name = id.to_string();
        let directory = openat(
            &self.inner.root,
            name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        Ok(OwnedRunDir::new(
            id,
            self.inner.configured_root.join(&name),
            directory,
        ))
    }

    fn publish_new_record(
        &self,
        run_dirs: &mut HashMap<Uuid, RunDirEntry>,
        instance: &SandboxInstance,
        json: &[u8],
    ) -> Result<()> {
        crate::failpoint::state("state-before-first-publication")?;
        let id = instance.id;
        let final_name = id.to_string();
        let staging_name = loop {
            let candidate = format!(".state-{id}-{}.tmp", Uuid::new_v4());
            match mkdirat(
                &self.inner.root,
                candidate.as_str(),
                Mode::from_bits_truncate(0o777),
            ) {
                Ok(()) => break candidate,
                Err(Errno::EXIST) => continue,
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
        };
        let directory = match openat(
            &self.inner.root,
            staging_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => directory,
            Err(error) => {
                let original = BlazeDaemonError::from(std::io::Error::from(error));
                let cleanup = remove_directory_if_exists(&self.inner.root, &staging_name);
                return Err(combine_publication_cleanup(original, cleanup));
            }
        };
        let run_dir = OwnedRunDir::new(id, self.inner.configured_root.join(&final_name), directory);
        let writer =
            run_dir.inner.writer.lock().map_err(|_| {
                BlazeDaemonError::Internal("state record writer lock poisoned".into())
            })?;
        if let Err(error) = self.write_record_locked(&run_dir, json) {
            let cleanup = self.discard_staging(&run_dir, &staging_name);
            return Err(retain_failed_publication(
                run_dirs, id, &run_dir, error, cleanup,
            ));
        }
        if let Err(error) = renameat_with(
            &self.inner.root,
            staging_name.as_str(),
            &self.inner.root,
            final_name.as_str(),
            RenameFlags::NOREPLACE,
        ) {
            let original = if error == Errno::EXIST {
                BlazeDaemonError::Conflict(format!(
                    "runtime directory for new instance {id} already exists"
                ))
            } else {
                BlazeDaemonError::from(std::io::Error::from(error))
            };
            let cleanup = self.discard_staging(&run_dir, &staging_name);
            return Err(retain_failed_publication(
                run_dirs, id, &run_dir, original, cleanup,
            ));
        }
        let linkage = crate::failpoint::state("state-post-publication-identity")
            .and_then(|_| self.linked_directory_matches(&final_name, &run_dir));
        match linkage {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                run_dirs.insert(id, RunDirEntry::Uncertain(run_dir.clone()));
                return Err(BlazeDaemonError::Conflict(format!(
                    "published runtime directory for new instance {id} changed identity"
                )));
            }
            Ok(None) => {
                run_dirs.insert(id, RunDirEntry::Uncertain(run_dir.clone()));
                return Err(BlazeDaemonError::Conflict(format!(
                    "published runtime directory for new instance {id} disappeared"
                )));
            }
            Err(error) => {
                run_dirs.insert(id, RunDirEntry::Uncertain(run_dir.clone()));
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "published runtime directory for new instance {id} could not be verified: \
                     {error}"
                )));
            }
        }
        run_dirs.insert(id, RunDirEntry::Owned(run_dir.clone()));
        crate::failpoint::state("state-first-publication-root-sync")?;
        if let Err(error) = fsync(&self.inner.root) {
            return Err(std::io::Error::from(error).into());
        }
        if instance.state == blaze_core::lifecycle::SandboxState::Destroyed {
            run_dirs.insert(id, RunDirEntry::Released);
        }
        drop(writer);
        Ok(())
    }

    fn write_record_locked(&self, run_dir: &OwnedRunDir, json: &[u8]) -> Result<()> {
        match unlinkat(&run_dir.inner.directory, TEMP_STATE_FILE, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        let temporary = openat(
            &run_dir.inner.directory,
            TEMP_STATE_FILE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o666),
        )
        .map_err(std::io::Error::from)?;
        let result = (|| -> Result<()> {
            let mut temporary = File::from(temporary);
            temporary.write_all(json)?;
            temporary.write_all(b"\n")?;
            temporary.sync_all()?;
            renameat(
                &run_dir.inner.directory,
                TEMP_STATE_FILE,
                &run_dir.inner.directory,
                STATE_FILE,
            )
            .map_err(std::io::Error::from)?;
            fsync(&run_dir.inner.directory).map_err(std::io::Error::from)?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(original) => {
                let cleanup = remove_file_if_exists(&run_dir.inner.directory, TEMP_STATE_FILE);
                Err(combine_record_cleanup(original, cleanup))
            }
        }
    }

    fn update_retention(&self, instance: &SandboxInstance, run_dir: &OwnedRunDir) -> Result<()> {
        let mut run_dirs =
            self.inner.run_dirs.lock().map_err(|_| {
                BlazeDaemonError::Internal("state run-directory lock poisoned".into())
            })?;
        if instance.state == blaze_core::lifecycle::SandboxState::Destroyed {
            if matches!(
                run_dirs.get(&instance.id),
                Some(RunDirEntry::Owned(retained)) if retained.same_object(run_dir)
            ) {
                run_dirs.insert(instance.id, RunDirEntry::Released);
            }
        } else {
            run_dirs.insert(instance.id, RunDirEntry::Owned(run_dir.clone()));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn retained_run_dir_count(&self) -> usize {
        self.inner
            .run_dirs
            .lock()
            .expect("state run-directory lock")
            .values()
            .filter(|entry| matches!(entry, RunDirEntry::Owned(_) | RunDirEntry::Uncertain(_)))
            .count()
    }

    fn discard_staging(&self, run_dir: &OwnedRunDir, staging_name: &str) -> Result<()> {
        crate::failpoint::state("state-before-staging-discard")?;
        match self.linked_directory_matches(staging_name, run_dir)? {
            Some(true) => {}
            Some(false) => {
                return Err(BlazeDaemonError::Conflict(format!(
                    "state staging entry {staging_name} changed identity before cleanup"
                )));
            }
            None => {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "state staging entry {staging_name} disappeared before cleanup"
                )));
            }
        }
        let mut errors = Vec::new();
        for name in [STATE_FILE, TEMP_STATE_FILE] {
            if let Err(error) = remove_file_if_exists(&run_dir.inner.directory, name) {
                errors.push(format!("remove {name}: {error}"));
            }
        }
        if let Err(error) = remove_directory_if_exists(&self.inner.root, staging_name) {
            errors.push(format!("remove {staging_name}: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(BlazeDaemonError::Internal(errors.join("; ")))
        }
    }

    fn linked_directory_matches(&self, name: &str, run_dir: &OwnedRunDir) -> Result<Option<bool>> {
        let linked = match openat(
            &self.inner.root,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(linked) => linked,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(std::io::Error::from(error).into()),
        };
        Ok(Some(same_opened_object(&linked, &run_dir.inner.directory)?))
    }

    fn revalidate_scanned_owner(
        &self,
        id: Uuid,
        run_dir: &OwnedRunDir,
        record_identity: &Stat,
    ) -> Result<()> {
        match self.linked_directory_matches(&id.to_string(), run_dir) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "owned state path {id} changed before inventory publication"
                )));
            }
            Ok(None) => {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "owned state path {id} disappeared before inventory publication"
                )));
            }
            Err(error) => {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "owned state path {id} could not be revalidated before inventory publication: {error}"
                )));
            }
        }
        let current_record = statat(
            &run_dir.inner.directory,
            STATE_FILE,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "owned state record for {id} could not be revalidated before inventory publication: {}",
                std::io::Error::from(error)
            ))
        })?;
        if !is_direct_state_record(&current_record)
            || !same_stat_object(record_identity, &current_record)
        {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "owned state record for {id} changed before inventory publication"
            )));
        }
        Ok(())
    }

    fn revalidate_scanned_inventory<F>(
        &self,
        scanned_owners: &[(Uuid, OwnedRunDir, Stat)],
        after_final_enumeration: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&[Uuid]) -> Result<()>,
    {
        let scanned_owners = scanned_owners
            .iter()
            .map(|(id, run_dir, record_identity)| (*id, (run_dir, record_identity)))
            .collect::<HashMap<_, _>>();
        let mut current_ids = Vec::with_capacity(scanned_owners.len());
        let entries = Dir::read_from(&self.inner.root).map_err(std::io::Error::from)?;
        for entry in entries {
            let entry = entry.map_err(std::io::Error::from)?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if is_state_staging_name(name) {
                continue;
            }
            let Ok(id) = Uuid::parse_str(name) else {
                continue;
            };
            if name != id.to_string() {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "owned state directory {name} changed the inventory before publication"
                )));
            }
            if !scanned_owners.contains_key(&id) {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "owned state directory {id} was added before inventory publication"
                )));
            }
            current_ids.push(id);
        }
        // Complete the name-set walk before checking any retained object's
        // identity so an early entry is not accepted while later names are
        // still being enumerated.
        let enumerated_ids = current_ids;
        let mut current_ids = enumerated_ids.clone();
        current_ids.sort_unstable();
        let mut scanned_ids = scanned_owners.keys().copied().collect::<Vec<_>>();
        scanned_ids.sort_unstable();
        if current_ids != scanned_ids {
            return Err(BlazeDaemonError::RecoveryRequired(
                "owned state directory set changed before inventory publication".into(),
            ));
        }
        after_final_enumeration(&enumerated_ids)?;
        // Set completeness is now established; validate every retained object
        // against the handles and record identity captured by the initial scan.
        for (id, (run_dir, record_identity)) in scanned_owners {
            self.revalidate_scanned_owner(id, run_dir, record_identity)?;
        }
        Ok(())
    }

    fn revalidate_uncertain(&self, id: Uuid, run_dir: &OwnedRunDir) -> Result<()> {
        let name = id.to_string();
        let linkage = crate::failpoint::state("state-post-publication-identity")
            .and_then(|_| self.linked_directory_matches(&name, run_dir));
        match linkage {
            Ok(Some(true)) => Ok(()),
            Ok(Some(false)) => Err(BlazeDaemonError::RecoveryRequired(format!(
                "published runtime directory for instance {id} has a different identity"
            ))),
            Ok(None) => Err(BlazeDaemonError::RecoveryRequired(format!(
                "published runtime directory for instance {id} is missing"
            ))),
            Err(error) => Err(BlazeDaemonError::RecoveryRequired(format!(
                "published runtime directory for instance {id} still cannot be verified: {error}"
            ))),
        }
    }

    fn remove_stale_staging(&self, staging_name: &str) -> Result<()> {
        let directory = openat(
            &self.inner.root,
            staging_name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let run_dir = OwnedRunDir::new(
            Uuid::nil(),
            self.inner.configured_root.join(staging_name),
            directory,
        );
        self.discard_staging(&run_dir, staging_name)
    }

    #[cfg(test)]
    fn load_from(run_dir: &OwnedRunDir) -> Result<SandboxInstance> {
        Self::load_from_with_identity(run_dir).map(|(instance, _)| instance)
    }

    fn load_from_with_identity(run_dir: &OwnedRunDir) -> Result<(SandboxInstance, Stat)> {
        let inspected = statat(
            &run_dir.inner.directory,
            STATE_FILE,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(std::io::Error::from)?;
        if !is_direct_state_record(&inspected) {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "owned state record for {} is not a direct regular file",
                run_dir.instance_id()
            )));
        }
        let changed = format!(
            "owned state record for {} changed while it was opened",
            run_dir.instance_id()
        );
        let state = open_inspected_object(
            &run_dir.inner.directory,
            STATE_FILE,
            &inspected,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            is_direct_state_record,
            &changed,
        )?;
        let mut state = File::from(state);
        let mut raw = Vec::new();
        state.read_to_end(&mut raw)?;
        Ok((serde_json::from_slice(&raw)?, inspected))
    }
}

fn is_directory(metadata: &Stat) -> bool {
    FileType::from_raw_mode(metadata.st_mode) == FileType::Directory
}

fn is_direct_state_record(metadata: &Stat) -> bool {
    FileType::from_raw_mode(metadata.st_mode) == FileType::RegularFile && metadata.st_nlink == 1
}

fn open_inspected_object(
    directory: &OwnedFd,
    name: &str,
    inspected: &Stat,
    flags: OFlags,
    expected_type: fn(&Stat) -> bool,
    changed: &str,
) -> Result<OwnedFd> {
    let object = openat(directory, name, flags, Mode::empty()).map_err(std::io::Error::from)?;
    let opened = fstat(&object).map_err(std::io::Error::from)?;
    if !expected_type(&opened) || !same_stat_object(inspected, &opened) {
        return Err(BlazeDaemonError::RecoveryRequired(changed.into()));
    }
    Ok(object)
}

fn same_stat_object(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn validate_terminal_record(instance: &SandboxInstance) -> Result<()> {
    if instance.state != SandboxState::Destroyed {
        return Ok(());
    }
    if matches!(
        instance.backend_ownership,
        BackendOwnership::NotStarted | BackendOwnership::Stopped
    ) && instance.operation.is_none()
    {
        return Ok(());
    }
    Err(BlazeDaemonError::RecoveryRequired(format!(
        "destroyed instance {} does not prove completed cleanup: backend ownership {:?}, active operation {:?}",
        instance.id,
        instance.backend_ownership,
        instance.operation.as_ref().map(|operation| operation.kind)
    )))
}

fn remove_file_if_exists(directory: &OwnedFd, name: &str) -> Result<()> {
    match unlinkat(directory, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

fn same_opened_object(left: &OwnedFd, right: &OwnedFd) -> Result<bool> {
    let left = fstat(left).map_err(std::io::Error::from)?;
    let right = fstat(right).map_err(std::io::Error::from)?;
    Ok(left.st_dev == right.st_dev && left.st_ino == right.st_ino)
}

fn remove_directory_if_exists(directory: &OwnedFd, name: &str) -> Result<()> {
    match unlinkat(directory, name, AtFlags::REMOVEDIR) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

fn combine_publication_cleanup(
    original: BlazeDaemonError,
    cleanup: Result<()>,
) -> BlazeDaemonError {
    match cleanup {
        Ok(()) => original,
        Err(cleanup) => BlazeDaemonError::Internal(format!(
            "{original}; unpublished state staging cleanup failed: {cleanup}"
        )),
    }
}

fn retain_failed_publication(
    run_dirs: &mut HashMap<Uuid, RunDirEntry>,
    id: Uuid,
    run_dir: &OwnedRunDir,
    original: BlazeDaemonError,
    cleanup: Result<()>,
) -> BlazeDaemonError {
    match cleanup {
        Ok(()) => original,
        Err(cleanup) => {
            run_dirs.insert(id, RunDirEntry::Uncertain(run_dir.clone()));
            BlazeDaemonError::RecoveryRequired(format!(
                "{original}; unpublished state staging cleanup failed: {cleanup}; \
                 runtime-directory owner retained for recovery"
            ))
        }
    }
}

fn combine_record_cleanup(original: BlazeDaemonError, cleanup: Result<()>) -> BlazeDaemonError {
    match cleanup {
        Ok(()) => original,
        Err(cleanup) => BlazeDaemonError::Internal(format!(
            "{original}; state record temporary-file cleanup failed: {cleanup}"
        )),
    }
}

fn is_state_staging_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix(".state-")
        .and_then(|body| body.strip_suffix(".tmp"))
    else {
        return false;
    };
    if body.len() != 73 || body.as_bytes().get(36) != Some(&b'-') {
        return false;
    }
    let Some(instance_id) = body.get(..36) else {
        return false;
    };
    let Some(nonce) = body.get(37..) else {
        return false;
    };
    Uuid::parse_str(instance_id).is_ok() && Uuid::parse_str(nonce).is_ok()
}

impl OwnedRunDir {
    fn new(instance_id: Uuid, configured_path: PathBuf, directory: OwnedFd) -> Self {
        #[cfg(target_os = "linux")]
        let stable_path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        #[cfg(not(target_os = "linux"))]
        let stable_path = configured_path.clone();
        Self {
            inner: Arc::new(OwnedRunDirInner {
                instance_id,
                configured_path,
                stable_path,
                directory,
                writer: Mutex::new(()),
            }),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.inner.stable_path
    }

    pub(crate) fn instance_id(&self) -> Uuid {
        self.inner.instance_id
    }

    /// Borrow the retained descriptor for this sandbox directory.
    ///
    /// Hibernation resolves its image directory relative to this descriptor so
    /// it never reopens a configured pathname after startup validation.
    pub(crate) fn descriptor(&self) -> &OwnedFd {
        &self.inner.directory
    }

    /// Report the configured pathname for diagnostics only.
    pub(crate) fn configured_path(&self) -> &Path {
        &self.inner.configured_path
    }

    fn same_object(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn inherit_into(&self, command: &mut tokio::process::Command) {
        use std::os::unix::process::CommandExt;

        let owner = self.clone();
        // SAFETY: `fcntl` is async-signal-safe. The closure only changes the
        // child-side copy of a descriptor retained through the spawn call.
        unsafe {
            command.as_std_mut().pre_exec(move || {
                let descriptor = owner.inner.directory.as_raw_fd();
                if libc::fcntl(descriptor, libc::F_SETFD, 0) == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn inherit_into(&self, _command: &mut tokio::process::Command) {}

    #[cfg(test)]
    pub(crate) fn for_test(instance_id: Uuid, path: PathBuf) -> Self {
        std::fs::create_dir_all(&path).expect("test runtime directory");
        let directory = open(
            &path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("open test runtime directory");
        Self::new(instance_id, path, directory)
    }
}

impl OwnedStateDirectory {
    pub(crate) fn new(configured_path: PathBuf, directory: OwnedFd) -> Self {
        Self {
            inner: Arc::new(OwnedStateDirectoryInner {
                configured_path,
                directory,
            }),
        }
    }

    pub(crate) fn configured_path(&self) -> &Path {
        &self.inner.configured_path
    }

    pub(crate) fn descriptor(&self) -> &OwnedFd {
        &self.inner.directory
    }
}

#[cfg(test)]
mod tests {
    use blaze_core::backend::BackendKind;
    use blaze_core::policy::WorkloadClass;

    use super::*;

    fn instance() -> SandboxInstance {
        SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:test".into(),
            "default".into(),
        )
    }

    fn scan_root(root: &Path) -> Result<HashMap<Uuid, SandboxInstance>> {
        StateStore::new(root.to_path_buf()).scan()
    }

    fn find_scanned_entry(store: &StateStore, id: Uuid) -> DirEntry {
        let expected = id.to_string();
        Dir::read_from(&store.inner.root)
            .expect("read state root")
            .map(|entry| entry.expect("state entry"))
            .find(|entry| entry.file_name().to_bytes() == expected.as_bytes())
            .expect("owned state entry")
    }

    #[test]
    fn scan_rejects_corrupt_owned_state() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let id = Uuid::new_v4();
        let owner = temporary.path().join(id.to_string());
        std::fs::create_dir(&owner).expect("owner directory");
        std::fs::write(owner.join(STATE_FILE), b"{not-json").expect("corrupt state");

        let error = scan_root(temporary.path()).expect_err("corrupt state must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&id.to_string())
                    && message.contains("cannot load persisted instance")
        ));
    }

    #[test]
    fn failed_scan_does_not_publish_a_partial_owner_inventory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first = instance();
        first.persist(temporary.path()).expect("first state");
        let second = instance();
        second.persist(temporary.path()).expect("second state");
        let store = StateStore::new(temporary.path().to_path_buf());
        let mut processed = Vec::new();
        let mut corrupted = None;

        let error = store
            .scan_with_owner_hook(|processed_id| {
                processed.push(processed_id);
                if processed.len() == 1 {
                    let pending_id = if processed_id == first.id {
                        second.id
                    } else {
                        assert_eq!(processed_id, second.id);
                        first.id
                    };
                    std::fs::write(
                        temporary
                            .path()
                            .join(pending_id.to_string())
                            .join(STATE_FILE),
                        b"{not-json",
                    )?;
                    corrupted = Some(pending_id);
                }
                Ok(())
            })
            .expect_err("corrupt pending state must stop the scan");

        assert_eq!(processed.len(), 1);
        let corrupted = corrupted.expect("one pending owner was corrupted");
        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&corrupted.to_string())
                    && message.contains("cannot load persisted instance")
        ));
        assert_eq!(store.retained_run_dir_count(), 0);
    }

    #[test]
    fn scan_rejects_a_processed_owner_replaced_before_publication() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let instance = instance();
        instance.persist(temporary.path()).expect("persisted state");
        let owner = temporary.path().join(instance.id.to_string());
        let displaced = temporary.path().join("displaced-owner");
        let store = StateStore::new(temporary.path().to_path_buf());

        let error = store
            .scan_with_owner_hook(|processed_id| {
                assert_eq!(processed_id, instance.id);
                std::fs::rename(&owner, &displaced)?;
                std::fs::create_dir(&owner)?;
                std::fs::copy(displaced.join(STATE_FILE), owner.join(STATE_FILE))?;
                Ok(())
            })
            .expect_err("a replaced processed owner must stop the scan");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&instance.id.to_string())
                    && message.contains("changed before inventory publication")
        ));
        assert_eq!(store.retained_run_dir_count(), 0);
        assert!(owner.join(STATE_FILE).is_file());
        assert!(displaced.join(STATE_FILE).is_file());
    }

    #[test]
    fn scan_rejects_a_processed_record_replaced_before_publication() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let instance = instance();
        instance.persist(temporary.path()).expect("persisted state");
        let owner = temporary.path().join(instance.id.to_string());
        let record = owner.join(STATE_FILE);
        let displaced = owner.join("state.read-by-scan.json");
        let replacement = instance.clone();
        let store = StateStore::new(temporary.path().to_path_buf());

        let error = store
            .scan_with_owner_hook(|processed_id| {
                assert_eq!(processed_id, instance.id);
                std::fs::rename(&record, &displaced)?;
                std::fs::write(&record, serde_json::to_vec_pretty(&replacement)?)?;
                Ok(())
            })
            .expect_err("a replaced processed record must stop the scan");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&instance.id.to_string())
                    && message.contains("record")
                    && message.contains("changed before inventory publication")
        ));
        assert_eq!(store.retained_run_dir_count(), 0);
        assert!(record.is_file());
        assert!(displaced.is_file());
    }

    #[test]
    fn scan_rejects_an_owner_added_before_publication() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first = instance();
        first
            .persist(temporary.path())
            .expect("first persisted state");
        let added = instance();
        let store = StateStore::new(temporary.path().to_path_buf());

        let error = store
            .scan_with_prevalidation_hook(|| {
                added.persist(temporary.path())?;
                Ok(())
            })
            .expect_err("an owner added after enumeration must stop the scan");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&added.id.to_string())
                    && message.contains("was added before inventory publication")
        ));
        assert_eq!(store.retained_run_dir_count(), 0);
        assert!(temporary.path().join(first.id.to_string()).is_dir());
        assert!(temporary.path().join(added.id.to_string()).is_dir());
    }

    #[test]
    fn scan_rejects_an_early_owner_replaced_after_final_enumeration() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first = instance();
        first.persist(temporary.path()).expect("first state");
        let second = instance();
        second.persist(temporary.path()).expect("second state");
        let store = StateStore::new(temporary.path().to_path_buf());
        let mut replaced = None;

        let error = store
            .scan_with_post_final_enumeration_hook(|enumerated_ids| {
                assert_eq!(enumerated_ids.len(), 2);
                let earlier_id = enumerated_ids[0];
                let owner = temporary.path().join(earlier_id.to_string());
                let displaced = temporary.path().join(format!("displaced-{earlier_id}"));
                std::fs::rename(&owner, &displaced)?;
                std::fs::create_dir(&owner)?;
                std::fs::copy(displaced.join(STATE_FILE), owner.join(STATE_FILE))?;
                replaced = Some((earlier_id, displaced));
                Ok(())
            })
            .expect_err("an owner replaced after the final enumeration must stop the scan");

        let (replaced_id, displaced) = replaced.expect("one enumerated owner was replaced");
        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&replaced_id.to_string())
                    && message.contains("changed before inventory publication")
        ));
        assert_eq!(store.retained_run_dir_count(), 0);
        assert!(
            temporary
                .path()
                .join(replaced_id.to_string())
                .join(STATE_FILE)
                .is_file()
        );
        assert!(displaced.join(STATE_FILE).is_file());
    }

    #[test]
    fn scan_rejects_an_early_record_replaced_after_final_enumeration() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first = instance();
        first.persist(temporary.path()).expect("first state");
        let second = instance();
        second.persist(temporary.path()).expect("second state");
        let store = StateStore::new(temporary.path().to_path_buf());
        let mut replaced = None;

        let error = store
            .scan_with_post_final_enumeration_hook(|enumerated_ids| {
                assert_eq!(enumerated_ids.len(), 2);
                let earlier_id = enumerated_ids[0];
                let owner = temporary.path().join(earlier_id.to_string());
                let record = owner.join(STATE_FILE);
                let displaced = owner.join("state.read-by-scan.json");
                std::fs::rename(&record, &displaced)?;
                std::fs::copy(&displaced, &record)?;
                replaced = Some((earlier_id, record, displaced));
                Ok(())
            })
            .expect_err("a record replaced after the final enumeration must stop the scan");

        let (replaced_id, record, displaced) =
            replaced.expect("one enumerated record was replaced");
        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&replaced_id.to_string())
                    && message.contains("record")
                    && message.contains("changed before inventory publication")
        ));
        assert_eq!(store.retained_run_dir_count(), 0);
        assert!(record.is_file());
        assert!(displaced.is_file());
    }

    #[test]
    fn scan_rejects_a_non_canonical_uuid_alias() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let valid = instance();
        valid.persist(temporary.path()).expect("valid state");
        let alias = valid.id.simple().to_string();
        let alias_owner = temporary.path().join(&alias);
        std::fs::create_dir(&alias_owner).expect("alias owner directory");
        std::fs::write(alias_owner.join(STATE_FILE), b"{not-json").expect("alias state");

        let error = scan_root(temporary.path()).expect_err("UUID alias must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&alias)
                    && message.contains(&valid.id.to_string())
                    && message.contains("non-canonical UUID alias")
        ));
    }

    #[test]
    fn scan_rejects_a_uuid_named_regular_file() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let id = Uuid::new_v4();
        std::fs::write(temporary.path().join(id.to_string()), b"not a directory")
            .expect("UUID-shaped file");

        let error = scan_root(temporary.path()).expect_err("UUID file must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&id.to_string()) && message.contains("is not a directory")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn scan_rejects_a_uuid_named_symbolic_link() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("unowned-target");
        std::fs::create_dir(&target).expect("target directory");
        let id = Uuid::new_v4();
        symlink(&target, temporary.path().join(id.to_string())).expect("UUID-shaped link");

        let error = scan_root(temporary.path()).expect_err("UUID link must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&id.to_string()) && message.contains("is not a directory")
        ));
    }

    #[test]
    fn scan_rejects_state_owned_by_a_different_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let stored = instance();
        stored.persist(temporary.path()).expect("persist state");
        let directory_id = Uuid::new_v4();
        std::fs::rename(
            temporary.path().join(stored.id.to_string()),
            temporary.path().join(directory_id.to_string()),
        )
        .expect("rename owner directory");

        let error = scan_root(temporary.path()).expect_err("mismatched ID must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&stored.id.to_string())
                    && message.contains(&directory_id.to_string())
        ));
    }

    #[test]
    fn scan_rejects_a_non_regular_state_record() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let id = Uuid::new_v4();
        let owner = temporary.path().join(id.to_string());
        std::fs::create_dir_all(owner.join(STATE_FILE)).expect("record directory");

        let error = scan_root(temporary.path()).expect_err("record directory must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&id.to_string())
                    && message.contains("cannot load persisted instance")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn scan_rejects_a_symbolic_link_state_record() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let external = tempfile::tempdir().expect("external directory");
        let stored = instance();
        stored.persist(external.path()).expect("external state");
        let owner = temporary.path().join(stored.id.to_string());
        std::fs::create_dir(&owner).expect("owner directory");
        symlink(
            external.path().join(stored.id.to_string()).join(STATE_FILE),
            owner.join(STATE_FILE),
        )
        .expect("record link");

        let error = scan_root(temporary.path()).expect_err("record link must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&stored.id.to_string())
                    && message.contains("cannot load persisted instance")
        ));
    }

    #[test]
    fn scan_rejects_a_hard_link_state_record() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let external = tempfile::tempdir().expect("external directory");
        let stored = instance();
        stored.persist(external.path()).expect("external state");
        let owner = temporary.path().join(stored.id.to_string());
        std::fs::create_dir(&owner).expect("owner directory");
        std::fs::hard_link(
            external.path().join(stored.id.to_string()).join(STATE_FILE),
            owner.join(STATE_FILE),
        )
        .expect("record hard link");

        let error = scan_root(temporary.path()).expect_err("hard link must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&stored.id.to_string())
                    && message.contains("cannot load persisted instance")
        ));
    }

    #[test]
    fn scan_rejects_destroyed_state_with_uncleared_ownership() {
        for ownership in [
            BackendOwnership::Unknown,
            BackendOwnership::Starting,
            BackendOwnership::Running,
        ] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let mut stored = instance();
            stored
                .transition(SandboxState::Destroyed)
                .expect("terminal transition");
            stored.backend_ownership = ownership;
            stored.persist(temporary.path()).expect("persist state");

            let error = scan_root(temporary.path())
                .expect_err("uncleared terminal ownership must stop startup");

            assert!(matches!(
                error,
                BlazeDaemonError::RecoveryRequired(message)
                    if message.contains(&stored.id.to_string())
                        && message.contains("does not prove completed cleanup")
            ));
        }
    }

    #[test]
    fn scan_rejects_destroyed_state_with_an_active_operation() {
        for operation in [
            blaze_core::lifecycle::OperationKind::Create,
            blaze_core::lifecycle::OperationKind::Destroy,
        ] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let mut stored = instance();
            stored.backend_ownership = BackendOwnership::Stopped;
            stored
                .transition(SandboxState::Destroyed)
                .expect("terminal transition");
            stored.begin_operation(operation);
            stored.persist(temporary.path()).expect("persist state");

            let error =
                scan_root(temporary.path()).expect_err("terminal operation must stop startup");

            assert!(matches!(
                error,
                BlazeDaemonError::RecoveryRequired(message)
                    if message.contains(&stored.id.to_string())
                        && message.contains(&format!("active operation Some({operation:?})"))
            ));
        }
    }

    #[test]
    fn scan_rejects_legacy_destroyed_state_without_ownership_evidence() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut stored = instance();
        stored
            .transition(SandboxState::Destroyed)
            .expect("terminal transition");
        let mut record = serde_json::to_value(&stored).expect("serialize state");
        let fields = record.as_object_mut().expect("state object");
        fields.remove("backend_ownership");
        fields.remove("operation");
        let owner = temporary.path().join(stored.id.to_string());
        std::fs::create_dir(&owner).expect("owner directory");
        std::fs::write(
            owner.join(STATE_FILE),
            serde_json::to_vec_pretty(&record).expect("serialize legacy state"),
        )
        .expect("write legacy state");

        let error = scan_root(temporary.path())
            .expect_err("legacy terminal state must prove ownership was released");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&stored.id.to_string())
                    && message.contains("backend ownership Unknown")
                    && message.contains("does not prove completed cleanup")
        ));
    }

    #[test]
    fn scan_accepts_destroyed_state_that_proves_cleanup() {
        for ownership in [BackendOwnership::NotStarted, BackendOwnership::Stopped] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let mut stored = instance();
            stored.backend_ownership = ownership;
            stored
                .transition(SandboxState::Destroyed)
                .expect("terminal transition");
            stored.persist(temporary.path()).expect("persist state");

            let records = scan_root(temporary.path()).expect("clean terminal state");

            assert_eq!(records[&stored.id].state, SandboxState::Destroyed);
        }
    }

    #[cfg(unix)]
    #[test]
    fn scan_rejects_an_owner_replaced_by_a_link_after_enumeration() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let external = tempfile::tempdir().expect("external directory");
        let stored = instance();
        stored.persist(temporary.path()).expect("local state");
        stored.persist(external.path()).expect("external state");
        let store = StateStore::new(temporary.path().to_path_buf());
        let entry = find_scanned_entry(&store, stored.id);
        let configured = temporary.path().join(stored.id.to_string());
        std::fs::rename(&configured, temporary.path().join("moved-owner"))
            .expect("move scanned owner");
        symlink(external.path().join(stored.id.to_string()), &configured)
            .expect("replace owner with link");

        let error = store
            .open_scanned_run_dir(&entry, stored.id)
            .expect_err("replacement link must not be followed");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&stored.id.to_string())
                    && message.contains("is not a directory")
        ));
    }

    #[test]
    fn scan_rejects_an_owner_replaced_after_enumeration() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let replacement = tempfile::tempdir().expect("replacement directory");
        let stored = instance();
        stored.persist(temporary.path()).expect("local state");
        stored
            .persist(replacement.path())
            .expect("replacement state");
        let store = StateStore::new(temporary.path().to_path_buf());
        let entry = find_scanned_entry(&store, stored.id);
        let configured = temporary.path().join(stored.id.to_string());
        std::fs::rename(&configured, temporary.path().join("moved-owner"))
            .expect("move scanned owner");
        std::fs::rename(replacement.path().join(stored.id.to_string()), &configured)
            .expect("replace owner directory");

        let error = store
            .open_scanned_run_dir(&entry, stored.id)
            .expect_err("replacement directory must be rejected");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains(&stored.id.to_string())
                    && message.contains("observed during the state scan")
        ));
    }

    #[test]
    fn owner_identity_check_rejects_replacement_between_inspect_and_open() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let replacement = tempfile::tempdir().expect("replacement directory");
        let stored = instance();
        stored.persist(temporary.path()).expect("local state");
        stored
            .persist(replacement.path())
            .expect("replacement state");
        let store = StateStore::new(temporary.path().to_path_buf());
        let name = stored.id.to_string();
        let inspected = statat(&store.inner.root, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .expect("inspect owner directory");
        let configured = temporary.path().join(&name);
        std::fs::rename(&configured, temporary.path().join("moved-owner"))
            .expect("move inspected owner");
        std::fs::rename(replacement.path().join(&name), &configured)
            .expect("replace inspected owner");
        let changed = format!("owned state path {} changed while it was opened", stored.id);

        let error = open_inspected_object(
            &store.inner.root,
            &name,
            &inspected,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            is_directory,
            &changed,
        )
        .expect_err("replacement directory must fail identity validation");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message) if message == changed
        ));
    }

    #[test]
    fn state_record_identity_check_rejects_replacement_between_inspect_and_open() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let stored = instance();
        stored.persist(temporary.path()).expect("stored state");
        let store = StateStore::new(temporary.path().to_path_buf());
        let owner = store
            .open_run_dir_object(stored.id)
            .expect("open owner directory");
        let inspected = statat(
            &owner.inner.directory,
            STATE_FILE,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .expect("inspect state record");
        let configured = temporary.path().join(stored.id.to_string());
        std::fs::rename(
            configured.join(STATE_FILE),
            configured.join("state.original.json"),
        )
        .expect("move inspected state record");
        let mut replacement = stored.clone();
        replacement.policy_name = "replacement".into();
        std::fs::write(
            configured.join(STATE_FILE),
            serde_json::to_vec_pretty(&replacement).expect("serialize replacement state"),
        )
        .expect("replace inspected state record");
        let changed = format!(
            "owned state record for {} changed while it was opened",
            stored.id
        );

        let error = open_inspected_object(
            &owner.inner.directory,
            STATE_FILE,
            &inspected,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            is_direct_state_record,
            &changed,
        )
        .expect_err("replacement record must fail identity validation");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message) if message == changed
        ));
    }

    #[test]
    fn record_lookup_stays_bound_to_the_opened_owner_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let replacement = tempfile::tempdir().expect("replacement directory");
        let stored = instance();
        stored.persist(temporary.path()).expect("original state");
        let mut replacement_record = stored.clone();
        replacement_record.policy_name = "replacement".into();
        replacement_record
            .persist(replacement.path())
            .expect("replacement state");
        let store = StateStore::new(temporary.path().to_path_buf());
        let entry = find_scanned_entry(&store, stored.id);
        let owner = store
            .open_scanned_run_dir(&entry, stored.id)
            .expect("open scanned owner");
        let configured = temporary.path().join(stored.id.to_string());
        std::fs::rename(&configured, temporary.path().join("moved-owner"))
            .expect("move opened owner");
        std::fs::rename(replacement.path().join(stored.id.to_string()), &configured)
            .expect("replace owner path");

        let loaded = StateStore::load_from(&owner).expect("load opened owner record");

        assert_eq!(loaded.policy_name, stored.policy_name);
    }

    #[test]
    fn store_centralizes_record_io_scan_and_run_directory_derivation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let instance = instance();

        store.persist(&instance).expect("persist instance");

        let loaded = store.load(instance.id).expect("load instance");
        assert_eq!(loaded.id, instance.id);
        let run_dir = store.run_dir(instance.id).expect("owned run directory");
        assert_eq!(
            std::fs::canonicalize(run_dir.path()).expect("resolve owned run directory"),
            std::fs::canonicalize(root.join(instance.id.to_string())).expect("resolve configured")
        );
        let scanned = StateStore::new(root).scan().expect("scan state store");
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[&instance.id].id, instance.id);
    }

    #[test]
    fn configured_root_replacement_does_not_redirect_record_io() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let configured = temporary.path().join("state");
        let owned = temporary.path().join("owned");
        std::fs::create_dir(&configured).expect("state directory");
        let store = StateStore::new(configured.clone());

        std::fs::rename(&configured, &owned).expect("move owned root");
        std::fs::create_dir(&configured).expect("replacement root");
        let instance = instance();
        store
            .persist(&instance)
            .expect("persist through owned root");

        assert!(
            owned
                .join(instance.id.to_string())
                .join(STATE_FILE)
                .is_file()
        );
        assert!(!configured.join(instance.id.to_string()).exists());
        assert_eq!(
            store.load(instance.id).expect("load owned record").id,
            instance.id
        );
    }

    #[test]
    fn opened_run_directory_replacement_does_not_redirect_record_io() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let mut instance = instance();
        store.persist(&instance).expect("initial persist");

        let configured_run_dir = root.join(instance.id.to_string());
        let owned_run_dir = root.join("owned-run-dir");
        std::fs::rename(&configured_run_dir, &owned_run_dir).expect("move owned run directory");
        std::fs::create_dir(&configured_run_dir).expect("replacement run directory");
        instance.policy_name = "updated".into();
        store
            .persist(&instance)
            .expect("persist through owned run directory");

        let owned: SandboxInstance = serde_json::from_slice(
            &std::fs::read(owned_run_dir.join(STATE_FILE)).expect("owned record"),
        )
        .expect("decode owned record");
        assert_eq!(owned.policy_name, "updated");
        assert!(!configured_run_dir.join(STATE_FILE).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_path_remains_attached_to_the_opened_run_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let instance = instance();
        store.persist(&instance).expect("initial persist");
        let runtime_owner = store.run_dir(instance.id).expect("runtime owner");

        let configured_run_dir = root.join(instance.id.to_string());
        let owned_run_dir = root.join("owned-run-dir");
        std::fs::rename(&configured_run_dir, &owned_run_dir).expect("move owned run directory");
        std::fs::create_dir(&configured_run_dir).expect("replacement run directory");
        std::fs::write(runtime_owner.path().join("backend.pid"), b"42\n")
            .expect("write through owned runtime path");

        assert_eq!(
            std::fs::read(owned_run_dir.join("backend.pid")).expect("owned backend marker"),
            b"42\n"
        );
        assert!(!configured_run_dir.join("backend.pid").exists());
    }

    #[test]
    fn first_publication_does_not_adopt_a_preexisting_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let instance = instance();
        let preexisting = root.join(instance.id.to_string());
        std::fs::create_dir(&preexisting).expect("preexisting directory");
        std::fs::write(preexisting.join("owner-marker"), b"external\n")
            .expect("preexisting marker");

        let error = store
            .persist(&instance)
            .expect_err("preexisting directory must not be adopted");

        assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        assert_eq!(
            std::fs::read(preexisting.join("owner-marker")).expect("unchanged marker"),
            b"external\n"
        );
        assert!(!preexisting.join(STATE_FILE).exists());
        assert!(
            std::fs::read_dir(&root)
                .expect("state entries")
                .all(|entry| !entry
                    .expect("state entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".state-"))
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn failed_staging_cleanup_retains_an_uncertain_owner() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let instance = instance();
        let preexisting = root.join(instance.id.to_string());
        std::fs::create_dir(&preexisting).expect("preexisting directory");
        std::fs::write(preexisting.join("owner-marker"), b"external\n")
            .expect("preexisting marker");
        let hook = crate::failpoint::TestFailpoint::new(&["state-before-staging-discard"]);

        let error = hook
            .run(async { store.persist(&instance) })
            .await
            .expect_err("failed staging cleanup must retain recovery ownership");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(
            error
                .to_string()
                .contains("runtime-directory owner retained for recovery")
        );
        assert!(
            store
                .has_run_dir_residual(instance.id)
                .expect("publication residual")
        );
        assert_eq!(store.retained_run_dir_count(), 1);
        assert!(matches!(
            store.run_dir(instance.id),
            Err(BlazeDaemonError::RecoveryRequired(_))
        ));
        assert_eq!(
            std::fs::read(preexisting.join("owner-marker")).expect("unchanged marker"),
            b"external\n"
        );
        assert!(!preexisting.join(STATE_FILE).exists());

        let staging = std::fs::read_dir(&root)
            .expect("state entries")
            .filter_map(|entry| entry.ok())
            .find(|entry| is_state_staging_name(&entry.file_name().to_string_lossy()))
            .expect("retained staging entry");
        assert!(staging.path().join(STATE_FILE).is_file());
    }

    #[test]
    fn linked_directory_identity_check_detects_replacement() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let id = Uuid::new_v4();
        let name = format!(".state-{id}-{}.tmp", Uuid::new_v4());
        let configured = root.join(&name);
        std::fs::create_dir(&configured).expect("staging directory");
        let directory = open(
            &configured,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("open staging directory");
        let owner = OwnedRunDir::new(id, configured.clone(), directory);

        assert_eq!(
            store
                .linked_directory_matches(&name, &owner)
                .expect("identity check"),
            Some(true)
        );

        std::fs::rename(&configured, root.join("moved-staging")).expect("move staging directory");
        std::fs::create_dir(&configured).expect("replacement staging directory");

        assert_eq!(
            store
                .linked_directory_matches(&name, &owner)
                .expect("replacement identity check"),
            Some(false)
        );
    }

    #[test]
    fn destroyed_commit_releases_cache_but_preserves_external_owner() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let mut instance = instance();
        store.persist(&instance).expect("initial persist");
        let runtime_owner = store.run_dir(instance.id).expect("runtime owner");

        instance
            .transition(blaze_core::lifecycle::SandboxState::Destroyed)
            .expect("destroy transition");
        store.persist(&instance).expect("terminal persist");

        assert_eq!(store.retained_run_dir_count(), 0);
        assert!(matches!(
            store.run_dir(instance.id),
            Err(BlazeDaemonError::NotFound(_))
        ));
        assert_eq!(
            store.load(instance.id).expect("load terminal record").state,
            blaze_core::lifecycle::SandboxState::Destroyed
        );
        assert_eq!(store.retained_run_dir_count(), 0);

        assert!(matches!(
            store.persist(&instance),
            Err(BlazeDaemonError::Conflict(_))
        ));
        assert_eq!(store.retained_run_dir_count(), 0);

        let configured_run_dir = root.join(instance.id.to_string());
        let owned_run_dir = root.join("owned-terminal-run-dir");
        std::fs::rename(&configured_run_dir, &owned_run_dir).expect("move terminal directory");
        std::fs::create_dir(&configured_run_dir).expect("replacement directory");
        std::fs::write(runtime_owner.path().join("runtime-marker"), b"owned\n")
            .expect("write through external owner");

        assert_eq!(
            std::fs::read(owned_run_dir.join("runtime-marker")).expect("owned marker"),
            b"owned\n"
        );
        assert!(!configured_run_dir.join("runtime-marker").exists());
    }

    #[test]
    fn scan_does_not_retain_terminal_run_directories() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let writer = StateStore::new(root.clone());
        let mut instance = instance();
        instance.backend_ownership = BackendOwnership::Stopped;
        instance
            .transition(blaze_core::lifecycle::SandboxState::Destroyed)
            .expect("destroy transition");
        writer.persist(&instance).expect("persist terminal record");

        let reader = StateStore::new(root);
        let scanned = reader.scan().expect("scan state store");

        assert_eq!(
            scanned[&instance.id].state,
            blaze_core::lifecycle::SandboxState::Destroyed
        );
        assert_eq!(reader.retained_run_dir_count(), 0);
        assert!(matches!(
            reader.persist(&instance),
            Err(BlazeDaemonError::Conflict(_))
        ));
        assert_eq!(reader.retained_run_dir_count(), 0);
        assert_eq!(
            reader
                .load(instance.id)
                .expect("reload repeated terminal record")
                .state,
            blaze_core::lifecycle::SandboxState::Destroyed
        );
    }

    #[test]
    fn scan_retains_nonterminal_run_directories() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let writer = StateStore::new(root.clone());
        let mut instance = instance();
        instance
            .transition(blaze_core::lifecycle::SandboxState::RecoveryRequired)
            .expect("recovery transition");
        writer.persist(&instance).expect("persist recovery record");
        drop(writer);

        let reader = StateStore::new(root);
        let scanned = reader.scan().expect("scan state store");

        assert_eq!(
            scanned[&instance.id].state,
            blaze_core::lifecycle::SandboxState::RecoveryRequired
        );
        assert_eq!(reader.retained_run_dir_count(), 1);
        assert!(reader.run_dir(instance.id).is_ok());
    }

    #[test]
    fn recovery_required_record_retains_its_runtime_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root);
        let mut instance = instance();
        instance
            .transition(blaze_core::lifecycle::SandboxState::RecoveryRequired)
            .expect("recovery transition");

        store.persist(&instance).expect("persist recovery record");

        assert_eq!(store.retained_run_dir_count(), 1);
        assert_eq!(
            store
                .run_dir(instance.id)
                .expect("recovery runtime owner")
                .instance_id(),
            instance.id
        );
    }

    #[test]
    fn startup_scan_removes_known_stale_state_temporaries() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let instance = instance();
        instance.persist(&root).expect("write lifecycle fixture");
        let record_temp = root.join(instance.id.to_string()).join(TEMP_STATE_FILE);
        std::fs::write(&record_temp, b"stale\n").expect("stale record temp");

        let staging_name = format!(".state-{}-{}.tmp", instance.id, Uuid::new_v4());
        let staging = root.join(&staging_name);
        std::fs::create_dir(&staging).expect("stale staging directory");
        std::fs::write(staging.join(STATE_FILE), b"stale\n").expect("stale staged state");
        std::fs::write(staging.join(TEMP_STATE_FILE), b"stale\n").expect("stale staged temp");

        let store = StateStore::new(root);
        let scanned = store.scan().expect("startup scan");

        assert_eq!(scanned[&instance.id].id, instance.id);
        assert!(!record_temp.exists());
        assert!(!staging.exists());
    }

    #[test]
    fn concurrent_commits_keep_record_and_retention_consistent() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root);

        for iteration in 0..16 {
            let mut active = instance();
            active.policy_name = format!("active-{iteration}");
            store.persist(&active).expect("initial active record");
            let id = active.id;

            let mut terminal = active.clone();
            terminal
                .transition(blaze_core::lifecycle::SandboxState::Destroyed)
                .expect("destroy transition");
            terminal.policy_name = format!("terminal-{iteration}");

            let barrier = Arc::new(std::sync::Barrier::new(3));
            let active_store = store.clone();
            let active_barrier = Arc::clone(&barrier);
            let active_thread = std::thread::spawn(move || {
                active_barrier.wait();
                active_store.persist(&active)
            });
            let terminal_store = store.clone();
            let terminal_barrier = Arc::clone(&barrier);
            let terminal_thread = std::thread::spawn(move || {
                terminal_barrier.wait();
                terminal_store.persist(&terminal)
            });
            barrier.wait();

            let active_result = active_thread.join().expect("active writer thread");
            terminal_thread
                .join()
                .expect("terminal writer thread")
                .expect("terminal persist");

            if let Err(error) = active_result {
                assert!(matches!(error, BlazeDaemonError::Conflict(_)));
            }
            assert_eq!(
                store.load(id).expect("load final record").state,
                blaze_core::lifecycle::SandboxState::Destroyed
            );
            assert!(matches!(
                store.run_dir(id),
                Err(BlazeDaemonError::NotFound(_))
            ));
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn child_inherits_the_owned_runtime_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let instance = instance();
        store.persist(&instance).expect("initial persist");
        let runtime_owner = store.run_dir(instance.id).expect("runtime owner");

        let configured_run_dir = root.join(instance.id.to_string());
        let owned_run_dir = root.join("owned-child-run-dir");
        std::fs::rename(&configured_run_dir, &owned_run_dir).expect("move owned run directory");
        std::fs::create_dir(&configured_run_dir).expect("replacement run directory");

        let marker = runtime_owner.path().join("child-marker");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("printf child > \"$1\"")
            .arg("sh")
            .arg(marker);
        runtime_owner.inherit_into(&mut command);
        drop(runtime_owner);
        drop(store);
        let status = command.status().await.expect("run child");

        assert!(status.success());
        assert_eq!(
            std::fs::read(owned_run_dir.join("child-marker")).expect("owned child marker"),
            b"child"
        );
        assert!(!configured_run_dir.join("child-marker").exists());
    }

    #[test]
    fn state_root_allows_only_one_production_owner() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let owner = StateStore::open(root.clone()).expect("first owner");

        let error = StateStore::open(root).expect_err("second owner must fail");

        assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        drop(owner);
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_catalog_retries_state_root_sync_when_existing() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        std::fs::create_dir(&root).expect("state directory");
        let store = StateStore::new(root.clone());
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-state-root-sync"]);

        let first_error = hook
            .run(async { store.checkpoint_directory() })
            .await
            .expect_err("initial state-root sync must fail");
        assert!(
            first_error
                .to_string()
                .contains("checkpoint-state-root-sync")
        );
        assert!(
            root.join(CHECKPOINT_DIRECTORY).is_dir(),
            "the failed parent sync leaves the newly created catalog"
        );

        let retry_error = hook
            .run(async { store.checkpoint_directory() })
            .await
            .expect_err("retry must synchronize the state root again");
        assert!(
            retry_error
                .to_string()
                .contains("checkpoint-state-root-sync")
        );

        store
            .checkpoint_directory()
            .expect("unarmed retry synchronizes the state root");
    }
}
