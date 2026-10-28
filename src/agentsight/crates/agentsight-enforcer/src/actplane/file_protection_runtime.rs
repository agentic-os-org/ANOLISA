//! Structured policy lowering into generation-scoped map entries.

use std::collections::BTreeMap;
use std::io;

use agentsight_enforcement_protocol::{FileProtectionExclusionKind, FileProtectionTargetKind};
use ebpf_ifc_engine::{
    FileProtectionDomainValue, FileProtectionGeneration, FileProtectionMapCounts,
    FileProtectionTargetValue, INODE_GUARD_RENAME, INODE_GUARD_UNLINK, TreeGuard, TreePathIdentity,
};
use uuid::Uuid;

use super::file_protection_policy::NormalizedFileProtectionPolicy;
use super::file_protection_resolver::FileIdentity;
use super::file_protection_topology::{
    PreparedEntrySlot, PreparedFileProtectionTopology, TopologyPathRecord,
};
use crate::BackendError;

const TARGET_DELETE: u32 = 1;
const TARGET_TREE: u32 = 2;
const TARGET_ORDINARY_WRITE: u32 = 4;
const DELETE_GUARD_FLAGS: u32 = INODE_GUARD_UNLINK | INODE_GUARD_RENAME;

/// Validated policy and held static topology, before any runtime mutation.
#[derive(Clone, Debug)]
pub(super) struct PreparedFileProtection {
    pub(super) policy: NormalizedFileProtectionPolicy,
    pub(super) topology: PreparedFileProtectionTopology,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PathKey {
    ino: u64,
    dev: u32,
    mnt_id: u32,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SlotKey {
    parent_ino: u64,
    parent_dev: u32,
    name: String,
}

#[derive(Clone, Copy, Debug, Default)]
struct SlotMasks {
    direct: u64,
    exact_exclude: u64,
    subtree_exclude: u64,
}

struct LoweredPolicy {
    roots: BTreeMap<PathKey, u64>,
    directories: BTreeMap<PathKey, u64>,
    stable_paths: BTreeMap<PathKey, u64>,
    slots: BTreeMap<SlotKey, (TreePathIdentity, SlotMasks)>,
    targets: Vec<FileProtectionTargetValue>,
    activation: FileProtectionDomainValue,
}

impl LoweredPolicy {
    fn counts(&self) -> FileProtectionMapCounts {
        FileProtectionMapCounts {
            roots: self.roots.len(),
            directories: self.directories.len(),
            stable_paths: self.stable_paths.len(),
            slots: self.slots.len(),
            targets: self.targets.len(),
            domains: 1,
        }
    }
}

impl PathKey {
    fn from_identity(identity: FileIdentity) -> Self {
        Self {
            ino: identity.ino,
            dev: identity.dev,
            mnt_id: identity.mnt_id,
        }
    }

    fn underlying(identity: FileIdentity) -> Self {
        Self {
            mnt_id: 0,
            ..Self::from_identity(identity)
        }
    }
}

fn lower(
    prepared: &PreparedFileProtection,
    binding_id: Uuid,
) -> Result<LoweredPolicy, BackendError> {
    let mut roots = BTreeMap::new();
    let mut directories = BTreeMap::new();
    let mut stable_paths = BTreeMap::new();
    let mut slots = BTreeMap::new();
    let mut targets = Vec::with_capacity(prepared.policy.targets.len());
    let mut tree_mask = 0;
    let mut delete_mask = 0;
    let mut ordinary_write_mask = 0;

    for (index, target) in prepared.policy.targets.iter().enumerate() {
        let target_id =
            u32::try_from(index + 1).map_err(|_| compile("structured target id exceeds u32"))?;
        let bit = target_bit(target_id)?;
        if target.delete {
            delete_mask |= bit;
        }
        if target.ordinary_write {
            ordinary_write_mask |= bit;
        }
        if target.kind == FileProtectionTargetKind::DirectoryTree {
            tree_mask |= bit;
        }
        targets.push(FileProtectionTargetValue {
            flags: (u32::from(target.delete) * TARGET_DELETE)
                | (u32::from(target.kind == FileProtectionTargetKind::DirectoryTree) * TARGET_TREE)
                | (u32::from(target.ordinary_write) * TARGET_ORDINARY_WRITE),
            target_id,
            guard: TreeGuard {
                flags: DELETE_GUARD_FLAGS,
                rule_id: target_id,
            },
        });
    }

    for record in &prepared.topology.direct_targets {
        if target_kind(&prepared.policy, record.target_id)?
            == FileProtectionTargetKind::DirectoryTree
        {
            add_root(&mut roots, &mut directories, record)?;
        }
    }
    for record in prepared
        .topology
        .existing_submounts
        .iter()
        .chain(&prepared.topology.underlying_submounts)
        .chain(&prepared.topology.known_bind_aliases)
    {
        add_root(&mut roots, &mut directories, record)?;
    }
    for record in &prepared.topology.ancestor_slots {
        let bit = target_bit(record.target_id)?;
        *stable_paths
            .entry(PathKey::underlying(record.path.identity))
            .or_insert(0) |= bit;
    }
    for slot in &prepared.topology.direct_slots {
        slot_masks(&mut slots, slot)?.direct |= target_bit(slot.target_id)?;
    }
    for exclusion in &prepared.topology.exclusions {
        let bit = target_bit(exclusion.slot.target_id)?;
        let masks = slot_masks(&mut slots, &exclusion.slot)?;
        match exclusion.kind {
            FileProtectionExclusionKind::ExactEntry => masks.exact_exclude |= bit,
            FileProtectionExclusionKind::DirectorySubtree => masks.subtree_exclude |= bit,
        }
    }

    let fallback_target = prepared
        .policy
        .targets
        .iter()
        .position(|target| target.delete || target.ordinary_write)
        .map(|index| index as u32 + 1)
        .ok_or_else(|| compile("structured policy has no protected target"))?;
    Ok(LoweredPolicy {
        roots,
        directories,
        stable_paths,
        slots,
        targets,
        activation: FileProtectionDomainValue {
            target_count: prepared.policy.targets.len() as u32,
            tree_mask,
            delete_mask,
            ordinary_write_mask,
            protection_set_id: stable_hash64(binding_id.as_bytes()),
            guard: TreeGuard {
                flags: DELETE_GUARD_FLAGS,
                rule_id: fallback_target,
            },
        },
    })
}

fn add_root(
    roots: &mut BTreeMap<PathKey, u64>,
    directories: &mut BTreeMap<PathKey, u64>,
    record: &TopologyPathRecord,
) -> Result<(), BackendError> {
    let bit = target_bit(record.target_id)?;
    *roots
        .entry(PathKey::from_identity(record.path.identity))
        .or_insert(0) |= bit;
    *directories
        .entry(PathKey::underlying(record.path.identity))
        .or_insert(0) |= bit;
    Ok(())
}

fn slot_masks<'a>(
    slots: &'a mut BTreeMap<SlotKey, (TreePathIdentity, SlotMasks)>,
    slot: &PreparedEntrySlot,
) -> Result<&'a mut SlotMasks, BackendError> {
    let parent = TreePathIdentity {
        ino: slot.parent.identity.ino,
        dev: slot.parent.identity.dev,
        mnt_id: 0,
    };
    Ok(&mut slots
        .entry(SlotKey {
            parent_ino: parent.ino,
            parent_dev: parent.dev,
            name: slot.name.clone(),
        })
        .or_insert((parent, SlotMasks::default()))
        .1)
}

