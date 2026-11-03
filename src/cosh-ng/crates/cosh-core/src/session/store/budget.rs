//! User-global budget location discovery; totals always come from current files.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use rustix::fs::{openat, Mode, OFlags};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::legacy::{open_directory_path_no_follow, workspace_owned_legacy_dir};
use super::{BudgetScope, SessionError, SessionStore};
use crate::config::DEFAULT_SESSION_PERSIST_DIR;
use crate::session::io::{
    expand_persist_dir, io_error, read_bounded_open_session_file, SessionLock,
};
use crate::session::scoped::ScopedStorage;
use crate::session::ProviderSessionId;

const INDEX_ID: &str = "00000000-0000-4000-8000-000000000000";
const INDEX_SCHEMA_VERSION: u32 = 3;
const LEGACY_OWNER_FILENAME: &str = ".cosh-legacy-owner";

#[derive(Serialize, Deserialize)]
struct LocationIndex {
    schema_version: u32,
    locations: Vec<Location>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Location {
    root: PathBuf,
    workspace: PathBuf,
    legacy: Vec<PathBuf>,
    legacy_generations: BTreeMap<PathBuf, LegacyGeneration>,
}

pub(super) struct BudgetNamespace {
    pub(super) path: PathBuf,
    current: Location,
}

impl BudgetNamespace {
    pub(super) fn new(root: &Path, workspace: &Path, legacy_candidate: Option<&Path>) -> Self {
        let legacy = legacy_candidate
            .map(Path::to_path_buf)
            .into_iter()
            .collect();
        Self {
            path: super::canonicalize_storage_root(&expand_persist_dir(
                DEFAULT_SESSION_PERSIST_DIR,
                workspace,
            ))
            .join(".budget"),
            current: Location {
                root: root.to_path_buf(),
                workspace: workspace.to_path_buf(),
                legacy,
                legacy_generations: BTreeMap::new(),
            },
        }
    }

    pub(super) fn lock(&self) -> Result<SessionLock, SessionError> {
        let storage = ScopedStorage::new(self.path.clone())?;
        let directory = storage.directory(true)?.ok_or_else(|| {
            io_error(
                "create user budget",
                &self.path,
                std::io::Error::other("missing directory"),
            )
        })?;
        storage.acquire_blocking_lock(&directory, super::BUDGET_LOCK_FILENAME)
    }

    fn locations(&self) -> Result<Vec<Location>, SessionError> {
        let storage = ScopedStorage::new(self.path.clone())?;
        let Some(directory) = storage.directory(false)? else {
            return Ok(Vec::new());
        };
        let id = ProviderSessionId::parse(INDEX_ID)?;
        let Some(file) = storage.open_session(&directory, &id)? else {
            return Ok(Vec::new());
        };
        let bytes = read_bounded_open_session_file(file, &storage.session_path(&id), &id)?;
        let index: LocationIndex = serde_json::from_slice(&bytes).map_err(|error| {
            io_error(
                "read budget locations",
                &storage.session_path(&id),
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            )
        })?;
        if index.schema_version != INDEX_SCHEMA_VERSION {
            return Err(io_error(
                "read budget locations",
                &storage.session_path(&id),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unsupported location index version",
                ),
            ));
        }
        let locations = index.locations;
        if locations.iter().any(|location| {
            !valid_location_path(&location.root)
                || !valid_location_path(&location.workspace)
                || location.legacy.iter().any(|path| {
                    !valid_location_path(path)
                        || !path.starts_with(&location.workspace)
                        || !location.legacy_generations.contains_key(path)
                })
        }) {
            return Err(io_error(
                "read budget locations",
                &storage.session_path(&id),
                std::io::Error::new(std::io::ErrorKind::InvalidData, "non-absolute location"),
            ));
        }
        Ok(locations)
    }

