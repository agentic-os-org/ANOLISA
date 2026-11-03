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
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;
use tracing::warn;

use crate::sys::openat_leaf;

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

/// Open the events-log target the containment guard approved without letting a
/// later rename or symlink swap move it.
///
/// `O_NOFOLLOW` on a plain `open` guards the final component only, and the
/// guard's answer is a *path*: every component of it that the operator can
/// write to may be renamed away and replaced with a link into the source tree
/// while the daemon is starting. The path itself never changes, so a plain
/// `open` would follow the replacement and create the log inside the
/// workspace — the pollution `resolve_events_path_outside_source` exists to
/// prevent, reached through the parent instead of the leaf.
///
/// Descending the parent chain with
/// `openat(O_PATH | O_NOFOLLOW | O_DIRECTORY)` pins each component to the
/// directory that was there when it was opened and fails with
/// `ELOOP`/`ENOTDIR` on a symlinked replacement, so the file that gets written
/// is the file that was approved. The leaf is then created relative to that
/// pinned directory, and the fd itself is checked for an alias the path could
/// not show — see [`refuse_aliased_target`].
///
/// The directory components are opened as path anchors rather than for
/// reading: `O_PATH` asks for no permission on the directory itself, so a
/// parent the operator may search and write but not list (`0300`, a legal way
/// to keep a log directory unlistable) stays usable, while
/// `O_DIRECTORY | O_NOFOLLOW` keeps refusing a symlinked component. The final
/// open is where the permissions that matter are checked: creating the leaf
/// needs write and search on this directory, which is exactly what appending
/// to the log requires.
///
/// Relative paths are refused: the guard resolves the flag against the process
/// cwd, so accepting one here would put an unchecked component (`cwd`) back
/// into the path after validation.
fn open_events_log(path: &Path) -> std::io::Result<std::fs::File> {
    let parent = path
        .parent()
        .filter(|parent| parent.is_absolute())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "events log path must be absolute so every component can be checked",
            )
        })?;
    let leaf = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "events log path has no file name component",
        )
    })?;

    let mut dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open("/")?;
    for component in parent.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => {
                dir = openat_leaf(
                    &dir,
                    name,
                    libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                    0,
                )?;
            }
            // The path was canonicalized before it was approved, so neither of
            // these can appear; resolving one here would step over a component
            // nobody checked.
            other => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("events log path component {other:?} is not a plain directory name"),
                ));
            }
        }
    }

    let file = openat_leaf(
        &dir,
        leaf,
        libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_NOFOLLOW,
        0o666,
    )?;
    refuse_aliased_target(&file, path)?;
    Ok(file)
}

/// Refuse a target another name reaches too.
///
/// The containment guard compares canonical paths, and a hard link is one
/// inode with two names: an external name for `<source>/SKILL.md` canonicalizes
/// outside the source, so the guard approves it, and `O_NOFOLLOW` is no help
/// either — the kernel only resolves *symlinks*, and a hard link is an ordinary
/// regular file. Appending events through that name would rewrite the source
/// file the mount exists to protect.
///
/// The alias is invisible to the path and visible only on the object, so the
/// link count is read from the fd just opened: any name inside the source can
/// only be a further link to this inode, so a count above one means the log may
/// be a file inside the tree, and a freshly created or singly linked log is
/// exactly the case that is provably disjoint. Failing closed here rejects the
/// aliased target before the writer thread exists, so not one event is written
/// through it.
fn refuse_aliased_target(file: &std::fs::File, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let links = file.metadata()?.nlink();
    if links > 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing to open events log '{}': it has {links} hard links, so a file inside \
                 the source tree could be this same inode and would receive these appends; \
                 only a singly linked target is proven disjoint from the source",
                path.display(),
            ),
        ));
    }
    Ok(())
}

