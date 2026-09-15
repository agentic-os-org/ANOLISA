use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::audit::AuditLogger;
use crate::config::AppConfig;
use crate::consolidation::{OwnedAuditEntry, run_consolidation_owned};
use crate::error::{MemoryError, Result};
use crate::index::{IndexHandle, SearchHit};
use crate::mount::pick_strategy;
use crate::ns::{MountPoint, Namespace};
use crate::session::{EndAction, SessionId, SessionLogService};
use crate::tools::{GrepHit, GrepOptions, ListEntry, ListOptions};

/// MemoryService is the top-level entry point used by both the MCP server and
/// the CLI. It owns the namespace mount, audit logger, and (for P3+) a
/// per-process Session Log scratch area, plus (for P4+) a background index,
/// plus (for P6.2+) an optional git versioning handle.
pub struct MemoryService {
    pub mount: MountPoint,
    pub audit: Arc<AuditLogger>,
    pub session: Option<Arc<SessionLogService>>,
    pub index: Option<Arc<IndexHandle>>,
    pub embedding: Option<Arc<dyn crate::embedding::EmbeddingProvider>>,
    pub git: Option<Arc<crate::git_repo::GitHandle>>,
    pub config: AppConfig,
    /// Whether the active mount strategy entered a user namespace.
    pub entered_userns: bool,
    pub mount_strategy_name: &'static str,
    /// Counter for incremental consolidation. Incremented on every audit_log
    /// call; when it reaches `consolidation.incremental_interval`, an
    /// incremental consolidation is triggered and the counter resets.
    audit_counter: AtomicUsize,
    /// Prevents recursive consolidation: set to true while consolidate() is
    /// running, so audit_log calls from within consolidation don't re-enter.
    consolidating: std::sync::atomic::AtomicBool,
    /// Cached consent configuration for memory sovereignty checks.
    pub consent_cache:
        std::sync::Arc<std::sync::Mutex<Option<crate::tools::memory_sovereignty::ConsentConfig>>>,
}

impl MemoryService {
    /// Build the service from configuration.
    /// Always ensures the mount; starts a Session Log if the configured base
    /// directory is writable. Failure to start the session is logged and
    /// degrades gracefully (mem_promote / mem_session_log will return errors).
    pub fn new(config: AppConfig) -> Result<Self> {
        let base = config.resolved_base_dir();
        std::fs::create_dir_all(&base)?;

        // Phase 2: pick mount strategy (may unshare into a user namespace).
        let picked = pick_strategy(config.memory.mount.strategy)?;
        let entered_userns = picked.entered_userns;
        let strategy_name = picked.strategy.name();

        let ns = Namespace::user(&config.global.user_id)?;
        let mount = MountPoint::ensure_with(ns.clone(), &base, picked.strategy.as_ref())?;
        let audit = Arc::new(AuditLogger::new_with_journald(
            mount.audit_log_path(),
            config.memory.audit.journald,
        )?);

        // Start a session if the configured directory is usable.
        let session = match start_session(&config, &ns) {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                tracing::warn!(
                    "session log unavailable ({e}); mem_promote / mem_session_log will return errors"
                );
                None
            }
        };

