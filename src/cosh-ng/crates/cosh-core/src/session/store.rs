//! Workspace-scoped session store, legacy upgrade, and summary pagination.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::config::DEFAULT_SESSION_PERSIST_DIR;
use crate::provider::Message;

use super::io::{
    expand_persist_dir, io_error, now_ms, open_file_time_ms, read_bounded_open_session_file,
    SessionLock, MAX_SESSION_FILE_BYTES,
};
use super::listing::{
    collect_list_page, entry_is_after_cursor, format_list_cursor, parse_list_cursor, ListCursor,
};
use super::scoped::ScopedStorage;
use super::summary::{
    bounded_summary_text, summary_from_session, MAX_SUMMARY_MODEL_BYTES,
    MAX_SUMMARY_WORKSPACE_BYTES,
};
use super::{
    PersistedSession, ProviderSessionId, SessionError, SessionHealth, SessionSummary,
    CURRENT_SCHEMA_VERSION,
};
use legacy::{
    collect_legacy_list_entries, collect_session_ids_from_legacy_directory,
    lock_legacy_session_file, open_directory_path_no_follow, open_legacy_session_file,
    remove_locked_legacy_session_file, workspace_owned_legacy_dir, LegacyDirectory,
    LegacySessionFile,
};

mod budget;
mod legacy;
use budget::{BudgetActivityGuard, BudgetNamespace};

const MAX_LIST_LIMIT: usize = 100;

/// Per-user task retention: 7 days, and [`TASK_BUDGET_BYTES`] below, whichever
/// comes first. Approved by the user; the decision record is not committed to
/// this repository, so the values are stated here rather than cited.
const TASK_RETENTION_DAYS: u64 = 7;
/// Per-user task budget: 100 MiB across every workspace scope of one user.
const TASK_BUDGET_BYTES: u64 = 100 * 1024 * 1024;
const MILLISECONDS_PER_DAY: u64 = 24 * 60 * 60 * 1000;
/// Length of the hex workspace-scope directory name under the session root.
const SCOPE_DIR_NAME_LEN: usize = 24;
/// Amortization window for re-reading every envelope during the age leg.
const BUDGET_RESCAN_INTERVAL_MS: u64 = 60 * 60 * 1000;
/// Lock file at the per-user session root that serializes budget decisions.
const BUDGET_LOCK_FILENAME: &str = ".budget.lock";

/// One regular session file accounted against the per-user task budget.
struct BudgetEntry {
    session_id: ProviderSessionId,
    bytes: u64,
}

/// One workspace scope directory under the per-user session root.
struct BudgetScope {
    path: PathBuf,
    entries: Vec<BudgetEntry>,
    activity: Vec<PathBuf>,
    legacy: bool,
    modern: bool,
    legacy_generation: Option<budget::LegacyGeneration>,
}

/// Workspace-scoped session persistence with atomic commits.
pub struct SessionStore {
    base_dir: PathBuf,
    budget: BudgetNamespace,
    scoped: ScopedStorage,
    legacy_dirs: Vec<LegacyDirectory>,
    workspace_scope: String,
    task_retention_ms: u64,
    task_budget_bytes: u64,
    last_budget_sweep_ms: AtomicU64,
}

pub(crate) struct CompactionLease<'a> {
    store: &'a SessionStore,
    directory: File,
    session_id: ProviderSessionId,
    lease: SessionLock,
}

impl Drop for CompactionLease<'_> {
    fn drop(&mut self) {
        let cleanup = (|| {
            let _budget_lock = self.store.budget.lock()?;
            self.store.scoped.remove_created_compaction_lease(
                &self.directory,
                &self.session_id,
                &self.lease,
            )
        })();
        if let Err(error) = cleanup {
            tracing::warn!("compaction activity cleanup failed: {error}");
        }
    }
}

