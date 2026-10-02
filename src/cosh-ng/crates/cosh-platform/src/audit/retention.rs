//! Deterministic retention planning and lock-safe orphan recovery.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use cosh_types::audit::{
    AuditActor, AuditActorKind, AuditComponent, AuditComponentName, AuditControlData,
    AuditEventOutcome, AuditEventType, AuditEventV1, AuditIdentity, AuditOutcomeStatus,
    AuditRedaction, AuditRedactionStatus, AuditSettings, AuditSubject, KnownAuditEventType,
};
use cosh_types::error::{CoshError, ErrorCode};
use nix::fcntl::{Flock, FlockArg};
use serde::Serialize;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::state::{read_state, update_state, AuditOperationalState, AuditStateError};
use super::store::{close_without_replace, ensure_private_dir, validate_private_file};
use super::store::{AuditDurability, AuditSegmentWriter};

/// Stable reason a closed segment is selected for deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionReason {
    /// Segment modification time is older than the configured age limit.
    Age,
    /// Oldest-first deletion is required to meet the disk cap.
    DiskCap,
}

/// One deterministic closed-segment retention candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetentionCandidate {
    /// Safe segment basename.
    pub basename: String,
    /// Segment size in bytes.
    pub bytes: u64,
    /// Filesystem observation time used for ordering.
    pub timestamp: DateTime<Utc>,
    /// Selection reason.
    pub reason: RetentionReason,
}

/// Deterministic retention plan shared by dry-run and execution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetentionPlan {
    /// Total bytes across discovered active and closed segments.
    pub total_bytes: u64,
    /// Bytes selected for removal.
    pub selected_bytes: u64,
    /// Ordered closed-segment candidates.
    pub candidates: Vec<RetentionCandidate>,
    /// Whether discovery stopped at a fixed safety limit.
    pub truncated: bool,
}

/// Result of one crash-orphan recovery attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrphanRecovery {
    /// Safe recovered segment basename.
    pub basename: String,
    /// Whether the recovered segment ended in a partial record.
    pub trailing_partial_record: bool,
}

/// Result of an executed retention pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetentionExecution {
    /// Completion status of the coordinated pass.
    pub status: RetentionExecutionStatus,
    /// Plan computed immediately before deletion.
    pub plan: RetentionPlan,
    /// Successfully deleted segment basenames.
    pub deleted: Vec<String>,
    /// Safe basenames that could not be deleted.
    pub failed: Vec<String>,
    /// Crash-orphan segments recovered before planning.
    pub recovered: Vec<OrphanRecovery>,
}

/// Outcome of one coordinated retention execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionExecutionStatus {
    /// Another live process owns the retention lock.
    SkippedBusy,
    /// Every selected deletion and audit event completed.
    #[default]
    Complete,
    /// At least one selected deletion failed.
    Partial,
}

/// Starts a best-effort retention pass when the last successful pass is older than 24 hours.
///
/// The returned thread is intentionally detached by long-lived callers. Coordination remains
/// bounded by `retention.lock`, so concurrent Core processes cannot delete the same segment.
// Keep the explicit predicates aligned with the query filter implementation.
#[allow(clippy::unnecessary_map_or)]
pub fn schedule_retention(root: PathBuf, settings: AuditSettings, component: AuditComponentName) {
    let now = Utc::now();
    let due = read_state(&root)
        .ok()
        .flatten()
        .and_then(|state| state.last_retention_success)
        .map_or(true, |last| {
            now.signed_duration_since(last) >= Duration::hours(24)
        });
    if !due {
        return;
    }
    std::thread::spawn(move || {
        let result = execute_retention(&root, &settings, Utc::now());
        let mut state = read_state(&root)
            .ok()
            .flatten()
            .unwrap_or_else(|| AuditOperationalState::new(settings.clone()));
        match result {
            Ok(execution) => {
                if execution.status == RetentionExecutionStatus::SkippedBusy {
                    return;
                }
                let event_result = emit_retention_event(&root, component, &execution);
                if execution.status == RetentionExecutionStatus::Complete && event_result.is_ok() {
                    state.last_retention_success = Some(Utc::now());
                    state.last_retention_error = None;
                } else {
                    state.last_retention_error = Some(AuditStateError {
                        operation: "retention".to_string(),
                        code: if event_result.is_err() {
                            "event_write_failed".to_string()
                        } else {
                            "partial_delete".to_string()
                        },
                        occurred_at: Utc::now(),
                    });
                }
            }
            Err(error) => {
                state.last_retention_error = Some(AuditStateError {
                    operation: "retention".to_string(),
                    code: format!("{:?}", error.code).to_ascii_lowercase(),
                    occurred_at: Utc::now(),
                });
            }
        }
        let _ = update_state(&root, settings, |current| {
            current.last_retention_error = state.last_retention_error;
            current.last_retention_success = state.last_retention_success;
        });
    });
}

