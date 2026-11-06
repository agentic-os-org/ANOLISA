//! Race-resistant static topology preparation in a target process mount namespace.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use agentsight_enforcement_protocol::{FileProtectionExclusionKind, FileProtectionTargetKind};

use super::file_protection_discovery::{
    discover_submounts, lexical_ancestors, path_in_scope, underlying_submount_paths,
};
use super::file_protection_manifest::{
    IdentityManifest, ProcTopologySource, TopologySnapshot, TopologySource, TopologyValidation,
    open_fresh_root,
};
use super::file_protection_mountinfo::{MountInfo, underlying_mount_path};
use super::file_protection_policy::NormalizedFileProtectionPolicy;
use super::file_protection_resolver::{
    HeldPath, open_held as resolve_held, open_held_directory as resolve_held_directory,
    require_directory, require_type,
};
use crate::BackendError;

#[derive(Clone, Debug)]
pub(super) struct PreparedEntrySlot {
    pub(super) target_id: u32,
    pub(super) parent: HeldPath,
    pub(super) name: String,
}

#[derive(Clone, Debug)]
pub(super) struct PreparedExclusion {
    pub(super) slot: PreparedEntrySlot,
    pub(super) kind: FileProtectionExclusionKind,
    pub(super) existing: Option<HeldPath>,
}

