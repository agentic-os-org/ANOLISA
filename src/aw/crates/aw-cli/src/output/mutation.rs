//! Present observed mutation facts without inferring rollback from exit status.

use aw_package::{MutationReport, NoticeCode, RecoveryState};
use serde::Serialize;
use std::io::Write;

#[derive(Serialize)]
pub(super) struct Summary {
    committed: bool,
    recovery: &'static str,
}

impl From<&MutationReport> for Summary {
    fn from(report: &MutationReport) -> Self {
        Self {
            committed: report.committed,
            recovery: match report.recovery {
                RecoveryState::NotAttempted => "not_attempted",
                RecoveryState::Complete => "complete",
                RecoveryState::Incomplete => "incomplete",
            },
        }
    }
}

#[derive(Serialize)]
pub(super) struct Warning {
    code: &'static str,
    path: std::path::PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    io_kind: Option<&'static str>,
}

pub(super) fn warnings(report: &MutationReport) -> Vec<Warning> {
    report
        .notices
        .iter()
        .map(|notice| Warning {
            code: match notice.code {
                NoticeCode::RollbackRemoveFailed => "rollback_remove_failed",
                NoticeCode::RollbackRestoreFailed => "rollback_restore_failed",
                NoticeCode::RollbackRetireFailed => "rollback_retire_failed",
                NoticeCode::BackupRetained => "backup_retained",
                NoticeCode::BackupCleanupFailed => "backup_cleanup_failed",
            },
            path: notice.path.clone(),
            io_kind: notice.io_kind.map(|kind| match kind {
                std::io::ErrorKind::PermissionDenied => "permission_denied",
                std::io::ErrorKind::NotFound => "not_found",
                std::io::ErrorKind::AlreadyExists => "already_exists",
                std::io::ErrorKind::DirectoryNotEmpty => "directory_not_empty",
                _ => "other",
            }),
        })
        .collect()
}

pub(super) fn human(report: &MutationReport) -> std::io::Result<()> {
    let mut stderr = std::io::stderr().lock();
    for warning in warnings(report) {
        writeln!(
            stderr,
            "aw: warning {}: {}{}",
            warning.code,
            super::terminal::escape(&warning.path.to_string_lossy()),
            warning
                .io_kind
                .map(|kind| format!(" ({kind})"))
                .unwrap_or_default()
        )?;
    }
    if report.recovery == RecoveryState::Incomplete {
        writeln!(
            stderr,
            "aw: recovery is incomplete; inspect the reported resources before retrying"
        )?;
    }
    Ok(())
}
