// SPDX-License-Identifier: Apache-2.0
//! Durable template artifact publication and lookup.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File};
#[cfg(test)]
use std::fs::{DirBuilder, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(test)]
use std::os::unix::fs::DirBuilderExt;
#[cfg(test)]
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use blaze_core::BlazeError;
use blaze_core::backend::{BackendKind, SnapshotKind};
use blaze_core::config::{PolicyLoadErrorMode, TemplateSection};
use blaze_core::error::ConfigErrorSource;
use blaze_core::storage::{TemplateArtifact, TemplateStorage};
use hyper::body::Bytes;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::{BlazeDaemonError, Result};
use crate::spawner::{HOST_NAMED_NETWORK_NAMESPACE_PATHS, HOST_NETWORK_COORDINATION_PATH};

use super::manager::SandboxManager;

const CATALOG_DIR_MODE: u32 = 0o700;
const CATALOG_FILE_MODE: u32 = 0o600;
const COPY_BUFFER_BYTES: usize = 1024 * 1024;
const LIST_RESPONSE_CONCURRENCY: usize = 1;
const ITEM_RESPONSE_CONCURRENCY: usize = 1;
// `sh` and `mount` join the set because the Firecracker launch now binds the
// sandbox's rootfs onto the portable snapshot-view path before exec.
const HOST_PATH_HELPERS: [&str; 7] = ["ip", "iptables", "kill", "mount", "sh", "sysctl", "unshare"];

#[derive(Clone, Copy)]
struct ImportLimits {
    max_files: usize,
    max_bytes: u64,
    max_metadata_bytes: u64,
    max_total_bytes: u64,
    max_entries: usize,
}

#[derive(Clone, Copy)]
struct CatalogBoundary {
    mount_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FilesystemLocation {
    device: (u64, u64),
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct MountEntry {
    device: (u64, u64),
    root: PathBuf,
    mount_point: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct MountTable {
    entries: Vec<MountEntry>,
}

struct CatalogUsage {
    bytes: u64,
    entries: usize,
}

#[derive(Clone)]
pub(crate) struct TemplateCatalog {
    inner: Arc<CatalogInner>,
}

/// Boot metadata a published `template.json` must carry for create to consume.
#[derive(Debug, Deserialize)]
struct TemplateManifest {
    format_version: u32,
    name: String,
    image_digest: String,
    backend: BackendKind,
    backend_version: Option<String>,
    resource_layout: Option<String>,
    boot_args: Option<String>,
    snapshot_kind: SnapshotKind,
    expose_guest_socket: bool,
    network: bool,
    vcpus: Option<u32>,
    memory_mib: Option<u64>,
    rootfs_size: u64,
    memory_size: u64,
    artifacts: Vec<TemplateArtifactManifest>,
}

#[derive(Debug, Deserialize)]
struct TemplateArtifactManifest {
    name: String,
    size_bytes: u64,
    sha256: String,
}

/// Validated launch semantics and stable artifact objects for one create.
///
/// Each artifact is an already-open file object, so the create path copies the
/// bytes the catalog validated even if a catalog path is replaced afterward.
#[derive(Debug)]
pub(super) struct ResolvedTemplate {
    pub(super) name: String,
    pub(super) image_digest: String,
    pub(super) backend: BackendKind,
    pub(super) backend_version: Option<String>,
    pub(super) boot_args: Option<String>,
    pub(super) snapshot_kind: SnapshotKind,
    pub(super) expose_guest_socket: bool,
    pub(super) network: bool,
    pub(super) vcpus: Option<u32>,
    pub(super) memory_mib: Option<u64>,
    pub(super) rootfs_size: u64,
    pub(super) memory_size: u64,
    pub(super) storage: TemplateStorage,
}

struct CatalogInner {
    root: File,
    import_root: Option<ImportRoot>,
    limits: ImportLimits,
    boundary: CatalogBoundary,
    state: Mutex<CatalogState>,
    list_permits: Arc<Semaphore>,
    item_permits: Arc<Semaphore>,
    active_count: watch::Sender<usize>,
    cancellation: CancellationToken,
    #[cfg(test)]
    copy_gate: Mutex<Option<Arc<TestCopyGate>>>,
    #[cfg(test)]
    list_gate: Mutex<Option<Arc<TestResponseGate>>>,
    #[cfg(test)]
    get_gate: Mutex<Option<Arc<TestResponseGate>>>,
    #[cfg(test)]
    fail_staging_open: AtomicBool,
    #[cfg(test)]
    fail_staging_cleanup: AtomicBool,
    #[cfg(test)]
    fail_cleanup_sync: AtomicBool,
}

#[derive(Debug)]
struct ImportRoot {
    directory: File,
    label: PathBuf,
}

#[derive(Debug)]
pub(crate) struct ValidatedTemplateRoots {
    root: File,
    import_root: Option<ImportRoot>,
    policy_load: PolicyLoadDisposition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PolicyLoadDisposition {
    LoadConfigured,
    UseEmpty,
}

impl PolicyLoadDisposition {
    fn combine(self, other: Self) -> Self {
        if self == Self::UseEmpty || other == Self::UseEmpty {
            Self::UseEmpty
        } else {
            Self::LoadConfigured
        }
    }
}

impl ValidatedTemplateRoots {
    pub(crate) fn policy_load_disposition(&self) -> PolicyLoadDisposition {
        self.policy_load
    }
}

#[derive(Debug)]
enum PinnedCatalogRoot {
    Existing(File),
    Missing(CatalogCreationPlan),
}

#[derive(Debug)]
struct CatalogCreationPlan {
    parent: File,
    parent_path: PathBuf,
    missing: Vec<(OsString, PathBuf)>,
}

/// Open daemon configuration object retained through catalog boundary checks.
pub(crate) struct PinnedConfigSource {
    file: File,
    configured_path: PathBuf,
    canonical_path: PathBuf,
}

impl PinnedConfigSource {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let absolute_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let configured_path = normalize_absolute_path(&absolute_path)?;
        let file =
            File::open(&absolute_path).map_err(|error| config_source_io("open", path, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| config_source_io("inspect", path, error))?;
        if !metadata.is_file() {
            return Err(invalid_config_source(path, "is not a regular file"));
        }

        // Capture the resolved name while retaining the object that supplied
        // the bytes. A later alias retarget cannot redirect validation, and a
        // rename of the resolved name is detected against the retained fd.
        let canonical_path = fs::canonicalize(&absolute_path)
            .map_err(|error| config_source_io("resolve", path, error))?;
        let source = Self {
            file,
            configured_path,
            canonical_path,
        };
        source.validate_identity()?;
        Ok(source)
    }

    pub(crate) fn read_to_string(&mut self) -> Result<String> {
        let mut raw = String::new();
        self.file
            .read_to_string(&mut raw)
            .map_err(|error| config_source_io("read", &self.canonical_path, error))?;
        Ok(raw)
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    fn configured_path(&self) -> &Path {
        &self.configured_path
    }

    fn validate_identity(&self) -> Result<()> {
        let retained = self
            .file
            .metadata()
            .map_err(|error| config_source_io("inspect", &self.canonical_path, error))?;
        let current = fs::metadata(&self.canonical_path).map_err(|error| {
            invalid_config_source(
                &self.canonical_path,
                &format!("changed while startup boundaries were validated: {error}"),
            )
        })?;
        if retained.dev() != current.dev() || retained.ino() != current.ino() {
            return Err(invalid_config_source(
                &self.canonical_path,
                "changed while startup boundaries were validated",
            ));
        }
        Ok(())
    }
}

struct CatalogState {
    active_names: HashSet<String>,
    committed_bytes: u64,
    reserved_bytes: u64,
    committed_entries: usize,
    reserved_entries: usize,
    stopping: bool,
    blocked: Option<String>,
}

struct ImportClaim {
    inner: Arc<CatalogInner>,
    name: String,
    reserved_bytes: u64,
    entry_reserved: bool,
}

struct PreparedFile {
    name: OsString,
    file: File,
    observed_bytes: u64,
    observed_dev: u64,
    observed_ino: u64,
    observed_mtime: i64,
    observed_mtime_nsec: i64,
    observed_ctime: i64,
    observed_ctime_nsec: i64,
}

struct PreparedImport {
    files: Vec<PreparedFile>,
    metadata: serde_json::Value,
    metadata_bytes: Vec<u8>,
    reserved_bytes: u64,
}

struct CatalogResponseOwner {
    body: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for CatalogResponseOwner {
    fn as_ref(&self) -> &[u8] {
        &self.body
    }
}

#[cfg(test)]
struct TestCopyGate {
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    release: AtomicBool,
}

#[cfg(test)]
struct TestResponseGate {
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    release: AtomicBool,
}

impl TemplateCatalog {
    pub(crate) fn open_validated(
        config: &TemplateSection,
        roots: ValidatedTemplateRoots,
    ) -> Result<Self> {
        let ValidatedTemplateRoots {
            root,
            import_root,
            policy_load: _,
        } = roots;
        Self::open_pinned(config, root, import_root)
    }

    #[cfg(test)]
    pub(crate) fn open(config: &TemplateSection) -> Result<Self> {
        reject_symlink_components(&config.dir, "template.dir")?;
        if let Some(import_root) = config.import_root.as_deref() {
            reject_symlink_components(import_root, "template.import_root")?;
        }
        let root = create_catalog_root(&config.dir)?;
        enforce_owned_mode_file(&root, true, CATALOG_DIR_MODE, &config.dir)?;
        let import_root = config
            .import_root
            .as_deref()
            .map(pin_import_root)
            .transpose()?;
        Self::open_pinned(config, root, import_root)
    }

    fn open_pinned(
        config: &TemplateSection,
        root: File,
        import_root: Option<ImportRoot>,
    ) -> Result<Self> {
        let limits = ImportLimits {
            max_files: config.max_files,
            max_bytes: config.max_bytes,
            max_metadata_bytes: config.max_metadata_bytes,
            max_total_bytes: config.max_total_bytes,
            max_entries: config.max_entries,
        };
        let boundary = CatalogBoundary {
            mount_id: opened_mount_id(&root)?,
        };
        Self::acquire_root_lock(&root)?;
        cleanup_staging(&root, limits, boundary)?;
        let usage = catalog_usage(&root, limits, boundary)?;
        if usage.bytes > limits.max_total_bytes {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template catalog uses {} bytes; configured limit is {}",
                usage.bytes, limits.max_total_bytes
            )));
        }
        if usage.entries > limits.max_entries {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template catalog has {} entries; configured limit is {}",
                usage.entries, limits.max_entries
            )));
        }
        let (active_count, _) = watch::channel(0);
        Ok(Self {
            inner: Arc::new(CatalogInner {
                root,
                import_root,
                limits,
                boundary,
                state: Mutex::new(CatalogState {
                    active_names: HashSet::new(),
                    committed_bytes: usage.bytes,
                    reserved_bytes: 0,
                    committed_entries: usage.entries,
                    reserved_entries: 0,
                    stopping: false,
                    blocked: None,
                }),
                list_permits: Arc::new(Semaphore::new(LIST_RESPONSE_CONCURRENCY)),
                item_permits: Arc::new(Semaphore::new(ITEM_RESPONSE_CONCURRENCY)),
                active_count,
                cancellation: CancellationToken::new(),
                #[cfg(test)]
                copy_gate: Mutex::new(None),
                #[cfg(test)]
                list_gate: Mutex::new(None),
                #[cfg(test)]
                get_gate: Mutex::new(None),
                #[cfg(test)]
                fail_staging_open: AtomicBool::new(false),
                #[cfg(test)]
                fail_staging_cleanup: AtomicBool::new(false),
                #[cfg(test)]
                fail_cleanup_sync: AtomicBool::new(false),
            }),
        })
    }

