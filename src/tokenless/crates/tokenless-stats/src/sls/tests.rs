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
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-',
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
#[cfg(unix)]
fn test_resolve_sls_path_accepts_valid_path() {
    // Valid paths under /var/log/ or /tmp/ are accepted and returned
    // canonicalized (e.g. `/private/var/log` and `/private/tmp` on macOS).
    let var_log = fs::canonicalize("/var/log").unwrap_or_else(|_| PathBuf::from("/var/log"));
    let tmp = fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"));
    assert_eq!(
        resolve_sls_path(Some("/var/log/custom/tokenless.jsonl")),
        var_log.join("custom/tokenless.jsonl")
    );
    assert_eq!(
        resolve_sls_path(Some("/tmp/tokenless-test.jsonl")),
        tmp.join("tokenless-test.jsonl")
    );
}

#[test]
#[cfg(unix)]
fn test_validate_sls_path_symlinked_prefix() {
    // When /var/log is symlinked to another filesystem (or /private/var/log
    // on macOS), canonicalization resolves it to the real target. Since
    // /var/log is root-owned (creating the symlink needs privilege), the
    // original path is trusted and the resolved path is still accepted.
    let expected_root = fs::canonicalize("/var/log").unwrap_or_else(|_| PathBuf::from("/var/log"));
    let result = validate_sls_path(std::path::Path::new(
        "/var/log/anolisa/sls/ops/tokenless.jsonl",
    ));
    let resolved = result.expect("path under /var/log/ must be accepted");
    assert!(
        resolved.starts_with(&expected_root),
        "expected {resolved:?} to start with {expected_root:?}"
    );
    assert!(
        resolved.ends_with("anolisa/sls/ops/tokenless.jsonl"),
        "unexpected resolved suffix: {resolved:?}"
    );
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
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let link = dir.path().join("escape");
    std::os::unix::fs::symlink("/etc", &link).unwrap();
    let escaped = link.join("cron.d/evil.jsonl");
    assert!(validate_sls_path(&escaped).is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn test_validate_sls_path_macos_private_prefixes() {
    assert_eq!(
        validate_sls_path(std::path::Path::new(
            "/private/var/log/custom/tokenless.jsonl"
        )),
        Some(PathBuf::from("/private/var/log/custom/tokenless.jsonl"))
    );
    assert_eq!(
        validate_sls_path(std::path::Path::new("/private/tmp/tokenless-test.jsonl")),
        Some(PathBuf::from("/private/tmp/tokenless-test.jsonl"))
    );
    assert!(validate_sls_path(std::path::Path::new("/private/etc/passwd")).is_none());
    assert!(validate_sls_path(std::path::Path::new("/private/tmp-evil/tokenless.jsonl")).is_none());
}
