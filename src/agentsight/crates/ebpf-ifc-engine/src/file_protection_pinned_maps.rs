//! Pinned Aya map access for structured file-protection generations.

use std::io;

use aya::Pod;

use super::file_protection_generation::FileProtectionStaging;
use super::file_protection_map_writer::{
    FileProtectionDomainValue, FileProtectionGeneration, FileProtectionMapCounts,
    FileProtectionMapWriter, FileProtectionSlot, FileProtectionSlotValue,
    FileProtectionTargetValue, TreeGuard, TreePathIdentity,
};
use super::{
    ignore_missing_remove, pinned_hash_map, pinned_map_data, InodeGuardVal, PinnedEngine,
    PinnedEnginePaths, PolicyDomainVal, PolicyRootVal, PolicySlotKey, PolicySlotVal,
    PolicyStableVal, PolicyTargetKey, PolicyTargetVal, TreePathKey, BPF_NOEXIST,
};

/// Pinned-map writer used by the engine transaction API.
#[derive(Clone, Debug)]
pub struct PinnedFileProtectionMaps {
    paths: PinnedEnginePaths,
}

impl PinnedEngine {
    /// Begins an inactive generation after checking live map usage and capacities.
    pub fn begin_file_protection_generation(
        &self,
        domain: u32,
        generation: FileProtectionGeneration,
        counts: FileProtectionMapCounts,
    ) -> io::Result<FileProtectionStaging> {
        FileProtectionStaging::begin(
            PinnedFileProtectionMaps::new(self.paths.clone()),
            domain,
            generation,
            counts,
        )
    }
}

impl PinnedFileProtectionMaps {
    fn new(paths: PinnedEnginePaths) -> Self {
        Self { paths }
    }

