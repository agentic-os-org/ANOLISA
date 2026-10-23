//! Observed outcomes of one existing Preview mutation, without terminal output.

use crate::Error;
use std::{io::ErrorKind, path::PathBuf};

/// Whether recovery ran for resources tracked by this invocation.
/// This does not describe machine-wide state, runtime activity, or durability.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecoveryState {
    /// No recovery was attempted; this is not a claim that no writes occurred.
    #[default]
    NotAttempted,
    /// All attempted recovery of tracked resources completed.
    Complete,
    /// At least one tracked resource could not be recovered or cleaned up.
    Incomplete,
}

/// A filesystem event that requires visibility without replacing the primary error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoticeCode {
    /// A tracked new file or directory could not be removed during rollback.
    RollbackRemoveFailed,
    /// A removed payload or receipt could not be restored.
    RollbackRestoreFailed,
    /// The invocation's new prefix or its owned lock could not be retired.
    RollbackRetireFailed,
    /// Recovery bytes remain in an uninstall backup directory.
    BackupRetained,
    /// An uninstall backup could not be fully removed.
    BackupCleanupFailed,
}

/// One observed cleanup or recovery event; callers choose presentation and escaping.
#[derive(Debug)]
pub struct MutationNotice {
    /// Stable event classification, independent of filesystem error text.
    pub code: NoticeCode,
    /// The tracked resource involved in this event, not a new authorization target.
    pub path: PathBuf,
    /// Filesystem failure category when the notice was caused by an I/O error.
    pub io_kind: Option<ErrorKind>,
}

/// Facts returned after one operation and its in-process recovery have finished.
#[derive(Debug, Default)]
pub struct MutationReport {
    /// Whether the operation passed its existing payload/receipt commit point.
    /// False does not prove that the filesystem is unchanged.
    pub committed: bool,
    /// Recovery of this invocation's tracked resources, not a crash-recovery promise.
    pub recovery: RecoveryState,
    /// Observed warnings, including cleanup failures after a successful commit.
    pub notices: Vec<MutationNotice>,
}

/// The primary failure and independently observed recovery facts.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct MutationFailure {
    /// Original operation failure; cancellation has its own typed variant.
    #[source]
    pub error: Error,
    /// Facts retained even when recovery or cleanup also failed.
    pub report: MutationReport,
}

/// A committed operation may still return notices about incomplete backup cleanup.
pub type MutationResult = std::result::Result<MutationReport, MutationFailure>;

impl MutationReport {
    /// Record a known branch without copying a potentially private error string.
    pub(super) fn notice(&mut self, code: NoticeCode, path: PathBuf, io_kind: Option<ErrorKind>) {
        self.notices.push(MutationNotice {
            code,
            path,
            io_kind,
        });
    }

    /// Preserve primary failure independently from resource-recovery observations.
    pub(super) fn finish(self, result: crate::Result<()>) -> MutationResult {
        match result {
            Ok(()) => Ok(self),
            Err(error) => Err(MutationFailure {
                error,
                report: self,
            }),
        }
    }
}
