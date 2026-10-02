//! Skill Security audit event stream (Package S2).
//!
//! Provides a JSONL-friendly representation of [`SkillEvent`] and a
//! best-effort file sink that writes one event per line to an append-only
//! file via a background writer thread.
//!
//! Design contract:
//!
//! * Filesystem operations must never fail because audit writing fails.
//!   Emission goes through a bounded `mpsc::sync_channel`; if the channel is
//!   full we drop the event and bump an internal counter rather than blocking
//!   the FUSE callback.
//! * The writer thread owns the file. If a write returns an error we count it
//!   internally and keep going so a transient ENOSPC or EIO does not wedge
//!   the audit pipeline.
//! * The default `SkillFs` sink remains [`super::event::NoopEventSink`].
//!   Callers must opt in by constructing [`JsonlFileAuditSink`] and wiring
//!   it through [`crate::SkillFs::with_event_sink`].
//!
//! Field shape is intentionally fixed and stable. Tests in this module pin
//! the field names; downstream consumers may rely on them.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tracing::warn;

use super::event::{SkillEvent, SkillEventAction, SkillEventKind, SkillEventSink};

/// Default bounded queue capacity used when callers do not specify one.
///
/// Sized so a short FUSE burst (rapid `readdir` + `open` storms) is unlikely
/// to drop events while still keeping memory usage modest.
pub const DEFAULT_AUDIT_QUEUE_CAPACITY: usize = 1024;

/// Configuration for [`JsonlFileAuditSink`].
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// Path to the append-only JSONL audit log. Created if missing.
    pub path: PathBuf,
    /// Maximum number of events buffered in the bounded channel before we
    /// start dropping. Zero is treated as [`DEFAULT_AUDIT_QUEUE_CAPACITY`].
    pub queue_capacity: usize,
}

impl AuditConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            queue_capacity: DEFAULT_AUDIT_QUEUE_CAPACITY,
        }
    }

    pub fn with_queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity;
        self
    }
}

/// Runtime-facing audit configuration.
///
/// Wraps [`AuditConfig`] for callers that want to drive audit logging from a
/// CLI flag or config file: `path = None` means "audit is not configured" and
/// the caller should keep the default [`super::event::NoopEventSink`];
/// `path = Some(...)` means "construct a [`JsonlFileAuditSink`] writing to
/// this file". `queue_capacity = 0` is normalized to
/// [`DEFAULT_AUDIT_QUEUE_CAPACITY`].
///
/// The intended use is:
///
/// ```text
/// runtime_config = AuditRuntimeConfig { path: cli.audit_log, queue_capacity: cli.audit_queue_capacity };
/// match runtime_config.build_sink() {
///     Ok(Some(sink)) => mount_with_security(.., Some(sink), ..),
///     Ok(None)       => mount_with_security(.., None, ..), // default NoopEventSink
///     Err(e)         => return Err(e), // refuse to mount on explicit-but-broken audit
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct AuditRuntimeConfig {
    /// Audit log path. `None` disables audit and keeps the default sink.
    pub path: Option<PathBuf>,
    /// Bounded channel capacity. `0` is treated as
    /// [`DEFAULT_AUDIT_QUEUE_CAPACITY`].
    pub queue_capacity: usize,
}