#[derive(Clone, Debug)]
pub(super) struct TopologyPathRecord {
    pub(super) target_id: u32,
    pub(super) path: HeldPath,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct TopologyCapacityCounts {
    pub(super) roots: usize,
    pub(super) directories: usize,
    pub(super) stable_paths: usize,
    pub(super) slots: usize,
    pub(super) targets: usize,
    pub(super) domains: usize,
}

#[derive(Clone, Debug)]
pub(super) struct PreparedFileProtectionTopology {
    pub(super) direct_targets: Vec<TopologyPathRecord>,
    pub(super) direct_slots: Vec<PreparedEntrySlot>,
    pub(super) exclusions: Vec<PreparedExclusion>,
    pub(super) ancestor_slots: Vec<TopologyPathRecord>,
    pub(super) existing_submounts: Vec<TopologyPathRecord>,
    pub(super) underlying_submounts: Vec<TopologyPathRecord>,
    pub(super) known_bind_aliases: Vec<TopologyPathRecord>,
    pub(super) capacities: TopologyCapacityCounts,
    validation: TopologyValidation,
}

impl PreparedFileProtectionTopology {
    pub(super) fn validate(&self) -> Result<(), BackendError> {
        self.validation.validate()
    }
}

pub(super) fn prepare(
    policy: &NormalizedFileProtectionPolicy,
    host_pid: i32,
    process_start_time: u64,
) -> Result<PreparedFileProtectionTopology, BackendError> {
    prepare_from(
        policy,
        host_pid,
        process_start_time,
        Arc::new(ProcTopologySource::new(host_pid)),
    )
}

fn prepare_from(
    policy: &NormalizedFileProtectionPolicy,
    host_pid: i32,
    process_start_time: u64,
    source: Arc<dyn TopologySource>,
) -> Result<PreparedFileProtectionTopology, BackendError> {
    let mut changed = None;
    for _ in 0..2 {
        let before = source.snapshot()?;
        before.validate_process(host_pid, process_start_time)?;
        let root = open_fresh_root(source.as_ref(), host_pid)?;
        let prepared = prepare_once(
            policy,
            &root,
            &before,
            host_pid,
            process_start_time,
            Arc::clone(&source),
        )?;
        let after = source.snapshot()?;
        after.validate_process(host_pid, process_start_time)?;
        if !before.same_topology(&after) {
            changed = Some(before.change_message(&after));
            continue;
        }
        prepared.validate()?;
        return Ok(prepared);
    }
    Err(kernel(changed.unwrap_or_else(|| {
        "target mount topology was unstable".into()
    })))
}

fn prepare_once(
    policy: &NormalizedFileProtectionPolicy,
    root: &File,
    snapshot: &TopologySnapshot,
    host_pid: i32,
    process_start_time: u64,
    source: Arc<dyn TopologySource>,
) -> Result<PreparedFileProtectionTopology, BackendError> {
    let mounts = &snapshot.mounts;
    let mut direct_targets = Vec::new();
    let mut direct_slots = Vec::new();
    let mut exclusions = Vec::new();
    let mut ancestor_slots = Vec::new();
    let mut existing_submounts = Vec::new();
    let mut underlying_submounts = Vec::new();
    let mut known_bind_aliases = Vec::new();
    let mut ancestor_seen = HashSet::new();

    for (index, target) in policy.targets.iter().enumerate() {
        let target_id = u32::try_from(index + 1)
            .map_err(|_| invalid("file protection target count exceeds u32"))?;
        let held = open_held(root, &target.path)?;
        require_type(&held, target.kind)?;
        reject_relevant_overlay(
            mounts,
            &target.path,
            target.kind == FileProtectionTargetKind::DirectoryTree,
            held.identity.mnt_id,
        )?;
        let record = TopologyPathRecord {
            target_id,
            path: held.clone(),
        };
        direct_targets.push(record);
        if target.path != "/" {
            direct_slots.push(prepare_entry_slot(root, target_id, &target.path)?);
        }

        let stable_path = match target.kind {
            FileProtectionTargetKind::ExactFile => Path::new(&target.path)
                .parent()
                .and_then(Path::to_str)
                .ok_or_else(|| invalid("exact-file target has no UTF-8 parent"))?,
            FileProtectionTargetKind::DirectoryTree => &target.path,
        };
        for ancestor in lexical_ancestors(stable_path) {
            let held_ancestor = open_directory(root, ancestor)?;
            if ancestor_seen.insert((target_id, held_ancestor.identity)) {
                ancestor_slots.push(TopologyPathRecord {
                    target_id,
                    path: held_ancestor,
                });
            }
        }

        if target.kind == FileProtectionTargetKind::DirectoryTree {
            let discovery = discover_submounts(root, mounts, &target.path, held.identity)?;
            let target_spellings = discovery.target_spellings;
            let mut alias_seen = HashSet::new();
            for alias in discovery.aliases {
                alias_seen.insert(alias.authored_path.clone());
                known_bind_aliases.push(TopologyPathRecord {
                    target_id,
                    path: alias,
                });
            }
            for spelling in target_spellings.iter().filter(|path| *path != &target.path) {
                if alias_seen.insert(spelling.clone()) {
                    known_bind_aliases.push(TopologyPathRecord {
                        target_id,
                        path: open_directory(root, spelling)?,
                    });
                }
            }
            for index in discovery.indices {
                let mount = &mounts[index];
                if matches!(mount.fs_type.as_str(), "overlay" | "overlayfs") {
                    return Err(invalid(format!(
                        "overlay filesystem at {:?} intersects protected scope {:?}",
                        mount.mountpoint, target.path
                    )));
                }
                let path = open_held(root, &mount.mountpoint)?;
                require_directory(&path)?;
                for underlying in underlying_submount_paths(root, mounts, mount, &path) {
                    underlying_submounts.push(TopologyPathRecord {
                        target_id,
                        path: underlying,
                    });
                }
                existing_submounts.push(TopologyPathRecord { target_id, path });
            }
            // Bind aliases expose a canonical filesystem-relative target path
            // as their mount root. Identity comparison prevents inference from
            // shared filesystem metadata alone.
            let underlying_roots: HashSet<_> = target_spellings
                .iter()
                .map(|path| underlying_mount_path(mounts, path))
                .collect::<Result<_, _>>()?;
            for mount in mounts.iter().filter(|mount| {
                target_spellings
                    .iter()
                    .all(|path| !path_in_scope(path, &mount.mountpoint))
                    && underlying_roots.contains(&mount.root)
            }) {
                let Ok(alias) = resolve_held(root, &mount.mountpoint) else {
                    continue;
                };
                if alias.identity.ino == held.identity.ino
                    && alias.identity.dev == held.identity.dev
                    && alias_seen.insert(alias.authored_path.clone())
                {
                    known_bind_aliases.push(TopologyPathRecord {
                        target_id,
                        path: alias,
                    });
                }
            }
        }

        for exclusion in &target.exclusions {
            let prepared = prepare_exclusion(root, target_id, exclusion)?;
            let mount_id = prepared
                .existing
                .as_ref()
                .map_or(prepared.slot.parent.identity.mnt_id, |path| {
                    path.identity.mnt_id
                });
            reject_relevant_overlay(
                mounts,
                &exclusion.path,
                exclusion.kind == FileProtectionExclusionKind::DirectorySubtree,
                mount_id,
            )?;
            exclusions.push(prepared);
        }
    }

    let capacities = capacity_counts(
        &direct_targets,
        &direct_slots,
        &exclusions,
        &ancestor_slots,
        &existing_submounts,
        &underlying_submounts,
        &known_bind_aliases,
    )?;
    let mut manifest = identity_manifest(
        &direct_targets,
        &direct_slots,
        &exclusions,
        &ancestor_slots,
        &existing_submounts,
        &underlying_submounts,
        &known_bind_aliases,
    )?;
    manifest.add_existing(&open_held(root, "/")?)?;
    Ok(PreparedFileProtectionTopology {
        direct_targets,
        direct_slots,
        exclusions,
        ancestor_slots,
        existing_submounts,
        underlying_submounts,
        known_bind_aliases,
        capacities,
        validation: TopologyValidation::new(
            host_pid,
            process_start_time,
            snapshot.clone(),
            manifest,
            source,
        ),
    })
}

fn prepare_entry_slot(
    root: &File,
    target_id: u32,
    authored_path: &str,
) -> Result<PreparedEntrySlot, BackendError> {
    let path = Path::new(authored_path);
    let parent_path = path
        .parent()
        .ok_or_else(|| invalid("file protection slot has no parent"))?;
    let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        invalid(format!(
            "file protection path {authored_path:?} has no UTF-8 final component"
        ))
    })?;
    if name.len() > 255 {
        return Err(invalid(format!(
            "file protection basename in {authored_path:?} exceeds NAME_MAX"
        )));
    }
    let parent_text = parent_path
        .to_str()
        .ok_or_else(|| invalid("file protection parent is not UTF-8"))?;
    let parent = open_directory(root, parent_text)?;
    Ok(PreparedEntrySlot {
        target_id,
        parent,
        name: name.into(),
    })
}