fn target_kind(
    policy: &NormalizedFileProtectionPolicy,
    target_id: u32,
) -> Result<FileProtectionTargetKind, BackendError> {
    let index = target_id
        .checked_sub(1)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| compile("invalid structured target id"))?;
    policy
        .targets
        .get(index)
        .map(|target| target.kind)
        .ok_or_else(|| compile("topology references an unknown structured target"))
}

fn target_bit(target_id: u32) -> Result<u64, BackendError> {
    if (1..=64).contains(&target_id) {
        Ok(1u64 << (target_id - 1))
    } else {
        Err(compile("structured target id is outside 1..=64"))
    }
}

fn generation(binding_id: Uuid, revision: &str) -> Result<FileProtectionGeneration, BackendError> {
    let mut bytes = Vec::with_capacity(16 + revision.len());
    bytes.extend_from_slice(binding_id.as_bytes());
    bytes.extend_from_slice(revision.as_bytes());
    let value = stable_hash64(&bytes) as u32;
    FileProtectionGeneration::new(value.max(1))
        .map_err(|error| compile(format!("derive file-protection generation: {error}")))
}

fn stable_hash64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn activate_validated<T>(
    topology: &PreparedFileProtectionTopology,
    activate: impl FnOnce() -> io::Result<T>,
) -> Result<T, BackendError> {
    topology.validate()?;
    activate().map_err(|error| kernel("activate structured file-protection generation", error))
}

fn compile(message: impl Into<String>) -> BackendError {
    BackendError::CompileFailure(message.into())
}

fn kernel(context: &str, error: io::Error) -> BackendError {
    BackendError::KernelFailure(format!("{context}: {error}"))
}

#[cfg(test)]
#[path = "file_protection_runtime_tests.rs"]
mod tests;
