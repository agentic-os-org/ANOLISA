//! Append-only audit log: JSONL format, 0600 permissions, size-based rotation.
//!
//! See `docs/audit-design.md` §8. Each `LogEntry` becomes one line of JSON.
//! Sensitive fields are redacted at write time (not at the PEP→PDP boundary,
//! so that PDPs can still see the raw values during evaluation).
//!
//! # Durability
//! Each write does an `O_APPEND` write followed by `fsync(2)`. The design
//! document calls for batched fsync (every 8 entries / 1 second) for the
//! long-lived TUI; we currently fsync per write because cosh-cli is the
//! dominant call site (one entry per process invocation), and the per-call
//! overhead is dwarfed by command latency. Switch to batching only if a
//! profiling pass identifies it as a hot path.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosh_types::audit::LogEntry;
use cosh_types::error::{CoshError, ErrorCode};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB
const MAX_RETAINED: usize = 7;

/// Resolve the active audit log file path. Honors:
/// 1. `$COSH_AUDIT_LOG`           (explicit override)
/// 2. `$XDG_STATE_HOME/cosh/audit.log`
/// 3. `$HOME/.local/state/cosh/audit.log`
/// 4. `/tmp/cosh-audit.log`       (last-resort fallback)
pub fn audit_log_path() -> PathBuf {
    if let Ok(p) = std::env::var("COSH_AUDIT_LOG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        if !state.is_empty() {
            return PathBuf::from(state).join("cosh/audit.log");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/cosh/audit.log");
    }
    PathBuf::from("/tmp/cosh-audit.log")
}

/// Write a single entry to the active audit log. The entry is serialized to
/// one line of JSON terminated by `\n`. The directory is created on demand.
/// The file is opened with mode `0600` so other users on the host cannot
/// read it.
pub fn write_entry(entry: &LogEntry) -> Result<(), CoshError> {
    write_entry_to(&audit_log_path(), entry)
}

pub fn write_entry_to(path: &Path, entry: &LogEntry) -> Result<(), CoshError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| io_err("create log dir", parent, e))?;
        }
    }
    rotate_if_needed(path)?;

    let line = serde_json::to_string(entry).map_err(|e| {
        CoshError::new(
            ErrorCode::AuditLogError,
            format!("failed to serialize log entry: {}", e),
            "audit",
        )
    })?;

    let mut opts = OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    opts.mode(0o600);

    let mut file = opts.open(path).map_err(|e| io_err("open log", path, e))?;
    writeln!(file, "{}", line).map_err(|e| io_err("write log", path, e))?;
    file.sync_data().map_err(|e| io_err("fsync log", path, e))?;
    Ok(())
}

/// Read all entries from the active log file. Corrupt lines are silently
/// skipped — JSONL's main durability property is "one bad line does not
/// poison the rest of the file" (audit-design.md §8.2).
pub fn read_entries(path: &Path) -> Result<Vec<LogEntry>, CoshError> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err("read log", path, e)),
    };
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue, // skip lines with read errors
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<LogEntry>(&line) {
            entries.push(e);
        }
    }
    Ok(entries)
}

fn rotate_if_needed(path: &Path) -> Result<(), CoshError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err("stat log", path, e)),
    };
    if meta.len() < MAX_LOG_BYTES {
        return Ok(());
    }
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audit.log");
    let rotated = path.with_file_name(format!("{}.{}", stem, stamp));
    std::fs::rename(path, &rotated).map_err(|e| io_err("rotate log", path, e))?;
    cleanup_old_rotations(path);
    Ok(())
}

fn cleanup_old_rotations(active_path: &Path) {
    let dir = match active_path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => return,
    };
    let stem = match active_path.file_name().and_then(|n| n.to_str()) {
        Some(s) => s,
        None => return,
    };
    let prefix = format!("{}.", stem);

    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut rotated: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix))
        })
        .collect();
    rotated.sort();
    while rotated.len() > MAX_RETAINED {
        let oldest = rotated.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
}

fn io_err(op: &str, path: &Path, e: std::io::Error) -> CoshError {
    CoshError::new(
        ErrorCode::AuditLogError,
        format!("{} {}: {}", op, path.display(), e),
        "audit",
    )
    .with_hint("check filesystem permissions or set $COSH_AUDIT_LOG to a writable path")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosh_types::audit::{Action, ActionSubsystem, Decision, LogEntry, LogSource, Outcome};

    fn sample_entry(seq: u32) -> LogEntry {
        LogEntry {
            timestamp: chrono::DateTime::from_timestamp(1_700_000_000 + seq as i64, 0).unwrap(),
            session_id: format!("sess-{}", seq),
            user: "alice".to_string(),
            uid: 1000,
            euid: 1000,
            sudo_user: None,
            pid: 1234,
            action: Action {
                subsystem: ActionSubsystem::Pkg,
                operation: "install".to_string(),
                target: Some("nginx".to_string()),
                args: vec![],
                raw: None,
            },
            decision: Decision {
                outcome: Outcome::Allow,
                reason: "test".to_string(),
                matched_rule: None,
                policy_version: "test@v1+sha256:000".to_string(),
            },
            source: LogSource::Cli,
            redacted: false,
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let e1 = sample_entry(1);
        let e2 = sample_entry(2);
        write_entry_to(&path, &e1).unwrap();
        write_entry_to(&path, &e2).unwrap();
        let read = read_entries(&path).unwrap();
        assert_eq!(read, vec![e1, e2]);
    }

    #[test]
    fn read_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-written.log");
        let read = read_entries(&path).unwrap();
        assert!(read.is_empty());
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        // Write one good entry, one garbage line, one good entry.
        write_entry_to(&path, &sample_entry(1)).unwrap();
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, "this is not json").unwrap();
        }
        write_entry_to(&path, &sample_entry(2)).unwrap();
        let read = read_entries(&path).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].session_id, "sess-1");
        assert_eq!(read[1].session_id, "sess-2");
    }

    #[cfg(unix)]
    #[test]
    fn log_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_entry_to(&path, &sample_entry(1)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got 0{:o}", mode);
    }

    #[test]
    fn rotation_renames_when_over_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");

        // Pre-create an oversized file by writing raw bytes.
        std::fs::write(&path, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();

        // Now write an entry — rotation should kick in.
        write_entry_to(&path, &sample_entry(1)).unwrap();

        // The active log should now contain only the freshly-written entry.
        let read = read_entries(&path).unwrap();
        assert_eq!(read.len(), 1);

        // And there must be at least one rotated file alongside it.
        let rotated_count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|s| s.starts_with("audit.log."))
            })
            .count();
        assert!(rotated_count >= 1, "expected a rotated audit.log.* file");
    }
}
