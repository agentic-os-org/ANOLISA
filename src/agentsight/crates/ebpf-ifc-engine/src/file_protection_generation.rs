//! Generation-scoped transactional registration for structured file-protection maps.

use std::io;

use super::file_protection_map_writer::{
    FileProtectionGeneration, FileProtectionMapCounts, FileProtectionMapWriter, FileProtectionSlot,
    FileProtectionSlotValue, FileProtectionTargetValue, TreePathIdentity,
};

/// A generation that has not yet published its domain activation record.
#[derive(Debug)]
pub struct FileProtectionStaging<W: FileProtectionMapWriter> {
    writer: Option<W>,
    domain: u32,
    generation: FileProtectionGeneration,
    limits: FileProtectionMapCounts,
    roots: Vec<TreePathIdentity>,
    directories: Vec<TreePathIdentity>,
    stable_paths: Vec<TreePathIdentity>,
    slots: Vec<FileProtectionSlot>,
    targets: Vec<u32>,
}

impl<W: FileProtectionMapWriter> FileProtectionStaging<W> {
    /// Begins a transaction after checking actual pinned-map capacities.
    pub fn begin(
        mut writer: W,
        domain: u32,
        generation: FileProtectionGeneration,
        counts: FileProtectionMapCounts,
    ) -> io::Result<Self> {
        if domain == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "domain must be nonzero",
            ));
        }
        counts.validate()?;
        for (map, needed) in [
            ("te_policy_roots", counts.roots),
            ("te_policy_dirs", counts.directories),
            ("te_policy_stable", counts.stable_paths),
            ("te_policy_slots", counts.slots),
            ("te_policy_targets", counts.targets),
            ("te_policy_domains", counts.domains),
        ] {
            let (used, maximum) = writer.usage(map)?;
            if needed > maximum.saturating_sub(used) {
                return Err(other(format!(
                    "{map} capacity precheck failed: {used} existing + {needed} staged > {maximum}"
                )));
            }
        }
        Ok(Self {
            writer: Some(writer),
            domain,
            generation,
            limits: counts,
            roots: Vec::new(),
            directories: Vec::new(),
            stable_paths: Vec::new(),
            slots: Vec::new(),
            targets: Vec::new(),
        })
    }

    /// Stages one canonical, alias, or submount root mask.
    pub fn stage_root(&mut self, identity: TreePathIdentity, root_mask: u64) -> io::Result<()> {
        validate_identity(identity)?;
        if identity.mnt_id == 0 {
            return Err(other("policy root requires a mount-aware identity"));
        }
        validate_mask(root_mask)?;
        self.ensure_room(self.roots.len(), self.limits.roots, "policy roots")?;
        let (domain, generation) = (self.domain, self.generation);
        if let Err(error) = self
            .writer_mut()?
            .insert_root(identity, domain, generation, root_mask)
        {
            let cleanup = self.cleanup();
            return Err(with_cleanup("stage policy root", error, cleanup));
        }
        self.roots.push(identity);
        Ok(())
    }

    /// Stages one underlying-directory root mask.
    pub fn stage_directory(
        &mut self,
        identity: TreePathIdentity,
        root_mask: u64,
    ) -> io::Result<()> {
        validate_identity(identity)?;
        validate_mask(root_mask)?;
        self.ensure_room(
            self.directories.len(),
            self.limits.directories,
            "policy directories",
        )?;
        let (domain, generation) = (self.domain, self.generation);
        if let Err(error) = self
            .writer_mut()?
            .insert_directory(identity, domain, generation, root_mask)
        {
            let cleanup = self.cleanup();
            return Err(with_cleanup("stage policy directory", error, cleanup));
        }
        self.directories.push(identity);
        Ok(())
    }

    /// Stages one stable direct-directory or configured-ancestor mask.
    pub fn stage_stable_path(
        &mut self,
        identity: TreePathIdentity,
        target_mask: u64,
    ) -> io::Result<()> {
        validate_identity(identity)?;
        validate_mask(target_mask)?;
        self.ensure_room(
            self.stable_paths.len(),
            self.limits.stable_paths,
            "stable paths",
        )?;
        let (domain, generation) = (self.domain, self.generation);
        if let Err(error) =
            self.writer_mut()?
                .insert_stable_path(identity, domain, generation, target_mask)
        {
            let cleanup = self.cleanup();
            return Err(with_cleanup("stage stable path", error, cleanup));
        }
        self.stable_paths.push(identity);
        Ok(())
    }

    /// Stages one exact direct-target or exclusion slot.
    pub fn stage_slot(
        &mut self,
        slot: FileProtectionSlot,
        value: FileProtectionSlotValue,
    ) -> io::Result<()> {
        validate_identity(slot.parent)?;
        if value.direct_mask | value.exact_exclude_mask | value.subtree_exclude_mask == 0 {
            return Err(other("file protection slot has no classification bits"));
        }
        self.ensure_room(self.slots.len(), self.limits.slots, "policy slots")?;
        let (domain, generation) = (self.domain, self.generation);
        if let Err(error) = self
            .writer_mut()?
            .insert_slot(slot, domain, generation, value)
        {
            let cleanup = self.cleanup();
            return Err(with_cleanup("stage policy slot", error, cleanup));
        }
        self.slots.push(slot);
        Ok(())
    }

    /// Stages one normalized target record.
    pub fn stage_target(&mut self, value: FileProtectionTargetValue) -> io::Result<()> {
        validate_target_id(value.target_id)?;
        if value.guard.rule_id != value.target_id {
            return Err(other("target guard rule_id must equal stable target_id"));
        }
        self.ensure_room(self.targets.len(), self.limits.targets, "policy targets")?;
        let (domain, generation) = (self.domain, self.generation);
        if let Err(error) = self.writer_mut()?.insert_target(domain, generation, value) {
            let cleanup = self.cleanup();
            return Err(with_cleanup("stage policy target", error, cleanup));
        }
        self.targets.push(value.target_id);
        Ok(())
    }

    fn ensure_room(&self, current: usize, limit: usize, name: &str) -> io::Result<()> {
        if current >= limit {
            Err(other(format!("staged {name} exceed the prechecked count")))
        } else {
            Ok(())
        }
    }

    fn writer_mut(&mut self) -> io::Result<&mut W> {
        self.writer
            .as_mut()
            .ok_or_else(|| other("file protection writer is no longer available"))
    }

    fn cleanup(&mut self) -> Vec<String> {
        let Some(mut writer) = self.writer.take() else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        remove_tracked(
            &mut self.targets,
            |target_id| writer.remove_target(self.domain, self.generation, target_id),
            &mut errors,
        );
        remove_tracked(
            &mut self.slots,
            |key| writer.remove_slot(key, self.domain, self.generation),
            &mut errors,
        );
        remove_tracked(
            &mut self.stable_paths,
            |key| writer.remove_stable_path(key, self.domain, self.generation),
            &mut errors,
        );
        remove_tracked(
            &mut self.directories,
            |key| writer.remove_directory(key, self.domain, self.generation),
            &mut errors,
        );
        remove_tracked(
            &mut self.roots,
            |key| writer.remove_root(key, self.domain, self.generation),
            &mut errors,
        );
        self.writer = Some(writer);
        errors
    }
}