impl AuditRuntimeConfig {
    /// Audit-disabled config (the default). Equivalent to
    /// `AuditRuntimeConfig::default()`.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Audit-enabled config writing to `path`, with default queue capacity.
    pub fn enabled(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            queue_capacity: 0,
        }
    }

    /// Override the bounded channel capacity. `0` keeps the default.
    pub fn with_queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity;
        self
    }

    /// Whether audit logging is configured.
    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Effective bounded channel capacity, with `0` normalized to the
    /// default.
    pub fn effective_queue_capacity(&self) -> usize {
        if self.queue_capacity == 0 {
            DEFAULT_AUDIT_QUEUE_CAPACITY
        } else {
            self.queue_capacity
        }
    }

    /// Validate that the audit log will not write into the source tree.
    ///
    /// W1 wires the source drift watcher to the same audit sink as
    /// FUSE-side emission. If the audit log file lives inside the
    /// configured source root, every audit write would either
    ///
    /// 1. trigger the watcher and feed back into a `source_changed` event,
    ///    cascading on each line; or
    /// 2. land on top of an actual `<source>/<skill>/SKILL.md`, corrupting
    ///    the manifest the SkillFS layer is meant to protect.
    ///
    /// This helper rejects either failure mode at startup. Disabled
    /// configs (`path = None`) always pass. Enabled configs canonicalize
    /// the audit log target — using the parent + filename when the file
    /// itself does not yet exist — and refuse to proceed if the resolved
    /// path lies under `source_canonical`.
    ///
    /// Resolution failures (audit log parent missing, permission denied)
    /// are treated as a "do not know" signal and are deferred to
    /// [`build_sink`](Self::build_sink), which already surfaces the
    /// underlying [`std::io::Error`] for unwritable paths.
    pub fn validate_audit_path_outside_source(
        &self,
        source_canonical: &Path,
    ) -> Result<(), AuditPathError> {
        let Some(audit_log) = self.path.as_ref() else {
            return Ok(());
        };
        let audit_canonical = match resolve_audit_path(audit_log) {
            Ok(p) => p,
            // Couldn't resolve the audit log location lexically. The
            // ambiguity is genuine (e.g. parent does not exist yet); let
            // build_sink surface the underlying io::Error rather than
            // guess. The inside-source check is intentionally skipped
            // here — false positives would refuse legitimate paths just
            // because their parent could not be canonicalized.
            Err(_) => return Ok(()),
        };
        if audit_canonical.starts_with(source_canonical) {
            return Err(AuditPathError::InsideSource {
                audit_log: audit_log.clone(),
                source_canonical: source_canonical.to_path_buf(),
                audit_canonical,
            });
        }
        Ok(())
    }

    /// Construct an audit sink from the runtime configuration.
    ///
    /// * `Ok(None)` — audit is not configured (`path = None`); the caller
    ///   should keep the default [`super::event::NoopEventSink`].
    /// * `Ok(Some(sink))` — audit is configured and the sink was created
    ///   successfully.
    /// * `Err(e)` — audit was configured but the sink could not be
    ///   constructed (e.g. unwritable path). Callers must surface this as a
    ///   startup/configuration error and refuse to mount; silently
    ///   downgrading would defeat the operator's intent to enable audit
    ///   logging.
    pub fn build_sink(&self) -> std::io::Result<Option<Arc<dyn SkillEventSink>>> {
        let Some(path) = self.path.as_ref() else {
            return Ok(None);
        };
        let sink_config = AuditConfig {
            path: path.clone(),
            queue_capacity: self.effective_queue_capacity(),
        };
        let sink = JsonlFileAuditSink::new(sink_config)?;
        Ok(Some(Arc::new(sink) as Arc<dyn SkillEventSink>))
    }
}

/// Errors returned by
/// [`AuditRuntimeConfig::validate_audit_path_outside_source`].
///
/// All variants are produced **before** any FUSE mount begins, so the CLI
/// can surface them as a startup error and exit non-zero without leaving
/// the operator with a partially-initialized mount or with an audit log
/// that overwrites a SkillFS-managed file.
#[derive(Debug)]
pub enum AuditPathError {
    /// The configured audit log path canonicalizes to a location inside
    /// the SkillFS source root. This is rejected because either the
    /// drift watcher would observe its own writes (creating a
    /// `source_changed` feedback loop on every line) or the audit log
    /// would land on top of a real `SKILL.md` file.
    InsideSource {
        /// User-supplied audit log path (verbatim).
        audit_log: PathBuf,
        /// Canonical source root the path was compared against.
        source_canonical: PathBuf,
        /// Canonical resolution of `audit_log` that landed under
        /// `source_canonical`.
        audit_canonical: PathBuf,
    },
}

impl std::fmt::Display for AuditPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditPathError::InsideSource {
                audit_log,
                source_canonical,
                audit_canonical,
            } => {
                write!(
                    f,
                    "--audit-log path '{}' (canonical '{}') lies inside the SkillFS \
                     source root '{}'. Pick a location outside the source tree so audit \
                     writes cannot trigger the drift watcher or overwrite SKILL.md files.",
                    audit_log.display(),
                    audit_canonical.display(),
                    source_canonical.display(),
                )
            }
        }
    }
}

