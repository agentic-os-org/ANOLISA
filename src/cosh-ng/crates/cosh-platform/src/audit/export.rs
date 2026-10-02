//! Fail-closed redacted audit incident bundle export.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use cosh_types::audit::{
    AuditActor, AuditActorKind, AuditComponent, AuditComponentName, AuditControlData,
    AuditEventOutcome, AuditEventType, AuditEventV1, AuditIdentity, AuditOutcomeStatus,
    AuditRedaction, AuditRedactionStatus, AuditSettings, AuditSubject, KnownAuditEventType,
};
use cosh_types::error::{CoshError, ErrorCode};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use super::query::{
    analyze_lifecycles, query_events, AuditEventFilter, AuditSchemaGenerationFilter, MAX_PAGE_SIZE,
};
use super::reader::{AuditDiagnosticKind, AuditReadDiagnostic, AuditStoredEvent};
use super::state::{update_state, AuditStateError};
use super::store::{AuditDurability, AuditSegmentWriter};

const EXPORT_SCHEMA: &str = "cosh.audit.export.manifest";
const EXPORT_REDACTION_POLICY: &str = "audit-export-redaction-v1";

/// Successful audit export result.
#[derive(Debug, Clone, Serialize)]
pub struct AuditExportResult {
    /// Published output basename, never the full path.
    pub output: String,
    /// Exported event count.
    pub events: usize,
    /// Manifest SHA-256.
    pub manifest_sha256: String,
}