impl<W: FileProtectionMapWriter> Drop for FileProtectionStaging<W> {
    fn drop(&mut self) {
        let errors = self.cleanup();
        if !errors.is_empty() {
            log::error!(
                "inactive file protection generation cleanup failed: {}",
                errors.join("; ")
            );
        }
    }
}

fn validate_identity(identity: TreePathIdentity) -> io::Result<()> {
    if identity.ino == 0 {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tree path identity requires a nonzero inode",
        ))
    } else {
        Ok(())
    }
}

fn validate_mask(mask: u64) -> io::Result<()> {
    if mask == 0 {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file protection mask must be nonzero",
        ))
    } else {
        Ok(())
    }
}

fn validate_target_id(target_id: u32) -> io::Result<()> {
    if (1..=64).contains(&target_id) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file protection target id must be in 1..=64",
        ))
    }
}

fn remove_tracked<K: Copy>(
    keys: &mut Vec<K>,
    mut remove: impl FnMut(K) -> io::Result<()>,
    errors: &mut Vec<String>,
) {
    let mut failed = Vec::new();
    for key in keys.drain(..).rev() {
        if let Err(error) = remove(key) {
            errors.push(error.to_string());
            failed.push(key);
        }
    }
    failed.reverse();
    *keys = failed;
}

fn with_cleanup(context: &str, error: io::Error, cleanup: Vec<String>) -> io::Error {
    if cleanup.is_empty() {
        other(format!("{context}: {error}"))
    } else {
        other(format!(
            "{context}: {error}; rollback failed: {}",
            cleanup.join("; ")
        ))
    }
}

fn other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

#[cfg(test)]
#[path = "file_protection_generation_tests.rs"]
mod tests;