fn emit_retention_event(
    root: &Path,
    component: AuditComponentName,
    execution: &RetentionExecution,
) -> Result<(), CoshError> {
    let mut writer = AuditSegmentWriter::create(root, component.clone())?;
    let now = Utc::now();
    let payload = AuditControlData {
        operation: Some("scheduled_retention".to_string()),
        count: Some(execution.deleted.len() as u64),
        bytes: Some(
            execution
                .plan
                .candidates
                .iter()
                .filter(|candidate| execution.deleted.contains(&candidate.basename))
                .map(|candidate| candidate.bytes)
                .sum(),
        ),
        ..AuditControlData::default()
    };
    let mut event = AuditEventV1::new(
        uuid::Uuid::new_v4().to_string(),
        AuditEventType::from(KnownAuditEventType::RetentionPruned),
        now,
        now,
        0,
        AuditComponent {
            name: component,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        AuditIdentity::default(),
        AuditActor {
            kind: AuditActorKind::System,
            uid: None,
            euid: None,
        },
        AuditEventOutcome {
            status: if execution.failed.is_empty() {
                AuditOutcomeStatus::Success
            } else {
                AuditOutcomeStatus::Failed
            },
            code: None,
            retryable: !execution.failed.is_empty(),
        },
        AuditSubject {
            kind: "retention".to_string(),
            name: None,
        },
        &payload,
        AuditRedaction {
            policy_version: "audit-redaction-v1".to_string(),
            status: AuditRedactionStatus::Clean,
            fields: Vec::new(),
        },
    )
    .map_err(|error| retention_error(format!("build retention event: {error}")))?;
    writer.append(&mut event, AuditDurability::SecurityBoundary)?;
    writer.close()
}

#[derive(Debug)]
struct SegmentInfo {
    path: PathBuf,
    basename: String,
    bytes: u64,
    timestamp: DateTime<Utc>,
    active: bool,
}

/// Plans age-first then oldest-first disk-cap retention without mutation.
///
/// # Errors
///
/// Returns a safe audit error when the segment tree cannot be inspected.
pub fn plan_retention(
    root: &Path,
    settings: &AuditSettings,
    now: DateTime<Utc>,
) -> Result<RetentionPlan, CoshError> {
    let (segments, truncated) = discover_segments(root)?;
    Ok(build_retention_plan(segments, truncated, settings, now))
}

/// Plans retention without mutating recoverable orphan segments.
///
/// Unlocked active files are modeled as the closed files that execution would
/// produce, while live locked writers remain active and ineligible.
///
/// # Errors
///
/// Returns a safe audit error when the segment tree cannot be inspected.
pub fn plan_retention_dry_run(
    root: &Path,
    settings: &AuditSettings,
    now: DateTime<Utc>,
) -> Result<RetentionPlan, CoshError> {
    let (mut segments, truncated) = discover_segments(root)?;
    let mut orphan_locks = Vec::new();
    for segment in segments.iter_mut().filter(|segment| segment.active) {
        let Ok(file) = open_existing_private(&segment.path) else {
            continue;
        };
        let Ok(lock) = Flock::lock(file, FlockArg::LockExclusiveNonblock) else {
            continue;
        };
        segment.active = false;
        segment.basename = segment.basename.trim_end_matches(".active").to_string();
        orphan_locks.push(lock);
    }
    let plan = build_retention_plan(segments, truncated, settings, now);
    drop(orphan_locks);
    Ok(plan)
}

fn build_retention_plan(
    mut segments: Vec<SegmentInfo>,
    truncated: bool,
    settings: &AuditSettings,
    now: DateTime<Utc>,
) -> RetentionPlan {
    segments.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.basename.cmp(&right.basename))
    });
    let total_bytes = segments
        .iter()
        .fold(0_u64, |total, segment| total.saturating_add(segment.bytes));
    let cutoff = now - Duration::days(i64::from(settings.retention_days));
    let mut candidates = Vec::new();
    let mut selected_bytes = 0_u64;

    for segment in segments.iter().filter(|segment| !segment.active) {
        if segment.timestamp < cutoff {
            selected_bytes = selected_bytes.saturating_add(segment.bytes);
            candidates.push(RetentionCandidate {
                basename: segment.basename.clone(),
                bytes: segment.bytes,
                timestamp: segment.timestamp,
                reason: RetentionReason::Age,
            });
        }
    }

    let mut remaining = total_bytes.saturating_sub(selected_bytes);
    if remaining > settings.max_disk_bytes {
        for segment in segments.iter().filter(|segment| !segment.active) {
            if remaining <= settings.max_disk_bytes {
                break;
            }
            if candidates
                .iter()
                .any(|candidate| candidate.basename == segment.basename)
            {
                continue;
            }
            candidates.push(RetentionCandidate {
                basename: segment.basename.clone(),
                bytes: segment.bytes,
                timestamp: segment.timestamp,
                reason: RetentionReason::DiskCap,
            });
            selected_bytes = selected_bytes.saturating_add(segment.bytes);
            remaining = remaining.saturating_sub(segment.bytes);
        }
    }

    RetentionPlan {
        total_bytes,
        selected_bytes,
        candidates,
        truncated,
    }
}