        // Build embedding provider from config. Best-effort.
        let embedding = match crate::embedding::build_provider(&config.memory.embedding) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("embedding provider unavailable: {e}");
                None
            }
        };

        // Start the BM25 index worker if enabled.
        let embedding_clone = embedding.clone();
        let index = if config.memory.index.enabled {
            let decay_lambda = config.memory.index.time_decay_lambda;
            let alpha = config.memory.index.time_decay_alpha;
            let exclude_cold = config.memory.index.exclude_cold_on_search;
            match IndexHandle::open(&mount, embedding_clone, decay_lambda, alpha, exclude_cold) {
                Ok(h) => Some(Arc::new(h)),
                Err(e) => {
                    tracing::warn!(
                        "index unavailable ({e}); memory_search / memory_observe will degrade"
                    );
                    None
                }
            }
        } else {
            None
        };

        // Optional git versioning (P6.2). Best-effort: failure logs and
        // continues with git=None.
        let git = match crate::git_repo::GitHandle::open(config.memory.git.clone(), &mount.root) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("git versioning disabled: {e}");
                None
            }
        };

        Ok(Self {
            mount,
            audit,
            session,
            index,
            embedding,
            git,
            config,
            entered_userns,
            mount_strategy_name: strategy_name,
            audit_counter: AtomicUsize::new(0),
            consolidating: std::sync::atomic::AtomicBool::new(false),
            consent_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
        })
    }

    // ---- Tier A facade methods ----

    pub fn read(&self, path: &str) -> Result<String> {
        crate::tools::read(self, path)
    }

    pub fn write(&self, path: &str, content: &str, overwrite: bool) -> Result<u64> {
        crate::tools::write(self, path, content, overwrite)
    }

    pub fn edit(&self, path: &str, old_str: &str, new_str: &str) -> Result<()> {
        crate::tools::edit(self, path, old_str, new_str)
    }

    pub fn append(&self, path: &str, content: &str) -> Result<u64> {
        crate::tools::append(self, path, content)
    }

    pub fn list(&self, dir: &str, opts: ListOptions) -> Result<Vec<ListEntry>> {
        crate::tools::list(self, dir, opts)
    }

    pub fn grep(&self, pattern: &str, opts: GrepOptions) -> Result<Vec<GrepHit>> {
        crate::tools::grep(self, pattern, opts)
    }

    pub fn diff(&self, path1: &str, path2: &str) -> Result<String> {
        crate::tools::diff(self, path1, path2)
    }

    pub fn mkdir(&self, path: &str) -> Result<()> {
        crate::tools::mkdir(self, path)
    }

    pub fn remove(&self, path: &str, recursive: bool) -> Result<()> {
        crate::tools::remove(self, path, recursive)
    }

    pub fn promote(&self, session_path: &str, store_path: &str) -> Result<u64> {
        crate::tools::promote(self, session_path, store_path)
    }

    pub fn session_log(&self) -> Result<String> {
        crate::tools::session_log(self)
    }

    // ---- Tier B facade methods ----

    pub fn memory_search(
        &self,
        query: &str,
        top_k: usize,
        mode: Option<&str>,
        category: Option<&str>,
        agent_scope: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        crate::tools::memory_search(self, query, top_k, mode, category, agent_scope)
    }

    pub fn memory_observe(
        &self,
        content: &str,
        hint: Option<&str>,
        memory_type: Option<&str>,
    ) -> Result<String> {
        crate::tools::memory_observe(self, content, hint, memory_type, &self.config.memory)
    }

    pub fn memory_get_context(&self, max_tokens: usize) -> Result<String> {
        crate::tools::memory_get_context(self, max_tokens)
    }

    // ---- Tier C facade methods (P6 governance) ----

    pub fn mem_snapshot(&self, name: Option<&str>) -> Result<crate::snapshot::SnapshotInfo> {
        crate::tools::snapshot(self, name)
    }

    pub fn mem_snapshot_list(&self) -> Result<Vec<crate::snapshot::SnapshotInfo>> {
        crate::tools::snapshot_list(self)
    }

    pub fn mem_snapshot_restore(&self, id: &str) -> Result<()> {
        crate::tools::snapshot_restore(self, id)
    }

    /// Convenience for shutdown handlers that don't have ownership of the
    /// MemoryService: clean the session directory if we still hold the only Arc.
    pub fn try_end_session(&self, action: EndAction) {
        if let Some(arc) = &self.session {
            if action == EndAction::Discard {
                let root = arc.root().to_path_buf();
                if root.exists() {
                    if let Err(e) = std::fs::remove_dir_all(&root) {
                        tracing::warn!("failed to discard session at {}: {}", root.display(), e);
                    }
                }
            }
        }
    }

    /// Audit-log helper used by all tools: writes to the durable mount audit
    /// log AND, if a session is active, also appends to the session's
    /// in-tmpfs log.jsonl and the persistent mirror under
    /// `<mount>/.anolisa/session-logs/<sid>.jsonl`. P6.2: when git auto-commit
    /// is enabled, also fires a best-effort `git commit -am ...`. Errors are
    /// swallowed (audit must never break the foreground tool call).
    ///
    /// Incremental consolidation: counts tool calls and triggers consolidation
    /// when `consolidation.incremental_interval` is reached (0 = disabled).
    /// This ensures session data is persisted even if the process is killed
    /// (SIGKILL) before the normal shutdown consolidation.
    pub(crate) fn audit_log(&self, entry: crate::audit::AuditEntry) {
        let _ = self.audit.log(entry.clone());
        if let Some(s) = &self.session {
            let _ = s.append_log(entry.clone());
        }
        if let Some(g) = &self.git {
            g.auto_commit_for(&entry);
        }

        // Incremental consolidation: count tool calls, trigger when threshold
        // is reached. Skip internal tools (consolidate, compact) to avoid
        // feedback loops, and skip if consolidation is already running.
        let interval = self.config.memory.consolidation.incremental_interval;
        if interval > 0
            && !self.consolidating.load(Ordering::Acquire)
            && entry.tool != "consolidate"
            && entry.tool != "compact"
        {
            let prev = self.audit_counter.fetch_add(1, Ordering::AcqRel);
            if prev + 1 >= interval {
                self.audit_counter.store(0, Ordering::Release);
                self.consolidating.store(true, Ordering::Release);
                let n = self.consolidate();
                self.consolidating.store(false, Ordering::Release);
                if n > 0 {
                    tracing::info!("incremental consolidation: {n} facts written");
                }
            }
        }
    }

    pub fn mem_log(
        &self,
        limit: usize,
        path: Option<&str>,
    ) -> Result<Vec<crate::git_repo::LogEntry>> {
        crate::tools::mem_log(self, limit, path)
    }

    pub fn mem_revert(&self, path: &str) -> Result<String> {
        crate::tools::mem_revert(self, path)
    }

    /// Consolidate the current session's audit log into L1 atomic facts.
    /// Called during shutdown, after the session log is complete but before
    /// the session directory is discarded. Best-effort — failures are logged
    /// but do not block shutdown.
    ///
    /// Returns the number of facts written (0 when consolidation was skipped
    /// or produced nothing).
    pub fn consolidate(&self) -> usize {
        let config = &self.config.memory.consolidation;
        if !config.enabled {
            tracing::debug!("consolidation disabled, skipping");
            return 0;
        }

        let session = match &self.session {
            Some(s) => s,
            None => {
                tracing::debug!("no session available, skipping consolidation");
                return 0;
            }
        };

        // Read the session log.
        let log_content = match session.read_log() {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => {
                tracing::debug!("session log is empty, skipping consolidation");
                return 0;
            }
            Err(e) => {
                tracing::warn!("failed to read session log for consolidation: {e}");
                return 0;
            }
        };

        // Parse JSONL entries into owned structs (AuditEntry uses &'static str
        // for tool which can't be deserialized).
        let entries: Vec<OwnedAuditEntry> = log_content
            .lines()
            .filter_map(|line| serde_json::from_str::<OwnedAuditEntry>(line).ok())
            .collect();

        if entries.is_empty() {
            tracing::debug!("no parseable audit entries, skipping consolidation");
            return 0;
        }

        let session_id = session.sid().as_str();

        // Check consent before running consolidation.
        if !crate::tools::memory_sovereignty::is_source_allowed(self, "auto-consolidation") {
            tracing::info!("consolidation skipped: auto-consolidation denied by consent config");
            return 0;
        }

        // Quality filter: check mutual exclusion BEFORE running heuristics
        // to avoid wasted I/O and CPU.
        let manual_count = entries
            .iter()
            .filter(|e| e.tool == "memory_observe")
            .count();
        if crate::consolidation::quality::should_skip_consolidation(manual_count) {
            tracing::info!(
                "skipping consolidation: session {session_id} has {manual_count} manual observations"
            );
            return 0;
        }

        // Convert to OwnedAuditEntry for heuristics.
        let mut facts = run_consolidation_owned(&entries, session_id, config);

        // Quality filter: remove derivable facts and normalize dates
        let before = facts.len();
        facts.retain(|f| !crate::consolidation::quality::is_derivable(&f.content));
        facts.iter_mut().for_each(|f| {
            f.content = crate::consolidation::quality::normalize_relative_dates(&f.content);
        });
        let filtered = before - facts.len();
        if filtered > 0 {
            tracing::debug!("filtered {filtered} derivable facts");
        }

        if facts.is_empty() {
            tracing::debug!("consolidation produced no facts for session {session_id}");
            return 0;
        }

        // Write facts to the memory store via sandboxed FactWriter.
        let mut writer = crate::consolidation::FactWriter::new(&self.mount.root)
            .with_root_fd(self.mount.root_fd.clone());
        // Wire conflict detection when both config and index are available.
        if config.conflict_detection {
            if let Some(ref index_handle) = self.index {
                let store = index_handle.store_arc();
                writer = writer.with_index(store, config.conflict_bm25_threshold);
            }
        }
        match writer.write_batch(&facts) {
            Ok(n) => {
                tracing::info!(
                    "consolidation complete: {n}/{} facts written from session {session_id}",
                    facts.len()
                );
                // Log an audit entry for consolidation.
                self.audit_log(
                    crate::audit::AuditEntry::new("consolidate")
                        .path(format!("{n} facts from session {session_id}"))
                        .bytes(n as u64),
                );
                n
            }
            Err(e) => {
                tracing::warn!("consolidation write failed: {e}");
                0
            }
        }
    }

    /// Compact the memory index: mark old, never-accessed files as cold.
    pub fn compact(&self) -> Result<usize> {
        let index = match self.index.as_ref() {
            Some(i) => i,
            None => {
                return Err(crate::error::MemoryError::NotImplemented(
                    "index disabled; compact requires an active index",
                ));
            }
        };
        let cold_after = self.config.memory.index.cold_after_days;
        index.compact(cold_after)
    }
}

