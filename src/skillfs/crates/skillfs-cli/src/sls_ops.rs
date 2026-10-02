//! Best-effort SLS ops JSONL writer for SkillFS CLI subcommands.
//!
//! Each CLI invocation (`list`, `validate`, `classify`, `mount`) appends one
//! JSON line to the deployment-owned ops log:
//!   `/var/log/anolisa/sls/ops/skillfs.jsonl`
//!
//! Design contract (mirrors the mount-session summary writer and the tokenless
//! SLS writer):
//!
//! * The target file is owned by the anolisa SLS component, which creates,
//!   rotates, and removes it. The CLI only appends: when the file does not
//!   exist the write is silently skipped ("SLS collection not active"). The
//!   CLI never creates the file or its parent directory.
//! * Every write opens the file, appends one JSONL line, and closes the handle
//!   so rename-based rotation never strands writes in a stale fd.
//! * Write failures never change CLI stdout/stderr, exit status, or panic.
//! * The default path may be overridden via `SKILLFS_SLS_OPS_PATH` for tests.
//!   The override is validated: it must not contain `..` and, after resolving
//!   symlinks, must live under `/var/log/` or `/tmp/`. Canonicalization means a
//!   parent-directory symlink (e.g. `/tmp/link -> /etc`) cannot escape those
//!   roots. `/var/log/` is additionally trusted pre-canonicalization so a
//!   root-owned `/var/log` symlinked to another filesystem still resolves.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use tracing::warn;

/// Default ops log path per the deployment convention.
pub const SKILLFS_SLS_OPS_LOG_PATH: &str = "/var/log/anolisa/sls/ops/skillfs.jsonl";

/// Environment variable to override the ops log path (test-scoped).
pub const SLS_OPS_PATH_ENV: &str = "SKILLFS_SLS_OPS_PATH";

/// agent_name recorded for CLI-initiated operations.
const CLI_AGENT_NAME: &str = "cli";

/// Allowed prefixes (post-canonicalization) for the `SKILLFS_SLS_OPS_PATH`
/// override. `/tmp/` is world-writable, so it is only appropriate for tests;
/// production uses the default `/var/log/` path.
const ALLOWED_OVERRIDE_PREFIXES: &[&str] = &["/var/log/", "/tmp/"];

/// Root-owned prefixes trusted pre-canonicalization: creating a symlink there
/// requires privilege, so the original path can be trusted even when
/// canonicalization resolves it elsewhere. World-writable `/tmp/` is excluded
/// so a user-placed symlink there cannot escape.
const TRUSTED_OVERRIDE_PREFIXES: &[&str] = &["/var/log/"];

/// One SLS ops record for a single CLI command attempt.
///
/// Field names use the dot-namespaced keys expected by the SLS schema; serde
/// `rename` produces the exact JSON keys.
#[derive(Debug, Clone, Serialize)]
pub struct SlsOpsRecord {
    #[serde(rename = "component.name")]
    pub component_name: String,
    #[serde(rename = "component.version")]
    pub component_version: String,
    #[serde(rename = "component.agent_name")]
    pub agent_name: String,
    pub ops_id: String,
    pub ops_name: String,
    pub ops_time: u64,
    pub err_reason: String,
}

impl SlsOpsRecord {
    /// Build a record for `ops_name` that took `elapsed_ms`, with `err_reason`
    /// set to `"none"` on success or a concise error string otherwise.
    pub fn new(ops_name: &str, elapsed_ms: u64, err_reason: Option<String>) -> Self {
        Self {
            component_name: "skillfs".to_string(),
            component_version: env!("CARGO_PKG_VERSION").to_string(),
            agent_name: CLI_AGENT_NAME.to_string(),
            ops_id: generate_ops_id(ops_name),
            ops_name: ops_name.to_string(),
            ops_time: elapsed_ms,
            err_reason: err_reason.unwrap_or_else(|| "none".to_string()),
        }
    }
}