    // The root file is retained in `CatalogInner`, so this advisory lock
    // remains held for the complete catalog-owner lifetime.
    fn acquire_root_lock(root: &File) -> Result<()> {
        #[cfg(test)]
        {
            let inherited_descriptor_deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(100);
            loop {
                match Self::acquire_root_lock_once(root) {
                    Err(BlazeDaemonError::Conflict(_))
                        if std::time::Instant::now() < inherited_descriptor_deadline =>
                    {
                        // Concurrent test processes can briefly inherit a CLOEXEC
                        // descriptor between fork and exec. Production keeps the
                        // non-blocking single-attempt behavior.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    result => return result,
                }
            }
        }

        #[cfg(not(test))]
        Self::acquire_root_lock_once(root)
    }

    fn acquire_root_lock_once(root: &File) -> Result<()> {
        if unsafe { libc::flock(root.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }

        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EAGAIN)
            || error.raw_os_error() == Some(libc::EWOULDBLOCK)
        {
            return Err(BlazeDaemonError::Conflict(
                "runtime template catalog is already owned by another daemon".to_string(),
            ));
        }

        Err(BlazeDaemonError::Internal(format!(
            "could not lock runtime template catalog: {error}"
        )))
    }

    async fn list(&self) -> Result<Bytes> {
        let permit = Arc::clone(&self.inner.list_permits)
            .try_acquire_owned()
            .map_err(|_| {
                BlazeDaemonError::ServiceUnavailable(
                    "a runtime template catalog list response is already in progress".to_string(),
                )
            })?;
        let catalog = self.clone();
        let owner = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            wait_for_list_gate(&catalog.inner);
            let body = list_published(
                &catalog.inner.root,
                catalog.inner.limits,
                catalog.inner.boundary,
            )?;
            Ok::<_, BlazeDaemonError>(CatalogResponseOwner {
                body,
                _permit: permit,
            })
        })
        .await
        .map_err(join_error("runtime template list"))??;
        Ok(Bytes::from_owner(owner))
    }

    async fn get(&self, name: String) -> Result<Bytes> {
        validate_name(&name, "runtime template")?;
        let permit = Arc::clone(&self.inner.item_permits)
            .try_acquire_owned()
            .map_err(|_| {
                BlazeDaemonError::ServiceUnavailable(
                    "a runtime template item response is already in progress".to_string(),
                )
            })?;
        let catalog = self.clone();
        let owner = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            wait_for_response_gate(&catalog.inner.get_gate);
            let value = get_published(
                &catalog.inner.root,
                &name,
                catalog.inner.limits,
                catalog.inner.boundary,
            )?;
            let body = serde_json::to_vec_pretty(&value)?;
            Ok::<_, BlazeDaemonError>(CatalogResponseOwner {
                body,
                _permit: permit,
            })
        })
        .await
        .map_err(join_error("runtime template read"))??;
        Ok(Bytes::from_owner(owner))
    }

    /// Resolve one published template into validated launch semantics and open
    /// artifact objects for a single create request.
    async fn resolve_for_create(&self, name: String) -> Result<ResolvedTemplate> {
        validate_name(&name, "runtime template")?;
        let catalog = self.clone();
        tokio::task::spawn_blocking(move || {
            resolve_published(
                &catalog.inner.root,
                &name,
                catalog.inner.limits,
                catalog.inner.boundary,
            )
        })
        .await
        .map_err(join_error("runtime template resolve"))?
    }

    async fn import(
        &self,
        name: String,
        source: PathBuf,
        description: String,
    ) -> Result<serde_json::Value> {
        validate_name(&name, "runtime template")?;
        validate_relative_source(&source)?;
        if self.inner.import_root.is_none() {
            return Err(BlazeDaemonError::Conflict(
                "runtime template import is disabled; configure \
                 template.import_root"
                    .to_string(),
            ));
        }

        // Register before scheduling blocking work. Shutdown can therefore
        // observe and wait for an import even when the blocking pool has not
        // started its closure yet.
        let claim = ImportClaim::begin(Arc::clone(&self.inner), name.clone())?;
        let catalog = self.clone();
        tokio::task::spawn_blocking(move || {
            catalog.import_blocking(claim, name, source, description)
        })
        .await
        .map_err(join_error("runtime template import"))?
    }

    fn import_blocking(
        &self,
        mut claim: ImportClaim,
        name: String,
        source: PathBuf,
        description: String,
    ) -> Result<serde_json::Value> {
        check_cancelled(&self.inner.cancellation)?;
        let import_root =
            self.inner.import_root.as_ref().ok_or_else(|| {
                BlazeDaemonError::Conflict("runtime template import disabled".into())
            })?;
        if published_name_exists(&self.inner.root, &name, self.inner.boundary)? {
            return Err(BlazeDaemonError::Conflict(format!(
                "runtime template {name} already exists"
            )));
        }
        let source = open_import_source(import_root, &source)?;
        let prepared = prepare_import(
            &source,
            &name,
            &description,
            self.inner.limits,
            &self.inner.cancellation,
        )?;
        claim.reserve(prepared.reserved_bytes)?;
        publish_prepared(
            &self.inner.root,
            &name,
            prepared,
            &self.inner.cancellation,
            &mut claim,
        )
    }

    pub(super) fn cancel_imports(&self) {
        let mut state = lock_catalog_state(&self.inner);
        state.stopping = true;
        drop(state);
        self.inner.cancellation.cancel();
    }

    pub(super) async fn wait_for_imports(&self) -> Result<()> {
        let mut active = self.inner.active_count.subscribe();
        loop {
            if *active.borrow_and_update() == 0 {
                return Ok(());
            }
            active.changed().await.map_err(|_| {
                BlazeDaemonError::Internal(
                    "runtime template import supervisor closed unexpectedly".to_string(),
                )
            })?;
        }
    }

    #[cfg(test)]
    fn active_imports(&self) -> usize {
        *self.inner.active_count.borrow()
    }

    #[cfg(test)]
    fn install_copy_gate(&self) -> tokio::sync::mpsc::UnboundedReceiver<()> {
        let (entered, receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(TestCopyGate {
            entered,
            release: AtomicBool::new(false),
        });
        *self.inner.copy_gate.lock().expect("copy gate lock") = Some(gate);
        receiver
    }

    #[cfg(test)]
    fn install_list_gate(
        &self,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<()>,
        Arc<TestResponseGate>,
    ) {
        let (entered, receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(TestResponseGate {
            entered,
            release: AtomicBool::new(false),
        });
        *self.inner.list_gate.lock().expect("list gate lock") = Some(gate.clone());
        (receiver, gate)
    }

    #[cfg(test)]
    fn install_get_gate(
        &self,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<()>,
        Arc<TestResponseGate>,
    ) {
        let (entered, receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(TestResponseGate {
            entered,
            release: AtomicBool::new(false),
        });
        *self.inner.get_gate.lock().expect("get gate lock") = Some(gate.clone());
        (receiver, gate)
    }

    #[cfg(test)]
    fn fail_next_staging_open(&self) {
        self.inner.fail_staging_open.store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn fail_next_staging_cleanup(&self) {
        self.inner
            .fail_staging_cleanup
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn fail_next_cleanup_sync(&self) {
        self.inner.fail_cleanup_sync.store(true, Ordering::Release);
    }
}

impl ImportClaim {
    fn begin(inner: Arc<CatalogInner>, name: String) -> Result<Self> {
        let mut state = lock_catalog_state(&inner);
        if state.stopping {
            return Err(BlazeDaemonError::ServiceUnavailable(
                "runtime template imports are stopping".to_string(),
            ));
        }
        if let Some(error) = &state.blocked {
            return Err(BlazeDaemonError::RecoveryRequired(error.clone()));
        }
        if state.active_names.contains(&name) {
            return Err(BlazeDaemonError::Conflict(format!(
                "runtime template {name} import is already in progress"
            )));
        }
        let claimed_entries = state
            .committed_entries
            .checked_add(state.reserved_entries)
            .ok_or_else(|| {
                BlazeDaemonError::RecoveryRequired(
                    "runtime template catalog entry accounting overflow".to_string(),
                )
            })?;
        if claimed_entries >= inner.limits.max_entries {
            return Err(BlazeDaemonError::Conflict(format!(
                "runtime template catalog entry limit {} is exhausted",
                inner.limits.max_entries
            )));
        }
        state.active_names.insert(name.clone());
        state.reserved_entries += 1;
        let count = state.active_names.len();
        inner.active_count.send_replace(count);
        drop(state);
        Ok(Self {
            inner,
            name,
            reserved_bytes: 0,
            entry_reserved: true,
        })
    }

    fn reserve(&mut self, bytes: u64) -> Result<()> {
        let mut state = lock_catalog_state(&self.inner);
        if state.stopping || self.inner.cancellation.is_cancelled() {
            return Err(BlazeDaemonError::ServiceUnavailable(
                "runtime template imports are stopping".to_string(),
            ));
        }
        if let Some(error) = &state.blocked {
            return Err(BlazeDaemonError::RecoveryRequired(error.clone()));
        }
        let used = state
            .committed_bytes
            .checked_add(state.reserved_bytes)
            .and_then(|value| value.checked_add(bytes))
            .ok_or_else(|| payload_too_large(u64::MAX, self.inner.limits.max_total_bytes))?;
        if used > self.inner.limits.max_total_bytes {
            return Err(payload_too_large(used, self.inner.limits.max_total_bytes));
        }
        state.reserved_bytes += bytes;
        self.reserved_bytes = bytes;
        Ok(())
    }

    fn publish(&mut self, actual_bytes: u64) -> Result<()> {
        if actual_bytes > self.reserved_bytes {
            let message = format!(
                "runtime template {} wrote {actual_bytes} bytes beyond its {}-byte reservation",
                self.name, self.reserved_bytes
            );
            self.block_catalog(message.clone());
            return Err(BlazeDaemonError::RecoveryRequired(message));
        }
        let mut state = lock_catalog_state(&self.inner);
        let Some(remaining_reserved) = state.reserved_bytes.checked_sub(self.reserved_bytes) else {
            let message = "runtime template reservation accounting underflow".to_string();
            state.blocked = Some(message.clone());
            return Err(BlazeDaemonError::RecoveryRequired(message));
        };
        let Some(committed_bytes) = state.committed_bytes.checked_add(actual_bytes) else {
            let message = "runtime template catalog byte accounting overflow".to_string();
            state.blocked = Some(message.clone());
            return Err(BlazeDaemonError::RecoveryRequired(message));
        };
        let Some(remaining_reserved_entries) = state.reserved_entries.checked_sub(1) else {
            let message = "runtime template entry reservation accounting underflow".to_string();
            state.blocked = Some(message.clone());
            return Err(BlazeDaemonError::RecoveryRequired(message));
        };
        let Some(committed_entries) = state.committed_entries.checked_add(1) else {
            let message = "runtime template catalog entry accounting overflow".to_string();
            state.blocked = Some(message.clone());
            return Err(BlazeDaemonError::RecoveryRequired(message));
        };
        if committed_entries
            .checked_add(remaining_reserved_entries)
            .is_none_or(|entries| entries > self.inner.limits.max_entries)
        {
            let message = "runtime template catalog entry accounting exceeded the configured limit"
                .to_string();
            state.blocked = Some(message.clone());
            return Err(BlazeDaemonError::RecoveryRequired(message));
        }
        if committed_bytes
            .checked_add(remaining_reserved)
            .is_none_or(|used| used > self.inner.limits.max_total_bytes)
        {
            let message = "runtime template catalog accounting exceeded the configured total limit"
                .to_string();
            state.blocked = Some(message.clone());
            return Err(BlazeDaemonError::RecoveryRequired(message));
        }
        state.reserved_bytes = remaining_reserved;
        state.committed_bytes = committed_bytes;
        state.reserved_entries = remaining_reserved_entries;
        state.committed_entries = committed_entries;
        self.reserved_bytes = 0;
        self.entry_reserved = false;
        Ok(())
    }

    fn block_catalog(&self, message: String) {
        let mut state = lock_catalog_state(&self.inner);
        state.blocked = Some(message);
    }
}

impl Drop for ImportClaim {
    fn drop(&mut self) {
        let mut state = lock_catalog_state(&self.inner);
        if self.reserved_bytes > 0 {
            state.reserved_bytes = state.reserved_bytes.saturating_sub(self.reserved_bytes);
        }
        if self.entry_reserved {
            state.reserved_entries = state.reserved_entries.saturating_sub(1);
        }
        state.active_names.remove(&self.name);
        let count = state.active_names.len();
        self.inner.active_count.send_replace(count);
    }
}

impl SandboxManager {
    /// List lightweight summaries of atomically published runtime artifact sets.
    pub async fn list_templates(&self) -> Result<Bytes> {
        self.template_catalog.list().await
    }

    /// Read one published runtime artifact set by name.
    pub async fn get_template(&self, name: String) -> Result<Bytes> {
        self.template_catalog.get(name).await
    }

    /// Resolve one published template into validated launch inputs for create.
    pub(super) async fn resolve_template_for_create(
        &self,
        name: String,
    ) -> Result<ResolvedTemplate> {
        self.template_catalog.resolve_for_create(name).await
    }

    /// Copy and atomically publish one operator-prepared artifact directory.
    pub async fn import_template(
        &self,
        name: String,
        source: PathBuf,
        description: String,
    ) -> Result<serde_json::Value> {
        self.template_catalog
            .import(name, source, description)
            .await
    }

    /// Reject new imports and request cancellation of every active import.
    pub(crate) fn cancel_template_imports(&self) {
        self.template_catalog.cancel_imports();
    }

    /// Wait until every registered import has released its filesystem handles.
    pub(crate) async fn wait_for_template_imports(&self) -> Result<()> {
        self.template_catalog.wait_for_imports().await
    }
}

fn prepare_import(
    source: &File,
    name: &str,
    description: &str,
    limits: ImportLimits,
    cancellation: &CancellationToken,
) -> Result<PreparedImport> {
    let names = source_entry_names(source, limits.max_files)?;
    let mut files = Vec::with_capacity(names.len());
    let mut metadata_file = None;
    let mut artifact_bytes = 0_u64;

    for entry_name in names {
        check_cancelled(cancellation)?;
        validate_artifact_name(&entry_name)?;
        validate_source_entry_before_open(source, &entry_name)?;
        let file = openat_regular(source, &entry_name)?;
        let metadata = file.metadata()?;
        validate_source_file(&metadata, &entry_name)?;
        if entry_name == OsStr::new("template.json") {
            if metadata.len() > limits.max_metadata_bytes {
                return Err(payload_too_large(metadata.len(), limits.max_metadata_bytes));
            }
            metadata_file = Some(file);
        } else {
            artifact_bytes = artifact_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| payload_too_large(u64::MAX, limits.max_bytes))?;
            files.push(PreparedFile {
                name: entry_name,
                file,
                observed_bytes: metadata.len(),
                observed_dev: metadata.dev(),
                observed_ino: metadata.ino(),
                observed_mtime: metadata.mtime(),
                observed_mtime_nsec: metadata.mtime_nsec(),
                observed_ctime: metadata.ctime(),
                observed_ctime_nsec: metadata.ctime_nsec(),
            });
        }
    }

    let published_files = files.len() + 1;
    if published_files > limits.max_files {
        return Err(BlazeDaemonError::BadRequest(format!(
            "runtime template contains {published_files} files; limit is {}",
            limits.max_files
        )));
    }
    let present = files
        .iter()
        .map(|file| file.name.as_os_str())
        .collect::<HashSet<_>>();
    for required in ["vmstate.snap", "mem.bin", "rootfs.ext4"] {
        if !present.contains(OsStr::new(required)) {
            return Err(BlazeDaemonError::BadRequest(format!(
                "runtime template source is missing regular artifact {required}"
            )));
        }
    }

    let mut metadata = match metadata_file {
        Some(mut file) => {
            let observed = file.metadata()?;
            let metadata = read_json_bounded(&mut file, limits.max_metadata_bytes).map_err(
                |error| match error {
                    BlazeDaemonError::Json(source) => BlazeDaemonError::BadRequest(format!(
                        "template.json contains invalid JSON: {source}"
                    )),
                    other => other,
                },
            )?;
            let current = file.metadata()?;
            if !same_file_identity(&observed, &current) {
                return Err(BlazeDaemonError::BadRequest(
                    "runtime template source metadata changed while it was imported".to_string(),
                ));
            }
            metadata
        }
        None => json!({"name": name}),
    };
    if !metadata.is_object() {
        return Err(BlazeDaemonError::BadRequest(
            "template.json must contain a JSON object".to_string(),
        ));
    }
    metadata["name"] = json!(name);
    if !description.is_empty() {
        metadata["description"] = json!(description);
    }
    if metadata
        .get("rootfs_size")
        .and_then(serde_json::Value::as_u64)
        .is_none()
    {
        metadata["rootfs_size"] = json!(8_u64 * 1024 * 1024 * 1024);
    }
    if metadata
        .get("memory_size")
        .and_then(serde_json::Value::as_u64)
        .is_none()
    {
        metadata["memory_size"] = json!(4_u64 * 1024 * 1024 * 1024);
    }
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)?;
    let metadata_len = u64::try_from(metadata_bytes.len()).unwrap_or(u64::MAX);
    if metadata_len > limits.max_metadata_bytes {
        return Err(payload_too_large(metadata_len, limits.max_metadata_bytes));
    }
    let reserved_bytes = artifact_bytes
        .checked_add(metadata_len)
        .ok_or_else(|| payload_too_large(u64::MAX, limits.max_bytes))?;
    if reserved_bytes > limits.max_bytes {
        return Err(payload_too_large(reserved_bytes, limits.max_bytes));
    }

    Ok(PreparedImport {
        files,
        metadata,
        metadata_bytes,
        reserved_bytes,
    })
}

fn publish_prepared(
    root: &File,
    name: &str,
    prepared: PreparedImport,
    cancellation: &CancellationToken,
    claim: &mut ImportClaim,
) -> Result<serde_json::Value> {
    let staging_name = OsString::from(format!(".import-{name}-{}.tmp", Uuid::new_v4()));
    let staging = create_staging_directory_at(root, &staging_name, claim)?;
    #[cfg(test)]
    wait_for_copy_gate(&claim.inner, cancellation);
    let result = populate_and_publish(
        root,
        &staging,
        &staging_name,
        name,
        prepared,
        cancellation,
        claim,
    );
    let original_error = match result {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    if let Err(cleanup_error) = remove_failed_import_staging(
        root,
        &staging_name,
        claim.inner.limits.max_files,
        &claim.inner,
    ) {
        let message = format!(
            "runtime template import failed and staging cleanup could not be confirmed; restart \
             after repairing the catalog: import error: {original_error}; cleanup error: \
             {cleanup_error}"
        );
        claim.block_catalog(message.clone());
        tracing::error!(
            path = %staging_name.to_string_lossy(),
            import_error = %original_error,
            cleanup_error = %cleanup_error,
            "runtime template staging cleanup failed"
        );
        return Err(BlazeDaemonError::RecoveryRequired(message));
    }
    if let Err(sync_error) = sync_failed_import_cleanup(root, &claim.inner) {
        let message = format!(
            "runtime template import failed and cleanup durability is unknown; restart after \
             repairing the catalog: import error: {original_error}; sync error: {sync_error}"
        );
        claim.block_catalog(message.clone());
        tracing::error!(
            import_error = %original_error,
            sync_error = %sync_error,
            "runtime template cleanup durability is unknown"
        );
        return Err(BlazeDaemonError::RecoveryRequired(message));
    }
    Err(original_error)
}

fn remove_failed_import_staging(
    root: &File,
    staging_name: &OsStr,
    max_files: usize,
    inner: &CatalogInner,
) -> Result<bool> {
    #[cfg(test)]
    if inner.fail_staging_cleanup.swap(false, Ordering::AcqRel) {
        return Err(BlazeDaemonError::Io(io::Error::from_raw_os_error(
            libc::EIO,
        )));
    }
    remove_staging_directory(root, staging_name, max_files, inner.boundary)
}

fn sync_failed_import_cleanup(root: &File, inner: &CatalogInner) -> Result<()> {
    #[cfg(test)]
    if inner.fail_cleanup_sync.swap(false, Ordering::AcqRel) {
        return Err(BlazeDaemonError::Io(io::Error::from_raw_os_error(
            libc::EIO,
        )));
    }
    #[cfg(not(test))]
    let _ = inner;
    sync_directory_file(root)
}

fn populate_and_publish(
    root: &File,
    staging: &File,
    staging_name: &OsStr,
    name: &str,
    mut prepared: PreparedImport,
    cancellation: &CancellationToken,
    claim: &mut ImportClaim,
) -> Result<serde_json::Value> {
    let mut actual_bytes = 0_u64;
    for source in &mut prepared.files {
        check_cancelled(cancellation)?;
        let remaining = prepared
            .reserved_bytes
            .checked_sub(actual_bytes)
            .and_then(|value| {
                value.checked_sub(u64::try_from(prepared.metadata_bytes.len()).unwrap_or(u64::MAX))
            })
            .ok_or_else(|| payload_too_large(u64::MAX, prepared.reserved_bytes))?;
        let copied = copy_regular_file_at(
            &mut source.file,
            staging,
            &source.name,
            remaining,
            cancellation,
        )?;
        let current = source.file.metadata()?;
        if copied != source.observed_bytes
            || current.len() != source.observed_bytes
            || current.dev() != source.observed_dev
            || current.ino() != source.observed_ino
            || current.mtime() != source.observed_mtime
            || current.mtime_nsec() != source.observed_mtime_nsec
            || current.ctime() != source.observed_ctime
            || current.ctime_nsec() != source.observed_ctime_nsec
        {
            return Err(BlazeDaemonError::BadRequest(format!(
                "runtime template source file {} changed while it was imported",
                source.name.to_string_lossy()
            )));
        }
        actual_bytes = actual_bytes
            .checked_add(copied)
            .ok_or_else(|| payload_too_large(u64::MAX, prepared.reserved_bytes))?;
    }

    let metadata_len = u64::try_from(prepared.metadata_bytes.len()).unwrap_or(u64::MAX);
    actual_bytes = actual_bytes
        .checked_add(metadata_len)
        .ok_or_else(|| payload_too_large(u64::MAX, prepared.reserved_bytes))?;
    if actual_bytes > prepared.reserved_bytes {
        return Err(payload_too_large(actual_bytes, prepared.reserved_bytes));
    }
    write_file_durable_at(
        staging,
        OsStr::new("template.json"),
        &prepared.metadata_bytes,
    )?;
    sync_directory_file(staging)?;
    check_cancelled(cancellation)?;

    rename_no_replace_at(root, staging_name, OsStr::new(name)).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            BlazeDaemonError::Conflict(format!("runtime template {name} already exists"))
        } else {
            error.into()
        }
    })?;

    // The directory is now publicly owned even if the parent fsync fails.
    // Account for it before reporting an uncertain durability result.
    claim.publish(actual_bytes)?;
    if let Err(error) = sync_directory_file(root) {
        let message = format!(
            "runtime template {name} was published but catalog durability is unknown: {error}"
        );
        claim.block_catalog(message.clone());
        return Err(BlazeDaemonError::RecoveryRequired(message));
    }
    Ok(prepared.metadata)
}

fn pin_import_root(path: &Path) -> Result<ImportRoot> {
    let directory = open_directory_path_no_follow(path).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot pin runtime template import root {}: {error}",
            path.display()
        ))
    })?;
    validate_source_directory(&directory.metadata()?, path)?;
    Ok(ImportRoot {
        directory,
        label: path.to_path_buf(),
    })
}

fn open_import_source(import_root: &ImportRoot, relative: &Path) -> Result<File> {
    // Start every lookup from the object pinned during startup. Reopening the
    // configured path would let a later ancestor rename redirect imports.
    let mut directory =
        openat_directory(&import_root.directory, OsStr::new(".")).map_err(|error| {
            BlazeDaemonError::BadRequest(format!(
                "cannot open configured runtime template import root {}: {error}",
                import_root.label.display()
            ))
        })?;
    validate_source_directory(&directory.metadata()?, &import_root.label)?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(BlazeDaemonError::BadRequest(
                "runtime template source must be a non-empty relative path below the configured \
                 import root"
                    .to_string(),
            ));
        };
        directory = openat_directory(&directory, name).map_err(|error| {
            BlazeDaemonError::BadRequest(format!(
                "cannot open runtime template source {}: {error}",
                relative.display()
            ))
        })?;
        validate_source_directory(&directory.metadata()?, relative)?;
    }
    Ok(directory)
}

fn source_entry_names(directory: &File, max_entries: usize) -> Result<Vec<OsString>> {
    // Open a fresh directory description so readdir does not advance the
    // retained catalog handle's shared directory offset.
    let descriptor = openat_directory(directory, OsStr::new("."))?.into_raw_fd();
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(descriptor);
        }
        return Err(error.into());
    }
    let mut names = Vec::new();
    loop {
        clear_errno();
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                unsafe {
                    libc::closedir(stream);
                }
                return Err(error.into());
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            if names.len() == max_entries {
                unsafe {
                    libc::closedir(stream);
                }
                return Err(BlazeDaemonError::BadRequest(format!(
                    "runtime template contains more than {max_entries} source entries"
                )));
            }
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
    let close_result = unsafe { libc::closedir(stream) };
    if close_result != 0 {
        return Err(io::Error::last_os_error().into());
    }
    names.sort();
    Ok(names)
}

#[cfg(target_os = "linux")]
fn clear_errno() {
    unsafe {
        *libc::__errno_location() = 0;
    }
}

#[cfg(not(target_os = "linux"))]
fn clear_errno() {
    unsafe {
        *libc::__error() = 0;
    }
}

fn open_directory_no_follow(path: &Path) -> io::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    file_from_fd(fd)
}

fn openat_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    file_from_fd(fd)
}

fn open_directory_path_no_follow(path: &Path) -> io::Result<File> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path is empty",
        ));
    }

    let anchor = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let directory = open_directory_no_follow(anchor)?;
    open_directory_components_no_follow(directory, path)
}

fn open_directory_components_no_follow(mut directory: File, path: &Path) -> io::Result<File> {
    let mut saw_name = false;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                saw_name = true;
                directory = openat_directory(&directory, name)?;
            }
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "parent-directory components are not allowed",
                ));
            }
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported path prefix",
                ));
            }
        }
    }
    if !saw_name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must name a directory",
        ));
    }
    Ok(directory)
}

fn descriptor_metadata(file: &File) -> io::Result<libc::stat> {
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { metadata.assume_init() })
}

fn not_regular_entry(name: &OsStr) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("entry {} is not a regular file", name.to_string_lossy()),
    )
}

#[cfg(target_os = "linux")]
fn not_directory_entry(name: &OsStr) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("entry {} is not a directory", name.to_string_lossy()),
    )
}

#[cfg(target_os = "linux")]
struct PinnedRegularEntry {
    descriptor: File,
    name: OsString,
}

#[cfg(target_os = "linux")]
impl PinnedRegularEntry {
    fn pin(parent: &File, name: &OsStr) -> io::Result<Self> {
        let raw_name = CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                raw_name.as_ptr(),
                libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        let descriptor = file_from_fd(fd)?;
        Ok(Self {
            descriptor,
            name: name.to_os_string(),
        })
    }

    fn classify(self) -> io::Result<ClassifiedRegularEntry> {
        let metadata = descriptor_metadata(&self.descriptor)?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(not_regular_entry(&self.name));
        }
        Ok(ClassifiedRegularEntry {
            descriptor: self.descriptor,
            device: metadata.st_dev,
            inode: metadata.st_ino,
            name: self.name,
        })
    }
}

#[cfg(target_os = "linux")]
struct ClassifiedRegularEntry {
    descriptor: File,
    device: libc::dev_t,
    inode: libc::ino_t,
    name: OsString,
}

#[cfg(target_os = "linux")]
impl ClassifiedRegularEntry {
    fn into_readable(self) -> io::Result<File> {
        let descriptor_path =
            CString::new(format!("/proc/self/fd/{}", self.descriptor.as_raw_fd())).map_err(
                |_| io::Error::new(io::ErrorKind::InvalidInput, "invalid descriptor path"),
            )?;
        let fd = unsafe { libc::open(descriptor_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        let readable = file_from_fd(fd)?;
        let metadata = descriptor_metadata(&readable)?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG
            || metadata.st_dev != self.device
            || metadata.st_ino != self.inode
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "entry {} changed after it was classified",
                    self.name.to_string_lossy()
                ),
            ));
        }
        Ok(readable)
    }
}

#[cfg(target_os = "linux")]
struct PinnedDirectoryEntry {
    descriptor: File,
    name: OsString,
}

#[cfg(target_os = "linux")]
impl PinnedDirectoryEntry {
    fn pin(parent: &File, name: &OsStr) -> io::Result<Self> {
        let raw_name = CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                raw_name.as_ptr(),
                libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
            )
        };
        let descriptor = file_from_fd(fd)?;
        Ok(Self {
            descriptor,
            name: name.to_os_string(),
        })
    }

    fn classify(self) -> io::Result<ClassifiedDirectoryEntry> {
        let metadata = descriptor_metadata(&self.descriptor)?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(not_directory_entry(&self.name));
        }
        Ok(ClassifiedDirectoryEntry {
            descriptor: self.descriptor,
            device: metadata.st_dev,
            inode: metadata.st_ino,
            name: self.name,
        })
    }
}

#[cfg(target_os = "linux")]
struct ClassifiedDirectoryEntry {
    descriptor: File,
    device: libc::dev_t,
    inode: libc::ino_t,
    name: OsString,
}

#[cfg(target_os = "linux")]
impl ClassifiedDirectoryEntry {
    fn open_readable(self) -> io::Result<ReadableDirectoryEntry> {
        let fd = unsafe {
            libc::openat(
                self.descriptor.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )
        };
        Ok(ReadableDirectoryEntry {
            descriptor: file_from_fd(fd)?,
            device: self.device,
            inode: self.inode,
            name: self.name,
        })
    }
}

#[cfg(target_os = "linux")]
struct ReadableDirectoryEntry {
    descriptor: File,
    device: libc::dev_t,
    inode: libc::ino_t,
    name: OsString,
}

#[cfg(target_os = "linux")]
impl ReadableDirectoryEntry {
    fn validate_identity(self) -> io::Result<File> {
        let metadata = descriptor_metadata(&self.descriptor)?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR
            || metadata.st_dev != self.device
            || metadata.st_ino != self.inode
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "entry {} changed after it was classified",
                    self.name.to_string_lossy()
                ),
            ));
        }
        Ok(self.descriptor)
    }
}

fn openat_regular_no_follow(parent: &File, name: &OsStr) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        PinnedRegularEntry::pin(parent, name)?
            .classify()?
            .into_readable()
    }

    #[cfg(not(target_os = "linux"))]
    {
        let expected = entry_metadata_no_follow(parent, name)?;
        if expected.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(not_regular_entry(name));
        }
        let raw_name = CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                raw_name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        let readable = file_from_fd(fd)?;
        let observed = descriptor_metadata(&readable)?;
        if observed.st_mode & libc::S_IFMT != libc::S_IFREG
            || observed.st_dev != expected.st_dev
            || observed.st_ino != expected.st_ino
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "entry {} changed after it was classified",
                    name.to_string_lossy()
                ),
            ));
        }
        Ok(readable)
    }
}

fn openat_regular(parent: &File, name: &OsStr) -> Result<File> {
    openat_regular_no_follow(parent, name).map_err(|error| {
        BlazeDaemonError::BadRequest(format!(
            "cannot open runtime template source entry {} without following links: {error}",
            name.to_string_lossy()
        ))
    })
}

