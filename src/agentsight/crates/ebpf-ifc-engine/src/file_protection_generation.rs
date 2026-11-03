//! Generation-scoped transactional registration for structured file-protection maps.

use std::fmt;
use std::io;

use super::file_protection_map_writer::{
    FileProtectionDomainValue, FileProtectionGeneration, FileProtectionMapCounts,
    FileProtectionMapWriter, FileProtectionSlot, FileProtectionSlotValue,
    FileProtectionTargetValue, TreePathIdentity,
};
use super::file_protection_pinned_maps::PinnedFileProtectionMaps;

/// A generation that has not yet published its domain activation record.
/// Callers must merge same-key masks before staging each key once; counts are distinct map keys.
#[derive(Debug)]
pub struct FileProtectionStaging<W: FileProtectionMapWriter = PinnedFileProtectionMaps> {
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

/// Active generation owner. Dropping it deactivates first, then attempts cleanup.
#[derive(Debug)]
pub struct ActiveFileProtectionGeneration<W: FileProtectionMapWriter = PinnedFileProtectionMaps> {
    staging: FileProtectionStaging<W>,
    active: bool,
}

/// Inactive generation whose partial cleanup can be retried without losing key metadata.
#[derive(Debug)]
pub struct InactiveFileProtectionGeneration<W: FileProtectionMapWriter = PinnedFileProtectionMaps> {
    staging: FileProtectionStaging<W>,
    needs_deactivation: bool,
}

/// Detach failure that retains the inactive retry owner and any held writer state.
#[derive(Debug)]
pub struct FileProtectionDetachError<W: FileProtectionMapWriter = PinnedFileProtectionMaps> {
    /// Individual map cleanup errors.
    pub errors: Vec<String>,
    /// Retry owner; activation may still exist when deactivation failed.
    pub retry: InactiveFileProtectionGeneration<W>,
}

impl<W: FileProtectionMapWriter> fmt::Display for FileProtectionDetachError<W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "file protection cleanup failed: {}",
            self.errors.join("; ")
        )
    }
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
        slot.validate()?;
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

    /// Publishes the activation record last; activation failure rolls back staged keys.
    pub fn activate(
        mut self,
        value: FileProtectionDomainValue,
    ) -> io::Result<ActiveFileProtectionGeneration<W>> {
        if self.limits.domains != 1 {
            let cleanup = self.cleanup();
            return Err(with_cleanup(
                "activate file protection generation",
                other("activation was not included in the prechecked count"),
                cleanup,
            ));
        }
        if value.target_count as usize != self.targets.len() {
            let cleanup = self.cleanup();
            return Err(with_cleanup(
                "activate file protection generation",
                other("activation target count does not match staged targets"),
                cleanup,
            ));
        }
        let (domain, generation) = (self.domain, self.generation);
        if let Err(error) = self.writer_mut()?.activate(domain, generation, value) {
            let cleanup = self.cleanup();
            return Err(with_cleanup(
                "activate file protection generation",
                error,
                cleanup,
            ));
        }
        Ok(ActiveFileProtectionGeneration {
            staging: self,
            active: true,
        })
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

impl<W: FileProtectionMapWriter> ActiveFileProtectionGeneration<W> {
    /// Deactivates first and then removes only this generation's staged keys.
    // Keep the retry owner inline instead of allocating another owner on cleanup failure.
    #[allow(clippy::result_large_err)]
    pub fn detach(mut self) -> Result<(), FileProtectionDetachError<W>> {
        let (domain, generation) = (self.staging.domain, self.staging.generation);
        if let Err(error) = self
            .staging
            .writer_mut()
            .and_then(|writer| writer.deactivate(domain, generation))
        {
            self.active = false;
            return Err(FileProtectionDetachError {
                errors: vec![format!("deactivate generation: {error}")],
                retry: InactiveFileProtectionGeneration {
                    staging: take_staging(&mut self.staging),
                    needs_deactivation: true,
                },
            });
        }
        self.active = false;
        let errors = self.staging.cleanup();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(FileProtectionDetachError {
                errors,
                retry: InactiveFileProtectionGeneration {
                    staging: take_staging(&mut self.staging),
                    needs_deactivation: false,
                },
            })
        }
    }
}

impl<W: FileProtectionMapWriter> Drop for ActiveFileProtectionGeneration<W> {
    fn drop(&mut self) {
        if self.active {
            let (domain, generation) = (self.staging.domain, self.staging.generation);
            if self
                .staging
                .writer_mut()
                .is_ok_and(|writer| writer.deactivate(domain, generation).is_ok())
            {
                self.active = false;
            } else {
                log::error!(
                    "active file protection generation {}:{} retained after drop deactivation failure",
                    domain,
                    generation.get()
                );
                let retained = take_staging(&mut self.staging);
                // Exit reclaims this owner, not pinned maps; cleanup must never precede deactivation.
                std::mem::forget(retained);
                self.active = false;
            }
        }
    }
}

impl<W: FileProtectionMapWriter> InactiveFileProtectionGeneration<W> {
    /// Retries deactivation when needed, then idempotently retries partial cleanup.
    // Keep the retry owner inline instead of allocating another owner on cleanup failure.
    #[allow(clippy::result_large_err)]
    pub fn retry_cleanup(mut self) -> Result<(), FileProtectionDetachError<W>> {
        if self.needs_deactivation {
            let (domain, generation) = (self.staging.domain, self.staging.generation);
            if let Err(error) = self
                .staging
                .writer_mut()
                .and_then(|writer| writer.deactivate(domain, generation))
            {
                return Err(FileProtectionDetachError {
                    errors: vec![format!("deactivate generation: {error}")],
                    retry: self,
                });
            }
            self.needs_deactivation = false;
        }
        let errors = self.staging.cleanup();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(FileProtectionDetachError {
                errors,
                retry: self,
            })
        }
    }
}

impl<W: FileProtectionMapWriter> Drop for InactiveFileProtectionGeneration<W> {
    fn drop(&mut self) {
        if self.needs_deactivation {
            log::error!(
                "file protection detach retry {}:{} retained after drop",
                self.staging.domain,
                self.staging.generation.get()
            );
            let retained = take_staging(&mut self.staging);
            // One failed owner is retained per drop; repeated failures are not process-lifetime bounded.
            std::mem::forget(retained);
        }
    }
}

fn take_staging<W: FileProtectionMapWriter>(
    staging: &mut FileProtectionStaging<W>,
) -> FileProtectionStaging<W> {
    FileProtectionStaging {
        writer: staging.writer.take(),
        domain: staging.domain,
        generation: staging.generation,
        limits: staging.limits,
        roots: std::mem::take(&mut staging.roots),
        directories: std::mem::take(&mut staging.directories),
        stable_paths: std::mem::take(&mut staging.stable_paths),
        slots: std::mem::take(&mut staging.slots),
        targets: std::mem::take(&mut staging.targets),
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
        io::Error::new(error.kind(), format!("{context}: {error}"))
    } else {
        io::Error::new(
            error.kind(),
            format!(
                "{context}: {error}; rollback failed: {}",
                cleanup.join("; ")
            ),
        )
    }
}

fn other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

#[cfg(test)]
#[path = "file_protection_generation_tests.rs"]
mod tests;
