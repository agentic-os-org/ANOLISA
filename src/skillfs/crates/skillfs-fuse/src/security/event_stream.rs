//! Security lifecycle event stream.
//!
//! The FUSE mutation paths feed a debounced external-decision pipeline;
//! lifecycle moves are surfaced as `FS Hook | Ledger Action | SkillFS
//! Decision` triples. This module ships the event-emission half:
//! the [`SecurityEvent`] payload, a [`SecurityEventWriter`] trait
//! (separate from [`crate::security::SkillEventSink`]), and three
//! reference implementations:
//!
//! * [`NoopSecurityEventWriter`] — drops every event. The default when
//!   `--events-log` is absent.
//! * [`JsonlSecurityEventWriter`] — best-effort append-only JSONL writer,
//!   one event per line, modeled on [`JsonlFileAuditSink`] but with a
//!   separate schema (no `kind`, no `errno`, no audit-shape fields)
//!   so the production audit JSONL parser cannot accidentally consume it.
//! * [`InMemorySecurityEventWriter`] — captures events in a `Vec` for tests.
//!
//! Failures are best-effort; an event drop never propagates back to a
//! FUSE callback. Field names use camelCase (`fsHook`, `ledgerAction`,
//! `ledgerStatus`, `skillfsDecision`) to mirror the lifecycle-board
//! column headers in `docs/security-ledger-integration-plan.md`.

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

/// Default queue capacity for the JSONL writer when callers don't pick
/// one. Security event bursts are smaller than audit bursts (one event per skill
/// debounce window), so 256 is plenty.
pub const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 256;

/// One security lifecycle event.
///
/// The event is built at the call site (debounce worker) and emitted via
/// [`SecurityEventWriter::emit`]. Field names match the JSONL schema
/// documented in `docs/security-ledger-integration-plan.md` §6.4: the
/// demo UI expects `fsHook`, `ledgerAction`, `ledgerStatus`, and
/// `skillfsDecision` exactly.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecurityEvent {
    /// ISO-8601 UTC timestamp, e.g. `2026-05-28T15:30:12.123Z`. Filled
    /// in by [`SecurityEvent::new`] from the system clock.
    pub time: String,
    /// Skill name being acted on.
    pub skill: String,
    /// Short human label of the FUSE hook that triggered the refresh,
    /// e.g. `write(SKILL.md)`, `mkdir`, `rename`, `unlink`,
    /// `setattr(truncate)`.
    #[serde(rename = "fsHook")]
    pub fs_hook: String,
    /// External-decision pipeline summary, e.g. `scan -> resolve` (the
    /// D1.3.1 happy path), `scan failed`, or
    /// `scan -> resolve failed`.
    #[serde(rename = "ledgerAction")]
    pub ledger_action: String,
    /// Provider status string returned by `resolve`, e.g. `pass`,
    /// `deny`, `error`. `null` when the resolve was not attempted (e.g.
    /// debounce-only event for ignored paths).
    #[serde(rename = "ledgerStatus")]
    pub ledger_status: Option<String>,
    /// SkillFS-side decision label, e.g. `current`, `fallback:v000001`,
    /// `hidden:no certified version yet`. Matches
    /// [`crate::security::ActiveTarget::as_label`].
    #[serde(rename = "skillfsDecision")]
    pub skillfs_decision: String,
    /// Optional human-readable explanation. UI renders verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SecurityEvent {
    /// Build a new event with the system clock filling in `time`.
    pub fn new(
        skill: impl Into<String>,
        fs_hook: impl Into<String>,
        ledger_action: impl Into<String>,
        skillfs_decision: impl Into<String>,
    ) -> Self {
        Self {
            time: current_iso8601(),
            skill: skill.into(),
            fs_hook: fs_hook.into(),
            ledger_action: ledger_action.into(),
            ledger_status: None,
            skillfs_decision: skillfs_decision.into(),
            message: None,
        }
    }

    pub fn with_ledger_status(mut self, status: impl Into<String>) -> Self {
        self.ledger_status = Some(status.into());
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

/// Sink trait for [`SecurityEvent`]. Intentionally separate from
/// [`crate::security::SkillEventSink`] so the event stream never
/// shares a writer thread with the production audit log.
pub trait SecurityEventWriter: Send + Sync {
    /// Emit one event. Implementations MUST be non-blocking and MUST
    /// NOT propagate errors back to FUSE callback threads.
    fn emit(&self, event: &SecurityEvent);
}

/// Convenience alias.
pub type SecurityEventSink = dyn SecurityEventWriter;

/// Drops every event. The default when `--events-log` is absent.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSecurityEventWriter;

impl SecurityEventWriter for NoopSecurityEventWriter {
    fn emit(&self, _event: &SecurityEvent) {}
}

/// Captures events in memory. Tests use this to assert the JSONL
/// payload without touching the filesystem.
#[derive(Debug, Default)]
pub struct InMemorySecurityEventWriter {
    events: Mutex<Vec<SecurityEvent>>,
}

impl InMemorySecurityEventWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<SecurityEvent> {
        self.events.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.events.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().is_empty()
    }
}

