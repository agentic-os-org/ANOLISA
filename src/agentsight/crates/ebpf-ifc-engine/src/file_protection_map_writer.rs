//! Map writer contract and value types shared by structured-policy generations.

use std::fmt;
use std::io;
use std::num::NonZeroU32;

/// Fixed capacities compiled into each structured file-protection BPF map.
pub const FILE_PROTECTION_MAP_CAPACITY: usize = 4096;
/// Maximum normalized targets representable by one policy binding.
pub const FILE_PROTECTION_MAX_TARGETS: usize = 64;
/// Linux directory-entry basename limit represented by the map ABI.
pub const FILE_PROTECTION_NAME_MAX: usize = 255;

/// Nonzero immutable generation associated with one staged topology.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileProtectionGeneration(NonZeroU32);

impl FileProtectionGeneration {
    /// Creates a generation, rejecting zero because it cannot identify staged state.
    pub fn new(value: u32) -> io::Result<Self> {
        NonZeroU32::new(value).map(Self).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "generation must be nonzero")
        })
    }

    /// Returns the map ABI value.
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// statx-derived identity used by path-based policy maps.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TreePathIdentity {
    /// Inode number.
    pub ino: u64,
    /// Kernel-format device number.
    pub dev: u32,
    /// Mount ID from `STATX_MNT_ID`; zero is reserved for underlying-directory keys.
    pub mnt_id: u32,
}

/// Guard payload retained in events and activation records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeGuard {
    /// Delete/rename/tree guard flags.
    pub flags: u32,
    /// Stable lowered rule or structured target identifier.
    pub rule_id: u32,
}

/// One exact basename slot under an underlying parent-directory identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileProtectionSlot {
    /// Parent directory identity.
    pub parent: TreePathIdentity,
    /// Basename bytes; bytes after `name_len` are zero.
    pub name: [u8; FILE_PROTECTION_NAME_MAX + 1],
    /// Basename byte length.
    pub name_len: u32,
}

impl FileProtectionSlot {
    /// Builds a verifier-compatible exact slot key.
    pub fn new(parent: TreePathIdentity, name: &str) -> io::Result<Self> {
        let bytes = name.as_bytes();
        if bytes.len() > FILE_PROTECTION_NAME_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file protection basename exceeds 255 bytes",
            ));
        }
        let mut stored = [0; FILE_PROTECTION_NAME_MAX + 1];
        stored[..bytes.len()].copy_from_slice(bytes);
        let slot = Self {
            parent,
            name: stored,
            name_len: bytes.len() as u32,
        };
        slot.validate()?;
        Ok(slot)
    }

    /// Rejects malformed public-field keys before they reach a map writer.
    pub(crate) fn validate(&self) -> io::Result<()> {
        if !(1..=FILE_PROTECTION_NAME_MAX as u32).contains(&self.name_len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file protection basename length must be 1..=255",
            ));
        }
        let name = &self.name[..self.name_len as usize];
        if name.contains(&0)
            || name.contains(&b'/')
            || name == b"."
            || name == b".."
            || self.name[self.name_len as usize..]
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file protection basename must be a NUL-free component with a zeroed tail",
            ));
        }
        Ok(())
    }
}

/// Masks attached to one exact path slot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FileProtectionSlotValue {
    /// Explicit direct targets at this slot.
    pub direct_mask: u64,
    /// Exact exclusions owned by directory-root bits.
    pub exact_exclude_mask: u64,
    /// Subtree exclusion boundaries owned by directory-root bits.
    pub subtree_exclude_mask: u64,
}

/// Per-target operation bits and stable event identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileProtectionTargetValue {
    /// Delete, ordinary-write, and target-kind flags interpreted by BPF.
    pub flags: u32,
    /// Stable normalized target id in `1..=64`.
    pub target_id: u32,
    /// Event guard carrying the same stable target identity.
    pub guard: TreeGuard,
}