    fn path_key(
        identity: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> TreePathKey {
        TreePathKey {
            ino: identity.ino,
            dev: identity.dev,
            mnt_id: identity.mnt_id,
            domain_id: domain,
            generation: generation.get(),
        }
    }

    fn guard(domain: u32, guard: TreeGuard) -> InodeGuardVal {
        InodeGuardVal {
            flags: guard.flags,
            domain_id: domain,
            rule_id: guard.rule_id,
            _pad: 0,
        }
    }

    fn slot_key(
        mut slot: FileProtectionSlot,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> PolicySlotKey {
        slot.parent.mnt_id = 0;
        PolicySlotKey {
            parent: Self::path_key(slot.parent, domain, generation),
            name_len: slot.name_len,
            name: slot.name,
            _pad: 0,
        }
    }

    fn target_key(
        domain: u32,
        generation: FileProtectionGeneration,
        target_id: u32,
    ) -> PolicyTargetKey {
        PolicyTargetKey {
            domain_id: domain,
            generation: generation.get(),
            target_id,
            _pad: 0,
        }
    }
}

impl FileProtectionMapWriter for PinnedFileProtectionMaps {
    fn usage(&mut self, name: &'static str) -> io::Result<(usize, usize)> {
        let data = pinned_map_data(&self.paths, name)?;
        let maximum = data
            .info()
            .map_err(|error| other(format!("inspect {name}: {error}")))?
            .max_entries() as usize;
        let used = match name {
            "te_policy_roots" | "te_policy_dirs" => {
                pinned_len::<TreePathKey, PolicyRootVal>(&self.paths, name)?
            }
            "te_policy_stable" => pinned_len::<TreePathKey, PolicyStableVal>(&self.paths, name)?,
            "te_policy_slots" => pinned_len::<PolicySlotKey, PolicySlotVal>(&self.paths, name)?,
            "te_policy_targets" => {
                pinned_len::<PolicyTargetKey, PolicyTargetVal>(&self.paths, name)?
            }
            "te_policy_domains" => pinned_len::<u32, PolicyDomainVal>(&self.paths, name)?,
            _ => return Err(other(format!("unknown file protection map {name}"))),
        };
        Ok((used, maximum))
    }

    fn insert_root(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        root_mask: u64,
    ) -> io::Result<()> {
        pinned_hash_map(&self.paths, "te_policy_roots")?
            .insert(
                Self::path_key(key, domain, generation),
                PolicyRootVal { root_mask },
                BPF_NOEXIST,
            )
            .map_err(|error| other(format!("stage policy root: {error}")))
    }

    fn remove_root(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()> {
        let mut map =
            pinned_hash_map::<TreePathKey, PolicyRootVal>(&self.paths, "te_policy_roots")?;
        ignore_missing_remove(
            map.remove(&Self::path_key(key, domain, generation)),
            "remove policy root",
        )
    }

    fn insert_directory(
        &mut self,
        mut key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        root_mask: u64,
    ) -> io::Result<()> {
        key.mnt_id = 0;
        pinned_hash_map(&self.paths, "te_policy_dirs")?
            .insert(
                Self::path_key(key, domain, generation),
                PolicyRootVal { root_mask },
                BPF_NOEXIST,
            )
            .map_err(|error| other(format!("stage policy directory: {error}")))
    }

    fn remove_directory(
        &mut self,
        mut key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()> {
        key.mnt_id = 0;
        let mut map = pinned_hash_map::<TreePathKey, PolicyRootVal>(&self.paths, "te_policy_dirs")?;
        ignore_missing_remove(
            map.remove(&Self::path_key(key, domain, generation)),
            "remove policy directory",
        )
    }

    fn insert_stable_path(
        &mut self,
        mut key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        target_mask: u64,
    ) -> io::Result<()> {
        key.mnt_id = 0;
        pinned_hash_map(&self.paths, "te_policy_stable")?
            .insert(
                Self::path_key(key, domain, generation),
                PolicyStableVal { target_mask },
                BPF_NOEXIST,
            )
            .map_err(|error| other(format!("stage stable policy path: {error}")))
    }

    fn remove_stable_path(
        &mut self,
        mut key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()> {
        key.mnt_id = 0;
        let mut map =
            pinned_hash_map::<TreePathKey, PolicyStableVal>(&self.paths, "te_policy_stable")?;
        ignore_missing_remove(
            map.remove(&Self::path_key(key, domain, generation)),
            "remove stable policy path",
        )
    }

    fn insert_slot(
        &mut self,
        key: FileProtectionSlot,
        domain: u32,
        generation: FileProtectionGeneration,
        value: FileProtectionSlotValue,
    ) -> io::Result<()> {
        let value = PolicySlotVal {
            direct_mask: value.direct_mask,
            exact_exclude_mask: value.exact_exclude_mask,
            subtree_exclude_mask: value.subtree_exclude_mask,
        };
        pinned_hash_map(&self.paths, "te_policy_slots")?
            .insert(Self::slot_key(key, domain, generation), value, BPF_NOEXIST)
            .map_err(|error| other(format!("stage policy slot: {error}")))
    }

    fn remove_slot(
        &mut self,
        key: FileProtectionSlot,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()> {
        let mut map =
            pinned_hash_map::<PolicySlotKey, PolicySlotVal>(&self.paths, "te_policy_slots")?;
        ignore_missing_remove(
            map.remove(&Self::slot_key(key, domain, generation)),
            "remove policy slot",
        )
    }

    fn insert_target(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        value: FileProtectionTargetValue,
    ) -> io::Result<()> {
        let target = PolicyTargetVal {
            guard: Self::guard(domain, value.guard),
            flags: value.flags,
            target_id: value.target_id,
        };
        pinned_hash_map(&self.paths, "te_policy_targets")?
            .insert(
                Self::target_key(domain, generation, value.target_id),
                target,
                BPF_NOEXIST,
            )
            .map_err(|error| other(format!("stage policy target: {error}")))
    }

    fn remove_target(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        target_id: u32,
    ) -> io::Result<()> {
        let mut map =
            pinned_hash_map::<PolicyTargetKey, PolicyTargetVal>(&self.paths, "te_policy_targets")?;
        ignore_missing_remove(
            map.remove(&Self::target_key(domain, generation, target_id)),
            "remove policy target",
        )
    }

    fn activate(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        value: FileProtectionDomainValue,
    ) -> io::Result<()> {
        let value = PolicyDomainVal {
            generation: generation.get(),
            target_count: value.target_count,
            tree_mask: value.tree_mask,
            delete_mask: value.delete_mask,
            ordinary_write_mask: value.ordinary_write_mask,
            protection_set_id: value.protection_set_id,
            guard: Self::guard(domain, value.guard),
        };
        pinned_hash_map(&self.paths, "te_policy_domains")?
            .insert(domain, value, BPF_NOEXIST)
            .map_err(|error| other(format!("activate policy domain: {error}")))
    }

    fn deactivate(&mut self, domain: u32, generation: FileProtectionGeneration) -> io::Result<()> {
        let mut map = pinned_hash_map::<u32, PolicyDomainVal>(&self.paths, "te_policy_domains")?;
        match map.get(&domain, 0) {
            Ok(value) if value.generation == generation.get() => {
                ignore_missing_remove(map.remove(&domain), "deactivate policy domain")
            }
            Ok(_)
            | Err(aya::maps::MapError::KeyNotFound | aya::maps::MapError::ElementNotFound) => {
                Ok(())
            }
            Err(error) => Err(other(format!("read policy domain activation: {error}"))),
        }
    }
}

fn pinned_len<K: Pod, V: Pod>(paths: &PinnedEnginePaths, name: &str) -> io::Result<usize> {
    pinned_hash_map::<K, V>(paths, name)?
        .keys()
        .map(|key| key.map_err(|error| other(format!("list {name}: {error}"))))
        .collect::<io::Result<Vec<_>>>()
        .map(|keys| keys.len())
}

fn other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_key_matches_underlying_parent_and_zeroes_explicit_padding() {
        let slot = FileProtectionSlot::new(
            TreePathIdentity {
                ino: 11,
                dev: 22,
                mnt_id: 33,
            },
            "entry",
        )
        .expect("valid slot");
        let generation = FileProtectionGeneration::new(44).expect("valid generation");

        let key = PinnedFileProtectionMaps::slot_key(slot, 55, generation);

        assert_eq!(key.parent.mnt_id, 0);
        assert_eq!(key._pad, 0);
    }
}