impl SecurityEventWriter for InMemorySecurityEventWriter {
    fn emit(&self, event: &SecurityEvent) {
        self.events.lock().push(event.clone());
    }
}

/// Best-effort JSONL security event writer.
///
/// Modeled on [`crate::security::JsonlFileAuditSink`]: the file is
/// opened once at construction time and a dedicated writer thread
/// drains a bounded `mpsc::sync_channel`. `emit` is non-blocking and
/// drops events when the channel is full or the writer thread has
/// exited. This keeps the event stream from ever blocking a FUSE
/// callback or the debounce worker, even on a misbehaving disk.
///
/// The JSONL shape is the camelCase schema defined by [`SecurityEvent`];
/// the audit JSONL parser cannot consume this stream because it has no
/// `kind` or `errno` fields.
pub struct JsonlSecurityEventWriter {
    tx: SyncSender<SecurityEvent>,
    dropped: Arc<AtomicU64>,
    written: Arc<AtomicU64>,
    write_failures: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl JsonlSecurityEventWriter {
    /// Create a writer that appends one JSON line per event to `path`.
    /// The file is created if missing. `queue_capacity = 0` selects
    /// [`DEFAULT_EVENT_QUEUE_CAPACITY`].
    pub fn new(path: impl Into<PathBuf>, queue_capacity: usize) -> std::io::Result<Self> {
        let path = path.into();
        let capacity = if queue_capacity == 0 {
            DEFAULT_EVENT_QUEUE_CAPACITY
        } else {
            queue_capacity
        };

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let (tx, rx) = sync_channel::<SecurityEvent>(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let written = Arc::new(AtomicU64::new(0));
        let write_failures = Arc::new(AtomicU64::new(0));

        let written_thread = written.clone();
        let write_failures_thread = write_failures.clone();
        let log_path = path.clone();
        let worker = std::thread::Builder::new()
            .name("skillfs-events".to_string())
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
                                "skillfs events: write failed; event dropped"
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
            worker: Some(worker),
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

impl SecurityEventWriter for JsonlSecurityEventWriter {
    fn emit(&self, event: &SecurityEvent) {
        match self.tx.try_send(event.clone()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for JsonlSecurityEventWriter {
    fn drop(&mut self) {
        // Detach the worker; the channel close on `tx` drop will cause
        // its `recv()` to return `Err` and the thread to exit.
        let _ = self.worker.take();
    }
}

/// Render a [`SecurityEvent`] as a single JSON line, without the trailing
/// newline. Stable because the field order is defined by the
/// [`SecurityEvent`] struct declaration order (serde uses declaration order
/// for struct serialization). `ledger_status: None` becomes
/// `"ledgerStatus": null` so the demo UI can render the column even
/// when no status is available; `message: None` is omitted entirely
/// because the schema does not require it.
pub fn serialize_event_jsonl(event: &SecurityEvent) -> String {
    serde_json::to_string(event)
        .unwrap_or_else(|_| String::from("{\"skillfsDecision\":\"unserializable\"}"))
}

/// ISO-8601 UTC timestamp with millisecond precision, e.g.
/// `2026-05-28T15:30:12.123Z`. Hand-rolled to avoid pulling in `chrono`
/// just for the event stream.
fn current_iso8601() -> String {
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

/// Convert Unix seconds-since-epoch (UTC) to civil broken-down time.
/// Based on Howard Hinnant's `days_from_civil` paper. Pure integer math
/// so we don't need `chrono` for a one-line ISO timestamp.
fn secs_to_civil(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Days since 1970-01-01 -> year/month/day. Algorithm below treats
    // March as the start of the year so leap day falls at the end.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, hour, minute, second)
}

/// Resolve a events log target the same way [`crate::security::audit::resolve_audit_path`]
/// does for the audit log: canonicalize the file if it exists, otherwise
/// canonicalize the parent and append the filename. Used by the CLI to
/// surface bogus paths at startup instead of waiting for the first FUSE
/// mutation.
pub fn resolve_events_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent_canonical = parent.canonicalize()?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "events path has no file name component",
        )
    })?;
    Ok(parent_canonical.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_field_order_is_stable() {
        let event = SecurityEvent::new(
            "demo-weather",
            "write(SKILL.md)",
            "check -> scan -> resolve",
            "fallback:v000001",
        )
        .with_ledger_status("deny")
        .with_message("Risky update detected; serving last trusted version");
        let line = serialize_event_jsonl(&event);
        // Pin the expected key order — the demo UI consumes the JSONL
        // sequentially and a future code shuffle should fail this test.
        let expected_prefix = "{\"time\":";
        assert!(line.starts_with(expected_prefix), "line={line}");
        // Sanity-check every documented field is present.
        for needle in [
            "\"skill\":\"demo-weather\"",
            "\"fsHook\":\"write(SKILL.md)\"",
            "\"ledgerAction\":\"check -> scan -> resolve\"",
            "\"ledgerStatus\":\"deny\"",
            "\"skillfsDecision\":\"fallback:v000001\"",
            "\"message\":\"Risky update detected; serving last trusted version\"",
        ] {
            assert!(line.contains(needle), "missing {needle:?} in {line}");
        }
        // Parse round-trip should also produce a stable struct.
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["skill"], "demo-weather");
        assert_eq!(parsed["fsHook"], "write(SKILL.md)");
    }

    #[test]
    fn jsonl_omits_message_when_unset_and_keeps_null_status() {
        let event = SecurityEvent::new("demo", "mkdir", "resolve", "hidden:awaiting decision");
        let line = serialize_event_jsonl(&event);
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(
            parsed.get("message").is_none(),
            "message must be omitted when None"
        );
        assert!(
            parsed["ledgerStatus"].is_null(),
            "ledgerStatus must be null when not set"
        );
    }

    #[test]
    fn iso8601_timestamp_shape_is_stable() {
        let s = current_iso8601();
        // YYYY-MM-DDTHH:MM:SS.mmmZ → 24 chars exactly.
        assert_eq!(s.len(), 24, "expected 24-char ISO timestamp, got {s:?}");
        assert_eq!(s.chars().nth(4), Some('-'));
        assert_eq!(s.chars().nth(7), Some('-'));
        assert_eq!(s.chars().nth(10), Some('T'));
        assert_eq!(s.chars().nth(13), Some(':'));
        assert_eq!(s.chars().nth(16), Some(':'));
        assert_eq!(s.chars().nth(19), Some('.'));
        assert!(s.ends_with('Z'));
    }

    #[test]
    fn secs_to_civil_pins_known_values() {
        // 1970-01-01T00:00:00 UTC = 0 — the algorithm's anchor.
        assert_eq!(secs_to_civil(0), (1970, 1, 1, 0, 0, 0));
        // 2000-02-29T12:00:00 UTC = 951_825_600 (leap day round trip).
        assert_eq!(secs_to_civil(951_825_600), (2000, 2, 29, 12, 0, 0));
        // 2026-05-28T15:30:12 UTC = 1_779_982_212.
        assert_eq!(secs_to_civil(1_779_982_212), (2026, 5, 28, 15, 30, 12));
    }

    #[test]
    fn noop_writer_drops_events_silently() {
        let w = NoopSecurityEventWriter;
        for _ in 0..16 {
            w.emit(&SecurityEvent::new("a", "b", "c", "d"));
        }
    }

    #[test]
    fn in_memory_writer_records_events() {
        let w = InMemorySecurityEventWriter::new();
        assert!(w.is_empty());
        w.emit(&SecurityEvent::new("alpha", "write", "resolve", "current"));
        w.emit(
            &SecurityEvent::new("beta", "mkdir", "resolve", "hidden:not yet certified")
                .with_ledger_status("none"),
        );
        assert_eq!(w.len(), 2);
        let events = w.events();
        assert_eq!(events[0].skill, "alpha");
        assert_eq!(events[1].ledger_status.as_deref(), Some("none"));
    }

    #[test]
    fn jsonl_writer_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo-events.jsonl");
        {
            let writer = JsonlSecurityEventWriter::new(&path, 0).expect("open jsonl writer");
            writer.emit(&SecurityEvent::new(
                "alpha",
                "write(SKILL.md)",
                "resolve",
                "current",
            ));
            writer.emit(
                &SecurityEvent::new("beta", "rename", "resolve", "fallback:v000001")
                    .with_ledger_status("deny")
                    .with_message("policy fallback"),
            );
            // Drop the writer to flush.
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        // Wait briefly for the worker to finish flushing pending events.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let body = std::fs::read_to_string(&path).expect("read demo-events file");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "expected two JSONL lines, got {body:?}");
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            assert!(parsed.get("skill").is_some());
            assert!(parsed.get("fsHook").is_some());
            assert!(parsed.get("ledgerAction").is_some());
            assert!(parsed.get("skillfsDecision").is_some());
        }
    }

    #[test]
    fn jsonl_writer_open_failure_surfaces_io_error() {
        let dir = tempfile::tempdir().unwrap();
        // Pointing at the directory itself rather than a file forces the
        // append open to fail, mirroring how the CLI surfaces a bogus
        // operator-supplied path at startup.
        match JsonlSecurityEventWriter::new(dir.path(), 0) {
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
    fn resolve_events_path_uses_parent_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("does-not-exist-yet.jsonl");
        let resolved = resolve_events_path(&target).expect("resolve");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("does-not-exist-yet.jsonl"));
    }
}
