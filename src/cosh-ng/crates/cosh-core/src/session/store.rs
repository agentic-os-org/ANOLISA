//! Workspace-scoped session store, legacy upgrade, and summary pagination.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::DEFAULT_SESSION_PERSIST_DIR;
use crate::provider::Message;

use super::io::{
    expand_persist_dir, io_error, now_ms, open_file_time_ms, read_bounded_open_session_file,
    MAX_SESSION_FILE_BYTES,
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

mod legacy;

const MAX_LIST_LIMIT: usize = 100;

/// Workspace-scoped session persistence with atomic commits.
pub struct SessionStore {
    base_dir: PathBuf,
    scoped: ScopedStorage,
    legacy_dirs: Vec<LegacyDirectory>,
    workspace_scope: String,
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
        let legacy_candidate = if persist_dir == DEFAULT_SESSION_PERSIST_DIR {
            Some(canonical.join("sessions"))
        } else if persist_dir_is_workspace_relative(persist_dir) {
            Some(root.clone())
        } else {
            None
        };
        let legacy_dirs = match legacy_candidate {
            Some(candidate) => {
                let workspace_directory = open_directory_path_no_follow(&canonical)
                    .map_err(|error| io_error("open canonical workspace", &canonical, error))?;
                workspace_owned_legacy_dir(&canonical, &workspace_directory, &candidate)
                    .into_iter()
                    .collect()
            }
            None => Vec::new(),
        };
        let base_dir = root.join(&scope_hash[..24]);
        let scoped = ScopedStorage::new(base_dir.clone())?;
        Ok(Self {
            base_dir,
            scoped,
            legacy_dirs,
            workspace_scope,
        })
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
        let _lock = self.scoped.acquire_lock(&directory, &session.session_id)?;
        let scoped_file = self.scoped.open_session(&directory, &session.session_id)?;
        let scoped_exists = scoped_file.is_some();
        let mut legacy = if scoped_exists {
            self.lock_legacy_session_for_clear(&session.session_id)?
        } else {
            self.lock_legacy_session(&session.session_id)?
        };
        let (current_generation, revision_floor) = match scoped_file {
            Some(file) => {
                let modified_at_ms = open_file_time_ms(&file);
                let bytes = read_bounded_open_session_file(
                    file,
                    &self.session_file(&session.session_id),
                    &session.session_id,
                )?;
                let content = decode_session_content(&session.session_id, &bytes)?;
                let current = self.parse_content(&session.session_id, content, modified_at_ms)?;
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

        self.scoped
            .write_atomic(&directory, &session.session_id, &bytes)?;
        *session = next;
        if let Some(legacy) = legacy {
            remove_locked_legacy_session_file(legacy, &session.session_id)?;
        }
        Ok(())
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
            });
        }

        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| SessionError::Corrupt {
                session_id: session_id.to_string(),
                message: "missing schema_version".to_string(),
            })? as u32;
        if version != CURRENT_SCHEMA_VERSION {
            return Err(SessionError::IncompatibleVersion {
                session_id: session_id.to_string(),
                version,
            });
        }
        let session: PersistedSession =
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
        Ok(session)
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