impl JsonlSecurityEventWriter {
    /// Create a writer that appends one JSON line per event to `path`.
    /// The file is created if missing. `queue_capacity = 0` selects
    /// [`DEFAULT_EVENT_QUEUE_CAPACITY`].
    ///
    /// `path` is expected to be the value `resolve_events_path_outside_source`
    /// approved; it is opened with [`open_events_log`], which refuses to follow
    /// a component swapped in after that check and to open a target whose inode
    /// a second name — such as a file inside the source tree — also reaches.
    pub fn new(path: impl Into<PathBuf>, queue_capacity: usize) -> std::io::Result<Self> {
        let path = path.into();
        let capacity = if queue_capacity == 0 {
            DEFAULT_EVENT_QUEUE_CAPACITY
        } else {
            queue_capacity
        };

        let file = open_events_log(&path)?;

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

/// Resolve an events log target to the path a later `open` would act on:
/// follow every symlinked final component (bounded, like the kernel) and
/// canonicalize the rest. Used by the CLI to surface bogus paths at
/// startup instead of waiting for the first FUSE mutation, and to open the
/// exact object validation approved — a dangling symlink is followed here
/// too, because `open(O_CREAT)` would follow it and create its target.
pub fn resolve_events_path(path: &Path) -> std::io::Result<PathBuf> {
    // A relative path is opened against the process cwd, so resolve it the
    // same way here: `parent()` of a bare filename is the empty path, whose
    // canonicalize() fails with ENOENT and would make the containment guard
    // defer to the open — writing the log inside the source tree.
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = path;
    for _ in 0..32 {
        match std::fs::symlink_metadata(&current) {
            // A real entry ends the chain: canonicalize still resolves any
            // symlinked parent components.
            Ok(meta) if !meta.file_type().is_symlink() => return current.canonicalize(),
            // A symlink would be followed by the later open, including a
            // dangling one whose target the open would create — keep
            // resolving so validation sees the real destination.
            Ok(_) => {
                let target = std::fs::read_link(&current)?;
                current = if target.is_absolute() {
                    target
                } else {
                    current
                        .parent()
                        .unwrap_or_else(|| Path::new("/"))
                        .join(target)
                };
            }
            // Nothing exists at the final component and its parent is
            // nameable: open(O_CREAT) would create the file right here.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let parent = current.parent().unwrap_or_else(|| Path::new("."));
                let parent_canonical = parent.canonicalize()?;
                let file_name = current.file_name().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "events path has no file name component",
                    )
                })?;
                return Ok(parent_canonical.join(file_name));
            }
            Err(e) => return Err(e),
        }
    }
    // ELOOP: the same errno open(2) answers for a symlink loop; also
    // covers chains deeper than the kernel's own 40-hop limit.
    Err(std::io::Error::from_raw_os_error(libc::ELOOP))
}

/// Error returned for an `--events-log` target the mount must not open.
#[derive(Debug)]
pub enum EventsPathError {
    /// The target could not be resolved, so no object can be approved for the
    /// writer to open. `open(O_CREAT)` cannot create a missing parent either,
    /// so this is a startup error rather than something to retry later.
    Unresolvable { log_path: PathBuf, reason: String },
    InsideSource {
        log_path: PathBuf,
        source_canonical: PathBuf,
        log_canonical: PathBuf,
    },
}

impl std::fmt::Display for EventsPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolvable { log_path, reason } => {
                write!(
                    f,
                    "--events-log path '{}' cannot be resolved: {reason}",
                    log_path.display(),
                )
            }
            Self::InsideSource {
                log_path,
                source_canonical,
                log_canonical,
            } => {
                write!(
                    f,
                    "--events-log path '{}' (canonical '{}') lies inside the \
                     SkillFS source root '{}'. Pick a location outside the source tree so event log writes cannot pollute skill workspaces or trigger scan noise.",
                    log_path.display(),
                    log_canonical.display(),
                    source_canonical.display(),
                )
            }
        }
    }
}

impl std::error::Error for EventsPathError {}