/// Generate an ops id unique enough for local operations: pid + nanosecond
/// timestamp + the command name.
fn generate_ops_id(ops_name: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", ops_name, std::process::id(), nanos)
}

/// Canonicalize a path, walking up the parent chain to resolve symlinks when
/// the leaf does not exist yet. Falls back to the original if nothing resolves.
fn canonicalize_or_reconstruct(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        let mut cursor = path.to_path_buf();
        let mut suffix: Vec<std::ffi::OsString> = Vec::new();
        loop {
            match cursor.canonicalize() {
                Ok(canon) => {
                    let mut result = canon;
                    for name in suffix.iter().rev() {
                        result.push(name);
                    }
                    return result;
                }
                Err(_) => {
                    if let Some(name) = cursor.file_name() {
                        suffix.push(name.to_os_string());
                    }
                    match cursor.parent() {
                        Some(p) => cursor = p.to_path_buf(),
                        None => return path.to_path_buf(),
                    }
                }
            }
        }
    })
}

/// Validate an override path: reject `..` traversal, then canonicalize to
/// resolve symlinks (including in parent components) before the prefix check.
/// Returns the canonicalized path if acceptable, else `None`.
///
/// A path is accepted when the resolved path is under an allowed prefix, OR the
/// original path is under a root-owned trusted prefix (see
/// [`TRUSTED_OVERRIDE_PREFIXES`]). The trusted fallback covers a root-owned
/// `/var/log` symlinked elsewhere; `/tmp/` is excluded so a user-placed parent
/// symlink there cannot escape the allowed roots.
fn validate_override_path(path: &Path) -> Option<PathBuf> {
    if path
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return None;
    }

    let resolved = canonicalize_or_reconstruct(path);
    let resolved_str = resolved.to_str().unwrap_or("");
    let original_str = path.to_str().unwrap_or("");
    let resolved_ok = ALLOWED_OVERRIDE_PREFIXES
        .iter()
        .any(|prefix| resolved_str.starts_with(prefix));
    let original_ok = TRUSTED_OVERRIDE_PREFIXES
        .iter()
        .any(|prefix| original_str.starts_with(prefix));
    if resolved_ok || original_ok {
        Some(resolved)
    } else {
        None
    }
}

/// Resolve the ops log path used by every SkillFS SLS writer (CLI ops and
/// runtime metrics), honoring the validated `SKILLFS_SLS_OPS_PATH` override.
/// Falls back to the default deployment path.
pub fn resolve_ops_log_path() -> PathBuf {
    let env_val = std::env::var(SLS_OPS_PATH_ENV).ok();
    resolve_ops_path(env_val.as_deref())
}

/// Resolve the ops log path from the optional env override, falling back to the
/// default when unset, empty, or invalid.
fn resolve_ops_path(env_val: Option<&str>) -> PathBuf {
    env_val
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .and_then(|p| validate_override_path(&p))
        .unwrap_or_else(|| PathBuf::from(SKILLFS_SLS_OPS_LOG_PATH))
}

/// Best-effort JSONL ops writer.
pub struct SlsOpsWriter {
    path: PathBuf,
    /// Telemetry disable sentinel. Pinned to the production sentinel in
    /// [`SlsOpsWriter::new`] (no override path exists); tests inject a temp path
    /// so the gate does not depend on the host's real sentinel.
    sentinel: PathBuf,
}

impl Default for SlsOpsWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl SlsOpsWriter {
    /// Create a writer from the `SKILLFS_SLS_OPS_PATH` override (validated),
    /// falling back to the default deployment path.
    pub fn new() -> Self {
        let env_val = std::env::var(SLS_OPS_PATH_ENV).ok();
        Self {
            path: resolve_ops_path(env_val.as_deref()),
            sentinel: PathBuf::from(skillfs_fuse::security::TELEMETRY_DISABLED_SENTINEL),
        }
    }

