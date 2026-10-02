//! Bounded reader for version 1 segments and legacy policy-decision logs.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

#[cfg(test)]
use chrono::Utc;
use cosh_types::audit::{
    AuditActor, AuditActorKind, AuditComponent, AuditComponentName, AuditDecisionData,
    AuditEventOutcome, AuditEventType, AuditEventV1, AuditIdentity, AuditOutcomeStatus,
    AuditRedaction, AuditRedactionStatus, AuditSubject, KnownAuditEventType, LogEntry, LogSource,
    Outcome, AUDIT_EVENT_SCHEMA, MAX_AUDIT_RECORD_BYTES,
};
use cosh_types::error::{CoshError, ErrorCode};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::store::validate_private_file;

/// Maximum files inspected by one bounded reader pass.
pub const MAX_AUDIT_FILES: usize = 1024;
/// Maximum source bytes inspected by one bounded reader pass.
pub const MAX_AUDIT_READ_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum complete records inspected by one bounded reader pass.
pub const MAX_AUDIT_RECORDS: usize = 100_000;
const MAX_AUDIT_DIAGNOSTICS: usize = 1_024;

/// Source schema generation of one returned event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSchemaGeneration {
    /// Projected legacy `LogEntry`.
    LegacyV0,
    /// Canonical unified event.
    V1,
}

/// One bounded event plus private ordering metadata.
#[derive(Debug, Clone, Serialize)]
pub struct AuditStoredEvent {
    /// Canonical public event envelope.
    pub event: AuditEventV1,
    /// Source schema generation.
    pub generation: AuditSchemaGeneration,
    /// Segment identifier used only as a deterministic ordering tie-breaker.
    #[serde(skip)]
    pub segment_id: String,
}

/// Stable reader diagnostic category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDiagnosticKind {
    /// An interior record was not valid JSON.
    InvalidJson,
    /// JSON used an unsupported schema name or version.
    UnsupportedSchema,
    /// JSON matched the schema but failed bounded validation.
    InvalidRecord,
    /// An incomplete final line was omitted.
    TrailingPartialRecord,
    /// A source could not be read safely.
    ReadError,
    /// Reader safety limits truncated discovery.
    SafetyLimit,
}

/// Safe reader diagnostic without record data or full paths.
#[derive(Debug, Clone, Serialize)]
pub struct AuditReadDiagnostic {
    /// Stable diagnostic category.
    pub kind: AuditDiagnosticKind,
    /// Safe source basename.
    pub source: String,
    /// One-based source line when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// Bounded canonical reader result.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AuditReadResult {
    /// Ordered canonical and projected events.
    pub events: Vec<AuditStoredEvent>,
    /// Visible corruption, partial-tail, and safety diagnostics.
    pub diagnostics: Vec<AuditReadDiagnostic>,
    /// Whether a safety limit stopped discovery.
    pub truncated: bool,
    /// Number of version 1 active segments discovered.
    pub active_segments: usize,
    /// Number of version 1 closed segments discovered.
    pub closed_segments: usize,
    /// Total bytes of discovered version 1 segment files.
    pub segment_bytes: u64,
    /// Number of legacy files discovered.
    pub legacy_files: usize,
    #[serde(skip)]
    inspected_bytes: u64,
    #[serde(skip)]
    inspected_records: usize,
}

/// Reads canonical segments and optional legacy policy logs under fixed limits.
///
/// # Errors
///
/// Returns an error only when safe evaluation of the requested root cannot
/// proceed. Individual corrupt records and unsafe source files are diagnostics.
pub fn read_all(root: &Path, include_legacy: bool) -> Result<AuditReadResult, CoshError> {
    let mut result = AuditReadResult::default();
    read_v1(root, &mut result)?;
    if include_legacy {
        read_legacy(&mut result)?;
    }
    result.events.sort_by(|left, right| {
        left.event
            .occurred_at
            .cmp(&right.event.occurred_at)
            .then_with(|| left.event.observed_at.cmp(&right.event.observed_at))
            .then_with(|| {
                left.event
                    .component
                    .name
                    .as_str()
                    .cmp(right.event.component.name.as_str())
            })
            .then_with(|| left.segment_id.cmp(&right.segment_id))
            .then_with(|| left.event.sequence.cmp(&right.event.sequence))
    });
    Ok(result)
}

