//! N3: SkillFS Protocol Event Log.
//!
//! Append-only JSONL event log for daemon reconcile, observability, and
//! troubleshooting. Independent from the existing audit JSONL
//! ([`super::audit`]) and security event stream ([`super::event_stream`]).
//!
//! The Ledger boundary follows §6 of `SKILL_LEDGER_SKILLFS_INTEGRATION_zh.md`;
//! N3 remains a SkillFS-local diagnostic protocol, not a Ledger identity source:
//!
//! ```json
//! {
//!   "schemaVersion": 1,
//!   "time": "2026-06-11T10:00:00.000Z",
//!   "skillDir": "/path/to/source/tianqi-weather",
//!   "skillName": "tianqi-weather",
//!   "eventKind": "write",
//!   "paths": ["SKILL.md"]
//! }
//! ```
//!
//! A4 adds an optional `reloadOutcome` field, present only when
//! `eventKind` is `"reload"`:
//!
//! ```json
//! {
//!   "schemaVersion": 1,
//!   "time": "2026-06-16T03:00:00.000Z",
//!   "skillDir": "/path/to/source/tianqi-weather",
//!   "skillName": "tianqi-weather",
//!   "eventKind": "reload",
//!   "paths": [],
//!   "reloadOutcome": "activation_updated"
//! }
//! ```
//!
//! Possible `reloadOutcome` values:
//! * `activation_updated` — resolver updated to a new target.
//! * `activation_unchanged` — daemon re-confirmed the same target.
//! * `activation_timeout` — poll timed out; current mapping kept.
//! * `activation_invalid_hidden` — invalid activation; fail-safe hidden.
//!
//! Three implementations:
//!
//! * [`NoopProtocolEventWriter`] — drops every event (default).
//! * [`JsonlProtocolEventWriter`] — best-effort append-only JSONL, one
//!   event per line, dedicated writer thread.
//! * [`InMemoryProtocolEventWriter`] — captures events in a `Vec` for
//!   tests.
//!
//! Write failures only warn; they never affect FUSE errno, notify
//! dispatch, or the active resolver.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;
use tracing::warn;

pub const PROTOCOL_EVENT_SCHEMA_VERSION: u64 = 1;
pub const DEFAULT_PROTOCOL_EVENT_QUEUE_CAPACITY: usize = 256;

// ---------------------------------------------------------------------------
// ProtocolEvent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolEvent {
    pub schema_version: u64,
    pub time: String,
    pub skill_dir: String,
    pub skill_name: String,
    pub event_kind: String,
    pub paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reload_outcome: Option<String>,
}

impl ProtocolEvent {
    pub fn new(
        skill_dir: impl Into<String>,
        skill_name: impl Into<String>,
        event_kind: impl Into<String>,
        paths: Vec<String>,
    ) -> Self {
        Self {
            schema_version: PROTOCOL_EVENT_SCHEMA_VERSION,
            time: current_rfc3339_utc(),
            skill_dir: skill_dir.into(),
            skill_name: skill_name.into(),
            event_kind: event_kind.into(),
            paths,
            reload_outcome: None,
        }
    }