fn prepare_exclusion(
    root: &File,
    target_id: u32,
    exclusion: &super::file_protection_policy::NormalizedFileProtectionExclusion,
) -> Result<PreparedExclusion, BackendError> {
    let slot = prepare_entry_slot(root, target_id, &exclusion.path)?;
    let existing = match resolve_held(root, &exclusion.path) {
        Ok(path) => Some(path),
        Err(error) if error.is_missing_final() => None,
        Err(error) => return Err(invalid(error.to_string())),
    };
    if exclusion.kind == FileProtectionExclusionKind::DirectorySubtree
        && let Some(existing) = existing.as_ref()
    {
        require_directory(existing)?;
    }
    Ok(PreparedExclusion {
        slot,
        kind: exclusion.kind,
        existing,
    })
}

fn open_held(root: &File, authored_path: &str) -> Result<HeldPath, BackendError> {
    resolve_held(root, authored_path).map_err(|error| invalid(error.to_string()))
}

fn open_directory(root: &File, authored_path: &str) -> Result<HeldPath, BackendError> {
    resolve_held_directory(root, authored_path).map_err(|error| invalid(error.to_string()))
}

fn identity_manifest(
    direct_targets: &[TopologyPathRecord],
    direct_slots: &[PreparedEntrySlot],
    exclusions: &[PreparedExclusion],
    ancestors: &[TopologyPathRecord],
    submounts: &[TopologyPathRecord],
    underlying_submounts: &[TopologyPathRecord],
    aliases: &[TopologyPathRecord],
) -> Result<IdentityManifest, BackendError> {
    let mut manifest = IdentityManifest::default();
    for record in direct_targets
        .iter()
        .chain(submounts)
        .chain(underlying_submounts)
    {
        manifest.add_existing(&record.path)?;
    }
    for record in ancestors.iter().chain(aliases) {
        manifest.add_directory(&record.path)?;
    }
    for slot in direct_slots {
        manifest.add_directory(&slot.parent)?;
    }
    for exclusion in exclusions {
        manifest.add_directory(&exclusion.slot.parent)?;
        if let Some(existing) = exclusion.existing.as_ref() {
            manifest.add_existing(existing)?;
        } else {
            let path = if exclusion.slot.parent.authored_path == "/" {
                format!("/{}", exclusion.slot.name)
            } else {
                format!(
                    "{}/{}",
                    exclusion.slot.parent.authored_path, exclusion.slot.name
                )
            };
            manifest.add_missing_final(&path, &exclusion.slot.parent)?;
        }
    }
    Ok(manifest)
}