impl std::error::Error for AuditPathError {}

/// Resolve an audit log target to an absolute, lexically-clean path.
///
/// If the file already exists, [`Path::canonicalize`] is used directly.
/// Otherwise we canonicalize the parent directory and append the
/// filename, so callers can validate paths that don't yet exist on disk
/// (which is the common case — the audit log is created by
/// [`JsonlFileAuditSink::new`]). Returns the underlying [`std::io::Error`]
/// when neither the path nor its parent can be canonicalized.
fn resolve_audit_path(audit_log: &Path) -> std::io::Result<PathBuf> {
    if audit_log.exists() {
        return audit_log.canonicalize();
    }
    let parent = audit_log.parent().unwrap_or_else(|| Path::new("."));
    let parent_canonical = parent.canonicalize()?;
    let file_name = audit_log.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "audit log path has no file name component",
        )
    })?;
    Ok(parent_canonical.join(file_name))
}

/// Stable string mapping for [`SkillEventKind`].
///
/// Field names are lowercase and snake_case so JSONL consumers can match
/// without case-folding. The mapping is exported as a free function so tests
/// (and future schema documentation) can reference it directly.
pub fn event_kind_str(kind: SkillEventKind) -> &'static str {
    match kind {
        SkillEventKind::Open => "open",
        SkillEventKind::Read => "read",
        SkillEventKind::Write => "write",
        SkillEventKind::Create => "create",
        SkillEventKind::Delete => "delete",
        SkillEventKind::Rename => "rename",
        SkillEventKind::Metadata => "metadata",
        SkillEventKind::Readlink => "readlink",
        SkillEventKind::SymlinkAttempt => "symlink_attempt",
        SkillEventKind::HardlinkAttempt => "hardlink_attempt",
        SkillEventKind::PolicyDecision => "policy_decision",
        SkillEventKind::PolicyDenied => "policy_denied",
        SkillEventKind::SourceChanged => "source_changed",
    }
}

/// Stable string mapping for [`SkillEventAction`].
pub fn event_action_str(action: SkillEventAction) -> &'static str {
    match action {
        SkillEventAction::Allowed => "allowed",
        SkillEventAction::Rejected => "rejected",
        SkillEventAction::Failed => "failed",
    }
}

/// Build the JSON object representation of `event` with stable field names.
///
/// Optional fields are omitted entirely when not set; this keeps the JSONL
/// stream compact and unambiguous (`null` is reserved for present-but-null,
/// which we never emit). `ts_unix_ms` is filled in from the system clock at
/// serialization time so callers do not need to set it on each event.
pub fn event_to_json(event: &SkillEvent) -> Value {
    let mut map = Map::new();
    map.insert("ts_unix_ms".into(), Value::from(current_unix_ms()));
    map.insert("kind".into(), Value::from(event_kind_str(event.kind)));
    if let Some(action) = event.action {
        map.insert("action".into(), Value::from(event_action_str(action)));
    }
    if let Some(ref name) = event.skill_name {
        map.insert("skill".into(), Value::from(name.as_str()));
    }
    if let Some(ref rel) = event.relative_path {
        // Use lossy UTF-8 for the JSON form so non-UTF-8 bytes still surface
        // (replaced with U+FFFD) rather than silently dropping the field.
        map.insert(
            "path".into(),
            Value::from(rel.to_string_lossy().into_owned()),
        );
    }
    if let Some(errno) = event.errno {
        map.insert("errno".into(), Value::from(errno));
    }
    if let Some(uid) = event.uid {
        map.insert("uid".into(), Value::from(uid));
    }
    if let Some(gid) = event.gid {
        map.insert("gid".into(), Value::from(gid));
    }
    if let Some(bytes) = event.bytes {
        map.insert("bytes".into(), Value::from(bytes));
    }
    if let Some(ref detail) = event.detail {
        map.insert("detail".into(), Value::from(detail.as_str()));
    }
    Value::Object(map)
}