/// Monotonic suffix for the writability probe file so two servers probing
/// the same candidate concurrently never collide on the name. Each probe
/// reserves a whole `PROBE_ATTEMPTS` block, so a retry cannot step onto a
/// name another prober was just handed either.
static PROBE_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Session base directories to try, in preference order.
///
/// The second element of each pair says whether the candidate is one *we*
/// picked (and therefore has to be hardened against a local user planting
/// the name first) or one the operator configured (their own choice, so it
/// is only required to be a usable directory).
///
/// The configured directory — `/run/anolisa/sessions` by default, and the
/// value the OpenClaw plugin forwards as `MEMORY_SESSION_DIR` on every
/// spawn — is only writable by root, yet the server always runs
/// unprivileged:
///
/// - the RPM ships `config/systemd/anolisa-memory-tmpfiles.conf`, which
///   creates `/run/anolisa` and `/run/anolisa/sessions` `0700 root:root`.
///   A non-root server can neither traverse the parent (no `x` for others)
///   nor create `<sid>` inside it.
/// - `make install`, containers and dev boxes ship no tmpfiles snippet at
///   all, and `/run` itself is `drwxr-xr-x root root`, so even the first
///   `create_dir_all` component fails with EACCES.
/// - the shipped unit is a *user* template (`anolisa-memory@.service`), and
///   MCP clients such as the OpenClaw plugin spawn `agent-memory serve`
///   directly as the logged-in user.
///
/// Before the fallback existed, every one of those cases made
/// `start_session` fail and `MemoryService::new` degrade to `session = None`
/// behind a single `warn!` — so on a stock install *every* `mem_promote`
/// returned `SessionUnavailable`, every `mem_session_log` returned
/// `NotImplemented`, and the session-scoped consolidation path had no log
/// to read.
///
/// Both fallbacks are per-user by construction: `$XDG_RUNTIME_DIR` is
/// already `0700` and owned by the user, and the tmp candidate carries the
/// uid so two users on one host never share a base.
fn session_base_candidates(configured: &Path) -> Vec<(PathBuf, bool)> {
    let mut out: Vec<(PathBuf, bool)> = vec![(configured.to_path_buf(), false)];

    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let xdg = xdg.trim();
        if !xdg.is_empty() {
            out.push((Path::new(xdg).join("anolisa").join("sessions"), true));
        }
    }

    let uid = nix::unistd::Uid::current().as_raw();
    let tmp = std::env::var("TMPDIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    out.push((tmp.join(format!("anolisa-sessions-{uid}")), true));

    out
}