/// Activation payload published only after every generation key is staged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileProtectionDomainValue {
    /// Number of normalized targets represented by the masks.
    pub target_count: u32,
    /// Directory-tree target bits.
    pub tree_mask: u64,
    /// Targets requesting delete protection.
    pub delete_mask: u64,
    /// Targets requesting ordinary write protection.
    pub ordinary_write_mask: u64,
    /// Stable logical policy-set identity selected by userspace.
    pub protection_set_id: u64,
    /// Fail-closed fallback event identity.
    pub guard: TreeGuard,
}

/// Entries required from each production map for one generation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FileProtectionMapCounts {
    /// Canonical roots, aliases, and submount roots.
    pub roots: usize,
    /// Underlying directory anchors.
    pub directories: usize,
    /// Stable direct directory targets and configured ancestors.
    pub stable_paths: usize,
    /// Exact target and exclusion basename slots.
    pub slots: usize,
    /// Per-target operation records.
    pub targets: usize,
    /// Activation records; currently zero or one.
    pub domains: usize,
}

impl FileProtectionMapCounts {
    /// Rejects counts that cannot fit the fixed map capacities or target budget.
    pub fn validate(self) -> io::Result<()> {
        for (name, count) in [
            ("te_policy_roots", self.roots),
            ("te_policy_dirs", self.directories),
            ("te_policy_stable", self.stable_paths),
            ("te_policy_slots", self.slots),
            ("te_policy_targets", self.targets),
            ("te_policy_domains", self.domains),
        ] {
            if count > FILE_PROTECTION_MAP_CAPACITY {
                return Err(io::Error::other(format!(
                    "{name} requires {count} entries, exceeding fixed capacity {FILE_PROTECTION_MAP_CAPACITY}"
                )));
            }
        }
        if self.targets > FILE_PROTECTION_MAX_TARGETS {
            return Err(io::Error::other(format!(
                "structured file protection supports at most {FILE_PROTECTION_MAX_TARGETS} targets"
            )));
        }
        if self.domains > 1 {
            return Err(io::Error::other(
                "one generation can activate at most one domain record",
            ));
        }
        Ok(())
    }
}

