//! Race-resistant static topology preparation in a target process mount namespace.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use agentsight_enforcement_protocol::{FileProtectionExclusionKind, FileProtectionTargetKind};

use super::file_protection_mountinfo::{
    MountInfo, read_mountinfo, underlying_mount_path, visible_paths_for_mount_root,
};
use super::file_protection_policy::NormalizedFileProtectionPolicy;
use super::file_protection_resolver::{
    HeldPath, open_held as resolve_held, require_directory, require_type,
};
use super::read_process_start_time;
use crate::BackendError;

const MAX_SNAPSHOT_ATTEMPTS: usize = 2;

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
}

#[derive(Clone, Debug)]
struct Snapshot {
    namespace: (u64, u64),
    start_time: u64,
    mountinfo_digest: u64,
    mounts: Vec<MountInfo>,
}

trait TopologySource {
    fn open_root(&self) -> io::Result<File>;
    fn snapshot(&self) -> Result<Snapshot, BackendError>;
}

struct ProcSource {
    pid: i32,
}

impl TopologySource for ProcSource {
    fn open_root(&self) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY)
            .open(format!("/proc/{}/root", self.pid))
    }

    fn snapshot(&self) -> Result<Snapshot, BackendError> {
        let namespace = fs::metadata(format!("/proc/{}/ns/mnt", self.pid))
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .map_err(|error| kernel(format!("read target mount namespace: {error}")))?;
        let start_time = read_process_start_time(self.pid)?;
        let (mounts, mountinfo_digest) = read_mountinfo(self.pid)?;
        Ok(Snapshot {
            namespace,
            start_time,
            mountinfo_digest,
            mounts,
        })
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
        &ProcSource { pid: host_pid },
    )
}

fn prepare_from(
    policy: &NormalizedFileProtectionPolicy,
    host_pid: i32,
    process_start_time: u64,
    source: &impl TopologySource,
) -> Result<PreparedFileProtectionTopology, BackendError> {
    let mut changed = None;
    for _ in 0..MAX_SNAPSHOT_ATTEMPTS {
        let before = source.snapshot()?;
        validate_identity(host_pid, process_start_time, &before)?;
        let root = source
            .open_root()
            .map_err(|error| kernel(format!("open /proc/{host_pid}/root with O_PATH: {error}")))?;
        let prepared = prepare_once(policy, &root, &before.mounts)?;
        let after = source.snapshot()?;
        validate_identity(host_pid, process_start_time, &after)?;
        if before.namespace == after.namespace
            && before.mountinfo_digest == after.mountinfo_digest
            && before.mounts == after.mounts
        {
            return Ok(prepared);
        }
        changed = Some(format!(
            "target mount topology changed while preparing policy (namespace {:?}->{:?}, digest {}->{})",
            before.namespace, after.namespace, before.mountinfo_digest, after.mountinfo_digest
        ));
    }
    Err(kernel(changed.unwrap_or_else(|| {
        "target mount topology was unstable".into()
    })))
}

fn prepare_once(
    policy: &NormalizedFileProtectionPolicy,
    root: &File,
    mounts: &[MountInfo],
) -> Result<PreparedFileProtectionTopology, BackendError> {
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
            let held_ancestor = open_held(root, ancestor)?;
            if ancestor_seen.insert((target_id, held_ancestor.identity)) {
                ancestor_slots.push(TopologyPathRecord {
                    target_id,
                    path: held_ancestor,
                });
            }
        }

        if target.kind == FileProtectionTargetKind::DirectoryTree {
            for mount in mounts.iter().filter(|mount| {
                mount.mountpoint != target.path
                    && is_component_descendant(&target.path, &mount.mountpoint)
            }) {
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
            // Bind aliases expose the target's filesystem-relative path as
            // their mount root. Identity comparison below prevents inference
            // from shared filesystem metadata alone.
            let underlying_root = underlying_mount_path(mounts, &target.path)?;
            for mount in mounts.iter().filter(|mount| {
                !path_in_scope(&target.path, &mount.mountpoint) && mount.root == underlying_root
            }) {
                let alias = open_held(root, &mount.mountpoint)?;
                if alias.identity.ino == held.identity.ino
                    && alias.identity.dev == held.identity.dev
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
    Ok(PreparedFileProtectionTopology {
        direct_targets,
        direct_slots,
        exclusions,
        ancestor_slots,
        existing_submounts,
        underlying_submounts,
        known_bind_aliases,
        capacities,
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
    let parent = open_held(root, parent_text)?;
    require_directory(&parent)?;
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

fn validate_identity(
    pid: i32,
    expected_start: u64,
    snapshot: &Snapshot,
) -> Result<(), BackendError> {
    if snapshot.start_time == expected_start {
        Ok(())
    } else {
        Err(BackendError::StaleProcess { pid })
    }
}

fn underlying_submount_paths(
    root: &File,
    mounts: &[MountInfo],
    mount: &MountInfo,
    mounted: &HeldPath,
) -> Vec<HeldPath> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for candidate in visible_paths_for_mount_root(mounts, mount) {
        let Ok(path) = resolve_held(root, &candidate) else {
            continue;
        };
        if path.identity.mode & libc::S_IFMT as u16 == libc::S_IFDIR as u16
            && path.identity.ino == mounted.identity.ino
            && path.identity.dev == mounted.identity.dev
            && seen.insert(path.identity)
        {
            paths.push(path);
        }
    }
    paths
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

fn path_in_scope(root: &str, candidate: &str) -> bool {
    root == candidate || is_component_descendant(root, candidate)
}

fn is_component_descendant(root: &str, candidate: &str) -> bool {
    root == "/" && candidate != "/"
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn lexical_ancestors(path: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut current = path;
    loop {
        result.push(current);
        let Some(index) = current.rfind('/') else {
            break;
        };
        if index == 0 {
            break;
        }
        current = &current[..index];
    }
    result
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