    pub(super) fn register(&self) -> Result<(), SessionError> {
        let mut locations = self.locations()?;
        let mut current = self.current.clone();
        current.legacy.retain(|path| {
            let Ok(workspace) = open_directory_path_no_follow(&current.workspace) else {
                return false;
            };
            let Some(directory) = workspace_owned_legacy_dir(&current.workspace, &workspace, path)
            else {
                return false;
            };
            let historical = locations
                .iter()
                .filter_map(|location| location.legacy_generations.get(path))
                .collect::<Vec<_>>();
            let generation = if historical.is_empty() {
                LegacyGeneration::claim(&directory.file)
            } else if historical
                .iter()
                .all(|generation| generation.matches(&directory.file))
            {
                Some(historical[0].clone())
            } else {
                None
            };
            if let Some(generation) = generation {
                current.legacy_generations.insert(path.clone(), generation);
                true
            } else {
                false
            }
        });
        if let Some(location) = locations.iter_mut().find(|location| {
            location.root == current.root && location.workspace == current.workspace
        }) {
            let before = location.clone();
            for path in current.legacy {
                if !location.legacy.contains(&path) {
                    location.legacy.push(path.clone());
                }
                location
                    .legacy_generations
                    .insert(path.clone(), current.legacy_generations[&path].clone());
            }
            if *location == before {
                return Ok(());
            }
        } else {
            locations.push(current);
        }
        let bytes = serde_json::to_vec(&LocationIndex {
            schema_version: INDEX_SCHEMA_VERSION,
            locations,
        })
        .map_err(|error| {
            io_error(
                "encode budget locations",
                &self.path,
                std::io::Error::other(error),
            )
        })?;
        if bytes.len() as u64 > crate::session::io::MAX_SESSION_FILE_BYTES {
            return Err(io_error(
                "encode budget locations",
                &self.path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "location index exceeds safety limit",
                ),
            ));
        }
        let storage = ScopedStorage::new(self.path.clone())?;
        let directory = storage.directory(true)?.ok_or_else(|| {
            io_error(
                "create budget locations",
                &self.path,
                std::io::Error::other("missing directory"),
            )
        })?;
        storage.write_atomic(&directory, &ProviderSessionId::parse(INDEX_ID)?, &bytes)
    }

    pub(super) fn scopes(&self) -> Result<Vec<BudgetScope>, SessionError> {
        let mut locations = self.locations()?;
        if !locations.contains(&self.current) {
            locations.push(self.current.clone())
        }
        let mut roots = locations
            .iter()
            .map(|location| location.root.clone())
            .collect::<HashSet<_>>();
        if let Some(root) = self.path.parent() {
            roots.insert(root.to_path_buf());
        }
        let mut scopes = BTreeMap::<PathBuf, BudgetScope>::new();
        for root in roots {
            match open_directory_path_no_follow(&root) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io_error("open budget root", &root, error)),
            }
            let directories = match fs::read_dir(&root) {
                Ok(directories) => directories,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io_error("list session root", &root, error)),
            };
            for entry in directories {
                let entry = entry.map_err(|error| io_error("list session root", &root, error))?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.len() != super::SCOPE_DIR_NAME_LEN
                    || !name.chars().all(|c| c.is_ascii_hexdigit())
                {
                    continue;
                }
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|error| io_error("inspect workspace scope", &path, error))?;
                if !metadata.is_dir() {
                    continue;
                }
                let entries = SessionStore::budget_entries(&path)?;
                scopes.insert(
                    path.clone(),
                    BudgetScope {
                        path: path.clone(),
                        entries,
                        activity: vec![path],
                        legacy: false,
                        modern: true,
                        legacy_generation: None,
                    },
                );
            }
        }
        for location in &locations {
            if location.legacy.is_empty() {
                continue;
            }
            let workspace = match open_directory_path_no_follow(&location.workspace) {
                Ok(workspace) => workspace,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(io_error(
                        "open budget workspace",
                        &location.workspace,
                        error,
                    ))
                }
            };
            for path in &location.legacy {
                let canonical = match fs::canonicalize(path) {
                    Ok(canonical) => canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(io_error("inspect legacy budget directory", path, error))
                    }
                };
                if !canonical.starts_with(&location.workspace) {
                    continue;
                }
                let metadata = fs::metadata(&canonical).map_err(|error| {
                    io_error("inspect legacy budget directory", &canonical, error)
                })?;
                if !metadata.is_dir() {
                    continue;
                }
                let legacy = workspace_owned_legacy_dir(&location.workspace, &workspace, path)
                    .ok_or_else(|| {
                        io_error(
                            "open legacy budget directory",
                            path,
                            std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "cannot pin qualified legacy directory",
                            ),
                        )
                    })?;
                let recorded = locations
                    .iter()
                    .filter_map(|location| location.legacy_generations.get(path))
                    .collect::<Vec<_>>();
                let current_candidate = self.current.workspace == location.workspace
                    && self.current.legacy.contains(path);
                let pinned_metadata = legacy.file.metadata().map_err(|error| {
                    io_error(
                        "inspect pinned legacy budget directory",
                        &legacy.path,
                        error,
                    )
                })?;
                let generation = if recorded.is_empty() {
                    if !current_candidate {
                        continue;
                    }
                    None
                } else if recorded
                    .iter()
                    .all(|generation| generation.matches_directory_metadata(&pinned_metadata))
                {
                    Some(recorded[0].clone())
                } else {
                    continue;
                };
                let activity = location.root.join(
                    &hex::encode(sha2::Sha256::digest(
                        location.workspace.to_string_lossy().as_bytes(),
                    ))[..super::SCOPE_DIR_NAME_LEN],
                );
                if let Some(scope) = scopes.get_mut(&legacy.path) {
                    scope.legacy = true;
                    scope.legacy_generation = generation;
                    if !scope.activity.contains(&activity) {
                        scope.activity.push(activity);
                    }
                    continue;
                }
                let entries = SessionStore::budget_entries(&legacy.path)?;
                scopes.insert(
                    legacy.path.clone(),
                    BudgetScope {
                        path: legacy.path,
                        entries,
                        activity: vec![activity],
                        legacy: true,
                        modern: false,
                        legacy_generation: generation,
                    },
                );
            }
        }
        Ok(scopes
            .into_values()
            .filter(|scope| !scope.entries.is_empty())
            .collect())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LegacyGeneration {
    device: u64,
    inode: u64,
    birth_time_ns: Option<u64>,
    owner: Option<String>,
}

