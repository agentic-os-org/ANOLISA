//! SLS (Simple Log Service) data model and writer for tokenless stats.
//!
//! Provides SlsRecord (SLS-specific field naming via serde rename) and
//! SlsWriter (JSONL append-only writer with fail-silent semantics).
//!
//! The writer stamps every record with the host trace identity from
//! [`TraceContext`] so an observability backend can attribute token savings to
//! the trace that produced them (see [`crate::trace`]).
//!
//! The JSONL file is owned and lifecycle-managed by the anolisa SLS
//! component (it creates, rotates, and removes it). tokenless never
//! creates, truncates, or deletes the file: on each `write()` it appends
//! only if the file already exists, and silently skips when it does not
//! (treated as "SLS collection not active").

use crate::trace::TraceContext;
use crate::{StatsRecord, VERSION};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Default SLS JSONL output path.
/// NOTE: the file and its parent directory are owned by the anolisa SLS
/// component. tokenless only appends to it when it already exists; it never
/// creates the file or directory. Override the path via TOKENLESS_SLS_PATH
/// (must be under /var/log/ or /tmp/, no `..`).
pub const DEFAULT_SLS_PATH: &str = "/var/log/anolisa/sls/ops/tokenless.jsonl";

/// Allowed path prefixes for TOKENLESS_SLS_PATH env var override.
/// NOTE: /tmp/ is world-writable on most Unix systems — other local users
/// can read the JSONL file if placed there. Prefer /var/log/ for production.
const ALLOWED_SLS_PREFIXES: &[&str] = &["/var/log/", "/tmp/"];

/// Root-owned prefixes where creating a symlink requires privilege. For
/// these, the original (pre-canonicalize) path can be trusted even when
/// canonicalization resolves it elsewhere (e.g. `/var/log` symlinked to
/// another filesystem). World-writable prefixes like `/tmp/` are excluded
/// because an unprivileged user can place a symlink there to escape.
const TRUSTED_SLS_PREFIXES: &[&str] = &["/var/log/"];

/// Canonicalize a path, walking up the parent chain to resolve symlinks
/// when the path or its ancestors don't exist yet. Returns the best-effort
/// canonicalized path (falls back to the original if the entire chain is
/// unresolvable).
fn canonicalize_or_reconstruct(path: &std::path::Path) -> PathBuf {
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

/// Validate an SLS output path: reject `..` traversal and paths outside
/// allowed prefixes. Returns the canonicalized path if acceptable, or `None`.
///
/// Canonicalizes the path to resolve symlinks before the prefix check,
/// so a symlinked `/var/log` → `/` cannot be used to escape the allowed
/// directories. A path is accepted when the resolved path matches an
/// allowed prefix, OR the original (pre-canonicalize) path matches a
/// root-owned trusted prefix (see `TRUSTED_SLS_PREFIXES`).
///
/// The trusted-prefix fallback covers systems where `/var/log` is itself
/// symlinked to another filesystem: canonicalization resolves it to the
/// real target (no longer starting with `/var/log/`), but since creating
/// that symlink requires root, trusting the original path is safe. The
/// world-writable `/tmp/` prefix is excluded from the fallback, so a
/// user-placed symlink in `/tmp/` cannot escape to an arbitrary location.
///
/// NOTE: `SlsWriter` stores the canonicalized path at construction time
/// to narrow the TOCTOU window between validation and write.
fn validate_sls_path(path: &std::path::Path) -> Option<PathBuf> {
    if path
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return None;
    }

    let resolved = canonicalize_or_reconstruct(path);
    let resolved_str = resolved.to_str().unwrap_or("");
    let original_str = path.to_str().unwrap_or("");
    let resolved_ok = ALLOWED_SLS_PREFIXES
        .iter()
        .any(|prefix| resolved_str.starts_with(prefix));
    let original_ok = TRUSTED_SLS_PREFIXES
        .iter()
        .any(|prefix| original_str.starts_with(prefix));
    if resolved_ok || original_ok {
        Some(resolved)
    } else {
        None
    }
}