/// Serialize `event` as a single JSONL line **without** the trailing newline.
///
/// Tests and callers that want the on-disk byte sequence should append `\n`.
pub fn serialize_event_jsonl(event: &SkillEvent) -> String {
    // serde_json::to_string is infallible for plain Value trees so this never
    // panics in practice. If it ever did we fall back to a deterministic
    // placeholder rather than propagating an error to the FUSE thread.
    serde_json::to_string(&event_to_json(event))
        .unwrap_or_else(|_| String::from("{\"kind\":\"unserializable\"}"))
}

/// Best-effort JSONL audit sink.
///
/// Construction opens the destination file in append mode and spawns a
/// dedicated writer thread. `emit` is non-blocking: it runs `try_send` on a
/// bounded channel and, when the channel is full or the writer has died,
/// drops the event and bumps [`JsonlFileAuditSink::dropped_count`]. Sink
/// failures never surface as filesystem errors.
pub struct JsonlFileAuditSink {
    tx: SyncSender<SkillEvent>,
    dropped: Arc<AtomicU64>,
    write_failures: Arc<AtomicU64>,
    written: Arc<AtomicU64>,
    /// Owned join handle so the worker thread is kept alive for the sink's
    /// lifetime. Wrapped in `Option` so `Drop` can take it.
    worker: Option<JoinHandle<()>>,
}

impl JsonlFileAuditSink {
    /// Open the audit log at `config.path` and spawn the background writer.
    pub fn new(config: AuditConfig) -> std::io::Result<Self> {
        let capacity = if config.queue_capacity == 0 {
            DEFAULT_AUDIT_QUEUE_CAPACITY
        } else {
            config.queue_capacity
        };

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config.path)?;

        let (tx, rx) = sync_channel::<SkillEvent>(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let write_failures = Arc::new(AtomicU64::new(0));
        let written = Arc::new(AtomicU64::new(0));

        let write_failures_thread = write_failures.clone();
        let written_thread = written.clone();
        let log_path = config.path.clone();
        let worker = std::thread::Builder::new()
            .name("skillfs-audit".to_string())
            .spawn(move || {
                let mut writer = BufWriter::new(file);
                while let Ok(event) = rx.recv() {
                    let mut line = serialize_event_jsonl(&event);
                    line.push('\n');
                    let res = writer
                        .write_all(line.as_bytes())
                        .and_then(|_| writer.flush());
                    match res {
                        Ok(()) => {
                            written_thread.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            write_failures_thread.fetch_add(1, Ordering::Relaxed);
                            warn!(
                                error = %e,
                                path = %log_path.display(),
                                "skillfs audit: write failed; event dropped"
                            );
                        }
                    }
                }
            })?;

        Ok(Self {
            tx,
            dropped,
            write_failures,
            written,
            worker: Some(worker),
        })
    }

    /// Number of events dropped because the bounded queue was full or the
    /// writer thread had exited.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Number of events the writer thread tried to flush but failed on.
    pub fn write_failure_count(&self) -> u64 {
        self.write_failures.load(Ordering::Relaxed)
    }

    /// Number of events successfully written to the audit log.
    pub fn written_count(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }
}

