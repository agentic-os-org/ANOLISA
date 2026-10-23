//! Durable local package changes: the database rename commits; replay preserves conflicting files.

pub(crate) mod capacity;
mod engine;
mod fs;
mod journal;
mod prepare;
mod replay;
#[cfg(test)]
mod tests;

use crate::{
    database::{DatabasePaths, InstalledDatabase},
    MutationResult, Payload, Result,
};
use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf, sync::atomic::AtomicBool};

/// One verified bundle payload; no package process runs during staging.
#[derive(Clone, Debug)]
pub(crate) struct SourceFile {
    pub source: PathBuf,
    pub metadata: Payload,
}

/// Complete before/after facts produced by planning, rechecked under exclusive locks.
#[derive(Clone, Debug)]
pub(crate) struct ChangeSet {
    pub prefix: PathBuf,
    pub database_before: Option<Vec<u8>>,
    pub database_after: InstalledDatabase,
    pub installation_id: String,
    pub verify: BTreeMap<String, Payload>,
    pub writes: BTreeMap<String, SourceFile>,
    pub removes: BTreeMap<String, Payload>,
    pub legacy: BTreeMap<String, Vec<u8>>,
    pub marker_before: Option<Vec<u8>>,
    pub marker_after: Option<Vec<u8>>,
    pub source_records: BTreeMap<PathBuf, Vec<u8>>,
}

/// Result of explicit replay; a committed database is never rolled back.
#[derive(Debug, Default, Serialize)]
pub struct RecoveryReport {
    /// Number of transactions whose published database required finishing cleanup.
    pub committed: usize,
    /// Number of transactions whose unpublished changes were safely undone.
    pub rolled_back: usize,
    /// Paths retaining backups because their ownership or current contents conflicted.
    pub conflicts: Vec<PathBuf>,
}

/// Replay interrupted package changes in the selected database, without starting programs.
///
/// # Errors
/// Rejects unknown journals, unsafe paths, unavailable exclusive locks, changed databases,
/// and cancellation. Ownership conflicts are returned in the report; bytes and journals remain.
pub fn recover(paths: &DatabasePaths, cancel: &AtomicBool) -> Result<RecoveryReport> {
    replay::recover(paths, cancel)
}

pub(crate) fn apply(
    paths: &DatabasePaths,
    changes: &ChangeSet,
    cancel: &AtomicBool,
) -> MutationResult {
    engine::apply(paths, changes, cancel)
}