    pub fn with_reload_outcome(
        skill_dir: impl Into<String>,
        skill_name: impl Into<String>,
        outcome: &str,
    ) -> Self {
        Self {
            schema_version: PROTOCOL_EVENT_SCHEMA_VERSION,
            time: current_rfc3339_utc(),
            skill_dir: skill_dir.into(),
            skill_name: skill_name.into(),
            event_kind: "reload".to_string(),
            paths: Vec::new(),
            reload_outcome: Some(outcome.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// ProtocolEventWriter trait + implementations
// ---------------------------------------------------------------------------

pub trait ProtocolEventWriter: Send + Sync {
    fn emit(&self, event: &ProtocolEvent);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopProtocolEventWriter;

impl ProtocolEventWriter for NoopProtocolEventWriter {
    fn emit(&self, _event: &ProtocolEvent) {}
}

#[derive(Debug, Default)]
pub struct InMemoryProtocolEventWriter {
    events: Mutex<Vec<ProtocolEvent>>,
}

impl InMemoryProtocolEventWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<ProtocolEvent> {
        self.events.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.events.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().is_empty()
    }
}

impl ProtocolEventWriter for InMemoryProtocolEventWriter {
    fn emit(&self, event: &ProtocolEvent) {
        self.events.lock().push(event.clone());
    }
}

pub struct JsonlProtocolEventWriter {
    tx: SyncSender<ProtocolEvent>,
    dropped: Arc<AtomicU64>,
    written: Arc<AtomicU64>,
    write_failures: Arc<AtomicU64>,
    _worker: Option<JoinHandle<()>>,
}

impl JsonlProtocolEventWriter {
    pub fn new(path: impl Into<PathBuf>, queue_capacity: usize) -> std::io::Result<Self> {
        let path = path.into();
        let capacity = if queue_capacity == 0 {
            DEFAULT_PROTOCOL_EVENT_QUEUE_CAPACITY
        } else {
            queue_capacity
        };

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let (tx, rx) = sync_channel::<ProtocolEvent>(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let written = Arc::new(AtomicU64::new(0));
        let write_failures = Arc::new(AtomicU64::new(0));

        let written_thread = written.clone();
        let write_failures_thread = write_failures.clone();
        let log_path = path.clone();
        let worker = std::thread::Builder::new()
            .name("skillfs-protocol-events".to_string())
            .spawn(move || {
                let mut writer = BufWriter::new(file);
                while let Ok(event) = rx.recv() {
                    let mut line = serialize_protocol_event(&event);
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
                                "protocol events: write failed; event dropped"
                            );
                        }
                    }
                }
            })?;

        Ok(Self {
            tx,
            dropped,
            written,
            write_failures,
            _worker: Some(worker),
        })
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn written_count(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    pub fn write_failure_count(&self) -> u64 {
        self.write_failures.load(Ordering::Relaxed)
    }
}

impl ProtocolEventWriter for JsonlProtocolEventWriter {
    fn emit(&self, event: &ProtocolEvent) {
        match self.tx.try_send(event.clone()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for JsonlProtocolEventWriter {
    fn drop(&mut self) {
        let _ = self._worker.take();
    }
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

pub fn serialize_protocol_event(event: &ProtocolEvent) -> String {
    serde_json::to_string(event)
        .unwrap_or_else(|_| String::from("{\"schemaVersion\":1,\"error\":\"unserializable\"}"))
}

/// Resolve a protocol events log path: canonicalize the file if it exists,
/// otherwise canonicalize the parent and append the filename.
pub fn resolve_protocol_events_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent_canonical = parent.canonicalize()?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "protocol events path has no file name component",
        )
    })?;
    Ok(parent_canonical.join(file_name))
}

// ---------------------------------------------------------------------------
// Source-tree guard
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ProtocolEventsPathError {
    InsideSource {
        log_path: PathBuf,
        source_canonical: PathBuf,
        log_canonical: PathBuf,
    },
}

impl std::fmt::Display for ProtocolEventsPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsideSource {
                log_path,
                source_canonical,
                log_canonical,
            } => {
                write!(
                    f,
                    "--activation-events-log path '{}' (canonical '{}') lies inside the \
                     SkillFS source root '{}'. Pick a location outside the source tree so \
                     event log writes cannot pollute skill workspaces or trigger scan noise.",
                    log_path.display(),
                    log_canonical.display(),
                    source_canonical.display(),
                )
            }
        }
    }
}

impl std::error::Error for ProtocolEventsPathError {}

/// Reject a protocol events log path that resolves inside the source tree.
///
/// Mirrors [`super::audit::AuditRuntimeConfig::validate_audit_path_outside_source`]:
/// the log must not land in the skill workspace where daemon scan/reconcile
/// would pick it up as skill content. Resolution failures are deferred to
/// the file-open step.
pub fn validate_protocol_events_path_outside_source(
    log_path: &Path,
    source_canonical: &Path,
) -> Result<(), ProtocolEventsPathError> {
    let log_canonical = match resolve_protocol_events_path(log_path) {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };
    if log_canonical.starts_with(source_canonical) {
        return Err(ProtocolEventsPathError::InsideSource {
            log_path: log_path.to_path_buf(),
            source_canonical: source_canonical.to_path_buf(),
            log_canonical,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Timestamp
// ---------------------------------------------------------------------------

fn current_rfc3339_utc() -> String {
    let now = SystemTime::now();
    let epoch = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => std::time::Duration::from_secs(0),
    };
    let total_secs = epoch.as_secs() as i64;
    let millis = epoch.subsec_millis();
    let (year, month, day, hour, minute, second) = secs_to_civil(total_secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, minute, second, millis
    )
}

fn secs_to_civil(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, hour, minute, second)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_event_json_shape() {
        let event = ProtocolEvent::new(
            "/srv/skills/tianqi-weather",
            "tianqi-weather",
            "write",
            vec!["SKILL.md".to_string()],
        );
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["schemaVersion"], 1);
        assert!(json["time"].as_str().unwrap().ends_with('Z'));
        assert_eq!(json["skillDir"], "/srv/skills/tianqi-weather");
        assert_eq!(json["skillName"], "tianqi-weather");
        assert_eq!(json["eventKind"], "write");
        assert_eq!(json["paths"], serde_json::json!(["SKILL.md"]));
    }

    #[test]
    fn protocol_event_without_reload_outcome_omits_field() {
        let event = ProtocolEvent::new("/a", "a", "write", vec![]);
        let line = serialize_protocol_event(&event);
        assert!(
            !line.contains("reloadOutcome"),
            "reload_outcome=None must not appear in JSON: {line}"
        );
    }

    #[test]
    fn protocol_event_with_reload_outcome_includes_field() {
        let event = ProtocolEvent::with_reload_outcome("/a/alpha", "alpha", "activation_updated");
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["eventKind"], "reload");
        assert_eq!(json["reloadOutcome"], "activation_updated");
        assert_eq!(json["paths"], serde_json::json!([]));
        assert_eq!(json["schemaVersion"], 1);
        assert!(json["time"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn protocol_event_reload_outcome_all_labels() {
        for label in &[
            "activation_updated",
            "activation_unchanged",
            "activation_timeout",
            "activation_invalid_hidden",
        ] {
            let event = ProtocolEvent::with_reload_outcome("/a/alpha", "alpha", label);
            assert_eq!(event.reload_outcome.as_deref(), Some(*label));
            let line = serialize_protocol_event(&event);
            assert!(
                line.contains(label),
                "label {label} must appear in serialized event: {line}"
            );
        }
    }

    #[test]
    fn protocol_event_json_field_names_are_camel_case() {
        let event = ProtocolEvent::new("/a", "a", "mkdir", vec![]);
        let line = serialize_protocol_event(&event);
        assert!(line.contains("\"schemaVersion\""), "line={line}");
        assert!(line.contains("\"skillDir\""), "line={line}");
        assert!(line.contains("\"skillName\""), "line={line}");
        assert!(line.contains("\"eventKind\""), "line={line}");
        assert!(
            !line.contains("\"schema_version\""),
            "snake_case leaked: {line}"
        );
    }

    #[test]
    fn protocol_event_time_is_rfc3339_utc() {
        let event = ProtocolEvent::new("/a", "a", "write", vec![]);
        let t = &event.time;
        assert_eq!(t.len(), 24, "expected 24-char RFC3339 timestamp, got {t:?}");
        assert_eq!(t.chars().nth(4), Some('-'));
        assert_eq!(t.chars().nth(7), Some('-'));
        assert_eq!(t.chars().nth(10), Some('T'));
        assert_eq!(t.chars().nth(13), Some(':'));
        assert_eq!(t.chars().nth(16), Some(':'));
        assert_eq!(t.chars().nth(19), Some('.'));
        assert!(t.ends_with('Z'));
    }

    #[test]
    fn protocol_event_empty_paths() {
        let event = ProtocolEvent::new("/a", "a", "mkdir", vec![]);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["paths"], serde_json::json!([]));
    }

    #[test]
    fn protocol_event_multiple_paths() {
        let event = ProtocolEvent::new(
            "/a",
            "a",
            "write",
            vec!["SKILL.md".to_string(), "scripts/run.sh".to_string()],
        );
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json["paths"],
            serde_json::json!(["SKILL.md", "scripts/run.sh"])
        );
    }

    #[test]
    fn noop_writer_drops_events_silently() {
        let w = NoopProtocolEventWriter;
        for _ in 0..16 {
            w.emit(&ProtocolEvent::new("/a", "a", "write", vec![]));
        }
    }

    #[test]
    fn in_memory_writer_records_events() {
        let w = InMemoryProtocolEventWriter::new();
        assert!(w.is_empty());
        w.emit(&ProtocolEvent::new(
            "/a/alpha",
            "alpha",
            "write",
            vec!["SKILL.md".to_string()],
        ));
        w.emit(&ProtocolEvent::new("/a/beta", "beta", "mkdir", vec![]));
        assert_eq!(w.len(), 2);
        let events = w.events();
        assert_eq!(events[0].skill_name, "alpha");
        assert_eq!(events[0].event_kind, "write");
        assert_eq!(events[0].paths, vec!["SKILL.md"]);
        assert_eq!(events[1].skill_name, "beta");
        assert_eq!(events[1].event_kind, "mkdir");
        assert!(events[1].paths.is_empty());
    }

    #[test]
    fn jsonl_writer_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("protocol-events.jsonl");
        {
            let writer = JsonlProtocolEventWriter::new(&path, 0).expect("open writer");
            writer.emit(&ProtocolEvent::new(
                "/a/alpha",
                "alpha",
                "write",
                vec!["SKILL.md".to_string()],
            ));
            writer.emit(&ProtocolEvent::new(
                "/a/beta",
                "beta",
                "rename",
                vec!["old.txt".to_string(), "new.txt".to_string()],
            ));
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let body = std::fs::read_to_string(&path).expect("read protocol events");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "expected two JSONL lines, got {body:?}");
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            assert_eq!(parsed["schemaVersion"], 1);
            assert!(parsed["time"].as_str().unwrap().ends_with('Z'));
            assert!(parsed.get("skillDir").is_some());
            assert!(parsed.get("skillName").is_some());
            assert!(parsed.get("eventKind").is_some());
            assert!(parsed.get("paths").is_some());
        }
    }

