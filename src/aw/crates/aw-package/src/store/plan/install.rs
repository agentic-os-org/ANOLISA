//! Additive same-prefix dependency selection and global owned-file collision checks.

use super::{
    capacity::{self, Limits},
    snapshot::{self, Snapshot},
    Action, DependencySummary, PackagePlan,
};
use crate::{
    database::{self, DatabasePaths, RegistrationPointer},
    open_bundle,
    transaction::{ChangeSet, SourceFile},
    Result,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[cfg(test)]
mod tests;

pub(super) fn plan(paths: &DatabasePaths, bundle: &Path, prefix: &Path) -> Result<PackagePlan> {
    plan_with_limit(paths, bundle, prefix, open_bundle::MAX_PAYLOAD_BYTES)
}

fn plan_with_limit(
    paths: &DatabasePaths,
    bundle: &Path,
    prefix: &Path,
    prefix_byte_limit: u64,
) -> Result<PackagePlan> {
    plan_with_limits(paths, bundle, prefix, prefix_byte_limit, Limits::default())
}

fn plan_with_limits(
    paths: &DatabasePaths,
    bundle: &Path,
    prefix: &Path,
    prefix_byte_limit: u64,
    limits: Limits,
) -> Result<PackagePlan> {
    let bundle = open_bundle::inspect(bundle)?;
    let snapshot = Snapshot::read(paths, prefix)?;
    let mut plan = snapshot.plan(paths, prefix, Action::Install);
    let mut installation = snapshot.installation(paths, prefix);
    let (verify, installed_bytes) = snapshot::verify(&mut plan, &installation);
    if !snapshot.legacy.is_empty() {
        super::runtime::check(&mut plan, &installation);
    }
    if installed_bytes
        .checked_add(bundle.payload_bytes)
        .is_none_or(|bytes| bytes > prefix_byte_limit)
    {
        plan.problem(
            "verification_limit",
            format!("combined installation payload exceeds its {prefix_byte_limit}-byte verification limit"),
            None,
            Some(prefix.into()),
        );
    }
    let mut available = installation
        .packages
        .iter()
        .map(|package| (package.package_id.clone(), package.clone()))
        .collect::<BTreeMap<_, _>>();
    for package in &bundle.packages {
        if available.contains_key(&package.package_id) {
            plan.problem("already_installed",format!("package {} is already installed; package replacement requires an upgrade operation",package.package_id),Some(&package.package_id),Some(prefix.into()));
        } else {
            available.insert(package.package_id.clone(), package.clone());
        }
    }
    let installation_id = installation.installation_id.clone();
    let mut writes = BTreeMap::new();
    let mut incoming = bundle.packages;
    for package in &mut incoming {
        for dependency in &mut package.dependencies {
            let selected = available.get(&dependency.package_id);
            let same_build = selected.is_some_and(|required| {
                !bundle.legacy_preview
                    || (required.source.source_commit == package.source.source_commit
                        && required.arch == package.arch
                        && required.os == package.os)
            });
            if selected.is_none_or(|required| required.version != dependency.version) || !same_build
            {
                plan.problem(
                    "dependency",
                    format!(
                        "{} requires {}@{} in this prefix{}",
                        package.package_id,
                        dependency.package_id,
                        dependency.version,
                        if bundle.legacy_preview {
                            " from the same Preview build"
                        } else {
                            ""
                        }
                    ),
                    Some(&package.package_id),
                    Some(prefix.into()),
                );
            }
            dependency.installation_id = installation_id.clone();
            plan.dependencies.push(DependencySummary {
                package_id: package.package_id.clone(),
                required_package_id: dependency.package_id.clone(),
                version: dependency.version.clone(),
                installation_id: plan.installation_id.clone(),
            });
        }
        for (name, metadata) in &package.files {
            let target = prefix.join(name);
            ownership(
                &mut plan,
                &snapshot,
                &installation,
                &target,
                &package.package_id,
            );
            destination(&mut plan, &target, prefix, &package.package_id);
            if let Some(source) = bundle.sources.get(name) {
                writes.insert(
                    name.clone(),
                    SourceFile {
                        source: source.clone(),
                        metadata: metadata.clone(),
                    },
                );
            }
        }
        plan.added.push(snapshot::summary(package));
        snapshot::files(&mut plan, Action::Install, package);
    }
    installation.packages.extend(incoming);
    let after = snapshot.after(Some(installation), prefix)?;
    if plan.ready() {
        database::validate_database(&after, paths.scope())?;
    }
    let marker =
        serde_json::to_vec_pretty(&RegistrationPointer::new(paths, installation_id.clone())?)?;
    if plan.ready() {
        let changes = ChangeSet {
            prefix: prefix.into(),
            database_before: snapshot.before.clone(),
            database_after: after,
            installation_id,
            verify,
            writes,
            removes: BTreeMap::new(),
            legacy: snapshot.legacy.clone(),
            marker_before: snapshot.marker.clone(),
            marker_after: Some(marker),
            source_records: bundle.records,
        };
        capacity::check(paths, &mut plan, &changes, limits, true)?;
        if plan.ready() {
            plan.changes = Some(changes);
        }
    }
    Ok(plan)
}

fn ownership(
    plan: &mut PackagePlan,
    snapshot: &Snapshot,
    selected: &database::Installation,
    target: &Path,
    package: &str,
) {
    let installations = snapshot.database.installations.iter().chain(
        snapshot
            .selected
            .iter()
            .filter(|_| !snapshot.legacy.is_empty()),
    );
    for installation in installations {
        for installed in &installation.packages {
            for path in installed.files.keys() {
                let owned = installation.prefix.join(path);
                if owned == target || owned.starts_with(target) || target.starts_with(&owned) {
                    plan.problem(
                        "file_owner",
                        format!(
                            "{} is already owned by {} in {}",
                            target.display(),
                            installed.package_id,
                            installation.prefix.display()
                        ),
                        Some(package),
                        Some(target.into()),
                    );
                }
            }
        }
    }
    // The selected installation may be fresh; its reserved metadata is never package payload.
    if target.starts_with(selected.prefix.join(".aw-packages")) {
        plan.problem(
            "reserved_path",
            "payload overlaps AW package records",
            Some(package),
            Some(target.into()),
        );
    }
}

fn destination(plan: &mut PackagePlan, target: &Path, prefix: &Path, package: &str) {
    for path in target
        .ancestors()
        .take_while(|path| path.starts_with(prefix))
    {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if path == target || !metadata.is_dir() || metadata.file_type().is_symlink() =>
            {
                plan.problem(
                    "file_exists",
                    format!(
                        "refusing to overwrite an existing file or traverse a non-directory: {}",
                        path.display()
                    ),
                    Some(package),
                    Some(path.to_path_buf()),
                );
                break;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                plan.problem(
                    "file_access",
                    error.to_string(),
                    Some(package),
                    Some(PathBuf::from(path)),
                );
                break;
            }
        }
    }
}
