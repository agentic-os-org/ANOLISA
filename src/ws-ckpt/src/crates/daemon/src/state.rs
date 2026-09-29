use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::{Mutex, Notify, OnceCell, RwLock, Semaphore};
use tracing::{info, warn};

use ws_ckpt_common::backend::BackendType;
use ws_ckpt_common::backend::StorageBackend;
use ws_ckpt_common::persist::{
    self, BackendIdentity, BackendPaths, DaemonStateFile, WorkspaceEntry, DAEMON_STATE_VERSION,
};
use ws_ckpt_common::{
    load_workspace_policy, load_workspace_policy_with_failsafe, DaemonConfig, ErrorCode,
    LoadPolicyOutcome, Response, SnapshotIndex, WorkspaceInfo, WorkspacePolicy, INDEXES_DIR,
    INDEX_FILE,
};

use crate::fs_watcher::WorkspaceWatcher;
use crate::index_store;

/// Resolve parent aliases without following the final workspace symlink.
///
/// Missing anchors and parent directories remain addressable after detachment.
/// Existing ancestors are resolved before appending missing normal components;
/// unresolved `..` and broken parent symlinks are rejected as ambiguous.
pub(crate) fn normalize_registration_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("workspace registration path is empty");
    }
    match path.file_name() {
        Some(name) => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Ok(normalize_registration_parent(parent)
                .with_context(|| format!("normalize workspace parent {parent:?}"))?
                .join(name))
        }
        None => std::fs::canonicalize(path)
            .with_context(|| format!("normalize workspace registration path {path:?}")),
    }
}

fn normalize_registration_parent(path: &Path) -> std::io::Result<PathBuf> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                for component in missing.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) => {
                // A broken symlink is not a missing directory: discarding it
                // would invent an anchor unrelated to the filesystem target.
                if error.kind() != std::io::ErrorKind::NotFound
                    || !matches!(std::fs::symlink_metadata(ancestor), Err(ref err)
                        if err.kind() == std::io::ErrorKind::NotFound)
                {
                    return Err(error);
                }
                let Some(name) = ancestor.file_name() else {
                    // In particular, never cancel '..' across a missing node.
                    return Err(error);
                };
                missing.push(name);
                ancestor = ancestor
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
            }
        }
    }
}

#[derive(Default)]
struct WorkspaceRegistry {
    workspaces: HashMap<String, Arc<RwLock<WorkspaceState>>>,
    path_to_wsid: HashMap<PathBuf, String>,
}

pub struct DaemonState {
    // Never hold this lock across an await or a workspace-state lock: lifecycle
    // publication and both lookup directions must be one atomic operation.
    registry: std::sync::RwLock<WorkspaceRegistry>,
    /// Daemon configuration (std RwLock for runtime-reloadable config)
    pub config: std::sync::RwLock<DaemonConfig>,
    /// Broadcast signal: dispatcher calls `notify_waiters()` after a successful
    /// `ReloadConfig`, and background loops use `notified().await` inside a
    /// `tokio::select!` to (a) break out of a running `sleep` and re-read the
    /// config, or (b) wake up from a disabled state (`auto_cleanup = false`
    /// / `interval_secs == 0`). This replaces the old polling-based design
    /// where loops periodically woke up to check for config changes.
    pub config_notify: Notify,
    /// Mount path for btrfs filesystem (convenience accessor, immutable)
    pub mount_path: PathBuf,
    /// Socket path (convenience accessor, immutable)
    pub socket_path: PathBuf,
    /// Storage backend (trait object for multi-backend support)
    pub backend: Arc<dyn StorageBackend>,
    /// Daemon start time for uptime calculation
    pub start_time: std::time::Instant,
    /// Lazy bootstrap guard for BtrfsLoop backend (runs at most once)
    bootstrapped: OnceCell<()>,
    /// File watchers for write-lock detection (ws_id -> watcher)
    watchers: std::sync::Mutex<HashMap<String, WorkspaceWatcher>>,
    /// State persistence directory path
    pub state_dir: PathBuf,
    /// Backend selection method: "auto-detect" | "config" | "persisted"
    selection_method: String,
    /// Per-ws-id mutation mutex serializing lifecycle and snapshot operations
    /// that race on the backend, `index_dir(ws_id)`, or `workspaces` slot.
    /// Distinct from `WorkspaceState::policy_io_mu` (which lives inside an Arc
    /// that recover may unregister). Held across `await`.
    wsid_locks: DashMap<String, Arc<Mutex<()>>>,
    /// Serializes init and recovery before a workspace ID can be resolved.
    pub(crate) init_lock: Mutex<()>,
    /// Serializes manifest snapshots and writes across different workspaces.
    manifest_save_lock: Mutex<()>,
}

pub struct WorkspaceState {
    pub ws_id: String,
    pub path: PathBuf,
    pub index: SnapshotIndex,
    /// Per-workspace policy override; `WorkspacePolicy::default()` when no
    /// `policy.toml` exists (i.e. inherit everything from global).
    pub policy: WorkspacePolicy,
    /// True iff `policy` is a synthetic fail-safe value injected at register
    /// time because `policy.toml` was unreadable. Blocks PATCH (which would
    /// silently persist the synthetic value as truth) until reload or reset.
    pub policy_failsafe: bool,
    /// Narrow per-ws serializer for PATCH/RESET on `policy.toml`. Decoupled
    /// from the ws RwLock so checkpoint/list/status are NOT blocked by a slow
    /// fsync; only competing PATCH/RESET wait. Held across spawn_blocking,
    /// hence `tokio::sync::Mutex`. The ws write lock itself is taken only to
    /// commit the in-memory result, never across the disk op.
    pub policy_io_mu: Arc<Mutex<()>>,
}

impl DaemonState {
    pub fn new(config: DaemonConfig, backend: Arc<dyn StorageBackend>, state_dir: PathBuf) -> Self {
        let mount_path = config.mount_path.clone();
        let socket_path = config.socket_path.clone();
        let selection_method = "auto-detect".to_string();
        Self {
            registry: std::sync::RwLock::new(WorkspaceRegistry::default()),
            config: std::sync::RwLock::new(config),
            config_notify: Notify::new(),
            mount_path,
            socket_path,
            backend,
            start_time: std::time::Instant::now(),
            bootstrapped: OnceCell::new(),
            watchers: std::sync::Mutex::new(HashMap::new()),
            state_dir,
            selection_method,
            wsid_locks: DashMap::new(),
            init_lock: Mutex::new(()),
            manifest_save_lock: Mutex::new(()),
        }
    }