fn reject_relevant_overlay(
    mounts: &[MountInfo],
    path: &str,
    include_descendants: bool,
    resolved_mount_id: u32,
) -> Result<(), BackendError> {
    for mount in mounts {
        let below = include_descendants && path_in_scope(path, &mount.mountpoint);
        if (mount.id == resolved_mount_id || below)
            && matches!(mount.fs_type.as_str(), "overlay" | "overlayfs")
        {
            return Err(invalid(format!(
                "overlay filesystem at {:?} intersects protected scope {path:?}",
                mount.mountpoint
            )));
        }
    }
    Ok(())
}

fn capacity_counts(
    direct: &[TopologyPathRecord],
    direct_slots: &[PreparedEntrySlot],
    exclusions: &[PreparedExclusion],
    ancestors: &[TopologyPathRecord],
    submounts: &[TopologyPathRecord],
    underlying_submounts: &[TopologyPathRecord],
    aliases: &[TopologyPathRecord],
) -> Result<TopologyCapacityCounts, BackendError> {
    let directory_roots = direct
        .iter()
        .filter(|record| record.path.identity.mode & libc::S_IFMT as u16 == libc::S_IFDIR as u16);
    let roots: HashSet<_> = directory_roots
        .clone()
        .chain(submounts)
        .chain(underlying_submounts)
        .chain(aliases)
        .map(|record| record.path.identity)
        .collect();
    let directories: HashSet<_> = directory_roots
        .chain(submounts)
        .chain(underlying_submounts)
        .chain(aliases)
        .map(|record| (record.path.identity.ino, record.path.identity.dev))
        .collect();
    let stable_paths: HashSet<_> = ancestors
        .iter()
        .map(|record| record.path.identity)
        .collect();
    let slots: HashSet<_> = direct_slots
        .iter()
        .map(|slot| (slot.parent.identity, slot.name.as_str()))
        .chain(
            exclusions
                .iter()
                .map(|entry| (entry.slot.parent.identity, entry.slot.name.as_str())),
        )
        .collect();
    checked_capacity_counts(
        roots.len(),
        directories.len(),
        stable_paths.len(),
        slots.len(),
        direct.len(),
    )
}

fn checked_capacity_counts(
    roots: usize,
    directories: usize,
    stable_paths: usize,
    slots: usize,
    targets: usize,
) -> Result<TopologyCapacityCounts, BackendError> {
    let _ = roots
        .checked_add(directories)
        .and_then(|count| count.checked_add(stable_paths))
        .and_then(|count| count.checked_add(slots))
        .and_then(|count| count.checked_add(targets))
        .ok_or_else(|| invalid("file protection capacity count overflow"))?;
    Ok(TopologyCapacityCounts {
        roots,
        directories,
        stable_paths,
        slots,
        targets,
        domains: 1,
    })
}

fn invalid(message: impl Into<String>) -> BackendError {
    BackendError::CompileFailure(message.into())
}

fn kernel(message: impl Into<String>) -> BackendError {
    BackendError::KernelFailure(message.into())
}

#[cfg(test)]
#[path = "file_protection_topology_tests.rs"]
mod tests;
