use std::collections::HashSet;
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::recommendation::personal_crypto::{derive_epoch_key, random_bytes, random_hex};
use crate::recommendation::personal_model::{
    CandidateSource, RecommendationState, RECOMMENDATION_SCHEMA_VERSION,
};
use crate::recommendation::personal_profile_policy::{
    rebuild_cache, reconcile_profile_snapshot_evidence, MAX_CACHE_CANDIDATES, MAX_SNAPSHOTS,
};

#[cfg(test)]
pub(crate) use super::personal_store_codec::AnalyzerGuardLease;
use super::personal_store_codec::{
    analyzer_guard_header, read_state_file, serialize_analyzer_guard, serialize_state,
};
#[path = "personal_store_prune.rs"]
mod prune;
pub(crate) use super::personal_store_codec::{read_analyzer_guard, AnalyzerGuardHeader};
use prune::prune_state;

pub(crate) const CURRENT_FILE: &str = "state.json";
pub(crate) const BACKUP_FILE: &str = "state.backup.json";
pub(crate) const LOCK_FILE: &str = "state.lock";
pub(crate) const KEY_FILE: &str = "recommendation.key";
pub(crate) const ANALYZER_GUARD_BYTES: usize = 1024;
const TEMP_FILE: &str = "state.tmp";
const BACKUP_TEMP_FILE: &str = "state.backup.tmp";
const QUARANTINE_FILE: &str = "state.quarantine";
const BACKUP_QUARANTINE_FILE: &str = "state.backup.quarantine";
pub(crate) const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_TOTAL_BYTES: u64 = 6 * 1024 * 1024;
const MAX_TRANSIENT_TOTAL_BYTES: u64 = MAX_TOTAL_BYTES + (3 * ANALYZER_GUARD_BYTES) as u64;
pub(crate) const MAX_JOURNAL_BYTES: usize = 1024 * 1024;
pub(crate) const JOURNAL_TTL_HOURS: u64 = 7 * 24;
const RECENT_TTL_HOURS: u64 = 14 * 24;
const FREQUENT_TTL_HOURS: u64 = 90 * 24;
const SNAPSHOT_TTL_HOURS: u64 = 90 * 24;
const MAX_JOURNAL_RECORDS: usize = 200;
const MAX_PROFILE_ITEMS: usize = 20;
const MAX_SNAPSHOT_BYTES: usize = 1024;
const MASTER_KEY_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoreError {
    Io(io::ErrorKind),
    CorruptState,
    UnsupportedSchema,
    UnsafePath(String),
    LockBusy,
    StateTooLarge,
    DiskLimitExceeded,
    InvalidKey,
    StaleState,
    Crypto,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(formatter, "recommendation store I/O failed: {kind:?}"),
            Self::CorruptState => formatter.write_str("recommendation state is corrupt"),
            Self::UnsupportedSchema => formatter.write_str("recommendation schema is unsupported"),
            Self::UnsafePath(path) => write!(formatter, "unsafe recommendation path: {path}"),
            Self::LockBusy => formatter.write_str("recommendation store lock is busy"),
            Self::StateTooLarge => formatter.write_str("recommendation state exceeds 2 MiB"),
            Self::DiskLimitExceeded => formatter.write_str("recommendation payloads exceed 6 MiB"),
            Self::InvalidKey => formatter.write_str("recommendation master key is invalid"),
            Self::StaleState => formatter.write_str("recommendation state version is stale"),
            Self::Crypto => formatter.write_str("recommendation cryptography failed"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        if matches!(error.raw_os_error(), Some(code) if code == nix::libc::EWOULDBLOCK || code == nix::libc::EAGAIN)
        {
            Self::LockBusy
        } else {
            Self::Io(error.kind())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StateVersion {
    pub(crate) store_epoch: String,
    pub(crate) generation: u64,
}

impl StateVersion {
    pub(crate) fn of(state: &RecommendationState) -> Self {
        Self {
            store_epoch: state.store_epoch.clone(),
            generation: state.generation,
        }
    }
}

pub(crate) struct PersonalStore {
    root: PathBuf,
}

impl PersonalStore {
    pub(crate) fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        ensure_directory(&root)?;
        Ok(Self { root })
    }

    pub(crate) fn initialize(
        &self,
        now_hour_bucket: u64,
    ) -> Result<RecommendationState, StoreError> {
        let _guard = self.lock()?;
        self.cleanup_transient_payloads()?;
        self.load_or_create_master_key()?;
        if let Some(mut state) = self.load_inner()? {
            let before = serde_json::to_vec(&state).map_err(|_| StoreError::CorruptState)?;
            prune_state(&mut state, now_hour_bucket)?;
            let after = serialize_state(&state)?;
            if before != after {
                self.atomic_write(&after)?;
            }
            return Ok(state);
        }
        let state = RecommendationState::empty(new_epoch()?, now_hour_bucket);
        self.atomic_write(&serialize_state(&state)?)?;
        Ok(state)
    }

    pub(crate) fn load(
        &self,
        now_hour_bucket: u64,
    ) -> Result<Option<RecommendationState>, StoreError> {
        let _guard = self.lock()?;
        self.cleanup_transient_payloads()?;
        let Some(mut state) = self.load_inner()? else {
            return Ok(None);
        };
        let before = serde_json::to_vec(&state).map_err(|_| StoreError::CorruptState)?;
        prune_state(&mut state, now_hour_bucket)?;
        let after = serialize_state(&state)?;
        if before != after {
            self.atomic_write(&after)?;
        }
        Ok(Some(state))
    }

    pub(crate) fn commit(
        &self,
        base: &StateVersion,
        mut next: RecommendationState,
        now_hour_bucket: u64,
    ) -> Result<RecommendationState, StoreError> {
        let _guard = self.lock()?;
        self.cleanup_transient_payloads()?;
        let current = self.load_inner()?.ok_or(StoreError::StaleState)?;
        ensure_current_version(&current, base)?;
        if next.store_epoch != base.store_epoch {
            return Err(StoreError::StaleState);
        }
        next.generation = current
            .generation
            .checked_add(1)
            .ok_or(StoreError::StaleState)?;
        next.updated_hour_bucket = now_hour_bucket;
        prune_state(&mut next, now_hour_bucket)?;
        self.atomic_write(&serialize_state(&next)?)?;
        Ok(next)
    }

    pub(crate) fn merge(
        &self,
        base: &StateVersion,
        now_hour_bucket: u64,
        update: impl FnOnce(&mut RecommendationState),
    ) -> Result<RecommendationState, StoreError> {
        let _guard = self.lock()?;
        self.cleanup_transient_payloads()?;
        let mut current = self.load_inner()?.ok_or(StoreError::StaleState)?;
        ensure_current_version(&current, base)?;
        let next_generation = current
            .generation
            .checked_add(1)
            .ok_or(StoreError::StaleState)?;
        update(&mut current);
        if current.store_epoch != base.store_epoch {
            return Err(StoreError::StaleState);
        }
        current.generation = next_generation;
        current.updated_hour_bucket = now_hour_bucket;
        prune_state(&mut current, now_hour_bucket)?;
        self.atomic_write(&serialize_state(&current)?)?;
        Ok(current)
    }

    pub(crate) fn clear(&self, now_hour_bucket: u64) -> Result<RecommendationState, StoreError> {
        let _guard = self.lock()?;
        self.cleanup_transient_files()?;
        let preferences = self
            .load_inner()?
            .map(|state| state.preferences)
            .unwrap_or_default();
        self.load_or_create_master_key()?;
        let mut state = RecommendationState::empty(new_epoch()?, now_hour_bucket);
        state.preferences = preferences;
        self.atomic_reset(&serialize_state(&state)?)?;
        Ok(state)
    }

    pub(crate) fn clear_if_current(
        &self,
        expected: &StateVersion,
        now_hour_bucket: u64,
    ) -> Result<RecommendationState, StoreError> {
        let _guard = self.lock()?;
        self.cleanup_transient_files()?;
        let current = self.load_inner()?.ok_or(StoreError::StaleState)?;
        ensure_current_version(&current, expected)?;
        let preferences = current.preferences;
        self.load_or_create_master_key()?;
        let mut state = RecommendationState::empty(new_epoch()?, now_hour_bucket);
        state.preferences = preferences;
        self.atomic_reset(&serialize_state(&state)?)?;
        Ok(state)
    }

    pub(crate) fn set_user_enabled(
        &self,
        enabled: bool,
        now_hour_bucket: u64,
    ) -> Result<RecommendationState, StoreError> {
        let _guard = self.lock()?;
        if enabled {
            self.cleanup_transient_payloads()?;
        } else {
            self.cleanup_transient_files()?;
        }
        self.load_or_create_master_key()?;
        let current = self
            .load_inner()?
            .unwrap_or_else(|| RecommendationState::empty(String::new(), now_hour_bucket));
        let mut preferences = current.preferences.clone();
        preferences.user_enabled = Some(enabled);
        let mut next = if enabled {
            let mut state = current;
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or(StoreError::StaleState)?;
            state
        } else {
            RecommendationState::empty(new_epoch()?, now_hour_bucket)
        };
        if next.store_epoch.is_empty() {
            next.store_epoch = new_epoch()?;
        }
        next.updated_hour_bucket = now_hour_bucket;
        next.preferences = preferences;
        prune_state(&mut next, now_hour_bucket)?;
        let bytes = serialize_state(&next)?;
        if enabled {
            self.atomic_write(&bytes)?;
        } else {
            self.atomic_reset(&bytes)?;
        }
        Ok(next)
    }

    pub(crate) fn mark_notice_seen(
        &self,
        notice_version: u16,
        now_hour_bucket: u64,
    ) -> Result<RecommendationState, StoreError> {
        let _guard = self.lock()?;
        self.cleanup_transient_payloads()?;
        let mut state = self.load_inner()?.ok_or(StoreError::StaleState)?;
        state.preferences.notice_version_seen = notice_version;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(StoreError::StaleState)?;
        state.updated_hour_bucket = now_hour_bucket;
        prune_state(&mut state, now_hour_bucket)?;
        self.atomic_write(&serialize_state(&state)?)?;
        Ok(state)
    }

    pub(crate) fn recover_corrupt_state(
        &self,
        user_enabled: bool,
        now_hour_bucket: u64,
    ) -> Result<RecommendationState, StoreError> {
        let _guard = self.lock()?;
        self.remove_payload_if_present(TEMP_FILE)?;
        self.remove_payload_if_present(BACKUP_TEMP_FILE)?;
        match self.load_inner() {
            Err(
                StoreError::CorruptState
                | StoreError::UnsupportedSchema
                | StoreError::StateTooLarge,
            ) => {}
            Ok(_) => return Err(StoreError::StaleState),
            Err(error) => return Err(error),
        }
        self.load_or_create_master_key()?;
        self.quarantine_payload(CURRENT_FILE, QUARANTINE_FILE)?;
        self.quarantine_payload(BACKUP_FILE, BACKUP_QUARANTINE_FILE)?;
        let mut state = RecommendationState::empty(new_epoch()?, now_hour_bucket);
        state.preferences.user_enabled = Some(user_enabled);
        self.atomic_write(&serialize_state(&state)?)?;
        Ok(state)
    }

    pub(crate) fn epoch_key(&self, store_epoch: &str) -> Result<[u8; 32], StoreError> {
        let _guard = self.lock()?;
        let master = self.load_or_create_master_key()?;
        derive_epoch_key(&master, store_epoch).map_err(|_| StoreError::Crypto)
    }

    fn lock(&self) -> Result<StoreLock, StoreError> {
        ensure_directory(&self.root)?;
        let path = self.root.join(LOCK_FILE);
        let file = open_owner_file(&path, true)?;
        let result =
            unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) };
        if result != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(StoreLock { file })
    }

    fn load_inner(&self) -> Result<Option<RecommendationState>, StoreError> {
        let current = self.root.join(CURRENT_FILE);
        match read_state_file(&current) {
            Ok(Some(state)) => Ok(Some(state)),
            Ok(None) => read_state_file(&self.root.join(BACKUP_FILE)),
            Err(original @ (StoreError::CorruptState | StoreError::UnsupportedSchema)) => {
                match read_state_file(&self.root.join(BACKUP_FILE))? {
                    Some(state) => Ok(Some(state)),
                    None => Err(original),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn load_or_create_master_key(&self) -> Result<Vec<u8>, StoreError> {
        let path = self.root.join(KEY_FILE);
        if path.exists() || fs::symlink_metadata(&path).is_ok() {
            let mut file = open_owner_file(&path, false)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            if bytes.len() != MASTER_KEY_BYTES {
                return Err(StoreError::InvalidKey);
            }
            return Ok(bytes);
        }
        let bytes = random_bytes(MASTER_KEY_BYTES).map_err(|_| StoreError::Crypto)?;
        let mut file = create_owner_file(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(bytes)
    }

    fn atomic_write(&self, bytes: &[u8]) -> Result<(), StoreError> {
        self.atomic_write_with_backup(bytes, true)
    }

    fn atomic_reset(&self, bytes: &[u8]) -> Result<(), StoreError> {
        self.atomic_write_with_backup(bytes, false)
    }

    fn atomic_write_with_backup(
        &self,
        bytes: &[u8],
        preserve_backup: bool,
    ) -> Result<(), StoreError> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(StoreError::StateTooLarge);
        }
        self.remove_payload_if_present(TEMP_FILE)?;
        self.remove_payload_if_present(BACKUP_TEMP_FILE)?;
        let state: RecommendationState =
            serde_json::from_slice(bytes).map_err(|_| StoreError::CorruptState)?;
        let guard = serialize_analyzer_guard(&analyzer_guard_header(&state))?;
        let temp = self.root.join(TEMP_FILE);
        let mut file = create_owner_file(&temp)?;
        file.write_all(&guard)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let parsed = read_state_file(&temp)?.ok_or(StoreError::CorruptState)?;
        if parsed.schema_version != RECOMMENDATION_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema);
        }
        if preserve_backup {
            self.enforce_total_limit()?;
        }

        let current = self.root.join(CURRENT_FILE);
        if preserve_backup && fs::symlink_metadata(&current).is_ok() {
            match read_state_file(&current) {
                Ok(Some(_))
                    if !self.root.join(QUARANTINE_FILE).exists()
                        && !self.root.join(BACKUP_QUARANTINE_FILE).exists() =>
                {
                    self.remove_payload_if_present(BACKUP_FILE)?;
                    let backup_temp = self.root.join(BACKUP_TEMP_FILE);
                    let mut source = open_owner_file(&current, false)?;
                    let mut backup = create_owner_file(&backup_temp)?;
                    io::copy(&mut source, &mut backup)?;
                    backup.sync_all()?;
                    drop(backup);
                    read_state_file(&backup_temp)?.ok_or(StoreError::CorruptState)?;
                    fs::rename(&backup_temp, self.root.join(BACKUP_FILE))?;
                }
                Err(StoreError::CorruptState | StoreError::UnsupportedSchema) => {
                    validate_file_path(&current)?;
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => return Err(error),
            }
        }
        fs::rename(&temp, &current)?;
        if !preserve_backup {
            for name in [
                BACKUP_FILE,
                BACKUP_TEMP_FILE,
                QUARANTINE_FILE,
                BACKUP_QUARANTINE_FILE,
            ] {
                self.remove_payload_if_present(name)?;
            }
        }
        sync_directory(&self.root)?;
        self.enforce_total_limit()
    }

    fn remove_payload_if_present(&self, name: &str) -> Result<(), StoreError> {
        let path = self.root.join(name);
        if fs::symlink_metadata(&path).is_err() {
            return Ok(());
        }
        validate_file_path(&path)?;
        fs::remove_file(path)?;
        Ok(())
    }

    fn quarantine_payload(&self, source: &str, destination: &str) -> Result<(), StoreError> {
        self.remove_payload_if_present(destination)?;
        let source = self.root.join(source);
        if fs::symlink_metadata(&source).is_err() {
            return Ok(());
        }
        validate_file_path(&source)?;
        fs::rename(source, self.root.join(destination))?;
        Ok(())
    }

    fn cleanup_transient_payloads(&self) -> Result<(), StoreError> {
        self.cleanup_transient_files()?;
        self.enforce_total_limit()
    }

    fn cleanup_transient_files(&self) -> Result<(), StoreError> {
        self.remove_payload_if_present(TEMP_FILE)?;
        self.remove_payload_if_present(BACKUP_TEMP_FILE)?;
        Ok(())
    }

    fn enforce_total_limit(&self) -> Result<(), StoreError> {
        match self.check_total_limit() {
            Err(StoreError::DiskLimitExceeded) => {
                self.remove_payload_if_present(QUARANTINE_FILE)?;
                self.remove_payload_if_present(BACKUP_QUARANTINE_FILE)?;
                self.check_total_limit()
            }
            result => result,
        }
    }

    fn check_total_limit(&self) -> Result<(), StoreError> {
        let mut total = 0u64;
        let mut count = 0usize;
        for name in [
            CURRENT_FILE,
            BACKUP_FILE,
            TEMP_FILE,
            BACKUP_TEMP_FILE,
            QUARANTINE_FILE,
            BACKUP_QUARANTINE_FILE,
        ] {
            let path = self.root.join(name);
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            validate_metadata(&path, &metadata, false)?;
            if !matches!(name, QUARANTINE_FILE | BACKUP_QUARANTINE_FILE)
                && metadata.len() > (MAX_STATE_BYTES + ANALYZER_GUARD_BYTES) as u64
            {
                return Err(StoreError::StateTooLarge);
            }
            total = total.saturating_add(metadata.len());
            count += 1;
        }
        let transient_present =
            self.root.join(TEMP_FILE).exists() || self.root.join(BACKUP_TEMP_FILE).exists();
        let total_limit = if transient_present {
            MAX_TRANSIENT_TOTAL_BYTES
        } else {
            MAX_TOTAL_BYTES
        };
        if total > total_limit || (count > 3 && !(count == 4 && transient_present)) {
            Err(StoreError::DiskLimitExceeded)
        } else {
            Ok(())
        }
    }
}

struct StoreLock {
    file: File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        unsafe {
            nix::libc::flock(self.file.as_raw_fd(), nix::libc::LOCK_UN);
        }
    }
}

fn ensure_current_version(
    state: &RecommendationState,
    expected: &StateVersion,
) -> Result<(), StoreError> {
    if state.store_epoch == expected.store_epoch && state.generation == expected.generation {
        Ok(())
    } else {
        Err(StoreError::StaleState)
    }
}

fn ensure_directory(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_metadata(path, &metadata, true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut missing = vec![path.to_path_buf()];
            let mut parent = path
                .parent()
                .ok_or_else(|| StoreError::UnsafePath(path.display().to_string()))?;
            while fs::symlink_metadata(parent).is_err() {
                missing.push(parent.to_path_buf());
                parent = parent
                    .parent()
                    .ok_or_else(|| StoreError::UnsafePath(path.display().to_string()))?;
            }
            validate_creation_parent(parent, &fs::symlink_metadata(parent)?)?;
            for directory in missing.into_iter().rev() {
                let mut builder = DirBuilder::new();
                builder.mode(0o700).create(&directory)?;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
                validate_metadata(&directory, &fs::symlink_metadata(&directory)?, true)?;
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_creation_parent(path: &Path, metadata: &fs::Metadata) -> Result<(), StoreError> {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(StoreError::UnsafePath(path.display().to_string()));
    }
    let euid = unsafe { nix::libc::geteuid() };
    let sticky = metadata.mode() & 0o1000 != 0;
    if metadata.uid() != euid && !sticky {
        return Err(StoreError::UnsafePath(path.display().to_string()));
    }
    Ok(())
}

fn validate_file_path(path: &Path) -> Result<(), StoreError> {
    validate_metadata(path, &fs::symlink_metadata(path)?, false)
}

fn validate_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    directory: bool,
) -> Result<(), StoreError> {
    let correct_type = if directory {
        metadata.file_type().is_dir()
    } else {
        metadata.file_type().is_file()
    };
    let euid = unsafe { nix::libc::geteuid() };
    if !correct_type
        || metadata.file_type().is_symlink()
        || metadata.uid() != euid
        || metadata.mode() & 0o077 != 0
        || (!directory && metadata.nlink() != 1)
    {
        Err(StoreError::UnsafePath(path.display().to_string()))
    } else {
        Ok(())
    }
}

pub(super) fn open_owner_file(path: &Path, create: bool) -> Result<File, StoreError> {
    if fs::symlink_metadata(path).is_ok() {
        validate_file_path(path)?;
    } else if !create {
        return Err(StoreError::Io(io::ErrorKind::NotFound));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    let file = options.open(path)?;
    validate_metadata(path, &file.metadata()?, false)?;
    Ok(file)
}

fn create_owner_file(path: &Path) -> Result<File, StoreError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)?;
    validate_metadata(path, &file.metadata()?, false)?;
    Ok(file)
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn new_epoch() -> Result<String, StoreError> {
    random_hex(32).map_err(|_| StoreError::Crypto)
}

#[cfg(test)]
#[path = "personal_store_tests.rs"]
mod tests;
