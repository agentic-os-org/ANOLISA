//! Selected removal preserves unknown files and every remaining exact dependency.

use super::{
    capacity::{self, Limits},
    snapshot::{self, Snapshot},
    Action, PackagePlan,
};
use crate::{
    database::{self, DatabasePaths, RegistrationPointer},
    description, require,
    transaction::ChangeSet,
    Result,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(super) fn plan(paths: &DatabasePaths, prefix: &Path, ids: &[String]) -> Result<PackagePlan> {
    plan_with_limits(paths, prefix, ids, Limits::default())
}

pub(super) fn plan_with_limits(
    paths: &DatabasePaths,
    prefix: &Path,
    ids: &[String],
    limits: Limits,
) -> Result<PackagePlan> {
    let mut names = BTreeSet::new();
    for id in ids {
        description::identifier(id)?;
        require(
            names.insert(id.as_str()),
            "duplicate package selected for removal",
        )?;
    }
    let snapshot = Snapshot::read(paths, prefix)?;
    let mut plan = snapshot.plan(paths, prefix, Action::Uninstall);
    let mut installation = snapshot.installation(paths, prefix);
    if installation.packages.is_empty() {
        plan.problem(
            "not_installed",
            "no package installation exists at the selected prefix",
            None,
            Some(prefix.into()),
        );
        return Ok(plan);
    }
    for name in &names {
        if !installation
            .packages
            .iter()
            .any(|package| &package.package_id == name)
        {
            plan.problem(
                "not_installed",
                format!("package {name} is not installed at this prefix"),
                Some(name),
                Some(prefix.into()),
            );
        }
    }
    let (verify, _) = snapshot::verify(&mut plan, &installation);
    let removed = installation
        .packages
        .iter()
        .filter(|package| names.is_empty() || names.contains(package.package_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let removing = removed
        .iter()
        .map(|package| {
            (
                installation.installation_id.clone(),
                package.package_id.clone(),
                package.version.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut all = snapshot.database.installations.clone();
    all.retain(|other| other.prefix != prefix);
    all.push(installation.clone());
    for other in &all {
        for package in &other.packages {
            if removing.contains(&(
                other.installation_id.clone(),
                package.package_id.clone(),
                package.version.clone(),
            )) {
                continue;
            }
            for dependency in &package.dependencies {
                if removing.contains(&(
                    dependency.installation_id.clone(),
                    dependency.package_id.clone(),
                    dependency.version.clone(),
                )) {
                    plan.problem(
                        "required_by",
                        format!(
                            "{} in {} still requires {}@{}",
                            package.package_id,
                            other.prefix.display(),
                            dependency.package_id,
                            dependency.version
                        ),
                        Some(&dependency.package_id),
                        Some(other.prefix.clone()),
                    );
                }
            }
        }
    }
    super::runtime::check(&mut plan, &installation);
    let mut removes = BTreeMap::new();
    for package in &removed {
        plan.removed.push(snapshot::summary(package));
        snapshot::files(&mut plan, Action::Uninstall, package);
        removes.extend(package.files.clone());
    }
    installation.packages.retain(|package| {
        !removed
            .iter()
            .any(|removed| removed.package_id == package.package_id)
    });
    let installation_id = installation.installation_id.clone();
    // Keep the locator after the last removal: historical installers must never regain
    // write authority, and a later install can reuse the same registered prefix.
    let marker_after = Some(serde_json::to_vec_pretty(&RegistrationPointer::new(
        paths,
        installation_id.clone(),
    )?)?);
    let after = snapshot.after(Some(installation), prefix)?;
    if plan.ready() {
        database::validate_database(&after, paths.scope())?;
        let changes = ChangeSet {
            prefix: prefix.into(),
            database_before: snapshot.before.clone(),
            database_after: after,
            installation_id,
            verify,
            writes: BTreeMap::new(),
            removes,
            legacy: snapshot.legacy.clone(),
            marker_before: snapshot.marker.clone(),
            marker_after,
            source_records: BTreeMap::new(),
        };
        capacity::check(paths, &mut plan, &changes, limits, false)?;
        if plan.ready() {
            plan.changes = Some(changes);
        }
    }
    Ok(plan)
}