fn read_v1(root: &Path, result: &mut AuditReadResult) -> Result<(), CoshError> {
    let segments = root.join("v1/segments");
    let dates = match std::fs::read_dir(&segments) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(reader_io_error("read segments", &segments, error)),
    };
    let mut files = BTreeSet::new();
    for date in dates {
        let date = match date {
            Ok(date) => date,
            Err(_) => {
                push_diagnostic(result, AuditDiagnosticKind::ReadError, "segments", None);
                continue;
            }
        };
        let metadata = match date.file_type() {
            Ok(kind) if kind.is_dir() && !kind.is_symlink() => kind,
            Ok(_) => continue,
            Err(_) => {
                push_diagnostic(
                    result,
                    AuditDiagnosticKind::ReadError,
                    &date.file_name().to_string_lossy(),
                    None,
                );
                continue;
            }
        };
        let _ = metadata;
        let entries = match std::fs::read_dir(date.path()) {
            Ok(entries) => entries,
            Err(_) => {
                push_diagnostic(
                    result,
                    AuditDiagnosticKind::ReadError,
                    &date.file_name().to_string_lossy(),
                    None,
                );
                continue;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                push_diagnostic(result, AuditDiagnosticKind::ReadError, "segments", None);
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".jsonl") || name.ends_with(".jsonl.active") {
                files.insert(entry.path());
                if files.len() > MAX_AUDIT_FILES {
                    files.pop_last();
                    mark_limit(result, "segments");
                }
            }
        }
    }
    for path in files {
        if result.inspected_records >= MAX_AUDIT_RECORDS
            || result.inspected_bytes >= MAX_AUDIT_READ_BYTES
        {
            mark_limit(result, "segments");
            break;
        }
        let name = safe_basename(&path);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                metadata
            }
            _ => {
                push_diagnostic(result, AuditDiagnosticKind::ReadError, &name, None);
                continue;
            }
        };
        if metadata.len() > MAX_AUDIT_READ_BYTES.saturating_sub(result.inspected_bytes) {
            mark_limit(result, &name);
            break;
        }
        result.inspected_bytes = result.inspected_bytes.saturating_add(metadata.len());
        result.segment_bytes = result.segment_bytes.saturating_add(metadata.len());
        if name.ends_with(".active") {
            result.active_segments += 1;
        } else {
            result.closed_segments += 1;
        }
        let segment_id = segment_id_from_name(&name);
        read_v1_file(&path, segment_id, result);
    }
    Ok(())
}

fn read_v1_file(path: &Path, segment_id: String, result: &mut AuditReadResult) {
    let name = safe_basename(path);
    let file = match open_private_read(path) {
        Ok(file) => file,
        Err(_) => {
            push_diagnostic(result, AuditDiagnosticKind::ReadError, &name, None);
            return;
        }
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut line_number = 0_u64;
    loop {
        line.clear();
        let (bytes, oversized) = match read_bounded_line(&mut reader, &mut line) {
            Ok(result) => result,
            Err(_) => {
                push_diagnostic(
                    result,
                    AuditDiagnosticKind::ReadError,
                    &name,
                    Some(line_number.saturating_add(1)),
                );
                return;
            }
        };
        if bytes == 0 {
            return;
        }
        line_number = line_number.saturating_add(1);
        result.inspected_records = result.inspected_records.saturating_add(1);
        if oversized {
            push_diagnostic(
                result,
                AuditDiagnosticKind::InvalidRecord,
                &name,
                Some(line_number),
            );
            if result.inspected_records >= MAX_AUDIT_RECORDS {
                mark_limit(result, &name);
                return;
            }
            continue;
        }
        if !line.ends_with(b"\n") {
            push_diagnostic(
                result,
                AuditDiagnosticKind::TrailingPartialRecord,
                &name,
                Some(line_number),
            );
            return;
        }
        line.pop();
        if line.is_empty() {
            continue;
        }
        if result.inspected_records >= MAX_AUDIT_RECORDS {
            mark_limit(result, &name);
            return;
        }
        match serde_json::from_slice::<AuditEventV1>(&line) {
            Ok(event) => match event.validate() {
                Ok(()) => result.events.push(AuditStoredEvent {
                    event,
                    generation: AuditSchemaGeneration::V1,
                    segment_id: segment_id.clone(),
                }),
                Err(_) => push_diagnostic(
                    result,
                    AuditDiagnosticKind::InvalidRecord,
                    &name,
                    Some(line_number),
                ),
            },
            Err(_) => {
                let kind = match serde_json::from_slice::<serde_json::Value>(&line) {
                    Ok(value)
                        if value.get("schema").and_then(|value| value.as_str())
                            != Some(AUDIT_EVENT_SCHEMA)
                            || value.get("schema_version").and_then(|value| value.as_u64())
                                != Some(1) =>
                    {
                        AuditDiagnosticKind::UnsupportedSchema
                    }
                    Ok(_) => AuditDiagnosticKind::InvalidRecord,
                    Err(_) => AuditDiagnosticKind::InvalidJson,
                };
                push_diagnostic(result, kind, &name, Some(line_number));
            }
        }
    }
}

fn read_legacy(result: &mut AuditReadResult) -> Result<(), CoshError> {
    let active = super::log::audit_log_path();
    let mut files = Vec::new();
    if active.is_file() {
        files.push(active.clone());
    }
    if let (Some(directory), Some(stem)) = (active.parent(), active.file_name()) {
        if let Ok(entries) = std::fs::read_dir(directory) {
            let prefix = format!("{}.", stem.to_string_lossy());
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(&prefix) && entry.path().is_file() {
                    files.push(entry.path());
                }
            }
        }
    }
    files.sort();
    files.dedup();
    files.truncate(MAX_AUDIT_FILES.saturating_sub(result.active_segments + result.closed_segments));
    result.legacy_files = files.len();
    for path in files {
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| reader_io_error("inspect legacy", &path, error))?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_AUDIT_READ_BYTES.saturating_sub(result.inspected_bytes)
        {
            mark_limit(result, &safe_basename(&path));
            break;
        }
        result.inspected_bytes = result.inspected_bytes.saturating_add(metadata.len());
        read_legacy_file(&path, result)?;
    }
    Ok(())
}