/// First candidate that can be created *and* written to, else an error
/// naming every directory that was tried.
fn pick_session_base(candidates: &[(PathBuf, bool)]) -> Result<PathBuf> {
    let mut rejected: Vec<String> = Vec::with_capacity(candidates.len());
    for (dir, ours) in candidates {
        match probe_session_base(dir, *ours) {
            Ok(()) => return Ok(dir.clone()),
            Err(e) => rejected.push(format!("{}: {e}", dir.display())),
        }
    }
    Err(MemoryError::Other(format!(
        "no writable session directory; tried {}",
        rejected.join(", ")
    )))
}

/// Resolve the session scratch base, falling back to a per-user runtime or
/// tmp directory when the configured one is not usable. A fallback is
/// reported with `warn!` so the operator can see that the configured
/// location was ignored and why.
fn resolve_session_base(config: &AppConfig) -> Result<PathBuf> {
    let configured = config.resolved_session_dir();
    let candidates = session_base_candidates(&configured);
    let base = pick_session_base(&candidates)?;
    if base != configured {
        tracing::warn!(
            "session dir {} is not usable by uid {}; using {} instead \
             (set MEMORY_SESSION_DIR to override)",
            configured.display(),
            nix::unistd::Uid::current().as_raw(),
            base.display()
        );
    }
    Ok(base)
}

