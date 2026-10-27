//! Recovery of AW-owned Provider extension transactions.

use crate::{database::DatabasePaths, Result};
use std::sync::atomic::AtomicBool;

pub use crate::store::transaction::RecoveryReport;

/// Recover interrupted AW changes without claiming another manager's database.
///
/// # Errors
/// Rejects foreign databases, unsafe journals, cancellation and unavailable locks.
pub fn recover(paths: &DatabasePaths, cancel: &AtomicBool) -> Result<RecoveryReport> {
    crate::require_aw_database(paths)?;
    crate::store::transaction::recover_checked(paths, cancel, |database| {
        crate::database::require_provider_installations(&database.installations)
    })
}