/// Narrow writer seam used by pinned maps and in-memory transaction tests.
pub trait FileProtectionMapWriter: fmt::Debug {
    /// Returns `(existing entries, maximum entries)` for a named map.
    fn usage(&mut self, map: &'static str) -> io::Result<(usize, usize)>;
    /// Inserts one canonical, alias, or submount root mask.
    fn insert_root(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        root_mask: u64,
    ) -> io::Result<()>;
    /// Removes one root key.
    fn remove_root(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()>;
    /// Inserts one underlying-directory root mask.
    fn insert_directory(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        root_mask: u64,
    ) -> io::Result<()>;
    /// Removes one underlying-directory key.
    fn remove_directory(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()>;
    /// Inserts one stable direct directory or configured ancestor mask.
    fn insert_stable_path(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
        target_mask: u64,
    ) -> io::Result<()>;
    /// Removes one stable-path key.
    fn remove_stable_path(
        &mut self,
        key: TreePathIdentity,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()>;
    /// Inserts one exact basename slot.
    fn insert_slot(
        &mut self,
        key: FileProtectionSlot,
        domain: u32,
        generation: FileProtectionGeneration,
        value: FileProtectionSlotValue,
    ) -> io::Result<()>;
    /// Removes one exact basename slot.
    fn remove_slot(
        &mut self,
        key: FileProtectionSlot,
        domain: u32,
        generation: FileProtectionGeneration,
    ) -> io::Result<()>;
    /// Inserts one stable target record.
    fn insert_target(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        value: FileProtectionTargetValue,
    ) -> io::Result<()>;
    /// Removes one target record.
    fn remove_target(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        target_id: u32,
    ) -> io::Result<()>;
    /// Activates a generation after every path record is staged.
    fn activate(
        &mut self,
        domain: u32,
        generation: FileProtectionGeneration,
        value: FileProtectionDomainValue,
    ) -> io::Result<()>;
    /// Deactivates only when the currently active generation matches.
    fn deactivate(&mut self, domain: u32, generation: FileProtectionGeneration) -> io::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: TreePathIdentity = TreePathIdentity {
        ino: 42,
        dev: 0x0801,
        mnt_id: 7,
    };

    #[test]
    fn generation_rejects_zero_and_keeps_nonzero_value() {
        let error =
            FileProtectionGeneration::new(0).expect_err("generation zero cannot identify state");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            FileProtectionGeneration::new(9)
                .expect("nonzero generation")
                .get(),
            9
        );
    }

    #[test]
    fn slot_keys_zero_every_byte_after_the_basename() {
        let slot = FileProtectionSlot::new(IDENTITY, "config.json").expect("valid basename");
        assert_eq!(slot.parent, IDENTITY);
        assert_eq!(slot.name_len, 11);
        assert_eq!(&slot.name[..11], b"config.json");
        assert!(
            slot.name[11..].iter().all(|byte| *byte == 0),
            "map-key tail bytes must be deterministic"
        );
    }

    #[test]
    fn slot_keys_accept_the_full_basename_range_and_reject_the_rest() {
        let longest = "n".repeat(FILE_PROTECTION_NAME_MAX);
        assert_eq!(
            FileProtectionSlot::new(IDENTITY, &longest)
                .expect("255-byte basename")
                .name_len as usize,
            FILE_PROTECTION_NAME_MAX
        );
        for rejected in [
            "",
            &"n".repeat(FILE_PROTECTION_NAME_MAX + 1),
            "bad\0name",
            "a/b",
            ".",
            "..",
        ] {
            let error = FileProtectionSlot::new(IDENTITY, rejected)
                .expect_err("invalid basename must be rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn map_counts_reject_oversized_generations() {
        FileProtectionMapCounts {
            roots: FILE_PROTECTION_MAP_CAPACITY,
            slots: FILE_PROTECTION_MAP_CAPACITY,
            targets: FILE_PROTECTION_MAX_TARGETS,
            domains: 1,
            ..FileProtectionMapCounts::default()
        }
        .validate()
        .expect("exact capacities must be accepted");

        for (counts, expected) in [
            (
                FileProtectionMapCounts {
                    directories: FILE_PROTECTION_MAP_CAPACITY + 1,
                    ..FileProtectionMapCounts::default()
                },
                "te_policy_dirs",
            ),
            (
                FileProtectionMapCounts {
                    stable_paths: FILE_PROTECTION_MAP_CAPACITY + 1,
                    ..FileProtectionMapCounts::default()
                },
                "te_policy_stable",
            ),
            (
                FileProtectionMapCounts {
                    targets: FILE_PROTECTION_MAX_TARGETS + 1,
                    ..FileProtectionMapCounts::default()
                },
                "at most 64 targets",
            ),
            (
                FileProtectionMapCounts {
                    domains: 2,
                    ..FileProtectionMapCounts::default()
                },
                "at most one domain record",
            ),
        ] {
            let error = counts.validate().expect_err("oversized counts must fail");
            assert!(
                error.to_string().contains(expected),
                "unexpected error for {counts:?}: {error}"
            );
        }
    }

    #[test]
    fn abi_payload_types_round_trip_their_masks_and_guards() {
        let slot = FileProtectionSlotValue {
            direct_mask: 0b001,
            exact_exclude_mask: 0b010,
            subtree_exclude_mask: 0b100,
        };
        assert_eq!(slot.direct_mask | slot.exact_exclude_mask, 0b011);
        assert_eq!(FileProtectionSlotValue::default().direct_mask, 0);

        let target = FileProtectionTargetValue {
            flags: 0b11,
            target_id: 1,
            guard: TreeGuard {
                flags: 0b1001,
                rule_id: 1,
            },
        };
        assert_eq!(
            target.guard.rule_id, target.target_id,
            "events must report the stable target id"
        );

        let domain = FileProtectionDomainValue {
            target_count: 1,
            tree_mask: 0b1,
            delete_mask: 0b1,
            ordinary_write_mask: 0,
            protection_set_id: 77,
            guard: TreeGuard {
                flags: target.guard.flags,
                rule_id: 0,
            },
        };
        assert_eq!(domain.target_count, domain.tree_mask.count_ones());
        assert_eq!(domain.delete_mask & domain.ordinary_write_mask, 0);
        assert_eq!(domain.protection_set_id, 77);
    }
}