impl SkillEventSink for JsonlFileAuditSink {
    fn emit(&self, event: &SkillEvent) {
        match self.tx.try_send(event.clone()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for JsonlFileAuditSink {
    fn drop(&mut self) {
        // Best-effort shutdown: detach the writer's `JoinHandle` and let
        // the destructor of `Self::tx` (which runs after this method
        // returns) close the channel. The writer thread will then observe
        // `recv() == Err` and exit on its own; we do **not** synchronously
        // drain or join, so callers that rely on every queued event being
        // flushed before drop completes must arrange for that themselves
        // (e.g. by adding a short sleep before dropping the sink, as the
        // integration test does). The bounded queue + per-event `flush`
        // bound the worst-case un-flushed window to `queue_capacity`
        // events sitting in the channel at the moment of drop.
        let _ = self.worker.take();
    }
}

/// Helper exposed for tests: current Unix epoch in milliseconds.
fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::event::{InMemoryEventSink, SkillEvent};
    use std::io::Read;
    use std::path::Path;
    use std::sync::Arc;

    fn parse_line(line: &str) -> Value {
        serde_json::from_str(line).expect("audit line must be valid JSON")
    }

    #[test]
    fn jsonl_field_names_are_stable() {
        let event = SkillEvent::new(SkillEventKind::PolicyDenied)
            .with_skill_name("alpha")
            .with_relative_path(Path::new(".skill-meta/manifest.json"))
            .with_action(SkillEventAction::Rejected)
            .with_errno(libc::EACCES)
            .with_caller(1000, 1001)
            .with_bytes(42)
            .with_detail("op=Write reason=.skill-meta is read-only");

        let line = serialize_event_jsonl(&event);
        let v = parse_line(&line);
        let obj = v.as_object().expect("top-level must be object");

        // Required field
        assert_eq!(obj["kind"].as_str().unwrap(), "policy_denied");
        // Optional fields must be present when set
        assert_eq!(obj["action"].as_str().unwrap(), "rejected");
        assert_eq!(obj["skill"].as_str().unwrap(), "alpha");
        assert_eq!(obj["path"].as_str().unwrap(), ".skill-meta/manifest.json");
        assert_eq!(obj["errno"].as_i64().unwrap(), libc::EACCES as i64);
        assert_eq!(obj["uid"].as_u64().unwrap(), 1000);
        assert_eq!(obj["gid"].as_u64().unwrap(), 1001);
        assert_eq!(obj["bytes"].as_u64().unwrap(), 42);
        assert_eq!(
            obj["detail"].as_str().unwrap(),
            "op=Write reason=.skill-meta is read-only"
        );
        // ts_unix_ms is always present
        assert!(obj["ts_unix_ms"].is_u64());
    }

    #[test]
    fn jsonl_omits_unset_optional_fields() {
        let event = SkillEvent::new(SkillEventKind::Read);
        let line = serialize_event_jsonl(&event);
        let obj = parse_line(&line);
        let map = obj.as_object().unwrap();
        assert_eq!(map["kind"].as_str().unwrap(), "read");
        assert!(map.contains_key("ts_unix_ms"));
        for omitted in [
            "action", "skill", "path", "errno", "uid", "gid", "bytes", "detail",
        ] {
            assert!(
                !map.contains_key(omitted),
                "expected `{}` to be omitted when unset",
                omitted
            );
        }
    }

    #[test]
    fn event_kind_str_covers_all_variants() {
        // Pin the canonical kind names. Adding a new SkillEventKind without
        // updating this test will fail to compile.
        assert_eq!(event_kind_str(SkillEventKind::Open), "open");
        assert_eq!(event_kind_str(SkillEventKind::Read), "read");
        assert_eq!(event_kind_str(SkillEventKind::Write), "write");
        assert_eq!(event_kind_str(SkillEventKind::Create), "create");
        assert_eq!(event_kind_str(SkillEventKind::Delete), "delete");
        assert_eq!(event_kind_str(SkillEventKind::Rename), "rename");
        assert_eq!(event_kind_str(SkillEventKind::Metadata), "metadata");
        assert_eq!(event_kind_str(SkillEventKind::Readlink), "readlink");
        assert_eq!(
            event_kind_str(SkillEventKind::SymlinkAttempt),
            "symlink_attempt"
        );
        assert_eq!(
            event_kind_str(SkillEventKind::HardlinkAttempt),
            "hardlink_attempt"
        );
        assert_eq!(
            event_kind_str(SkillEventKind::PolicyDecision),
            "policy_decision"
        );
        assert_eq!(
            event_kind_str(SkillEventKind::PolicyDenied),
            "policy_denied"
        );
        assert_eq!(
            event_kind_str(SkillEventKind::SourceChanged),
            "source_changed"
        );
    }

    #[test]
    fn event_action_str_is_stable() {
        assert_eq!(event_action_str(SkillEventAction::Allowed), "allowed");
        assert_eq!(event_action_str(SkillEventAction::Rejected), "rejected");
        assert_eq!(event_action_str(SkillEventAction::Failed), "failed");
    }

    #[test]
    fn jsonl_file_sink_writes_events_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("audit.jsonl");
        let sink = JsonlFileAuditSink::new(AuditConfig::new(&log)).unwrap();

        sink.emit(
            &SkillEvent::new(SkillEventKind::Open)
                .with_skill_name("alpha")
                .with_relative_path(Path::new("scripts/run.sh"))
                .with_action(SkillEventAction::Allowed)
                .with_caller(1000, 1000),
        );
        sink.emit(
            &SkillEvent::new(SkillEventKind::PolicyDenied)
                .with_skill_name("alpha")
                .with_relative_path(Path::new(".skill-meta/manifest.json"))
                .with_action(SkillEventAction::Rejected)
                .with_errno(libc::EACCES)
                .with_caller(1000, 1000),
        );

        // Drain by closing the sink so the writer thread flushes and exits.
        drop(sink);

        // Sleep briefly to allow the worker to flush and exit. The writer
        // already calls `flush` after every write, so this is mostly to
        // guard against scheduler delay; without it the test is still racy
        // by design.
        let mut content = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            content.clear();
            std::fs::File::open(&log)
                .unwrap()
                .read_to_string(&mut content)
                .unwrap();
            if content.lines().count() >= 2 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("audit log did not receive both events; got: {:?}", content);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "expected exactly 2 events; got {:?}", lines);
        let first = parse_line(lines[0]);
        let second = parse_line(lines[1]);
        assert_eq!(first["kind"], "open");
        assert_eq!(first["action"], "allowed");
        assert_eq!(first["skill"], "alpha");
        assert_eq!(first["path"], "scripts/run.sh");
        assert_eq!(second["kind"], "policy_denied");
        assert_eq!(second["errno"].as_i64().unwrap(), libc::EACCES as i64);
    }

