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

/// Shortest query worth reporting, in bytes.
const MIN_QUERY_BYTES: usize = 3;

/// The query text a search audit record carries — `None` when it carries none.
///
/// `memory_search` puts `<mode>:<payload>` in `path`, and the payload has been
/// `len=<N>` ever since a89dbc3bf ("audit_log sanitization") kept query text
/// out of the audit log, which is also fanned out to systemd-journald. That
/// marker is not a query. Reading it as one made consolidation write a
/// `搜索: len=NN` fact into `facts/interest/` for every search of every session
/// and made `mem_dream` report "interested in: len=14". `mem_grep` logs the
/// directory it walked, which is not a query either.
///
/// Any other payload is returned as the query, so a record that does carry one
/// — the pre-sanitization shape, or a future caller that logs one — keeps
/// working. Both consumers of the field (the consolidation interest rule and
/// the profile synthesis) read it through here, so the format has one reader;
/// `tests/tier_b_test.rs` pins that reader to what `memory_search` really
/// writes.
pub fn search_query(tool: &str, path: &str) -> Option<String> {
    if tool != "memory_search" {
        return None;
    }
    let payload = path.split_once(':')?.1.trim();
    if is_length_marker(payload) {
        return None;
    }
    (payload.len() > MIN_QUERY_BYTES).then(|| payload.to_string())
}

/// The audit `path` in the form persisted memory may quote it.
///
/// Episodes and the lesson rule copy a record's `path` into facts, and
/// FactWriter puts those under `facts/` where the index picks them up: whatever
/// is quoted becomes searchable memory content. For `memory_search` the field is
/// `<mode>:len=<N>`, and the length marker describes the audit record rather
/// than the call. Quoting it verbatim wrote `bm25:len=14` into an episodic fact
/// at confidence 0.9 — above the 0.8 `memory_session_context` injects into every
/// later prompt — and into the lesson fact of a failed embedding. The mode does
/// describe the call, so that is what survives here.
///
/// Every other record is quoted as it stands: `mem_grep`'s directory, a
/// `mem_promote` pair and a payload that really is a query are the call's own
/// input, not a marker.
pub fn quotable_path<'a>(tool: &str, path: &'a str) -> &'a str {
    if tool != "memory_search" {
        return path;
    }
    match path.split_once(':') {
        Some((mode, payload)) if is_length_marker(payload.trim()) => mode,
        _ => path,
    }
}

/// `<mode>:len=<N>` — the sanitized shape `memory_search` logs today.
fn is_length_marker(payload: &str) -> bool {
    match payload.strip_prefix("len=") {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
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
    fn search_query_refuses_the_sanitized_marker() {
        // Exactly what tools/memory_search.rs writes: "<mode>:len=<N>" for
        // every mode, including the no-embedding fallback marker.
        for path in [
            "bm25:len=23",
            "bm25(fallback from hybrid):len=9",
            "vector:len=7",
            "embed:len=7",
        ] {
            assert_eq!(
                search_query("memory_search", path),
                None,
                "{path} read as a query"
            );
        }
    }

    #[test]
    fn search_query_refuses_a_grep_directory() {
        // mem_grep logs the directory it walked (tools/grep.rs), never the
        // pattern.
        assert_eq!(search_query("mem_grep", "notes/kconfig"), None);
        assert_eq!(search_query("mem_grep", "bm25:len=9"), None);
    }

    #[test]
    fn quotable_path_drops_the_marker_and_keeps_the_mode() {
        // What tools/memory_search.rs writes, including the no-embedding
        // fallback marker and the shape a failed embedding logs.
        assert_eq!(quotable_path("memory_search", "bm25:len=23"), "bm25");
        assert_eq!(
            quotable_path("memory_search", "bm25(fallback from hybrid):len=9"),
            "bm25(fallback from hybrid)"
        );
        assert_eq!(quotable_path("memory_search", "embed:len=7"), "embed");
        // A payload that is not the marker is the record's own text.
        assert_eq!(
            quotable_path("memory_search", "bm25:rust ownership rules"),
            "bm25:rust ownership rules"
        );
        assert_eq!(
            quotable_path("memory_search", "bm25:len= without a number"),
            "bm25:len= without a number"
        );
        assert_eq!(quotable_path("memory_search", ""), "");
    }

    #[test]
    fn quotable_path_leaves_every_other_record_alone() {
        // A grep directory and a promote pair are the call's own input.
        assert_eq!(quotable_path("mem_grep", "notes/kconfig"), "notes/kconfig");
        assert_eq!(
            quotable_path("mem_promote", "scratch/a.md -> notes/a.md"),
            "scratch/a.md -> notes/a.md"
        );
        assert_eq!(quotable_path("mem_read", "notes/a.md"), "notes/a.md");
    }

    #[test]
    fn search_query_returns_a_payload_that_is_one() {
        // The pre-sanitization shape, and the shape any future caller that
        // logs the query itself would produce.
        assert_eq!(
            search_query("memory_search", "bm25:rust ownership rules").as_deref(),
            Some("rust ownership rules")
        );
        // Below the minimum length, and with no mode prefix at all.
        assert_eq!(search_query("memory_search", "bm25:rc"), None);
        assert_eq!(search_query("memory_search", "notes/kconfig"), None);
        // "len=" with no digits is a payload, not the marker.
        assert_eq!(
            search_query("memory_search", "bm25:len= without a number").as_deref(),
            Some("len= without a number")
        );
    }
}