/// Recovers only active segments whose advisory lock can be acquired.
///
/// # Errors
///
/// Returns an error when the segment tree cannot be inspected safely. Locked
/// live segments are skipped and are not errors.
pub fn recover_orphans(root: &Path) -> Result<Vec<OrphanRecovery>, CoshError> {
    let (segments, _) = discover_segments(root)?;
    let mut recovered = Vec::new();
    for segment in segments.into_iter().filter(|segment| segment.active) {
        let file = match open_existing_private(&segment.path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let mut locked = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(locked) => locked,
            Err(_) => continue,
        };
        let trailing_partial_record = has_partial_tail(&mut locked).unwrap_or(true);
        let closed = segment
            .path
            .with_file_name(segment.basename.trim_end_matches(".active"));
        close_without_replace(&segment.path, &closed)
            .map_err(|error| retention_io_error("recover orphan", &segment.path, error))?;
        recovered.push(OrphanRecovery {
            basename: closed
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<invalid-basename>")
                .to_string(),
            trailing_partial_record,
        });
    }
    recovered.sort_by(|left, right| left.basename.cmp(&right.basename));
    Ok(recovered)
}

/// Executes one bounded retention pass under `retention.lock`.
///
/// # Errors
///
/// Returns a stable error when lock setup or safe discovery cannot proceed.
/// A busy lock returns an execution with no mutation.
pub fn execute_retention(
    root: &Path,
    settings: &AuditSettings,
    now: DateTime<Utc>,
) -> Result<RetentionExecution, CoshError> {
    ensure_private_dir(root)?;
    ensure_private_dir(&root.join("v1"))?;
    let Some(_lock) = try_retention_lock(root)? else {
        return Ok(RetentionExecution {
            status: RetentionExecutionStatus::SkippedBusy,
            ..RetentionExecution::default()
        });
    };
    let recovered = recover_orphans(root)?;
    let plan = plan_retention(root, settings, now)?;
    let (segments, _) = discover_segments(root)?;
    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    for candidate in &plan.candidates {
        let Some(segment) = segments
            .iter()
            .find(|segment| !segment.active && segment.basename == candidate.basename)
        else {
            failed.push(candidate.basename.clone());
            continue;
        };
        match std::fs::remove_file(&segment.path) {
            Ok(()) => deleted.push(candidate.basename.clone()),
            Err(_) => failed.push(candidate.basename.clone()),
        }
    }
    Ok(RetentionExecution {
        status: if failed.is_empty() {
            RetentionExecutionStatus::Complete
        } else {
            RetentionExecutionStatus::Partial
        },
        plan,
        deleted,
        failed,
        recovered,
    })
}