    #[test]
    fn jsonl_file_sink_returns_io_error_when_path_unwritable() {
        // A path inside a non-existent directory cannot be created in append
        // mode; the constructor must surface the std::io::Error rather than
        // panic. Filesystem callers can decide whether to fall back to a
        // NoopEventSink when this happens.
        let bogus = std::path::PathBuf::from("/nonexistent/skillfs/audit.jsonl");
        let err = JsonlFileAuditSink::new(AuditConfig::new(&bogus));
        assert!(err.is_err(), "expected creation to fail on unwritable path");
    }

    #[test]
    fn full_queue_increments_dropped_counter_without_panic() {
        // Capacity 1 so the second back-to-back try_send sees Full before the
        // writer can consume. The writer is synchronous-ish but slower than
        // a tight emit loop; we verify the drop path doesn't panic and that
        // *some* events make it through.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("audit-bounded.jsonl");
        let sink = JsonlFileAuditSink::new(AuditConfig::new(&log).with_queue_capacity(1)).unwrap();

        // Burst far more events than the channel can hold to force drops.
        for i in 0..2048 {
            sink.emit(
                &SkillEvent::new(SkillEventKind::Read)
                    .with_skill_name("alpha")
                    .with_bytes(i),
            );
        }
        // Some events will be queued + written, others will be dropped.
        // We don't pin exact numbers (timing-dependent) — we just require
        // dropped_count + written_count + still-in-queue <= total emitted.
        let dropped = sink.dropped_count();
        let written = sink.written_count();
        assert!(
            dropped + written <= 2048,
            "dropped + written must not exceed emitted: dropped={} written={}",
            dropped,
            written
        );
        // At least one drop should occur with capacity=1 + 2048 emits.
        assert!(
            dropped > 0,
            "expected at least one dropped event with capacity=1"
        );
    }

    #[test]
    fn sink_failure_does_not_propagate_to_caller() {
        // The trait contract is that emit() is infallible from the caller's
        // perspective. We exercise it via the typed sink object as well as
        // via a dyn Arc<dyn SkillEventSink> handle to mirror SkillFs usage.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("audit-failure.jsonl");
        let sink: Arc<dyn SkillEventSink> = Arc::new(
            JsonlFileAuditSink::new(AuditConfig::new(&log).with_queue_capacity(1)).unwrap(),
        );
        // Repeated emit() calls must never panic, even when the queue is
        // saturated. NoopEventSink is the canonical no-op; we exercise both
        // here so the test pins both branches of the contract.
        let noop: Arc<dyn SkillEventSink> = Arc::new(crate::security::NoopEventSink);
        for _ in 0..256 {
            sink.emit(&SkillEvent::new(SkillEventKind::Read));
            noop.emit(&SkillEvent::new(SkillEventKind::Read));
        }
    }