impl SessionStore {
    /// Resolves a canonical workspace and its deterministic storage directory.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Io`] when the workspace cannot be canonicalized,
    /// or [`SessionError::InvalidRequest`] when its canonical path is not UTF-8.
    pub fn for_workspace(persist_dir: &str, workspace: &Path) -> Result<Self, SessionError> {
        let canonical = fs::canonicalize(workspace)
            .map_err(|error| io_error("resolve workspace", workspace, error))?;
        let workspace_scope = canonical
            .to_str()
            .ok_or_else(|| SessionError::InvalidRequest {
                message: "canonical workspace path is not valid UTF-8".to_string(),
            })?
            .to_string();
        let root = expand_persist_dir(persist_dir, &canonical);
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()
                .map_err(|error| io_error("resolve session root", &root, error))?
                .join(root)
        };
        // Resolve symlinks in the storage root once, up front, so descriptor
        // pinning can refuse symlinks strictly below the root without
        // rejecting legitimately symlinked home or dotfile layouts.
        let root = canonicalize_storage_root(&root);
        let scope_hash = hex::encode(Sha256::digest(workspace_scope.as_bytes()));
        let legacy_candidate = (if persist_dir == DEFAULT_SESSION_PERSIST_DIR {
            Some(canonical.join("sessions"))
        } else if persist_dir_is_workspace_relative(persist_dir) {
            Some(root.clone())
        } else {
            None
        })
        .map(|candidate| canonicalize_storage_root(&candidate))
        .filter(|candidate| candidate.starts_with(&canonical));
        let legacy_dirs = match legacy_candidate.as_ref() {
            Some(candidate) => {
                let workspace_directory = open_directory_path_no_follow(&canonical)
                    .map_err(|error| io_error("open canonical workspace", &canonical, error))?;
                workspace_owned_legacy_dir(&canonical, &workspace_directory, candidate)
                    .into_iter()
                    .collect()
            }
            None => Vec::new(),
        };
        let budget = BudgetNamespace::new(&root, &canonical, legacy_candidate.as_deref());
        let base_dir = root.join(&scope_hash[..24]);
        let scoped = ScopedStorage::new(base_dir.clone())?;
        Ok(Self {
            base_dir,
            budget,
            scoped,
            legacy_dirs,
            workspace_scope,
            task_retention_ms: TASK_RETENTION_DAYS.saturating_mul(MILLISECONDS_PER_DAY),
            task_budget_bytes: TASK_BUDGET_BYTES,
            last_budget_sweep_ms: AtomicU64::new(0),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test_workspace(
        persist_dir: &str,
        workspace: &Path,
    ) -> Result<Self, SessionError> {
        let default_root = workspace.join(".test-scoped-sessions");
        let effective_dir = if persist_dir == DEFAULT_SESSION_PERSIST_DIR {
            default_root.to_string_lossy().into_owned()
        } else {
            persist_dir.to_string()
        };
        let mut store = Self::for_workspace(&effective_dir, workspace)?;
        if persist_dir == DEFAULT_SESSION_PERSIST_DIR {
            let canonical = Path::new(&store.workspace_scope);
            let directory = open_directory_path_no_follow(canonical)
                .map_err(|error| io_error("open test workspace", canonical, error))?;
            store.legacy_dirs =
                workspace_owned_legacy_dir(canonical, &directory, &canonical.join("sessions"))
                    .into_iter()
                    .collect();
            let candidate = canonicalize_storage_root(&canonical.join("sessions"));
            let candidate = candidate.starts_with(canonical).then_some(candidate);
            store.budget = BudgetNamespace::new(
                store.base_dir.parent().unwrap(),
                canonical,
                candidate.as_deref(),
            );
        }
        let namespace = store.base_dir.parent().unwrap().join(".budget");
        Ok(store.with_budget_namespace(namespace))
    }

    /// Overrides the approved per-user task budget for tests only.
    #[cfg(test)]
    pub(crate) fn with_task_budget(mut self, retention_days: u64, budget_bytes: u64) -> Self {
        self.task_retention_ms = retention_days.saturating_mul(MILLISECONDS_PER_DAY);
        self.task_budget_bytes = budget_bytes;
        self.budget.path = self.base_dir.parent().unwrap().join(".budget");
        self
    }

    #[cfg(test)]
    pub(crate) fn with_budget_namespace(mut self, path: PathBuf) -> Self {
        self.budget.path = path;
        self
    }

    /// Serializes every registered storage root through the fixed user namespace.
    fn acquire_budget_lock(&self) -> Result<Option<SessionLock>, SessionError> {
        let guard = self.budget.lock()?;
        self.budget.register()?;
        Ok(Some(guard))
    }

    /// Holds an activity lease for `session_id` for as long as the returned guard
    /// lives, which the caller ties to the lifetime of a running session.
    ///
    /// A running process holds no session lock between turns, so without a lease
    /// the age leg can reclaim its envelope while its next turn is in flight; that
    /// turn then commits against a missing generation, gets `Conflict`, and the
    /// work is lost. The lease is an flock, so the kernel releases it when the
    /// process dies and a crashed session cannot pin its envelope forever — no PID
    /// file, heartbeat or TTL is needed to decide liveness.
    ///
    /// Creates the scoped directory so the first commit is protected too.
    pub(crate) fn acquire_live_lease(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<Option<SessionLock>, SessionError> {
        let _budget_lock = self.acquire_budget_lock()?;
        let Some(directory) = self.scoped.directory(true)? else {
            return Ok(None);
        };
        self.scoped
            .acquire_live_lease(&directory, session_id)
            .map(Some)
    }

    /// Protects existing history before registering a resumed workspace.
    pub(crate) fn acquire_resume_live_lease(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<Option<SessionLock>, SessionError> {
        let _budget_lock = self.budget.lock()?;
        let directory = self.scoped.directory(false)?;
        let lease = directory
            .as_ref()
            .map(|directory| self.scoped.acquire_live_lease(directory, session_id))
            .transpose()?;
        let exists = match directory.as_ref() {
            Some(directory) => self.has_session_data(directory, session_id),
            None => self
                .open_legacy_session(session_id)
                .map(|file| file.is_some()),
        };
        if !matches!(exists, Ok(true)) {
            if let (Some(directory), Some(lease)) = (directory.as_ref(), lease.as_ref()) {
                if let Err(cleanup_error) = self
                    .scoped
                    .remove_created_live_lease(directory, session_id, lease)
                {
                    tracing::warn!("failed resume activity cleanup failed: {cleanup_error}");
                }
            }
            exists?;
            return Err(SessionError::NotFound {
                session_id: session_id.to_string(),
            });
        }
        if let Err(error) = self.budget.register() {
            if let (Some(directory), Some(lease)) = (directory.as_ref(), lease.as_ref()) {
                if let Err(cleanup_error) = self
                    .scoped
                    .remove_created_live_lease(directory, session_id, lease)
                {
                    tracing::warn!("failed resume activity cleanup failed: {cleanup_error}");
                }
            }
            return Err(error);
        }
        if lease.is_some() {
            return Ok(lease);
        }
        let Some(directory) = self.scoped.directory(true)? else {
            return Ok(None);
        };
        self.scoped
            .acquire_live_lease(&directory, session_id)
            .map(Some)
    }

    pub(crate) fn cleanup_failed_resume(
        &self,
        id: &ProviderSessionId,
        lease: &SessionLock,
    ) -> Result<(), SessionError> {
        let _budget_lock = self.budget.lock()?;
        if let Some(directory) = self.scoped.directory(false)? {
            self.scoped
                .remove_created_live_lease(&directory, id, lease)?;
        }
        Ok(())
    }

    fn has_session_data(
        &self,
        directory: &File,
        id: &ProviderSessionId,
    ) -> Result<bool, SessionError> {
        Ok(self.scoped.open_session(directory, id)?.is_some()
            || self.open_legacy_session(id)?.is_some())
    }

    pub(crate) fn cleanup_rejected_commit(
        &self,
        id: &ProviderSessionId,
        lease: &SessionLock,
    ) -> Result<bool, SessionError> {
        let _budget_lock = self.budget.lock()?;
        if let Some(directory) = self.scoped.directory(false)? {
            // A failed directory sync may have already published the envelope.
            if self.has_session_data(&directory, id)? {
                return Ok(false);
            }
            self.scoped
                .remove_created_live_lease(&directory, id, lease)?;
        }
        Ok(true)
    }

    /// Applies the per-user task budget before `incoming` bytes are committed.
    ///
    /// Age and capacity are enforced in that order, whichever comes first, and
    /// the session being committed is never a candidate. When nothing may be
    /// evicted to make room, the commit is refused so the caller can stop new
    /// optional persistence instead of destroying an interaction in progress.
    ///
    /// Enumeration is rooted at the session root, so the audit trail, the run
    /// logs and the gateway approval ledger are outside it by construction:
    /// ordinary task cleanup can never reach an approval record or an unresolved
    /// execution state. Those stores are accounted by their own owners.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::BudgetExhausted`] when the budget is full and no
    /// other session of this user may be evicted for it.
    pub(crate) fn enforce_task_budget(
        &self,
        keep: &ProviderSessionId,
        incoming: u64,
    ) -> Result<(), SessionError> {
        let scopes = self.budget_scopes()?;
        // The commit replaces whatever `keep` already occupies, so only the
        // other sessions plus the incoming bytes are projected against budget.
        let mut accounted = 0u64;
        for scope in &scopes {
            for entry in &scope.entries {
                if scope.path == self.base_dir && entry.session_id == *keep {
                    continue;
                }
                accounted = accounted.saturating_add(entry.bytes);
            }
        }
        // Capacity pressure always gets a pass, because that is the case the
        // budget exists for; otherwise the envelope reads of the age leg are
        // amortized across the rescan interval.
        if accounted.saturating_add(incoming) <= self.task_budget_bytes && !self.budget_sweep_due()
        {
            return Ok(());
        }

        let cutoff = now_ms().saturating_sub(self.task_retention_ms);
        let (reclaimed, survivors) = self.evict_expired(&scopes, keep, cutoff);
        accounted = accounted.saturating_sub(reclaimed);
        if accounted.saturating_add(incoming) <= self.task_budget_bytes {
            return Ok(());
        }

        let reclaimed = self.evict_for_capacity(survivors, accounted, incoming);
        accounted = accounted.saturating_sub(reclaimed);
        let projected = accounted.saturating_add(incoming);
        if projected > self.task_budget_bytes {
            return Err(SessionError::BudgetExhausted {
                projected_bytes: projected,
                budget_bytes: self.task_budget_bytes,
            });
        }
        Ok(())
    }

    /// Age leg: drops every session whose last commit is older than `cutoff`.
    ///
    /// One pass reads each candidate's authoritative clock under its advisory
    /// lock and deletes the expired ones while that lock is still held, so no
    /// decision can race a concurrent commit. Returns the reclaimed bytes plus
    /// the survivors with the clock already read, so the capacity leg does not
    /// read them a second time.
    fn evict_expired<'a>(
        &self,
        scopes: &'a [BudgetScope],
        keep: &ProviderSessionId,
        cutoff: u64,
    ) -> (u64, Vec<(u64, &'a BudgetScope, &'a BudgetEntry)>) {
        let mut reclaimed: u64 = 0;
        let mut survivors = Vec::new();
        for scope in scopes {
            let Ok(storage) = ScopedStorage::new(scope.path.clone()) else {
                continue;
            };
            for entry in &scope.entries {
                if scope.path == self.base_dir && entry.session_id == *keep {
                    continue;
                }
                let Some((clock, evicted)) =
                    locked_clock_and_evict(&storage, scope, entry, |clock| clock <= cutoff)
                else {
                    continue;
                };
                if evicted {
                    reclaimed = reclaimed.saturating_add(entry.bytes);
                } else {
                    survivors.push((clock, scope, entry));
                }
            }
        }
        (reclaimed, survivors)
    }

    /// Capacity leg: reclaims the least recently interacted sessions until the
    /// incoming commit fits, and returns the reclaimed bytes.
    fn evict_for_capacity(
        &self,
        mut survivors: Vec<(u64, &BudgetScope, &BudgetEntry)>,
        mut accounted: u64,
        incoming: u64,
    ) -> u64 {
        // Ordered by the same envelope commit clock, never by the file's mtime.
        survivors.sort_by_key(|(clock, _, _)| *clock);
        let mut reclaimed: u64 = 0;
        for (clock, scope, entry) in survivors {
            if accounted.saturating_add(incoming) <= self.task_budget_bytes {
                break;
            }
            let Ok(storage) = ScopedStorage::new(scope.path.clone()) else {
                continue;
            };
            // Re-read under the lock: a session that committed since ordering is
            // no longer the least recently interacted one, so it must not become
            // the victim of a decision taken from a stale clock.
            let evicted =
                locked_clock_and_evict(&storage, scope, entry, |current| current <= clock)
                    .is_some_and(|(_, evicted)| evicted);
            if evicted {
                accounted = accounted.saturating_sub(entry.bytes);
                reclaimed = reclaimed.saturating_add(entry.bytes);
            }
        }
        reclaimed
    }

    /// Amortizes the envelope reads of the age leg to at most one pass per
    /// [`BUDGET_RESCAN_INTERVAL_MS`] per store, so a turn is not charged for
    /// re-reading every session of the user. Capacity pressure always gets a
    /// pass, because that is the case the budget exists for.
    fn budget_sweep_due(&self) -> bool {
        let now = now_ms();
        let last = self.last_budget_sweep_ms.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < BUDGET_RESCAN_INTERVAL_MS {
            return false;
        }
        self.last_budget_sweep_ms.store(now, Ordering::Relaxed);
        true
    }

    pub(crate) fn acquire_compaction_lease(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<CompactionLease<'_>, SessionError> {
        let _budget_lock = self.budget.lock()?;
        let directory = self.scoped.directory(false)?;
        let exists = match directory.as_ref() {
            Some(directory) => self.has_session_data(directory, session_id)?,
            None => self.open_legacy_session(session_id)?.is_some(),
        };
        if !exists {
            return Err(SessionError::NotFound {
                session_id: session_id.to_string(),
            });
        }
        self.budget.register()?;
        let directory = match directory {
            Some(directory) => directory,
            None => self
                .scoped
                .directory(true)?
                .ok_or_else(|| SessionError::NotFound {
                    session_id: session_id.to_string(),
                })?,
        };
        let lease = self
            .scoped
            .acquire_compaction_lease(&directory, session_id)?;
        Ok(CompactionLease {
            store: self,
            directory,
            session_id: session_id.clone(),
            lease,
        })
    }

    /// Walks registered scoped and workspace-owned legacy storage roots.
    ///
    /// Refuses unmeasurable ordinary scopes; symlinks and control files are not data.
    fn budget_scopes(&self) -> Result<Vec<BudgetScope>, SessionError> {
        self.budget.scopes()
    }

    fn budget_entries(path: &Path) -> Result<Vec<BudgetEntry>, SessionError> {
        let storage = ScopedStorage::new(path.to_path_buf())?;
        if storage.directory(false)?.is_none() {
            return Ok(Vec::new());
        }
        let files =
            fs::read_dir(path).map_err(|error| io_error("list budget sessions", path, error))?;
        let mut entries = Vec::new();
        for file in files {
            let file = file.map_err(|error| io_error("list budget sessions", path, error))?;
            let name = file.file_name();
            let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            let Ok(session_id) = ProviderSessionId::parse(stem) else {
                continue;
            };
            let metadata = fs::symlink_metadata(file.path())
                .map_err(|error| io_error("measure budget session", &file.path(), error))?;
            if metadata.is_file() {
                entries.push(BudgetEntry {
                    session_id,
                    bytes: metadata.len(),
                });
            }
        }
        Ok(entries)
    }

    /// Returns the canonical workspace owned by this store.
    pub fn workspace_scope(&self) -> &str {
        &self.workspace_scope
    }

    /// Returns the validated session's envelope path.
    pub fn session_file(&self, session_id: &ProviderSessionId) -> PathBuf {
        self.scoped.session_path(session_id)
    }

    /// Loads and validates a persisted or legacy session.
    ///
    /// # Errors
    ///
    /// Returns a typed error for missing, corrupt, incompatible, or mismatched data.
    pub fn load(&self, session_id: &ProviderSessionId) -> Result<PersistedSession, SessionError> {
        let (bytes, modified_at_ms) = self.read_session_bytes(session_id)?;
        let content = decode_session_content(session_id, &bytes)?;
        self.parse_content(session_id, content, modified_at_ms)
    }

    /// Atomically commits a session if its generation is current and its
    /// compaction revision clock does not move backwards.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, conflict, serialization, or I/O error.
    /// [`SessionError::Conflict`] also covers a stored compaction revision
    /// higher than the one being written.
    pub fn persist(&self, session: &mut PersistedSession) -> Result<(), SessionError> {
        if session.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(SessionError::IncompatibleVersion {
                session_id: session.session_id.to_string(),
                version: session.schema_version,
            });
        }
        if session.workspace_scope != self.workspace_scope {
            return Err(SessionError::ScopeMismatch {
                session_id: session.session_id.to_string(),
                expected: self.workspace_scope.clone(),
                actual: session.workspace_scope.clone(),
            });
        }
        let projection_cut = match session.compaction.as_ref() {
            Some(state)
                if state.compacted_through == 0
                    || state.compacted_through > session.messages.len() =>
            {
                return Err(SessionError::Corrupt {
                    session_id: session.session_id.to_string(),
                    message: format!(
                        "compaction boundary {} is outside transcript length {}",
                        state.compacted_through,
                        session.messages.len()
                    ),
                });
            }
            Some(state) => Some(state.compacted_through),
            None => None,
        };

        let Some(directory) = self.scoped.directory(true)? else {
            return Err(io_error(
                "create scoped session directory",
                &self.base_dir,
                io::Error::other("directory creation returned no descriptor"),
            ));
        };
        let lock = self.scoped.acquire_lock(&directory, &session.session_id)?;
        let mut budget_lock = None;
        let result = (|| {
            budget_lock = Some(self.budget.lock()?);
            self.budget.register()?;
            let scoped_file = self.scoped.open_session(&directory, &session.session_id)?;
            let scoped_exists = scoped_file.is_some();
            let mut legacy = if scoped_exists {
                self.lock_legacy_session_for_clear(&session.session_id)?
            } else {
                self.lock_legacy_session(&session.session_id)?
            };
            if session.was_loaded && !scoped_exists && legacy.is_none() {
                return Err(SessionError::Conflict {
                    session_id: session.session_id.to_string(),
                });
            }
            let (current_generation, revision_floor) = match scoped_file {
                Some(file) => {
                    let modified_at_ms = open_file_time_ms(&file);
                    let bytes = read_bounded_open_session_file(
                        file,
                        &self.session_file(&session.session_id),
                        &session.session_id,
                    )?;
                    let content = decode_session_content(&session.session_id, &bytes)?;
                    let current =
                        self.parse_content(&session.session_id, content, modified_at_ms)?;
                    (current.generation, current.compaction_revision)
                }
                None => (0, 0),
            };
            if current_generation != session.generation {
                return Err(SessionError::Conflict {
                    session_id: session.session_id.to_string(),
                });
            }
            // The durable compaction clock may never move backwards. The
            // generation check above cannot catch an envelope rewritten in place
            // without bumping it, and the clock exists precisely to survive such
            // untrusted on-disk edits: writing a lower value would let a later
            // commit republish a revision this session already used. Failing with
            // Conflict keeps the caller on its existing recovery path — reload,
            // observe the higher floor, and retry above it.
            if session.compaction_revision < revision_floor {
                return Err(SessionError::Conflict {
                    session_id: session.session_id.to_string(),
                });
            }
            let next_generation =
                session
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| SessionError::Corrupt {
                        session_id: session.session_id.to_string(),
                        message: "session generation is exhausted".to_string(),
                    })?;
            if scoped_exists {
                if let Some(legacy) = legacy.take() {
                    remove_locked_legacy_session_file(legacy, &session.session_id)?;
                }
            }

            let mut next = session.clone();
            next.generation = next_generation;
            next.was_loaded = true;
            next.updated_at_ms = now_ms();
            // Hold the same invariant `load` restores: the stored clock is never
            // below the revision of the projection stored beside it, so no writer
            // can persist a projection whose revision the clock would forget.
            if let Some(state) = next.compaction.as_ref() {
                next.compaction_revision = next.compaction_revision.max(state.revision);
            }
            // Redact only the on-disk copy; the caller's in-memory turn context
            // keeps the original text for the current provider conversation.
            let mut envelope = next.clone();
            crate::redaction::redact_messages(&mut envelope.messages);
            // Bind persisted projections to the redacted transcript loaded on resume.
            if let (Some(state), Some(cut)) = (envelope.compaction.as_mut(), projection_cut) {
                state.source_digest = crate::compaction::source_digest(&envelope.messages[..cut]);
            }
            let bytes =
                serde_json::to_vec_pretty(&envelope).map_err(|error| SessionError::Corrupt {
                    session_id: session.session_id.to_string(),
                    message: format!("serialization failed: {error}"),
                })?;
            if bytes.len() as u64 > MAX_SESSION_FILE_BYTES {
                return Err(SessionError::Corrupt {
                    session_id: session.session_id.to_string(),
                    message: format!(
                        "serialized session exceeds the {} byte safety limit",
                        MAX_SESSION_FILE_BYTES
                    ),
                });
            }

            // Per-user task budget (7 days / 100 MiB): reclaim expired and least
            // recently interacted sessions before committing, and refuse this
            // commit when nothing may be evicted to make room for it. The per-user
            // lock spans the decision and the write it authorizes, so two processes
            // committing different sessions cannot both admit from one snapshot and
            // land over the cap.
            self.enforce_task_budget(&session.session_id, bytes.len() as u64)?;

            self.scoped
                .write_atomic(&directory, &session.session_id, &bytes)?;
            *session = next;
            if let Some(legacy) = legacy {
                remove_locked_legacy_session_file(legacy, &session.session_id)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let cleanup = (|| {
                if budget_lock.is_none() {
                    budget_lock = Some(self.budget.lock()?);
                }
                if !self.has_session_data(&directory, &session.session_id)? {
                    self.scoped.remove_created_generation_lock(
                        &directory,
                        &session.session_id,
                        &lock,
                    )?;
                }
                Ok::<(), SessionError>(())
            })();
            if let Err(cleanup_error) = cleanup {
                tracing::warn!("uncommitted session control cleanup failed: {cleanup_error}");
            }
        }
        drop(budget_lock);
        result
    }

    /// Reads summary metadata without requiring the session to be healthy.
    ///
    /// # Errors
    ///
    /// Returns an I/O or not-found error when the envelope cannot be read.
    pub fn inspect(&self, session_id: &ProviderSessionId) -> Result<SessionSummary, SessionError> {
        let (bytes, modified_at_ms) = self.read_session_bytes(session_id)?;
        Ok(self.summary_from_bytes(session_id, &bytes, modified_at_ms))
    }

    /// Fully validates a session and returns its resumable summary.
    ///
    /// # Errors
    ///
    /// Returns the same typed validation failures as [`Self::load`].
    pub fn validate(&self, session_id: &ProviderSessionId) -> Result<SessionSummary, SessionError> {
        let session = self.load(session_id)?;
        Ok(summary_from_session(&session, SessionHealth::Ready))
    }

    /// Lists a bounded page of newest-first session summaries.
    ///
    /// When `all_workspaces` is true, aggregates sessions from every
    /// workspace-scoped store under the persistence root; sessions whose
    /// recorded workspace differs from the requested scope are returned with
    /// [`SessionHealth::ScopeMismatch`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Io`] when the scoped directory cannot be read.
    pub fn list(
        &self,
        limit: usize,
        cursor: Option<&str>,
        all_workspaces: bool,
    ) -> Result<(Vec<SessionSummary>, Option<String>), SessionError> {
        let cursor = cursor.map(parse_list_cursor).transpose()?;
        let limit = limit.clamp(1, MAX_LIST_LIMIT);
        if all_workspaces {
            self.list_all_workspaces(limit, cursor)
        } else {
            self.list_current_workspace(limit, cursor)
        }
    }

    fn list_current_workspace(
        &self,
        limit: usize,
        cursor: Option<ListCursor>,
    ) -> Result<(Vec<SessionSummary>, Option<String>), SessionError> {
        let directory = self.scoped.directory(false)?;
        let mut entries = match directory.as_ref() {
            Some(directory) => self.scoped.entries(directory)?,
            None => Vec::new(),
        };
        let mut seen_ids = entries
            .iter()
            .map(|entry| entry.session_id.clone())
            .collect::<HashSet<_>>();
        for legacy in &self.legacy_dirs {
            collect_legacy_list_entries(legacy, &mut seen_ids, &mut entries)?;
        }
        Self::sort_entries(&mut entries);
        self.page_entries(&entries, directory.as_ref(), limit, cursor)
    }

    fn list_all_workspaces(
        &self,
        limit: usize,
        cursor: Option<ListCursor>,
    ) -> Result<(Vec<SessionSummary>, Option<String>), SessionError> {
        let root = self.base_dir.parent().ok_or_else(|| SessionError::Io {
            operation: "resolve session root",
            path: self.base_dir.clone(),
            message: "scoped base directory has no parent".to_string(),
        })?;

        // Collect lightweight (path, entry) tuples from every workspace-scoped
        // directory first, then sort and page so only the target page is read
        // and deserialized. Unreadable workspace directories are skipped so a
        // single damaged or permission-denied scope cannot break the entire
        // --all listing.
        let mut all_entries = Vec::new();
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((Vec::new(), None));
            }
            Err(error) => return Err(io_error("list session roots", root, error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| io_error("read session root entry", root, error))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.len() != 24 || !name_str.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let scoped = match ScopedStorage::new(path.clone()) {
                Ok(scoped) => scoped,
                Err(error) => {
                    tracing::warn!(
                        target: "cosh_session",
                        "skip unreadable workspace {}: {}",
                        path.display(),
                        error
                    );
                    continue;
                }
            };
            let directory = match scoped.directory(false) {
                Ok(directory) => directory,
                Err(error) => {
                    tracing::warn!(
                        target: "cosh_session",
                        "skip unreadable workspace {}: {}",
                        path.display(),
                        error
                    );
                    continue;
                }
            };
            let Some(directory) = directory else {
                continue;
            };
            match scoped.entries(&directory) {
                Ok(entries) => {
                    for list_entry in entries {
                        all_entries.push((path.clone(), list_entry));
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: "cosh_session",
                        "skip unreadable workspace {}: {}",
                        path.display(),
                        error
                    );
                    continue;
                }
            }
        }

        all_entries.sort_by(|(_, left), (_, right)| {
            right
                .modified_at_ms
                .cmp(&left.modified_at_ms)
                .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
        });

        let start = match cursor.as_ref() {
            Some(cursor) => all_entries
                .iter()
                .position(|(_, entry)| entry_is_after_cursor(entry, cursor))
                .unwrap_or(all_entries.len()),
            None => 0,
        };
        let limit = limit.clamp(1, MAX_LIST_LIMIT);
        let mut page = Vec::with_capacity(limit);
        let mut examined_end = start.min(all_entries.len());
        for (workspace_path, entry) in all_entries.iter().skip(start) {
            examined_end = examined_end.saturating_add(1);

            // Open the directory handle lazily and only for entries that fall
            // on the target page. This keeps fd usage O(1) instead of
            // O(total sessions), avoiding EMFILE when many workspaces are
            // present.
            let scoped = match ScopedStorage::new(workspace_path.clone()) {
                Ok(scoped) => scoped,
                Err(_) => continue,
            };
            let directory = match scoped.directory(false) {
                Ok(directory) => directory,
                Err(_) => continue,
            };
            let Some(directory) = directory else {
                continue;
            };
            if let Some(summary) = self.scoped_entry_summary(&directory, entry) {
                page.push(summary);
                if page.len() == limit {
                    break;
                }
            }
        }
        let next_cursor = (examined_end < all_entries.len())
            .then(|| {
                all_entries
                    .get(examined_end.saturating_sub(1))
                    .map(|(_, entry)| format_list_cursor(entry))
            })
            .flatten();
        Ok((page, next_cursor))
    }

    fn sort_entries(entries: &mut [super::listing::ListEntry]) {
        entries.sort_by(|left, right| {
            right
                .modified_at_ms
                .cmp(&left.modified_at_ms)
                .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
        });
    }

    fn page_entries(
        &self,
        entries: &[super::listing::ListEntry],
        directory: Option<&File>,
        limit: usize,
        cursor: Option<ListCursor>,
    ) -> Result<(Vec<SessionSummary>, Option<String>), SessionError> {
        let start = match cursor.as_ref() {
            Some(cursor) => entries
                .iter()
                .position(|entry| entry_is_after_cursor(entry, cursor))
                .unwrap_or(entries.len()),
            None => 0,
        };
        let (page, examined_end) = collect_list_page(entries, start, limit, |entry| {
            self.list_entry_summary(directory, entry)
        });
        let next_cursor = (examined_end < entries.len())
            .then(|| {
                entries
                    .get(examined_end.saturating_sub(1))
                    .map(format_list_cursor)
            })
            .flatten();
        Ok((page, next_cursor))
    }

    /// Summarizes one scoped entry without legacy fallback.
    ///
    /// Used for cross-workspace listing where legacy sessions are intentionally
    /// not aggregated across workspace boundaries.
    fn scoped_entry_summary(
        &self,
        directory: &File,
        entry: &super::listing::ListEntry,
    ) -> Option<SessionSummary> {
        match self.read_scoped_session_bytes_from(directory, &entry.session_id) {
            Ok((content, _)) => {
                Some(self.summary_from_bytes(&entry.session_id, &content, entry.modified_at_ms))
            }
            Err(SessionError::NotFound { .. }) => None,
            Err(SessionError::Corrupt { .. }) => {
                Some(self.corrupt_summary(&entry.session_id, entry.modified_at_ms))
            }
            Err(_) => None,
        }
    }

    /// Summarizes one listed entry from scoped storage or its legacy fallback.
    fn list_entry_summary(
        &self,
        directory: Option<&File>,
        entry: &super::listing::ListEntry,
    ) -> Option<SessionSummary> {
        if let Some(directory) = directory {
            match self.read_scoped_session_bytes_from(directory, &entry.session_id) {
                Ok((content, _)) => {
                    return Some(self.summary_from_bytes(
                        &entry.session_id,
                        &content,
                        entry.modified_at_ms,
                    ));
                }
                Err(SessionError::NotFound { .. }) => {}
                Err(SessionError::Corrupt { .. }) => {
                    return Some(self.corrupt_summary(&entry.session_id, entry.modified_at_ms));
                }
                Err(_) => return None,
            }
        }
        match self.read_legacy_session_bytes(&entry.session_id) {
            Ok(Some((content, _))) => {
                Some(self.summary_from_bytes(&entry.session_id, &content, entry.modified_at_ms))
            }
            Ok(None) => None,
            Err(SessionError::Corrupt { .. }) => {
                Some(self.corrupt_summary(&entry.session_id, entry.modified_at_ms))
            }
            Err(_) => None,
        }
    }

    /// Lists canonical stored IDs without loading message history or summaries.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Io`] when the scoped directory cannot be read.
    pub fn session_ids(&self) -> Result<Vec<ProviderSessionId>, SessionError> {
        let mut unique_ids = HashSet::new();
        if let Some(directory) = self.scoped.directory(false)? {
            unique_ids.extend(
                self.scoped
                    .entries(&directory)?
                    .into_iter()
                    .map(|entry| entry.session_id),
            );
        }
        for directory in &self.legacy_dirs {
            collect_session_ids_from_legacy_directory(directory, &mut unique_ids)?;
        }
        let mut session_ids = unique_ids.into_iter().collect::<Vec<_>>();
        session_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        Ok(session_ids)
    }

    /// Removes a session unless its ID appears in the protected set.
    ///
    /// # Errors
    ///
    /// Returns a typed protected, conflict, missing, or I/O error.
    pub fn clear(
        &self,
        session_id: &ProviderSessionId,
        protected: &[ProviderSessionId],
    ) -> Result<(), SessionError> {
        if protected.iter().any(|value| value == session_id) {
            return Err(SessionError::ActiveSession {
                session_id: session_id.to_string(),
            });
        }
        let _budget_lock = self.acquire_budget_lock()?;
        let directory = self.scoped.directory(false)?;
        let scoped_file = directory
            .as_ref()
            .map(|directory| self.scoped.open_session(directory, session_id))
            .transpose()?
            .flatten();
        let scoped_exists = scoped_file.is_some();
        let legacy = self.open_legacy_session(session_id)?;
        if !scoped_exists && legacy.is_none() {
            return Err(SessionError::NotFound {
                session_id: session_id.to_string(),
            });
        }

        let _scoped_lock = match (&directory, scoped_exists) {
            (Some(directory), true) => Some(self.scoped.acquire_lock(directory, session_id)?),
            _ => None,
        };
        let legacy = legacy
            .map(|legacy| lock_legacy_session_file(legacy, session_id))
            .transpose()?;
        if let Some(legacy) = legacy {
            remove_locked_legacy_session_file(legacy, session_id)?;
        }
        if scoped_exists {
            let Some(directory) = directory.as_ref() else {
                return Err(io_error(
                    "remove scoped session",
                    &self.base_dir,
                    io::Error::other("session file has no pinned parent directory"),
                ));
            };
            self.scoped.remove_session(directory, session_id)?;
        }
        Ok(())
    }

    fn parse_content(
        &self,
        session_id: &ProviderSessionId,
        content: &str,
        modified_at_ms: u64,
    ) -> Result<PersistedSession, SessionError> {
        let mut session = self.parse_raw_session(session_id, content, modified_at_ms)?;
        if session.workspace_scope != self.workspace_scope {
            return Err(SessionError::ScopeMismatch {
                session_id: session_id.to_string(),
                expected: self.workspace_scope.clone(),
                actual: session.workspace_scope,
            });
        }
        // Envelopes are redacted at persist time; redacting again on load
        // keeps externally written or pre-redaction files equally safe.
        crate::redaction::redact_messages(&mut session.messages);
        // Bound untrusted model metadata to the summary budget so resume
        // cannot replay an oversized string into init payloads or provider
        // state that the 256-byte summary bound already refuses to carry.
        session.model = bounded_summary_text(&session.model, MAX_SUMMARY_MODEL_BYTES);
        // Raise the durable revision floor from the stored projection *before*
        // sanitization can drop it. Pre-`compaction_revision` envelopes carry
        // the clock only inside the projection, and a projection about to be
        // rejected still proves that its revision was published once.
        if let Some(state) = session.compaction.as_ref() {
            session.compaction_revision = session.compaction_revision.max(state.revision);
        }
        // A damaged or out-of-contract projection degrades to the complete
        // transcript instead of failing the load; the transcript is always a
        // safe effective context. The revision clock survives regardless, so
        // the next commit cannot reuse a revision that was already published.
        if let Some(state) = session.compaction.take() {
            session.compaction = crate::compaction::sanitize_loaded_state(state, &session.messages);
        }
        Ok(session)
    }

    /// Parses a session envelope without workspace-scope validation or the
    /// post-processing steps (redaction, model bounding, compaction repair)
    /// that are only safe for sessions that will be resumed in this workspace.
    fn parse_raw_session(
        &self,
        session_id: &ProviderSessionId,
        content: &str,
        modified_at_ms: u64,
    ) -> Result<PersistedSession, SessionError> {
        let value: serde_json::Value =
            serde_json::from_str(content).map_err(|error| SessionError::Corrupt {
                session_id: session_id.to_string(),
                message: error.to_string(),
            })?;
        if value.is_array() {
            let mut messages: Vec<Message> =
                serde_json::from_value(value).map_err(|error| SessionError::Corrupt {
                    session_id: session_id.to_string(),
                    message: error.to_string(),
                })?;
            // Redact before replay so secrets in pre-redaction files never
            // reach the provider context or picker previews.
            crate::redaction::redact_messages(&mut messages);
            return Ok(PersistedSession {
                schema_version: CURRENT_SCHEMA_VERSION,
                session_id: session_id.clone(),
                workspace_scope: self.workspace_scope.clone(),
                created_at_ms: modified_at_ms,
                updated_at_ms: modified_at_ms,
                model: String::new(),
                generation: 0,
                messages,
                compaction: None,
                compaction_revision: 0,
                was_loaded: true,
            });
        }

        parse_modern_session(session_id, value)
    }

    fn read_session_bytes(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<(Vec<u8>, u64), SessionError> {
        match self.read_scoped_session_bytes(session_id) {
            Ok(value) => return Ok(value),
            Err(SessionError::NotFound { .. }) => {}
            Err(error) => return Err(error),
        }
        if let Some(legacy) = self.read_legacy_session_bytes(session_id)? {
            return Ok(legacy);
        }
        Err(SessionError::NotFound {
            session_id: session_id.to_string(),
        })
    }

    fn read_scoped_session_bytes(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<(Vec<u8>, u64), SessionError> {
        let Some(directory) = self.scoped.directory(false)? else {
            return Err(SessionError::NotFound {
                session_id: session_id.to_string(),
            });
        };
        self.read_scoped_session_bytes_from(&directory, session_id)
    }

    fn read_scoped_session_bytes_from(
        &self,
        directory: &File,
        session_id: &ProviderSessionId,
    ) -> Result<(Vec<u8>, u64), SessionError> {
        let Some(file) = self.scoped.open_session(directory, session_id)? else {
            return Err(SessionError::NotFound {
                session_id: session_id.to_string(),
            });
        };
        let modified_at_ms = open_file_time_ms(&file);
        let bytes =
            read_bounded_open_session_file(file, &self.session_file(session_id), session_id)?;
        Ok((bytes, modified_at_ms))
    }

    fn read_legacy_session_bytes(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<Option<(Vec<u8>, u64)>, SessionError> {
        let Some(legacy) = self.open_legacy_session(session_id)? else {
            return Ok(None);
        };
        let modified_at_ms = open_file_time_ms(&legacy.file);
        let legacy_bytes = read_bounded_open_session_file(legacy.file, &legacy.path, session_id)?;
        let legacy_content = decode_session_content(session_id, &legacy_bytes)?;
        let value: serde_json::Value =
            serde_json::from_str(legacy_content).map_err(|error| SessionError::Corrupt {
                session_id: session_id.to_string(),
                message: error.to_string(),
            })?;
        if !value.is_array() {
            return Err(SessionError::Corrupt {
                session_id: session_id.to_string(),
                message: "legacy session is not a message array".to_string(),
            });
        }
        serde_json::from_value::<Vec<Message>>(value).map_err(|error| SessionError::Corrupt {
            session_id: session_id.to_string(),
            message: error.to_string(),
        })?;
        Ok(Some((legacy_bytes, modified_at_ms)))
    }

    fn lock_legacy_session(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<Option<LegacySessionFile<'_>>, SessionError> {
        let Some(legacy) = self.open_legacy_session(session_id)? else {
            return Ok(None);
        };
        let legacy = lock_legacy_session_file(legacy, session_id)?;
        let bytes = read_bounded_open_session_file(
            legacy
                .file
                .try_clone()
                .map_err(|error| io_error("clone legacy session", &legacy.path, error))?,
            &legacy.path,
            session_id,
        )?;
        let content = decode_session_content(session_id, &bytes)?;
        let value: serde_json::Value =
            serde_json::from_str(content).map_err(|error| SessionError::Corrupt {
                session_id: session_id.to_string(),
                message: error.to_string(),
            })?;
        if !value.is_array() || serde_json::from_value::<Vec<Message>>(value).is_err() {
            return Err(SessionError::Corrupt {
                session_id: session_id.to_string(),
                message: "legacy session is not a valid message array".to_string(),
            });
        }
        Ok(Some(legacy))
    }

    fn lock_legacy_session_for_clear(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<Option<LegacySessionFile<'_>>, SessionError> {
        self.open_legacy_session(session_id)?
            .map(|legacy| lock_legacy_session_file(legacy, session_id))
            .transpose()
    }

    fn open_legacy_session(
        &self,
        session_id: &ProviderSessionId,
    ) -> Result<Option<LegacySessionFile<'_>>, SessionError> {
        for directory in &self.legacy_dirs {
            match open_legacy_session_file(directory, session_id) {
                Ok(Some(legacy)) => return Ok(Some(legacy)),
                Ok(None) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    fn summary_from_bytes(
        &self,
        session_id: &ProviderSessionId,
        content: &[u8],
        modified_at_ms: u64,
    ) -> SessionSummary {
        match std::str::from_utf8(content) {
            Ok(content) => self.summary_from_content(session_id, content, modified_at_ms),
            Err(_) => self.corrupt_summary(session_id, modified_at_ms),
        }
    }

    fn summary_from_content(
        &self,
        session_id: &ProviderSessionId,
        content: &str,
        modified_at_ms: u64,
    ) -> SessionSummary {
        match self.parse_content(session_id, content, modified_at_ms) {
            Ok(session) => summary_from_session(&session, SessionHealth::Ready),
            Err(SessionError::IncompatibleVersion { version, .. }) => SessionSummary {
                session_id: session_id.clone(),
                workspace_scope: bounded_summary_text(
                    &self.workspace_scope,
                    MAX_SUMMARY_WORKSPACE_BYTES,
                ),
                created_at_ms: modified_at_ms,
                updated_at_ms: modified_at_ms,
                model: None,
                message_count: 0,
                first_prompt: None,
                schema_version: Some(version),
                health: SessionHealth::Incompatible,
            },
            Err(SessionError::ScopeMismatch { .. }) => {
                // A foreign workspace session is still visible in --all; show
                // its real metadata so the picker row matches local sessions.
                match self.parse_raw_session(session_id, content, modified_at_ms) {
                    Ok(mut session) => {
                        crate::redaction::redact_messages(&mut session.messages);
                        summary_from_session(&session, SessionHealth::ScopeMismatch)
                    }
                    Err(_) => self.corrupt_summary(session_id, modified_at_ms),
                }
            }
            Err(_) => self.corrupt_summary(session_id, modified_at_ms),
        }
    }

    fn corrupt_summary(
        &self,
        session_id: &ProviderSessionId,
        modified_at_ms: u64,
    ) -> SessionSummary {
        SessionSummary {
            session_id: session_id.clone(),
            workspace_scope: bounded_summary_text(
                &self.workspace_scope,
                MAX_SUMMARY_WORKSPACE_BYTES,
            ),
            created_at_ms: modified_at_ms,
            updated_at_ms: modified_at_ms,
            model: None,
            message_count: 0,
            first_prompt: None,
            schema_version: None,
            health: SessionHealth::Corrupt,
        }
    }
}

/// Reads a budget candidate's authoritative commit clock under its advisory
/// lock, and removes it while that lock is still held when `evict` accepts.
///
/// Returns `None` when the session must not be touched at all: another process
/// holds the lock, which is on-disk proof that a commit is in flight, or the
/// envelope carries no readable clock. Otherwise returns the clock and whether
/// the session was evicted.
fn locked_clock_and_evict(
    scoped: &ScopedStorage,
    scope: &BudgetScope,
    entry: &BudgetEntry,
    evict: impl FnOnce(u64) -> bool,
) -> Option<(u64, bool)> {
    let directory = scoped.directory(false).ok().flatten()?;
    // Keep proof of inactivity through unlink; dropping a probe would let a
    // runtime claim the session while it is still a pending eviction victim.
    // Both candidate locks are nonblocking while the user budget lock is held.
    let mut guards = Vec::new();
    for path in &scope.activity {
        let activity = ScopedStorage::new(path.clone()).ok()?;
        let directory = activity.activity_directory().ok().flatten()?;
        let lease = activity
            .acquire_live_lease(&directory, &entry.session_id)
            .ok()?;
        guards.push(BudgetActivityGuard::new(
            activity,
            directory,
            entry.session_id.clone(),
            lease,
        ));
        guards.last_mut()?.acquire_compaction().ok()?;
        guards.last_mut()?.acquire_generation().ok()?;
    }
    let file = scoped
        .open_session(&directory, &entry.session_id)
        .ok()
        .flatten()?;
    let modified_at_ms = open_file_time_ms(&file);
    if scope.legacy && !super::io::try_exclusive_lock(&file).ok()? {
        return None;
    }
    let bytes = read_bounded_open_session_file(
        file.try_clone().ok()?,
        &scoped.session_path(&entry.session_id),
        &entry.session_id,
    )
    .ok()?;
    let clock = if scope.legacy {
        if serde_json::from_slice::<Vec<Message>>(&bytes).is_ok() {
            if !scope.legacy_generation.as_ref()?.can_evict(&directory) {
                return None;
            }
            modified_at_ms
        } else if scope.modern {
            envelope_commit_clock(&bytes, &entry.session_id, &scope.path)?
        } else {
            return None;
        }
    } else {
        envelope_commit_clock(&bytes, &entry.session_id, &scope.path)?
    };
    if !evict(clock) {
        return Some((clock, false));
    }
    let evicted = scoped.remove_session(&directory, &entry.session_id).is_ok();
    Some((clock, evicted))
}

fn parse_modern_session(
    session_id: &ProviderSessionId,
    value: serde_json::Value,
) -> Result<PersistedSession, SessionError> {
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| SessionError::Corrupt {
            session_id: session_id.to_string(),
            message: "missing schema_version".to_string(),
        })?;
    let version = u32::try_from(version).map_err(|_| SessionError::Corrupt {
        session_id: session_id.to_string(),
        message: "schema_version exceeds u32".to_string(),
    })?;
    if version != CURRENT_SCHEMA_VERSION {
        return Err(SessionError::IncompatibleVersion {
            session_id: session_id.to_string(),
            version,
        });
    }
    let mut session: PersistedSession =
        serde_json::from_value(value).map_err(|error| SessionError::Corrupt {
            session_id: session_id.to_string(),
            message: error.to_string(),
        })?;
    if &session.session_id != session_id {
        return Err(SessionError::Corrupt {
            session_id: session_id.to_string(),
            message: "filename and envelope session IDs differ".to_string(),
        });
    }
    session.was_loaded = true;
    Ok(session)
}

fn envelope_commit_clock(bytes: &[u8], id: &ProviderSessionId, scope: &Path) -> Option<u64> {
    let envelope = parse_modern_session(id, serde_json::from_slice(bytes).ok()?).ok()?;
    let workspace = Path::new(&envelope.workspace_scope);
    if envelope.workspace_scope.contains('\0')
        || !workspace.is_absolute()
        || workspace
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || workspace.components().collect::<PathBuf>().to_str()
            != Some(envelope.workspace_scope.as_str())
    {
        return None;
    }
    let expected = hex::encode(sha2::Sha256::digest(envelope.workspace_scope.as_bytes()));
    if scope.file_name()?.to_str()? != &expected[..SCOPE_DIR_NAME_LEN] {
        return None;
    }
    Some(envelope.updated_at_ms)
}

fn persist_dir_is_workspace_relative(persist_dir: &str) -> bool {
    let path = Path::new(persist_dir);
    persist_dir != "~"
        && !persist_dir.starts_with("~/")
        && path.is_relative()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

/// Resolves symlinks in the deepest existing prefix of a storage root.
///
/// Missing trailing components are reattached verbatim; they are created
/// later as real directories by the descriptor-relative walk.
fn canonicalize_storage_root(root: &Path) -> PathBuf {
    let mut suffix = Vec::new();
    let mut current = root.to_path_buf();
    loop {
        match fs::canonicalize(&current) {
            Ok(mut canonical) => {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return canonical;
            }
            Err(_) => match (current.parent(), current.file_name()) {
                (Some(parent), Some(name)) => {
                    suffix.push(name.to_os_string());
                    current = parent.to_path_buf();
                }
                _ => return root.to_path_buf(),
            },
        }
    }
}

fn decode_session_content<'a>(
    session_id: &ProviderSessionId,
    bytes: &'a [u8],
) -> Result<&'a str, SessionError> {
    std::str::from_utf8(bytes).map_err(|error| SessionError::Corrupt {
        session_id: session_id.to_string(),
        message: format!("invalid UTF-8: {error}"),
    })
}

#[cfg(test)]
mod tests;