fn try_retention_lock(root: &Path) -> Result<Option<Flock<File>>, CoshError> {
    let path = root.join("v1/retention.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    let file = options
        .open(&path)
        .map_err(|error| retention_io_error("open retention lock", &path, error))?;
    validate_private_file(&file, &path)?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(Some(lock)),
        Err(_) => Ok(None),
    }
}

fn discover_segments(root: &Path) -> Result<(Vec<SegmentInfo>, bool), CoshError> {
    const LIMIT: usize = 4096;
    let directory = root.join("v1/segments");
    let dates = match std::fs::read_dir(&directory) {
        Ok(dates) => dates,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), false));
        }
        Err(error) => return Err(retention_io_error("read segments", &directory, error)),
    };
    let mut paths = BTreeSet::new();
    let mut truncated = false;
    for date in dates.flatten() {
        if !date.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(date.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let basename = entry.file_name().to_string_lossy().into_owned();
            let active = basename.ends_with(".jsonl.active");
            if !active && !basename.ends_with(".jsonl") {
                continue;
            }
            paths.insert(entry.path());
            if paths.len() > LIMIT {
                paths.pop_last();
                truncated = true;
            }
        }
    }
    let mut segments = Vec::with_capacity(paths.len());
    for path in paths {
        let basename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let active = basename.ends_with(".jsonl.active");
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                metadata
            }
            _ => continue,
        };
        let timestamp = metadata
            .modified()
            .map(DateTime::<Utc>::from)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        segments.push(SegmentInfo {
            path,
            basename,
            bytes: metadata.len(),
            timestamp,
            active,
        });
    }
    Ok((segments, truncated))
}

fn open_existing_private(path: &Path) -> Result<File, CoshError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|error| retention_io_error("open active segment", path, error))?;
    validate_private_file(&file, path)?;
    Ok(file)
}

fn has_partial_tail(file: &mut File) -> std::io::Result<bool> {
    let length = file.metadata()?.len();
    if length == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte)?;
    Ok(byte[0] != b'\n')
}

fn retention_io_error(operation: &str, path: &Path, error: std::io::Error) -> CoshError {
    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<invalid-basename>");
    CoshError::new(
        ErrorCode::AuditUnavailable,
        format!("{operation} {basename}: {error}"),
        "audit",
    )
}

fn retention_error(message: impl Into<String>) -> CoshError {
    CoshError::new(ErrorCode::AuditLogError, message, "audit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn private_root() -> crate::audit::AuditTestDir {
        let root = crate::audit::AuditTestDir::create();
        ensure_private_dir(&root.path().join("v1")).unwrap();
        ensure_private_dir(&root.path().join("v1/segments")).unwrap();
        root
    }

    #[cfg(unix)]
    #[test]
    fn planner_selects_age_before_disk_cap_and_never_active() {
        let root = private_root();
        let date = root.path().join("v1/segments/2020-01-01");
        ensure_private_dir(&date).unwrap();
        std::fs::write(date.join("old.jsonl"), vec![0_u8; 10]).unwrap();
        std::fs::write(date.join("live.jsonl.active"), vec![0_u8; 100]).unwrap();
        #[cfg(unix)]
        for path in [date.join("old.jsonl"), date.join("live.jsonl.active")] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let settings = AuditSettings {
            retention_days: 1,
            max_disk_bytes: 1,
            ..AuditSettings::default()
        };
        let plan = plan_retention(root.path(), &settings, Utc::now() + Duration::days(2)).unwrap();
        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.candidates[0].basename, "old.jsonl");
        assert_eq!(plan.candidates[0].reason, RetentionReason::Age);
    }
}