    #[test]
    fn in_memory_sink_still_records_for_test_callers() {
        // Ensure the existing test helper behaves identically after audit
        // module is added.
        let sink = InMemoryEventSink::new();
        sink.emit(&SkillEvent::new(SkillEventKind::Open));
        assert_eq!(sink.len(), 1);
    }

    #[test]
    fn runtime_config_default_is_disabled() {
        let cfg = AuditRuntimeConfig::default();
        assert!(!cfg.is_enabled());
        assert!(cfg.path.is_none());
    }

    #[test]
    fn runtime_config_disabled_yields_no_sink() {
        // Default runtime config means audit is not configured. Callers
        // must keep the default NoopEventSink and not write any log file.
        let cfg = AuditRuntimeConfig::default();
        let built = cfg
            .build_sink()
            .expect("disabled runtime config must not error");
        assert!(
            built.is_none(),
            "disabled runtime config must yield Ok(None) so the caller keeps NoopEventSink"
        );
    }

    #[test]
    fn runtime_config_enabled_builds_sink_and_writes_event_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("audit.jsonl");
        let cfg = AuditRuntimeConfig::enabled(&log);
        assert!(cfg.is_enabled());

        let sink = cfg
            .build_sink()
            .expect("explicit enabled config must build a sink")
            .expect("enabled config must yield Some(sink)");

        sink.emit(
            &SkillEvent::new(SkillEventKind::Open)
                .with_skill_name("alpha")
                .with_action(SkillEventAction::Allowed),
        );

        // Drop the typed Arc so the writer thread observes channel close.
        drop(sink);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut content = String::new();
        loop {
            content.clear();
            std::fs::File::open(&log)
                .unwrap()
                .read_to_string(&mut content)
                .unwrap();
            if !content.is_empty() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "audit log did not receive any event through runtime helper; got: {:?}",
                    content
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let line = content.lines().next().expect("at least one line");
        let v = parse_line(line);
        assert_eq!(v["kind"], "open");
        assert_eq!(v["action"], "allowed");
        assert_eq!(v["skill"], "alpha");
    }

    #[test]
    fn runtime_config_invalid_path_returns_io_error() {
        // The CLI/embedder must surface this Err and refuse to mount —
        // silently downgrading to NoopEventSink would defeat the operator's
        // intent.
        let bogus = std::path::PathBuf::from("/nonexistent/skillfs-runtime/audit.jsonl");
        let cfg = AuditRuntimeConfig::enabled(&bogus);
        let err = cfg.build_sink();
        assert!(
            err.is_err(),
            "explicit but unwritable audit path must return Err, got Ok"
        );
    }

    #[test]
    fn runtime_config_zero_capacity_normalizes_to_default() {
        let cfg = AuditRuntimeConfig {
            path: None,
            queue_capacity: 0,
        };
        assert_eq!(cfg.effective_queue_capacity(), DEFAULT_AUDIT_QUEUE_CAPACITY);

        // Missing capacity (default value) is also 0 → default.
        let cfg = AuditRuntimeConfig::default();
        assert_eq!(cfg.queue_capacity, 0);
        assert_eq!(cfg.effective_queue_capacity(), DEFAULT_AUDIT_QUEUE_CAPACITY);

        // And a sink built from it accepts events without dropping the very
        // first one (a true 0-capacity bounded channel would always reject).
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("audit-zero.jsonl");
        let sink = AuditRuntimeConfig::enabled(&log)
            .with_queue_capacity(0)
            .build_sink()
            .expect("zero-capacity runtime config must build a sink")
            .expect("enabled config must yield Some(sink)");
        sink.emit(&SkillEvent::new(SkillEventKind::Read));
        // The sink is best-effort, so we don't assert on dropped count
        // here — the config-level effective_queue_capacity assertion
        // already pins the normalization rule.
        drop(sink);
    }

    #[test]
    fn runtime_config_explicit_capacity_overrides_default() {
        let cfg = AuditRuntimeConfig {
            path: None,
            queue_capacity: 7,
        };
        assert_eq!(cfg.effective_queue_capacity(), 7);

        let cfg = AuditRuntimeConfig::default().with_queue_capacity(13);
        assert_eq!(cfg.effective_queue_capacity(), 13);
    }