    #[test]
    fn jsonl_writer_open_failure_surfaces_io_error() {
        let dir = tempfile::tempdir().unwrap();
        match JsonlProtocolEventWriter::new(dir.path(), 0) {
            Ok(_) => panic!("expected open to fail when target is a directory"),
            Err(err) => {
                assert!(
                    err.kind() == std::io::ErrorKind::IsADirectory
                        || err.raw_os_error() == Some(libc::EISDIR),
                    "expected EISDIR-ish, got {err:?}"
                );
            }
        }
    }

    #[test]
    fn resolve_protocol_events_path_uses_parent_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("does-not-exist-yet.jsonl");
        let resolved = resolve_protocol_events_path(&target).expect("resolve");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("does-not-exist-yet.jsonl"));
    }

    #[test]
    fn secs_to_civil_pins_known_values() {
        assert_eq!(secs_to_civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(secs_to_civil(951_825_600), (2000, 2, 29, 12, 0, 0));
        assert_eq!(secs_to_civil(1_779_982_212), (2026, 5, 28, 15, 30, 12));
    }

    #[test]
    fn validate_path_outside_source_accepts_disjoint_directory() {
        let source = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();
        let log_path = log_dir.path().join("events.jsonl");
        let source_canonical = source.path().canonicalize().unwrap();
        assert!(validate_protocol_events_path_outside_source(&log_path, &source_canonical).is_ok());
    }

    #[test]
    fn validate_path_outside_source_rejects_path_inside_source() {
        let source = tempfile::tempdir().unwrap();
        let log_path = source.path().join("alpha/events.jsonl");
        std::fs::create_dir_all(source.path().join("alpha")).unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let result = validate_protocol_events_path_outside_source(&log_path, &source_canonical);
        assert!(
            matches!(result, Err(ProtocolEventsPathError::InsideSource { .. })),
            "path inside source must be rejected: {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("--activation-events-log"),
            "error message should name the flag: {err}"
        );
    }

    #[test]
    fn validate_path_outside_source_defers_unresolvable_paths() {
        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let log_path = PathBuf::from("/nonexistent-parent-9999/events.jsonl");
        assert!(
            validate_protocol_events_path_outside_source(&log_path, &source_canonical).is_ok(),
            "unresolvable paths should defer to file-open step"
        );
    }
}