/// Resolve the SLS output path from an optional env var value.
/// Falls back to DEFAULT_SLS_PATH when the env var is unset, empty,
/// or contains an invalid path. Returns the canonicalized path
/// to narrow the TOCTOU window between validation and write.
fn resolve_sls_path(env_val: Option<&str>) -> PathBuf {
    env_val
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .and_then(|p| match validate_sls_path(&p) {
            Some(resolved) => Some(resolved),
            None => {
                eprintln!(
                    "tokenless-sls: TOKENLESS_SLS_PATH rejected (must be under \
                     /var/log/ or /tmp/, and must not contain '..'), \
                     falling back to default: {DEFAULT_SLS_PATH}"
                );
                None
            }
        })
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SLS_PATH))
}

/// SLS-specific data model with namespace-style field names.
///
/// Field names use dot-separated namespaces (component.*, tokenless.*,
/// tokenless.compression.*) as required by the SLS ingestion schema.
/// serde `rename` attributes produce the exact JSON keys expected by SLS.
#[derive(Debug, Clone, Serialize)]
pub struct SlsRecord {
    #[serde(rename = "component.name")]
    pub component_name: String,
    #[serde(rename = "component.version")]
    pub component_version: String,
    #[serde(rename = "component.agent_name")]
    pub agent_name: String,
    #[serde(rename = "tokenless.operation")]
    pub operation: String,
    #[serde(rename = "tokenless.session_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "tokenless.tool_use_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// W3C trace id of the host span this operation ran under, when the host
    /// propagated one. `None` for hosts without OpenTelemetry instrumentation.
    #[serde(rename = "tokenless.trace_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Id of the enclosing host span, paired with [`Self::trace_id`].
    #[serde(rename = "tokenless.span_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(rename = "tokenless.source_pid")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_pid: Option<i64>,
    #[serde(rename = "tokenless.compression.before_chars")]
    pub before_chars: usize,
    #[serde(rename = "tokenless.compression.before_tokens")]
    pub before_tokens: usize,
    #[serde(rename = "tokenless.compression.after_chars")]
    pub after_chars: usize,
    #[serde(rename = "tokenless.compression.after_tokens")]
    pub after_tokens: usize,
    #[serde(rename = "tokenless.compression.chars_saved")]
    pub chars_saved: usize,
    #[serde(rename = "tokenless.compression.tokens_saved")]
    pub tokens_saved: usize,
    #[serde(rename = "tokenless.compression.chars_saved_percent")]
    pub chars_saved_percent: f64,
    #[serde(rename = "tokenless.compression.tokens_saved_percent")]
    pub tokens_saved_percent: f64,
    #[serde(rename = "tokenless.compression.content_type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(rename = "tokenless.compression.content_origin")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_origin: Option<String>,
    #[serde(rename = "tokenless.compression.applied_operations")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_operations: Option<Vec<String>>,
    #[serde(rename = "tokenless.compression.recoverability")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recoverability: Option<String>,
    #[serde(rename = "tokenless.compression.tokenizer_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenizer_id: Option<String>,
    #[serde(rename = "tokenless.compression.unrecoverable_truncations")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unrecoverable_truncations: Option<i64>,
}

impl From<&StatsRecord> for SlsRecord {
    fn from(r: &StatsRecord) -> Self {
        Self {
            component_name: "tokenless".to_string(),
            component_version: VERSION.to_string(),
            agent_name: r.agent_id.clone(),
            operation: r.operation.as_str().to_string(),
            session_id: r.session_id.clone(),
            tool_use_id: r.tool_use_id.clone(),
            // Filled from the environment by SlsWriter::write — the record
            // itself carries no trace identity (see crate::trace).
            trace_id: None,
            span_id: None,
            source_pid: r.source_pid,
            before_chars: r.before_chars,
            before_tokens: r.before_tokens,
            after_chars: r.after_chars,
            after_tokens: r.after_tokens,
            chars_saved: r.chars_saved(),
            tokens_saved: r.tokens_saved(),
            chars_saved_percent: r.chars_percent(),
            tokens_saved_percent: r.tokens_percent(),
            content_type: r.content_type.clone(),
            content_origin: r.content_origin.clone(),
            applied_operations: r.applied_operations.clone(),
            recoverability: r.recoverability.clone(),
            tokenizer_id: r.tokenizer_id.clone(),
            unrecoverable_truncations: r.unrecoverable_truncations,
        }
    }
}