fn entry_metadata_no_follow(parent: &File, name: &OsStr) -> io::Result<libc::stat> {
    let raw_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "entry name contains NUL"))?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            raw_name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { metadata.assume_init() })
}

fn validate_source_entry_before_open(parent: &File, name: &OsStr) -> Result<()> {
    let metadata = entry_metadata_no_follow(parent, name).map_err(|error| {
        BlazeDaemonError::BadRequest(format!(
            "cannot inspect runtime template source entry {} without following links: {error}",
            name.to_string_lossy()
        ))
    })?;
    if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(BlazeDaemonError::BadRequest(format!(
            "runtime template source entry {} is not a regular file",
            name.to_string_lossy()
        )));
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.st_uid != expected_uid || metadata.st_mode & 0o022 != 0 {
        return Err(BlazeDaemonError::BadRequest(format!(
            "runtime template source file {} must be owned by the daemon user and not writable \
             by group or other users",
            name.to_string_lossy()
        )));
    }
    Ok(())
}

fn published_artifact_open_error(
    template: &OsStr,
    name: &OsStr,
    error: io::Error,
) -> BlazeDaemonError {
    BlazeDaemonError::RecoveryRequired(format!(
        "cannot open runtime template {} artifact {}: {error}",
        template.to_string_lossy(),
        name.to_string_lossy()
    ))
}

fn catalog_directory_recovery_error(label: &Path, error: BlazeDaemonError) -> BlazeDaemonError {
    match error {
        BlazeDaemonError::Io(error) => BlazeDaemonError::RecoveryRequired(format!(
            "cannot open runtime template catalog directory {}: {error}",
            label.display()
        )),
        error => error,
    }
}

#[cfg(target_os = "linux")]
fn with_validated_catalog_mount<T>(
    pinned: PinnedRegularEntry,
    boundary: CatalogBoundary,
    label: &Path,
    operation: impl FnOnce(PinnedRegularEntry) -> Result<T>,
) -> Result<T> {
    // fdinfo reads mount identity from the local descriptor table. A statx or
    // readable open could invoke the nested filesystem before it is rejected.
    validate_catalog_mount_fdinfo(&pinned.descriptor, boundary, label)?;
    operation(pinned)
}

#[cfg(target_os = "linux")]
fn with_validated_catalog_directory_mount<T>(
    pinned: PinnedDirectoryEntry,
    boundary: CatalogBoundary,
    label: &Path,
    operation: impl FnOnce(PinnedDirectoryEntry) -> Result<T>,
) -> Result<T> {
    // Validate the local O_PATH descriptor before classification or a
    // read-capable directory open can invoke the nested filesystem.
    validate_catalog_mount_fdinfo(&pinned.descriptor, boundary, label)?;
    operation(pinned)
}

#[cfg(target_os = "linux")]
fn with_revalidated_catalog_directory_mount<T>(
    readable: ReadableDirectoryEntry,
    boundary: CatalogBoundary,
    label: &Path,
    operation: impl FnOnce(ReadableDirectoryEntry) -> Result<T>,
) -> Result<T> {
    // Recheck the readable descriptor before metadata or directory traversal.
    validate_catalog_mount_fdinfo(&readable.descriptor, boundary, label)?;
    operation(readable)
}

fn try_open_catalog_directory(
    parent: &File,
    name: &OsStr,
    boundary: CatalogBoundary,
    label: &Path,
) -> Result<Option<File>> {
    #[cfg(target_os = "linux")]
    {
        let pinned = match PinnedDirectoryEntry::pin(parent, name) {
            Ok(pinned) => pinned,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        with_validated_catalog_directory_mount(pinned, boundary, label, |pinned| {
            let readable = pinned
                .classify()
                .and_then(ClassifiedDirectoryEntry::open_readable)
                .map_err(BlazeDaemonError::from)?;
            with_revalidated_catalog_directory_mount(readable, boundary, label, |readable| {
                Ok(Some(
                    readable
                        .validate_identity()
                        .map_err(BlazeDaemonError::from)?,
                ))
            })
        })
    }

    #[cfg(not(target_os = "linux"))]
    {
        let readable = match openat_directory(parent, name) {
            Ok(readable) => readable,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        validate_catalog_mount(&readable, boundary, label)?;
        Ok(Some(readable))
    }
}

fn open_catalog_directory(
    parent: &File,
    name: &OsStr,
    boundary: CatalogBoundary,
    label: &Path,
) -> Result<File> {
    try_open_catalog_directory(parent, name, boundary, label)?.ok_or_else(|| {
        BlazeDaemonError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "runtime template catalog directory {} disappeared while it was being opened",
                label.display()
            ),
        ))
    })
}

fn open_published_artifact(
    parent: &File,
    template: &OsStr,
    name: &OsStr,
    boundary: CatalogBoundary,
) -> Result<File> {
    #[cfg(target_os = "linux")]
    {
        let pinned = PinnedRegularEntry::pin(parent, name)
            .map_err(|error| published_artifact_open_error(template, name, error))?;
        with_validated_catalog_mount(pinned, boundary, Path::new(name), |pinned| {
            let readable = pinned
                .classify()
                .and_then(ClassifiedRegularEntry::into_readable)
                .map_err(|error| published_artifact_open_error(template, name, error))?;
            validate_catalog_mount_fdinfo(&readable, boundary, Path::new(name))?;
            Ok(readable)
        })
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Other platforms perform a no-follow metadata classification and
        // verify identity after opening because O_PATH is unavailable.
        let readable = openat_regular_no_follow(parent, name)
            .map_err(|error| published_artifact_open_error(template, name, error))?;
        validate_catalog_mount(&readable, boundary, Path::new(name))?;
        Ok(readable)
    }
}

#[cfg(not(target_os = "linux"))]
fn entry_exists_at(parent: &File, name: &OsStr) -> io::Result<bool> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

fn file_from_fd(fd: libc::c_int) -> io::Result<File> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn statx_mount_id(file: &File) -> io::Result<Option<u64>> {
    let empty = c"";
    let mut metadata = MaybeUninit::<libc::statx>::zeroed();
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            empty.as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            metadata.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.stx_mask & libc::STATX_MNT_ID == 0 {
        return Ok(None);
    }
    Ok(Some(metadata.stx_mnt_id))
}

#[cfg(target_os = "linux")]
fn parse_fdinfo_mount_id(contents: &str) -> io::Result<u64> {
    let value = contents
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "fdinfo does not report a mount identifier",
            )
        })?;
    value.trim().parse::<u64>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("fdinfo mount identifier is invalid: {error}"),
        )
    })
}

#[cfg(target_os = "linux")]
fn fdinfo_mount_id(file: &File) -> io::Result<u64> {
    let contents = fs::read_to_string(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?;
    parse_fdinfo_mount_id(&contents)
}

#[cfg(target_os = "linux")]
fn opened_mount_id_from_statx(
    file: &File,
    statx_result: io::Result<Option<u64>>,
) -> io::Result<u64> {
    match statx_result {
        Ok(Some(mount_id)) => Ok(mount_id),
        Ok(None) => fdinfo_mount_id(file),
        Err(statx_error) => fdinfo_mount_id(file).map_err(|fdinfo_error| {
            io::Error::new(
                fdinfo_error.kind(),
                format!(
                    "cannot determine mount identifier with statx ({statx_error}) or fdinfo: \
                     {fdinfo_error}"
                ),
            )
        }),
    }
}

#[cfg(target_os = "linux")]
fn opened_mount_id(file: &File) -> io::Result<u64> {
    opened_mount_id_from_statx(file, statx_mount_id(file))
}

#[cfg(not(target_os = "linux"))]
fn opened_mount_id(file: &File) -> io::Result<u64> {
    Ok(file.metadata()?.dev())
}

fn validate_catalog_mount_id(mount_id: u64, boundary: CatalogBoundary, label: &Path) -> Result<()> {
    if mount_id != boundary.mount_id {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template catalog path {} crosses a nested mount boundary",
            label.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_catalog_mount_fdinfo(
    file: &File,
    boundary: CatalogBoundary,
    label: &Path,
) -> Result<()> {
    validate_catalog_mount_id(fdinfo_mount_id(file)?, boundary, label)
}

#[cfg(any(not(target_os = "linux"), test))]
fn validate_catalog_mount(file: &File, boundary: CatalogBoundary, label: &Path) -> Result<()> {
    validate_catalog_mount_id(opened_mount_id(file)?, boundary, label)
}

fn validate_source_directory(metadata: &std::fs::Metadata, path: &Path) -> Result<()> {
    if !metadata.is_dir() {
        return Err(BlazeDaemonError::BadRequest(format!(
            "runtime template source {} is not a directory",
            path.display()
        )));
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != expected_uid || metadata.mode() & 0o022 != 0 {
        return Err(BlazeDaemonError::BadRequest(format!(
            "runtime template source directory {} must be owned by the daemon user and not \
             writable by group or other users",
            path.display()
        )));
    }
    Ok(())
}

fn validate_source_file(metadata: &std::fs::Metadata, name: &OsStr) -> Result<()> {
    if !metadata.file_type().is_file() {
        return Err(BlazeDaemonError::BadRequest(format!(
            "runtime template source entry {} is not a regular file",
            name.to_string_lossy()
        )));
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != expected_uid || metadata.mode() & 0o022 != 0 {
        return Err(BlazeDaemonError::BadRequest(format!(
            "runtime template source file {} must be owned by the daemon user and not writable \
             by group or other users",
            name.to_string_lossy()
        )));
    }
    Ok(())
}

fn same_file_identity(observed: &std::fs::Metadata, current: &std::fs::Metadata) -> bool {
    observed.len() == current.len()
        && observed.dev() == current.dev()
        && observed.ino() == current.ino()
        && observed.mtime() == current.mtime()
        && observed.mtime_nsec() == current.mtime_nsec()
        && observed.ctime() == current.ctime()
        && observed.ctime_nsec() == current.ctime_nsec()
}

#[cfg(test)]
fn create_catalog_root(root: &Path) -> Result<File> {
    if root.as_os_str().is_empty() {
        return Err(invalid_catalog_path(root, "path is empty"));
    }

    let anchor = if root.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut directory = open_directory_no_follow(anchor).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot pin runtime template catalog anchor {}: {error}",
            anchor.display()
        ))
    })?;
    let mut saw_name = false;
    let mut label = PathBuf::from(anchor);

    for component in root.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                saw_name = true;
                label.push(name);
                directory = open_or_create_catalog_component(&directory, name, &label)?;
            }
            Component::ParentDir => {
                return Err(invalid_catalog_path(
                    root,
                    "parent-directory components are not allowed",
                ));
            }
            Component::Prefix(_) => {
                return Err(invalid_catalog_path(root, "unsupported path prefix"));
            }
        }
    }
    if !saw_name {
        return Err(invalid_catalog_path(
            root,
            "path must name a catalog directory",
        ));
    }

    Ok(directory)
}

fn pin_catalog_root(root: &Path) -> Result<PinnedCatalogRoot> {
    if root.as_os_str().is_empty() {
        return Err(invalid_catalog_path(root, "path is empty"));
    }
    for component in root.components() {
        match component {
            Component::ParentDir => {
                return Err(invalid_catalog_path(
                    root,
                    "parent-directory components are not allowed",
                ));
            }
            Component::Prefix(_) => {
                return Err(invalid_catalog_path(root, "unsupported path prefix"));
            }
            Component::RootDir | Component::CurDir | Component::Normal(_) => {}
        }
    }

    let normalized = normalize_startup_path(root)?;
    let mut directory = open_directory_no_follow(Path::new("/")).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot pin runtime template catalog anchor /: {error}"
        ))
    })?;
    let mut parent_path = PathBuf::from("/");
    let mut components = normalized
        .components()
        .filter_map(|component| match component {
            Component::RootDir | Component::CurDir => None,
            Component::Normal(name) => Some(Ok(name.to_os_string())),
            Component::ParentDir => Some(Err(invalid_catalog_path(
                root,
                "parent-directory components are not allowed",
            ))),
            Component::Prefix(_) => {
                Some(Err(invalid_catalog_path(root, "unsupported path prefix")))
            }
        });
    let mut saw_name = false;

    while let Some(component) = components.next() {
        let name = component?;
        saw_name = true;
        let label = parent_path.join(&name);
        match openat_directory(&directory, &name) {
            Ok(child) => {
                directory = child;
                parent_path = label;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut missing = vec![(name, label.clone())];
                let mut missing_label = label;
                for component in components {
                    let name = component?;
                    missing_label.push(&name);
                    missing.push((name, missing_label.clone()));
                }
                return Ok(PinnedCatalogRoot::Missing(CatalogCreationPlan {
                    parent: directory,
                    parent_path,
                    missing,
                }));
            }
            Err(error) => {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "cannot pin runtime template catalog component {}: {error}",
                    label.display()
                )));
            }
        }
    }

    if !saw_name {
        return Err(invalid_catalog_path(
            root,
            "path must name a catalog directory",
        ));
    }
    Ok(PinnedCatalogRoot::Existing(directory))
}

fn materialize_catalog_root(mut plan: CatalogCreationPlan) -> Result<File> {
    for (name, label) in plan.missing {
        plan.parent = create_planned_catalog_component(&plan.parent, &name, &label)?;
    }
    Ok(plan.parent)
}

fn create_planned_catalog_component(parent: &File, name: &OsStr, label: &Path) -> Result<File> {
    match mkdirat_directory(parent, name, CATALOG_DIR_MODE) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template catalog component {} appeared while startup boundaries were \
                 being validated",
                label.display()
            )));
        }
        Err(error) => return Err(error.into()),
    }

    let directory = match openat_directory(parent, name) {
        Ok(directory) => directory,
        Err(error) => {
            let cleanup = unlinkat(parent, name, libc::AT_REMOVEDIR);
            return match cleanup {
                Ok(()) => Err(error.into()),
                Err(cleanup_error) => Err(BlazeDaemonError::RecoveryRequired(format!(
                    "cannot open newly created runtime template catalog component {}: {error}; \
                     cleanup failed: {cleanup_error}",
                    label.display()
                ))),
            };
        }
    };
    enforce_owned_mode_file(&directory, true, CATALOG_DIR_MODE, label)?;
    sync_directory_file(parent)?;
    Ok(directory)
}

#[cfg(test)]
fn open_or_create_catalog_component(parent: &File, name: &OsStr, label: &Path) -> Result<File> {
    match openat_directory(parent, name) {
        Ok(directory) => return Ok(directory),
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
        Err(_) => {}
    }

    let created = match mkdirat_directory(parent, name, CATALOG_DIR_MODE) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    let directory = match openat_directory(parent, name) {
        Ok(directory) => directory,
        Err(error) if created => {
            let cleanup = unlinkat(parent, name, libc::AT_REMOVEDIR);
            return match cleanup {
                Ok(()) => Err(error.into()),
                Err(cleanup_error) => Err(BlazeDaemonError::RecoveryRequired(format!(
                    "cannot open newly created runtime template catalog component {}: {error}; \
                     cleanup failed: {cleanup_error}",
                    label.display()
                ))),
            };
        }
        Err(error) => return Err(error.into()),
    };
    if created {
        enforce_owned_mode_file(&directory, true, CATALOG_DIR_MODE, label)?;
        sync_directory_file(parent)?;
    }
    Ok(directory)
}

fn invalid_catalog_path(path: &Path, reason: &str) -> BlazeDaemonError {
    BlazeDaemonError::Core(BlazeError::ConfigError {
        source: ConfigErrorSource::InvalidValue(format!(
            "template.dir ({}) {reason}",
            path.display()
        )),
    })
}

fn config_source_io(action: &str, path: &Path, error: io::Error) -> BlazeDaemonError {
    BlazeDaemonError::Core(BlazeError::from(io::Error::new(
        error.kind(),
        format!(
            "cannot {action} daemon config_path {}: {error}",
            path.display()
        ),
    )))
}