    /// Create a writer targeting an explicit path (for tests). The telemetry
    /// sentinel is pointed at a sibling path the tests never create, isolating
    /// them from the host's real `/etc/anolisa/.telemetry_disabled`.
    #[cfg(test)]
    fn with_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let sentinel = path
            .parent()
            .map(|p| p.join(".telemetry_disabled"))
            .unwrap_or_else(|| PathBuf::from(skillfs_fuse::security::TELEMETRY_DISABLED_SENTINEL));
        Self { path, sentinel }
    }

    /// Override the telemetry sentinel path (tests only).
    #[cfg(test)]
    fn with_sentinel(mut self, sentinel: impl Into<PathBuf>) -> Self {
        self.sentinel = sentinel.into();
        self
    }

    /// Append one ops record as a JSONL line (open + append + close).
    ///
    /// Skips silently when the target file does not exist — deployment owns
    /// file creation. Serialization or write failures are logged via
    /// `tracing::warn` and swallowed; they never change CLI behavior.
    pub fn write(&self, record: &SlsOpsRecord) {
        // Re-check the disable sentinel on every write (before serialization
        // and open) so creating/removing it takes effect immediately without
        // restarting; disabled is a normal state, so skip silently.
        if !skillfs_fuse::security::telemetry_allowed_at(&self.sentinel) {
            return;
        }
        if !self.path.exists() {
            return;
        }

        let mut line = match serde_json::to_string(record) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "skillfs sls ops: failed to serialize record");
                return;
            }
        };
        line.push('\n');

        let mut opts = std::fs::OpenOptions::new();
        opts.append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A legit SLS file is never a symlink; O_NOFOLLOW blocks a
            // swap-to-symlink between the existence check and the open.
            opts.custom_flags(libc::O_NOFOLLOW);
        }

        if let Err(e) = opts
            .open(&self.path)
            .and_then(|mut f| f.write_all(line.as_bytes()))
        {
            warn!(
                error = %e,
                path = %self.path.display(),
                "skillfs sls ops: failed to append record (non-fatal)"
            );
        }
    }
}

