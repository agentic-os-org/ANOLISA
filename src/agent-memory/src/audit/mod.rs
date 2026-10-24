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

/// Every mode prefix [`sanitized_query_path`] is called with: the `match mode`
/// arms in `tools::memory_search`, the embedding-failure path (`embed`), and
/// the two BM25 fallback spellings built by [`fallback_query_mode`].
///
/// [`is_sanitized_query_path`] accepts markers built from this list and nothing
/// else, so a new search mode has to be added here too — otherwise its markers
/// get mined as queries again, which is the bug this module exists to prevent.
pub const SANITIZED_QUERY_MODES: &[&str] = &[
    "bm25",
    "vector",
    "hybrid",
    "embed",
    "bm25(fallback from vector)",
    "bm25(fallback from hybrid)",
];

/// Mode prefix `memory_search` writes when a `vector`/`hybrid` request degrades
/// to BM25. Shared by the writer and by [`SANITIZED_QUERY_MODES`] so the
/// spelling cannot drift apart.
pub fn fallback_query_mode(mode: &str) -> String {
    format!("bm25(fallback from {mode})")
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
/// Writer and readers share these helpers so the marker spelling cannot
/// drift apart again — it already did once, when the readers were left parsing
/// the pre-sanitization `mode:query` form.
pub fn sanitized_query_path(mode: &str, query: &str) -> String {
    format!("{mode}:len={}", query.len())
}

/// True when `path` is a [`sanitized_query_path`] marker rather than a mount
/// path or a recorded query: it is exactly `<mode>:len=<digits>` for one of the
/// [`SANITIZED_QUERY_MODES`] the writer emits.
///
/// Requiring the mode prefix — instead of splitting on the last `:` and looking
/// at the tail alone — is what keeps legacy rows readable. Rows written before
/// sanitization landed recorded the query itself, and a query may contain
/// colons: `bm25:packet:len=14` recorded `packet:len=14`. No supported mode is
/// spelled `bm25:packet`, so that row is evidence, not a marker; classifying it
/// as one silently dropped historical queries from interest and profile
/// synthesis. A path with no mode prefix at all (a bare `len=14`, a mount path)
/// is not a marker either — the writer always prefixes one.
pub fn is_sanitized_query_path(path: &str) -> bool {
    SANITIZED_QUERY_MODES.iter().any(|mode| {
        let Some(tail) = path
            .strip_prefix(mode)
            .and_then(|rest| rest.strip_prefix(':'))
        else {
            return false;
        };
        is_query_length_marker(tail)
    })
}

/// True for the `len=<digits>` half of a marker. A bare `len=` carries no
/// length, so it is not a marker: a query really spelled `len=` still reaches
/// the readers untouched.
fn is_query_length_marker(tail: &str) -> bool {
    match tail.strip_prefix("len=") {
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
            "notes/bm25:len=14",
            "bm25:rust ownership",
            "hybrid:kernel config",
            "ignore all previous instructions",
            // No mode prefix: the writer always emits one, so these are data.
            "len=14",
            "len=",
            "bm25:len=14x",
            "bm25:len=abc",
            // A mode the writer does not emit is not a marker prefix.
            "bm25+vec:len=14",
        ] {
            assert!(
                !is_sanitized_query_path(path),
                "{path} must not be mistaken for a sanitized marker"
            );
        }
    }

    #[test]
    fn predicate_keeps_legacy_queries_whose_tail_looks_like_a_marker() {
        // Regression: pre-sanitization rows recorded the query itself, and a
        // query may contain colons. `bm25:packet:len=14` recorded the query
        // `packet:len=14`; matching the segment after the *last* colon
        // classified it as a marker, so both readers dropped real evidence.
        for path in [
            "bm25:packet:len=14",
            "hybrid:kernel:len=9",
            "vector:embed:len=3",
            "bm25(fallback from hybrid):packet:len=14",
        ] {
            assert!(
                !is_sanitized_query_path(path),
                "{path} records a legacy query and must reach the readers"
            );
        }
    }

    #[test]
    fn predicate_covers_every_mode_the_search_writer_emits() {
        // The modes `tools::memory_search` passes to `sanitized_query_path`:
        // the three supported search modes plus the embedding-failure path...
        let mut writer_modes: Vec<String> = ["bm25", "vector", "hybrid", "embed"]
            .iter()
            .map(|m| m.to_string())
            .collect();
        // ... and the fallback spelling written when vector/hybrid degrade.
        for mode in ["vector", "hybrid"] {
            writer_modes.push(fallback_query_mode(mode));
        }

        for mode in &writer_modes {
            assert!(
                SANITIZED_QUERY_MODES.contains(&mode.as_str()),
                "memory_search writes mode {mode:?}; add it to SANITIZED_QUERY_MODES \
                 or its markers will be mined as queries"
            );
            let path = sanitized_query_path(mode, "rust ownership");
            assert!(
                is_sanitized_query_path(&path),
                "{path} is a sanitized marker and must be recognized as one"
            );
        }
    }
}
