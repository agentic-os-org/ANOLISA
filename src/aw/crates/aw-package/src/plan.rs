//! Provider admission for read-only extension plans and private file transactions.

use crate::{
    database::{DatabasePaths, Inventory},
    MutationFailure, MutationReport, MutationResult, Result,
};
use serde::Serialize;
use std::{ops::Deref, path::Path, sync::atomic::AtomicBool};

pub use crate::store::plan::{Action, DependencySummary, FileChange, PackageSummary, Problem};

/// An AW-admitted installation or removal plan with read-only operation details.
/// The private inner plan prevents executing a generic software plan through AW.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct PackagePlan(crate::store::plan::PackagePlan);

impl Deref for PackagePlan {
    type Target = crate::store::plan::PackagePlan;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Plan installation of a declared Provider extension.
///
/// # Errors
/// Rejects malformed bundles, unsafe paths and inconsistent package records.
pub fn install(paths: &DatabasePaths, bundle: &Path, prefix: &Path) -> Result<PackagePlan> {
    crate::require_aw_database(paths)?;
    crate::database::reject_legacy_prefix(prefix)?;
    admit(
        paths,
        prefix,
        crate::store::plan::install_checked(paths, bundle, prefix, admit_bundle)?,
    )
    .map(PackagePlan)
}

fn admit_bundle(input: &crate::store::open_bundle::Bundle) -> Result<()> {
    crate::require(
        input.packages.iter().all(|package| {
            package.provider.is_some() && package.package_id != "aw-core"
        }),
        "AW installs local Provider extensions; use the distribution or native package manager for ordinary software",
    )?;
    Ok(())
}

/// Plan selective removal from a registered AW Provider installation.
///
/// # Errors
/// Rejects unsafe paths, inconsistent package records and duplicate package IDs.
pub fn uninstall(paths: &DatabasePaths, prefix: &Path, ids: &[String]) -> Result<PackagePlan> {
    crate::require_aw_database(paths)?;
    crate::database::reject_legacy_prefix(prefix)?;
    admit(
        paths,
        prefix,
        crate::store::plan::uninstall(paths, prefix, ids)?,
    )
    .map(PackagePlan)
}

/// Execute an AW-created plan while retaining the package engine's ownership checks.
///
/// # Errors
/// Rejects foreign databases, blocked or stale plans, contention and filesystem failures.
pub fn execute(paths: &DatabasePaths, plan: &PackagePlan, cancel: &AtomicBool) -> MutationResult {
    crate::require_aw_database(paths).map_err(|error| MutationFailure {
        error,
        report: MutationReport::default(),
    })?;
    crate::store::plan::execute(paths, &plan.0, cancel)
}

fn admit(
    paths: &DatabasePaths,
    prefix: &Path,
    plan: crate::store::plan::PackagePlan,
) -> Result<crate::store::plan::PackagePlan> {
    crate::database::reject_legacy_prefix(prefix)?;
    Inventory::read(paths, None)?;
    Ok(plan)
}