fn invalid_config_source(path: &Path, reason: &str) -> BlazeDaemonError {
    BlazeDaemonError::Core(BlazeError::ConfigError {
        source: ConfigErrorSource::InvalidValue(format!(
            "daemon config_path ({}) {reason}",
            path.display()
        )),
    })
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn validate_template_roots(
    config: &TemplateSection,
    images_dir: &Path,
    instances_dir: &Path,
    policy_dir: &Path,
    backend_binaries: &HashMap<String, PathBuf>,
    state_dir: &Path,
    socket_path: &Path,
    config_source: Option<&PinnedConfigSource>,
) -> Result<ValidatedTemplateRoots> {
    validate_template_roots_with_policy_mode(
        config,
        images_dir,
        instances_dir,
        policy_dir,
        backend_binaries,
        state_dir,
        socket_path,
        config_source,
        PolicyLoadErrorMode::Fail,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_template_roots_with_policy_mode(
    config: &TemplateSection,
    images_dir: &Path,
    instances_dir: &Path,
    policy_dir: &Path,
    backend_binaries: &HashMap<String, PathBuf>,
    state_dir: &Path,
    socket_path: &Path,
    config_source: Option<&PinnedConfigSource>,
    policy_error_mode: PolicyLoadErrorMode,
) -> Result<ValidatedTemplateRoots> {
    let host_named_network_namespace_paths = HOST_NAMED_NETWORK_NAMESPACE_PATHS.map(Path::new);
    validate_template_roots_with_hook_and_policy_mode(
        config,
        images_dir,
        instances_dir,
        policy_dir,
        backend_binaries,
        state_dir,
        socket_path,
        config_source,
        Path::new(HOST_NETWORK_COORDINATION_PATH),
        &host_named_network_namespace_paths,
        policy_error_mode,
        || {},
    )
}

// Keep the test-only transition hook adjacent to the complete boundary set so
// the production wrapper and deterministic replacement test exercise one path.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn validate_template_roots_with_hook<F>(
    config: &TemplateSection,
    images_dir: &Path,
    instances_dir: &Path,
    policy_dir: &Path,
    backend_binaries: &HashMap<String, PathBuf>,
    state_dir: &Path,
    socket_path: &Path,
    config_source: Option<&PinnedConfigSource>,
    host_network_coordination_path: &Path,
    host_named_network_namespace_paths: &[&Path],
    before_identity_check: F,
) -> Result<ValidatedTemplateRoots>
where
    F: FnOnce(),
{
    validate_template_roots_with_hook_and_policy_mode(
        config,
        images_dir,
        instances_dir,
        policy_dir,
        backend_binaries,
        state_dir,
        socket_path,
        config_source,
        host_network_coordination_path,
        host_named_network_namespace_paths,
        PolicyLoadErrorMode::Fail,
        before_identity_check,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_template_roots_with_hook_and_policy_mode<F>(
    config: &TemplateSection,
    images_dir: &Path,
    instances_dir: &Path,
    policy_dir: &Path,
    backend_binaries: &HashMap<String, PathBuf>,
    state_dir: &Path,
    socket_path: &Path,
    config_source: Option<&PinnedConfigSource>,
    host_network_coordination_path: &Path,
    host_named_network_namespace_paths: &[&Path],
    policy_error_mode: PolicyLoadErrorMode,
    before_identity_check: F,
) -> Result<ValidatedTemplateRoots>
where
    F: FnOnce(),
{
    if let Some(config_source) = config_source {
        config_source.validate_identity()?;
    }
    reject_symlink_components(&config.dir, "template.dir")?;
    if let Some(import_root) = config.import_root.as_deref() {
        reject_symlink_components(import_root, "template.import_root")?;
    }

    // Pin an existing catalog or the deepest existing creation parent before
    // resolving any boundary. The retained object is later handed directly to
    // the catalog or used for descriptor-relative creation, so startup never
    // reopens a validated root through its pathname.
    let pinned_catalog = pin_catalog_root(&config.dir)?;
    let import_root = config
        .import_root
        .as_deref()
        .map(pin_import_root)
        .transpose()?;
    let helper_executable_roots = host_helper_executable_roots()?;

    let initial_policy_load = validate_template_root_paths_with_policy_mode(
        config,
        images_dir,
        instances_dir,
        policy_dir,
        backend_binaries,
        state_dir,
        socket_path,
        config_source,
        host_network_coordination_path,
        host_named_network_namespace_paths,
        &helper_executable_roots,
        policy_error_mode,
    )?;

    before_identity_check();

    let root = match pinned_catalog {
        PinnedCatalogRoot::Existing(directory) => directory,
        PinnedCatalogRoot::Missing(plan) => {
            validate_pinned_directory_path(
                "template.dir creation parent",
                &plan.parent_path,
                &plan.parent,
            )?;
            materialize_catalog_root(plan)?
        }
    };
    validate_pinned_directory_path("template.dir", &config.dir, &root)?;
    if let (Some(path), Some(root)) = (config.import_root.as_deref(), import_root.as_ref()) {
        validate_pinned_directory_path("template.import_root", path, &root.directory)?;
    }

    if let Some(config_source) = config_source {
        config_source.validate_identity()?;
    }

    // Repeat the resolved comparison after pinning. This catches changes to
    // peer roots while preserving the exact catalog/import objects that were
    // observed during the first pass.
    let final_policy_load = validate_template_root_paths_with_policy_mode(
        config,
        images_dir,
        instances_dir,
        policy_dir,
        backend_binaries,
        state_dir,
        socket_path,
        config_source,
        host_network_coordination_path,
        host_named_network_namespace_paths,
        &helper_executable_roots,
        policy_error_mode,
    )?;
    if let Some(config_source) = config_source {
        config_source.validate_identity()?;
    }
    enforce_owned_mode_file(&root, true, CATALOG_DIR_MODE, &config.dir)?;

    Ok(ValidatedTemplateRoots {
        root,
        import_root,
        policy_load: initial_policy_load.combine(final_policy_load),
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_template_root_paths_with_policy_mode(
    config: &TemplateSection,
    images_dir: &Path,
    instances_dir: &Path,
    policy_dir: &Path,
    backend_binaries: &HashMap<String, PathBuf>,
    state_dir: &Path,
    socket_path: &Path,
    config_source: Option<&PinnedConfigSource>,
    host_network_coordination_path: &Path,
    host_named_network_namespace_paths: &[&Path],
    helper_executable_roots: &[(String, PathBuf)],
    policy_error_mode: PolicyLoadErrorMode,
) -> Result<PolicyLoadDisposition> {
    if let Some(config_source) = config_source {
        config_source.validate_identity()?;
    }
    let mounts = MountTable::load()?;
    validate_template_root_paths_with_mounts_and_policy_mode(
        config,
        images_dir,
        instances_dir,
        policy_dir,
        backend_binaries,
        state_dir,
        socket_path,
        config_source,
        host_network_coordination_path,
        host_named_network_namespace_paths,
        helper_executable_roots,
        &mounts,
        policy_error_mode,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn validate_template_root_paths_with_mounts(
    config: &TemplateSection,
    images_dir: &Path,
    instances_dir: &Path,
    policy_dir: &Path,
    backend_binaries: &HashMap<String, PathBuf>,
    state_dir: &Path,
    socket_path: &Path,
    config_source: Option<&PinnedConfigSource>,
    host_network_coordination_path: &Path,
    host_named_network_namespace_paths: &[&Path],
    mounts: &MountTable,
) -> Result<()> {
    let helper_executable_roots = host_helper_executable_roots()?;
    validate_template_root_paths_with_mounts_and_policy_mode(
        config,
        images_dir,
        instances_dir,
        policy_dir,
        backend_binaries,
        state_dir,
        socket_path,
        config_source,
        host_network_coordination_path,
        host_named_network_namespace_paths,
        &helper_executable_roots,
        mounts,
        PolicyLoadErrorMode::Fail,
    )
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
fn validate_template_root_paths_with_mounts_and_policy_mode(
    config: &TemplateSection,
    images_dir: &Path,
    instances_dir: &Path,
    policy_dir: &Path,
    backend_binaries: &HashMap<String, PathBuf>,
    state_dir: &Path,
    socket_path: &Path,
    config_source: Option<&PinnedConfigSource>,
    host_network_coordination_path: &Path,
    host_named_network_namespace_paths: &[&Path],
    helper_executable_roots: &[(String, PathBuf)],
    mounts: &MountTable,
    policy_error_mode: PolicyLoadErrorMode,
) -> Result<PolicyLoadDisposition> {
    reject_symlink_components(&config.dir, "template.dir")?;
    if let Some(import_root) = config.import_root.as_deref() {
        reject_symlink_components(import_root, "template.import_root")?;
    }

    let catalog = resolve_existing_prefix(&config.dir)?;
    let configured_state = normalize_startup_path(state_dir)?;
    let state = resolve_existing_prefix(state_dir)?;
    validate_lifecycle_boundary(
        "template.dir",
        &config.dir,
        &catalog,
        &configured_state,
        mounts,
    )?;
    if state != configured_state {
        validate_lifecycle_boundary("template.dir", &config.dir, &catalog, &state, mounts)?;
    }
    let mut roots = Vec::new();
    for (label, path) in [
        ("storage.images_dir", images_dir),
        ("storage.instances_dir", instances_dir),
        ("policy.dir", policy_dir),
        ("daemon.socket", socket_path),
    ] {
        push_configured_and_resolved_root(&mut roots, label, path)?;
    }
    let policy_load = push_policy_entry_roots(&mut roots, policy_dir, policy_error_mode)?;
    push_configured_and_resolved_root(
        &mut roots,
        "host network coordination",
        host_network_coordination_path,
    )?;
    for path in host_named_network_namespace_paths {
        push_configured_and_resolved_root(&mut roots, "host named network namespace", path)?;
    }
    // Every Firecracker owner creates this fixed mount target before launching,
    // and it does so through whatever the path resolves to. Reserve both the
    // literal and its resolved target, so neither a catalog root configured at
    // this path nor a symlinked parent pointing into one can end up owning the
    // file, which startup accounting would read as a malformed published entry.
    push_configured_and_resolved_root(
        &mut roots,
        "snapshot view rootfs",
        Path::new(crate::spawner::firecracker::PORTABLE_ROOTFS_PATH),
    )?;
    roots.extend(helper_executable_roots.iter().cloned());
    let mut configured_backends = backend_binaries.iter().collect::<Vec<_>>();
    configured_backends.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    for (name, path) in configured_backends {
        push_configured_and_resolved_root(&mut roots, &format!("backends.{name}"), path)?;
    }
    if let Some(config_source) = config_source {
        config_source.validate_identity()?;
        let configured = config_source.configured_path().to_path_buf();
        let resolved = resolve_existing_prefix(config_source.canonical_path())?;
        roots.push((
            "config_path configured path".to_string(),
            configured.clone(),
        ));
        if resolved != configured {
            roots.push(("config_path resolved target".to_string(), resolved));
        }
    }
    let protected_path_count = roots.len();
    if let Some(import_root) = config.import_root.as_deref() {
        roots.push((
            "template.import_root".to_string(),
            resolve_existing_prefix(import_root)?,
        ));
    }
    for (label, root) in &roots {
        if paths_overlap_across_mounts(&catalog, root, mounts)? {
            return Err(invalid_root_overlap(
                "template.dir",
                &config.dir,
                label,
                root,
            ));
        }
    }

    if let Some(import_root) = config.import_root.as_deref() {
        let resolved_import = resolve_existing_prefix(import_root)?;
        validate_lifecycle_boundary(
            "template.import_root",
            import_root,
            &resolved_import,
            &configured_state,
            mounts,
        )?;
        if state != configured_state {
            validate_lifecycle_boundary(
                "template.import_root",
                import_root,
                &resolved_import,
                &state,
                mounts,
            )?;
        }
        for (label, root) in roots.iter().take(protected_path_count) {
            if paths_overlap_across_mounts(&resolved_import, root, mounts)? {
                return Err(invalid_root_overlap(
                    "template.import_root",
                    import_root,
                    label,
                    root,
                ));
            }
        }
    }
    Ok(policy_load)
}

fn push_configured_and_resolved_root(
    roots: &mut Vec<(String, PathBuf)>,
    label: &str,
    path: &Path,
) -> Result<()> {
    let configured = normalize_startup_path(path)?;
    let resolved = resolve_existing_prefix(path)?;
    roots.push((format!("{label} configured path"), configured.clone()));
    if resolved != configured {
        roots.push((format!("{label} resolved target"), resolved));
    }
    Ok(())
}

fn host_helper_executable_roots() -> Result<Vec<(String, PathBuf)>> {
    let Some(search_path) = std::env::var_os("PATH") else {
        return Ok(Vec::new());
    };
    host_helper_executable_roots_from_search_path(&search_path)
}

fn host_helper_executable_roots_from_search_path(
    search_path: &OsStr,
) -> Result<Vec<(String, PathBuf)>> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for name in HOST_PATH_HELPERS {
        for directory in std::env::split_paths(search_path) {
            let candidate = directory.join(name);
            let Ok(metadata) = fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
            let configured = normalize_startup_path(&candidate)?;
            if !seen.insert(configured) {
                continue;
            }
            push_configured_and_resolved_root(
                &mut roots,
                &format!("host helper executable {name}"),
                &candidate,
            )?;
        }
    }
    Ok(roots)
}

fn push_policy_entry_roots(
    roots: &mut Vec<(String, PathBuf)>,
    policy_dir: &Path,
    error_mode: PolicyLoadErrorMode,
) -> Result<PolicyLoadDisposition> {
    let entries = match fs::read_dir(policy_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if error_mode == PolicyLoadErrorMode::Warn {
                tracing::warn!(
                    policy_dir = %policy_dir.display(),
                    %error,
                    "policy entry boundary discovery failed; warn mode will load an empty policy engine"
                );
            }
            return Ok(PolicyLoadDisposition::UseEmpty);
        }
        Err(error) => {
            let error = BlazeDaemonError::from(error);
            if error_mode == PolicyLoadErrorMode::Warn {
                tracing::warn!(
                    policy_dir = %policy_dir.display(),
                    %error,
                    "policy entry boundary discovery failed; warn mode will load an empty policy engine"
                );
                return Ok(PolicyLoadDisposition::UseEmpty);
            }
            return Err(error);
        }
    };
    let discovered = (|| {
        let mut discovered = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(OsStr::to_str) != Some("toml") {
                continue;
            }
            let label = format!("policy entry {}", path.display());
            push_configured_and_resolved_root(&mut discovered, &label, &path)?;
        }
        Ok::<_, BlazeDaemonError>(discovered)
    })();

    match discovered {
        Ok(discovered) => {
            roots.extend(discovered);
            Ok(PolicyLoadDisposition::LoadConfigured)
        }
        Err(error) if error_mode == PolicyLoadErrorMode::Warn => {
            tracing::warn!(
                policy_dir = %policy_dir.display(),
                %error,
                "policy entry boundary discovery failed; warn mode will load an empty policy engine"
            );
            Ok(PolicyLoadDisposition::UseEmpty)
        }
        Err(error) => Err(error),
    }
}

fn validate_pinned_directory_path(label: &str, path: &Path, pinned: &File) -> Result<()> {
    let current = open_directory_path_no_follow(path).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot confirm pinned {label} {}: {error}",
            path.display()
        ))
    })?;
    let expected = pinned.metadata()?;
    let observed = current.metadata()?;
    if expected.dev() != observed.dev() || expected.ino() != observed.ino() {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "{label} {} changed while startup boundaries were being validated",
            path.display()
        )));
    }
    Ok(())
}

fn reject_symlink_components(path: &Path, label: &str) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BlazeDaemonError::Core(BlazeError::ConfigError {
                    source: ConfigErrorSource::InvalidValue(format!(
                        "{label} ({}) contains symbolic-link component {}",
                        path.display(),
                        current.display()
                    )),
                }));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn resolve_existing_prefix(path: &Path) -> Result<PathBuf> {
    // Existing daemon paths may be relative to the startup working directory.
    // Anchor them before walking missing suffixes so a fresh single-component
    // path still has the working directory as an existing ancestor.
    let absolute = normalize_startup_path(path)?;
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or(error)?;
                missing.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "path has no existing ancestor")
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn normalize_startup_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    normalize_absolute_path(&absolute)
}

impl MountTable {
    fn load() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            Self::parse(&std::fs::read("/proc/self/mountinfo")?)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Self::default())
        }
    }

    #[cfg(any(test, target_os = "linux"))]
    fn parse(contents: &[u8]) -> io::Result<Self> {
        let mut entries = Vec::new();
        for line in contents.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let fields = line
                .split(|byte| *byte == b' ')
                .filter(|field| !field.is_empty())
                .collect::<Vec<_>>();
            if fields.len() < 6 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "mountinfo entry has fewer than six fields",
                ));
            }
            let device = parse_mount_device(fields[2])?;
            let root = decode_mount_path(fields[3])?;
            let mount_point = decode_mount_path(fields[4])?;
            if !root.is_absolute() || !mount_point.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "mountinfo root and mount point must be absolute",
                ));
            }
            entries.push(MountEntry {
                device,
                root: normalize_absolute_path(&root)?,
                mount_point: normalize_absolute_path(&mount_point)?,
            });
        }
        Ok(Self { entries })
    }

    fn location(&self, path: &Path) -> io::Result<Option<FilesystemLocation>> {
        let path = normalize_absolute_path(path)?;
        let Some(entry) = self
            .entries
            .iter()
            .filter(|entry| path.starts_with(&entry.mount_point))
            .max_by_key(|entry| entry.mount_point.components().count())
        else {
            return Ok(None);
        };
        let relative = path.strip_prefix(&entry.mount_point).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "mount point stopped containing the resolved path",
            )
        })?;
        Ok(Some(FilesystemLocation {
            device: entry.device,
            path: normalize_absolute_path(&entry.root.join(relative))?,
        }))
    }
}

#[cfg(any(test, target_os = "linux"))]
fn parse_mount_device(value: &[u8]) -> io::Result<(u64, u64)> {
    let value = std::str::from_utf8(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let (major, minor) = value
        .split_once(':')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "mountinfo device lacks ':'"))?;
    let major = major
        .parse::<u64>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let minor = minor
        .parse::<u64>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok((major, minor))
}

#[cfg(any(test, target_os = "linux"))]
fn decode_mount_path(value: &[u8]) -> io::Result<PathBuf> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] == b'\\' && index + 3 < value.len() {
            let digits = &value[index + 1..index + 4];
            if digits.iter().all(|digit| matches!(digit, b'0'..=b'7')) {
                let byte = (digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + (digits[2] - b'0');
                decoded.push(byte);
                index += 4;
                continue;
            }
        }
        decoded.push(value[index]);
        index += 1;
    }
    if decoded.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mountinfo path contains NUL",
        ));
    }
    Ok(PathBuf::from(OsString::from_vec(decoded)))
}

fn normalize_absolute_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "filesystem boundary path must be absolute",
        ));
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => normalized.push(name),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported filesystem boundary path prefix",
                ));
            }
        }
    }
    Ok(normalized)
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn paths_overlap_across_mounts(left: &Path, right: &Path, mounts: &MountTable) -> Result<bool> {
    if paths_overlap(left, right) {
        return Ok(true);
    }
    let Some(left) = mounts.location(left)? else {
        return Ok(false);
    };
    let Some(right) = mounts.location(right)? else {
        return Ok(false);
    };
    Ok(left.device == right.device && paths_overlap(&left.path, &right.path))
}

fn validate_lifecycle_boundary(
    label: &str,
    original: &Path,
    resolved: &Path,
    state: &Path,
    mounts: &MountTable,
) -> Result<()> {
    validate_lifecycle_relation(label, original, resolved, state)?;
    let Some(resolved_location) = mounts.location(resolved)? else {
        return Ok(());
    };
    let Some(state_location) = mounts.location(state)? else {
        return Ok(());
    };
    if resolved_location.device == state_location.device
        && (resolved_location.path != resolved || state_location.path != state)
    {
        validate_lifecycle_relation(
            label,
            original,
            &resolved_location.path,
            &state_location.path,
        )?;
    }
    Ok(())
}

fn validate_lifecycle_relation(
    label: &str,
    original: &Path,
    resolved: &Path,
    state: &Path,
) -> Result<()> {
    let enters_lifecycle_entry = resolved
        .strip_prefix(state)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .is_some_and(|name| Uuid::parse_str(name).is_ok());
    if resolved == state || state.starts_with(resolved) || enters_lifecycle_entry {
        return Err(BlazeDaemonError::Core(BlazeError::ConfigError {
            source: ConfigErrorSource::InvalidValue(format!(
                "{label} ({}) must not own daemon.state_dir ({}) or a sandbox UUID subtree",
                original.display(),
                state.display()
            )),
        }));
    }
    Ok(())
}

fn invalid_root_overlap(
    left_label: &str,
    left: &Path,
    right_label: &str,
    resolved_right: &Path,
) -> BlazeDaemonError {
    BlazeDaemonError::Core(BlazeError::ConfigError {
        source: ConfigErrorSource::InvalidValue(format!(
            "{left_label} ({}) and {right_label} ({}) must resolve to disjoint paths",
            left.display(),
            resolved_right.display()
        )),
    })
}

#[cfg(test)]
fn create_private_directory(path: &Path) -> Result<()> {
    DirBuilder::new().mode(CATALOG_DIR_MODE).create(path)?;
    enforce_owned_mode(path, true, CATALOG_DIR_MODE)
}

fn mkdirat_directory(parent: &File, name: &OsStr, mode: u32) -> io::Result<()> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
    #[cfg(target_os = "macos")]
    let mode = mode as libc::mode_t;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn create_staging_directory_at(parent: &File, name: &OsStr, claim: &ImportClaim) -> Result<File> {
    mkdirat_directory(parent, name, CATALOG_DIR_MODE)?;

    let open_result = (|| {
        #[cfg(test)]
        if claim.inner.fail_staging_open.swap(false, Ordering::AcqRel) {
            return Err(BlazeDaemonError::Io(io::Error::from_raw_os_error(
                libc::EMFILE,
            )));
        }
        let directory =
            open_catalog_directory(parent, name, claim.inner.boundary, Path::new(name))?;
        enforce_owned_mode_file(&directory, true, CATALOG_DIR_MODE, Path::new(name))?;
        Ok(directory)
    })();
    let error = match open_result {
        Ok(directory) => return Ok(directory),
        Err(error) => error,
    };

    // No file can have been written before the initial directory open succeeds.
    // Remove the empty directory directly so descriptor exhaustion does not make
    // cleanup depend on another openat call that is expected to fail as well.
    let cleanup_result = unlinkat(parent, name, libc::AT_REMOVEDIR)
        .map_err(BlazeDaemonError::from)
        .and_then(|_| sync_directory_file(parent));
    if let Err(cleanup_error) = cleanup_result {
        let message = format!(
            "runtime template staging setup failed and cleanup could not be confirmed; restart \
             after repairing the catalog: setup error: {error}; cleanup error: {cleanup_error}"
        );
        claim.block_catalog(message.clone());
        return Err(BlazeDaemonError::RecoveryRequired(message));
    }
    Err(error)
}

#[cfg(test)]
fn enforce_owned_mode(path: &Path, directory: bool, mode: u32) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.file_type().is_file())
    {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template catalog path {} has an unexpected file type",
            path.display()
        )));
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != expected_uid {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template catalog path {} is not owned by the daemon user",
            path.display()
        )));
    }
    if metadata.mode() & 0o777 != mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn enforce_owned_mode_file(file: &File, directory: bool, mode: u32, label: &Path) -> Result<()> {
    let metadata = file.metadata()?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.file_type().is_file()) {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template catalog path {} has an unexpected file type",
            label.display()
        )));
    }
    if !directory && metadata.nlink() != 1 {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template catalog file {} must have exactly one hard link",
            label.display()
        )));
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != expected_uid {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template catalog path {} is not owned by the daemon user",
            label.display()
        )));
    }
    if metadata.mode() & 0o777 != mode {
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn catalog_usage(
    root: &File,
    limits: ImportLimits,
    boundary: CatalogBoundary,
) -> Result<CatalogUsage> {
    let mut total = 0_u64;
    let mut entries = 0_usize;
    let names = source_entry_names(root, limits.max_entries).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot inspect runtime template catalog: {error}"
        ))
    })?;
    for name in names {
        if name.to_string_lossy().starts_with('.') {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template catalog contains unresolved hidden entry {}",
                name.to_string_lossy()
            )));
        }
        entries = entries.checked_add(1).ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(
                "runtime template catalog entry count overflow".to_string(),
            )
        })?;
        if entries > limits.max_entries {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template catalog exceeds the configured {}-entry limit",
                limits.max_entries
            )));
        }
        let directory = open_catalog_directory(root, &name, boundary, Path::new(&name))
            .map_err(|error| catalog_directory_recovery_error(Path::new(&name), error))?;
        enforce_owned_mode_file(&directory, true, CATALOG_DIR_MODE, Path::new(&name))?;
        let mut file_count = 0_usize;
        let mut template_bytes = 0_u64;
        let artifacts = source_entry_names(&directory, limits.max_files).map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "cannot inspect runtime template {}: {error}",
                name.to_string_lossy()
            ))
        })?;
        for artifact_name in artifacts {
            let artifact = open_published_artifact(&directory, &name, &artifact_name, boundary)?;
            enforce_owned_mode_file(
                &artifact,
                false,
                CATALOG_FILE_MODE,
                Path::new(&artifact_name),
            )?;
            file_count = file_count.checked_add(1).ok_or_else(|| {
                BlazeDaemonError::RecoveryRequired(
                    "runtime template catalog file count overflow".to_string(),
                )
            })?;
            if file_count > limits.max_files {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "runtime template {} exceeds the configured file limit",
                    name.to_string_lossy()
                )));
            }
            let artifact_bytes = artifact.metadata()?.len();
            if artifact_name == OsStr::new("template.json")
                && artifact_bytes > limits.max_metadata_bytes
            {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "runtime template {} metadata exceeds the configured limit",
                    name.to_string_lossy()
                )));
            }
            template_bytes = template_bytes.checked_add(artifact_bytes).ok_or_else(|| {
                BlazeDaemonError::RecoveryRequired(
                    "runtime template byte accounting overflow".to_string(),
                )
            })?;
            total = total.checked_add(artifact_bytes).ok_or_else(|| {
                BlazeDaemonError::RecoveryRequired(
                    "runtime template catalog byte accounting overflow".to_string(),
                )
            })?;
        }
        if template_bytes > limits.max_bytes {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template {} exceeds the configured per-import byte limit",
                name.to_string_lossy()
            )));
        }
        let name = name.into_string().map_err(|_| {
            BlazeDaemonError::RecoveryRequired(
                "runtime template catalog contains a non-UTF-8 published name".to_string(),
            )
        })?;
        validate_name(&name, "runtime template").map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "runtime template catalog contains invalid published name {name}: {error}"
            ))
        })?;
        read_published_directory(&directory, &name, limits, boundary)?;
    }
    Ok(CatalogUsage {
        bytes: total,
        entries,
    })
}