/// How many distinct probe names one `probe_writable` call may burn before
/// it gives up on the candidate. Only a stale probe left by a killed server
/// can occupy a name (a symlink ends the attempt immediately), so a handful
/// is plenty; the whole block is reserved out of `PROBE_SEQ` at once so two
/// concurrent probers can never be handed the same name.
const PROBE_ATTEMPTS: usize = 8;

/// Whether a session base *we* chose may be used given the uid that owns it.
///
/// There is deliberately no root exemption. `/tmp/anolisa-sessions-0` is a
/// predictable name in a world-writable directory, so "the owner is not me
/// but I am root, so it is fine" lets any local user pre-create the base and
/// then own every session root a root server builds inside it — scratch
/// files, `log.jsonl` and the `mem_promote` source tree — including swapping
/// a known `MEMORY_SESSION_ID` entry for a symlink that the server's own
/// `create_dir_all` / chmod / metadata writes then follow.
fn fallback_owner_is_us(owner: u32, me: u32) -> bool {
    owner == me
}

/// Prove `dir` is writable by creating and removing a scratch file in it.
///
/// `seq_base` is the first of `PROBE_ATTEMPTS` reserved probe names.
///
/// The name is predictable, so on a base another local user can write to —
/// which includes an operator-configured one, since a group- or
/// world-writable `MEMORY_SESSION_DIR` is explicitly allowed — it can be
/// pre-planted. `std::fs::write` follows symlinks and truncates the target,
/// so probing with it turned the writability check itself into a
/// file-clobber primitive against anything the server uid can write.
/// `create_new` is `O_CREAT|O_EXCL`, which refuses to open, let alone
/// follow, whatever already occupies the name.
fn probe_writable(dir: &Path, seq_base: usize) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let pid = std::process::id();
    for offset in 0..PROBE_ATTEMPTS {
        let probe = dir.join(format!(".anolisa-probe-{pid}-{}", seq_base + offset));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&probe)
        {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A symlink means somebody is actively squatting on this
                // base, so refuse the candidate rather than unlink their
                // file. A regular file is a stale probe from a killed
                // server: not ours to delete, so take the next name.
                if std::fs::symlink_metadata(&probe).is_ok_and(|md| md.file_type().is_symlink()) {
                    return Err(MemoryError::Other(format!(
                        "{} holds a symlink at {}; refusing it as a session dir",
                        dir.display(),
                        probe.display()
                    )));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    Err(MemoryError::Other(format!(
        "no free .anolisa-probe-* name under {} after {PROBE_ATTEMPTS} attempts",
        dir.display()
    )))
}

/// Create `dir` if needed and prove it is writable.
///
/// Existence alone is not enough: a pre-existing read-only directory
/// passes `create_dir_all` and then fails on the first session, so the
/// probe does a real create-and-remove.
///
/// For directories *we* chose (`ours`), additionally refuse a symlink, an
/// owner other than our effective uid (root included), and a base that
/// cannot be tightened to `0700`. Without that, any local user could plant
/// `/tmp/anolisa-sessions-<victim uid>` as a symlink — or, against a root
/// server, own the directory outright — and redirect both the probe and
/// every session root into a directory of their choosing. The
/// operator-configured directory is exempt from those three: a symlink, a
/// shared owner or a group-writable mount there is the operator's own
/// decision. It still gets the `O_EXCL` probe, because "the operator chose
/// a world-writable location" is not "the operator chose to let neighbours
/// clobber our files".
fn probe_session_base(dir: &Path, ours: bool) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if ours && std::fs::symlink_metadata(dir).is_ok_and(|md| md.file_type().is_symlink()) {
        return Err(MemoryError::Other(format!(
            "{} is a symlink; refusing it as a session dir",
            dir.display()
        )));
    }

    let mut md = match std::fs::metadata(dir) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)?;
            std::fs::metadata(dir)?
        }
        Err(e) => return Err(e.into()),
    };

    if !md.is_dir() {
        return Err(MemoryError::Other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }

    if ours {
        let me = nix::unistd::Uid::current().as_raw();
        if !fallback_owner_is_us(md.uid(), me) {
            return Err(MemoryError::Other(format!(
                "{} is owned by uid {}, not {me}; refusing it as a session dir",
                dir.display(),
                md.uid()
            )));
        }

        // Fallbacks can land under a world-writable /tmp, and a base created
        // before this hardening keeps whatever the umask gave it. Each
        // `<sid>` is already forced to 0700 by `SessionLogService::start`,
        // but the base should not be listable or writable by other users
        // either. Both failures reject the candidate instead of being
        // swallowed: a chmod that errors, or that a filesystem accepts
        // without applying (some FUSE and network mounts do), would
        // otherwise leave session data in a base we just promised was 0700.
        if md.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
                MemoryError::Other(format!("cannot tighten {} to 0700: {e}", dir.display()))
            })?;
            md = std::fs::metadata(dir)?;
            if md.mode() & 0o077 != 0 {
                return Err(MemoryError::Other(format!(
                    "{} is still mode {:o} after chmod; refusing it as a session dir",
                    dir.display(),
                    md.mode() & 0o777
                )));
            }
        }
    }

    probe_writable(dir, PROBE_SEQ.fetch_add(PROBE_ATTEMPTS, Ordering::Relaxed))
}