fn read_legacy_file(path: &Path, result: &mut AuditReadResult) -> Result<(), CoshError> {
    let name = safe_basename(path);
    let file = File::open(path).map_err(|error| reader_io_error("read legacy", path, error))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut offset = 0_u64;
    let mut line_number = 0_u64;
    loop {
        line.clear();
        if result.inspected_records >= MAX_AUDIT_RECORDS {
            mark_limit(result, &name);
            break;
        }
        let (count, oversized) = read_bounded_line(&mut reader, &mut line)
            .map_err(|error| reader_io_error("read legacy", path, error))?;
        if count == 0 {
            break;
        }
        line_number = line_number.saturating_add(1);
        result.inspected_records = result.inspected_records.saturating_add(1);
        if oversized {
            push_diagnostic(
                result,
                AuditDiagnosticKind::InvalidRecord,
                &name,
                Some(line_number),
            );
            offset = offset.saturating_add(count as u64);
            continue;
        }
        if !line.ends_with(b"\n") {
            push_diagnostic(
                result,
                AuditDiagnosticKind::TrailingPartialRecord,
                &name,
                Some(line_number),
            );
            break;
        }
        line.pop();
        match serde_json::from_slice::<LogEntry>(&line) {
            Ok(entry) => match project_legacy(path, offset, &line, entry) {
                Ok(event) => result.events.push(event),
                Err(_) => push_diagnostic(
                    result,
                    AuditDiagnosticKind::InvalidRecord,
                    &name,
                    Some(line_number),
                ),
            },
            Err(_) => push_diagnostic(
                result,
                AuditDiagnosticKind::InvalidJson,
                &name,
                Some(line_number),
            ),
        }
        offset = offset.saturating_add(count as u64);
    }
    Ok(())
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<(usize, bool)> {
    let mut limited = reader.by_ref().take((MAX_AUDIT_RECORD_BYTES + 1) as u64);
    let count = limited.read_until(b'\n', line)?;
    let mut total = count;
    let oversized = count > MAX_AUDIT_RECORD_BYTES;
    if oversized && !line.ends_with(b"\n") {
        let mut discarded = Vec::new();
        loop {
            discarded.clear();
            let read = reader
                .by_ref()
                .take(8192)
                .read_until(b'\n', &mut discarded)?;
            total = total.saturating_add(read);
            if read == 0 || discarded.ends_with(b"\n") {
                break;
            }
        }
    }
    Ok((total, oversized))
}

fn project_legacy(
    path: &Path,
    offset: u64,
    line: &[u8],
    entry: LogEntry,
) -> Result<AuditStoredEvent, CoshError> {
    let mut hasher = Sha256::new();
    hasher.update(path.as_os_str().to_string_lossy().as_bytes());
    hasher.update(offset.to_le_bytes());
    hasher.update(Sha256::digest(line));
    let event_id = format!("legacy-{}", hex_bytes(&hasher.finalize()));
    let component_name = match entry.source {
        LogSource::Cli => AuditComponentName::CoshCli,
        LogSource::Tui { .. } => AuditComponentName::CoshShell,
        LogSource::External { .. } => AuditComponentName::CoshCore,
    };
    let decision = match entry.decision.outcome {
        Outcome::Allow => "allow",
        Outcome::Deny => "deny",
        Outcome::RequireApproval => "require_approval",
    };
    let status = match entry.decision.outcome {
        Outcome::Allow => AuditOutcomeStatus::Allowed,
        Outcome::Deny => AuditOutcomeStatus::Denied,
        Outcome::RequireApproval => AuditOutcomeStatus::Started,
    };
    let payload = AuditDecisionData {
        decision: decision.to_string(),
        reason_code: Some("legacy_policy_decision".to_string()),
        policy_version: Some(entry.decision.policy_version),
        duration_ms: None,
    };
    let mut event = AuditEventV1::new(
        event_id,
        AuditEventType::from(KnownAuditEventType::PolicyDecision),
        entry.timestamp,
        entry.timestamp,
        offset,
        AuditComponent {
            name: component_name,
            version: "legacy".to_string(),
        },
        AuditIdentity {
            provider_session_id: Some(entry.session_id),
            ..AuditIdentity::default()
        },
        AuditActor {
            kind: AuditActorKind::User,
            uid: Some(entry.uid),
            euid: Some(entry.euid),
        },
        AuditEventOutcome {
            status,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "policy_action".to_string(),
            name: Some(entry.action.operation),
        },
        &payload,
        AuditRedaction {
            policy_version: "legacy-redaction-v0".to_string(),
            status: if entry.redacted {
                AuditRedactionStatus::Redacted
            } else {
                AuditRedactionStatus::Clean
            },
            fields: Vec::new(),
        },
    )
    .map_err(|error| {
        CoshError::new(
            ErrorCode::AuditCorrupt,
            format!("legacy projection failed: {error}"),
            "audit",
        )
    })?;
    event.legacy_schema = Some(0);
    Ok(AuditStoredEvent {
        event,
        generation: AuditSchemaGeneration::LegacyV0,
        segment_id: format!("legacy-{}", safe_basename(path)),
    })
}

pub(crate) fn open_private_read(path: &Path) -> Result<File, CoshError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|error| reader_io_error("open segment", path, error))?;
    validate_private_file(&file, path)?;
    Ok(file)
}