fn list_published(root: &File, limits: ImportLimits, boundary: CatalogBoundary) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    body.push(b'[');
    let mut listed = 0_usize;
    let names = source_entry_names(root, limits.max_entries).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot inspect runtime template catalog: {error}"
        ))
    })?;
    for name in names {
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if listed >= limits.max_entries {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template catalog exceeds the configured {}-entry limit",
                limits.max_entries
            )));
        }
        let name = name.into_string().map_err(|_| {
            BlazeDaemonError::RecoveryRequired(
                "runtime template catalog contains a non-UTF-8 name".to_string(),
            )
        })?;
        validate_name(&name, "runtime template").map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "runtime template catalog contains invalid published name {name}: {error}"
            ))
        })?;
        // Validate the complete entry, but retain only the bounded discovery
        // summary. Callers can fetch the full metadata through the item route.
        drop(read_published(root, &name, limits, boundary)?);
        if listed > 0 {
            body.push(b',');
        }
        serde_json::to_writer(&mut body, &json!({ "name": name }))?;
        listed += 1;
    }
    body.push(b']');
    Ok(body)
}

fn published_name_exists(root: &File, name: &str, boundary: CatalogBoundary) -> Result<bool> {
    try_open_catalog_directory(root, OsStr::new(name), boundary, Path::new(name))
        .map(|directory| directory.is_some())
        .map_err(|error| catalog_directory_recovery_error(Path::new(name), error))
}

fn get_published(
    root: &File,
    name: &str,
    limits: ImportLimits,
    boundary: CatalogBoundary,
) -> Result<serde_json::Value> {
    let directory =
        match try_open_catalog_directory(root, OsStr::new(name), boundary, Path::new(name))? {
            Some(directory) => directory,
            None => {
                return Err(BlazeDaemonError::NotFound(format!(
                    "runtime template {name}"
                )));
            }
        };
    read_published_directory(&directory, name, limits, boundary)
}

/// Resolve one published template into validated launch inputs.
///
/// Structure and boundary checks match ordinary lookup, then the metadata is
/// parsed as boot metadata, cross-checked for backend consistency, and each
/// artifact is opened and re-hashed so create receives stable file objects.
fn resolve_published(
    root: &File,
    name: &str,
    limits: ImportLimits,
    boundary: CatalogBoundary,
) -> Result<ResolvedTemplate> {
    let directory =
        match try_open_catalog_directory(root, OsStr::new(name), boundary, Path::new(name))? {
            Some(directory) => directory,
            None => {
                return Err(BlazeDaemonError::NotFound(format!(
                    "runtime template {name}"
                )));
            }
        };
    let metadata = read_published_directory(&directory, name, limits, boundary)?;
    let manifest = serde_json::from_value::<TemplateManifest>(metadata).map_err(|error| {
        BlazeDaemonError::Conflict(format!(
            "runtime template {name} does not contain boot metadata: {error}"
        ))
    })?;
    validate_template_manifest(name, &manifest)?;

    let mut artifacts = manifest
        .artifacts
        .iter()
        .map(|artifact| (artifact.name.as_str(), artifact))
        .collect::<HashMap<_, _>>();
    let vmstate = open_verified_template_artifact(
        &directory,
        name,
        artifacts
            .remove("vmstate.snap")
            .expect("validated VM-state manifest"),
        boundary,
    )?;
    let memory = open_verified_template_artifact(
        &directory,
        name,
        artifacts
            .remove("mem.bin")
            .expect("validated memory manifest"),
        boundary,
    )?;
    let rootfs = open_verified_template_artifact(
        &directory,
        name,
        artifacts
            .remove("rootfs.ext4")
            .expect("validated rootfs manifest"),
        boundary,
    )?;

    Ok(ResolvedTemplate {
        name: manifest.name,
        image_digest: manifest.image_digest,
        backend: manifest.backend,
        backend_version: manifest.backend_version,
        boot_args: manifest.boot_args,
        snapshot_kind: manifest.snapshot_kind,
        expose_guest_socket: manifest.expose_guest_socket,
        network: manifest.network,
        vcpus: manifest.vcpus,
        memory_mib: manifest.memory_mib,
        rootfs_size: manifest.rootfs_size,
        memory_size: manifest.memory_size,
        storage: TemplateStorage {
            vmstate,
            memory,
            rootfs,
        },
    })
}

/// Check that manifest metadata describes a self-consistent bootable template.
///
/// Firecracker entries carry the stricter contract: a pinned backend version,
/// the `portable-v1` resource layout, the captured kernel command line,
/// non-zero VM shape, and a `memory_size` that equals `memory_mib` expressed in
/// bytes. All backends must describe the three known artifacts exactly once
/// with lowercase-hex digests and sizes that agree with the recorded rootfs and
/// memory sizes.
fn validate_template_manifest(expected_name: &str, manifest: &TemplateManifest) -> Result<()> {
    let invalid = |reason: &str| {
        BlazeDaemonError::Conflict(format!(
            "runtime template {expected_name} is not bootable: {reason}"
        ))
    };
    if manifest.format_version != 1 {
        return Err(invalid("format_version must be 1"));
    }
    if manifest.name != expected_name {
        return Err(invalid("metadata name does not match the catalog name"));
    }
    if manifest.image_digest.trim().is_empty() {
        return Err(invalid("image_digest must not be empty"));
    }
    if manifest.backend == BackendKind::Firecracker
        && manifest
            .backend_version
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err(invalid("firecracker templates require backend_version"));
    }
    if manifest.backend == BackendKind::Firecracker
        && manifest.resource_layout.as_deref() != Some("portable-v1")
    {
        return Err(invalid(
            "firecracker templates require resource_layout portable-v1",
        ));
    }
    if manifest.backend == BackendKind::Firecracker && manifest.boot_args.is_none() {
        return Err(invalid("firecracker templates require boot_args"));
    }
    if manifest.backend == BackendKind::Firecracker
        && (manifest.vcpus.is_none_or(|vcpus| vcpus == 0)
            || manifest.memory_mib.is_none_or(|memory| memory == 0))
    {
        return Err(invalid(
            "firecracker templates require non-zero vcpus and memory_mib",
        ));
    }
    if manifest.backend == BackendKind::Firecracker {
        let expected_memory_size = manifest
            .memory_mib
            .expect("validated Firecracker memory")
            .checked_mul(1024 * 1024)
            .ok_or_else(|| invalid("memory_mib exceeds the supported artifact size"))?;
        if manifest.memory_size != expected_memory_size {
            return Err(invalid(
                "memory_size must equal memory_mib expressed in bytes",
            ));
        }
    }
    if manifest.rootfs_size == 0 || manifest.memory_size == 0 {
        return Err(invalid(
            "rootfs_size and memory_size must be greater than zero",
        ));
    }
    if manifest.artifacts.len() != 3 {
        return Err(invalid(
            "artifacts must describe vmstate.snap, mem.bin, and rootfs.ext4 exactly once",
        ));
    }

    let mut names = HashSet::new();
    for artifact in &manifest.artifacts {
        if !matches!(
            artifact.name.as_str(),
            "vmstate.snap" | "mem.bin" | "rootfs.ext4"
        ) || !names.insert(artifact.name.as_str())
        {
            return Err(invalid(
                "artifacts must describe vmstate.snap, mem.bin, and rootfs.ext4 exactly once",
            ));
        }
        if artifact.size_bytes == 0 {
            return Err(invalid("artifact sizes must be greater than zero"));
        }
        if artifact.sha256.len() != 64
            || !artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("artifact sha256 values must be lowercase hex"));
        }
        if artifact.name == "rootfs.ext4" && artifact.size_bytes != manifest.rootfs_size {
            return Err(invalid("rootfs_size does not match rootfs.ext4"));
        }
        if artifact.name == "mem.bin" && artifact.size_bytes != manifest.memory_size {
            return Err(invalid("memory_size does not match mem.bin"));
        }
    }
    Ok(())
}

/// Open one published artifact and re-hash it against the manifest values.
///
/// The returned open object binds later materialization to the exact bytes the
/// catalog validated, so a catalog path swapped after this check cannot change
/// what is copied into the sandbox.
fn open_verified_template_artifact(
    directory: &File,
    template: &str,
    expected: &TemplateArtifactManifest,
    boundary: CatalogBoundary,
) -> Result<TemplateArtifact> {
    let mut file = open_published_artifact(
        directory,
        OsStr::new(template),
        OsStr::new(&expected.name),
        boundary,
    )?;
    let metadata = file.metadata()?;
    validate_published_file(&metadata, template, OsStr::new(&expected.name))?;
    if metadata.len() != expected.size_bytes {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template {template} artifact {} has {} bytes; metadata records {}",
            expected.name,
            metadata.len(),
            expected.size_bytes
        )));
    }

    let mut digest = Sha256::new();
    let mut remaining = expected.size_bytes;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template {template} artifact {} ended before its declared size",
                expected.name
            )));
        }
        digest.update(&buffer[..read]);
        remaining -= u64::try_from(read).unwrap_or(remaining);
    }
    let mut trailing = [0_u8; 1];
    if file.read(&mut trailing)? != 0 {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template {template} artifact {} exceeds its declared size",
            expected.name
        )));
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected.sha256 {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template {template} artifact {} digest mismatch",
            expected.name
        )));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(TemplateArtifact {
        file,
        size_bytes: expected.size_bytes,
        sha256: expected.sha256.clone(),
    })
}

fn read_published(
    root: &File,
    name: &str,
    limits: ImportLimits,
    boundary: CatalogBoundary,
) -> Result<serde_json::Value> {
    let directory = open_catalog_directory(root, OsStr::new(name), boundary, Path::new(name))
        .map_err(|error| catalog_directory_recovery_error(Path::new(name), error))?;
    read_published_directory(&directory, name, limits, boundary)
}

fn read_published_directory(
    directory: &File,
    expected_name: &str,
    limits: ImportLimits,
    boundary: CatalogBoundary,
) -> Result<serde_json::Value> {
    let names = source_entry_names(directory, limits.max_files).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot inspect runtime template {expected_name}: {error}"
        ))
    })?;
    if names.len() > limits.max_files {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template {expected_name} exceeds the configured file limit"
        )));
    }
    let mut total_bytes = 0_u64;
    let mut metadata_file = None;
    let mut required_artifacts = [false; 3];
    for name in names {
        validate_artifact_name(&name).map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "runtime template {expected_name} contains an invalid artifact: {error}"
            ))
        })?;
        let file = open_published_artifact(directory, OsStr::new(expected_name), &name, boundary)?;
        let metadata = file.metadata()?;
        validate_published_file(&metadata, expected_name, &name)?;
        if name == OsStr::new("template.json") && metadata.len() > limits.max_metadata_bytes {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template {expected_name} metadata exceeds the configured limit"
            )));
        }
        total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(format!(
                "runtime template {expected_name} byte accounting overflow"
            ))
        })?;
        if name == OsStr::new("template.json") {
            metadata_file = Some(file);
        } else if let Some(index) = ["vmstate.snap", "mem.bin", "rootfs.ext4"]
            .iter()
            .position(|required| name == OsStr::new(required))
        {
            required_artifacts[index] = true;
        }
    }
    if total_bytes > limits.max_bytes {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template {expected_name} exceeds the configured byte limit"
        )));
    }

    let mut metadata = metadata_file.ok_or_else(|| {
        BlazeDaemonError::RecoveryRequired(format!(
            "runtime template {expected_name} is missing regular artifact template.json"
        ))
    })?;
    let value = read_json_bounded(&mut metadata, limits.max_metadata_bytes).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot read runtime template {expected_name} metadata: {error}"
        ))
    })?;
    if !value.is_object()
        || value.get("name").and_then(serde_json::Value::as_str) != Some(expected_name)
    {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template {expected_name} metadata does not match its catalog name"
        )));
    }
    for (required, present) in ["vmstate.snap", "mem.bin", "rootfs.ext4"]
        .into_iter()
        .zip(required_artifacts)
    {
        if !present {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "runtime template {expected_name} is missing regular artifact {required}"
            )));
        }
    }
    Ok(value)
}

fn validate_published_file(
    metadata: &std::fs::Metadata,
    template: &str,
    name: &OsStr,
) -> Result<()> {
    let expected_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_file()
        || metadata.uid() != expected_uid
        || metadata.mode() & 0o777 != CATALOG_FILE_MODE
        || metadata.nlink() != 1
    {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template {template} artifact {} has unexpected type, ownership, mode, or hard-link count",
            name.to_string_lossy()
        )));
    }
    Ok(())
}

fn copy_regular_file_at(
    source: &mut File,
    destination_parent: &File,
    destination_name: &OsStr,
    max_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<u64> {
    let mut destination = create_regular_file_at(destination_parent, destination_name)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut copied = 0_u64;
    loop {
        check_cancelled(cancellation)?;
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let next = copied
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| payload_too_large(u64::MAX, max_bytes))?;
        if next > max_bytes {
            return Err(payload_too_large(next, max_bytes));
        }
        destination.write_all(&buffer[..read])?;
        copied = next;
    }
    destination.sync_all()?;
    destination.set_permissions(std::fs::Permissions::from_mode(CATALOG_FILE_MODE))?;
    Ok(copied)
}

fn create_regular_file_at(parent: &File, name: &OsStr) -> Result<File> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CLOEXEC | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            CATALOG_FILE_MODE,
        )
    };
    Ok(file_from_fd(fd)?)
}

#[cfg(test)]
fn write_file_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(CATALOG_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    file.set_permissions(std::fs::Permissions::from_mode(CATALOG_FILE_MODE))?;
    Ok(())
}

fn write_file_durable_at(parent: &File, name: &OsStr, bytes: &[u8]) -> Result<()> {
    let mut file = create_regular_file_at(parent, name)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    file.set_permissions(std::fs::Permissions::from_mode(CATALOG_FILE_MODE))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_no_replace_at(parent: &File, source: &OsStr, destination: &OsStr) -> io::Result<()> {
    rename_no_replace_at_linux(parent, source, destination)
}

#[cfg(not(target_os = "linux"))]
fn rename_no_replace_at(parent: &File, source: &OsStr, destination: &OsStr) -> io::Result<()> {
    if entry_exists_at(parent, destination)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "destination already exists",
        ));
    }
    let source = CString::new(source.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source name contains NUL"))?;
    let destination = CString::new(destination.as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination name contains NUL")
    })?;
    let result = unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            destination.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rename_no_replace_at_linux(
    parent: &File,
    source: &OsStr,
    destination: &OsStr,
) -> io::Result<()> {
    let source = CString::new(source.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source name contains NUL"))?;
    let destination = CString::new(destination.as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination name contains NUL")
    })?;
    let result = unsafe {
        libc::renameat2(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn read_json_bounded(file: &mut File, limit: u64) -> Result<serde_json::Value> {
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > limit {
        return Err(payload_too_large(actual, limit));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn sync_directory_file(directory: &File) -> Result<()> {
    directory.sync_all()?;
    Ok(())
}

fn unlinkat(parent: &File, name: &OsStr, flags: libc::c_int) -> io::Result<()> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn remove_staging_directory(
    root: &File,
    name: &OsStr,
    max_files: usize,
    boundary: CatalogBoundary,
) -> Result<bool> {
    let directory = match try_open_catalog_directory(root, name, boundary, Path::new(name))? {
        Some(directory) => directory,
        None => return Ok(false),
    };
    let metadata = directory.metadata()?;
    let expected_uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != expected_uid {
        return Err(BlazeDaemonError::RecoveryRequired(format!(
            "runtime template staging entry {} has unexpected ownership or type",
            name.to_string_lossy()
        )));
    }
    let entries = source_entry_names(&directory, max_files).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot inspect runtime template staging entry {}: {error}",
            name.to_string_lossy()
        ))
    })?;
    for entry in entries {
        unlinkat(&directory, &entry, 0).map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "cannot remove runtime template staging artifact {}: {error}",
                entry.to_string_lossy()
            ))
        })?;
    }
    drop(directory);
    unlinkat(root, name, libc::AT_REMOVEDIR)?;
    Ok(true)
}

fn cleanup_staging(root: &File, limits: ImportLimits, boundary: CatalogBoundary) -> Result<usize> {
    let mut removed = 0;
    let names = source_entry_names(root, limits.max_entries).map_err(|error| {
        BlazeDaemonError::RecoveryRequired(format!(
            "cannot inspect runtime template catalog staging entries: {error}"
        ))
    })?;
    for name in names {
        if !is_staging_name(&name) {
            continue;
        }
        if remove_staging_directory(root, &name, limits.max_files, boundary)? {
            removed += 1;
        }
    }
    if removed > 0 {
        sync_directory_file(root)?;
        tracing::info!(
            removed,
            "removed stale runtime template staging directories"
        );
    }
    Ok(removed)
}

fn is_staging_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with(".import-") && name.ends_with(".tmp")
}

fn validate_name(value: &str, label: &str) -> Result<()> {
    let mut chars = value.chars();
    let first = chars.next();
    if value.len() > 128
        || !first.is_some_and(|ch| ch.is_ascii_alphanumeric())
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(BlazeDaemonError::BadRequest(format!(
            "{label} must start with an ASCII letter or digit and contain at most 128 \
             letters, digits, dots, dashes, or underscores"
        )));
    }
    Ok(())
}

fn validate_artifact_name(value: &OsStr) -> Result<()> {
    let value = value.to_str().ok_or_else(|| {
        BlazeDaemonError::BadRequest(
            "runtime template artifact names must be valid UTF-8".to_string(),
        )
    })?;
    validate_name(value, "runtime template artifact")
}