impl LegacyGeneration {
    fn claim(directory: &fs::File) -> Option<Self> {
        let metadata = directory.metadata().ok()?;
        let pending = Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            birth_time_ns: directory_birth_time(&metadata),
            owner: None,
        };
        match openat(
            directory,
            LEGACY_OWNER_FILENAME,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(descriptor) => {
                let mut marker = fs::File::from(descriptor);
                if marker
                    .set_permissions(fs::Permissions::from_mode(0o600))
                    .is_err()
                    || marker
                        .write_all(ProviderSessionId::new().as_str().as_bytes())
                        .is_err()
                    || marker.sync_all().is_err()
                    || directory.sync_all().is_err()
                {
                    return Some(pending);
                }
            }
            Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Some(pending),
        }
        Some(Self::read(directory).unwrap_or(pending))
    }

    fn read(directory: &fs::File) -> Option<Self> {
        let descriptor = openat(
            directory,
            LEGACY_OWNER_FILENAME,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .ok()?;
        let marker = fs::File::from(descriptor);
        let metadata = marker.metadata().ok()?;
        if !metadata.is_file()
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() != 36
        {
            return None;
        }
        let mut owner = String::new();
        marker.take(37).read_to_string(&mut owner).ok()?;
        ProviderSessionId::parse(&owner).ok()?;
        let directory = directory.metadata().ok()?;
        Some(Self {
            device: directory.dev(),
            inode: directory.ino(),
            birth_time_ns: directory_birth_time(&directory),
            owner: Some(owner),
        })
    }

    pub(super) fn can_evict(&self, directory: &fs::File) -> bool {
        self.birth_time_ns.is_some() && self.owner.is_some() && self.matches(directory)
    }

    fn matches_directory_metadata(&self, metadata: &fs::Metadata) -> bool {
        metadata.dev() == self.device
            && metadata.ino() == self.inode
            && match (self.birth_time_ns, directory_birth_time(metadata)) {
                (Some(recorded), Some(current)) => recorded == current,
                _ => true,
            }
    }

    pub(super) fn matches(&self, directory: &fs::File) -> bool {
        if self.owner.is_none() {
            return directory
                .metadata()
                .is_ok_and(|metadata| self.matches_directory_metadata(&metadata));
        }
        Self::read(directory).is_some_and(|current| &current == self)
    }
}

fn directory_birth_time(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .created()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .try_into()
        .ok()
}

fn valid_location_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some()
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

pub(super) struct BudgetActivityGuard {
    storage: ScopedStorage,
    directory: fs::File,
    id: ProviderSessionId,
    live: SessionLock,
    generation: Option<SessionLock>,
    compaction: Option<SessionLock>,
}

impl BudgetActivityGuard {
    pub(super) fn new(
        storage: ScopedStorage,
        directory: fs::File,
        id: ProviderSessionId,
        live: SessionLock,
    ) -> Self {
        Self {
            storage,
            directory,
            id,
            live,
            generation: None,
            compaction: None,
        }
    }

    pub(super) fn acquire_compaction(&mut self) -> Result<(), SessionError> {
        self.compaction = Some(
            self.storage
                .acquire_compaction_lease(&self.directory, &self.id)?,
        );
        Ok(())
    }

    pub(super) fn acquire_generation(&mut self) -> Result<(), SessionError> {
        self.generation = Some(self.storage.acquire_lock(&self.directory, &self.id)?);
        Ok(())
    }
}

impl Drop for BudgetActivityGuard {
    fn drop(&mut self) {
        if let Err(error) =
            self.storage
                .remove_created_live_lease(&self.directory, &self.id, &self.live)
        {
            tracing::warn!("budget activity cleanup failed: {error}");
        }
        if let Some(compaction) = &self.compaction {
            if let Err(error) =
                self.storage
                    .remove_created_compaction_lease(&self.directory, &self.id, compaction)
            {
                tracing::warn!("budget compaction cleanup failed: {error}");
            }
        }
        if let Some(generation) = &self.generation {
            if let Err(error) =
                self.storage
                    .remove_created_generation_lock(&self.directory, &self.id, generation)
            {
                tracing::warn!("budget generation cleanup failed: {error}");
            }
        }
    }
}