fn segment_id_from_name(name: &str) -> String {
    name.strip_suffix(".active")
        .unwrap_or(name)
        .strip_suffix(".jsonl")
        .unwrap_or(name)
        .rsplit('-')
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("-")
}

fn push_diagnostic(
    result: &mut AuditReadResult,
    kind: AuditDiagnosticKind,
    source: &str,
    line: Option<u64>,
) {
    if result.diagnostics.len() >= MAX_AUDIT_DIAGNOSTICS {
        result.truncated = true;
        return;
    }
    result.diagnostics.push(AuditReadDiagnostic {
        kind,
        source: source.to_string(),
        line,
    });
}

fn mark_limit(result: &mut AuditReadResult, source: &str) {
    result.truncated = true;
    push_diagnostic(result, AuditDiagnosticKind::SafetyLimit, source, None);
}

fn safe_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<invalid-basename>")
        .to_string()
}

fn reader_io_error(operation: &str, path: &Path, error: std::io::Error) -> CoshError {
    CoshError::new(
        ErrorCode::AuditUnavailable,
        format!("{operation} {}: {error}", safe_basename(path)),
        "audit",
    )
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosh_types::audit::{Action, ActionSubsystem, Decision};

    fn legacy_entry() -> LogEntry {
        LogEntry {
            timestamp: Utc::now(),
            session_id: "session-1".to_string(),
            user: "ignored".to_string(),
            uid: 1000,
            euid: 1000,
            sudo_user: None,
            pid: 42,
            action: Action {
                subsystem: ActionSubsystem::Pkg,
                operation: "install".to_string(),
                target: Some("secret-target".to_string()),
                args: Vec::new(),
                raw: Some("secret-command".to_string()),
            },
            decision: Decision {
                outcome: Outcome::Allow,
                reason: "secret reason".to_string(),
                matched_rule: None,
                policy_version: "test-v1".to_string(),
            },
            source: LogSource::Cli,
            redacted: false,
        }
    }

    #[test]
    fn legacy_projection_is_stable_and_drops_raw_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audit.log");
        let line = serde_json::to_vec(&legacy_entry()).unwrap();
        let first = project_legacy(&path, 0, &line, legacy_entry()).unwrap();
        let second = project_legacy(&path, 0, &line, legacy_entry()).unwrap();
        assert_eq!(first.event.event_id, second.event.event_id);
        let json = serde_json::to_string(&first.event).unwrap();
        assert!(!json.contains("secret-command"));
        assert!(!json.contains("secret-target"));
        assert!(!json.contains("secret reason"));
        assert_eq!(first.event.legacy_schema, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn oversized_record_is_diagnosed_without_becoming_an_event() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::audit::AuditTestDir::create();
        let date = root.path().join("v1/segments/2026-07-23");
        super::super::store::ensure_private_dir(&root.path().join("v1")).unwrap();
        super::super::store::ensure_private_dir(&root.path().join("v1/segments")).unwrap();
        super::super::store::ensure_private_dir(&date).unwrap();
        let path = date.join("cosh-core-1-1-00000000-0000-0000-0000-000000000000.jsonl");
        let mut bytes = vec![b'x'; MAX_AUDIT_RECORD_BYTES + 1];
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let result = read_all(root.path(), false).unwrap();

        assert!(result.events.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].kind,
            AuditDiagnosticKind::InvalidRecord
        );
    }
}
