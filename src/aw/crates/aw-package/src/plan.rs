//! Read-only local package plans and execution through one persistent transaction backend.

mod capacity;
mod install;
mod runtime;
mod snapshot;
mod uninstall;

use crate::{
    database::{DatabasePaths, Scope},
    transaction::ChangeSet,
    Error, MutationFailure, MutationReport, MutationResult, Result,
};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// Requested package operation; package upgrades require a separate contract.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Add packages without replacing any existing package or file.
    Install,
    /// Remove selected packages, or the complete prefix when none are selected.
    Uninstall,
}

/// Package and exact version displayed before an operation runs.
#[derive(Clone, Debug, Serialize)]
pub struct PackageSummary {
    /// Producer-selected package ID.
    pub package_id: String,
    /// Exact package version.
    pub version: String,
}

/// One owned file that the requested operation would add or remove.
#[derive(Clone, Debug, Serialize)]
pub struct FileChange {
    /// Add or remove operation for this relative file.
    pub action: Action,
    /// Path relative to the selected prefix.
    pub path: String,
    /// Expected content digest.
    pub sha256: String,
    /// Expected Unix permission bits.
    pub mode: u32,
}

/// Exact same-prefix dependency decision; no remote or PATH lookup is performed.
#[derive(Clone, Debug, Serialize)]
pub struct DependencySummary {
    /// Package declaring this requirement.
    pub package_id: String,
    /// Required package ID.
    pub required_package_id: String,
    /// Required exact version.
    pub version: String,
    /// Persistent installation selected for the requirement; absent until first registration.
    pub installation_id: Option<String>,
}

/// A precondition preventing execution; blocked plans remain useful for dry-run output.
#[derive(Clone, Debug, Serialize)]
pub struct Problem {
    /// Stable machine-readable category.
    pub code: String,
    /// Concrete reason the operation cannot proceed.
    pub message: String,
    /// Affected package, when known.
    pub package_id: Option<String>,
    /// Affected filesystem path, when known.
    pub path: Option<PathBuf>,
}

/// Reviewable operation derived entirely from current local facts.
/// Execution rechecks these facts under exclusive locks and never trusts caller-edited JSON.
#[derive(Clone, Debug, Serialize)]
pub struct PackagePlan {
    /// Plan output format, currently one.
    pub format: u32,
    /// Requested install or uninstall action.
    pub action: Action,
    /// Explicit absolute payload directory.
    pub prefix: PathBuf,
    /// Explicit database privilege boundary.
    pub scope: Scope,
    /// Snapshot generation, absent for a database that does not yet exist.
    pub expected_generation: Option<u64>,
    /// Existing stable ID; new installation IDs are assigned only during execution.
    pub installation_id: Option<String>,
    /// Whether execution will archive and replace old component records.
    pub register_legacy: bool,
    /// Packages to be added.
    pub added: Vec<PackageSummary>,
    /// Packages to be removed.
    pub removed: Vec<PackageSummary>,
    /// Files that would change; unknown files never enter this list.
    pub files: Vec<FileChange>,
    /// Exact dependency decisions used by the plan.
    pub dependencies: Vec<DependencySummary>,
    /// Every observed precondition preventing this operation.
    pub problems: Vec<Problem>,
    #[serde(skip)]
    pub(crate) changes: Option<ChangeSet>,
    #[serde(skip)]
    pub(crate) database_root: PathBuf,
}

impl PackagePlan {
    /// Whether all read-only preconditions passed; execution still rechecks under locks.
    pub fn ready(&self) -> bool {
        self.problems.is_empty()
    }

    pub(crate) fn problem(
        &mut self,
        code: &str,
        message: impl Into<String>,
        package_id: Option<&str>,
        path: Option<PathBuf>,
    ) {
        self.problems.push(Problem {
            code: code.into(),
            message: message.into(),
            package_id: package_id.map(str::to_owned),
            path,
        });
    }
}

/// Inspect a local bundle and derive an additive install plan without filesystem writes.
///
/// # Errors
/// Rejects malformed input, unsafe paths and inconsistent database records.
pub fn install(paths: &DatabasePaths, bundle: &Path, prefix: &Path) -> Result<PackagePlan> {
    install::plan(paths, bundle, prefix)
}

/// Derive a selective removal plan; an empty ID list selects the whole prefix.
///
/// # Errors
/// Rejects malformed paths, duplicate IDs and inconsistent database records.
pub fn uninstall(paths: &DatabasePaths, prefix: &Path, ids: &[String]) -> Result<PackagePlan> {
    uninstall::plan(paths, prefix, ids)
}

/// Apply a library-created plan after reacquiring exclusive locks and rechecking all evidence.
///
/// # Errors
/// Returns blocked/stale plans, cancellation, runtime locks and I/O failures with recovery facts.
pub fn execute(paths: &DatabasePaths, plan: &PackagePlan, cancel: &AtomicBool) -> MutationResult {
    let Some(changes) = plan.changes.as_ref().filter(|_| plan.ready()) else {
        return Err(MutationFailure {
            error: Error::Invalid("package plan is blocked; inspect its problems".into()),
            report: MutationReport::default(),
        });
    };
    if paths.scope() != plan.scope || paths.root != plan.database_root {
        return Err(MutationFailure {
            error: Error::Invalid("package plan scope changed".into()),
            report: MutationReport::default(),
        });
    }
    crate::transaction::apply(paths, changes, cancel)
}