    #[test]
    fn validate_audit_path_outside_source_accepts_disabled_config() {
        // Disabled (path = None) is the "audit not configured" case. The
        // CLI passes through to the default NoopEventSink without ever
        // touching an on-disk file, so there is nothing to compare; the
        // helper must always pass.
        let cfg = AuditRuntimeConfig::default();
        let source = tempfile::tempdir().unwrap();
        cfg.validate_audit_path_outside_source(source.path())
            .expect("disabled audit config must always pass the inside-source check");
    }

    #[test]
    fn validate_audit_path_outside_source_accepts_disjoint_directory() {
        let source = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();
        let cfg = AuditRuntimeConfig::enabled(log_dir.path().join("audit.jsonl"));
        cfg.validate_audit_path_outside_source(&source.path().canonicalize().unwrap())
            .expect("disjoint audit log path must pass");
    }

    #[test]
    fn validate_audit_path_outside_source_rejects_audit_path_inside_source_root() {
        // Operator points --audit-log directly inside the SkillFS source
        // root. W1 would observe every audit write and feed it back
        // through the drift watcher; refuse at startup.
        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let inside = source.path().join("audit.jsonl");
        let cfg = AuditRuntimeConfig::enabled(&inside);
        let err = cfg
            .validate_audit_path_outside_source(&source_canonical)
            .expect_err("audit path inside source root must be rejected");
        match err {
            AuditPathError::InsideSource {
                audit_log,
                source_canonical: src,
                audit_canonical,
            } => {
                assert_eq!(audit_log, inside);
                assert_eq!(src, source_canonical);
                // Audit canonical must lexically be under source canonical.
                assert!(
                    audit_canonical.starts_with(&source_canonical),
                    "audit_canonical={:?} should lie under source={:?}",
                    audit_canonical,
                    source_canonical
                );
            }
        }
    }

    #[test]
    fn validate_audit_path_outside_source_rejects_audit_path_replacing_skill_md() {
        // Specifically the "audit log overwrites SKILL.md" failure mode
        // the W1 review called out. The audit log doesn't have to exist
        // yet for the check to fire — we resolve via the parent.
        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let alpha = source.path().join("alpha");
        std::fs::create_dir_all(&alpha).unwrap();
        let target = alpha.join("SKILL.md");

        let cfg = AuditRuntimeConfig::enabled(&target);
        let err = cfg
            .validate_audit_path_outside_source(&source_canonical)
            .expect_err("audit path replacing SKILL.md must be rejected");
        assert!(
            matches!(err, AuditPathError::InsideSource { .. }),
            "expected InsideSource for SKILL.md target, got {err:?}"
        );
    }

    #[test]
    fn validate_audit_path_outside_source_defers_unresolvable_paths_to_build_sink() {
        // When the audit log's parent directory does not exist yet, we
        // cannot reliably determine whether the resolved path lies
        // inside the source. The helper must defer to the existing
        // build_sink io::Error path rather than refuse on a guess.
        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let bogus = std::path::PathBuf::from("/nonexistent/skillfs-w1-audit-path/audit.jsonl");
        let cfg = AuditRuntimeConfig::enabled(&bogus);
        cfg.validate_audit_path_outside_source(&source_canonical)
            .expect("unresolvable parent must defer to build_sink, not be rejected here");
        // Defense in depth: build_sink must still surface the io::Error.
        assert!(cfg.build_sink().is_err());
    }

    #[test]
    fn audit_path_error_message_names_audit_log_and_source() {
        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let inside = source.path().join("audit.jsonl");
        let cfg = AuditRuntimeConfig::enabled(&inside);
        let err = cfg
            .validate_audit_path_outside_source(&source_canonical)
            .expect_err("inside-source must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("--audit-log"), "missing flag in: {msg}");
        assert!(
            msg.contains(&source_canonical.display().to_string()),
            "missing source root in: {msg}"
        );
        assert!(
            msg.contains(&inside.display().to_string()),
            "missing audit log in: {msg}"
        );
    }
}