    /// Acquires the per-workspace mutation mutex.
    ///
    /// When both locks are needed, acquire this mutex before the workspace
    /// `RwLock`. Entries are retained because removal could split waiters for
    /// the same ID across different mutex instances.
    pub async fn lock_wsid(&self, ws_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let mtx = self
            .wsid_locks
            .entry(ws_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        mtx.lock_owned().await
    }

    /// Acquires the mutation mutex if this workspace instance remains registered.
    pub(crate) async fn lock_workspace_mutation_if_current(
        &self,
        workspace: &Arc<RwLock<WorkspaceState>>,
    ) -> Option<(String, tokio::sync::OwnedMutexGuard<()>)> {
        let ws_id = workspace.read().await.ws_id.clone();
        let guard = self.lock_wsid(&ws_id).await;
        self.workspace_arc_is_current(&ws_id, workspace)
            .then_some((ws_id, guard))
    }

    /// get the index storage directory for a workspace
    pub fn index_dir(&self, ws_id: &str) -> PathBuf {
        self.state_dir.join(INDEXES_DIR).join(ws_id)
    }

    /// Poison-safe snapshot of the current daemon config.
    ///
    /// A panic under the write lock poisons it, making `.read().unwrap()`
    /// re-panic and take down every config reader (policy IPCs, scheduler).
    /// We consume the poison and return the last-written value, which is
    /// what read-only consumers want. Prefer this over `.read().unwrap()`.
    pub fn config_snapshot(&self) -> DaemonConfig {
        let guard = match self.config.read() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!(
                    "config RwLock was poisoned by a panicking writer; reading anyway \
                     to keep policy IPCs and the scheduler alive"
                );
                poisoned.into_inner()
            }
        };
        guard.clone()
    }

    /// Rebuild runtime state from persisted file
    pub async fn rebuild_from_persisted(
        state_file: &DaemonStateFile,
        config: DaemonConfig,
        backend: Arc<dyn StorageBackend>,
        state_dir: PathBuf,
        selection_method: &str,
    ) -> anyhow::Result<Self> {
        let mut state = Self::new(config, backend, state_dir);
        state.selection_method = selection_method.to_string();

        for entry in &state_file.workspaces {
            let ws_id = &entry.ws_id;
            let workspace_path = normalize_registration_path(&entry.workspace_path).with_context(|| {
                format!(
                    "restore workspace {ws_id:?} at {:?}: restore its parent directory before restarting",
                    entry.workspace_path
                )
            })?;
            let index_dir = state.index_dir(ws_id);
            let index_path = index_dir.join(INDEX_FILE);

            let recovered = Self::restore_interrupted_unregister(&index_dir).await?;
            let index = if let Some((index, _)) = &recovered {
                index.clone()
            } else {
                match tokio::fs::read_to_string(&index_path).await {
                    Ok(content) => serde_json::from_str::<SnapshotIndex>(&content)
                        .with_context(|| format!("parse snapshot index {index_path:?}"))?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        SnapshotIndex::new(workspace_path.clone())
                    }
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("read snapshot index {index_path:?}"))
                    }
                }
            };

            info!(
                "Restoring workspace from persisted state: {} -> {:?}",
                ws_id, workspace_path
            );

            // Shared helper: Missing → inherit-global; Err → fail-safe
            // (auto_cleanup=false + policy_failsafe=true). See [[ws-failsafe]].
            let (policy, failsafe) = match recovered {
                Some((_, policy)) => (policy, false),
                None => load_workspace_policy_with_failsafe(&index_dir, ws_id, "rebuild"),
            };
            state
                .register_workspace_with_policy(
                    ws_id.clone(),
                    workspace_path.clone(),
                    index,
                    policy,
                    failsafe,
                )
                .with_context(|| format!("restore workspace registration {ws_id:?}"))?;
            // Start file watcher
            match WorkspaceWatcher::start(&workspace_path) {
                Ok(watcher) => {
                    state.register_watcher(ws_id.clone(), watcher);
                }
                Err(e) => {
                    warn!(
                        "Failed to start file watcher for workspace {}: {}",
                        ws_id, e
                    );
                }
            }
        }

        // Reconcile both sides of a crash between backend mutation and index save.
        for ws_id in state.workspace_ids() {
            if let Some(ws_arc) = state.get_by_wsid(&ws_id) {
                let mut ws = ws_arc.write().await;
                let snapshot_dir = state.backend.snapshots_root().join(&ws_id);
                if index_store::reconcile_from_fs(&snapshot_dir, &mut ws.index).await? {
                    index_store::save(&state.index_dir(&ws_id), &ws.index)
                        .await
                        .with_context(|| format!("save reconciled index for {ws_id}"))?;
                }
            }
        }

        Ok(state)
    }

    // The manifest is the unregister commit point. An archive for a still-listed
    // workspace belongs to an interrupted unregister and must remain registered.
    async fn restore_interrupted_unregister(
        index_dir: &Path,
    ) -> anyhow::Result<Option<(SnapshotIndex, WorkspacePolicy)>> {
        match tokio::fs::symlink_metadata(index_dir).await {
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context(format!("inspect index directory {index_dir:?}")),
        }
        let ws_id = index_dir
            .file_name()
            .context("index directory has no workspace ID")?;
        let archive = index_dir.with_file_name(format!("{}.unregistered", ws_id.to_string_lossy()));
        match tokio::fs::symlink_metadata(&archive).await {
            Ok(meta) => anyhow::ensure!(
                meta.is_dir(),
                "index archive is not a directory: {archive:?}"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context(format!("inspect index archive {archive:?}")),
        }

        // Validate before renaming so a corrupt archive also fails visibly on
        // the next restart, instead of falling back to an empty active index.
        let content = tokio::fs::read_to_string(archive.join(INDEX_FILE))
            .await
            .with_context(|| format!("read interrupted unregister index in {archive:?}"))?;
        let index = serde_json::from_str(&content)
            .with_context(|| format!("parse interrupted unregister index in {archive:?}"))?;
        let policy = match load_workspace_policy(&archive)
            .with_context(|| format!("load interrupted unregister policy in {archive:?}"))?
        {
            LoadPolicyOutcome::Missing => WorkspacePolicy::default(),
            LoadPolicyOutcome::Loaded(policy) => policy,
        };
        tokio::fs::rename(&archive, index_dir)
            .await
            .with_context(|| {
                format!("restore interrupted unregister archive {archive:?} to {index_dir:?}")
            })?;
        index_store::sync_parent(index_dir).await?;
        warn!(
            "Restored index archive after interrupted unregister: {:?}",
            index_dir
        );
        Ok(Some((index, policy)))
    }

    /// Save current runtime state to state.json (atomic write+rename+fsync)
    pub async fn save_manifest(&self) -> anyhow::Result<()> {
        let _save_guard = self.manifest_save_lock.lock().await;
        self.write_manifest(None).await
    }

    /// Persist removal before changing runtime registration or stopping its watcher.
    /// The caller must hold the workspace mutation lock through this operation.
    pub(crate) async fn persist_unregister_workspace(&self, ws_id: &str) -> anyhow::Result<()> {
        let _save_guard = self.manifest_save_lock.lock().await;
        self.write_manifest(Some(ws_id)).await?;
        self.unregister_workspace(ws_id).await;
        Ok(())
    }

    async fn write_manifest(&self, omitted_ws_id: Option<&str>) -> anyhow::Result<()> {
        let backend_type = self.backend.backend_type();
        let backend = BackendIdentity {
            backend_type,
            selection_method: self.selection_method.clone(),
            selected_at: Utc::now(),
        };
        let paths = match backend_type {
            BackendType::BtrfsLoop => BackendPaths::BtrfsLoop {
                mount_path: self.backend.data_root().to_path_buf(),
                data_root: self.backend.data_root().to_path_buf(),
                snapshots_root: self.backend.snapshots_root().to_path_buf(),
                loop_img: self.backend.loop_img_state().await,
            },
            BackendType::BtrfsBase => BackendPaths::BtrfsBase {
                mount_path: self.backend.data_root().to_path_buf(),
                data_root: self.backend.data_root().to_path_buf(),
                snapshots_root: self.backend.snapshots_root().to_path_buf(),
            },
        };
        let state_file = DaemonStateFile::new(
            DAEMON_STATE_VERSION,
            backend,
            paths,
            self.collect_workspace_entries()
                .into_iter()
                .filter(|entry| Some(entry.ws_id.as_str()) != omitted_ws_id)
                .collect(),
        );

        // Perform sync IO in a blocking thread
        let state_dir = self.state_dir.clone();
        tokio::task::spawn_blocking(move || persist::save_state(&state_dir, &state_file))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))??;

        Ok(())
    }

    /// Snapshot registry identities without taking workspace-state locks.
    fn collect_workspace_entries(&self) -> Vec<WorkspaceEntry> {
        let registry = self.registry.read().unwrap_or_else(|err| err.into_inner());
        registry
            .path_to_wsid
            .iter()
            .map(|(path, ws_id)| WorkspaceEntry {
                ws_id: ws_id.clone(),
                workspace_path: path.clone(),
                registered_at: Utc::now(),
                origin_backend: self.backend.backend_type(),
            })
            .collect()
    }

    fn workspace_ids(&self) -> Vec<String> {
        self.registry
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .workspaces
            .keys()
            .cloned()
            .collect()
    }

    /// Idempotently call the backend's bootstrap hook (runs at most once).
    pub async fn ensure_bootstrapped(&self) -> anyhow::Result<()> {
        self.bootstrapped
            .get_or_try_init(|| async {
                let config = self.config_snapshot();
                self.backend.bootstrap(&config).await
            })
            .await?;
        Ok(())
    }

    /// Seed the OnceCell after startup already called bootstrap directly.
    pub fn mark_bootstrapped(&self) {
        if self.bootstrapped.set(()).is_err() {
            warn!("mark_bootstrapped called but OnceCell already set; likely duplicate call");
        }
    }

    pub fn get_by_wsid(&self, ws_id: &str) -> Option<Arc<RwLock<WorkspaceState>>> {
        self.registry
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .workspaces
            .get(ws_id)
            .cloned()
    }

    pub fn get_by_path(&self, path: &Path) -> Option<Arc<RwLock<WorkspaceState>>> {
        {
            let registry = self.registry.read().unwrap_or_else(|err| err.into_inner());
            if let Some(ws_id) = registry.path_to_wsid.get(path) {
                return registry.workspaces.get(ws_id).cloned();
            }
        }
        let path = normalize_registration_path(path).ok()?;
        let registry = self.registry.read().unwrap_or_else(|err| err.into_inner());
        let ws_id = registry.path_to_wsid.get(&path)?;
        registry.workspaces.get(ws_id).cloned()
    }

    /// Returns the workspace ID for an exact normalized registration anchor.
    /// Guarded identity discovery deliberately does not resolve caller aliases.
    pub(crate) fn wsid_for_exact_registration_path(&self, path: &Path) -> Option<String> {
        self.registry
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .path_to_wsid
            .get(path)
            .cloned()
    }

    /// Confirms that the workspace ID still maps to this exact allocation.
    pub(crate) fn workspace_arc_is_current(
        &self,
        ws_id: &str,
        workspace: &Arc<RwLock<WorkspaceState>>,
    ) -> bool {
        self.registry
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .workspaces
            .get(ws_id)
            .is_some_and(|entry| Arc::ptr_eq(entry, workspace))
    }

    /// Confirms that both registration indexes still name the same workspace.
    pub(crate) fn exact_registration_is_current(
        &self,
        path: &Path,
        ws_id: &str,
        workspace: &Arc<RwLock<WorkspaceState>>,
    ) -> bool {
        let registry = self.registry.read().unwrap_or_else(|err| err.into_inner());
        registry
            .path_to_wsid
            .get(path)
            .is_some_and(|id| id == ws_id)
            && registry
                .workspaces
                .get(ws_id)
                .is_some_and(|entry| Arc::ptr_eq(entry, workspace))
    }

    /// Resolve a workspace by identifier: tries workspace ID first, then filesystem path.
    /// Supports absolute paths, relative paths, and workspace IDs (e.g., "ws-6d5aaa").
    pub async fn resolve_workspace(&self, workspace: &str) -> Option<Arc<RwLock<WorkspaceState>>> {
        if workspace.trim().is_empty() {
            return None;
        }
        // Normalize: strip trailing slashes so "/a/b/" and "/a/b" are equivalent.
        let workspace = {
            let t = workspace.trim_end_matches('/');
            if t.is_empty() {
                "/"
            } else {
                t
            }
        };
        // 1. Try as workspace ID
        if let Some(arc) = self.get_by_wsid(workspace) {
            return Some(arc);
        }
        // Exact registrations must win even if a managed link was retargeted;
        // mutation callers must check its own detached-registration guard.
        let path = Path::new(workspace);
        if let Some(arc) = self.get_by_path(path) {
            return Some(arc);
        }
        if let Ok(abs_path) = tokio::fs::canonicalize(workspace).await {
            if let Some(arc) = self.get_by_path(&abs_path) {
                return Some(arc);
            }
            // Aliases follow the final managed symlink to data_root/ws_id.
            // Nested directories and unregistered storage are not workspaces.
            if let Ok(data_root) = tokio::fs::canonicalize(self.backend.data_root()).await {
                if let Ok(rest) = abs_path.strip_prefix(data_root) {
                    if rest.components().count() == 1 {
                        if let Some(ws_id) = rest.to_str() {
                            return self.get_by_wsid(ws_id);
                        }
                    }
                }
            }
        }
        None
    }

    /// True iff `registration_path` still resolves to the workspace's live
    /// subvolume (`data_root/ws_id`).
    ///
    /// Canonicalize-equality rather than `read_link`-equality: registration
    /// paths may reach the subvolume through symlink chains, relative
    /// symlinks, or directly through the backend mount (init's bind-mount
    /// adoption case), and only canonicalization follows every hop. Any
    /// resolution failure (path missing, broken symlink) counts as detached.
    pub(crate) async fn registration_is_live(&self, ws_id: &str, registration_path: &Path) -> bool {
        let live_path = self.backend.data_root().join(ws_id);
        match tokio::try_join!(
            tokio::fs::canonicalize(registration_path),
            tokio::fs::canonicalize(live_path)
        ) {
            Ok((registration_target, live_target)) => registration_target == live_target,
            Err(_) => false,
        }
    }

    /// V1 detached-registration guard: `Some(Error)` when the registered path
    /// no longer resolves to the workspace's live subvolume, `None` when the
    /// registration is healthy and the caller may proceed.
    ///
    /// When a workspace symlink is deleted and the path recreated as a plain
    /// directory, V1 path-addressed ops would otherwise resolve the stale
    /// registry binding and silently operate on the old subvolume while the
    /// user reads/writes the replacement directory — snapshots containing
    /// none of the user's data, "successful" rollbacks the user never sees.
    /// Guarded (V2) requests enforce the same liveness contract via
    /// [`Self::registration_is_live`]. `recover_workspace` must not use this
    /// guard: it is the repair path this error points the operator at.
    pub(crate) async fn detached_registration_error(
        &self,
        workspace: &Arc<RwLock<WorkspaceState>>,
    ) -> Option<Response> {
        // The read guard must span the liveness probe: rollback swaps the live
        // subvolume (rename to `<ws_id>.rollback-tmp`, then create the
        // replacement) while holding the workspace write lock, so probing
        // unlocked can observe the transient gap and refuse a healthy
        // workspace. Mutation-capable callers already hold `lock_wsid` but
        // invoke this guard before taking the workspace write lock, preserving
        // the canonical lock order. V2 guarded checkpoint enforces the same
        // contract by probing under its workspace lock.
        let ws = workspace.read().await;
        let ws_id = ws.ws_id.clone();
        let registration_path = ws.path.clone();
        if self.registration_is_live(&ws_id, &registration_path).await {
            return None;
        }
        drop(ws);
        warn!(
            "workspace {} registration detached at {:?}; refusing operation — \
             run 'ws-ckpt recover -w {:?}' to restore",
            ws_id, registration_path, registration_path
        );
        // Distinguish the detach states so the operator knows whether
        // recover is safe or would clobber a replacement directory.
        let (cause, hint) = match tokio::fs::symlink_metadata(&registration_path).await {
            Err(_) => ("the registered path no longer exists", ""),
            Ok(meta) if !meta.file_type().is_symlink() => (
                "the registered path is no longer a workspace symlink",
                "\n  note: path is currently a regular directory — \
                 move or rename it before running recover to avoid data loss",
            ),
            Ok(_) => (
                "the registered symlink no longer points at the live workspace subvolume",
                "",
            ),
        };
        Some(Response::Error {
            code: ErrorCode::InternalError,
            message: format!(
                "workspace registered (ws_id={}) but {}; \
                 run 'ws-ckpt recover -w {}' to restore, then re-init{}",
                ws_id,
                cause,
                registration_path.display(),
                hint
            ),
        })
    }

    /// Distinguish user registration anchors from backend-owned storage paths.
    pub(crate) fn registration_path_is_internal(&self, path: &Path) -> anyhow::Result<bool> {
        let path = normalize_registration_path(path)?;
        let root = self.backend.data_root();
        let data_root = match std::fs::canonicalize(root) {
            Ok(root) => root,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && matches!(std::fs::symlink_metadata(root), Err(ref err)
                    if err.kind() == std::io::ErrorKind::NotFound) =>
            {
                normalize_registration_path(root)?
            }
            Err(error) => {
                return Err(error).with_context(|| format!("resolve backend data root {root:?}"))
            }
        };
        Ok(path.starts_with(data_root) || path == normalize_registration_path(root)?)
    }

    /// Atomically register a unique workspace identity and normalized anchor.
    pub fn register_workspace(
        &self,
        ws_id: String,
        path: PathBuf,
        index: SnapshotIndex,
    ) -> anyhow::Result<()> {
        self.register_workspace_with_policy(ws_id, path, index, WorkspacePolicy::default(), false)
    }

    /// Reject duplicate IDs or anchors without modifying either registry index.
    pub fn register_workspace_with_policy(
        &self,
        ws_id: String,
        path: PathBuf,
        mut index: SnapshotIndex,
        policy: WorkspacePolicy,
        failsafe: bool,
    ) -> anyhow::Result<()> {
        let path = normalize_registration_path(&path)?;
        if path == Path::new("/") || self.registration_path_is_internal(&path)? {
            anyhow::bail!("workspace registration anchor {path:?} must be a user path outside backend storage");
        }
        index.workspace_path = path.clone();
        let state = Arc::new(RwLock::new(WorkspaceState {
            ws_id: ws_id.clone(),
            path: path.clone(),
            index,
            policy,
            policy_failsafe: failsafe,
            policy_io_mu: Arc::new(Mutex::new(())),
        }));
        {
            let mut registry = self.registry.write().unwrap_or_else(|err| err.into_inner());
            if registry.workspaces.contains_key(&ws_id) {
                anyhow::bail!("workspace ID {ws_id:?} is already registered");
            }
            if let Some(existing) = registry.path_to_wsid.get(&path) {
                anyhow::bail!("workspace path {path:?} is already registered as {existing:?}");
            }
            registry.workspaces.insert(ws_id.clone(), state);
            registry.path_to_wsid.insert(path, ws_id);
        }
        self.config_notify.notify_waiters();
        Ok(())
    }

    /// Reload every `policy.toml` from disk into the in-memory state.
    /// Called by `handle_reload_config` so out-of-band edits are picked up
    /// by `ws-ckpt reload` / `systemctl reload`.
    ///
    /// Three invariants, each fixing a real bug:
    /// 1. **fs-serialize via `policy_io_mu`, not the ws RwLock**: a per-ws
    ///    narrow mutex (the same one PATCH/RESET take around save+commit) is
    ///    held across the disk read; the ws RwLock is taken only for the
    ///    few-microsecond memory commit. This still excludes any concurrent
    ///    SET stomp, but lets checkpoint/list/status keep running while the
    ///    blocking pool is busy. See [[ws-lock-no-fs-loops]].
    /// 2. **Blocking I/O on a worker**: `load_workspace_policy` is sync
    ///    `std::fs`; run it on a blocking thread so N reads don't starve
    ///    concurrent CLI requests.
    /// 3. **Strict error handling**: only `Missing` collapses to default;
    ///    a transient I/O / parse error must NOT erase live policy — we
    ///    keep it and `warn!`.
    ///
    /// Per-ws tasks run concurrently (capped by semaphore) so a 500-ws fleet
    /// doesn't serialize into a multi-second reload; invariant #1 still holds
    /// per task because `policy_io_mu` is per-ws, not shared.
    pub async fn reload_all_workspace_policies(&self) {
        // Cap to avoid flooding the blocking pool during reload.
        const RELOAD_CONCURRENCY: usize = 32;
        let sem = Arc::new(Semaphore::new(RELOAD_CONCURRENCY));

        let ws_ids = self.workspace_ids();
        let mut set = tokio::task::JoinSet::new();
        for ws_id in ws_ids {
            let arc = match self.get_by_wsid(&ws_id) {
                Some(a) => a,
                None => continue, // unregistered between snapshot and now
            };
            let dir = self.index_dir(&ws_id);
            let sem = Arc::clone(&sem);
            set.spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore not closed");
                Self::reload_one_workspace_policy_inner(&ws_id, dir, &arc).await;
            });
        }
        // Drain so a panicking task can't abort the reload.
        while let Some(res) = set.join_next().await {
            if let Err(e) = res {
                warn!("reload: per-ws task panicked or was cancelled: {}", e);
            }
        }
    }

    /// Reload a single workspace's `policy.toml`. Returns `false` if the ws is
    /// no longer registered. Same write-lock-first invariant as the bulk path.
    pub async fn reload_workspace_policy(&self, ws_id: &str) -> bool {
        let Some(arc) = self.get_by_wsid(ws_id) else {
            return false;
        };
        let dir = self.index_dir(ws_id);
        Self::reload_one_workspace_policy_inner(ws_id, dir, &arc).await;
        true
    }

    async fn reload_one_workspace_policy_inner(
        ws_id: &str,
        dir: PathBuf,
        arc: &Arc<RwLock<WorkspaceState>>,
    ) {
        // (1) Tiny read lock: grab the per-ws fs serializer.
        let policy_io_mu = {
            let ws = arc.read().await;
            ws.policy_io_mu.clone()
        };

        // (2) Serialize against concurrent PATCH/RESET via the narrow per-ws
        //     mutex (same one PATCH/RESET hold around save+commit). This gives
        //     us the original "no SET stomp" guarantee without blocking
        //     checkpoint/list/status on a slow disk via the ws RwLock.
        let _io_guard = policy_io_mu.lock().await;

        // (3) Disk read on a blocking worker, with NO ws RwLock held.
        let outcome = tokio::task::spawn_blocking(move || load_workspace_policy(&dir)).await;

        // (4) Tiny write lock to commit the in-memory result. Disk is the
        //     source of truth; this just brings memory in sync. PATCH/RESET
        //     can't have raced here because they're behind `policy_io_mu`.
        match outcome {
            // Successful read: real on-disk truth, drop any fail-safe marker.
            Ok(Ok(LoadPolicyOutcome::Missing)) => {
                let mut ws = arc.write().await;
                ws.policy = WorkspacePolicy::default();
                ws.policy_failsafe = false;
            }
            Ok(Ok(LoadPolicyOutcome::Loaded(p))) => {
                let mut ws = arc.write().await;
                ws.policy = p;
                ws.policy_failsafe = false;
            }
            Ok(Err(e)) => warn!(
                "reload: failed to load policy for {}: {}; preserving live in-memory policy",
                ws_id, e
            ),
            Err(join_err) => warn!(
                "reload: spawn_blocking joined with error for {}: {}; preserving live policy",
                ws_id, join_err
            ),
        }
    }

    /// True iff at least one workspace's effective policy (local-or-global)
    /// would do work this tick. Lets `auto_cleanup_loop` decide whether to
    /// park. Subsumes the legacy global-only short-circuits by checking the
    /// merged policy of every ws, so a per-ws override that re-enables
    /// cleanup wins over a globally-off default.
    pub async fn any_ws_has_effective_cleanup(&self) -> bool {
        let cfg_snapshot = self.config_snapshot();
        for arc in self.all_workspaces() {
            let ws = arc.read().await;
            if !ws.policy.effective_for(&cfg_snapshot).is_disabled() {
                return true;
            }
        }
        false
    }

    pub async fn unregister_workspace(&self, ws_id: &str) {
        // Stop watcher if present
        if let Ok(mut watchers) = self.watchers.lock() {
            if let Some(w) = watchers.remove(ws_id) {
                w.stop();
            }
        }
        {
            let mut registry = self.registry.write().unwrap_or_else(|err| err.into_inner());
            registry.workspaces.remove(ws_id);
            registry
                .path_to_wsid
                .retain(|_, registered_id| registered_id != ws_id);
        }
        // Symmetric with register: re-park/re-evaluate the
        // scheduler when a ws goes away. No-op when no one is parked.
        self.config_notify.notify_waiters();
    }

    /// Register a file watcher for a workspace.
    pub fn register_watcher(&self, ws_id: String, watcher: WorkspaceWatcher) {
        if let Ok(mut watchers) = self.watchers.lock() {
            watchers.insert(ws_id, watcher);
        }
    }

    /// Check if a workspace is quiescent (no recent writes).
    /// Returns true if safe to snapshot, or if no watcher is registered.
    pub async fn check_workspace_quiescent(&self, ws_id: &str) -> bool {
        // Extract the AtomicBool from the watcher without holding the lock across await
        let is_writing_arc = {
            let watchers = match self.watchers.lock() {
                Ok(w) => w,
                Err(_) => return true,
            };
            match watchers.get(ws_id) {
                Some(w) => Some(std::sync::Arc::clone(&w.is_writing_flag())),
                None => None,
            }
        };
        match is_writing_arc {
            None => true,
            Some(flag) => {
                if !flag.load(std::sync::atomic::Ordering::Acquire) {
                    return true;
                }
                // Wait 100ms quiet period
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                flag.store(false, std::sync::atomic::Ordering::Release);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                !flag.load(std::sync::atomic::Ordering::Acquire)
            }
        }
    }

    pub async fn rebuild_from_disk(
        config: DaemonConfig,
        backend: Arc<dyn StorageBackend>,
        state_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        let state = Self::new(config.clone(), backend, state_dir);

        // Use backend's snapshots root (not config.mount_path) so BtrfsBase and
        // BtrfsLoop both point at the correct on-disk location.
        let snapshots_dir = state.backend.snapshots_root().to_path_buf();

        let mut read_dir = match tokio::fs::read_dir(&snapshots_dir).await {
            Ok(rd) => rd,
            Err(e) => {
                warn!(
                    "Could not read snapshots directory {:?}: {}",
                    snapshots_dir, e
                );
                return Ok(state);
            }
        };

        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            let file_type = match entry.file_type().await {
                Ok(ft) => ft,
                Err(e) => {
                    warn!("Error reading file type for {:?}: {}", path, e);
                    continue;
                }
            };
            if !file_type.is_dir() {
                continue;
            }

            if let Err(e) = Self::rebuild_single_workspace(&state, &path).await {
                warn!("Failed to rebuild workspace at {:?}: {}", path, e);
            }
        }

        Ok(state)
    }

    async fn rebuild_single_workspace(state: &Self, path: &Path) -> anyhow::Result<()> {
        let ws_id = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid path: missing file name"))?
            .to_string_lossy()
            .to_string();

        let index_path = path.join(INDEX_FILE);
        let index_content = match tokio::fs::read_to_string(&index_path).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read {:?}: {}", index_path, e);
                return Ok(());
            }
        };

        let index: SnapshotIndex = match serde_json::from_str(&index_content) {
            Ok(idx) => idx,
            Err(e) => {
                warn!("Failed to parse {:?}: {}", index_path, e);
                return Ok(());
            }
        };

        let workspace_path = normalize_registration_path(&index.workspace_path)?;

        // If loaded index has no snapshots, try rebuilding from filesystem
        let index = if index.snapshots.is_empty() {
            match index_store::rebuild_from_fs(path, workspace_path.clone()).await {
                Ok(mut rebuilt) if !rebuilt.snapshots.is_empty() => {
                    // Filesystem recovery reconstructs snapshot metadata only;
                    // retained guarded receipts must survive the rebuild.
                    rebuilt.governed_evidence = index.governed_evidence.clone();
                    rebuilt.guarded_rollbacks = index.guarded_rollbacks.clone();
                    info!(
                        "Rebuilt {} snapshot(s) from filesystem for {}",
                        rebuilt.snapshots.len(),
                        ws_id
                    );
                    // Persist rebuilt index
                    let _ = index_store::save(path, &rebuilt).await;
                    rebuilt
                }
                _ => index,
            }
        } else {
            index
        };

        info!("Restored workspace {} -> {:?}", ws_id, workspace_path);
        // Shared fail-safe helper; same `(policy, failsafe)` semantics as
        // every other register entry. See [[ws-failsafe]].
        let policy_dir = state.index_dir(&ws_id);
        let (policy, failsafe) =
            load_workspace_policy_with_failsafe(&policy_dir, &ws_id, "rebuild");
        state.register_workspace_with_policy(
            ws_id.clone(),
            workspace_path.clone(),
            index,
            policy,
            failsafe,
        )?;
        // Start file watcher for write-lock detection
        match WorkspaceWatcher::start(&workspace_path) {
            Ok(watcher) => {
                state.register_watcher(ws_id.clone(), watcher);
            }
            Err(e) => {
                warn!("Failed to start watcher for {}: {}", ws_id, e);
            }
        }

        Ok(())
    }

    pub fn all_workspaces(&self) -> Vec<Arc<RwLock<WorkspaceState>>> {
        self.registry
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .workspaces
            .values()
            .cloned()
            .collect()
    }

    /// Cross-workspace snapshot lookup by exact ID.
    /// Returns `(workspace_path, snapshot_id)` if exactly one match is found.
    pub async fn resolve_snapshot_globally(&self, snapshot_ref: &str) -> Option<(String, String)> {
        let mut found: Vec<(String, String)> = Vec::new();

        for workspace in self.all_workspaces() {
            let ws = workspace.read().await;
            if ws.index.snapshots.contains_key(snapshot_ref) {
                let ws_path = ws.path.to_string_lossy().to_string();
                found.push((ws_path, snapshot_ref.to_string()));
            }
        }

        if found.len() == 1 {
            Some(found.into_iter().next().unwrap())
        } else {
            None
        }
    }

    /// Collect summary information about all registered workspaces. Awaits the
    /// read lock so the result reflects real path/snapshot_count even when a
    /// ws is held under a write lock.
    pub async fn get_all_workspace_info(&self) -> Vec<WorkspaceInfo> {
        let mut out = Vec::new();
        for arc in self.all_workspaces() {
            let state = arc.read().await;
            out.push(WorkspaceInfo {
                ws_id: state.ws_id.clone(),
                path: state.path.to_string_lossy().to_string(),
                snapshot_count: state.index.snapshots.len() as u32,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ws_ckpt_common::{
        save_workspace_policy, CleanupRetention, DaemonConfig, GuardedCheckpointEvidenceV2,
        GuardedCheckpointOutcomeV2, SnapshotIndex, SnapshotMeta, WorkspaceGenerationTokenV2,
    };

    fn test_backend() -> Arc<dyn StorageBackend> {
        Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            PathBuf::from("/tmp/test-mount"),
            PathBuf::from("/tmp/test.img"),
        ))
    }

    fn test_config() -> DaemonConfig {
        DaemonConfig {
            mount_path: PathBuf::from("/tmp/test-mount"),
            socket_path: PathBuf::from("/tmp/test.sock"),
            log_level: "info".to_string(),
            auto_cleanup: false,
            auto_cleanup_keep: CleanupRetention::Count(20),
            auto_cleanup_interval_secs: 86_400,
            health_check_interval_secs: 300,
            backend_type: "auto".to_string(),
            img_size: 30,
            img_max_percent: 40.0,
            min_free_bytes: 512 * 1024 * 1024,
            min_free_percent: 1.0,
        }
    }

    fn test_state_dir() -> PathBuf {
        PathBuf::from("/tmp/test-state")
    }

    #[test]
    fn new_state_has_empty_workspaces() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        assert!(state.all_workspaces().is_empty());
    }

    #[tokio::test]
    async fn filesystem_rebuild_preserves_guarded_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_path = temp.path().join("workspace");
        std::fs::create_dir(&workspace_path).unwrap();
        let index_dir = temp.path().join("indexes").join("ws-abcdef");

        let mut index = SnapshotIndex::new(workspace_path.clone());
        index.governed_evidence.insert(
            "skipped-1".to_string(),
            GuardedCheckpointEvidenceV2 {
                ws_id: "ws-abcdef".to_string(),
                registered_path: workspace_path.to_string_lossy().into_owned(),
                generation: WorkspaceGenerationTokenV2::from_bytes([1; 32]),
                checkpoint_id: "skipped-1".to_string(),
                operation_digest: [2; 32],
                caller_uid: 1000,
                outcome: GuardedCheckpointOutcomeV2::Skipped {
                    reason: "empty".to_string(),
                },
            },
        );
        crate::index_store::save(&index_dir, &index).await.unwrap();
        std::fs::create_dir(index_dir.join("orphan-snapshot")).unwrap();

        let state = DaemonState::new(test_config(), test_backend(), temp.path().join("state"));
        DaemonState::rebuild_single_workspace(&state, &index_dir)
            .await
            .unwrap();

        let restored = state.get_by_wsid("ws-abcdef").expect("rebuilt workspace");
        let restored = restored.read().await;
        assert!(restored.index.snapshots.contains_key("orphan-snapshot"));
        assert!(restored.index.governed_evidence.contains_key("skipped-1"));
    }

    async fn unregister_restart_fixture(root: &Path) -> DaemonState {
        let backend = Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            root.join("mount"),
            root.join("test.img"),
        ));
        let state = DaemonState::new(test_config(), backend, root.join("state"));
        let path = root.join("missing-workspace");
        let mut index = SnapshotIndex::new(path.clone());
        index.snapshots.insert(
            "retained-snapshot".to_string(),
            SnapshotMeta {
                message: Some("keep this snapshot".to_string()),
                metadata: None,
                pinned: true,
                created_at: Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        tokio::fs::create_dir_all(
            state
                .backend
                .snapshots_root()
                .join("ws-restart/retained-snapshot"),
        )
        .await
        .unwrap();
        index_store::save(&state.index_dir("ws-restart"), &index)
            .await
            .unwrap();
        save_workspace_policy(
            &state.index_dir("ws-restart"),
            &WorkspacePolicy {
                auto_cleanup: Some(false),
                auto_cleanup_keep: Some(CleanupRetention::Count(7)),
            },
        )
        .unwrap();
        state
            .register_workspace("ws-restart".to_string(), path, index)
            .unwrap();
        state.save_manifest().await.unwrap();
        state
    }

    #[tokio::test]
    async fn restart_preserves_invalid_indexes_instead_of_adopting_protected_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let state = unregister_restart_fixture(temp.path()).await;
        let manifest = persist::load_state(&state.state_dir).unwrap().unwrap();
        let path = state.index_dir("ws-restart").join(INDEX_FILE);
        // Invalid JSON and an unreadable index must both stop reconciliation.
        for contents in [b"{broken".as_slice(), b"\xff".as_slice()] {
            std::fs::write(&path, contents).unwrap();
            assert!(DaemonState::rebuild_from_persisted(
                &manifest,
                test_config(),
                state.backend.clone(),
                state.state_dir.clone(),
                "persisted",
            )
            .await
            .is_err());
            assert_eq!(std::fs::read(&path).unwrap(), contents);
            assert!(state
                .backend
                .snapshots_root()
                .join("ws-restart/retained-snapshot")
                .exists());
        }
    }

    #[tokio::test]
    async fn restart_rolls_back_uncommitted_unregister_archive() {
        let temp = tempfile::tempdir().unwrap();
        let state = unregister_restart_fixture(temp.path()).await;
        let active = state.index_dir("ws-restart");
        let archive = active.with_file_name("ws-restart.unregistered");
        tokio::fs::rename(&active, &archive).await.unwrap();
        // Model termination after archive rename, before manifest replacement.
        let manifest = persist::load_state(&state.state_dir).unwrap().unwrap();
        let rebuilt = DaemonState::rebuild_from_persisted(
            &manifest,
            test_config(),
            state.backend.clone(),
            state.state_dir.clone(),
            "persisted",
        )
        .await
        .unwrap();
        let ws = rebuilt.get_by_wsid("ws-restart").unwrap();
        let ws = ws.read().await;
        assert_eq!(ws.index.snapshots.len(), 1);
        let snapshot = &ws.index.snapshots["retained-snapshot"];
        assert!(snapshot.pinned);
        assert!(!snapshot.missing);
        assert_eq!(snapshot.message.as_deref(), Some("keep this snapshot"));
        assert_eq!(ws.policy.auto_cleanup, Some(false));
        assert_eq!(
            ws.policy.auto_cleanup_keep,
            Some(CleanupRetention::Count(7))
        );
        assert!(!ws.policy_failsafe);
        assert!(active.join(INDEX_FILE).exists());
        assert!(active.join("policy.toml").exists());
        assert!(!archive.exists());
    }

    #[tokio::test]
    async fn restart_keeps_committed_unregister_archived() {
        let temp = tempfile::tempdir().unwrap();
        let state = unregister_restart_fixture(temp.path()).await;
        let active = state.index_dir("ws-restart");
        let archive = active.with_file_name("ws-restart.unregistered");
        tokio::fs::rename(&active, &archive).await.unwrap();
        state
            .persist_unregister_workspace("ws-restart")
            .await
            .unwrap();
        // Model termination after manifest replacement, before link cleanup.
        let manifest = persist::load_state(&state.state_dir).unwrap().unwrap();
        let rebuilt = DaemonState::rebuild_from_persisted(
            &manifest,
            test_config(),
            state.backend.clone(),
            state.state_dir.clone(),
            "persisted",
        )
        .await
        .unwrap();
        assert!(rebuilt.all_workspaces().is_empty());
        assert!(!active.exists());
        assert!(archive.join(INDEX_FILE).exists());
        assert!(archive.join("policy.toml").exists());
    }

    #[tokio::test]
    async fn restart_rejects_corrupt_uncommitted_unregister_archive() {
        for damaged_file in [INDEX_FILE, "policy.toml"] {
            let temp = tempfile::tempdir().unwrap();
            let state = unregister_restart_fixture(temp.path()).await;
            let active = state.index_dir("ws-restart");
            let archive = active.with_file_name("ws-restart.unregistered");
            tokio::fs::rename(&active, &archive).await.unwrap();
            tokio::fs::write(archive.join(damaged_file), "invalid metadata")
                .await
                .unwrap();
            let manifest = persist::load_state(&state.state_dir).unwrap().unwrap();
            let result = DaemonState::rebuild_from_persisted(
                &manifest,
                test_config(),
                state.backend.clone(),
                state.state_dir.clone(),
                "persisted",
            )
            .await;
            let error = result.err().expect("corrupt archive must stop rebuilding");
            assert!(error.to_string().contains("interrupted unregister"));
            assert!(archive.exists());
            assert!(!active.exists());
        }
    }

    #[test]
    fn register_and_get_by_wsid() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let index = SnapshotIndex::new(PathBuf::from("/tmp/ws"));
        state
            .register_workspace("ws-abc".to_string(), PathBuf::from("/tmp/ws"), index)
            .unwrap();

        let ws = state.get_by_wsid("ws-abc");
        assert!(ws.is_some());
    }

    #[test]
    fn register_and_get_by_path() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let path = PathBuf::from("/tmp/project");
        let index = SnapshotIndex::new(path.clone());
        state
            .register_workspace("ws-001".to_string(), path.clone(), index)
            .unwrap();

        let ws = state.get_by_path(&path);
        assert!(ws.is_some());
    }

    #[tokio::test]
    async fn register_and_verify_ws_id_content() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let path = PathBuf::from("/tmp/ws2");
        let index = SnapshotIndex::new(path.clone());
        state
            .register_workspace("ws-xyz".to_string(), path.clone(), index)
            .unwrap();

        let arc = state.get_by_wsid("ws-xyz").unwrap();
        let ws = arc.read().await;
        assert_eq!(ws.ws_id, "ws-xyz");
        assert_eq!(ws.path, path);
        assert!(ws.index.snapshots.is_empty());
    }

    #[test]
    fn get_by_wsid_nonexistent_returns_none() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        assert!(state.get_by_wsid("nonexistent").is_none());
    }

    #[test]
    fn get_by_path_nonexistent_returns_none() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        assert!(state.get_by_path(&PathBuf::from("/no/such/path")).is_none());
    }

    #[tokio::test]
    async fn resolve_workspace_by_wsid() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let path = PathBuf::from("/tmp/ws");
        let index = SnapshotIndex::new(path.clone());
        state
            .register_workspace("ws-abc123".to_string(), path, index)
            .unwrap();
        assert!(state.resolve_workspace("ws-abc123").await.is_some());
    }

    #[tokio::test]
    async fn resolve_workspace_by_path() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tokio::fs::canonicalize(tmpdir.path()).await.unwrap();
        let index = SnapshotIndex::new(path.clone());
        state
            .register_workspace("ws-path-test".to_string(), path, index)
            .unwrap();
        assert!(state
            .resolve_workspace(&tmpdir.path().to_string_lossy())
            .await
            .is_some());
    }

    #[tokio::test]
    async fn resolve_workspace_not_found_returns_none() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        assert!(state.resolve_workspace("nonexistent").await.is_none());
        assert!(state.resolve_workspace("/no/such/path").await.is_none());
    }

    #[test]
    fn path_to_wsid_bidirectional_mapping() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let path = PathBuf::from("/tmp/myws");
        let index = SnapshotIndex::new(path.clone());
        state
            .register_workspace("ws-map".to_string(), path.clone(), index)
            .unwrap();

        // path -> ws -> verify ws_id
        let arc = state.get_by_path(&path).unwrap();
        let ws = arc.try_read().unwrap();
        assert_eq!(ws.ws_id, "ws-map");
    }

    #[test]
    fn duplicate_register_preserves_original_mapping() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let path1 = PathBuf::from("/tmp/first");
        let path2 = PathBuf::from("/tmp/second");
        let index1 = SnapshotIndex::new(path1.clone());
        let index2 = SnapshotIndex::new(path2.clone());

        state
            .register_workspace("ws-dup".to_string(), path1.clone(), index1)
            .unwrap();
        assert!(state
            .register_workspace("ws-dup".to_string(), path2.clone(), index2)
            .is_err());

        let arc = state.get_by_wsid("ws-dup").unwrap();
        let ws = arc.try_read().unwrap();
        assert_eq!(ws.path, path1);
        assert!(state.get_by_path(&path1).is_some());
        assert!(state.get_by_path(&path2).is_none());
    }

    #[tokio::test]
    async fn unregister_workspace_removes_both_mappings() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let path = PathBuf::from("/tmp/removable");
        let index = SnapshotIndex::new(path.clone());
        state
            .register_workspace("ws-rm".to_string(), path.clone(), index)
            .unwrap();

        // Verify it exists
        assert!(state.get_by_wsid("ws-rm").is_some());
        assert!(state.get_by_path(&path).is_some());

        // Unregister
        state.unregister_workspace("ws-rm").await;

        // Verify both mappings removed
        assert!(state.get_by_wsid("ws-rm").is_none());
        assert!(state.get_by_path(&path).is_none());
    }

    #[test]
    fn all_workspaces_returns_all_registered() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        state
            .register_workspace(
                "ws-a".to_string(),
                PathBuf::from("/a"),
                SnapshotIndex::new(PathBuf::from("/a")),
            )
            .unwrap();
        state
            .register_workspace(
                "ws-b".to_string(),
                PathBuf::from("/b"),
                SnapshotIndex::new(PathBuf::from("/b")),
            )
            .unwrap();
        assert_eq!(state.all_workspaces().len(), 2);
    }

    #[tokio::test]
    async fn resolve_snapshot_globally_exact_match() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let mut index = SnapshotIndex::new(PathBuf::from("/tmp/ws"));
        index.snapshots.insert(
            "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            SnapshotMeta {
                message: Some("test".to_string()),
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        state
            .register_workspace("ws-abc".to_string(), PathBuf::from("/tmp/ws"), index)
            .unwrap();

        let result = state
            .resolve_snapshot_globally("abcdef1234567890abcdef1234567890abcdef12")
            .await;
        assert!(result.is_some());
        let (ws_path, snap_id) = result.unwrap();
        assert_eq!(ws_path, "/tmp/ws");
        assert_eq!(snap_id, "abcdef1234567890abcdef1234567890abcdef12");
    }

    #[tokio::test]
    async fn resolve_snapshot_globally_rejects_prefix() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let mut index = SnapshotIndex::new(PathBuf::from("/ws1"));
        index.snapshots.insert(
            "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        state
            .register_workspace("ws-1".to_string(), PathBuf::from("/ws1"), index)
            .unwrap();

        let result = state.resolve_snapshot_globally("abcdef").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_snapshot_globally_not_found() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        state
            .register_workspace(
                "ws-1".to_string(),
                PathBuf::from("/ws1"),
                SnapshotIndex::new(PathBuf::from("/ws1")),
            )
            .unwrap();
        let result = state.resolve_snapshot_globally("nonexistent").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_snapshot_globally_ambiguous_cross_workspace() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let meta = SnapshotMeta {
            message: None,
            metadata: None,
            pinned: false,
            created_at: chrono::Utc::now(),
            missing: false,
            parent_id: None,
            child_ids: vec![],
        };

        let mut idx1 = SnapshotIndex::new(PathBuf::from("/ws1"));
        idx1.snapshots.insert("same-id".to_string(), meta.clone());
        state
            .register_workspace("ws-1".to_string(), PathBuf::from("/ws1"), idx1)
            .unwrap();

        let mut idx2 = SnapshotIndex::new(PathBuf::from("/ws2"));
        idx2.snapshots.insert("same-id".to_string(), meta);
        state
            .register_workspace("ws-2".to_string(), PathBuf::from("/ws2"), idx2)
            .unwrap();

        // The exact ID exists in both workspaces
        let result = state.resolve_snapshot_globally("same-id").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn ensure_bootstrapped_btrfs_base_runs_default_bootstrap() {
        // BtrfsBase bootstrap just creates data_root & snapshots dirs; must succeed on a writable mount point.
        let tmp = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> =
            Arc::new(crate::backends::btrfs_base::BtrfsBaseBackend::new(
                tmp.path().to_path_buf(),
                crate::backends::btrfs_base::BtrfsBaseScenario::InPlace,
            ));
        let state = DaemonState::new(test_config(), backend, test_state_dir());
        state.ensure_bootstrapped().await.unwrap();
    }

    #[tokio::test]
    async fn ensure_bootstrapped_btrfs_loop_only_runs_once() {
        // For BtrfsLoop backend, the OnceCell ensures bootstrap is called at most once.
        // We can't actually run bootstrap in unit tests (requires root + btrfs),
        // but we can verify the OnceCell is properly initialized.
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        assert!(state.bootstrapped.get().is_none());
    }

    #[tokio::test]
    async fn collect_workspace_entries_does_not_drop_write_locked_ws() {
        use tokio::sync::oneshot;

        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let path_a = PathBuf::from("/ws-locked");
        let path_b = PathBuf::from("/ws-free");
        state
            .register_workspace(
                "ws-a".to_string(),
                path_a.clone(),
                SnapshotIndex::new(path_a),
            )
            .unwrap();
        state
            .register_workspace(
                "ws-b".to_string(),
                path_b.clone(),
                SnapshotIndex::new(path_b),
            )
            .unwrap();

        let ws_a = state.get_by_wsid("ws-a").unwrap();
        let (acquired_tx, acquired_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let holder = tokio::spawn(async move {
            let _guard = ws_a.write().await;
            let _ = acquired_tx.send(());
            let _ = release_rx.await;
        });
        acquired_rx.await.unwrap();

        let entries = state.collect_workspace_entries();
        let ids: std::collections::HashSet<&str> =
            entries.iter().map(|e| e.ws_id.as_str()).collect();
        assert_eq!(entries.len(), 2);
        assert!(ids.contains("ws-a"));
        assert!(ids.contains("ws-b"));

        let _ = release_tx.send(());
        holder.await.unwrap();
    }

    // ── Per-workspace policy plumbing tests ──

    #[tokio::test]
    async fn register_workspace_default_policy_is_inherit_global() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let path = PathBuf::from("/tmp/inherit");
        state
            .register_workspace("ws-i".to_string(), path.clone(), SnapshotIndex::new(path))
            .unwrap();
        let arc = state.get_by_wsid("ws-i").unwrap();
        let ws = arc.read().await;
        assert!(ws.policy.is_empty(), "default register sets empty policy");
    }

    #[tokio::test]
    async fn any_ws_has_effective_cleanup_covers_global_and_local_paths() {
        // test_config() has auto_cleanup=false, so a fresh state with no
        // ws (or only inherit-default ws) should report no effective work.
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        assert!(!state.any_ws_has_effective_cleanup().await);

        // Inherit-only ws under a globally-off config → still false.
        let path_a = PathBuf::from("/tmp/a");
        state
            .register_workspace_with_policy(
                "ws-a".to_string(),
                path_a.clone(),
                SnapshotIndex::new(path_a),
                WorkspacePolicy::default(),
                false,
            )
            .unwrap();
        assert!(!state.any_ws_has_effective_cleanup().await);

        // Per-ws override re-enables on top of globally-off → true.
        let path_b = PathBuf::from("/tmp/b");
        state
            .register_workspace_with_policy(
                "ws-b".to_string(),
                path_b.clone(),
                SnapshotIndex::new(path_b),
                WorkspacePolicy {
                    auto_cleanup: Some(true),
                    auto_cleanup_keep: None,
                },
                false,
            )
            .unwrap();
        assert!(state.any_ws_has_effective_cleanup().await);

        // Per-ws auto_cleanup=Some(false) on top of globally-off → still
        // false (the ws-b override above is what keeps the aggregate true).
        let path_c = PathBuf::from("/tmp/c");
        state
            .register_workspace_with_policy(
                "ws-c".to_string(),
                path_c.clone(),
                SnapshotIndex::new(path_c),
                WorkspacePolicy {
                    auto_cleanup: Some(false),
                    auto_cleanup_keep: None,
                },
                false,
            )
            .unwrap();
        assert!(state.any_ws_has_effective_cleanup().await);
    }

    #[tokio::test]
    async fn any_ws_has_effective_cleanup_global_keep_disabled_per_ws_overrides() {
        // Globally on but global keep is disabled (Count(0)). A ws with no
        // override inherits → effective is_disabled. Aggregate must be false.
        let mut cfg = test_config();
        cfg.auto_cleanup = true;
        cfg.auto_cleanup_keep = ws_ckpt_common::CleanupRetention::Count(0);
        let state = DaemonState::new(cfg, test_backend(), test_state_dir());

        let path_a = PathBuf::from("/tmp/a");
        state
            .register_workspace_with_policy(
                "ws-a".to_string(),
                path_a.clone(),
                SnapshotIndex::new(path_a),
                WorkspacePolicy::default(),
                false,
            )
            .unwrap();
        assert!(!state.any_ws_has_effective_cleanup().await);

        // ws-b overrides keep with a real number → effective re-enabled.
        let path_b = PathBuf::from("/tmp/b");
        state
            .register_workspace_with_policy(
                "ws-b".to_string(),
                path_b.clone(),
                SnapshotIndex::new(path_b),
                WorkspacePolicy {
                    auto_cleanup: None,
                    auto_cleanup_keep: Some(ws_ckpt_common::CleanupRetention::Count(5)),
                },
                false,
            )
            .unwrap();
        assert!(state.any_ws_has_effective_cleanup().await);
    }

    #[tokio::test]
    async fn reload_all_workspace_policies_picks_up_disk_changes() {
        // Use a real state_dir so index_dir() points at writable paths.
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let state = DaemonState::new(test_config(), test_backend(), state_dir.clone());

        let ws_id = "ws-reload";
        let path = PathBuf::from("/tmp/reload");
        state
            .register_workspace_with_policy(
                ws_id.to_string(),
                path.clone(),
                SnapshotIndex::new(path),
                WorkspacePolicy::default(),
                false,
            )
            .unwrap();

        // Operator hand-edits the policy file out-of-band (no IPC).
        let ws_index_dir = state.index_dir(ws_id);
        let new_policy = WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: Some(CleanupRetention::Count(7)),
        };
        save_workspace_policy(&ws_index_dir, &new_policy).unwrap();

        // Before reload, in-memory state still says "inherit".
        {
            let ws = state.get_by_wsid(ws_id).unwrap();
            let g = ws.read().await;
            assert!(g.policy.is_empty());
        }

        state.reload_all_workspace_policies().await;

        let ws = state.get_by_wsid(ws_id).unwrap();
        let g = ws.read().await;
        assert_eq!(g.policy, new_policy);
    }

    #[tokio::test]
    async fn reload_clears_failsafe_when_disk_read_succeeds() {
        // A ws registered as fail-safe must drop the marker once reload reads
        // the real policy.toml, so subsequent PATCH is no longer refused.
        let tmp = tempfile::tempdir().unwrap();
        let state = DaemonState::new(test_config(), test_backend(), tmp.path().to_path_buf());

        let ws_id = "ws-failsafe-reload";
        let path = PathBuf::from("/tmp/failsafe-reload");
        state
            .register_workspace_with_policy(
                ws_id.to_string(),
                path.clone(),
                SnapshotIndex::new(path),
                WorkspacePolicy {
                    auto_cleanup: Some(false),
                    auto_cleanup_keep: None,
                },
                true,
            )
            .unwrap();

        let real_policy = WorkspacePolicy {
            auto_cleanup: Some(true),
            auto_cleanup_keep: Some(CleanupRetention::Count(100)),
        };
        save_workspace_policy(&state.index_dir(ws_id), &real_policy).unwrap();

        state.reload_all_workspace_policies().await;

        let ws = state.get_by_wsid(ws_id).unwrap();
        let g = ws.read().await;
        assert_eq!(g.policy, real_policy);
        assert!(
            !g.policy_failsafe,
            "successful reload must drop the fail-safe marker"
        );
    }

    // ── lock_wsid: serializes mutations on a shared ws_id ──

    #[tokio::test]
    async fn lock_wsid_serializes_same_id() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let g1 = state.lock_wsid("ws-abc").await;

        let acquired = Arc::new(AtomicBool::new(false));
        let acquired_c = acquired.clone();
        let state_c = state.clone();
        let h = tokio::spawn(async move {
            let _g = state_c.lock_wsid("ws-abc").await;
            acquired_c.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !acquired.load(Ordering::SeqCst),
            "second lock on same ws_id must block while first guard is held"
        );

        drop(g1);
        h.await.unwrap();
        assert!(acquired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn lock_wsid_independent_for_different_ids() {
        use std::time::Duration;

        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let _g1 = state.lock_wsid("ws-a").await;
        // Different ws_id must not block.
        let _g2 = tokio::time::timeout(Duration::from_millis(100), state.lock_wsid("ws-b"))
            .await
            .expect("different ws_id must not block on each other");
    }

    #[test]
    fn duplicate_path_rejects_another_workspace_id() {
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let path = real.join("workspace");
        state
            .register_workspace(
                "ws-a".into(),
                path.clone(),
                SnapshotIndex::new(path.clone()),
            )
            .unwrap();
        let duplicate = alias.join("workspace");
        assert!(state
            .register_workspace(
                "ws-b".into(),
                duplicate.clone(),
                SnapshotIndex::new(duplicate)
            )
            .is_err());
        assert!(state.get_by_wsid("ws-b").is_none());
        assert_eq!(state.collect_workspace_entries().len(), 1);
        let original = state.get_by_wsid("ws-a").unwrap();
        assert!(Arc::ptr_eq(&original, &state.get_by_path(&path).unwrap()));
    }

    #[tokio::test]
    async fn normalized_registration_survives_detachment_and_restart() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::create_dir(real.join("tmp")).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let live = root.path().join("live");
        std::fs::create_dir(&live).unwrap();
        let anchor = real.join("workspace");
        std::os::unix::fs::symlink(&live, &anchor).unwrap();
        let input = alias.join("tmp/../workspace");
        let state_dir = root.path().join("state");
        let state = DaemonState::new(test_config(), test_backend(), state_dir.clone());
        state
            .register_workspace(
                "ws-normalized".into(),
                input.clone(),
                SnapshotIndex::new(input),
            )
            .unwrap();
        let workspace = state.get_by_wsid("ws-normalized").unwrap();
        assert_eq!(workspace.read().await.path, anchor);
        assert_eq!(workspace.read().await.index.workspace_path, anchor);
        state.save_manifest().await.unwrap();
        let mut manifest = persist::load_state(&state.state_dir).unwrap().unwrap();
        // Also migrate persisted records written by the old raw-path registry.
        manifest.workspaces[0].workspace_path = alias.join("workspace");
        std::fs::remove_file(&anchor).unwrap();
        for replacement_directory in [false, true] {
            if replacement_directory {
                std::fs::create_dir(&anchor).unwrap();
            }
            let rebuilt = DaemonState::rebuild_from_persisted(
                &manifest,
                test_config(),
                test_backend(),
                state_dir.clone(),
                "persisted",
            )
            .await
            .unwrap();
            let by_path = rebuilt.get_by_path(&anchor).unwrap();
            assert_eq!(by_path.read().await.ws_id, "ws-normalized");
            assert_eq!(
                rebuilt.collect_workspace_entries()[0].workspace_path,
                anchor
            );
            assert!(rebuilt
                .detached_registration_error(&by_path)
                .await
                .is_some());
        }
    }

    #[test]
    fn missing_parent_does_not_hide_an_existing_registration() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let anchor = parent.join("workspace");
        let state = DaemonState::new(test_config(), test_backend(), test_state_dir());
        state
            .register_workspace(
                "ws-detached".into(),
                anchor.clone(),
                SnapshotIndex::new(anchor.clone()),
            )
            .unwrap();
        std::fs::remove_dir(&parent).unwrap();
        let workspace = state.get_by_path(&anchor).unwrap();
        assert!(state.exact_registration_is_current(&anchor, "ws-detached", &workspace));
        assert_eq!(
            state.wsid_for_exact_registration_path(&anchor).as_deref(),
            Some("ws-detached")
        );
        assert!(state
            .register_workspace("ws-new".into(), anchor.clone(), SnapshotIndex::new(anchor))
            .is_err());
    }

    #[tokio::test]
    async fn restart_retains_registration_when_parent_was_removed() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let anchor = parent.join("workspace");
        let state_dir = root.path().join("state");
        let state = DaemonState::new(test_config(), test_backend(), state_dir.clone());
        state
            .register_workspace(
                "ws-detached".into(),
                anchor.clone(),
                SnapshotIndex::new(anchor.clone()),
            )
            .unwrap();
        state.save_manifest().await.unwrap();
        let manifest = persist::load_state(&state_dir).unwrap().unwrap();
        std::fs::remove_dir(&parent).unwrap();
        let rebuilt = DaemonState::rebuild_from_persisted(
            &manifest,
            test_config(),
            test_backend(),
            state_dir,
            "persisted",
        )
        .await
        .unwrap();
        let workspace = rebuilt.get_by_path(&anchor).unwrap();
        assert_eq!(workspace.read().await.path, anchor);
        assert!(rebuilt
            .detached_registration_error(&workspace)
            .await
            .is_some());
    }

    #[test]
    fn missing_parent_normalization_rejects_ambiguous_paths() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        assert_eq!(
            normalize_registration_path(&missing.join("child/workspace")).unwrap(),
            missing.join("child/workspace")
        );
        assert!(normalize_registration_path(&missing.join("../workspace")).is_err());
        let broken = root.path().join("broken");
        std::os::unix::fs::symlink(&missing, &broken).unwrap();
        assert!(normalize_registration_path(&broken.join("workspace")).is_err());
        assert!(normalize_registration_path(&broken.join("child/workspace")).is_err());
    }

    #[tokio::test]
    async fn persisted_conflicting_registration_fails_without_choosing_an_owner() {
        let root = tempfile::tempdir().unwrap();
        let state = unregister_restart_fixture(root.path()).await;
        let mut manifest = persist::load_state(&state.state_dir).unwrap().unwrap();
        let mut conflict = manifest.workspaces[0].clone();
        conflict.ws_id = "ws-conflict".into();
        manifest.workspaces.push(conflict);
        let result = DaemonState::rebuild_from_persisted(
            &manifest,
            test_config(),
            state.backend.clone(),
            state.state_dir.clone(),
            "persisted",
        )
        .await;
        assert!(
            matches!(result, Err(error) if format!("{error:#}").contains("already registered"))
        );
    }

    #[tokio::test]
    async fn persisted_internal_registration_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let state = unregister_restart_fixture(root.path()).await;
        let mut manifest = persist::load_state(&state.state_dir).unwrap().unwrap();
        manifest.workspaces[0].workspace_path = state.backend.data_root().join("ws-restart");
        let result = DaemonState::rebuild_from_persisted(
            &manifest,
            test_config(),
            state.backend.clone(),
            state.state_dir.clone(),
            "persisted",
        )
        .await;
        assert!(
            matches!(result, Err(error) if format!("{error:#}").contains("outside backend storage"))
        );
    }

    #[test]
    fn registration_boundary_resolves_backend_root_alias() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data");
        std::fs::create_dir(&data_root).unwrap();
        let alias = root.path().join("data-alias");
        std::os::unix::fs::symlink(&data_root, &alias).unwrap();
        let backend = Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            alias.clone(),
            root.path().join("test.img"),
        ));
        let state = DaemonState::new(test_config(), backend, root.path().join("state"));
        for path in [
            data_root.join("ws-a"),
            alias.join("ws-a"),
            data_root.clone(),
            alias.clone(),
            PathBuf::from("/"),
        ] {
            assert!(state
                .register_workspace("ws-a".into(), path.clone(), SnapshotIndex::new(path))
                .is_err());
        }
        let user = root.path().join("user");
        std::os::unix::fs::symlink(data_root.join("ws-a"), &user).unwrap();
        assert!(!state.registration_path_is_internal(&user).unwrap());
        state
            .register_workspace("ws-a".into(), user.clone(), SnapshotIndex::new(user))
            .unwrap();
    }

    #[test]
    fn normalization_preserves_symlink_parent_semantics() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir_all(real.join("child")).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(real.join("child"), &alias).unwrap();
        assert_eq!(
            normalize_registration_path(&alias.join("../workspace")).unwrap(),
            real.join("workspace")
        );
        let anchor = real.join("workspace");
        std::os::unix::fs::symlink(root.path().join("other"), &anchor).unwrap();
        assert_eq!(normalize_registration_path(&anchor).unwrap(), anchor);
    }

    #[test]
    fn concurrent_registration_has_one_owner_and_atomic_reverse_index() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workspace");
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let results: Vec<_> = (0..8)
            .map(|i| {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    state
                        .register_workspace(
                            format!("ws-{i}"),
                            path.clone(),
                            SnapshotIndex::new(path),
                        )
                        .is_ok()
                })
            })
            .collect();
        assert_eq!(
            results
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|ok| *ok)
                .count(),
            1
        );
        let entries = state.collect_workspace_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(state.all_workspaces().len(), 1);
        assert!(Arc::ptr_eq(
            &state.get_by_path(&path).unwrap(),
            &state.get_by_wsid(&entries[0].ws_id).unwrap()
        ));
    }
}