/// Writes SlsRecord entries to a JSONL file (one JSON object per line).
///
/// Each `write()` call opens the file (append+create), writes one JSON
/// line followed by a newline, then closes (file handle drops).
/// Fail-silent: errors are printed to stderr but never propagated.
pub struct SlsWriter {
    path: PathBuf,
    trace: Option<TraceContext>,
}

impl Default for SlsWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl SlsWriter {
    /// Create a writer using the TOKENLESS_SLS_PATH env var,
    /// falling back to DEFAULT_SLS_PATH if the env var is not set,
    /// empty, or contains an invalid path.
    ///
    /// The host trace identity is resolved once here, from the same
    /// environment: a hook process is short-lived and its `traceparent`
    /// cannot change between records.
    pub fn new() -> Self {
        let env_val = std::env::var("TOKENLESS_SLS_PATH").ok();
        Self {
            path: resolve_sls_path(env_val.as_deref()),
            trace: TraceContext::from_env(),
        }
    }

    /// Create a writer with an explicit path (for testing).
    /// NOTE: This bypasses `validate_sls_path` — the caller is responsible
    /// for ensuring the path is safe. Production code should use `new()`.
    #[cfg(test)]
    pub(crate) fn with_path(path: PathBuf) -> Self {
        Self { path, trace: None }
    }

    /// Attach an explicit trace context (for testing).
    ///
    /// Tests inject the identity instead of mutating `TRACEPARENT`, which
    /// would race across parallel test threads in the same process.
    #[cfg(test)]
    pub(crate) fn with_trace(mut self, trace: Option<TraceContext>) -> Self {
        self.trace = trace;
        self
    }

