//! I2: Configurable Installer Staging Compatibility.
//!
//! Supports installers that create temporary staging directories under the
//! managed skills root (e.g. `.openclaw-install-stage-*`), write files,
//! and later rename to the final skill name.
//!
//! Staging roots are hidden from the `/skills` parent listing and from the
//! agent discovery view, but **exact-path access** is fully allowed:
//! lookup, stat, opendir, readdir, read, write, create, mkdir, rename,
//! unlink, rmdir, and setattr all follow normal physical passthrough
//! behavior.  The active resolver hidden gate is bypassed for staging
//! roots so that installers can traverse and populate the staging
//! directory through the FUSE mount even when a ledger-driven resolver is
//! attached.  SKILL.md inside a staging root is served as a raw physical
//! file (no compiler pass, no virtual size projection).
//!
//! Staging intermediate mutations are suppressed — no notify, no refresh,
//! no quiet-timeout.  Completion is signaled by an atomic rename from a
//! configured staging root to a valid skill name.  The rename enqueues
//! exactly one rename mutation notification through the background notify
//! worker.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::config::ConfigError;
use super::inbox::is_valid_inbox_skill_name;
use super::lifecycle::is_reserved_lifecycle_name;
use super::notify::NotifyController;
use super::path::SKILL_META_DIR;
use super::refresh::MutationKind;

// ---------------------------------------------------------------------------
// StagingPattern
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagingPattern {
    Exact(String),
    PrefixStar(String),
}

impl StagingPattern {
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        if s.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "install.staging_patterns",
                value: String::new(),
                allowed: "non-empty pattern",
            });
        }
        if s.contains('/') {
            return Err(ConfigError::InvalidValue {
                field: "install.staging_patterns",
                value: s.to_string(),
                allowed: "pattern must not contain '/'",
            });
        }
        if s.contains("..") {
            return Err(ConfigError::InvalidValue {
                field: "install.staging_patterns",
                value: s.to_string(),
                allowed: "pattern must not contain '..'",
            });
        }

        if let Some(prefix) = s.strip_suffix('*') {
            if prefix.is_empty() {
                return Err(ConfigError::InvalidValue {
                    field: "install.staging_patterns",
                    value: s.to_string(),
                    allowed: "prefix before '*' must be non-empty",
                });
            }
            if prefix.contains('*') {
                return Err(ConfigError::InvalidValue {
                    field: "install.staging_patterns",
                    value: s.to_string(),
                    allowed: "only trailing '*' allowed",
                });
            }
            Ok(StagingPattern::PrefixStar(prefix.to_string()))
        } else {
            if s.contains('*') {
                return Err(ConfigError::InvalidValue {
                    field: "install.staging_patterns",
                    value: s.to_string(),
                    allowed: "only trailing '*' allowed",
                });
            }
            Ok(StagingPattern::Exact(s.to_string()))
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        match self {
            StagingPattern::Exact(exact) => name == exact,
            StagingPattern::PrefixStar(prefix) => name.starts_with(prefix),
        }
    }
}

// ---------------------------------------------------------------------------
// UnactivatedVisibility
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnactivatedVisibility {
    Hidden,
}

impl UnactivatedVisibility {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }
}

impl Default for UnactivatedVisibility {
    fn default() -> Self {
        Self::Hidden
    }
}

// ---------------------------------------------------------------------------
// StagingConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StagingConfig {
    pub patterns: Vec<StagingPattern>,
    pub unactivated_visibility: UnactivatedVisibility,
}

impl Default for StagingConfig {
    fn default() -> Self {
        Self {
            patterns: Vec::new(),
            unactivated_visibility: UnactivatedVisibility::Hidden,
        }
    }
}

// ---------------------------------------------------------------------------
// StagingMatcher
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StagingMatcher {
    config: StagingConfig,
}

impl StagingMatcher {
    pub fn new(config: StagingConfig) -> Self {
        Self { config }
    }

    /// Returns `true` when `name` matches a configured staging pattern.
    /// Only meant for top-level directory names under the managed root.
    pub fn is_staging_root(&self, name: &str) -> bool {
        self.config
            .patterns
            .iter()
            .any(|pattern| pattern.matches(name))
    }