fn start_session(config: &AppConfig, ns: &Namespace) -> Result<SessionLogService> {
    let base = resolve_session_base(config)?;
    let sid = match std::env::var("MEMORY_SESSION_ID") {
        Ok(s) if !s.is_empty() => match SessionId::from_string(&s) {
            Ok(sid) => sid,
            Err(e) => {
                tracing::warn!("MEMORY_SESSION_ID={s:?} rejected ({e}); generating a fresh id");
                SessionId::generate()
            }
        },
        _ => SessionId::generate(),
    };
    let agent_id = std::env::var("MCP_CLIENT_NAME").ok();

    // Persistent mirror directory: <mount>/.anolisa/session-logs/
    // Session logs are mirrored here so they survive SIGKILL and tmpfs loss.
    let mirror_dir = mount_root_for_session_mirror(config, ns);

    SessionLogService::start(
        &base,
        sid,
        &config.global.user_id,
        agent_id.as_deref(),
        &ns.dir_name(),
        mirror_dir.as_deref(),
    )
}

/// Compute the persistent mirror directory for session logs.
/// Returns `<base_dir>/<ns>/session-logs/` under the mount root.
fn mount_root_for_session_mirror(config: &AppConfig, ns: &Namespace) -> Option<PathBuf> {
    let base = config.resolved_base_dir();
    let mirror = base
        .join(ns.dir_name())
        .join(".anolisa")
        .join("session-logs");
    Some(mirror)
}