#[derive(Debug, Serialize)]
struct ExportManifest {
    schema: &'static str,
    schema_version: u16,
    tool_version: &'static str,
    source_schema: &'static str,
    redaction_policy: &'static str,
    created_at: DateTime<Utc>,
    filters: ExportFilterSummary,
    event_count: usize,
    omitted_count: usize,
    diagnostics: Vec<ExportDiagnostic>,
    time_start: Option<DateTime<Utc>>,
    time_end: Option<DateTime<Utc>>,
    hashes: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ExportFilterSummary {
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    event_type_count: usize,
    component_count: usize,
    outcome_count: usize,
    identity_filter: bool,
    generation: Option<AuditSchemaGenerationFilter>,
}

#[derive(Debug, Serialize)]
struct ExportDiagnostic {
    kind: AuditDiagnosticKind,
    line: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ExportSummary {
    event_count: usize,
    event_types: BTreeMap<String, usize>,
    failure_classes: BTreeMap<String, usize>,
    gap_count: usize,
    identity_aliases: usize,
}

/// Creates and atomically publishes a four-file redacted audit bundle.
///
/// # Errors
///
/// Fails closed for unsafe output targets, query/redaction/serialization/hash/
/// scan errors, permission failures, or publication failures. Staging is
/// removed on every error.
pub fn create_export(
    root: &Path,
    mut filter: AuditEventFilter,
    output: &Path,
    force: bool,
) -> Result<AuditExportResult, CoshError> {
    let existing_output = validate_output(output, force)?;
    filter.normalize();
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let basename = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| export_error("output basename is not valid UTF-8"))?;
    let staging = parent.join(format!(".{basename}.audit-stage-{}", Uuid::new_v4()));
    let backup = parent.join(format!(".{basename}.audit-backup-{}", Uuid::new_v4()));
    let result: Result<AuditExportResult, CoshError> = (|| {
        create_private_directory(&staging)?;
        let (events, diagnostics, safety_truncated) = collect_events(root, filter.clone())?;
        let salt = Uuid::new_v4().to_string();
        let mut aliases = BTreeMap::new();
        let mut event_lines = Vec::new();
        let mut event_types = BTreeMap::new();
        let mut failures = BTreeMap::new();
        for stored in &events {
            let mut value = serde_json::to_value(&stored.event)
                .map_err(|_| export_error("event serialization failed"))?;
            apply_export_allowlist(&mut value, &stored.event.event_type);
            alias_identities(&mut value, &salt, &mut aliases);
            let line = serde_json::to_vec(&value)
                .map_err(|_| export_error("event serialization failed"))?;
            event_lines.extend_from_slice(&line);
            event_lines.push(b'\n');
            let event_type = match stored.event.event_type {
                AuditEventType::Known(known) => known.as_str(),
                AuditEventType::Unknown(_) => "unknown",
            };
            *event_types.entry(event_type.to_string()).or_insert(0) += 1;
            let status = format!("{:?}", stored.event.outcome.status).to_ascii_lowercase();
            if matches!(
                stored.event.outcome.status,
                cosh_types::audit::AuditOutcomeStatus::Failed
                    | cosh_types::audit::AuditOutcomeStatus::Denied
                    | cosh_types::audit::AuditOutcomeStatus::Degraded
            ) {
                *failures.entry(status).or_insert(0) += 1;
            }
        }
        write_private_file(&staging.join("events.jsonl"), &event_lines)?;

        let (gaps, _) = analyze_lifecycles(&events);
        let summary = ExportSummary {
            event_count: events.len(),
            event_types,
            failure_classes: failures,
            gap_count: gaps.len(),
            identity_aliases: aliases.len(),
        };
        let summary_bytes = serde_json::to_vec_pretty(&summary)
            .map_err(|_| export_error("summary serialization failed"))?;
        write_private_file(&staging.join("summary.json"), &summary_bytes)?;

        let mut hashes = BTreeMap::new();
        hashes.insert("events.jsonl".to_string(), sha256_hex(&event_lines));
        hashes.insert("summary.json".to_string(), sha256_hex(&summary_bytes));
        let manifest_filter = ExportFilterSummary {
            since: filter.since,
            until: filter.until,
            event_type_count: filter.event_types.len(),
            component_count: filter.components.len(),
            outcome_count: filter.outcomes.len(),
            identity_filter: filter.identity.is_some(),
            generation: filter.generation,
        };
        let manifest = ExportManifest {
            schema: EXPORT_SCHEMA,
            schema_version: 1,
            tool_version: env!("CARGO_PKG_VERSION"),
            source_schema: "cosh.audit.event@1+legacy@0",
            redaction_policy: EXPORT_REDACTION_POLICY,
            created_at: Utc::now(),
            filters: manifest_filter,
            event_count: events.len(),
            omitted_count: usize::from(safety_truncated),
            diagnostics: diagnostics
                .into_iter()
                .map(|diagnostic| ExportDiagnostic {
                    kind: diagnostic.kind,
                    line: diagnostic.line,
                })
                .collect(),
            time_start: events.first().map(|event| event.event.occurred_at),
            time_end: events.last().map(|event| event.event.occurred_at),
            hashes,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|_| export_error("manifest serialization failed"))?;
        write_private_file(&staging.join("manifest.json"), &manifest_bytes)?;

        let checksum_lines = format!(
            "{}  manifest.json\n{}  summary.json\n{}  events.jsonl\n",
            sha256_hex(&manifest_bytes),
            sha256_hex(&summary_bytes),
            sha256_hex(&event_lines)
        );
        write_private_file(&staging.join("SHA256SUMS"), checksum_lines.as_bytes())?;
        scan_bundle(&staging)?;
        publish(output, &staging, &backup, existing_output.as_ref())?;
        Ok(AuditExportResult {
            output: basename.to_string(),
            events: events.len(),
            manifest_sha256: sha256_hex(&manifest_bytes),
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        if backup.exists() && !output.exists() {
            let _ = std::fs::rename(&backup, output);
        }
    }
    match result {
        Ok(exported) => {
            let event_result = emit_export_created(root, &exported);
            update_export_state(root, event_result.err());
            Ok(exported)
        }
        Err(error) => {
            update_export_state(root, Some(error.clone()));
            Err(error)
        }
    }
}

fn emit_export_created(root: &Path, exported: &AuditExportResult) -> Result<(), CoshError> {
    let mut writer = AuditSegmentWriter::create(root, AuditComponentName::CoshCli)?;
    let now = Utc::now();
    let mut event = AuditEventV1::new(
        Uuid::new_v4().to_string(),
        AuditEventType::from(KnownAuditEventType::ExportCreated),
        now,
        now,
        0,
        AuditComponent {
            name: AuditComponentName::CoshCli,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        AuditIdentity::default(),
        AuditActor {
            kind: AuditActorKind::User,
            uid: Some(nix::unistd::Uid::current().as_raw()),
            euid: Some(nix::unistd::Uid::effective().as_raw()),
        },
        AuditEventOutcome {
            status: AuditOutcomeStatus::Success,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "export".to_string(),
            name: None,
        },
        &AuditControlData {
            operation: Some("redacted_bundle_created".to_string()),
            count: Some(exported.events as u64),
            ..AuditControlData::default()
        },
        AuditRedaction {
            policy_version: EXPORT_REDACTION_POLICY.to_string(),
            status: AuditRedactionStatus::Redacted,
            fields: vec!["output_path".to_string()],
        },
    )
    .map_err(|error| export_error(format!("build export event: {error}")))?;
    writer.append(&mut event, AuditDurability::SecurityBoundary)?;
    writer.close()
}

fn update_export_state(root: &Path, error: Option<CoshError>) {
    let last_export_error = error.map(|error| AuditStateError {
        operation: "export".to_string(),
        code: format!("{:?}", error.code).to_ascii_lowercase(),
        occurred_at: Utc::now(),
    });
    let _ = update_state(root, AuditSettings::default(), |state| {
        state.last_export_error = last_export_error;
    });
}

fn collect_events(
    root: &Path,
    filter: AuditEventFilter,
) -> Result<(Vec<AuditStoredEvent>, Vec<AuditReadDiagnostic>, bool), CoshError> {
    let mut events = Vec::new();
    let mut diagnostics = Vec::new();
    let mut cursor = None;
    let mut truncated = false;
    loop {
        let page = query_events(root, filter.clone(), MAX_PAGE_SIZE, cursor.as_deref())?;
        events.extend(page.events);
        diagnostics.extend(page.diagnostics);
        truncated |= page.safety_truncated;
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    Ok((events, diagnostics, truncated))
}

fn apply_export_allowlist(event: &mut Value, event_type: &AuditEventType) {
    const DATA_FIELDS: &[&str] = &[
        "duration_ms",
        "reason_code",
        "retry_count",
        "provider",
        "model",
        "input_tokens",
        "output_tokens",
        "finish_category",
        "decision",
        "policy_version",
        "tool_kind",
        "input_shape",
        "input_hash",
        "execution_path",
        "result_category",
        "output_bytes",
        "truncated",
        "output_ref",
        "risk",
        "assessment",
        "wait_ms",
        "preview_hash",
        "program",
        "command_hash",
        "cwd_hash",
        "exit_code",
        "scheme",
        "evidence_type",
        "size_category",
        "range_category",
        "operation",
        "error_code",
        "count",
        "bytes",
    ];
    if matches!(event_type, AuditEventType::Unknown(_)) {
        event["event_type"] = Value::String("unknown".to_string());
    }
    if let Some(actor) = event.get_mut("actor").and_then(Value::as_object_mut) {
        actor.remove("uid");
        actor.remove("euid");
    }
    if let Some(subject) = event.get_mut("subject").and_then(Value::as_object_mut) {
        subject.remove("name");
    }
    if let Some(outcome) = event.get_mut("outcome").and_then(Value::as_object_mut) {
        outcome.remove("code");
    }
    if let Some(data) = event.get_mut("data").and_then(Value::as_object_mut) {
        data.retain(|key, _| DATA_FIELDS.contains(&key.as_str()));
    }
    if let Some(redaction) = event.get_mut("redaction").and_then(Value::as_object_mut) {
        redaction.insert(
            "policy_version".to_string(),
            Value::String(EXPORT_REDACTION_POLICY.to_string()),
        );
    }
}

fn alias_identities(
    event: &mut Value,
    salt: &str,
    aliases: &mut BTreeMap<(String, String), String>,
) {
    let Some(identity) = event.get_mut("identity").and_then(Value::as_object_mut) else {
        return;
    };
    for (kind, value) in identity.iter_mut() {
        let Some(original) = value.as_str() else {
            continue;
        };
        let key = (kind.clone(), original.to_string());
        let alias = aliases.entry(key).or_insert_with(|| {
            let digest = sha256_hex(format!("{salt}:{kind}:{original}").as_bytes());
            format!("{}-{}", alias_prefix(kind), &digest[..12])
        });
        *value = Value::String(alias.clone());
    }
}

fn alias_prefix(kind: &str) -> &str {
    match kind {
        "installation_id" => "installation",
        "shell_session_id" | "provider_session_id" => "session",
        "run_id" => "run",
        "turn_id" => "turn",
        "request_id" => "request",
        "tool_use_id" => "tool",
        "command_id" => "command",
        _ => "identity",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn validate_output(output: &Path, force: bool) -> Result<Option<OutputIdentity>, CoshError> {
    let metadata = match std::fs::symlink_metadata(output) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(export_io_error("inspect output", output, error)),
    };
    if !force {
        return Err(export_error("audit export output already exists"));
    }
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(export_error(
            "--force accepts only an existing cosh audit export directory",
        ));
    }
    validate_existing_bundle(output)?;
    Ok(Some(output_identity(&metadata)))
}

fn validate_existing_bundle(output: &Path) -> Result<(), CoshError> {
    let manifest = output.join("manifest.json");
    let bytes = std::fs::read(&manifest)
        .map_err(|_| export_error("existing directory has no valid audit manifest"))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| export_error("existing directory has no valid audit manifest"))?;
    if value.get("schema").and_then(Value::as_str) != Some(EXPORT_SCHEMA)
        || value.get("schema_version").and_then(Value::as_u64) != Some(1)
    {
        return Err(export_error(
            "existing directory has no valid audit manifest",
        ));
    }
    Ok(())
}

fn output_identity(metadata: &std::fs::Metadata) -> OutputIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        OutputIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        OutputIdentity {}
    }
}

fn publish(
    output: &Path,
    staging: &Path,
    backup: &Path,
    existing: Option<&OutputIdentity>,
) -> Result<(), CoshError> {
    if let Some(expected) = existing {
        std::fs::rename(output, backup)
            .map_err(|error| export_io_error("stage replacement", output, error))?;
        let actual = std::fs::symlink_metadata(backup)
            .map(|metadata| output_identity(&metadata))
            .map_err(|error| export_io_error("verify replacement", backup, error));
        if actual.as_ref().ok() != Some(expected) || validate_existing_bundle(backup).is_err() {
            restore_backup(output, backup)?;
            return Err(export_error(
                "audit export output changed during replacement",
            ));
        }
    }
    if let Err(error) = std::fs::rename(staging, output) {
        if backup.exists() {
            let _ = std::fs::rename(backup, output);
        }
        return Err(export_io_error("publish export", staging, error));
    }
    if backup.exists() {
        std::fs::remove_dir_all(backup)
            .map_err(|error| export_io_error("remove replaced export", backup, error))?;
    }
    Ok(())
}

fn restore_backup(output: &Path, backup: &Path) -> Result<(), CoshError> {
    if std::fs::symlink_metadata(output).is_ok() {
        return Err(export_error(
            "cannot restore replaced export because output path is occupied",
        ));
    }
    std::fs::rename(backup, output)
        .map_err(|error| export_io_error("restore replaced export", backup, error))
}

fn create_private_directory(path: &Path) -> Result<(), CoshError> {
    std::fs::create_dir(path).map_err(|error| export_io_error("create staging", path, error))?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| export_io_error("set staging mode", path, error))?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), CoshError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    let mut file = options
        .open(path)
        .map_err(|error| export_io_error("create export file", path, error))?;
    file.write_all(bytes)
        .map_err(|error| export_io_error("write export file", path, error))?;
    file.sync_data()
        .map_err(|error| export_io_error("sync export file", path, error))?;
    Ok(())
}

fn scan_bundle(directory: &Path) -> Result<(), CoshError> {
    for name in ["manifest.json", "summary.json"] {
        let path = directory.join(name);
        let bytes =
            std::fs::read(&path).map_err(|error| export_io_error("scan export", &path, error))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| export_error("final audit export is not valid JSON"))?;
        scan_json_value(&value)?;
    }
    let events_path = directory.join("events.jsonl");
    let events = std::fs::read_to_string(&events_path)
        .map_err(|error| export_io_error("scan export", &events_path, error))?;
    for line in events.lines() {
        let value: Value = serde_json::from_str(line)
            .map_err(|_| export_error("final audit export is not valid JSONL"))?;
        scan_json_value(&value)?;
    }
    let checksum_path = directory.join("SHA256SUMS");
    let checksums = std::fs::read_to_string(&checksum_path)
        .map_err(|error| export_io_error("scan export", &checksum_path, error))?;
    let checksum_pattern = Regex::new(
        r"(?m)\A[0-9a-f]{64}  manifest\.json\n[0-9a-f]{64}  summary\.json\n[0-9a-f]{64}  events\.jsonl\n\z",
    )
    .map_err(|_| export_error("initialize checksum scanner"))?;
    if !checksum_pattern.is_match(&checksums) {
        return Err(export_error(
            "final audit export checksum format is invalid",
        ));
    }
    Ok(())
}