fn validate_relative_source(source: &Path) -> Result<()> {
    if source.as_os_str().is_empty()
        || source
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(BlazeDaemonError::BadRequest(
            "runtime template source must be a non-empty relative path below the configured \
             import root"
                .to_string(),
        ));
    }
    Ok(())
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(BlazeDaemonError::ServiceUnavailable(
            "runtime template import cancelled during daemon shutdown".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn wait_for_copy_gate(inner: &CatalogInner, cancellation: &CancellationToken) {
    let gate = inner.copy_gate.lock().expect("copy gate lock").clone();
    if let Some(gate) = gate {
        let _ = gate.entered.send(());
        while !gate.release.load(Ordering::Acquire) && !cancellation.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
fn wait_for_list_gate(inner: &CatalogInner) {
    wait_for_response_gate(&inner.list_gate);
}

#[cfg(test)]
fn wait_for_response_gate(gate: &Mutex<Option<Arc<TestResponseGate>>>) {
    let gate = gate.lock().expect("response gate lock").clone();
    if let Some(gate) = gate {
        let _ = gate.entered.send(());
        while !gate.release.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

fn payload_too_large(actual: u64, limit: u64) -> BlazeDaemonError {
    BlazeDaemonError::PayloadTooLarge {
        actual,
        limit: usize::try_from(limit).unwrap_or(usize::MAX),
    }
}

fn join_error(context: &'static str) -> impl FnOnce(tokio::task::JoinError) -> BlazeDaemonError {
    move |error| BlazeDaemonError::Internal(format!("{context} task: {error}"))
}

fn lock_catalog_state(inner: &CatalogInner) -> std::sync::MutexGuard<'_, CatalogState> {
    match inner.state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn firecracker_manifest(boot_args: Option<&str>) -> TemplateManifest {
        let artifact = |name: &str, size_bytes: u64| TemplateArtifactManifest {
            name: name.to_string(),
            size_bytes,
            sha256: "0".repeat(64),
        };
        TemplateManifest {
            format_version: 1,
            name: "runtime-base".to_string(),
            image_digest: "sha256:image".to_string(),
            backend: BackendKind::Firecracker,
            backend_version: Some("Firecracker v1.16.0".to_string()),
            resource_layout: Some("portable-v1".to_string()),
            boot_args: boot_args.map(str::to_string),
            snapshot_kind: SnapshotKind::Full,
            expose_guest_socket: false,
            network: false,
            vcpus: Some(1),
            memory_mib: Some(1),
            rootfs_size: 1,
            memory_size: 1024 * 1024,
            artifacts: vec![
                artifact("vmstate.snap", 1),
                artifact("mem.bin", 1024 * 1024),
                artifact("rootfs.ext4", 1),
            ],
        }
    }

    #[test]
    fn firecracker_template_manifest_requires_captured_boot_arguments() {
        validate_template_manifest(
            "runtime-base",
            &firecracker_manifest(Some("console=ttyS0 panic=1")),
        )
        .expect("captured command line");

        let error = validate_template_manifest("runtime-base", &firecracker_manifest(None))
            .expect_err("missing command line must be rejected");
        assert!(matches!(
            error,
            BlazeDaemonError::Conflict(message) if message.contains("require boot_args")
        ));
    }

    fn test_config(root: &Path, import_root: &Path) -> TemplateSection {
        TemplateSection {
            dir: root.to_path_buf(),
            import_root: Some(import_root.to_path_buf()),
            max_files: 8,
            max_bytes: 1024,
            max_metadata_bytes: 512,
            max_total_bytes: 2048,
            max_entries: 8,
        }
    }

    #[test]
    fn validation_accepts_missing_relative_peer_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&catalog).expect("catalog");
        std::fs::create_dir(&import_root).expect("import root");

        let prefix = PathBuf::from(format!(".blaze-missing-relative-peers-{}", Uuid::new_v4()));
        let peer = |name: &str| prefix.join(name);

        validate_template_roots(
            &test_config(&catalog, &import_root),
            &peer("images"),
            &peer("instances"),
            &peer("policies"),
            &HashMap::new(),
            &peer("state"),
            &peer("run/api.sock"),
            None,
        )
        .expect("fresh relative peer paths remain valid");
    }

    #[test]
    fn relative_peer_locations_inside_template_roots_are_rejected() {
        use std::os::unix::fs::symlink;

        let current_dir = std::env::current_dir().expect("current directory");
        for (label, peer_kind) in [
            ("storage.images_dir", "images"),
            ("storage.instances_dir", "instances"),
            ("policy.dir", "policy"),
            ("daemon.state_dir", "state"),
            ("daemon.socket", "socket"),
        ] {
            for use_import_root in [false, true] {
                let temp = tempfile::tempdir_in(&current_dir).expect("tempdir");
                let catalog = temp.path().join("catalog");
                let import_root = temp.path().join("imports");
                let images = temp.path().join("safe-images");
                let instances = temp.path().join("safe-instances");
                let policy = temp.path().join("safe-policy");
                let state = temp.path().join("safe-state");
                for directory in [&catalog, &import_root, &images, &instances, &policy, &state] {
                    std::fs::create_dir(directory).expect("directory");
                }
                let protected_root = if use_import_root {
                    &import_root
                } else {
                    &catalog
                };
                std::fs::set_permissions(protected_root, std::fs::Permissions::from_mode(0o750))
                    .expect("protected root mode");

                let target = temp.path().join(format!("outside-{peer_kind}"));
                if peer_kind == "socket" {
                    std::fs::write(&target, b"socket placeholder").expect("socket target");
                } else {
                    std::fs::create_dir(&target).expect("peer target");
                }
                let configured_link = protected_root.join(format!("configured-{peer_kind}"));
                symlink(&target, &configured_link).expect("configured peer link");
                let relative_link = configured_link
                    .strip_prefix(&current_dir)
                    .expect("relative configured peer")
                    .to_path_buf();

                let peer_images = if peer_kind == "images" {
                    relative_link.as_path()
                } else {
                    &images
                };
                let peer_instances = if peer_kind == "instances" {
                    relative_link.as_path()
                } else {
                    &instances
                };
                let peer_policy = if peer_kind == "policy" {
                    relative_link.as_path()
                } else {
                    &policy
                };
                let peer_state = if peer_kind == "state" {
                    relative_link.as_path()
                } else {
                    &state
                };
                let safe_socket = temp.path().join("safe-run/api.sock");
                let peer_socket = if peer_kind == "socket" {
                    relative_link.as_path()
                } else {
                    &safe_socket
                };

                let error = validate_template_roots(
                    &test_config(&catalog, &import_root),
                    peer_images,
                    peer_instances,
                    peer_policy,
                    &HashMap::new(),
                    peer_state,
                    peer_socket,
                    None,
                )
                .expect_err("configured peer location inside owned root must be rejected");
                assert!(error.to_string().contains(label), "{label}: {error}");
                assert_eq!(
                    std::fs::symlink_metadata(protected_root)
                        .expect("protected root metadata")
                        .mode()
                        & 0o777,
                    0o750,
                    "{label} must fail before changing the protected root"
                );
            }
        }
    }

    #[tokio::test]
    async fn import_publishes_artifacts_with_private_permissions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");

        let metadata = catalog
            .import(
                "runtime-base".to_string(),
                PathBuf::from("source"),
                "base runtime template".to_string(),
            )
            .await
            .expect("import");
        let destination = root.join("runtime-base");

        assert_eq!(metadata["name"], "runtime-base");
        assert_eq!(metadata["description"], "base runtime template");
        assert_eq!(
            std::fs::symlink_metadata(&destination)
                .expect("directory")
                .mode()
                & 0o777,
            CATALOG_DIR_MODE
        );
        for file in ["vmstate.snap", "mem.bin", "rootfs.ext4", "template.json"] {
            assert_eq!(
                std::fs::symlink_metadata(destination.join(file))
                    .expect("artifact")
                    .mode()
                    & 0o777,
                CATALOG_FILE_MODE
            );
        }
    }

    #[tokio::test]
    async fn import_rejects_invalid_source_metadata_as_bad_request() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        std::fs::write(source.join("template.json"), b"{broken").expect("metadata");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");

        let error = catalog
            .import(
                "invalid-metadata".to_string(),
                PathBuf::from("source"),
                String::new(),
            )
            .await
            .expect_err("invalid metadata");

        assert!(matches!(
            &error,
            BlazeDaemonError::BadRequest(message)
                if message.contains("template.json contains invalid JSON")
        ));
        assert_eq!(error.status_code(), 400);
        assert!(!root.join("invalid-metadata").exists());
    }

    #[tokio::test]
    async fn import_rejects_special_entries_and_cleans_staging() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let fifo = CString::new(source.join("fifo").as_os_str().as_bytes()).expect("fifo path");
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");

        catalog
            .import("special".into(), PathBuf::from("source"), String::new())
            .await
            .expect_err("special file");

        assert!(!root.join("special").exists());
        assert_eq!(std::fs::read_dir(root).expect("catalog").count(), 0);
    }

    #[tokio::test]
    async fn import_rejects_special_entry_before_read_open() {
        use std::os::unix::net::UnixListener;

        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let _socket = UnixListener::bind(source.join("00-special")).expect("source socket");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");

        let error = catalog
            .import(
                "special-before-open".into(),
                PathBuf::from("source"),
                String::new(),
            )
            .await
            .expect_err("special file");

        assert!(matches!(
            error,
            BlazeDaemonError::BadRequest(message)
                if message.contains("00-special is not a regular file")
        ));
        assert!(!root.join("special-before-open").exists());
        assert_eq!(std::fs::read_dir(root).expect("catalog").count(), 0);
    }

    #[tokio::test]
    async fn import_rejects_source_entries_beyond_configured_limit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        std::fs::write(source.join("extra-one"), b"one").expect("extra one");
        std::fs::write(source.join("extra-two"), b"two").expect("extra two");
        let mut config = test_config(&root, &import_root);
        config.max_files = 4;
        let catalog = TemplateCatalog::open(&config).expect("catalog");

        let error = catalog
            .import("too-many".into(), PathBuf::from("source"), String::new())
            .await
            .expect_err("source entry limit");

        assert!(matches!(error, BlazeDaemonError::BadRequest(_)));
        assert!(!root.join("too-many").exists());
    }

    #[test]
    fn catalog_open_rejects_symlinked_root_before_changing_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        let alias = temp.path().join("catalog-link");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&target).expect("target");
        std::fs::create_dir(&import_root).expect("import root");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750))
            .expect("target mode");
        symlink(&target, &alias).expect("catalog link");

        assert!(TemplateCatalog::open(&test_config(&alias, &import_root)).is_err());

        assert_eq!(
            std::fs::symlink_metadata(&target)
                .expect("target metadata")
                .mode()
                & 0o777,
            0o750
        );
    }

    #[test]
    fn catalog_component_creation_stays_with_the_pinned_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let configured_parent = temp.path().join("configured-parent");
        let pinned_parent = temp.path().join("pinned-parent");
        let replacement_target = temp.path().join("replacement-target");
        std::fs::create_dir(&configured_parent).expect("configured parent");
        std::fs::create_dir(&replacement_target).expect("replacement target");
        let parent = open_directory_no_follow(&configured_parent).expect("pin parent");

        std::fs::rename(&configured_parent, &pinned_parent).expect("rename pinned parent");
        symlink(&replacement_target, &configured_parent).expect("replacement parent link");

        open_or_create_catalog_component(
            &parent,
            OsStr::new("catalog"),
            &configured_parent.join("catalog"),
        )
        .expect("create below pinned parent");

        assert!(pinned_parent.join("catalog").is_dir());
        assert!(!replacement_target.join("catalog").exists());
    }

    #[test]
    fn import_root_walk_stays_with_the_pinned_ancestor() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let configured_parent = temp.path().join("configured-parent");
        let pinned_parent = temp.path().join("pinned-parent");
        let replacement_target = temp.path().join("replacement-target");
        let pinned_import = configured_parent.join("imports");
        let replacement_import = replacement_target.join("imports");
        std::fs::create_dir_all(&pinned_import).expect("pinned import root");
        std::fs::create_dir_all(&replacement_import).expect("replacement import root");
        let parent = open_directory_no_follow(&configured_parent).expect("pin parent");

        std::fs::rename(&configured_parent, &pinned_parent).expect("rename pinned parent");
        symlink(&replacement_target, &configured_parent).expect("replacement parent link");

        let opened = open_directory_components_no_follow(parent, Path::new("imports"))
            .expect("walk below pinned parent");
        let pinned = std::fs::metadata(pinned_parent.join("imports")).expect("pinned metadata");
        let replacement = std::fs::metadata(replacement_import).expect("replacement metadata");
        let opened = opened.metadata().expect("opened metadata");

        assert_eq!((opened.dev(), opened.ino()), (pinned.dev(), pinned.ino()));
        assert_ne!(
            (pinned.dev(), pinned.ino()),
            (replacement.dev(), replacement.ino())
        );
    }

    #[test]
    fn import_root_walk_rejects_a_linked_ancestor() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        let alias = temp.path().join("alias");
        std::fs::create_dir_all(target.join("imports")).expect("target import root");
        symlink(&target, &alias).expect("ancestor link");

        assert!(open_directory_path_no_follow(&alias.join("imports")).is_err());
    }

    #[test]
    fn resolved_roots_reject_alias_overlap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let storage_alias = temp.path().join("storage-alias");
        let import_root = temp.path().join("imports");
        let instances = temp.path().join("instances");
        let state = temp.path().join("state");
        std::fs::create_dir(&catalog).expect("catalog");
        std::fs::create_dir(&import_root).expect("import root");
        std::fs::create_dir(&instances).expect("instances");
        std::fs::create_dir(&state).expect("state");
        symlink(&catalog, &storage_alias).expect("storage alias");

        validate_template_roots(
            &test_config(&catalog, &import_root),
            &storage_alias,
            &instances,
            &temp.path().join("policies"),
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
        )
        .expect_err("resolved overlap");
    }

    #[test]
    fn mount_table_detects_bind_aliases_and_hidden_ancestry() {
        let mounts = MountTable::parse(
            b"24 1 8:1 / / rw - ext4 /dev/root rw\n\
              25 24 8:1 /srv/storage /mnt/catalog rw - ext4 /dev/root rw\n",
        )
        .expect("mount table");

        assert!(
            paths_overlap_across_mounts(
                Path::new("/srv/storage"),
                Path::new("/mnt/catalog"),
                &mounts,
            )
            .expect("exact alias")
        );
        assert!(
            paths_overlap_across_mounts(
                Path::new("/srv/storage"),
                Path::new("/mnt/catalog/templates/base"),
                &mounts,
            )
            .expect("aliased subtree")
        );
        assert!(
            !paths_overlap_across_mounts(
                Path::new("/srv/other"),
                Path::new("/mnt/catalog"),
                &mounts,
            )
            .expect("disjoint locations")
        );
    }

    #[test]
    fn root_validation_rejects_bind_alias_to_storage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for path in [
            &catalog,
            &import_root,
            &images,
            &instances,
            &policies,
            &state,
        ] {
            std::fs::create_dir(path).expect("root directory");
        }
        let mounts = MountTable {
            entries: vec![
                MountEntry {
                    device: (8, 1),
                    root: PathBuf::from("/"),
                    mount_point: PathBuf::from("/"),
                },
                MountEntry {
                    device: (8, 1),
                    root: images.clone(),
                    mount_point: catalog.clone(),
                },
            ],
        };

        let error = validate_template_root_paths_with_mounts(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            Path::new(HOST_NETWORK_COORDINATION_PATH),
            &[],
            &mounts,
        )
        .expect_err("catalog bind alias must overlap storage");

        assert!(error.to_string().contains("storage.images_dir"));
    }

    #[test]
    fn root_validation_rejects_bind_alias_to_configured_backend_binary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        let backend_root = temp.path().join("backends");
        let binary = backend_root.join("firecracker");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for path in [
            &catalog,
            &import_root,
            &backend_root,
            &images,
            &instances,
            &policies,
            &state,
        ] {
            std::fs::create_dir(path).expect("root directory");
        }
        std::fs::write(&binary, b"firecracker").expect("backend binary");
        let backend_binaries = HashMap::from([("firecracker".to_string(), binary)]);
        let mounts = MountTable {
            entries: vec![
                MountEntry {
                    device: (8, 1),
                    root: PathBuf::from("/"),
                    mount_point: PathBuf::from("/"),
                },
                MountEntry {
                    device: (8, 1),
                    root: backend_root,
                    mount_point: catalog.clone(),
                },
            ],
        };

        let error = validate_template_root_paths_with_mounts(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &backend_binaries,
            &state,
            &temp.path().join("run/api.sock"),
            None,
            Path::new(HOST_NETWORK_COORDINATION_PATH),
            &[],
            &mounts,
        )
        .expect_err("catalog bind alias must overlap the configured backend binary");

        assert!(error.to_string().contains("backends.firecracker"));
    }

    #[test]
    fn root_validation_rejects_path_resolved_host_helper() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let helper_dir = catalog.join("bin");
        let helper = helper_dir.join("ip");
        let import_root = temp.path().join("imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for directory in [
            &helper_dir,
            &import_root,
            &images,
            &instances,
            &policies,
            &state,
        ] {
            std::fs::create_dir_all(directory).expect("directory");
        }
        std::fs::write(&helper, b"helper").expect("helper executable");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))
            .expect("helper mode");
        std::fs::set_permissions(&catalog, std::fs::Permissions::from_mode(0o750))
            .expect("catalog mode");
        let search_path = std::env::join_paths([helper_dir.as_path()]).expect("search path");
        let helper_roots = host_helper_executable_roots_from_search_path(&search_path)
            .expect("resolve host helpers");

        let error = validate_template_root_paths_with_mounts_and_policy_mode(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            &temp.path().join("network.lock"),
            &[],
            &helper_roots,
            &MountTable::default(),
            PolicyLoadErrorMode::Fail,
        )
        .expect_err("catalog must not own a PATH-resolved host helper");

        assert!(error.to_string().contains("host helper executable ip"));
        assert_eq!(
            std::fs::symlink_metadata(&catalog)
                .expect("catalog metadata")
                .mode()
                & 0o777,
            0o750
        );
    }

    #[test]
    fn roots_reject_host_network_coordination_path_before_mode_changes() {
        use std::os::unix::fs::symlink;

        for network_in_import_root in [false, true] {
            for linked_coordination_path in [false, true] {
                let temp = tempfile::tempdir().expect("tempdir");
                let catalog = temp.path().join("catalog");
                let import_root = temp.path().join("imports");
                let images = temp.path().join("images");
                let instances = temp.path().join("instances");
                let policies = temp.path().join("policies");
                let state = temp.path().join("state");
                for directory in [
                    &catalog,
                    &import_root,
                    &images,
                    &instances,
                    &policies,
                    &state,
                ] {
                    std::fs::create_dir(directory).expect("directory");
                }
                for root in [&catalog, &import_root] {
                    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))
                        .expect("root mode");
                }

                let owner = if network_in_import_root {
                    &import_root
                } else {
                    &catalog
                };
                let coordination_path = owner.join("blaze-network.lock");
                let coordination_target = temp.path().join("network-lock-target");
                if linked_coordination_path {
                    std::fs::write(&coordination_target, b"lock").expect("coordination target");
                    symlink(&coordination_target, &coordination_path)
                        .expect("coordination path link");
                }

                let error = match validate_template_roots_with_hook(
                    &test_config(&catalog, &import_root),
                    &images,
                    &instances,
                    &policies,
                    &HashMap::new(),
                    &state,
                    &temp.path().join("run/api.sock"),
                    None,
                    &coordination_path,
                    &[],
                    || {},
                ) {
                    Ok(_) => panic!("runtime-template roots must not own the network lock"),
                    Err(error) => error,
                };

                assert!(
                    error
                        .to_string()
                        .contains("host network coordination configured path")
                );
                for root in [&catalog, &import_root] {
                    assert_eq!(
                        std::fs::symlink_metadata(root)
                            .expect("root metadata")
                            .mode()
                            & 0o777,
                        0o750
                    );
                }
                if linked_coordination_path {
                    assert!(
                        std::fs::symlink_metadata(&coordination_path)
                            .expect("coordination link")
                            .file_type()
                            .is_symlink()
                    );
                    assert_eq!(
                        std::fs::read(&coordination_target).expect("coordination target"),
                        b"lock"
                    );
                } else {
                    assert!(!coordination_path.exists());
                }
            }
        }
    }

    #[test]
    fn roots_reject_resolved_host_network_coordination_target() {
        use std::os::unix::fs::symlink;

        for target_in_import_root in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let catalog = temp.path().join("catalog");
            let import_root = temp.path().join("imports");
            let images = temp.path().join("images");
            let instances = temp.path().join("instances");
            let policies = temp.path().join("policies");
            let state = temp.path().join("state");
            for directory in [
                &catalog,
                &import_root,
                &images,
                &instances,
                &policies,
                &state,
            ] {
                std::fs::create_dir(directory).expect("directory");
            }
            for root in [&catalog, &import_root] {
                std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))
                    .expect("root mode");
            }

            let owner = if target_in_import_root {
                &import_root
            } else {
                &catalog
            };
            let coordination_target = owner.join("network-lock-target");
            let coordination_path = temp.path().join("network-lock-configured");
            std::fs::write(&coordination_target, b"lock").expect("coordination target");
            symlink(&coordination_target, &coordination_path).expect("coordination path link");

            let error = match validate_template_roots_with_hook(
                &test_config(&catalog, &import_root),
                &images,
                &instances,
                &policies,
                &HashMap::new(),
                &state,
                &temp.path().join("run/api.sock"),
                None,
                &coordination_path,
                &[],
                || {},
            ) {
                Ok(_) => panic!("runtime-template roots must not own the network lock target"),
                Err(error) => error,
            };

            assert!(
                error
                    .to_string()
                    .contains("host network coordination resolved target")
            );
            for root in [&catalog, &import_root] {
                assert_eq!(
                    std::fs::symlink_metadata(root)
                        .expect("root metadata")
                        .mode()
                        & 0o777,
                    0o750
                );
            }
            assert_eq!(
                std::fs::read(&coordination_target).expect("coordination target"),
                b"lock"
            );
        }
    }

    #[test]
    fn root_validation_rejects_bind_alias_to_host_network_coordination_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        let network_root = temp.path().join("network-locks");
        let coordination_path = network_root.join("blaze-network.lock");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for path in [
            &catalog,
            &import_root,
            &network_root,
            &images,
            &instances,
            &policies,
            &state,
        ] {
            std::fs::create_dir(path).expect("root directory");
        }
        let mounts = MountTable {
            entries: vec![
                MountEntry {
                    device: (8, 1),
                    root: PathBuf::from("/"),
                    mount_point: PathBuf::from("/"),
                },
                MountEntry {
                    device: (8, 1),
                    root: network_root,
                    mount_point: catalog.clone(),
                },
            ],
        };

        let error = validate_template_root_paths_with_mounts(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            &coordination_path,
            &[],
            &mounts,
        )
        .expect_err("catalog bind alias must overlap host network coordination");

        assert!(
            error
                .to_string()
                .contains("host network coordination configured path")
        );
    }

    #[test]
    fn roots_reject_named_network_namespace_tree_before_mode_changes() {
        for namespace_in_import_root in [false, true] {
            for namespace_exists in [false, true] {
                let temp = tempfile::tempdir().expect("tempdir");
                let catalog = temp.path().join("catalog");
                let import_root = temp.path().join("imports");
                let images = temp.path().join("images");
                let instances = temp.path().join("instances");
                let policies = temp.path().join("policies");
                let state = temp.path().join("state");
                for directory in [
                    &catalog,
                    &import_root,
                    &images,
                    &instances,
                    &policies,
                    &state,
                ] {
                    std::fs::create_dir(directory).expect("directory");
                }
                for root in [&catalog, &import_root] {
                    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))
                        .expect("root mode");
                }

                let owner = if namespace_in_import_root {
                    &import_root
                } else {
                    &catalog
                };
                let namespace_path = owner.join("netns");
                let marker = namespace_path.join("blz-ns-existing");
                if namespace_exists {
                    std::fs::create_dir(&namespace_path).expect("namespace directory");
                    std::fs::write(&marker, b"namespace").expect("namespace marker");
                }
                let coordination_path = temp.path().join("network.lock");

                let error = match validate_template_roots_with_hook(
                    &test_config(&catalog, &import_root),
                    &images,
                    &instances,
                    &policies,
                    &HashMap::new(),
                    &state,
                    &temp.path().join("run/api.sock"),
                    None,
                    &coordination_path,
                    &[namespace_path.as_path()],
                    || {},
                ) {
                    Ok(_) => panic!("runtime-template roots must not own the named netns tree"),
                    Err(error) => error,
                };

                assert!(
                    error
                        .to_string()
                        .contains("host named network namespace configured path")
                );
                for root in [&catalog, &import_root] {
                    assert_eq!(
                        std::fs::symlink_metadata(root)
                            .expect("root metadata")
                            .mode()
                            & 0o777,
                        0o750
                    );
                }
                if namespace_exists {
                    assert_eq!(
                        std::fs::read(&marker).expect("namespace marker"),
                        b"namespace"
                    );
                } else {
                    assert!(!namespace_path.exists());
                }
            }
        }
    }

    #[test]
    fn roots_reject_resolved_named_network_namespace_target() {
        use std::os::unix::fs::symlink;

        for target_in_import_root in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let catalog = temp.path().join("catalog");
            let import_root = temp.path().join("imports");
            let images = temp.path().join("images");
            let instances = temp.path().join("instances");
            let policies = temp.path().join("policies");
            let state = temp.path().join("state");
            for directory in [
                &catalog,
                &import_root,
                &images,
                &instances,
                &policies,
                &state,
            ] {
                std::fs::create_dir(directory).expect("directory");
            }
            for root in [&catalog, &import_root] {
                std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))
                    .expect("root mode");
            }

            let owner = if target_in_import_root {
                &import_root
            } else {
                &catalog
            };
            let namespace_target = owner.join("netns-target");
            let namespace_path = temp.path().join("configured-netns");
            std::fs::create_dir(&namespace_target).expect("namespace target");
            symlink(&namespace_target, &namespace_path).expect("namespace path link");

            let error = match validate_template_roots_with_hook(
                &test_config(&catalog, &import_root),
                &images,
                &instances,
                &policies,
                &HashMap::new(),
                &state,
                &temp.path().join("run/api.sock"),
                None,
                &temp.path().join("network.lock"),
                &[namespace_path.as_path()],
                || {},
            ) {
                Ok(_) => panic!("runtime-template roots must not own a resolved netns target"),
                Err(error) => error,
            };

            assert!(
                error
                    .to_string()
                    .contains("host named network namespace resolved target")
            );
            for root in [&catalog, &import_root] {
                assert_eq!(
                    std::fs::symlink_metadata(root)
                        .expect("root metadata")
                        .mode()
                        & 0o777,
                    0o750
                );
            }
        }
    }

    #[test]
    fn root_validation_rejects_bind_alias_to_named_network_namespace_tree() {
        for namespace_in_import_root in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let catalog = temp.path().join("catalog");
            let import_root = temp.path().join("imports");
            let network_root = temp.path().join("host-network");
            let namespace_path = network_root.join("netns");
            let images = temp.path().join("images");
            let instances = temp.path().join("instances");
            let policies = temp.path().join("policies");
            let state = temp.path().join("state");
            for path in [
                &catalog,
                &import_root,
                &network_root,
                &namespace_path,
                &images,
                &instances,
                &policies,
                &state,
            ] {
                if !path.exists() {
                    std::fs::create_dir(path).expect("root directory");
                }
            }
            let owner = if namespace_in_import_root {
                &import_root
            } else {
                &catalog
            };
            let mounts = MountTable {
                entries: vec![
                    MountEntry {
                        device: (8, 1),
                        root: PathBuf::from("/"),
                        mount_point: PathBuf::from("/"),
                    },
                    MountEntry {
                        device: (8, 1),
                        root: network_root,
                        mount_point: owner.clone(),
                    },
                ],
            };

            let error = validate_template_root_paths_with_mounts(
                &test_config(&catalog, &import_root),
                &images,
                &instances,
                &policies,
                &HashMap::new(),
                &state,
                &temp.path().join("run/api.sock"),
                None,
                &temp.path().join("network.lock"),
                &[namespace_path.as_path()],
                &mounts,
            )
            .expect_err("bind alias must overlap the named netns tree");

            assert!(
                error
                    .to_string()
                    .contains("host named network namespace configured path")
            );
        }
    }

    #[test]
    fn mount_table_decodes_escaped_mount_paths() {
        let mounts = MountTable::parse(
            b"24 1 8:1 / / rw - ext4 /dev/root rw\n\
              25 24 8:1 /srv/runtime\\040templates /mnt/runtime\\040templates rw - ext4 /dev/root rw\n",
        )
        .expect("mount table");

        assert_eq!(
            mounts
                .location(Path::new("/mnt/runtime templates/base"))
                .expect("location")
                .expect("mounted location")
                .path,
            Path::new("/srv/runtime templates/base")
        );
    }

    #[test]
    fn resolved_roots_reject_policy_alias_overlap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let catalog_alias = temp.path().join("catalog-policy-alias");
        let import_root = temp.path().join("imports");
        let import_alias = temp.path().join("import-policy-alias");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let state = temp.path().join("state");
        for directory in [
            &catalog,
            &import_root,
            &images,
            &instances,
            &temp.path().join("policies"),
            &state,
        ] {
            std::fs::create_dir(directory).expect("directory");
        }
        std::fs::set_permissions(&catalog, std::fs::Permissions::from_mode(0o750))
            .expect("catalog mode");
        std::fs::set_permissions(&import_root, std::fs::Permissions::from_mode(0o750))
            .expect("import mode");
        symlink(&catalog, &catalog_alias).expect("catalog policy alias");
        symlink(&import_root, &import_alias).expect("import policy alias");
        let config = test_config(&catalog, &import_root);

        for policy_alias in [&catalog_alias, &import_alias] {
            let error = validate_template_roots(
                &config,
                &images,
                &instances,
                policy_alias,
                &HashMap::new(),
                &state,
                &temp.path().join("run/api.sock"),
                None,
            )
            .expect_err("resolved policy overlap");
            assert!(error.to_string().contains("policy.dir"));
        }

        assert_eq!(
            std::fs::symlink_metadata(&catalog)
                .expect("catalog metadata")
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            std::fs::symlink_metadata(&import_root)
                .expect("import metadata")
                .mode()
                & 0o777,
            0o750
        );
    }

    #[test]
    fn resolved_policy_file_target_is_rejected_before_catalog_mode_changes() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let published = catalog.join("base");
        let policy_target = published.join("policy.toml");
        let import_root = temp.path().join("imports");
        let policies = temp.path().join("policies");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let state = temp.path().join("state");
        for directory in [
            &published,
            &import_root,
            &policies,
            &images,
            &instances,
            &state,
        ] {
            std::fs::create_dir_all(directory).expect("directory");
        }
        std::fs::write(&policy_target, b"policy_name = 'catalog-target'\n").expect("policy target");
        std::fs::set_permissions(&catalog, std::fs::Permissions::from_mode(0o750))
            .expect("catalog mode");
        std::fs::set_permissions(&published, std::fs::Permissions::from_mode(0o750))
            .expect("published mode");
        std::fs::set_permissions(&policy_target, std::fs::Permissions::from_mode(0o640))
            .expect("policy mode");
        symlink(&policy_target, policies.join("active.toml")).expect("policy link");

        for error_mode in [PolicyLoadErrorMode::Fail, PolicyLoadErrorMode::Warn] {
            let error = validate_template_roots_with_policy_mode(
                &test_config(&catalog, &import_root),
                &images,
                &instances,
                &policies,
                &HashMap::new(),
                &state,
                &temp.path().join("run/api.sock"),
                None,
                error_mode,
            )
            .expect_err("resolved policy file must not overlap the catalog");

            assert!(error.to_string().contains("policy entry"));
        }
        assert_eq!(
            std::fs::symlink_metadata(&catalog)
                .expect("catalog metadata")
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            std::fs::symlink_metadata(&policy_target)
                .expect("policy metadata")
                .mode()
                & 0o777,
            0o640
        );
    }

    #[test]
    fn lifecycle_root_is_rejected_before_catalog_changes_sandbox_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let sandbox = state.join("86b59faf-3b91-46e4-9db0-2468b8336eb6");
        let state_file = sandbox.join("state.json");
        let import_root = temp.path().join("imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        for directory in [&sandbox, &import_root, &images, &instances] {
            std::fs::create_dir_all(directory).expect("directory");
        }
        std::fs::write(&state_file, b"state").expect("state file");
        std::fs::set_permissions(&sandbox, std::fs::Permissions::from_mode(0o750))
            .expect("sandbox mode");
        std::fs::set_permissions(&state_file, std::fs::Permissions::from_mode(0o640))
            .expect("state mode");

        validate_template_roots(
            &test_config(&state, &import_root),
            &images,
            &instances,
            &temp.path().join("policies"),
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
        )
        .expect_err("catalog cannot own lifecycle root");

        assert_eq!(
            std::fs::symlink_metadata(&sandbox)
                .expect("sandbox metadata")
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            std::fs::symlink_metadata(&state_file)
                .expect("state metadata")
                .mode()
                & 0o777,
            0o640
        );
    }

    #[test]
    fn lifecycle_root_allows_non_uuid_catalog_child() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let catalog = state.join("runtime-templates");
        let import_root = state.join("runtime-template-imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        for directory in [
            &state,
            &catalog,
            &import_root,
            &images,
            &instances,
            &temp.path().join("policies"),
        ] {
            std::fs::create_dir_all(directory).expect("directory");
        }

        validate_template_roots(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &temp.path().join("policies"),
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
        )
        .expect("non-UUID children do not overlap lifecycle entries");
    }

    #[test]
    fn resolved_roots_reject_socket_alias_overlap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let socket_alias = temp.path().join("socket-alias");
        let import_root = temp.path().join("imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let state = temp.path().join("state");
        for directory in [
            &catalog,
            &import_root,
            &images,
            &instances,
            &temp.path().join("policies"),
            &state,
        ] {
            std::fs::create_dir(directory).expect("directory");
        }
        symlink(&catalog, &socket_alias).expect("socket parent alias");

        validate_template_roots(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &temp.path().join("policies"),
            &HashMap::new(),
            &state,
            &socket_alias.join("api.sock"),
            None,
        )
        .expect_err("resolved socket overlap");
    }

    #[test]
    fn resolved_import_root_rejects_socket_alias_overlap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let socket_alias = temp.path().join("socket-alias");
        let import_root = temp.path().join("imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let state = temp.path().join("state");
        for directory in [&catalog, &import_root, &images, &instances, &state] {
            std::fs::create_dir(directory).expect("directory");
        }
        symlink(&import_root, &socket_alias).expect("socket parent alias");

        let error = validate_template_roots(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &temp.path().join("policies"),
            &HashMap::new(),
            &state,
            &socket_alias.join("api.sock"),
            None,
        )
        .expect_err("resolved import root and socket overlap");
        assert!(error.to_string().contains("daemon.socket"));
    }

    #[test]
    fn validation_rejects_catalog_replacement_before_changing_modes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let images = temp.path().join("images");
        let exchange = temp.path().join("exchange");
        let import_root = temp.path().join("imports");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for directory in [
            &catalog,
            &images,
            &import_root,
            &instances,
            &policies,
            &state,
        ] {
            std::fs::create_dir(directory).expect("directory");
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o750))
                .expect("mode");
        }

        let error = match validate_template_roots_with_hook(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            Path::new(HOST_NETWORK_COORDINATION_PATH),
            &[],
            || {
                std::fs::rename(&catalog, &exchange).expect("move catalog");
                std::fs::rename(&images, &catalog).expect("move images");
                std::fs::rename(&exchange, &images).expect("move catalog into images path");
            },
        ) {
            Ok(_) => panic!("replaced catalog path must be rejected"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("changed while startup boundaries")
        );
        for directory in [&catalog, &images] {
            assert_eq!(
                std::fs::symlink_metadata(directory)
                    .expect("directory metadata")
                    .mode()
                    & 0o777,
                0o750
            );
        }
    }

    #[test]
    fn validation_rejects_object_appearing_at_a_planned_catalog_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let images = temp.path().join("images");
        let import_root = temp.path().join("imports");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for directory in [&images, &import_root, &instances, &policies, &state] {
            std::fs::create_dir(directory).expect("directory");
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o750))
                .expect("mode");
        }
        std::fs::write(images.join("marker"), b"storage image").expect("image marker");

        let error = match validate_template_roots_with_hook(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            Path::new(HOST_NETWORK_COORDINATION_PATH),
            &[],
            || std::fs::rename(&images, &catalog).expect("move images into catalog path"),
        ) {
            Ok(_) => panic!("an object appearing at the planned catalog path must be rejected"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("appeared while startup boundaries were being validated")
        );
        assert_eq!(
            std::fs::symlink_metadata(&catalog)
                .expect("moved image directory metadata")
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            std::fs::read(catalog.join("marker")).expect("image marker remains"),
            b"storage image"
        );
    }

    #[test]
    fn validation_rejects_replacement_of_a_planned_catalog_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let configured_parent = temp.path().join("configured-parent");
        let detached_parent = temp.path().join("detached-parent");
        let catalog = configured_parent.join("catalog");
        let import_root = temp.path().join("imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for directory in [
            &configured_parent,
            &import_root,
            &images,
            &instances,
            &policies,
            &state,
        ] {
            std::fs::create_dir(directory).expect("directory");
        }

        let error = match validate_template_roots_with_hook(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            Path::new(HOST_NETWORK_COORDINATION_PATH),
            &[],
            || {
                std::fs::rename(&configured_parent, &detached_parent)
                    .expect("detach planned parent");
                std::fs::create_dir(&configured_parent).expect("replacement parent");
            },
        ) {
            Ok(_) => panic!("a replaced planned catalog parent must be rejected"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("changed while startup boundaries were being validated")
        );
        assert!(!configured_parent.join("catalog").exists());
        assert!(!detached_parent.join("catalog").exists());
    }

    #[test]
    fn policy_entry_discovery_failure_honors_warn_mode() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for directory in [
            &catalog,
            &import_root,
            &images,
            &instances,
            &policies,
            &state,
        ] {
            std::fs::create_dir(directory).expect("directory");
        }
        symlink("loop.toml", policies.join("loop.toml")).expect("policy loop");

        let roots = validate_template_roots_with_policy_mode(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            PolicyLoadErrorMode::Warn,
        )
        .expect("warn mode must continue with the empty policy engine");
        assert_eq!(
            roots.policy_load_disposition(),
            PolicyLoadDisposition::UseEmpty
        );

        validate_template_roots_with_policy_mode(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            PolicyLoadErrorMode::Fail,
        )
        .expect_err("fail mode must report policy entry discovery failure");
    }

    #[test]
    fn policy_discovery_fallback_is_sticky_after_the_directory_recovers() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        let policies = temp.path().join("policies");
        let state = temp.path().join("state");
        for directory in [
            &catalog,
            &import_root,
            &images,
            &instances,
            &policies,
            &state,
        ] {
            std::fs::create_dir(directory).expect("directory");
        }
        let policy = policies.join("recovered.toml");
        symlink("recovered.toml", &policy).expect("policy loop");

        let roots = validate_template_roots_with_hook_and_policy_mode(
            &test_config(&catalog, &import_root),
            &images,
            &instances,
            &policies,
            &HashMap::new(),
            &state,
            &temp.path().join("run/api.sock"),
            None,
            &temp.path().join("network.lock"),
            &[],
            PolicyLoadErrorMode::Warn,
            || {
                std::fs::remove_file(&policy).expect("remove policy loop");
                std::fs::write(&policy, "policy_name = 'recovered'\n")
                    .expect("write recovered policy");
            },
        )
        .expect("warn mode must retain its fallback decision");

        assert_eq!(
            roots.policy_load_disposition(),
            PolicyLoadDisposition::UseEmpty
        );
    }

    #[tokio::test]
    async fn import_does_not_follow_source_links() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        symlink("mem.bin", source.join("linked-memory")).expect("source link");
        symlink("source", import_root.join("source-link")).expect("directory link");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");

        catalog
            .import("file-link".into(), PathBuf::from("source"), String::new())
            .await
            .expect_err("file link");
        catalog
            .import(
                "directory-link".into(),
                PathBuf::from("source-link"),
                String::new(),
            )
            .await
            .expect_err("directory link");

        assert_eq!(std::fs::read_dir(root).expect("catalog").count(), 0);
    }

    #[tokio::test]
    async fn metadata_and_catalog_capacity_are_enforced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let mut config = test_config(&root, &import_root);
        config.max_metadata_bytes = 64;
        let catalog = TemplateCatalog::open(&config).expect("catalog");

        let error = catalog
            .import("metadata".into(), PathBuf::from("source"), "x".repeat(128))
            .await
            .expect_err("metadata limit");
        assert!(matches!(error, BlazeDaemonError::PayloadTooLarge { .. }));
        let owner = Arc::downgrade(&catalog.inner);
        drop(catalog);
        assert!(owner.upgrade().is_none(), "completed import released owner");

        let mut config = test_config(&root, &import_root);
        config.max_total_bytes = 8;
        let catalog = TemplateCatalog::open(&config).expect("catalog");
        let error = catalog
            .import("capacity".into(), PathBuf::from("source"), String::new())
            .await
            .expect_err("catalog capacity");
        assert!(matches!(error, BlazeDaemonError::PayloadTooLarge { .. }));
    }

    #[tokio::test]
    async fn catalog_entry_limit_bounds_import_and_listing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        write_artifacts(&import_root.join("first"));
        write_artifacts(&import_root.join("second"));
        let root = temp.path().join("catalog");
        let mut config = test_config(&root, &import_root);
        config.max_entries = 1;
        let catalog = TemplateCatalog::open(&config).expect("catalog");

        catalog
            .import("first".into(), PathBuf::from("first"), String::new())
            .await
            .expect("first import");
        let error = catalog
            .import("second".into(), PathBuf::from("second"), String::new())
            .await
            .expect_err("entry limit");

        assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        let listed = catalog.list().await.expect("bounded list");
        let listed: serde_json::Value = serde_json::from_slice(&listed).expect("list JSON");
        assert_eq!(listed.as_array().expect("list array").len(), 1);
        let owner = Arc::downgrade(&catalog.inner);
        drop(catalog);
        assert!(owner.upgrade().is_none(), "completed list released owner");
        TemplateCatalog::open(&config).expect("bounded catalog reopens");
    }

    #[tokio::test]
    async fn catalog_list_returns_bounded_summaries_and_holds_single_flight_permit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let second_source = import_root.join("second");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        write_artifacts(&second_source);
        std::fs::write(
            source.join("template.json"),
            serde_json::to_vec(&json!({
                "description": "large discovery detail",
                "private_detail": "x".repeat(256),
            }))
            .expect("metadata JSON"),
        )
        .expect("metadata");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog
            .import("zeta".into(), PathBuf::from("source"), String::new())
            .await
            .expect("import");
        catalog
            .import("alpha".into(), PathBuf::from("second"), String::new())
            .await
            .expect("second import");

        let first = catalog.list().await.expect("first list");
        let retained = first.clone();
        let listed: serde_json::Value = serde_json::from_slice(&first).expect("list JSON");
        assert_eq!(listed, json!([{ "name": "alpha" }, { "name": "zeta" }]));
        assert!(listed[0].get("private_detail").is_none());
        let detail = catalog.get("zeta".into()).await.expect("full metadata");
        let detail: serde_json::Value = serde_json::from_slice(&detail).expect("metadata JSON");
        assert_eq!(detail["private_detail"], "x".repeat(256));

        let error = catalog
            .list()
            .await
            .expect_err("a retained list body must hold the permit");
        assert!(matches!(error, BlazeDaemonError::ServiceUnavailable(_)));
        drop(first);
        let error = catalog
            .list()
            .await
            .expect_err("a cloned list body must retain the permit");
        assert!(matches!(error, BlazeDaemonError::ServiceUnavailable(_)));
        drop(retained);

        let retry = catalog.list().await.expect("list after body release");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&retry).expect("retry JSON"),
            json!([{ "name": "alpha" }, { "name": "zeta" }])
        );
    }

    #[tokio::test]
    async fn catalog_get_holds_single_flight_permit_until_response_release() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let second_source = import_root.join("second");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        write_artifacts(&second_source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog
            .import("published".into(), PathBuf::from("source"), String::new())
            .await
            .expect("import");
        catalog
            .import("second".into(), PathBuf::from("second"), String::new())
            .await
            .expect("second import");

        let missing = catalog
            .get("missing".into())
            .await
            .expect_err("missing item");
        assert!(matches!(missing, BlazeDaemonError::NotFound(_)));

        let first = catalog.get("published".into()).await.expect("first get");
        let retained = first.clone();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&first).expect("metadata JSON")["name"],
            "published"
        );
        let listed = catalog
            .list()
            .await
            .expect("item response must not consume the independent list permit");
        drop(listed);
        let error = catalog
            .get("second".into())
            .await
            .expect_err("the item permit must be global across names");
        assert!(matches!(error, BlazeDaemonError::ServiceUnavailable(_)));
        drop(first);
        let error = catalog
            .get("published".into())
            .await
            .expect_err("a cloned item body must retain the permit");
        assert!(matches!(error, BlazeDaemonError::ServiceUnavailable(_)));
        drop(retained);

        let retry = catalog
            .get("published".into())
            .await
            .expect("get after body release");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&retry).expect("retry JSON")["name"],
            "published"
        );
    }

    #[tokio::test]
    async fn cancelled_list_keeps_its_permit_until_blocking_scan_exits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog
            .import("published".into(), PathBuf::from("source"), String::new())
            .await
            .expect("import");
        let (mut entered, gate) = catalog.install_list_gate();
        let list = tokio::spawn({
            let catalog = catalog.clone();
            async move { catalog.list().await }
        });
        entered.recv().await.expect("list scan entered");

        list.abort();
        assert!(list.await.expect_err("list task cancelled").is_cancelled());
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(100), catalog.list()).await;
        gate.release.store(true, Ordering::Release);
        let error = blocked
            .expect("a second list must not enter the blocking scan")
            .expect_err("detached blocking scan must retain the permit");
        assert!(matches!(error, BlazeDaemonError::ServiceUnavailable(_)));

        let retry = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                match catalog.list().await {
                    Ok(body) => break body,
                    Err(BlazeDaemonError::ServiceUnavailable(_)) => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                    Err(error) => panic!("unexpected retry error: {error}"),
                }
            }
        })
        .await
        .expect("detached list scan releases permit");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&retry).expect("retry JSON"),
            json!([{ "name": "published" }])
        );
    }

    #[tokio::test]
    async fn cancelled_get_keeps_its_permit_until_blocking_read_exits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog
            .import("published".into(), PathBuf::from("source"), String::new())
            .await
            .expect("import");
        let (mut entered, gate) = catalog.install_get_gate();
        let get = tokio::spawn({
            let catalog = catalog.clone();
            async move { catalog.get("published".into()).await }
        });
        entered.recv().await.expect("item read entered");

        get.abort();
        assert!(get.await.expect_err("get task cancelled").is_cancelled());
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            catalog.get("published".into()),
        )
        .await;
        gate.release.store(true, Ordering::Release);
        let error = blocked
            .expect("a second get must not enter the blocking read")
            .expect_err("detached blocking read must retain the permit");
        assert!(matches!(error, BlazeDaemonError::ServiceUnavailable(_)));

        let retry = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                match catalog.get("published".into()).await {
                    Ok(body) => break body,
                    Err(BlazeDaemonError::ServiceUnavailable(_)) => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                    Err(error) => panic!("unexpected retry error: {error}"),
                }
            }
        })
        .await
        .expect("detached item read releases permit");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&retry).expect("retry JSON")["name"],
            "published"
        );
    }

    #[tokio::test]
    async fn catalog_list_rejects_invalid_published_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog
            .import("valid".into(), PathBuf::from("source"), String::new())
            .await
            .expect("import");
        std::fs::rename(root.join("valid"), root.join("invalid name"))
            .expect("rename published entry");

        let error = catalog.list().await.expect_err("invalid published name");
        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(error.to_string().contains("invalid published name"));

        std::fs::rename(root.join("invalid name"), root.join("valid"))
            .expect("restore published entry");
        catalog.list().await.expect("permit released after error");
    }

    #[tokio::test]
    async fn duplicate_import_is_rejected_before_source_preparation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog
            .import("existing".into(), PathBuf::from("source"), String::new())
            .await
            .expect("initial import");

        let error = catalog
            .import(
                "existing".into(),
                PathBuf::from("source-that-does-not-exist"),
                String::new(),
            )
            .await
            .expect_err("duplicate name must win before source lookup");

        assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        assert_eq!(std::fs::read_dir(&root).expect("catalog").count(), 1);
    }

    #[tokio::test]
    async fn catalog_operations_stay_bound_to_the_opened_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        write_artifacts(&import_root.join("source"));
        let root = temp.path().join("catalog");
        let detached_root = temp.path().join("opened-catalog");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");

        std::fs::rename(&root, &detached_root).expect("rename opened catalog");
        std::fs::create_dir(&root).expect("replacement catalog path");
        std::fs::write(root.join("marker"), b"replacement").expect("replacement marker");

        catalog
            .import("pinned".into(), PathBuf::from("source"), String::new())
            .await
            .expect("import into pinned catalog");

        assert!(detached_root.join("pinned").is_dir());
        assert!(!root.join("pinned").exists());
        assert_eq!(
            std::fs::read(root.join("marker")).expect("replacement marker remains"),
            b"replacement"
        );
        let listed = catalog.list().await.expect("list pinned catalog");
        let listed: serde_json::Value = serde_json::from_slice(&listed).expect("list JSON");
        assert_eq!(listed.as_array().expect("list array").len(), 1);
        let fetched = catalog
            .get("pinned".into())
            .await
            .expect("get pinned template");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fetched).expect("template JSON")["name"],
            "pinned"
        );
    }

    #[tokio::test]
    async fn imports_stay_bound_to_the_opened_import_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let detached_root = temp.path().join("opened-imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        std::fs::write(source.join("mem.bin"), b"pinned-memory").expect("pinned memory");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");

        std::fs::rename(&import_root, &detached_root).expect("rename opened import root");
        let replacement_source = import_root.join("source");
        write_artifacts(&replacement_source);
        std::fs::write(replacement_source.join("mem.bin"), b"replacement-memory")
            .expect("replacement memory");

        catalog
            .import(
                "pinned-source".into(),
                PathBuf::from("source"),
                String::new(),
            )
            .await
            .expect("import from pinned root");

        assert_eq!(
            std::fs::read(root.join("pinned-source/mem.bin")).expect("published memory"),
            b"pinned-memory"
        );
        assert_eq!(
            std::fs::read(replacement_source.join("mem.bin")).expect("replacement remains"),
            b"replacement-memory"
        );
    }

    #[tokio::test]
    async fn import_removes_empty_staging_when_initial_open_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog.fail_next_staging_open();

        let error = catalog
            .import("interrupted".into(), PathBuf::from("source"), String::new())
            .await
            .expect_err("staging open failure");

        assert!(matches!(error, BlazeDaemonError::Io(_)));
        assert_eq!(std::fs::read_dir(&root).expect("catalog").count(), 0);
        catalog
            .import("retry".into(), PathBuf::from("source"), String::new())
            .await
            .expect("catalog remains usable after confirmed cleanup");
        assert!(root.join("retry").is_dir());
    }

    #[test]
    fn failed_staging_cleanup_returns_recovery_for_current_and_later_imports() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        write_artifacts(&import_root.join("source"));
        let root = temp.path().join("catalog");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog.fail_next_staging_cleanup();

        let error = publish_cancelled_import(&catalog, "cleanup-failed");

        assert!(matches!(
            &error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains("import cancelled during daemon shutdown")
                    && message.contains("staging cleanup could not be confirmed")
        ));
        assert_eq!(error.status_code(), 500);
        assert!(matches!(
            ImportClaim::begin(Arc::clone(&catalog.inner), "later".into()),
            Err(BlazeDaemonError::RecoveryRequired(_))
        ));
    }

    #[test]
    fn failed_cleanup_sync_returns_recovery_for_current_and_later_imports() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        write_artifacts(&import_root.join("source"));
        let root = temp.path().join("catalog");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog.fail_next_cleanup_sync();

        let error = publish_cancelled_import(&catalog, "sync-failed");

        assert!(matches!(
            &error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains("import cancelled during daemon shutdown")
                    && message.contains("cleanup durability is unknown")
        ));
        assert_eq!(error.status_code(), 500);
        assert_eq!(std::fs::read_dir(&root).expect("catalog").count(), 0);
        assert!(matches!(
            ImportClaim::begin(Arc::clone(&catalog.inner), "later".into()),
            Err(BlazeDaemonError::RecoveryRequired(_))
        ));
    }

    #[test]
    fn concurrent_reservations_share_one_catalog_limit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");
        let root = temp.path().join("catalog");
        let mut config = test_config(&root, &import_root);
        config.max_total_bytes = 100;
        let catalog = TemplateCatalog::open(&config).expect("catalog");
        let mut first =
            ImportClaim::begin(Arc::clone(&catalog.inner), "first".into()).expect("first claim");
        let mut second =
            ImportClaim::begin(Arc::clone(&catalog.inner), "second".into()).expect("second claim");

        first.reserve(60).expect("first reservation");
        assert!(matches!(
            second.reserve(60),
            Err(BlazeDaemonError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn accounting_failure_blocks_later_imports() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");
        let root = temp.path().join("catalog");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        let mut claim =
            ImportClaim::begin(Arc::clone(&catalog.inner), "first".into()).expect("claim");
        claim.reserve(10).expect("reservation");

        let error = claim.publish(11).expect_err("reservation mismatch");
        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(matches!(
            ImportClaim::begin(Arc::clone(&catalog.inner), "later".into()),
            Err(BlazeDaemonError::RecoveryRequired(_))
        ));
    }

    #[test]
    fn copy_counts_bytes_read_after_preflight() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_path = temp.path().join("source");
        std::fs::write(&source_path, b"one").expect("source");
        let mut source = OpenOptions::new()
            .read(true)
            .open(&source_path)
            .expect("open source");
        std::fs::write(&source_path, b"longer").expect("grow source");
        let destination_root = open_directory_no_follow(temp.path()).expect("destination root");

        let error = copy_regular_file_at(
            &mut source,
            &destination_root,
            OsStr::new("destination"),
            3,
            &CancellationToken::new(),
        )
        .expect_err("actual bytes exceed reservation");

        assert!(matches!(error, BlazeDaemonError::PayloadTooLarge { .. }));
    }

    #[tokio::test]
    async fn shutdown_waits_for_registered_import_claims() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");
        let root = temp.path().join("catalog");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        let claim = ImportClaim::begin(Arc::clone(&catalog.inner), "active".into()).expect("claim");
        assert_eq!(catalog.active_imports(), 1);

        catalog.cancel_imports();
        let waiting = tokio::spawn({
            let catalog = catalog.clone();
            async move { catalog.wait_for_imports().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        drop(claim);
        waiting.await.expect("join").expect("imports stopped");
        assert_eq!(catalog.active_imports(), 0);
        assert!(matches!(
            ImportClaim::begin(Arc::clone(&catalog.inner), "late".into()),
            Err(BlazeDaemonError::ServiceUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn shutdown_cancels_copy_and_removes_staging() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        let mut entered = catalog.install_copy_gate();
        let import = tokio::spawn({
            let catalog = catalog.clone();
            async move {
                catalog
                    .import("cancelled".into(), PathBuf::from("source"), String::new())
                    .await
            }
        });
        entered.recv().await.expect("copy entered");

        catalog.cancel_imports();
        catalog.wait_for_imports().await.expect("imports quiescent");
        let error = import
            .await
            .expect("import task")
            .expect_err("cancelled import");

        assert!(matches!(error, BlazeDaemonError::ServiceUnavailable(_)));
        assert!(!root.join("cancelled").exists());
        assert_eq!(std::fs::read_dir(root).expect("catalog").count(), 0);
    }

    #[test]
    fn list_reports_corrupt_published_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");
        let root = temp.path().join("catalog");
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        let published = root.join("published");
        create_private_directory(&published).expect("published");
        write_file_durable(&published.join("template.json"), b"{broken").expect("metadata");

        let error = list_published(
            &catalog.inner.root,
            catalog.inner.limits,
            catalog.inner.boundary,
        )
        .expect_err("corrupt metadata");
        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classified_directory_open_stays_bound_after_name_replacement() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("entry");
        let detached = temp.path().join("detached");
        std::fs::create_dir(&original).expect("original directory");
        std::fs::write(original.join("marker"), b"original").expect("original marker");
        let parent = open_directory_no_follow(temp.path()).expect("catalog root");
        let pinned =
            PinnedDirectoryEntry::pin(&parent, OsStr::new("entry")).expect("pin catalog directory");
        let classified = pinned.classify().expect("classify catalog directory");

        std::fs::rename(&original, &detached).expect("detach classified directory");
        std::fs::create_dir(&original).expect("replacement directory");
        std::fs::write(original.join("marker"), b"replacement").expect("replacement marker");
        let readable = classified
            .open_readable()
            .expect("open classified directory")
            .validate_identity()
            .expect("validate opened directory");
        let opened = readable.metadata().expect("opened metadata");
        let detached = std::fs::metadata(&detached).expect("detached metadata");
        let replacement = std::fs::metadata(&original).expect("replacement metadata");

        assert_eq!(
            (opened.dev(), opened.ino()),
            (detached.dev(), detached.ino())
        );
        assert_ne!(
            (opened.dev(), opened.ino()),
            (replacement.dev(), replacement.ino())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classified_regular_open_stays_bound_after_name_replacement() {
        use std::os::unix::net::UnixListener;

        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("artifact");
        let detached = temp.path().join("detached");
        std::fs::write(&original, b"classified artifact").expect("artifact");
        let directory = open_directory_no_follow(temp.path()).expect("directory");
        let pinned = PinnedRegularEntry::pin(&directory, OsStr::new("artifact"))
            .expect("pin regular artifact");
        let classified = pinned.classify().expect("classify regular artifact");

        std::fs::rename(&original, &detached).expect("detach classified artifact");
        let _replacement = UnixListener::bind(&original).expect("replacement socket");
        let mut readable = classified
            .into_readable()
            .expect("open classified artifact");
        let mut contents = String::new();
        readable
            .read_to_string(&mut contents)
            .expect("read classified artifact");

        assert_eq!(contents, "classified artifact");
        assert!(
            !std::fs::symlink_metadata(&original)
                .expect("replacement metadata")
                .file_type()
                .is_file()
        );
    }

    #[tokio::test]
    async fn catalog_reads_and_startup_classify_special_artifacts_before_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        let root = temp.path().join("catalog");
        write_artifacts(&source);
        let catalog = TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");
        catalog
            .import("published".into(), PathBuf::from("source"), String::new())
            .await
            .expect("publish template");
        let special = root.join("published/00-special");
        let special_path = CString::new(special.as_os_str().as_bytes()).expect("special path");
        assert_eq!(unsafe { libc::mkfifo(special_path.as_ptr(), 0o600) }, 0);

        let get_error = catalog
            .get("published".to_string())
            .await
            .expect_err("get must reject special artifact");
        let list_error = catalog
            .list()
            .await
            .expect_err("list must reject special artifact");
        for error in [&get_error, &list_error] {
            assert!(matches!(
                error,
                BlazeDaemonError::RecoveryRequired(message)
                    if message.contains("entry 00-special is not a regular file")
            ));
        }

        drop(catalog);
        let startup_error = TemplateCatalog::open(&test_config(&root, &import_root))
            .err()
            .expect("startup must reject special artifact");
        assert!(matches!(
            startup_error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains("entry 00-special is not a regular file")
        ));
        assert!(
            !std::fs::symlink_metadata(&special)
                .expect("special entry metadata")
                .file_type()
                .is_file()
        );
    }

    #[test]
    fn startup_rejects_hard_linked_artifact_before_changing_peer_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        let root = temp.path().join("catalog");
        let published = root.join("published");
        let peer = temp.path().join("peer-artifact");
        std::fs::create_dir(&import_root).expect("import root");
        std::fs::create_dir(&root).expect("catalog root");
        std::fs::create_dir(&published).expect("published entry");
        std::fs::write(&peer, b"snapshot").expect("peer artifact");
        std::fs::set_permissions(&peer, std::fs::Permissions::from_mode(0o644)).expect("peer mode");
        std::fs::hard_link(&peer, published.join("vmstate.snap")).expect("hard link");
        std::fs::write(published.join("mem.bin"), b"memory").expect("memory");
        std::fs::write(published.join("rootfs.ext4"), b"rootfs").expect("rootfs");
        std::fs::write(published.join("template.json"), br#"{"name":"published"}"#)
            .expect("metadata");

        let error = TemplateCatalog::open(&test_config(&root, &import_root))
            .err()
            .expect("hard-linked artifact must fail startup");

        assert!(error.to_string().contains("exactly one hard link"));
        assert_eq!(
            std::fs::symlink_metadata(&peer)
                .expect("peer metadata")
                .mode()
                & 0o777,
            0o644
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mount_id_falls_back_to_fdinfo_when_statx_field_is_unavailable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = open_directory_no_follow(temp.path()).expect("catalog root");
        let expected = fdinfo_mount_id(&catalog).expect("fdinfo mount id");

        assert_eq!(
            opened_mount_id_from_statx(&catalog, Ok(None)).expect("missing statx field fallback"),
            expected
        );
        assert_eq!(
            opened_mount_id_from_statx(&catalog, Err(io::Error::from_raw_os_error(libc::ENOSYS)),)
                .expect("unavailable statx fallback"),
            expected
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fdinfo_mount_id_rejects_missing_or_invalid_values() {
        assert_eq!(
            parse_fdinfo_mount_id("pos:\t0\nflags:\t0100000\n")
                .expect_err("missing mount id")
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            parse_fdinfo_mount_id("mnt_id:\tnot-a-number\n")
                .expect_err("invalid mount id")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn catalog_object_rejects_nested_mount_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = open_directory_no_follow(temp.path()).expect("catalog root");
        let proc_root = open_directory_no_follow(Path::new("/proc")).expect("proc root");
        let boundary = CatalogBoundary {
            mount_id: opened_mount_id(&catalog).expect("catalog mount"),
        };

        let error = validate_catalog_mount(&proc_root, boundary, Path::new("published"))
            .expect_err("nested mount must be rejected");

        assert!(error.to_string().contains("nested mount boundary"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn catalog_directory_mount_is_validated_before_readable_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = open_directory_no_follow(temp.path()).expect("catalog root");
        let root = open_directory_no_follow(Path::new("/")).expect("root directory");
        let pinned = PinnedDirectoryEntry::pin(&root, OsStr::new("proc"))
            .expect("pin nested-mount directory");
        let boundary = CatalogBoundary {
            mount_id: opened_mount_id(&catalog).expect("catalog mount"),
        };
        let readable_opened = std::cell::Cell::new(false);

        let error = with_validated_catalog_directory_mount(
            pinned,
            boundary,
            Path::new("published"),
            |_| -> Result<()> {
                readable_opened.set(true);
                Ok(())
            },
        )
        .expect_err("nested mount must fail before readable directory open");

        assert!(!readable_opened.get());
        assert!(error.to_string().contains("nested mount boundary"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readable_catalog_directory_mount_is_rechecked_before_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = open_directory_no_follow(temp.path()).expect("catalog root");
        let readable = ReadableDirectoryEntry {
            descriptor: open_directory_no_follow(Path::new("/proc")).expect("proc root"),
            device: 0,
            inode: 0,
            name: OsString::from("published"),
        };
        let boundary = CatalogBoundary {
            mount_id: opened_mount_id(&catalog).expect("catalog mount"),
        };
        let identity_checked = std::cell::Cell::new(false);

        let error = with_revalidated_catalog_directory_mount(
            readable,
            boundary,
            Path::new("published"),
            |_| -> Result<()> {
                identity_checked.set(true);
                Ok(())
            },
        )
        .expect_err("readable nested mount must fail before identity check");

        assert!(!identity_checked.get());
        assert!(error.to_string().contains("nested mount boundary"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn catalog_mount_is_validated_before_readable_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = open_directory_no_follow(temp.path()).expect("catalog root");
        let proc_root = open_directory_no_follow(Path::new("/proc")).expect("proc root");
        let pinned = PinnedRegularEntry::pin(&proc_root, OsStr::new("cpuinfo"))
            .expect("pin nested-mount artifact");
        let boundary = CatalogBoundary {
            mount_id: opened_mount_id(&catalog).expect("catalog mount"),
        };
        let readable_opened = std::cell::Cell::new(false);

        let error = with_validated_catalog_mount(
            pinned,
            boundary,
            Path::new("cpuinfo"),
            |_| -> Result<()> {
                readable_opened.set(true);
                Ok(())
            },
        )
        .expect_err("nested mount must fail before readable open");

        assert!(!readable_opened.get());
        assert!(error.to_string().contains("nested mount boundary"));
    }

    #[test]
    fn startup_removes_owned_staging_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");
        let root = temp.path().join("catalog");
        create_catalog_root(&root).expect("root");
        let staging = root.join(".import-pending-uuid.tmp");
        create_private_directory(&staging).expect("staging");

        TemplateCatalog::open(&test_config(&root, &import_root)).expect("catalog");

        assert!(!staging.exists());
    }

    #[test]
    fn second_catalog_owner_cannot_clean_live_staging() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");
        let root = temp.path().join("catalog");
        let config = test_config(&root, &import_root);
        let first = TemplateCatalog::open(&config).expect("first catalog owner");
        let staging = root.join(".import-live-uuid.tmp");
        create_private_directory(&staging).expect("live staging");

        let error = match TemplateCatalog::open(&config) {
            Ok(_) => panic!("second catalog owner must be rejected"),
            Err(error) => error,
        };

        assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        assert!(staging.exists(), "second owner must not clean live staging");

        drop(first);
        TemplateCatalog::open(&config).expect("catalog lock released with owner");
        assert!(!staging.exists(), "next owner cleans interrupted staging");
    }

    #[test]
    fn startup_bounds_catalog_staging_enumeration() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");
        let root = temp.path().join("catalog");
        create_catalog_root(&root).expect("root");
        create_private_directory(&root.join(".import-first-uuid.tmp")).expect("first staging");
        create_private_directory(&root.join(".import-second-uuid.tmp")).expect("second staging");
        let mut config = test_config(&root, &import_root);
        config.max_entries = 1;

        let error = match TemplateCatalog::open(&config) {
            Ok(_) => panic!("catalog enumeration must honor the entry limit"),
            Err(error) => error,
        };

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(std::fs::read_dir(&root).expect("catalog").count(), 2);
    }

    #[test]
    fn startup_bounds_staging_artifact_enumeration() {
        let temp = tempfile::tempdir().expect("tempdir");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");
        let root = temp.path().join("catalog");
        create_catalog_root(&root).expect("root");
        let staging = root.join(".import-pending-uuid.tmp");
        create_private_directory(&staging).expect("staging");
        let config = test_config(&root, &import_root);
        for index in 0..=config.max_files {
            std::fs::write(staging.join(format!("artifact-{index}")), b"data")
                .expect("staging artifact");
        }

        let error = match TemplateCatalog::open(&config) {
            Ok(_) => panic!("staging enumeration must honor the file limit"),
            Err(error) => error,
        };

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            std::fs::read_dir(&staging).expect("staging").count(),
            config.max_files + 1
        );
    }

    fn publish_cancelled_import(catalog: &TemplateCatalog, name: &str) -> BlazeDaemonError {
        let import_root = catalog.inner.import_root.as_ref().expect("import root");
        let source = open_import_source(import_root, Path::new("source")).expect("source");
        let prepare_cancellation = CancellationToken::new();
        let prepared = prepare_import(
            &source,
            name,
            "",
            catalog.inner.limits,
            &prepare_cancellation,
        )
        .expect("prepared import");
        let mut claim = ImportClaim::begin(Arc::clone(&catalog.inner), name.into()).expect("claim");
        claim.reserve(prepared.reserved_bytes).expect("reservation");
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        publish_prepared(
            &catalog.inner.root,
            name,
            prepared,
            &cancellation,
            &mut claim,
        )
        .expect_err("cancelled publication")
    }

    fn write_artifacts(source: &Path) {
        std::fs::create_dir_all(source).expect("source directory");
        std::fs::write(source.join("vmstate.snap"), b"snapshot").expect("snapshot");
        std::fs::write(source.join("mem.bin"), b"memory").expect("memory");
        std::fs::write(source.join("rootfs.ext4"), b"rootfs").expect("rootfs");
    }
}
