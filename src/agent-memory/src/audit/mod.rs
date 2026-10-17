pub mod journald;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// One line of the JSONL audit log written to `<mount>/.anolisa/audit.log`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// RFC3339 UTC timestamp.
    pub ts: String,
    /// Tool name, e.g. `mem_write`.
    pub tool: &'static str,
    /// Path relative to mount root (or empty if not applicable).
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub path: String,
    /// Whether the call succeeded.
    pub ok: bool,
    /// Bytes read or written, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Estimated token count for the response (bytes / 4 approximation).
    /// Only populated for search and context retrieval tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Error message if `ok == false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Optional trace id for cross-tool correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl AuditEntry {
    pub fn new(tool: &'static str) -> Self {
        Self {
            ts: Utc::now().to_rfc3339(),
            tool,
            path: String::new(),
            ok: true,
            bytes: None,
            tokens: None,
            error: None,
            trace_id: None,
        }
    }

    pub fn path(mut self, p: impl Into<String>) -> Self {
        self.path = p.into();
        self
    }

    pub fn ok(mut self, v: bool) -> Self {
        self.ok = v;
        self
    }

    pub fn bytes(mut self, n: u64) -> Self {
        self.bytes = Some(n);
        self
    }

    pub fn tokens(mut self, n: u64) -> Self {
        self.tokens = Some(n);
        self
    }

    pub fn error(mut self, msg: impl Into<String>) -> Self {
        self.error = Some(msg.into());
        self.ok = false;
        self
    }
}

/// Sanitized `memory_search` audit path: `<mode>:len=<query bytes>`.
///
/// Search queries are free-form agent text, so `memory_search` never writes
/// them to the audit log — it writes the query's byte length behind a mode
/// prefix instead. Readers that mine queries back out of the log
/// (`consolidation::heuristics`, `tools::user_profile`) must recognize the
/// marker and skip it: the query is unrecoverable, and persisting `len=14` as
/// an "interest" writes junk facts that search then serves back forever.
///
/// Writer and readers share these two helpers so the marker spelling cannot
/// drift apart again — it already did once, when the readers were left parsing
/// the pre-sanitization `mode:query` form.
pub fn sanitized_query_path(mode: &str, query: &str) -> String {
    format!("{mode}:len={}", query.len())
}

/// True when `path` is a [`sanitized_query_path`] marker rather than a mount
/// path or a recorded query: the segment after the last `:` — the whole string
/// when there is no `:` — is exactly `len=<digits>`.
pub fn is_sanitized_query_path(path: &str) -> bool {
    let tail = path.rsplit_once(':').map_or(path, |(_, tail)| tail);
    match tail.strip_prefix("len=") {
        // A bare `len=` carries no length, so it is not a marker: a query
        // really spelled `len=` still reaches the readers untouched.
        None | Some("") => false,
        Some(digits) => digits.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Append-only JSONL logger with a held file handle.
///
/// Each `log()` call writes to a persistent `File` handle guarded by a
/// process-local mutex, avoiding repeated open/close syscalls. When
/// `journald_enabled = true`, each entry is also sent to systemd-journald
/// (Linux only; no-op elsewhere).
pub struct AuditLogger {
    path: PathBuf,
    file: Mutex<File>,
    journald_enabled: bool,
}

impl AuditLogger {
    pub fn new(path: PathBuf) -> Result<Self> {
        Self::new_with_journald(path, false)
    }

    pub fn new_with_journald(path: PathBuf, journald_enabled: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // O_NOFOLLOW refuses to open the path if its final component is a
        // symlink, so a co-tenant process that swapped `audit.log` for
        // `→ /tmp/evil` cannot redirect our audit stream off-mount. The
        // audit log is the trust anchor for tamper-evidence (see CLAUDE.md
        // "信任链传导") so this open path matters as much as safe_fs does.
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&path)?;
        if journald_enabled {
            journald::probe();
        }
        Ok(Self {
            path,
            file: Mutex::new(f),
            journald_enabled,
        })
    }

    pub fn log(&self, entry: AuditEntry) -> Result<()> {
        let line = serde_json::to_string(&entry)? + "\n";
        {
            let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
            f.write_all(line.as_bytes())?;
            f.sync_all()?;
        }
        if self.journald_enabled {
            journald::fanout(&entry);
        }
        Ok(())
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn appends_jsonl_lines() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("audit.log");
        let log = AuditLogger::new(p.clone()).unwrap();

        log.log(AuditEntry::new("mem_write").path("notes/a.md").bytes(10))
            .unwrap();
        log.log(AuditEntry::new("mem_read").path("notes/a.md").error("nope"))
            .unwrap();

        let contents = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["tool"], "mem_write");
        assert_eq!(v["path"], "notes/a.md");
        assert_eq!(v["ok"], true);
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "nope");
    }

    #[test]
    fn sanitized_query_path_round_trips_through_the_predicate() {
        for mode in [
            "bm25",
            "vector",
            "hybrid",
            "embed",
            "bm25(fallback from hybrid)",
        ] {
            let path = sanitized_query_path(mode, "rust ownership");
            assert!(
                is_sanitized_query_path(&path),
                "{path} is a sanitized marker and must be recognized as one"
            );
        }
    }

    #[test]
    fn sanitized_marker_predicate_spares_paths_and_recorded_queries() {
        // Mount paths and legacy `mode:query` rows are not markers.
        for path in [
            "notes/a.md",
            "notes/",
            "bm25:rust ownership",
            "hybrid:kernel config",
            "ignore all previous instructions",
            "len=",
            "bm25:len=14x",
            "bm25:len=abc",
        ] {
            assert!(
                !is_sanitized_query_path(path),
                "{path} must not be mistaken for a sanitized marker"
            );
        }
    }
}