/// Resolve an events log target and reject it when it lands inside the source
/// tree, returning the resolved path for the caller to open.
///
/// Mirrors `audit::AuditRuntimeConfig::validate_audit_path_outside_source` and
/// `protocol_events::validate_protocol_events_path_outside_source`: the log
/// must not land in the skill workspace, where every write would be observed
/// by the drift watcher (self-feeding scan loop) or could overwrite a
/// `SKILL.md`. The returned path is the object the guard approved, so the
/// writer must open it instead of resolving the raw flag again: a symlink
/// swapped in after this check would otherwise aim the open at a path nobody
/// validated, and `O_NOFOLLOW` only guards the final component of the path it
/// is handed.
pub fn resolve_events_path_outside_source(
    log_path: &Path,
    source_canonical: &Path,
) -> Result<PathBuf, EventsPathError> {
    let log_canonical =
        resolve_events_path(log_path).map_err(|e| EventsPathError::Unresolvable {
            log_path: log_path.to_path_buf(),
            reason: e.to_string(),
        })?;
    if log_canonical.starts_with(source_canonical) {
        return Err(EventsPathError::InsideSource {
            log_path: log_path.to_path_buf(),
            source_canonical: source_canonical.to_path_buf(),
            log_canonical,
        });
    }
    Ok(log_canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_log_inside_source_is_rejected() {
        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let alpha = source.path().join("alpha");
        std::fs::create_dir_all(&alpha).unwrap();
        let inside = alpha.join("events.jsonl");
        let err = resolve_events_path_outside_source(&inside, &source_canonical)
            .expect_err("events log inside the source root must be rejected");
        match err {
            EventsPathError::InsideSource {
                log_path,
                log_canonical,
                ..
            } => {
                assert_eq!(log_path, inside);
                assert!(log_canonical.starts_with(&source_canonical));
            }
            other => panic!("expected an in-source rejection, got {other:?}"),
        }

        let outside = tempfile::tempdir().unwrap();
        resolve_events_path_outside_source(&outside.path().join("events.jsonl"), &source_canonical)
            .expect("disjoint events log must pass");
    }

    #[test]
    fn relative_events_path_resolves_against_cwd() {
        let resolved = resolve_events_path(Path::new("relative-events.jsonl"))
            .expect("relative path must resolve via the process cwd");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("relative-events.jsonl"));

        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let err = resolve_events_path_outside_source(Path::new("relative-events.jsonl"), &cwd)
            .expect_err("a relative path inside the cwd/source must be rejected");
        assert!(matches!(err, EventsPathError::InsideSource { .. }));
    }

    /// The CLI validates the resolved target and later opens that same path.
    /// Re-resolving the raw flag at open time (the shape this guards against)
    /// would let a symlink repointed at the source after validation land the
    /// log inside the tree.
    #[test]
    fn link_swapped_after_validation_cannot_redirect_the_log() {
        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let approved = outside.path().join("events.jsonl");
        let link = outside.path().join("events-link.jsonl");
        std::os::unix::fs::symlink(&approved, &link).unwrap();

        let validated = resolve_events_path_outside_source(&link, &source_canonical)
            .expect("an external target is approved");

        // The attack: the link is repointed into the source after the guard.
        let planted = source.path().join("planted-events.jsonl");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&planted, &link).unwrap();
        assert!(
            resolve_events_path(&link)
                .expect("the swapped link resolves")
                .starts_with(&source_canonical),
            "the swap must really move the link into the source, or this test proves nothing"
        );

        // The writer opens the approved path, so the swap has no effect.
        let writer =
            JsonlSecurityEventWriter::new(&validated, 0).expect("open the approved target");
        drop(writer);

        assert!(
            !planted.exists(),
            "a link swapped in after validation must not redirect the log into the source"
        );
        assert_eq!(validated, approved, "validation returns what it checked");
        assert!(approved.exists(), "the approved target is the file created");
    }

    /// The same validate-then-open boundary, one level up: the approved path
    /// stays byte-identical, but the directory that held it is replaced. A
    /// plain `open` follows the replacement, so the log would be created
    /// inside the source even though nothing about the approved path changed.
    #[test]
    fn parent_swapped_after_validation_cannot_redirect_the_log() {
        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().canonicalize().unwrap();
        let log_dir = outside.join("events-dir");
        std::fs::create_dir(&log_dir).unwrap();
        let approved = log_dir.join("events.jsonl");

        let validated = resolve_events_path_outside_source(&approved, &source_canonical)
            .expect("an external directory is approved");
        assert_eq!(validated, approved, "validation returns what it checked");

        // The attack: the approved parent is moved aside and its name is taken
        // by a link into the source. The approved path never changes.
        let displaced = outside.join("events-dir-moved");
        std::fs::rename(&log_dir, &displaced).unwrap();
        std::os::unix::fs::symlink(source.path(), &log_dir).unwrap();
        assert!(
            resolve_events_path(&approved)
                .expect("the swapped parent resolves")
                .starts_with(&source_canonical),
            "the swap must really point the approved name into the source, or this test proves nothing"
        );

        // The writer refuses the replaced component instead of following it.
        let err = match JsonlSecurityEventWriter::new(&validated, 0) {
            Ok(_) => panic!("a parent replaced by a link into the source must not be followed"),
            Err(err) => err,
        };
        assert!(
            matches!(err.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)),
            "expected a no-follow refusal, got {err:?}"
        );
        assert!(
            !source.path().join("events.jsonl").exists(),
            "the log must not be created inside the source"
        );
        assert!(
            !displaced.join("events.jsonl").exists(),
            "the open must fail closed rather than write through the moved directory"
        );
    }

    /// A hard link is the alias the path guard cannot see: two names, two
    /// canonical paths, one inode. Opening the external name would append
    /// every event to the source file behind it, so the writer must refuse
    /// the target on the fd even though validation approved the path.
    #[test]
    fn hard_link_to_source_file_is_refused() {
        use std::os::unix::fs::MetadataExt;

        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let manifest = source.path().join("SKILL.md");
        std::fs::write(&manifest, "# trusted manifest\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_canonical = outside.path().canonicalize().unwrap();
        let link = outside.path().join("events.jsonl");
        std::fs::hard_link(&manifest, &link).expect("create the external hard link");
        assert_eq!(
            std::fs::metadata(&manifest).unwrap().nlink(),
            2,
            "the scenario needs one inode under two names"
        );

        // The path guard approves the external name: it really is outside.
        let approved = resolve_events_path_outside_source(&link, &source_canonical)
            .expect("the link's own path lies outside the source");
        assert_eq!(approved, outside_canonical.join("events.jsonl"));

        // The writer sees the shared inode and fails closed.
        let err = JsonlSecurityEventWriter::new(&approved, 0)
            .err()
            .expect("a log aliasing a source file must be refused");
        assert!(
            err.to_string().contains("hard links"),
            "expected the hard-link refusal, got {err}"
        );
        assert_eq!(
            std::fs::read(&manifest).unwrap(),
            b"# trusted manifest\n",
            "the refused target must not be opened for append"
        );

        // A singly linked external log is still an ordinary target.
        let plain = outside.path().join("plain.jsonl");
        JsonlSecurityEventWriter::new(&plain, 0).expect("a singly linked log opens");
        assert!(plain.exists());
    }

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

    /// A `0300` parent is a legal configuration: the operator can search the
    /// directory and create or append the log in it, but cannot list it. The
    /// approved path resolves under it and the leaf open only needs write and
    /// search, so the writer must not add a read requirement of its own — an
    /// anchor opened for reading fails here with `EACCES`, which aborts the
    /// mount at startup. The refusal needs an unprivileged process to bite,
    /// which is how the daemon runs.
    #[test]
    fn log_is_created_and_appended_under_a_write_only_parent() {
        use std::os::unix::fs::PermissionsExt;

        let source = tempfile::tempdir().unwrap();
        let source_canonical = source.path().canonicalize().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().canonicalize().unwrap();
        let log_dir = outside.join("write-only");
        std::fs::create_dir(&log_dir).unwrap();
        std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o300)).unwrap();
        let approved = log_dir.join("events.jsonl");

        let validated = resolve_events_path_outside_source(&approved, &source_canonical)
            .expect("a write-only external directory is approved");

        // Create: the first event brings the file into being.
        {
            let writer = JsonlSecurityEventWriter::new(&validated, 0)
                .expect("a write-only parent must not need read permission");
            writer.emit(&SecurityEvent::new("alpha", "write", "resolve", "current"));
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        // Append: a second writer over the same path must neither refuse nor
        // truncate what the first one wrote.
        {
            let writer = JsonlSecurityEventWriter::new(&validated, 0)
                .expect("appending under a write-only parent must work too");
            writer.emit(&SecurityEvent::new("beta", "mkdir", "resolve", "current"));
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Reached by name, which needs search on the parent and nothing more;
        // nothing in this path ever lists the directory.
        let body = std::fs::read_to_string(&validated).expect("the log is readable by name");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "both events must land in the log: {body:?}");
        assert!(lines[0].contains("\"skill\":\"alpha\""), "got {body:?}");
        assert!(lines[1].contains("\"skill\":\"beta\""), "got {body:?}");

        // Restore listability so the temp guard can remove the tree.
        std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
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

    #[test]
    fn resolve_events_path_follows_dangling_symlink() {
        // A dangling link is exactly what open(O_CREAT) would follow and
        // create, so the resolver must report the target's location — not
        // the link's — or the containment guard validates the wrong path.
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link.jsonl");
        let target = dir.path().join("nested").join("target.jsonl");
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let resolved = resolve_events_path(&link).expect("resolve");
        let expected_parent = dir.path().join("nested").canonicalize().unwrap();
        assert_eq!(
            resolved,
            expected_parent.join("target.jsonl"),
            "the dangling link must resolve to its target's canonical location"
        );
    }
}