fn scan_json_value(value: &Value) -> Result<(), CoshError> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_sensitive_key(key) {
                    return Err(export_error(
                        "final audit export contains a sensitive field",
                    ));
                }
                scan_json_value(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                scan_json_value(value)?;
            }
        }
        Value::String(value)
            if value.chars().any(char::is_control)
                || Path::new(value).is_absolute()
                || value.starts_with("~/")
                || secret_patterns()?
                    .iter()
                    .any(|pattern| pattern.is_match(value)) =>
        {
            return Err(export_error("final audit export secret scan failed"));
        }
        _ => {}
    }
    Ok(())
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "password"
            | "passwd"
            | "secret"
            | "token"
            | "api_key"
            | "access_token"
            | "refresh_token"
            | "authorization"
            | "cookie"
            | "credential"
            | "private_key"
            | "access_key_secret"
            | "client_secret"
    )
}

fn secret_patterns() -> Result<&'static [Regex], CoshError> {
    static PATTERNS: OnceLock<Result<Vec<Regex>, String>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            [
                r"(?i)-----BEGIN [A-Z ]*PRIVATE KEY-----",
                r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}",
                r"(?i)\bBasic\s+[A-Za-z0-9+/=]{8,}",
                r"\bgh[pousr]_[A-Za-z0-9]{20,}",
                r"\bgithub_pat_[A-Za-z0-9_]{20,}",
                r"\bsk-[A-Za-z0-9_-]{16,}",
                r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
                r"\bLTAI[A-Za-z0-9]{12,}\b",
                r"(?i)\b[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@",
                r"(?i)\b[A-Z]:[\\/]",
                r"\\\\[^\\]+",
            ]
            .into_iter()
            .map(Regex::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(|_| export_error("initialize secret scanner"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn export_error(message: impl Into<String>) -> CoshError {
    CoshError::new(ErrorCode::AuditExportError, message, "audit")
}

fn export_io_error(operation: &str, path: &Path, error: std::io::Error) -> CoshError {
    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<invalid-basename>");
    export_error(format!("{operation} {basename}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_rejects_unrelated_directory() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("existing");
        std::fs::create_dir(&output).unwrap();
        assert!(validate_output(&output, true).is_err());
    }

    #[test]
    fn secret_scanner_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "manifest.json",
            "summary.json",
            "events.jsonl",
            "SHA256SUMS",
        ] {
            std::fs::write(root.path().join(name), b"clean").unwrap();
        }
        std::fs::write(root.path().join("events.jsonl"), b"Bearer synthetic-secret").unwrap();
        assert!(scan_bundle(root.path()).is_err());
    }
}