    /// Convert a StatsRecord to SlsRecord and append it as a JSON line.
    ///
    /// The JSONL file is owned by the anolisa SLS component, which is
    /// responsible for creating, rotating, and removing it. tokenless only
    /// appends: when the file does not yet exist the write is silently
    /// skipped (treated as "SLS collection not active"); tokenless never
    /// creates, truncates, or deletes the file or its parent directory.
    pub fn write(&self, record: &StatsRecord) {
        // Skip silently when the SLS collector has not created the file yet.
        if !self.path.exists() {
            return;
        }

        let mut sls_record = SlsRecord::from(record);
        if let Some(trace) = &self.trace {
            sls_record.trace_id = Some(trace.trace_id.clone());
            sls_record.span_id = Some(trace.span_id.clone());
        }
        let line = match serde_json::to_string(&sls_record) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("tokenless-sls: serialization error: {e}");
                return;
            }
        };

        let mut opts = OpenOptions::new();
        opts.append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Refuse to open if the final path component is a symlink. The
            // file is owned by the anolisa SLS component and tokenless never
            // creates one, so a legit target is never a symlink here;
            // O_NOFOLLOW blocks a swap-to-symlink between the existence
            // check and the open (narrowing the TOCTOU window on /tmp/).
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        if let Err(e) = opts.open(&self.path).and_then(|mut f| {
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")
        }) {
            static WRITE_ERROR_WARNED: AtomicBool = AtomicBool::new(false);
            if !WRITE_ERROR_WARNED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "tokenless-sls: write error to {}: {} \
                     (further write errors suppressed)",
                    self.path.display(),
                    e
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::OperationType;
    use std::fs;

    fn make_record() -> StatsRecord {
        StatsRecord::new(
            OperationType::CompressSchema,
            "copilot-shell".to_string(),
            1000,
            400,
            500,
            200,
        )
        .with_session_id("session-123")
        .with_tool_use_id("call_abc")
        .with_source_pid(12345)
    }

    fn make_record_minimal() -> StatsRecord {
        StatsRecord::new(
            OperationType::CompressResponse,
            "test-agent".to_string(),
            200,
            80,
            100,
            40,
        )
    }

    #[test]
    fn test_sls_record_from_stats_record() {
        let r = make_record();
        let sls = SlsRecord::from(&r);

        assert_eq!(sls.component_name, "tokenless");
        assert_eq!(sls.component_version, VERSION);
        assert_eq!(sls.agent_name, "copilot-shell");
        assert_eq!(sls.operation, "compress-schema");
        assert_eq!(sls.session_id, Some("session-123".to_string()));
        assert_eq!(sls.tool_use_id, Some("call_abc".to_string()));
        assert_eq!(sls.source_pid, Some(12345));
        assert_eq!(sls.before_chars, 1000);
        assert_eq!(sls.before_tokens, 400);
        assert_eq!(sls.after_chars, 500);
        assert_eq!(sls.after_tokens, 200);
        assert_eq!(sls.chars_saved, 500);
        assert_eq!(sls.tokens_saved, 200);
        assert!((sls.chars_saved_percent - 50.0).abs() < 0.01);
        assert!((sls.tokens_saved_percent - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_sls_record_optional_fields_none() {
        let r = make_record_minimal();
        let sls = SlsRecord::from(&r);

        assert_eq!(sls.session_id, None);
        assert_eq!(sls.tool_use_id, None);
        assert_eq!(sls.source_pid, None);
        assert_eq!(sls.content_type, None);
        assert_eq!(sls.applied_operations, None);
        assert_eq!(sls.recoverability, None);
        assert_eq!(sls.tokenizer_id, None);
        assert_eq!(sls.unrecoverable_truncations, None);
    }

    #[test]
    fn test_sls_record_mirrors_entry_metadata() {
        let r = make_record().with_entry_metadata(
            Some("api-records".to_string()),
            Some("file_content".to_string()),
            Some(vec!["json_cleanup".to_string()]),
            Some("lossless".to_string()),
            "heuristic-v1",
            Some(1),
        );
        let sls = SlsRecord::from(&r);
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&sls).unwrap()).unwrap();
        assert_eq!(json["tokenless.compression.content_type"], "api-records");
        assert_eq!(json["tokenless.compression.content_origin"], "file_content");
        assert_eq!(
            json["tokenless.compression.applied_operations"],
            serde_json::json!(["json_cleanup"])
        );
        assert_eq!(json["tokenless.compression.recoverability"], "lossless");
        assert_eq!(json["tokenless.compression.tokenizer_id"], "heuristic-v1");
        assert_eq!(json["tokenless.compression.unrecoverable_truncations"], 1);
    }

    #[test]
    fn test_sls_record_json_field_names() {
        let r = make_record();
        let sls = SlsRecord::from(&r);
        let json = serde_json::to_string(&sls).unwrap();
        let obj: serde_json::Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<&str> = obj
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();

        let expected_keys = [
            "component.name",
            "component.version",
            "component.agent_name",
            "tokenless.operation",
            "tokenless.session_id",
            "tokenless.tool_use_id",
            "tokenless.source_pid",
            "tokenless.compression.before_chars",
            "tokenless.compression.before_tokens",
            "tokenless.compression.after_chars",
            "tokenless.compression.after_tokens",
            "tokenless.compression.chars_saved",
            "tokenless.compression.tokens_saved",
            "tokenless.compression.chars_saved_percent",
            "tokenless.compression.tokens_saved_percent",
        ];
        for expected in &expected_keys {
            assert!(keys.contains(expected), "missing key: {expected}");
        }
        assert_eq!(keys.len(), expected_keys.len());
    }

    #[test]
    fn test_sls_record_json_no_camelcase() {
        let r = make_record();
        let sls = SlsRecord::from(&r);
        let json = serde_json::to_string(&sls).unwrap();
        let obj: serde_json::Value = serde_json::from_str(&json).unwrap();

        for key in obj.as_object().unwrap().keys() {
            assert!(
                !key.chars().any(|c| c.is_ascii_uppercase()),
                "key contains uppercase: {key}"
            );
        }
    }

    #[test]
    fn test_sls_record_json_valid_chars_only() {
        let r = make_record();
        let sls = SlsRecord::from(&r);
        let json = serde_json::to_string(&sls).unwrap();
        let obj: serde_json::Value = serde_json::from_str(&json).unwrap();

        for key in obj.as_object().unwrap().keys() {
            for c in key.chars() {
                assert!(
                    c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || c == '.'
                        || c == '_'
                        || c == '-',
                    "invalid char '{c}' in key: {key}"
                );
            }
        }
    }

    #[test]
    fn test_sls_record_json_optional_fields_omitted() {
        let r = make_record_minimal();
        let sls = SlsRecord::from(&r);
        let json = serde_json::to_string(&sls).unwrap();
        let obj: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Option<T> fields with None are omitted from serialization
        assert!(
            !obj.as_object()
                .unwrap()
                .contains_key("tokenless.session_id")
        );
        assert!(
            !obj.as_object()
                .unwrap()
                .contains_key("tokenless.tool_use_id")
        );
        assert!(
            !obj.as_object()
                .unwrap()
                .contains_key("tokenless.source_pid")
        );
    }

    #[test]
    fn test_sls_writer_default_trait() {
        let _writer = SlsWriter::default();
    }

    #[test]
    fn test_sls_writer_writes_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        // The anolisa SLS component owns the file; tokenless only appends.
        fs::write(&path, "").unwrap();
        let writer = SlsWriter::with_path(path.clone());

        let r = make_record();
        writer.write(&r);

        let content = fs::read_to_string(&path).unwrap();
        let obj: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(obj["component.name"], "tokenless");
        assert_eq!(obj["component.agent_name"], "copilot-shell");
    }

    #[test]
    fn test_sls_writer_appends_multiple_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.jsonl");
        // The anolisa SLS component owns the file; tokenless only appends.
        fs::write(&path, "").unwrap();
        let writer = SlsWriter::with_path(path.clone());

        let r1 = make_record();
        let r2 = make_record_minimal();
        writer.write(&r1);
        writer.write(&r2);

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let obj1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(obj1["component.agent_name"], "copilot-shell");

        let obj2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(obj2["component.agent_name"], "test-agent");
    }

    #[test]
    fn test_sls_writer_skips_when_file_missing() {
        // tokenless never creates the file — when it is absent the write is
        // silently skipped (SLS collection not active).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        let writer = SlsWriter::with_path(path.clone());

        writer.write(&make_record());

        assert!(!path.exists(), "tokenless must not create the SLS file");
    }

    #[test]
    fn test_sls_writer_fail_silent_on_invalid_path() {
        // Path in a non-existent deep directory — the file is absent so the
        // write is skipped silently and must not panic.
        let writer = SlsWriter::with_path(PathBuf::from("/nonexistent/deep/dir/test.jsonl"));
        let r = make_record();
        writer.write(&r); // no panic
    }

    #[cfg(unix)]
    #[test]
    fn test_sls_writer_refuses_symlink_final_component() {
        // O_NOFOLLOW hardening: a legit SLS file is never a symlink. After the
        // SLS component creates it, an attacker swapping it for a symlink must
        // NOT redirect the write to the symlink target — the open fails
        // (ELOOP) and the write is skipped silently.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.jsonl");
        fs::write(&path, "").unwrap(); // SLS component pre-created the file
        let writer = SlsWriter::with_path(path.clone());

        // First write appends to the regular file.
        writer.write(&make_record());
        assert!(path.is_file());

        // Swap the file for a symlink and write again — must not follow it.
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", &path).unwrap();
        let before = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
        writer.write(&make_record()); // no panic, no follow
        let after = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
        assert_eq!(before, after, "symlink target must not be modified");
    }

    #[test]
    fn test_resolve_sls_path_default() {
        // When env var is not set, falls back to DEFAULT_SLS_PATH.
        assert_eq!(resolve_sls_path(None), PathBuf::from(DEFAULT_SLS_PATH));
    }

    #[test]
    fn test_resolve_sls_path_empty() {
        // Empty env var is treated as unset.
        assert_eq!(resolve_sls_path(Some("")), PathBuf::from(DEFAULT_SLS_PATH));
    }

    #[test]
    fn test_resolve_sls_path_rejects_parent_traversal() {
        // Paths containing ".." fall back to default.
        assert_eq!(
            resolve_sls_path(Some("/var/log/../../etc/passwd")),
            PathBuf::from(DEFAULT_SLS_PATH)
        );
    }

    #[test]
    fn test_resolve_sls_path_rejects_non_whitelisted_prefix() {
        // Paths outside /var/log/ and /tmp/ fall back to default.
        assert_eq!(
            resolve_sls_path(Some("/etc/cron.d/evil")),
            PathBuf::from(DEFAULT_SLS_PATH)
        );
    }

    #[test]
    fn test_resolve_sls_path_accepts_valid_path() {
        // Valid paths under /var/log/ or /tmp/ are accepted.
        assert_eq!(
            resolve_sls_path(Some("/var/log/custom/tokenless.jsonl")),
            PathBuf::from("/var/log/custom/tokenless.jsonl")
        );
        assert_eq!(
            resolve_sls_path(Some("/tmp/tokenless-test.jsonl")),
            PathBuf::from("/tmp/tokenless-test.jsonl")
        );
    }

    #[test]
    fn test_validate_sls_path_symlinked_prefix() {
        // When /var/log is symlinked to another filesystem, canonicalization
        // resolves it to the real target which no longer starts with
        // /var/log/. Since /var/log is root-owned (creating the symlink needs
        // privilege), the original path is trusted and the path is still
        // accepted. Here /var/log is a real directory (no symlink), so both
        // the original and reconstructed paths match the /var/log/ prefix.
        let result = validate_sls_path(std::path::Path::new(
            "/var/log/anolisa/sls/ops/tokenless.jsonl",
        ));
        assert!(result.is_some());
        let resolved = result.unwrap();
        assert!(resolved.to_str().unwrap().starts_with("/var/log/"));
    }

    #[test]
    fn test_sls_record_conversion_leaves_trace_unset() {
        // The trace identity comes from the environment, not from StatsRecord,
        // so the pure conversion must not invent one.
        let sls = SlsRecord::from(&make_record());
        assert_eq!(sls.trace_id, None);
        assert_eq!(sls.span_id, None);
    }

    #[test]
    fn test_sls_writer_stamps_host_trace_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traced.jsonl");
        // The anolisa SLS component owns the file; tokenless only appends.
        fs::write(&path, "").unwrap();
        let trace =
            TraceContext::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").unwrap();
        let writer = SlsWriter::with_path(path.clone()).with_trace(Some(trace));

        writer.write(&make_record());

        let content = fs::read_to_string(&path).unwrap();
        let obj: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(
            obj["tokenless.trace_id"],
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(obj["tokenless.span_id"], "00f067aa0ba902b7");
        // Existing attribution is untouched by trace stamping.
        assert_eq!(obj["tokenless.session_id"], "session-123");
    }

    #[test]
    fn test_sls_writer_omits_trace_keys_without_host_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("untraced.jsonl");
        fs::write(&path, "").unwrap();
        let writer = SlsWriter::with_path(path.clone());

        writer.write(&make_record());

        let content = fs::read_to_string(&path).unwrap();
        let obj: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        let keys = obj.as_object().unwrap();
        assert!(!keys.contains_key("tokenless.trace_id"));
        assert!(!keys.contains_key("tokenless.span_id"));
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_sls_path_rejects_tmp_symlink_escape() {
        // /tmp/ is world-writable, so an unprivileged user can place a
        // symlink there that escapes the allowed prefixes (e.g. -> /etc).
        // The path must be REJECTED: the resolved path no longer matches an
        // allowed prefix, and /tmp/ is not a trusted prefix, so the original
        // path must not be used as a fallback.
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("escape");
        std::os::unix::fs::symlink("/etc", &link).unwrap();
        let escaped = link.join("cron.d/evil.jsonl");
        assert!(validate_sls_path(&escaped).is_none());
    }
}