/// Log one CLI command attempt to the ops log. Best-effort: never panics,
/// never changes exit status.
///
/// `err_reason` is `None` for success and `Some(reason)` for a concise failure
/// description.
pub fn log_command(ops_name: &str, start: Instant, err_reason: Option<String>) {
    let elapsed_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let record = SlsOpsRecord::new(ops_name, elapsed_ms, err_reason);
    SlsOpsWriter::new().write(&record);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_has_expected_fields_on_success() {
        let record = SlsOpsRecord::new("list", 12, None);
        let json = serde_json::to_string(&record).unwrap();
        let obj: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(obj["component.name"], "skillfs");
        assert_eq!(obj["component.agent_name"], "cli");
        assert!(
            !obj["component.version"].as_str().unwrap().is_empty(),
            "component.version must be populated"
        );
        assert_eq!(obj["ops_name"], "list");
        assert_eq!(obj["ops_time"], 12);
        assert_eq!(obj["err_reason"], "none");
        assert!(
            obj["ops_id"].as_str().unwrap().starts_with("list-"),
            "ops_id should embed the ops_name"
        );
    }

    #[test]
    fn record_captures_error_reason() {
        let record = SlsOpsRecord::new("validate", 5, Some("2 skill(s) failed".to_string()));
        let json = serde_json::to_string(&record).unwrap();
        let obj: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(obj["ops_name"], "validate");
        assert_eq!(obj["err_reason"], "2 skill(s) failed");
    }

    #[test]
    fn writer_appends_one_line_to_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skillfs.jsonl");
        std::fs::File::create(&path).unwrap();

        let writer = SlsOpsWriter::with_path(&path);
        writer.write(&SlsOpsRecord::new("list", 3, None));

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let obj: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(obj["component.name"], "skillfs");
        assert_eq!(obj["ops_name"], "list");
    }

    #[test]
    fn disabled_sentinel_suppresses_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skillfs.jsonl");
        std::fs::File::create(&path).unwrap();

        // Sentinel present -> the write is suppressed even though the log file
        // exists.
        let sentinel = dir.path().join(".telemetry_disabled");
        std::fs::File::create(&sentinel).unwrap();
        let writer = SlsOpsWriter::with_path(&path).with_sentinel(&sentinel);
        writer.write(&SlsOpsRecord::new("list", 3, None));

        assert!(
            std::fs::read_to_string(&path).unwrap().is_empty(),
            "disabled telemetry must not append any record"
        );
    }

    #[test]
    fn writer_appends_multiple_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skillfs.jsonl");
        std::fs::File::create(&path).unwrap();

        let writer = SlsOpsWriter::with_path(&path);
        writer.write(&SlsOpsRecord::new("list", 1, None));
        writer.write(&SlsOpsRecord::new("validate", 2, Some("bad".to_string())));

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["ops_name"], "list");
        assert_eq!(second["ops_name"], "validate");
        assert_eq!(second["err_reason"], "bad");
    }

    #[test]
    fn writer_skips_missing_file_without_creating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");

        let writer = SlsOpsWriter::with_path(&path);
        writer.write(&SlsOpsRecord::new("classify", 4, None));

        assert!(
            !path.exists(),
            "writer must not create the missing ops log file"
        );
    }

    #[test]
    fn writer_is_non_fatal_on_invalid_path() {
        // Missing parent directory: the file does not exist, so the write is
        // skipped silently and must not panic.
        let writer = SlsOpsWriter::with_path("/nonexistent/deep/dir/skillfs.jsonl");
        writer.write(&SlsOpsRecord::new("mount", 7, None)); // no panic
    }

    #[test]
    fn resolve_ops_path_defaults_when_unset() {
        assert_eq!(
            resolve_ops_path(None),
            PathBuf::from(SKILLFS_SLS_OPS_LOG_PATH)
        );
        assert_eq!(
            resolve_ops_path(Some("")),
            PathBuf::from(SKILLFS_SLS_OPS_LOG_PATH)
        );
    }

    #[test]
    fn resolve_ops_path_rejects_traversal_and_bad_prefix() {
        assert_eq!(
            resolve_ops_path(Some("/var/log/../../etc/passwd")),
            PathBuf::from(SKILLFS_SLS_OPS_LOG_PATH)
        );
        assert_eq!(
            resolve_ops_path(Some("/etc/cron.d/evil")),
            PathBuf::from(SKILLFS_SLS_OPS_LOG_PATH)
        );
    }

    #[test]
    fn resolve_ops_path_accepts_allowed_prefixes() {
        // Under /tmp (a real dir on Linux) the reconstructed path keeps the
        // /tmp/ prefix; a /var/log/ path is accepted via the trusted fallback.
        let tmp = resolve_ops_path(Some("/tmp/skillfs-test.jsonl"));
        assert!(
            tmp.to_str().unwrap().starts_with("/tmp/"),
            "unexpected resolved tmp path: {}",
            tmp.display()
        );
        let varlog = resolve_ops_path(Some("/var/log/custom/skillfs.jsonl"));
        assert_eq!(
            varlog,
            PathBuf::from("/var/log/custom/skillfs.jsonl"),
            "trusted /var/log path should be accepted as-is"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_rejects_tmp_parent_symlink_escape() {
        // /tmp/ is world-writable: a parent-directory symlink there must not be
        // usable to escape the allowed roots. Canonicalization resolves the
        // symlink, and /tmp/ is not a trusted prefix, so the path is rejected
        // and resolution falls back to the default.
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let link = dir.path().join("escape");
        std::os::unix::fs::symlink("/etc", &link).unwrap();
        let escaped = link.join("cron.d/evil.jsonl");
        assert!(validate_override_path(&escaped).is_none());
        assert_eq!(
            resolve_ops_path(escaped.to_str()),
            PathBuf::from(SKILLFS_SLS_OPS_LOG_PATH)
        );
    }
}
