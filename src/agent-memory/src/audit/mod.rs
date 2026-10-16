pub mod journald;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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
        //
        // The same co-tenant threat model applies to *reading*: the log
        // records every tool call's paths and error text, so it is created
        // owner-only (0600, mirroring the tokenless stash database) and a
        // pre-existing group/other-readable file is tightened on open —
        // files created by older builds are repaired on first run.
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&path)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
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

    #[cfg(unix)]
    #[test]
    fn audit_log_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let p = tmp.path().join("audit.log");
        let log = AuditLogger::new(p.clone()).unwrap();
        log.log(AuditEntry::new("mem_write").path("notes/a.md"))
            .unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600,
            "the audit log must not be readable by group/other"
        );
    }

    #[cfg(unix)]
    #[test]
    fn audit_log_tightens_pre_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let p = tmp.path().join("audit.log");
        // A log created by an older build (or a permissive umask at creation
        // time) may already exist group/other-readable.
        std::fs::write(&p, b"{\"legacy\":true}\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();

        let log = AuditLogger::new(p.clone()).unwrap();
        log.log(AuditEntry::new("mem_read").path("notes/a.md"))
            .unwrap();
        drop(log);
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600,
            "opening an existing audit log must repair its permissions"
        );
        // The pre-existing line and the new append both survive the reopen.
        let contents = std::fs::read_to_string(&p).unwrap();
        assert!(contents.contains("\"legacy\":true"));
        assert!(contents.contains("mem_read"));
    }
}