    pub fn config(&self) -> &StagingConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Rename target validation
// ---------------------------------------------------------------------------

/// Validate whether `target_name` is an acceptable final skill name for a
/// staging-to-skill rename. Rejects sensitive namespaces, invalid skill
/// names, and staging patterns themselves.
pub fn is_valid_staging_rename_target(target_name: &str, matcher: &StagingMatcher) -> bool {
    if !is_valid_inbox_skill_name(target_name) {
        return false;
    }
    if target_name == SKILL_META_DIR {
        return false;
    }
    if target_name == super::inbox::INBOX_DIR_NAME {
        return false;
    }
    if target_name == "skill-discover" {
        return false;
    }
    if is_reserved_lifecycle_name(target_name) {
        return false;
    }
    if matcher.is_staging_root(target_name) {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// InstallerStagingController
// ---------------------------------------------------------------------------

/// Handles mutation notification for staging-to-skill renames.
///
/// Staging root writes are silently suppressed (no notify, no pending
/// tracking). When a staging root is renamed to a valid final skill
/// name, `emit_staging_rename_notify` enqueues one immediate rename
/// mutation notification through the background notify worker.
pub struct InstallerStagingController {
    matcher: Arc<StagingMatcher>,
    notify_controller: Arc<NotifyController>,
}

impl InstallerStagingController {
    pub fn new(
        matcher: Arc<StagingMatcher>,
        notify_controller: Arc<NotifyController>,
    ) -> Arc<Self> {
        Arc::new(Self {
            matcher,
            notify_controller,
        })
    }

    /// Returns `true` when `name` matches a configured staging pattern.
    pub fn is_staging_root(&self, name: &str) -> bool {
        self.matcher.is_staging_root(name)
    }

    /// Enqueue a rename mutation notification for a staging-to-skill
    /// rename. Non-blocking: the background worker dispatches the socket
    /// send and activation reload poll. The FUSE reply is not delayed.
    pub fn emit_staging_rename_notify(&self, skill_name: &str) {
        self.notify_controller
            .enqueue_immediate(skill_name, MutationKind::Rename, Vec::new());
        debug!(
            skill = %skill_name,
            "staging: rename mutation notification enqueued"
        );
    }

    pub fn matcher(&self) -> &StagingMatcher {
        &self.matcher
    }
}

// ---------------------------------------------------------------------------
// QuietTimeoutController
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum QuietCommand {
    Wakeup,
    Shutdown,
}

/// Emits aggregated mutation notifications for final skill directories
/// that have been quiet (no mutations) for a configured duration. Each
/// skill is tracked independently; multiple writes within the quiet
/// window are collapsed into a single notification after the timeout
/// expires. A subsequent write after a fired notification starts a new
/// window.
///
/// This targets direct-to-final-skill installs where the installer
/// writes files into a legitimate skill directory without using the
/// staging rename boundary or the `.install-complete` sentinel.
///
/// The controller owns a background worker thread. `shutdown()` sends
/// the stop signal and joins the worker; `Drop` sends the signal
/// without joining (best-effort).
pub struct QuietTimeoutController {
    notify_controller: Arc<NotifyController>,
    timeout: Duration,
    pending: Arc<Mutex<HashMap<String, QuietPendingEntry>>>,
    sender: mpsc::UnboundedSender<QuietCommand>,
    worker_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Debug, Clone, Copy)]
struct QuietPendingEntry {
    last_mutation: Instant,
    kind: MutationKind,
}

impl QuietTimeoutController {
    pub fn new(notify_controller: Arc<NotifyController>, timeout: Duration) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let pending: Arc<Mutex<HashMap<String, QuietPendingEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let worker_pending = pending.clone();
        let worker_notify = notify_controller.clone();
        let worker_timeout = timeout;
        let handle = std::thread::Builder::new()
            .name("skillfs-quiet-timeout".to_string())
            .spawn(move || {
                let rt = match Builder::new_current_thread()
                    .enable_time()
                    .enable_io()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        warn!(
                            error = %e,
                            "failed to build Tokio runtime for quiet-timeout worker"
                        );
                        return;
                    }
                };
                rt.block_on(quiet_worker_loop(
                    worker_pending,
                    worker_notify,
                    worker_timeout,
                    rx,
                ));
            });
        let join_handle = match handle {
            Ok(h) => Some(h),
            Err(e) => {
                warn!(error = %e, "failed to spawn skillfs-quiet-timeout worker thread");
                None
            }
        };
        Arc::new(Self {
            notify_controller,
            timeout,
            pending,
            sender: tx,
            worker_handle: Mutex::new(join_handle),
        })
    }

    /// Record a mutation on a final skill directory. Resets the quiet
    /// window for this skill. Filters out skill-discover, lifecycle
    /// reserved roots, and `.skill-meta/**` paths.
    pub fn observe_skill_mutation(
        &self,
        skill_name: &str,
        relative_path: Option<&std::path::Path>,
        kind: MutationKind,
    ) {
        if !is_quiet_eligible(skill_name) {
            return;
        }
        if let Some(rel) = relative_path {
            if super::path::is_skill_meta_path(rel) {
                return;
            }
        }
        self.pending.lock().insert(
            skill_name.to_string(),
            QuietPendingEntry {
                last_mutation: Instant::now(),
                kind,
            },
        );
        let _ = self.sender.send(QuietCommand::Wakeup);
    }

    /// Cancel any pending quiet window for a skill name.
    pub fn cancel(&self, skill_name: &str) {
        self.pending.lock().remove(skill_name);
    }

    /// Fire entries whose quiet window has actually expired (`now >=
    /// last_mutation + timeout`). Entries that have not yet reached
    /// their deadline are left in place.
    pub fn flush_for_testing(&self) -> usize {
        let now = Instant::now();
        let mut due: Vec<(String, MutationKind)> = Vec::new();
        {
            let mut guard = self.pending.lock();
            let keys: Vec<String> = guard.keys().cloned().collect();
            for key in keys {
                if let Some(entry) = guard.get(&key) {
                    if now >= entry.last_mutation + self.timeout {
                        let kind = entry.kind;
                        guard.remove(&key);
                        due.push((key, kind));
                    }
                }
            }
        }
        let count = due.len();
        for (name, kind) in due {
            self.notify_controller
                .enqueue_immediate(&name, kind, Vec::new());
            debug!(
                skill = %name,
                "quiet-timeout: mutation notification enqueued"
            );
        }
        count
    }

    /// Send shutdown signal and join the worker thread.
    pub fn shutdown(&self) {
        let _ = self.sender.send(QuietCommand::Shutdown);
        if let Some(handle) = self.worker_handle.lock().take() {
            let _ = handle.join();
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Drop for QuietTimeoutController {
    fn drop(&mut self) {
        let _ = self.sender.send(QuietCommand::Shutdown);
    }
}

fn is_quiet_eligible(skill_name: &str) -> bool {
    if skill_name.is_empty() {
        return false;
    }
    if skill_name == "skill-discover" {
        return false;
    }
    if is_reserved_lifecycle_name(skill_name) {
        return false;
    }
    true
}

async fn quiet_worker_loop(
    pending: Arc<Mutex<HashMap<String, QuietPendingEntry>>>,
    notify_controller: Arc<NotifyController>,
    timeout: Duration,
    mut rx: mpsc::UnboundedReceiver<QuietCommand>,
) {
    debug!("quiet-timeout worker starting");
    loop {
        let sleep_for = {
            let guard = pending.lock();
            guard
                .values()
                .map(|entry| {
                    let fire_at = entry.last_mutation + timeout;
                    fire_at.saturating_duration_since(Instant::now())
                })
                .min()
                .unwrap_or(Duration::from_secs(60))
        };

        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(QuietCommand::Wakeup) => {}
                    Some(QuietCommand::Shutdown) | None => {
                        debug!("quiet-timeout worker shutting down");
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(sleep_for) => {}
        }

        let now = Instant::now();
        let mut due: Vec<(String, MutationKind)> = Vec::new();
        {
            let mut guard = pending.lock();
            let keys: Vec<String> = guard.keys().cloned().collect();
            for key in keys {
                if let Some(entry) = guard.get(&key) {
                    if now.duration_since(entry.last_mutation) >= timeout {
                        let kind = entry.kind;
                        guard.remove(&key);
                        due.push((key, kind));
                    }
                }
            }
        }
        for (name, kind) in due {
            notify_controller.enqueue_immediate(&name, kind, Vec::new());
            debug!(
                skill = %name,
                "quiet-timeout: mutation notification enqueued"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// PendingInstallController
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum PendingCommand {
    Wakeup,
    Shutdown,
}

#[derive(Debug, Clone)]
struct PendingInstallEntry {
    last_mutation: Instant,
    kind: MutationKind,
    paths: Vec<String>,
    notified: bool,
}

/// Tracks newly created, not-yet-activated final skill directories during
/// direct-write installation.
///
/// When a new skill directory is created and no activation exists for it,
/// intermediate mutations are captured here instead of flowing through the
/// normal notify/refresh/quiet-timeout path.  After a quiet window expires,
/// the controller checks whether the directory has a complete skill shape
/// (directory exists, `SKILL.md` exists, `SKILL.md` can be parsed by the
/// existing parser).  If complete, one aggregated ordinary mutation
/// notification is emitted.  If incomplete, the entry stays pending and
/// waits for the next mutation to restart the quiet window.
///
/// Pending skills are hidden from `/skills` listing and agent discovery
/// but remain accessible for exact-path access so the installer can
/// continue writing.
pub struct PendingInstallController {
    notify_controller: Arc<NotifyController>,
    timeout: Duration,
    source_root: std::path::PathBuf,
    pending: Arc<Mutex<HashMap<String, PendingInstallEntry>>>,
    sender: mpsc::UnboundedSender<PendingCommand>,
    worker_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    post_publish_controller: Option<Arc<PostPublishGraceController>>,
}

impl PendingInstallController {
    pub fn new(
        notify_controller: Arc<NotifyController>,
        timeout: Duration,
        source_root: std::path::PathBuf,
    ) -> Arc<Self> {
        Self::new_with_post_publish(notify_controller, timeout, source_root, None)
    }

    pub fn new_with_post_publish(
        notify_controller: Arc<NotifyController>,
        timeout: Duration,
        source_root: std::path::PathBuf,
        post_publish_controller: Option<Arc<PostPublishGraceController>>,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let pending: Arc<Mutex<HashMap<String, PendingInstallEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let worker_pending = pending.clone();
        let worker_notify = notify_controller.clone();
        let worker_timeout = timeout;
        let worker_source = source_root.clone();
        let worker_pp = post_publish_controller.clone();
        let handle = std::thread::Builder::new()
            .name("skillfs-pending-install".to_string())
            .spawn(move || {
                let rt = match Builder::new_current_thread()
                    .enable_time()
                    .enable_io()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        warn!(
                            error = %e,
                            "failed to build Tokio runtime for pending-install worker"
                        );
                        return;
                    }
                };
                rt.block_on(pending_install_worker_loop(
                    worker_pending,
                    worker_notify,
                    worker_timeout,
                    worker_source,
                    worker_pp,
                    rx,
                ));
            });
        let join_handle = match handle {
            Ok(h) => Some(h),
            Err(e) => {
                warn!(error = %e, "failed to spawn skillfs-pending-install worker thread");
                None
            }
        };
        Arc::new(Self {
            notify_controller,
            timeout,
            source_root,
            pending,
            sender: tx,
            worker_handle: Mutex::new(join_handle),
            post_publish_controller,
        })
    }

    /// Record a mutation on a pending skill directory.  Resets the quiet
    /// window.  Filters out skill-discover, lifecycle reserved roots,
    /// and `.skill-meta/**` paths.
    ///
    /// Returns `true` when the mutation was absorbed by the pending
    /// controller (the caller should skip normal notify).  Returns
    /// `false` when the mutation was not absorbed — either because the
    /// skill is ineligible, the path is filtered, or the entry has
    /// already been notified and should fall through to the normal path.
    pub fn observe_mutation(
        &self,
        skill_name: &str,
        relative_path: Option<&std::path::Path>,
        kind: MutationKind,
    ) -> bool {
        if !is_quiet_eligible(skill_name) {
            return false;
        }
        if let Some(rel) = relative_path {
            if super::path::is_skill_meta_path(rel) {
                return false;
            }
        }
        let mut guard = self.pending.lock();
        if let Some(entry) = guard.get_mut(skill_name) {
            if entry.notified {
                return false;
            }
            entry.last_mutation = Instant::now();
            entry.kind = kind;
            if let Some(rel) = relative_path {
                let p = rel.to_string_lossy().to_string();
                if !entry.paths.contains(&p) {
                    entry.paths.push(p);
                }
            }
        } else {
            let mut paths = Vec::new();
            if let Some(rel) = relative_path {
                paths.push(rel.to_string_lossy().to_string());
            }
            guard.insert(
                skill_name.to_string(),
                PendingInstallEntry {
                    last_mutation: Instant::now(),
                    kind,
                    paths,
                    notified: false,
                },
            );
        }
        let _ = self.sender.send(PendingCommand::Wakeup);
        true
    }

    /// Returns `true` when `skill_name` is currently tracked as a pending
    /// install.  Used by the visibility layer to hide pending skills from
    /// `/skills` listing while keeping exact-path access open.
    pub fn is_pending(&self, skill_name: &str) -> bool {
        self.pending.lock().contains_key(skill_name)
    }

    /// Cancel any pending window for a skill (e.g. when activation is
    /// written).
    pub fn cancel(&self, skill_name: &str) {
        self.pending.lock().remove(skill_name);
    }

    /// Remove the pending entry for a skill that now has activation.
    /// Called from `observe_mutation` when the active resolver has an
    /// entry for the skill — the pending lifecycle is over.
    pub fn clear_if_activated(&self, skill_name: &str) {
        self.pending.lock().remove(skill_name);
    }

    /// Fire entries whose quiet window has expired and whose skill shape
    /// is complete (directory + parseable `SKILL.md`).  Incomplete entries
    /// are left in place.  Complete entries transition to `notified` state
    /// (kept for exact-path access until activation arrives).
    /// Returns the number of entries that fired.
    pub fn flush_for_testing(&self) -> usize {
        let now = Instant::now();
        let mut due: Vec<(String, MutationKind, Vec<String>)> = Vec::new();
        {
            let mut guard = self.pending.lock();
            let keys: Vec<String> = guard.keys().cloned().collect();
            for key in keys {
                if let Some(entry) = guard.get_mut(&key) {
                    if entry.notified {
                        continue;
                    }
                    if now >= entry.last_mutation + self.timeout
                        && is_skill_complete(&self.source_root, &key)
                    {
                        let kind = entry.kind;
                        let paths = std::mem::take(&mut entry.paths);
                        entry.notified = true;
                        due.push((key, kind, paths));
                    }
                }
            }
        }
        let count = due.len();
        for (name, kind, paths) in &due {
            self.notify_controller
                .enqueue_immediate(name, *kind, paths.clone());
            debug!(
                skill = %name,
                "pending-install: mutation notification enqueued (complete)"
            );
        }
        // I4: start post-publish grace sessions for completed installs.
        if let Some(ref pp) = self.post_publish_controller {
            for (name, _, _) in &due {
                pp.start_session(name, PostPublishSessionKind::DirectInstallComplete);
            }
        }
        count
    }

    /// Send shutdown signal and join the worker thread.
    pub fn shutdown(&self) {
        let _ = self.sender.send(PendingCommand::Shutdown);
        if let Some(handle) = self.worker_handle.lock().take() {
            let _ = handle.join();
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Drop for PendingInstallController {
    fn drop(&mut self) {
        let _ = self.sender.send(PendingCommand::Shutdown);
    }
}

/// Check whether a skill directory has a complete skill shape:
/// - directory exists as a real directory (not a symlink)
/// - `SKILL.md` exists as a real regular file (not a symlink)
/// - `SKILL.md` can be parsed successfully (Ok or Degraded status)
///
/// Uses `symlink_metadata` (no-follow) to prevent external symlinks
/// from triggering a pending install notification.
fn is_skill_complete(source_root: &std::path::Path, skill_name: &str) -> bool {
    let skill_dir = source_root.join(skill_name);
    match std::fs::symlink_metadata(&skill_dir) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return false,
    }
    let skill_md = skill_dir.join("SKILL.md");
    match std::fs::symlink_metadata(&skill_md) {
        Ok(meta) if meta.file_type().is_file() => {}
        _ => return false,
    }
    match skillfs_core::parser::parse_skill_file(&skill_md) {
        Ok(entry) => !entry.parse_status.is_error(),
        Err(_) => false,
    }
}

async fn pending_install_worker_loop(
    pending: Arc<Mutex<HashMap<String, PendingInstallEntry>>>,
    notify_controller: Arc<NotifyController>,
    timeout: Duration,
    source_root: std::path::PathBuf,
    post_publish_controller: Option<Arc<PostPublishGraceController>>,
    mut rx: mpsc::UnboundedReceiver<PendingCommand>,
) {
    debug!("pending-install worker starting");
    loop {
        let sleep_for = {
            let guard = pending.lock();
            guard
                .values()
                .filter(|entry| !entry.notified)
                .map(|entry| {
                    let fire_at = entry.last_mutation + timeout;
                    fire_at.saturating_duration_since(Instant::now())
                })
                .min()
                .unwrap_or(Duration::from_secs(60))
        };

        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(PendingCommand::Wakeup) => {}
                    Some(PendingCommand::Shutdown) | None => {
                        debug!("pending-install worker shutting down");
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(sleep_for) => {}
        }

        let now = Instant::now();
        let mut due: Vec<(String, MutationKind, Vec<String>)> = Vec::new();
        {
            let mut guard = pending.lock();
            let keys: Vec<String> = guard.keys().cloned().collect();
            for key in keys {
                if let Some(entry) = guard.get_mut(&key) {
                    if entry.notified {
                        continue;
                    }
                    if now.duration_since(entry.last_mutation) >= timeout
                        && is_skill_complete(&source_root, &key)
                    {
                        let kind = entry.kind;
                        let paths = std::mem::take(&mut entry.paths);
                        entry.notified = true;
                        due.push((key, kind, paths));
                    }
                }
            }
        }
        for (name, kind, paths) in &due {
            notify_controller.enqueue_immediate(name, *kind, paths.clone());
            debug!(
                skill = %name,
                "pending-install: mutation notification enqueued (complete)"
            );
        }
        // I4: start post-publish grace sessions for completed installs.
        if let Some(ref pp) = post_publish_controller {
            for (name, _, _) in &due {
                pp.start_session(name, PostPublishSessionKind::DirectInstallComplete);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Config parsing helper
// ---------------------------------------------------------------------------

/// Names that must never be matched by a staging pattern because they are
/// sensitive namespaces.
fn is_sensitive_staging_pattern(pattern: &StagingPattern) -> bool {
    let sensitive_exact = [
        SKILL_META_DIR,
        super::inbox::INBOX_DIR_NAME,
        "skill-discover",
    ];
    for name in &sensitive_exact {
        if pattern.matches(name) {
            return true;
        }
    }
    for reserved in super::lifecycle::LIFECYCLE_RESERVED_NAMES {
        if pattern.matches(reserved) {
            return true;
        }
    }
    false
}

pub fn validate_staging_patterns(patterns: &[String]) -> Result<Vec<StagingPattern>, ConfigError> {
    let mut parsed = Vec::with_capacity(patterns.len());
    for raw in patterns {
        let pattern = StagingPattern::parse(raw)?;
        if is_sensitive_staging_pattern(&pattern) {
            return Err(ConfigError::InvalidValue {
                field: "install.staging_patterns",
                value: raw.clone(),
                allowed: "pattern must not match sensitive namespaces \
                          (.skill-meta, .skillfs-inbox, skill-discover, \
                           lifecycle reserved names)",
            });
        }
        parsed.push(pattern);
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// I4: Post-Publish Write Pattern
// ---------------------------------------------------------------------------

/// A validated relative-path pattern for post-publish grace writes.
///
/// Patterns describe paths *inside* a skill directory that the installer
/// is allowed to write after staging rename or pending install completion.
/// Examples: `.openclaw/**` (recursive), `.installer-meta/*` (one level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostPublishWritePattern {
    /// Matches `prefix/**` — the prefix directory and everything below it.
    PrefixRecursive(String),
    /// Matches `prefix/*` — only direct children of the prefix directory.
    PrefixSingleLevel(String),
    /// Matches an exact relative path segment (a single directory or file name).
    Exact(String),
}

impl PostPublishWritePattern {
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        if s.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "install.post_publish_write_patterns",
                value: String::new(),
                allowed: "non-empty pattern",
            });
        }
        if s.starts_with('/') {
            return Err(ConfigError::InvalidValue {
                field: "install.post_publish_write_patterns",
                value: s.to_string(),
                allowed: "pattern must not be an absolute path",
            });
        }
        if s.contains("..") {
            return Err(ConfigError::InvalidValue {
                field: "install.post_publish_write_patterns",
                value: s.to_string(),
                allowed: "pattern must not contain '..'",
            });
        }

        if let Some(prefix) = s.strip_suffix("/**") {
            if prefix.is_empty() {
                return Err(ConfigError::InvalidValue {
                    field: "install.post_publish_write_patterns",
                    value: s.to_string(),
                    allowed: "prefix before '/**' must be non-empty",
                });
            }
            if prefix.contains('*') {
                return Err(ConfigError::InvalidValue {
                    field: "install.post_publish_write_patterns",
                    value: s.to_string(),
                    allowed: "only trailing '/**' or '/*' allowed",
                });
            }
            Ok(PostPublishWritePattern::PrefixRecursive(prefix.to_string()))
        } else if let Some(prefix) = s.strip_suffix("/*") {
            if prefix.is_empty() {
                return Err(ConfigError::InvalidValue {
                    field: "install.post_publish_write_patterns",
                    value: s.to_string(),
                    allowed: "prefix before '/*' must be non-empty",
                });
            }
            if prefix.contains('*') {
                return Err(ConfigError::InvalidValue {
                    field: "install.post_publish_write_patterns",
                    value: s.to_string(),
                    allowed: "only trailing '/**' or '/*' allowed",
                });
            }
            Ok(PostPublishWritePattern::PrefixSingleLevel(
                prefix.to_string(),
            ))
        } else {
            if s.contains('*') {
                return Err(ConfigError::InvalidValue {
                    field: "install.post_publish_write_patterns",
                    value: s.to_string(),
                    allowed: "only trailing '/**' or '/*' allowed",
                });
            }
            Ok(PostPublishWritePattern::Exact(s.to_string()))
        }
    }

    /// Check whether a relative path within a skill directory matches
    /// this pattern.
    pub fn matches(&self, relative_path: &std::path::Path) -> bool {
        let rel_str = relative_path.to_string_lossy();
        match self {
            PostPublishWritePattern::PrefixRecursive(prefix) => {
                // Matches `prefix` itself and `prefix/...`
                rel_str == *prefix || rel_str.starts_with(&format!("{}/", prefix))
            }
            PostPublishWritePattern::PrefixSingleLevel(prefix) => {
                // Matches `prefix/<one-component>` only.
                if let Some(rest) = rel_str.strip_prefix(&format!("{}/", prefix)) {
                    !rest.is_empty() && !rest.contains('/')
                } else {
                    false
                }
            }
            PostPublishWritePattern::Exact(exact) => rel_str == *exact,
        }
    }
}

/// Validate post-publish write patterns, rejecting sensitive namespaces.
pub fn validate_post_publish_patterns(
    patterns: &[String],
) -> Result<Vec<PostPublishWritePattern>, ConfigError> {
    let mut parsed = Vec::with_capacity(patterns.len());
    for raw in patterns {
        let pattern = PostPublishWritePattern::parse(raw)?;
        if is_sensitive_post_publish_pattern(&pattern) {
            return Err(ConfigError::InvalidValue {
                field: "install.post_publish_write_patterns",
                value: raw.clone(),
                allowed: "pattern must not match .skill-meta/**, \
                          lifecycle reserved roots, or skill-discover",
            });
        }
        parsed.push(pattern);
    }
    Ok(parsed)
}

fn is_sensitive_post_publish_pattern(pattern: &PostPublishWritePattern) -> bool {
    let test_path = match pattern {
        PostPublishWritePattern::PrefixRecursive(p)
        | PostPublishWritePattern::PrefixSingleLevel(p) => p.as_str(),
        PostPublishWritePattern::Exact(e) => e.as_str(),
    };
    let first_component = test_path.split('/').next().unwrap_or("");
    if first_component == SKILL_META_DIR {
        return true;
    }
    if first_component == "skill-discover" {
        return true;
    }
    if super::lifecycle::is_reserved_lifecycle_name(first_component) {
        return true;
    }
    if super::inbox::is_inbox_dir_name(first_component) {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// I4: Post-Publish Session + Controller
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostPublishSessionKind {
    StagingRename,
    DirectInstallComplete,
}

#[derive(Debug)]
struct PostPublishSession {
    expires_at: Instant,
    #[allow(dead_code)]
    kind: PostPublishSessionKind,
    last_mutation_at: Instant,
}

/// Manages post-publish grace windows.
///
/// After staging rename or direct-install pending completion, the
/// controller opens a time-limited grace session for the skill.
/// During the grace window, exact-path writes that match the
/// configured patterns are allowed to bypass the active resolver's
/// hidden/fallback read view — writes go directly to the physical
/// source/current directory.
///
/// Default off: only constructed when both `post_publish_grace_ms`
/// and non-empty `post_publish_write_patterns` are configured.
pub struct PostPublishGraceController {
    grace_duration: Duration,
    patterns: Vec<PostPublishWritePattern>,
    sessions: Mutex<HashMap<String, PostPublishSession>>,
}

impl PostPublishGraceController {
    pub fn new(grace_duration: Duration, patterns: Vec<PostPublishWritePattern>) -> Arc<Self> {
        Arc::new(Self {
            grace_duration,
            patterns,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Start a grace session for a skill.
    pub fn start_session(&self, skill_name: &str, kind: PostPublishSessionKind) {
        let now = Instant::now();
        self.sessions.lock().insert(
            skill_name.to_string(),
            PostPublishSession {
                expires_at: now + self.grace_duration,
                kind,
                last_mutation_at: now,
            },
        );
        debug!(
            skill = %skill_name,
            kind = ?kind,
            grace_ms = self.grace_duration.as_millis(),
            "post-publish grace session started"
        );
    }

    /// Check whether a write to `relative_path` within `skill_name`
    /// is allowed by an active grace session.
    ///
    /// Returns `true` only when all of:
    /// 1. A non-expired session exists for the skill.
    /// 2. The path matches at least one configured pattern.
    /// 3. The path does NOT start with `.skill-meta/`.
    pub fn is_grace_allowed(&self, skill_name: &str, relative_path: &std::path::Path) -> bool {
        if super::path::is_skill_meta_path(relative_path) {
            return false;
        }
        let guard = self.sessions.lock();
        let session = match guard.get(skill_name) {
            Some(s) => s,
            None => return false,
        };
        if Instant::now() >= session.expires_at {
            return false;
        }
        self.patterns.iter().any(|p| p.matches(relative_path))
    }

    /// Update the last-mutation timestamp for a skill's grace session.
    pub fn touch_mutation(&self, skill_name: &str) {
        if let Some(session) = self.sessions.lock().get_mut(skill_name) {
            session.last_mutation_at = Instant::now();
        }
    }

    /// Remove expired sessions (lazy cleanup).
    pub fn expire_sessions(&self) {
        let now = Instant::now();
        self.sessions.lock().retain(|_, s| now < s.expires_at);
    }

    /// Returns `true` when a non-expired session exists for the skill,
    /// regardless of path matching. Used to allow skill-dir traversal
    /// so the installer can reach whitelisted files.
    pub fn has_active_session(&self, skill_name: &str) -> bool {
        let guard = self.sessions.lock();
        guard
            .get(skill_name)
            .is_some_and(|s| Instant::now() < s.expires_at)
    }

    pub fn shutdown(&self) {
        self.sessions.lock().clear();
    }

    pub fn grace_duration(&self) -> Duration {
        self.grace_duration
    }

    pub fn patterns(&self) -> &[PostPublishWritePattern] {
        &self.patterns
    }
}

impl std::fmt::Debug for PostPublishGraceController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostPublishGraceController")
            .field("grace_duration", &self.grace_duration)
            .field("patterns", &self.patterns)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // StagingPattern parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_exact_pattern() {
        let p = StagingPattern::parse(".my-staging").unwrap();
        assert_eq!(p, StagingPattern::Exact(".my-staging".to_string()));
    }

    #[test]
    fn parse_prefix_star_pattern() {
        let p = StagingPattern::parse(".openclaw-install-stage-*").unwrap();
        assert_eq!(
            p,
            StagingPattern::PrefixStar(".openclaw-install-stage-".to_string())
        );
    }

    #[test]
    fn parse_empty_pattern_rejected() {
        assert!(StagingPattern::parse("").is_err());
    }

    #[test]
    fn parse_slash_pattern_rejected() {
        assert!(StagingPattern::parse("foo/bar").is_err());
        assert!(StagingPattern::parse("foo/*").is_err());
    }

    #[test]
    fn parse_dotdot_pattern_rejected() {
        assert!(StagingPattern::parse("foo..bar").is_err());
        assert!(StagingPattern::parse("..").is_err());
    }

    #[test]
    fn parse_bare_star_rejected() {
        assert!(StagingPattern::parse("*").is_err());
    }

    #[test]
    fn parse_embedded_star_rejected() {
        assert!(StagingPattern::parse("foo*bar").is_err());
        assert!(StagingPattern::parse("*foo").is_err());
    }

    #[test]
    fn parse_middle_star_in_prefix_star_rejected() {
        assert!(StagingPattern::parse("a*b*").is_err());
    }

    // -----------------------------------------------------------------------
    // StagingPattern matching
    // -----------------------------------------------------------------------

    #[test]
    fn exact_pattern_matches() {
        let p = StagingPattern::Exact(".my-staging".to_string());
        assert!(p.matches(".my-staging"));
        assert!(!p.matches(".my-staging2"));
        assert!(!p.matches("my-staging"));
        assert!(!p.matches(""));
    }

    #[test]
    fn prefix_star_pattern_matches() {
        let p = StagingPattern::PrefixStar(".openclaw-install-stage-".to_string());
        assert!(p.matches(".openclaw-install-stage-foo"));
        assert!(p.matches(".openclaw-install-stage-bar"));
        assert!(p.matches(".openclaw-install-stage-"));
        assert!(!p.matches(".openclaw-install-stag"));
        assert!(!p.matches("openclaw-install-stage-foo"));
        assert!(!p.matches("alpha"));
    }

    // -----------------------------------------------------------------------
    // StagingMatcher
    // -----------------------------------------------------------------------

    #[test]
    fn matcher_with_multiple_patterns() {
        let config = StagingConfig {
            patterns: vec![
                StagingPattern::PrefixStar(".openclaw-install-stage-".to_string()),
                StagingPattern::Exact(".pip-staging".to_string()),
            ],
            ..StagingConfig::default()
        };
        let matcher = StagingMatcher::new(config);
        assert!(matcher.is_staging_root(".openclaw-install-stage-foo"));
        assert!(matcher.is_staging_root(".pip-staging"));
        assert!(!matcher.is_staging_root("alpha"));
        assert!(!matcher.is_staging_root(".staging"));
        assert!(!matcher.is_staging_root(".skill-meta"));
        assert!(!matcher.is_staging_root(".skillfs-inbox"));
    }

    #[test]
    fn matcher_with_no_patterns() {
        let config = StagingConfig::default();
        let matcher = StagingMatcher::new(config);
        assert!(!matcher.is_staging_root(".openclaw-install-stage-foo"));
        assert!(!matcher.is_staging_root("alpha"));
    }

    // -----------------------------------------------------------------------
    // Rename target validation
    // -----------------------------------------------------------------------

    #[test]
    fn valid_rename_target_accepted() {
        let matcher = StagingMatcher::new(StagingConfig {
            patterns: vec![StagingPattern::PrefixStar(
                ".openclaw-install-stage-".to_string(),
            )],
            ..StagingConfig::default()
        });
        assert!(is_valid_staging_rename_target("my-new-skill", &matcher));
        assert!(is_valid_staging_rename_target("weather", &matcher));
    }

    #[test]
    fn rename_to_skill_meta_rejected() {
        let matcher = StagingMatcher::new(StagingConfig::default());
        assert!(!is_valid_staging_rename_target(".skill-meta", &matcher));
    }

    #[test]
    fn rename_to_inbox_rejected() {
        let matcher = StagingMatcher::new(StagingConfig::default());
        assert!(!is_valid_staging_rename_target(".skillfs-inbox", &matcher));
    }

    #[test]
    fn rename_to_skill_discover_rejected() {
        let matcher = StagingMatcher::new(StagingConfig::default());
        assert!(!is_valid_staging_rename_target("skill-discover", &matcher));
    }

    #[test]
    fn rename_to_lifecycle_reserved_rejected() {
        let matcher = StagingMatcher::new(StagingConfig::default());
        assert!(!is_valid_staging_rename_target(".staging", &matcher));
        assert!(!is_valid_staging_rename_target(".certified", &matcher));
        assert!(!is_valid_staging_rename_target(".quarantine", &matcher));
        assert!(!is_valid_staging_rename_target(".archive", &matcher));
    }

    #[test]
    fn rename_to_staging_pattern_rejected() {
        let matcher = StagingMatcher::new(StagingConfig {
            patterns: vec![StagingPattern::PrefixStar(
                ".openclaw-install-stage-".to_string(),
            )],
            ..StagingConfig::default()
        });
        assert!(!is_valid_staging_rename_target(
            ".openclaw-install-stage-foo",
            &matcher
        ));
    }

    #[test]
    fn rename_to_invalid_skill_name_rejected() {
        let matcher = StagingMatcher::new(StagingConfig::default());
        assert!(!is_valid_staging_rename_target("", &matcher));
        assert!(!is_valid_staging_rename_target(".git", &matcher));
        assert!(!is_valid_staging_rename_target("Foo_Bar", &matcher));
        assert!(!is_valid_staging_rename_target("-leading", &matcher));
        assert!(!is_valid_staging_rename_target("trailing-", &matcher));
    }

    // -----------------------------------------------------------------------
    // validate_staging_patterns
    // -----------------------------------------------------------------------

    #[test]
    fn validate_valid_patterns() {
        let patterns = vec![
            ".openclaw-install-stage-*".to_string(),
            ".pip-staging".to_string(),
        ];
        let parsed = validate_staging_patterns(&patterns).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn validate_pattern_matching_skill_meta_rejected() {
        let patterns = vec![".skill-meta".to_string()];
        assert!(validate_staging_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_pattern_matching_inbox_rejected() {
        let patterns = vec![".skillfs-inbox".to_string()];
        assert!(validate_staging_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_pattern_matching_skill_discover_rejected() {
        let patterns = vec!["skill-discover".to_string()];
        assert!(validate_staging_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_pattern_matching_lifecycle_rejected() {
        let patterns = vec![".staging".to_string()];
        assert!(validate_staging_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_prefix_star_catching_lifecycle_rejected() {
        let patterns = vec![".stag*".to_string()];
        assert!(validate_staging_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_empty_patterns_list_ok() {
        let parsed = validate_staging_patterns(&[]).unwrap();
        assert!(parsed.is_empty());
    }

    // -----------------------------------------------------------------------
    // UnactivatedVisibility
    // -----------------------------------------------------------------------

    #[test]
    fn unactivated_visibility_parse() {
        assert_eq!(
            UnactivatedVisibility::parse("hidden"),
            Some(UnactivatedVisibility::Hidden)
        );
        assert_eq!(UnactivatedVisibility::parse("visible"), None);
        assert_eq!(UnactivatedVisibility::parse(""), None);
    }

    #[test]
    fn unactivated_visibility_default_is_hidden() {
        assert_eq!(
            UnactivatedVisibility::default(),
            UnactivatedVisibility::Hidden
        );
    }

    // -----------------------------------------------------------------------
    // is_skill_complete — no-follow symlink checks
    // -----------------------------------------------------------------------

    #[test]
    fn is_skill_complete_real_dir_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("good-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: good-skill\ndescription: test\n---\n",
        )
        .unwrap();
        assert!(
            super::is_skill_complete(dir.path(), "good-skill"),
            "real dir + real SKILL.md must be complete"
        );
    }

    #[test]
    fn is_skill_complete_symlink_dir_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-skill");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(
            real.join("SKILL.md"),
            "---\nname: real-skill\ndescription: test\n---\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link-skill")).unwrap();
        assert!(
            !super::is_skill_complete(dir.path(), "link-skill"),
            "symlink skill dir must not be considered complete"
        );
    }

    #[test]
    fn is_skill_complete_symlink_skill_md_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("sym-md-skill");
        std::fs::create_dir_all(&skill).unwrap();
        let real_md = dir.path().join("external-SKILL.md");
        std::fs::write(
            &real_md,
            "---\nname: sym-md-skill\ndescription: test\n---\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&real_md, skill.join("SKILL.md")).unwrap();
        assert!(
            !super::is_skill_complete(dir.path(), "sym-md-skill"),
            "symlink SKILL.md must not be considered complete"
        );
    }

    #[test]
    fn is_skill_complete_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !super::is_skill_complete(dir.path(), "nonexistent"),
            "missing directory must not be complete"
        );
    }

    #[test]
    fn is_skill_complete_missing_skill_md() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("no-md");
        std::fs::create_dir_all(&skill).unwrap();
        assert!(
            !super::is_skill_complete(dir.path(), "no-md"),
            "missing SKILL.md must not be complete"
        );
    }

    // -----------------------------------------------------------------------
    // QuietTimeoutController
    // -----------------------------------------------------------------------

    use super::super::notify::InMemoryNotifyClient;

    fn make_quiet_controller(
        timeout_ms: u64,
    ) -> (
        Arc<QuietTimeoutController>,
        Arc<InMemoryNotifyClient>,
        Arc<NotifyController>,
    ) {
        let client = Arc::new(InMemoryNotifyClient::new());
        let notify_ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );
        let ctrl =
            QuietTimeoutController::new(notify_ctrl.clone(), Duration::from_millis(timeout_ms));
        (ctrl, client, notify_ctrl)
    }

    #[test]
    fn quiet_multiple_observations_collapse_to_one_notify() {
        let (ctrl, client, notify_ctrl) = make_quiet_controller(100);
        for _ in 0..5 {
            ctrl.observe_skill_mutation("demo-weather", None, MutationKind::Write);
        }
        std::thread::sleep(Duration::from_millis(200));
        ctrl.flush_for_testing();
        notify_ctrl.flush_for_testing();
        let events = client.events();
        let mutation_events: Vec<_> = events
            .iter()
            .filter(|e| e.skill_id == "demo-weather" && e.event_kind == "write")
            .collect();
        assert_eq!(
            mutation_events.len(),
            1,
            "five observations must collapse to one mutation notify, got {:?}",
            events,
        );
        notify_ctrl.shutdown();
        ctrl.shutdown();
    }

    #[test]
    fn quiet_second_round_fires_again() {
        let (ctrl, client, notify_ctrl) = make_quiet_controller(100);
        ctrl.observe_skill_mutation("demo-weather", None, MutationKind::Write);
        std::thread::sleep(Duration::from_millis(200));
        ctrl.flush_for_testing();
        notify_ctrl.flush_for_testing();
        assert_eq!(
            client
                .events()
                .iter()
                .filter(|e| e.event_kind == "write")
                .count(),
            1
        );

        ctrl.observe_skill_mutation("demo-weather", None, MutationKind::Write);
        std::thread::sleep(Duration::from_millis(200));
        ctrl.flush_for_testing();
        notify_ctrl.flush_for_testing();
        assert_eq!(
            client
                .events()
                .iter()
                .filter(|e| e.event_kind == "write")
                .count(),
            2,
            "second round of writes must produce a second mutation notify"
        );
        notify_ctrl.shutdown();
        ctrl.shutdown();
    }

    #[test]
    fn quiet_cancel_prevents_fire() {
        let (ctrl, client, notify_ctrl) = make_quiet_controller(150);
        ctrl.observe_skill_mutation("demo-weather", None, MutationKind::Write);
        std::thread::sleep(Duration::from_millis(50));
        ctrl.cancel("demo-weather");
        std::thread::sleep(Duration::from_millis(200));
        ctrl.flush_for_testing();
        notify_ctrl.flush_for_testing();
        let events = client.events();
        let matching: Vec<_> = events
            .iter()
            .filter(|e| e.skill_id == "demo-weather")
            .collect();
        assert!(
            matching.is_empty(),
            "cancel must prevent mutation notify, got: {:?}",
            matching
        );
        notify_ctrl.shutdown();
        ctrl.shutdown();
    }

    #[test]
    fn quiet_shutdown_does_not_leak_thread() {
        let start = std::time::Instant::now();
        for _ in 0..8 {
            let (ctrl, _client, notify_ctrl) = make_quiet_controller(100);
            ctrl.observe_skill_mutation("demo-weather", None, MutationKind::Write);
            ctrl.shutdown();
            notify_ctrl.shutdown();
            drop(ctrl);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "8 controller create/drop cycles took {:?}; a leaked thread would block",
            elapsed
        );
    }

    #[test]
    fn quiet_filters_skill_discover() {
        let (ctrl, client, notify_ctrl) = make_quiet_controller(100);
        ctrl.observe_skill_mutation("skill-discover", None, MutationKind::Write);
        std::thread::sleep(Duration::from_millis(200));
        ctrl.flush_for_testing();
        notify_ctrl.flush_for_testing();
        assert!(
            client.is_empty(),
            "skill-discover must not trigger quiet timeout"
        );
        notify_ctrl.shutdown();
        ctrl.shutdown();
    }

    #[test]
    fn quiet_filters_lifecycle_reserved() {
        let (ctrl, client, notify_ctrl) = make_quiet_controller(100);
        for name in &[".staging", ".certified", ".quarantine", ".archive"] {
            ctrl.observe_skill_mutation(name, None, MutationKind::Write);
        }
        std::thread::sleep(Duration::from_millis(200));
        ctrl.flush_for_testing();
        notify_ctrl.flush_for_testing();
        assert!(
            client.is_empty(),
            "lifecycle reserved roots must not trigger quiet timeout"
        );
        notify_ctrl.shutdown();
        ctrl.shutdown();
    }

    #[test]
    fn quiet_filters_skill_meta_path() {
        let (ctrl, client, notify_ctrl) = make_quiet_controller(100);
        ctrl.observe_skill_mutation(
            "demo-weather",
            Some(std::path::Path::new(".skill-meta/activation.json")),
            MutationKind::Write,
        );
        std::thread::sleep(Duration::from_millis(200));
        ctrl.flush_for_testing();
        notify_ctrl.flush_for_testing();
        assert!(
            client.is_empty(),
            ".skill-meta/** must not trigger quiet timeout"
        );
        notify_ctrl.shutdown();
        ctrl.shutdown();
    }

    #[test]
    fn quiet_flush_does_not_fire_before_timeout() {
        let (ctrl, client, notify_ctrl) = make_quiet_controller(500);
        ctrl.observe_skill_mutation("demo-weather", None, MutationKind::Write);
        let fired = ctrl.flush_for_testing();
        assert_eq!(fired, 0, "flush must not fire before timeout");
        assert!(client.is_empty());
        notify_ctrl.shutdown();
        ctrl.shutdown();
    }

    // -----------------------------------------------------------------------
    // PostPublishWritePattern parsing
    // -----------------------------------------------------------------------

    #[test]
    fn post_publish_parse_prefix_recursive() {
        let p = PostPublishWritePattern::parse(".openclaw/**").unwrap();
        assert_eq!(
            p,
            PostPublishWritePattern::PrefixRecursive(".openclaw".to_string())
        );
    }

    #[test]
    fn post_publish_parse_prefix_single_level() {
        let p = PostPublishWritePattern::parse(".installer-meta/*").unwrap();
        assert_eq!(
            p,
            PostPublishWritePattern::PrefixSingleLevel(".installer-meta".to_string())
        );
    }

    #[test]
    fn post_publish_parse_exact() {
        let p = PostPublishWritePattern::parse(".openclaw").unwrap();
        assert_eq!(p, PostPublishWritePattern::Exact(".openclaw".to_string()));
    }

    #[test]
    fn post_publish_parse_empty_rejected() {
        assert!(PostPublishWritePattern::parse("").is_err());
    }

    #[test]
    fn post_publish_parse_absolute_path_rejected() {
        assert!(PostPublishWritePattern::parse("/foo/**").is_err());
        assert!(PostPublishWritePattern::parse("/abs").is_err());
    }

    #[test]
    fn post_publish_parse_dotdot_rejected() {
        assert!(PostPublishWritePattern::parse("foo/../bar/**").is_err());
        assert!(PostPublishWritePattern::parse("../**").is_err());
    }

    #[test]
    fn post_publish_parse_bare_recursive_rejected() {
        assert!(PostPublishWritePattern::parse("/**").is_err());
    }

    #[test]
    fn post_publish_parse_bare_star_rejected() {
        assert!(PostPublishWritePattern::parse("/*").is_err());
    }

    #[test]
    fn post_publish_parse_embedded_star_rejected() {
        assert!(PostPublishWritePattern::parse("foo*bar/**").is_err());
        assert!(PostPublishWritePattern::parse("*foo/**").is_err());
    }

    #[test]
    fn post_publish_parse_mid_star_rejected() {
        assert!(PostPublishWritePattern::parse("a/*/b").is_err());
    }

    // -----------------------------------------------------------------------
    // PostPublishWritePattern matching
    // -----------------------------------------------------------------------

    #[test]
    fn post_publish_recursive_matches() {
        let p = PostPublishWritePattern::PrefixRecursive(".openclaw".to_string());
        assert!(p.matches(std::path::Path::new(".openclaw")));
        assert!(p.matches(std::path::Path::new(".openclaw/foo")));
        assert!(p.matches(std::path::Path::new(".openclaw/bar/baz")));
        assert!(p.matches(std::path::Path::new(".openclaw/.fs-safe-replace.tmp")));
        assert!(!p.matches(std::path::Path::new(".other/foo")));
        assert!(!p.matches(std::path::Path::new("openclaw/foo")));
        assert!(!p.matches(std::path::Path::new(".openclawx")));
    }

    #[test]
    fn post_publish_single_level_matches() {
        let p = PostPublishWritePattern::PrefixSingleLevel(".meta".to_string());
        assert!(p.matches(std::path::Path::new(".meta/foo")));
        assert!(p.matches(std::path::Path::new(".meta/bar.txt")));
        assert!(!p.matches(std::path::Path::new(".meta")));
        assert!(!p.matches(std::path::Path::new(".meta/sub/deep")));
        assert!(!p.matches(std::path::Path::new(".other/foo")));
    }

    #[test]
    fn post_publish_exact_matches() {
        let p = PostPublishWritePattern::Exact(".openclaw".to_string());
        assert!(p.matches(std::path::Path::new(".openclaw")));
        assert!(!p.matches(std::path::Path::new(".openclaw/foo")));
        assert!(!p.matches(std::path::Path::new(".other")));
    }

    // -----------------------------------------------------------------------
    // validate_post_publish_patterns
    // -----------------------------------------------------------------------

    #[test]
    fn validate_post_publish_valid_patterns() {
        let patterns = vec![".openclaw/**".to_string(), ".installer-meta/*".to_string()];
        let parsed = validate_post_publish_patterns(&patterns).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn validate_post_publish_skill_meta_rejected() {
        let patterns = vec![".skill-meta/**".to_string()];
        assert!(validate_post_publish_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_post_publish_skill_meta_exact_rejected() {
        let patterns = vec![".skill-meta".to_string()];
        assert!(validate_post_publish_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_post_publish_lifecycle_rejected() {
        for name in &[
            ".staging/**",
            ".certified/**",
            ".quarantine/**",
            ".archive/**",
        ] {
            assert!(
                validate_post_publish_patterns(&[name.to_string()]).is_err(),
                "lifecycle root '{}' must be rejected",
                name
            );
        }
    }

    #[test]
    fn validate_post_publish_skill_discover_rejected() {
        let patterns = vec!["skill-discover/**".to_string()];
        assert!(validate_post_publish_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_post_publish_inbox_rejected() {
        let patterns = vec![".skillfs-inbox/**".to_string()];
        assert!(validate_post_publish_patterns(&patterns).is_err());
    }

    #[test]
    fn validate_post_publish_empty_list_ok() {
        let parsed = validate_post_publish_patterns(&[]).unwrap();
        assert!(parsed.is_empty());
    }

    // -----------------------------------------------------------------------
    // PostPublishGraceController
    // -----------------------------------------------------------------------

    #[test]
    fn grace_session_allows_matching_path() {
        let patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".openclaw".to_string(),
        )];
        let ctrl = PostPublishGraceController::new(Duration::from_millis(500), patterns);
        ctrl.start_session("my-skill", PostPublishSessionKind::StagingRename);
        assert!(ctrl.is_grace_allowed("my-skill", std::path::Path::new(".openclaw/metadata.tmp")));
    }

    #[test]
    fn grace_session_rejects_non_matching_path() {
        let patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".openclaw".to_string(),
        )];
        let ctrl = PostPublishGraceController::new(Duration::from_millis(500), patterns);
        ctrl.start_session("my-skill", PostPublishSessionKind::StagingRename);
        assert!(!ctrl.is_grace_allowed("my-skill", std::path::Path::new("other-dir/file.txt")));
    }

    #[test]
    fn grace_session_rejects_skill_meta() {
        let patterns = vec![
            PostPublishWritePattern::PrefixRecursive(".openclaw".to_string()),
            PostPublishWritePattern::PrefixRecursive(".skill-meta".to_string()),
        ];
        let ctrl = PostPublishGraceController::new(Duration::from_millis(500), patterns);
        ctrl.start_session("my-skill", PostPublishSessionKind::StagingRename);
        assert!(
            !ctrl.is_grace_allowed(
                "my-skill",
                std::path::Path::new(".skill-meta/activation.json")
            ),
            ".skill-meta/** must always be rejected even with matching pattern"
        );
    }

    #[test]
    fn grace_session_rejects_no_session() {
        let patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".openclaw".to_string(),
        )];
        let ctrl = PostPublishGraceController::new(Duration::from_millis(500), patterns);
        assert!(!ctrl.is_grace_allowed("unknown-skill", std::path::Path::new(".openclaw/foo")));
    }

    #[test]
    fn grace_session_expires() {
        let patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".openclaw".to_string(),
        )];
        let ctrl = PostPublishGraceController::new(Duration::from_millis(100), patterns);
        ctrl.start_session("my-skill", PostPublishSessionKind::StagingRename);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !ctrl.is_grace_allowed("my-skill", std::path::Path::new(".openclaw/metadata.tmp")),
            "grace must not allow writes after expiry"
        );
    }

    #[test]
    fn grace_expire_sessions_cleans_up() {
        let patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".openclaw".to_string(),
        )];
        let ctrl = PostPublishGraceController::new(Duration::from_millis(100), patterns);
        ctrl.start_session("a", PostPublishSessionKind::StagingRename);
        ctrl.start_session("b", PostPublishSessionKind::DirectInstallComplete);
        std::thread::sleep(Duration::from_millis(200));
        ctrl.expire_sessions();
        assert!(!ctrl.is_grace_allowed("a", std::path::Path::new(".openclaw/x")));
        assert!(!ctrl.is_grace_allowed("b", std::path::Path::new(".openclaw/x")));
    }

    #[test]
    fn grace_touch_mutation_updates_timestamp() {
        let patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".openclaw".to_string(),
        )];
        let ctrl = PostPublishGraceController::new(Duration::from_millis(500), patterns);
        ctrl.start_session("my-skill", PostPublishSessionKind::StagingRename);
        std::thread::sleep(Duration::from_millis(10));
        ctrl.touch_mutation("my-skill");
        // Just verify it doesn't panic; last_mutation_at is internal.
    }

    #[test]
    fn grace_shutdown_clears_sessions() {
        let patterns = vec![PostPublishWritePattern::PrefixRecursive(
            ".openclaw".to_string(),
        )];
        let ctrl = PostPublishGraceController::new(Duration::from_millis(5000), patterns);
        ctrl.start_session("my-skill", PostPublishSessionKind::StagingRename);
        ctrl.shutdown();
        assert!(!ctrl.is_grace_allowed("my-skill", std::path::Path::new(".openclaw/foo")));
    }
}