#[cfg(test)]
mod session_base_tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn modes(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn prefers_a_writable_configured_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let configured = tmp.path().join("sessions");
        let fallback = tmp.path().join("fallback");
        let picked =
            pick_session_base(&[(configured.clone(), false), (fallback.clone(), true)]).unwrap();
        assert_eq!(picked, configured);
        assert!(
            !fallback.exists(),
            "must not touch the fallback unnecessarily"
        );
    }

    #[test]
    fn skips_a_configured_dir_that_cannot_be_created() {
        // A regular file where the session base should be: create_dir_all can
        // never succeed. This is the shape a non-root server hits on the
        // shipped default /run/anolisa/sessions.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let fallback = tmp.path().join("fallback");

        let picked =
            pick_session_base(&[(blocker.join("sessions"), false), (fallback.clone(), true)])
                .unwrap();
        assert_eq!(picked, fallback);
        assert!(fallback.join("x").parent().unwrap().exists());
    }

    #[test]
    fn skips_an_existing_read_only_dir() {
        if nix::unistd::Uid::current().is_root() {
            eprintln!("skipped: root bypasses directory permissions");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let ro = tmp.path().join("ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
        let fallback = tmp.path().join("fallback");

        // create_dir_all succeeds on `ro` (it already exists), so only the
        // write probe can catch it.
        let picked = pick_session_base(&[(ro.clone(), false), (fallback.clone(), true)]).unwrap();
        assert_eq!(picked, fallback);
        assert!(
            !std::fs::read_dir(&ro)
                .unwrap()
                .next()
                .is_some_and(|e| e.is_ok_and(|e| e
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".anolisa-probe-"))),
            "probe must not leave a file behind in a directory it rejected"
        );
    }

    #[test]
    fn refuses_a_symlinked_fallback_but_honours_a_symlinked_configured_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let good = tmp.path().join("good");

        // A directory we chose has to be a real one, or any local user could
        // plant the name and redirect every session root.
        let picked = pick_session_base(&[(link.clone(), true), (good.clone(), true)]).unwrap();
        assert_eq!(picked, good);

        // The operator's own symlink is their decision, not an attack.
        let picked = pick_session_base(&[(link.clone(), false), (good, true)]).unwrap();
        assert_eq!(picked, link);
    }

    #[test]
    fn creates_a_fallback_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let ours = tmp.path().join("fresh").join("anolisa-sessions-1000");
        let picked = pick_session_base(&[(ours.clone(), true)]).unwrap();
        assert_eq!(picked, ours);
        assert_eq!(modes(&ours), 0o700, "fallback base must not be listable");
    }

    #[test]
    fn reports_every_candidate_when_none_is_usable() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let err = pick_session_base(&[(blocker.join("a"), false), (blocker.join("b"), true)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("no writable session directory"), "{err}");
        assert!(err.contains("blocker/a"), "{err}");
        assert!(err.contains("blocker/b"), "{err}");
    }

    #[test]
    fn candidate_chain_starts_with_the_configured_dir_and_ends_in_tmp() {
        let configured = Path::new("/run/anolisa/sessions");
        let candidates = session_base_candidates(configured);
        assert_eq!(candidates[0].0, configured);
        assert!(
            !candidates[0].1,
            "the configured dir is the operator's choice"
        );
        let (last, ours) = candidates.last().unwrap();
        assert!(ours, "fallbacks must be hardened");
        assert!(
            last.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("anolisa-sessions-")),
            "tmp fallback must be uid-suffixed, got {}",
            last.display()
        );
    }

    // ---------- review follow-ups: hardening the probe and the fallback ----------

    #[test]
    fn fallback_owner_check_has_no_root_exemption() {
        // `/tmp/anolisa-sessions-0` is a predictable name in a world-writable
        // directory. Accepting an existing base just because the server
        // happens to be uid 0 hands any local user control over every session
        // root the root server builds inside it, so the check compares
        // against the effective uid and nothing else.
        assert!(fallback_owner_is_us(0, 0));
        assert!(fallback_owner_is_us(1000, 1000));
        assert!(
            !fallback_owner_is_us(1000, 0),
            "a root server must reject a user-owned fallback"
        );
        assert!(!fallback_owner_is_us(0, 1000));
    }

    #[test]
    fn rejects_a_foreign_owned_fallback_even_as_root() {
        use nix::unistd::{Gid, Uid, chown};

        if !nix::unistd::Uid::current().is_root() {
            eprintln!("skipped: planting a foreign owner needs root");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let planted = tmp.path().join("anolisa-sessions-0");
        std::fs::create_dir_all(&planted).unwrap();
        // Addressed numerically, so this does not depend on `nobody` existing.
        chown(
            &planted,
            Some(Uid::from_raw(65534)),
            Some(Gid::from_raw(65534)),
        )
        .unwrap();
        let good = tmp.path().join("next");

        let picked = pick_session_base(&[(planted.clone(), true), (good.clone(), true)]).unwrap();

        assert_eq!(
            picked,
            good,
            "must fall through instead of using the foreign-owned {}",
            planted.display()
        );
    }

    #[test]
    fn probe_refuses_to_follow_a_planted_symlink() {
        // The probe name is predictable (pid + a counter), so on a base
        // another local user can write to it can be pre-planted. That
        // includes an operator-configured one: a group-writable
        // MEMORY_SESSION_DIR is explicitly allowed. `std::fs::write` followed
        // the link and truncated the target, which made the writability check
        // itself a file-clobber primitive against anything the server uid can
        // write; O_CREAT|O_EXCL refuses the name instead.
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let base = tmp.path().join("base");
        std::fs::create_dir_all(&base).unwrap();
        let planted = base.join(format!(".anolisa-probe-{}-7", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        let err = probe_writable(&base, 7).unwrap_err().to_string();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"precious",
            "the symlink target must survive the probe"
        );
        assert!(
            std::fs::symlink_metadata(&planted)
                .unwrap()
                .file_type()
                .is_symlink(),
            "a squatter's file is not ours to unlink"
        );
    }

    #[test]
    fn probe_steps_over_a_stale_file_and_removes_only_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        std::fs::create_dir_all(&base).unwrap();
        let pid = std::process::id();
        let stale = base.join(format!(".anolisa-probe-{pid}-3"));
        std::fs::write(&stale, b"left behind by a killed server").unwrap();

        probe_writable(&base, 3).unwrap();

        assert_eq!(
            std::fs::read(&stale).unwrap(),
            b"left behind by a killed server",
            "a stale probe is not ours to delete"
        );
        let probes: Vec<String> = std::fs::read_dir(&base)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".anolisa-probe-"))
            .collect();
        assert_eq!(
            probes,
            vec![stale.file_name().unwrap().to_string_lossy().into_owned()],
            "our own probe must be cleaned up"
        );
    }

    #[test]
    fn tightens_a_pre_existing_fallback_to_0700() {
        // A base created before this hardening keeps whatever the umask gave
        // it. The chmod that fixes that is no longer a `let _ =`, so a
        // filesystem that refuses it rejects the candidate instead of quietly
        // holding session data in a directory other users can list.
        let tmp = tempfile::tempdir().unwrap();
        let ours = tmp.path().join("anolisa-sessions-1000");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::set_permissions(&ours, std::fs::Permissions::from_mode(0o755)).unwrap();

        let picked = pick_session_base(&[(ours.clone(), true)]).unwrap();

        assert_eq!(picked, ours);
        assert_eq!(modes(&ours), 0o700);
    }

    #[test]
    fn leaves_an_operator_configured_dir_mode_alone() {
        // The tightening above is for directories *we* chose. An operator who
        // points MEMORY_SESSION_DIR at a group-shared mount made that call
        // deliberately, and the probe must not widen or narrow it.
        let tmp = tempfile::tempdir().unwrap();
        let configured = tmp.path().join("shared");
        std::fs::create_dir_all(&configured).unwrap();
        std::fs::set_permissions(&configured, std::fs::Permissions::from_mode(0o770)).unwrap();

        let picked = pick_session_base(&[(configured.clone(), false)]).unwrap();

        assert_eq!(picked, configured);
        assert_eq!(modes(&configured), 0o770);
    }
}
