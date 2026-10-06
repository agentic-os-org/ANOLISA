//! FUSE namespace-mutation callbacks: `mkdir`, `unlink`, `rmdir`, `rename`.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fuser::{FileType, ReplyEmpty, ReplyEntry, Request};
use skillfs_core::{parser, store::adopt_directory_name};
use tracing::{debug, info, warn};

use super::super::SkillFs;
use crate::path::{PathType, is_skill_discover_path};
use crate::security::{MutationKind, SkillEvent, SkillEventAction, SkillEventKind};
use crate::sync::SyncEvent;
use crate::sys::{
    errno, mkdirat_leaf, open_dir_path, rename_noreplace, renameat2_leaf, unlinkat_leaf,
};

/// Which source directory a skill-dir rename addresses, and therefore
/// how strictly the backing store entry must be identified below the
/// source root. See [`SkillFs::rename_source_backs_store_entry`].
enum RenameSourceIdentity<'a> {
    /// A flat `/skills/<name>` slot. The virtual path resolves THROUGH
    /// the store entry (`skill_physical_dir` reads the recorded
    /// `source_path`), so the entry itself defines the backing
    /// directory: a categorized source viewed through a flat mount
    /// records a deeper origin (`catalog/demo`), and renaming the slot
    /// legitimately migrates that entry. Any depth below the source
    /// root is accepted.
    FlatSlot,
    /// The exact component chain below the source root. An inbox
    /// candidate is always `source/<name>` (bare leaf); a Hermes
    /// nested skill is `<category>/<leaf>`. A same-leaf entry with a
    /// different chain — a real `beta/docs` skill while a plain
    /// `source/docs` candidate is renamed through the inbox — does not
    /// back the renamed directory and must be left alone.
    ExactChain(&'a [String]),
}

impl SkillFs {
    pub(in crate::fs) fn mkdir_impl(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let path_str = match self.build_fuse_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let path_type = self.parse_fuse_path(Path::new(&path_str));

        // L1: the inbox virtual root is always present — refuse to
        // shadow it with a real directory. The inbox-skill case lands
        // in the normal mkdir path below (it creates the physical
        // candidate `source/<skill>` and inserts a store placeholder).
        if matches!(path_type, PathType::InboxDir) {
            reply.error(libc::EEXIST);
            return;
        }
        if let PathType::InboxSkillDir { ref skill_name } = path_type {
            if !Self::is_inbox_skill_name_allowed(skill_name) {
                reply.error(libc::EACCES);
                return;
            }
        }
        if let PathType::InboxPassthrough { ref skill_name, .. } = path_type {
            if !Self::is_inbox_skill_name_allowed(skill_name) {
                reply.error(libc::ENOENT);
                return;
            }
        }

        // The skill-discover namespace is always read-only — mirror the
        // write/open/setattr/symlink/link guards (write.rs) so `mkdir`
        // cannot create (or inject a store placeholder for) the reserved
        // virtual name or mutate its physical backing tree.
        if let Some(errno) = enforce_skill_discover_readonly(&path_type) {
            reply.error(errno);
            return;
        }

        // S3: refuse to mkdir on a reserved lifecycle namespace name. The
        // gate runs before `.skill-meta` enforcement so the lifecycle
        // boundary cannot be sidestepped by also matching `.skill-meta`.
        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Create, req, None)
        {
            reply.error(errno);
            return;
        }

        // S1: refuse to create directories under `.skill-meta/**`.
        if let Some(errno) = self.enforce_skill_meta(&path_type, SkillEventKind::Create, req, None)
        {
            reply.error(errno);
            return;
        }

        // I4/H3: reject mkdir on hidden skills unless the path
        // matches the post-publish grace whitelist. SkillDir /
        // NestedSkillDir mkdir is not gated — creating a new skill
        // directory is the start of an install.
        {
            let reject = match &path_type {
                PathType::Passthrough {
                    skill_name,
                    relative_path,
                } => self.should_reject_hidden_write(skill_name, Some(relative_path)),
                PathType::NestedPassthrough {
                    category,
                    skill_name,
                    relative_path,
                } => self.should_reject_hermes_nested_hidden_write(
                    category,
                    skill_name,
                    Some(relative_path),
                ),
                _ => false,
            };
            if reject {
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Create,
                    SkillEventAction::Rejected,
                    Some(libc::ENOENT),
                    None,
                    Some("class=hidden_skill".to_string()),
                );
                reply.error(libc::ENOENT);
                return;
            }
        }

        let physical = match self.resolve_physical_path(&path_str) {
            Some(p) => p,
            None => {
                self.ro_warn("mkdir", &path_str);
                reply.error(libc::EROFS);
                return;
            }
        };

        debug!(parent, name = %name.to_string_lossy(), ?physical, "mkdir");

        // POSIX: directory permission bits shall be initialized from mode
        // and then masked by the process file-mode creation mask. The FUSE
        // protocol delivers both, so we honor them explicitly instead of
        // inheriting the FUSE daemon's own umask.
        let effective_mode = mode & !umask & 0o7777;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(effective_mode);
        let mkdir_result = match builder.create(&physical) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                // Long-path fallback: the absolute physical path exceeds
                // PATH_MAX, but `mkdir -p`'s component-by-component walk
                // through the kernel only required NAME_MAX per component.
                // Open the parent dir and use `mkdirat` so the leaf name is
                // the only string the syscall sees.
                match self.open_parent_dir_for(&path_str) {
                    Ok((parent_fd, leaf)) => mkdirat_leaf(&parent_fd, &leaf, effective_mode),
                    Err(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        match mkdir_result {
            Ok(()) => {
                let ino = self.inodes.allocate(&path_str, FileType::Directory, parent);
                self.inodes.remember(ino);
                let mut attr = self.dir_attr();
                attr.ino = ino;

                // If this is a skill-level directory, immediately add a placeholder
                // entry so the new skill appears in readdir/lookup right away.
                // The async Reparse (triggered when SKILL.md is later written) will
                // replace the placeholder with the real parsed entry.
                //
                // L1: a `mkdir /.skillfs-inbox/<skill>` is the install
                // entrance for a brand-new (or hidden / repaired)
                // skill. It maps to the same physical
                // `source/<skill>` candidate directory, so the store
                // placeholder also lands here — `scan -> resolve`
                // (triggered later by the install-complete sentinel)
                // can then surface the fully parsed skill at
                // `/skills/<skill>` if the resolver returns
                // `current` / `fallback`.
                let placeholder_skill = match &path_type {
                    PathType::SkillDir { skill_name } => Some(skill_name.clone()),
                    PathType::InboxSkillDir { skill_name }
                        if Self::is_inbox_skill_name_allowed(skill_name) =>
                    {
                        Some(skill_name.clone())
                    }
                    _ => None,
                };
                if let Some(skill_name) = placeholder_skill {
                    use skillfs_core::{ParseStatus, SkillEntry, SkillMetadata};
                    let placeholder = SkillEntry {
                        metadata: SkillMetadata {
                            name: skill_name.clone(),
                            ..SkillMetadata::default()
                        },
                        parameters: vec![],
                        returns: vec![],
                        body: String::new(),
                        parse_status: ParseStatus::Degraded(
                            "directory created, awaiting SKILL.md".to_string(),
                        ),
                        source_path: physical.join("SKILL.md"),
                        last_modified: std::time::SystemTime::now(),
                    };
                    self.store.write().upsert(placeholder);
                    debug!(name = %skill_name, "mkdir: inserted placeholder into store");
                }

                // D1.3-demo: a fresh skill dir or a sub-dir under an
                // existing skill triggers the debounced refresh. New
                // skills stay hidden until the controller's resolve
                // returns `current` / `fallback` because the resolver
                // treats "no entry" as hidden in demo mode (see
                // `SkillFs::resolve_skill_read`).
                //
                // L1: an inbox-side `mkdir` does NOT enqueue a
                // refresh on its own — installers are expected to
                // populate the candidate dir with multiple files and
                // then signal completion via the
                // `.install-complete` sentinel. Inbox sub-dir
                // mkdirs only enqueue when the sub-dir is the
                // sentinel itself (which is unusual but stays
                // consistent with the inbox observe rule).
                match &path_type {
                    PathType::SkillDir { skill_name } => {
                        self.observe_mutation(skill_name, None, MutationKind::Mkdir);
                    }
                    PathType::Passthrough {
                        skill_name,
                        relative_path,
                    } => {
                        self.observe_mutation(
                            skill_name,
                            Some(relative_path.as_path()),
                            MutationKind::Mkdir,
                        );
                    }
                    PathType::InboxPassthrough {
                        skill_name,
                        relative_path,
                    } => {
                        self.inbox_observe_install_complete(
                            skill_name,
                            relative_path.as_path(),
                            MutationKind::Mkdir,
                        );
                    }
                    PathType::NestedSkillDir {
                        category,
                        skill_name,
                    } => {
                        let nested_id = Self::hermes_skill_id(category, skill_name);
                        self.observe_mutation(&nested_id, None, MutationKind::Mkdir);
                    }
                    PathType::NestedPassthrough {
                        category,
                        skill_name,
                        relative_path,
                    } => {
                        let nested_id = Self::hermes_skill_id(category, skill_name);
                        self.observe_mutation(
                            &nested_id,
                            Some(relative_path.as_path()),
                            MutationKind::Mkdir,
                        );
                    }
                    _ => {}
                }

                reply.entry(&Duration::from_secs(1), &attr, 0);
            }
            Err(e) => {
                warn!(op = "mkdir", path = %path_str, error = %e, "mkdir failed");
                reply.error(errno(&e));
            }
        }
    }
    pub(in crate::fs) fn unlink_impl(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: ReplyEmpty,
    ) {
        let path_str = match self.build_fuse_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let path_type = self.parse_fuse_path(Path::new(&path_str));
        let (skill_name_for_event, relative_for_event) = match &path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => (Some(skill_name.clone()), Some(relative_path.clone())),
            PathType::SkillMd { skill_name } => {
                (Some(skill_name.clone()), Some(PathBuf::from("SKILL.md")))
            }
            PathType::SkillDir { skill_name } => (Some(skill_name.clone()), None),
            _ => (None, None),
        };

        // The skill-discover namespace is always read-only — mirror the
        // write/open/setattr/symlink/link guards (write.rs) so `unlink`
        // cannot delete the physical backing tree through the virtual
        // view.
        if let Some(errno) = enforce_skill_discover_readonly(&path_type) {
            reply.error(errno);
            return;
        }

        // S3: refuse to unlink under a reserved lifecycle namespace.
        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Delete, req, None)
        {
            reply.error(errno);
            return;
        }

        // S1: refuse to unlink anything under `.skill-meta/**`.
        if let Some(errno) = self.enforce_skill_meta(&path_type, SkillEventKind::Delete, req, None)
        {
            reply.error(errno);
            return;
        }

        // I4/H3: reject unlink on hidden skills unless grace-allowed.
        {
            let reject = match &path_type {
                PathType::Passthrough {
                    skill_name,
                    relative_path,
                } => self.should_reject_hidden_write(skill_name, Some(relative_path)),
                PathType::SkillMd { skill_name } => {
                    self.should_reject_hidden_write(skill_name, Some(Path::new("SKILL.md")))
                }
                PathType::NestedPassthrough {
                    category,
                    skill_name,
                    relative_path,
                } => self.should_reject_hermes_nested_hidden_write(
                    category,
                    skill_name,
                    Some(relative_path),
                ),
                PathType::NestedSkillMd {
                    category,
                    skill_name,
                } => self.should_reject_hermes_nested_hidden_write(
                    category,
                    skill_name,
                    Some(Path::new("SKILL.md")),
                ),
                _ => false,
            };
            if reject {
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Delete,
                    SkillEventAction::Rejected,
                    Some(libc::ENOENT),
                    None,
                    Some("class=hidden_skill".to_string()),
                );
                reply.error(libc::ENOENT);
                return;
            }
        }

        let physical = match self.resolve_physical_path(&path_str) {
            Some(p) => p,
            None => {
                self.ro_warn("unlink", &path_str);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Delete)
                        .with_optional_skill_name(skill_name_for_event)
                        .with_optional_relative_path(relative_for_event)
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::EROFS)
                        .with_caller(req.uid(), req.gid()),
                );
                reply.error(libc::EROFS);
                return;
            }
        };

        debug!(parent, name = %name.to_string_lossy(), ?physical, "unlink");

        let unlink_result = match std::fs::remove_file(&physical) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                match self.open_parent_dir_for(&path_str) {
                    Ok((parent_fd, leaf)) => unlinkat_leaf(&parent_fd, &leaf, 0),
                    Err(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        match unlink_result {
            Ok(()) => {
                // Remove inode mapping.
                if let Some(ino) = self.inodes.lookup_by_path(&path_str) {
                    self.inodes.remove(ino);
                }
                // Fast-path store sync: if deleting SKILL.md, remove from store.
                if let PathType::SkillMd { skill_name } = &path_type {
                    self.store.write().remove(skill_name);
                    info!(name = %skill_name, "sync: removed skill (SKILL.md deleted)");
                }
                // D1.3-demo: enqueue a refresh against the owning
                // skill so the controller can either install a fresh
                // decision (if any state remains) or hide the entry.
                //
                // L1: inbox unlinks only enqueue a refresh when the
                // leaf is the install-complete sentinel (e.g. the
                // installer is rolling back its own complete signal
                // mid-install). Plain candidate-file deletions during
                // an in-progress install must not run scan/resolve.
                match &path_type {
                    PathType::SkillMd { skill_name } => self.observe_mutation(
                        skill_name,
                        Some(Path::new("SKILL.md")),
                        MutationKind::Unlink,
                    ),
                    PathType::Passthrough {
                        skill_name,
                        relative_path,
                    } => self.observe_mutation(
                        skill_name,
                        Some(relative_path.as_path()),
                        MutationKind::Unlink,
                    ),
                    PathType::InboxPassthrough {
                        skill_name,
                        relative_path,
                    } => self.inbox_observe_install_complete(
                        skill_name,
                        relative_path.as_path(),
                        MutationKind::Unlink,
                    ),
                    PathType::NestedSkillMd {
                        category,
                        skill_name,
                    } => {
                        let nested_id = Self::hermes_skill_id(category, skill_name);
                        self.observe_mutation(
                            &nested_id,
                            Some(Path::new("SKILL.md")),
                            MutationKind::Unlink,
                        );
                    }
                    PathType::NestedPassthrough {
                        category,
                        skill_name,
                        relative_path,
                    } => {
                        let nested_id = Self::hermes_skill_id(category, skill_name);
                        self.observe_mutation(
                            &nested_id,
                            Some(relative_path.as_path()),
                            MutationKind::Unlink,
                        );
                    }
                    _ => {}
                }
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Delete)
                        .with_optional_skill_name(skill_name_for_event)
                        .with_optional_relative_path(relative_for_event)
                        .with_action(SkillEventAction::Allowed)
                        .with_caller(req.uid(), req.gid()),
                );
                reply.ok();
            }
            Err(e) => {
                warn!(op = "unlink", path = %path_str, error = %e, "unlink failed");
                let err = errno(&e);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Delete)
                        .with_optional_skill_name(skill_name_for_event)
                        .with_optional_relative_path(relative_for_event)
                        .with_action(SkillEventAction::Failed)
                        .with_errno(err)
                        .with_caller(req.uid(), req.gid()),
                );
                reply.error(err);
            }
        }
    }
    pub(in crate::fs) fn rmdir_impl(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: ReplyEmpty,
    ) {
        let path_str = match self.build_fuse_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let path_type = self.parse_fuse_path(Path::new(&path_str));

        // The skill-discover namespace is always read-only — mirror the
        // write/open/setattr/symlink/link guards (write.rs) so `rmdir`
        // cannot delete the physical backing tree through the virtual
        // view.
        if let Some(errno) = enforce_skill_discover_readonly(&path_type) {
            reply.error(errno);
            return;
        }

        // S3: refuse to rmdir a reserved lifecycle namespace or any
        // directory beneath one. The gate fires before any physical
        // resolution so the source tree is untouched.
        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Delete, req, None)
        {
            reply.error(errno);
            return;
        }

        // S1: refuse to remove `.skill-meta/**` directories.
        if let Some(errno) = self.enforce_skill_meta(&path_type, SkillEventKind::Delete, req, None)
        {
            reply.error(errno);
            return;
        }

        // I4/H3: reject rmdir on hidden skills unless grace-allowed.
        {
            let reject = match &path_type {
                PathType::Passthrough {
                    skill_name,
                    relative_path,
                } => self.should_reject_hidden_write(skill_name, Some(relative_path)),
                PathType::SkillDir { skill_name } => {
                    self.should_reject_hidden_write(skill_name, None)
                }
                PathType::NestedPassthrough {
                    category,
                    skill_name,
                    relative_path,
                } => self.should_reject_hermes_nested_hidden_write(
                    category,
                    skill_name,
                    Some(relative_path),
                ),
                PathType::NestedSkillDir {
                    category,
                    skill_name,
                } => self.should_reject_hermes_nested_hidden_write(category, skill_name, None),
                _ => false,
            };
            if reject {
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Delete,
                    SkillEventAction::Rejected,
                    Some(libc::ENOENT),
                    None,
                    Some("class=hidden_skill".to_string()),
                );
                reply.error(libc::ENOENT);
                return;
            }
        }

        let physical = match self.resolve_physical_path(&path_str) {
            Some(p) => p,
            None => {
                self.ro_warn("rmdir", &path_str);
                reply.error(libc::EROFS);
                return;
            }
        };

        debug!(parent, name = %name.to_string_lossy(), ?physical, "rmdir");

        let rmdir_result = match std::fs::remove_dir(&physical) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                match self.open_parent_dir_for(&path_str) {
                    Ok((parent_fd, leaf)) => unlinkat_leaf(&parent_fd, &leaf, libc::AT_REMOVEDIR),
                    Err(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        match rmdir_result {
            Ok(()) => {
                // Remove inode and all children.
                self.inodes.remove_recursive(&path_str);
                // Fast-path store sync: if removing a skill directory.
                // L1: inbox-side rmdir of `<inbox>/<skill>` removes the
                // physical `source/<skill>` candidate dir, so the store
                // entry should drop too — the live source for that
                // skill no longer exists.
                let removed_skill = match &path_type {
                    PathType::SkillDir { skill_name } | PathType::InboxSkillDir { skill_name } => {
                        self.store.write().remove(skill_name);
                        info!(name = %skill_name, "sync: removed skill (directory deleted)");
                        Some(skill_name.clone())
                    }
                    _ => None,
                };
                // D1.3-demo: enqueue a refresh. For a removed skill
                // directory the resolve will typically fail (the dir
                // no longer exists) and the controller's
                // failed-resolve policy hides the entry, which lines
                // up with the store removal above.
                //
                // L1: an inbox-side rmdir of the candidate skill dir
                // tears down the runtime mapping the same way (the
                // resolve below will fail because the dir is gone, and
                // the controller's `HideOnFailure` default kicks in).
                // Inbox sub-dir rmdirs only enqueue when the leaf is
                // the install-complete sentinel.
                match &path_type {
                    PathType::SkillDir { .. } | PathType::InboxSkillDir { .. } => {
                        if let Some(name) = removed_skill {
                            self.observe_mutation(&name, None, MutationKind::Rmdir);
                        }
                    }
                    PathType::Passthrough {
                        skill_name,
                        relative_path,
                    } => self.observe_mutation(
                        skill_name,
                        Some(relative_path.as_path()),
                        MutationKind::Rmdir,
                    ),
                    PathType::InboxPassthrough {
                        skill_name,
                        relative_path,
                    } => self.inbox_observe_install_complete(
                        skill_name,
                        relative_path.as_path(),
                        MutationKind::Rmdir,
                    ),
                    PathType::NestedSkillDir {
                        category,
                        skill_name,
                    } => {
                        let nested_id = Self::hermes_skill_id(category, skill_name);
                        self.observe_mutation(&nested_id, None, MutationKind::Rmdir);
                    }
                    PathType::NestedPassthrough {
                        category,
                        skill_name,
                        relative_path,
                    } => {
                        let nested_id = Self::hermes_skill_id(category, skill_name);
                        self.observe_mutation(
                            &nested_id,
                            Some(relative_path.as_path()),
                            MutationKind::Rmdir,
                        );
                    }
                    _ => {}
                }
                reply.ok();
            }
            Err(e) => {
                warn!(op = "rmdir", path = %path_str, error = %e, "rmdir failed");
                reply.error(errno(&e));
            }
        }
    }
    pub(in crate::fs) fn rename_impl(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        newparent: u64,
        newname: &std::ffi::OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        // Phase 1 rename flag policy: only plain rename and `RENAME_NOREPLACE`
        // are supported. Any other bit (including `RENAME_EXCHANGE`,
        // `RENAME_WHITEOUT`, or unknown bits) must be rejected with `EINVAL`
        // so callers don't get a silent fall-through to plain rename.
        #[cfg(target_os = "linux")]
        const SUPPORTED_RENAME_FLAGS: u32 = libc::RENAME_NOREPLACE;
        #[cfg(not(target_os = "linux"))]
        const SUPPORTED_RENAME_FLAGS: u32 = 0;

        if flags & !SUPPORTED_RENAME_FLAGS != 0 {
            warn!(flags, "rename: rejecting unsupported flags");
            self.emit_event(
                SkillEvent::new(SkillEventKind::Rename)
                    .with_action(SkillEventAction::Failed)
                    .with_errno(libc::EINVAL)
                    .with_caller(req.uid(), req.gid())
                    .with_detail(format!("flags=0x{:x}", flags)),
            );
            reply.error(libc::EINVAL);
            return;
        }
        let no_replace = flags & SUPPORTED_RENAME_FLAGS != 0;

        let old_path = match self.build_fuse_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let new_path = match self.build_fuse_path(newparent, newname) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let old_path_type = self.parse_fuse_path(Path::new(&old_path));
        let new_path_type = self.parse_fuse_path(Path::new(&new_path));
        let (event_skill, event_relative) = match &old_path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => (Some(skill_name.clone()), Some(relative_path.clone())),
            PathType::SkillMd { skill_name } => {
                (Some(skill_name.clone()), Some(PathBuf::from("SKILL.md")))
            }
            PathType::SkillDir { skill_name } => (Some(skill_name.clone()), None),
            _ => (None, None),
        };

        // The skill-discover namespace is always read-only — mirror the
        // write/open/setattr/symlink/link guards (write.rs) for both
        // sides of the rename so the physical backing tree can neither
        // be moved out from under the virtual view nor be replaced by a
        // rename onto the reserved name. This gate must fire before the
        // cross-namespace EXDEV short-circuit below: a skill-discover
        // path renamed to or from the inbox is still a mutation of the
        // read-only namespace, and answering it with EXDEV would make
        // `mv` fall back to copy+unlink against the read-only side,
        // leaving a partial target behind.
        if let Some(errno) = enforce_skill_discover_readonly(&old_path_type) {
            reply.error(errno);
            return;
        }
        if let Some(errno) = enforce_skill_discover_readonly(&new_path_type) {
            reply.error(errno);
            return;
        }

        // L1: cross-namespace renames between `/skills` and the
        // inbox would either silently rebind the same physical inode
        // under both namespaces or break the symmetry the inbox is
        // supposed to provide. Refuse with `EXDEV` so callers can
        // re-issue as create + write + unlink on each side.
        let old_is_inbox = matches!(
            old_path_type,
            PathType::InboxDir | PathType::InboxSkillDir { .. } | PathType::InboxPassthrough { .. }
        );
        let new_is_inbox = matches!(
            new_path_type,
            PathType::InboxDir | PathType::InboxSkillDir { .. } | PathType::InboxPassthrough { .. }
        );
        if old_is_inbox != new_is_inbox {
            warn!(
                old = %old_path,
                new = %new_path,
                "rename: refusing cross-namespace rename between inbox and /skills"
            );
            self.emit_event(
                SkillEvent::new(SkillEventKind::Rename)
                    .with_optional_skill_name(event_skill.clone())
                    .with_optional_relative_path(event_relative.clone())
                    .with_action(SkillEventAction::Rejected)
                    .with_errno(libc::EXDEV)
                    .with_caller(req.uid(), req.gid())
                    .with_detail(format!(
                        "class=cross_namespace_rename old={} new={}",
                        old_path, new_path
                    )),
            );
            reply.error(libc::EXDEV);
            return;
        }

        // L1: defense in depth — keep the inbox name shape rule on
        // both sides of an inbox-internal rename, so `mv inbox/foo
        // inbox/.git` cannot create `source/.git` and quietly drop
        // out of the inbox listing.
        for pt in [&old_path_type, &new_path_type] {
            match pt {
                PathType::InboxSkillDir { skill_name }
                | PathType::InboxPassthrough { skill_name, .. } => {
                    if !Self::is_inbox_skill_name_allowed(skill_name) {
                        self.emit_event(
                            SkillEvent::new(SkillEventKind::Rename)
                                .with_optional_skill_name(event_skill.clone())
                                .with_optional_relative_path(event_relative.clone())
                                .with_action(SkillEventAction::Rejected)
                                .with_errno(libc::EACCES)
                                .with_caller(req.uid(), req.gid())
                                .with_detail(format!(
                                    "class=invalid_inbox_skill_name skill={} old={} new={}",
                                    skill_name, old_path, new_path
                                )),
                        );
                        reply.error(libc::EACCES);
                        return;
                    }
                }
                _ => {}
            }
        }

        // S3: reject renames that source from or target a reserved
        // lifecycle namespace. Both sides are checked before physical
        // resolution so the source remains untouched on rejection.
        if let Some(errno) = self.enforce_lifecycle_reservation(
            &old_path_type,
            SkillEventKind::Rename,
            req,
            Some(new_path.clone()),
        ) {
            reply.error(errno);
            return;
        }
        if let Some(errno) = self.enforce_lifecycle_reservation(
            &new_path_type,
            SkillEventKind::Rename,
            req,
            Some(new_path.clone()),
        ) {
            reply.error(errno);
            return;
        }

        // S1: refuse renames that move out of `.skill-meta/**` (mutates the
        // protected metadata directory) or into `.skill-meta/**` (creates a
        // new entry inside it). The from-side check fires before any
        // physical resolution so the source remains untouched.
        if let Some(errno) = self.enforce_skill_meta(
            &old_path_type,
            SkillEventKind::Rename,
            req,
            Some(new_path.clone()),
        ) {
            reply.error(errno);
            return;
        }
        if let Some(errno) = self.enforce_skill_meta(
            &new_path_type,
            SkillEventKind::Rename,
            req,
            Some(new_path.clone()),
        ) {
            reply.error(errno);
            return;
        }

        // I4/H3: reject renames on hidden skills unless both sides
        // match the post-publish grace whitelist.
        //
        // I2 carve-out: a staging-root → skill-dir rename is the
        // install-completion mechanism. The destination name is
        // usually still ledger-hidden at rename time (the resolver has
        // not seen the new skill yet), so the hidden gate must not
        // fire on either side of that rename — the post-publish grace
        // session started right after the rename is what gates
        // follow-up mutations. (The staging validation block below
        // re-derives the same shape for its own checks.)
        let staging_install_rename = matches!(
            (&old_path_type, &new_path_type),
            (
                PathType::SkillDir { skill_name: old_name },
                PathType::SkillDir { .. },
            ) if self.is_staging_skill_root(old_name)
        ) || matches!(
            (&old_path_type, &new_path_type),
            (
                PathType::NestedSkillDir { skill_name: old_skill, .. },
                PathType::NestedSkillDir { .. },
            ) if self.is_staging_skill_root(old_skill)
        );
        // Hidden *sources* are always rejected, and so are hidden
        // *targets that already exist on disk*. A target side that
        // does not physically exist is the whole-skill rename /
        // store-sync flow: the new name has no resolver entry yet,
        // which `should_reject_hidden_write` would misread as
        // `Hidden` (a missing entry resolves hidden), so the free
        // target is only rejected when a physical skill directory is
        // actually being renamed onto.
        for (is_target_side, pt) in [(false, &old_path_type), (true, &new_path_type)] {
            let reject = match pt {
                PathType::Passthrough {
                    skill_name,
                    relative_path,
                } => self.should_reject_hidden_write(skill_name, Some(relative_path)),
                PathType::SkillMd { skill_name } => {
                    self.should_reject_hidden_write(skill_name, Some(Path::new("SKILL.md")))
                }
                PathType::SkillDir { skill_name } => {
                    !staging_install_rename
                        && self.should_reject_hidden_write(skill_name, None)
                        && (!is_target_side
                            || std::fs::symlink_metadata(self.skill_physical_dir(skill_name))
                                .is_ok())
                }
                PathType::NestedPassthrough {
                    category,
                    skill_name,
                    relative_path,
                } => self.should_reject_hermes_nested_hidden_write(
                    category,
                    skill_name,
                    Some(relative_path),
                ),
                PathType::NestedSkillMd {
                    category,
                    skill_name,
                } => self.should_reject_hermes_nested_hidden_write(
                    category,
                    skill_name,
                    Some(Path::new("SKILL.md")),
                ),
                PathType::NestedSkillDir {
                    category,
                    skill_name,
                } => {
                    !staging_install_rename
                        && self.should_reject_hermes_nested_hidden_write(category, skill_name, None)
                }
                _ => false,
            };
            if reject {
                self.emit_op_event_with_detail(
                    req,
                    pt,
                    SkillEventKind::Rename,
                    SkillEventAction::Rejected,
                    Some(libc::ENOENT),
                    None,
                    Some("class=hidden_skill".to_string()),
                );
                reply.error(libc::ENOENT);
                return;
            }
        }

        // I2/H3: staging-to-skill rename validation. When a staging root
        // is renamed to a skill directory, validate the target name
        // against sensitive namespaces and invalid skill name shapes.
        // Flat layout: SkillDir → SkillDir.
        // Hermes layout: NestedSkillDir → NestedSkillDir (same category),
        // and top-level staging roots classified by NAME (see the H3 arm
        // below for why the type pairs alone cannot enumerate them).
        let is_staging_rename = if let Some(ref matcher) = self.staging_matcher {
            match (&old_path_type, &new_path_type) {
                (
                    PathType::SkillDir {
                        skill_name: old_name,
                    },
                    PathType::SkillDir {
                        skill_name: new_name,
                    },
                ) if matcher.is_staging_root(old_name) => {
                    if !crate::security::install::is_valid_staging_rename_target(new_name, matcher)
                    {
                        warn!(
                            old = %old_path,
                            new = %new_path,
                            "rename: rejecting staging rename to invalid target"
                        );
                        self.emit_event(
                            SkillEvent::new(SkillEventKind::Rename)
                                .with_optional_skill_name(event_skill.clone())
                                .with_optional_relative_path(event_relative.clone())
                                .with_action(SkillEventAction::Rejected)
                                .with_errno(libc::EACCES)
                                .with_caller(req.uid(), req.gid())
                                .with_detail(format!(
                                    "class=invalid_staging_rename_target old={} new={}",
                                    old_path, new_path
                                )),
                        );
                        reply.error(libc::EACCES);
                        return;
                    }
                    true
                }
                // H3: Hermes intra-category staging rename.
                (
                    PathType::NestedSkillDir {
                        category: old_cat,
                        skill_name: old_skill,
                    },
                    PathType::NestedSkillDir {
                        category: new_cat,
                        skill_name: new_skill,
                    },
                ) if old_cat == new_cat && matcher.is_staging_root(old_skill) => {
                    if !crate::security::install::is_valid_staging_rename_target(new_skill, matcher)
                    {
                        warn!(
                            old = %old_path,
                            new = %new_path,
                            "rename: rejecting Hermes staging rename to invalid target"
                        );
                        self.emit_event(
                            SkillEvent::new(SkillEventKind::Rename)
                                .with_optional_skill_name(event_skill.clone())
                                .with_optional_relative_path(event_relative.clone())
                                .with_action(SkillEventAction::Rejected)
                                .with_errno(libc::EACCES)
                                .with_caller(req.uid(), req.gid())
                                .with_detail(format!(
                                    "class=invalid_staging_rename_target old={} new={}",
                                    old_path, new_path
                                )),
                        );
                        reply.error(libc::EACCES);
                        return;
                    }
                    true
                }
                // H3: Hermes top-level staging rename, classified by NAME
                // rather than by type pair. While a top-level staging root
                // is being populated it parses as a CategoryDir (no
                // SKILL.md yet — an interrupted install) and as a SkillDir
                // once the manifest has been written. The rename target
                // parses as a CategoryDir (non-in-place mount, or a name
                // that already exists as a directory) or as a HermesMeta
                // (in-place mount, fresh name: the depth-1 in-place
                // rewrite classifies not-yet-existing entries as
                // top-level files). A (SkillDir, SkillDir) pair is already
                // validated by the flat arm above, so this arm observes
                // every other combination. Without it the validation — and
                // the install-completion notify below — could be bypassed
                // by choosing the mount mode or the manifest timing: a
                // top-level staging rename onto `.skill-meta` silently
                // succeeded, and a completed install never notified.
                (
                    PathType::SkillDir {
                        skill_name: old_name,
                    }
                    | PathType::CategoryDir { category: old_name },
                    PathType::SkillDir {
                        skill_name: new_name,
                    }
                    | PathType::CategoryDir { category: new_name }
                    | PathType::HermesMeta { name: new_name },
                ) if self.skill_layout == crate::path::SkillLayout::Hermes
                    && matcher.is_staging_root(old_name) =>
                {
                    if !crate::security::install::is_valid_staging_rename_target(new_name, matcher)
                    {
                        warn!(
                            old = %old_path,
                            new = %new_path,
                            "rename: rejecting Hermes top-level staging rename to invalid target"
                        );
                        self.emit_event(
                            SkillEvent::new(SkillEventKind::Rename)
                                .with_optional_skill_name(event_skill.clone())
                                .with_optional_relative_path(event_relative.clone())
                                .with_action(SkillEventAction::Rejected)
                                .with_errno(libc::EACCES)
                                .with_caller(req.uid(), req.gid())
                                .with_detail(format!(
                                    "class=invalid_staging_rename_target old={} new={}",
                                    old_path, new_path
                                )),
                        );
                        reply.error(libc::EACCES);
                        return;
                    }
                    true
                }
                _ => false,
            }
        } else {
            false
        };

        let old_physical = match self.resolve_physical_path(&old_path) {
            Some(p) => p,
            None => {
                self.ro_warn("rename", &old_path);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Rename)
                        .with_optional_skill_name(event_skill.clone())
                        .with_optional_relative_path(event_relative.clone())
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::EROFS)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(new_path.clone()),
                );
                reply.error(libc::EROFS);
                return;
            }
        };
        let resolved_new_physical = match self.resolve_physical_path(&new_path) {
            Some(p) => p,
            None => {
                self.ro_warn("rename", &new_path);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Rename)
                        .with_optional_skill_name(event_skill.clone())
                        .with_optional_relative_path(event_relative.clone())
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::EROFS)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(new_path.clone()),
                );
                reply.error(libc::EROFS);
                return;
            }
        };
        let new_physical = match (&old_path_type, &new_path_type) {
            (
                PathType::SkillDir { .. },
                PathType::SkillDir {
                    skill_name: new_name,
                },
            ) if self.skill_source_path(new_name).is_none()
                && matches!(
                    std::fs::symlink_metadata(&resolved_new_physical),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                ) =>
            {
                old_physical
                    .parent()
                    .map(|parent| parent.join(new_name))
                    .unwrap_or(resolved_new_physical)
            }
            _ => resolved_new_physical,
        };

        debug!(
            old = %old_path, new = %new_path,
            ?old_physical, ?new_physical,
            no_replace,
            "rename"
        );

        let rename_result = if no_replace {
            rename_noreplace(&old_physical, &new_physical)
        } else {
            std::fs::rename(&old_physical, &new_physical)
        };
        let rename_result = match rename_result {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                // Long-path fallback: rename via two parent dir fds + leafs.
                // Both sides may exceed PATH_MAX on the absolute physical
                // path even when each parent dir individually fits.
                let old_parent = old_physical
                    .parent()
                    .and_then(|parent| open_dir_path(parent).ok())
                    .zip(old_physical.file_name().map(|leaf| leaf.to_os_string()));
                let new_parent = new_physical
                    .parent()
                    .and_then(|parent| open_dir_path(parent).ok())
                    .zip(new_physical.file_name().map(|leaf| leaf.to_os_string()));
                match (old_parent, new_parent) {
                    (Some((old_fd, old_leaf)), Some((new_fd, new_leaf))) => {
                        let flags: u32 = if no_replace {
                            SUPPORTED_RENAME_FLAGS
                        } else {
                            0
                        };
                        renameat2_leaf(&old_fd, &old_leaf, &new_fd, &new_leaf, flags)
                    }
                    _ => Err(e),
                }
            }
            Err(e) => Err(e),
        };

        match rename_result {
            Ok(()) => {
                // Update inode mappings.
                self.inodes.rename_path(&old_path, &new_path);

                // Store sync for skill-level renames. SkillDir is a flat
                // `/skills/<name>` rename; InboxSkillDir is an inbox-internal
                // rename of the physical `source/<name>` candidate (mkdir /
                // rmdir already sync inbox entries, so rename must too);
                // NestedSkillDir is the Hermes-layout equivalent, whose store
                // key is likewise the directory leaf name. Cross-namespace
                // (inbox <-> /skills) renames were rejected with EXDEV above,
                // so old and new are both in or both out of the inbox here.
                let old_type = old_path_type.clone();
                let new_type = new_path_type.clone();
                match (&old_type, &new_type) {
                    (
                        PathType::SkillDir {
                            skill_name: old_name,
                        }
                        | PathType::InboxSkillDir {
                            skill_name: old_name,
                        }
                        | PathType::NestedSkillDir {
                            skill_name: old_name,
                            ..
                        },
                        PathType::SkillDir {
                            skill_name: new_name,
                        }
                        | PathType::InboxSkillDir {
                            skill_name: new_name,
                        }
                        | PathType::NestedSkillDir {
                            skill_name: new_name,
                            ..
                        },
                    ) => {
                        // Identity migration needs proof that the source
                        // directory actually backs the managed skill keyed
                        // by `old_name`. Lexical path classification is not
                        // identity: path.rs deliberately classifies a plain
                        // category child with no SKILL.md (e.g. `apple/docs`)
                        // as NestedSkillDir for traversal, and the store keys
                        // skills by bare leaf name, so a different category
                        // may already own a real skill with the same leaf —
                        // and the inbox chain is a single component, so even
                        // a plain `source/docs` candidate rename could
                        // collide with a real `beta/docs` skill. An ungated
                        // remove+upsert would delete that unrelated entry
                        // and/or fabricate a placeholder for a plain
                        // directory. A flat `/skills` slot resolves through
                        // the store entry itself and accepts any depth
                        // below the source root (categorized source behind
                        // a flat mount); inbox and Hermes nested renames
                        // must match the entry's FULL relative chain below
                        // the source root (category component included for
                        // nested skills).
                        let exact_chain: Vec<String> = match &old_type {
                            PathType::NestedSkillDir {
                                category,
                                skill_name,
                            } => vec![category.clone(), skill_name.clone()],
                            _ => vec![old_name.clone()],
                        };
                        let identity = match &old_type {
                            PathType::SkillDir { .. } => RenameSourceIdentity::FlatSlot,
                            _ => RenameSourceIdentity::ExactChain(&exact_chain),
                        };
                        if !self.rename_source_backs_store_entry(old_name, identity) {
                            // Plain (never-activated) directory rename: the
                            // store holds no entry originating here, so
                            // leave it untouched — same stance as the
                            // sentinel-gated inbox activation flow.
                            info!(
                                old = %old_name, new = %new_name,
                                "sync: non-skill dir rename left the store untouched"
                            );
                        } else {
                            // Reuse the shared #3999 refresh helper so the
                            // rename keeps its invalid-dirname Degraded
                            // semantics (adopt_directory_name) instead of
                            // the previously inlined remove+upsert.
                            self.update_store_after_skill_rename(old_name, new_name, &new_physical);
                            info!(
                                old = %old_name, new = %new_name,
                                "sync: skill renamed (immediate store update)"
                            );
                        }
                    }
                    _ => {
                        // File-level rename inside a skill — trigger re-parse
                        // if SKILL.md is involved.
                        if let PathType::SkillMd { skill_name } = &new_type {
                            self.send_sync(SyncEvent::Reparse {
                                skill_name: skill_name.clone(),
                                source_path: new_physical.clone(),
                            });
                        }
                        if let PathType::SkillMd { skill_name } = &old_type {
                            self.store.write().remove(skill_name);
                        }
                    }
                }

                // I2/H3: staging-to-skill rename triggers exactly one
                // rename mutation notify (non-blocking enqueue).
                // The generic old/new observe pair below is skipped for
                // staging renames.
                if is_staging_rename {
                    let notify_id = match &new_type {
                        PathType::SkillDir {
                            skill_name: new_name,
                        } => Some(new_name.clone()),
                        // H3: Hermes intra-category staging rename.
                        PathType::NestedSkillDir {
                            category,
                            skill_name,
                        } => Some(Self::hermes_skill_id(category, skill_name)),
                        // H3: Hermes top-level staging rename — the new
                        // top-level skill id is the bare name, whether the
                        // target parsed as a category, as a HermesMeta
                        // (in-place fresh name), or as an existing
                        // top-level Skill. Matches
                        // enumerate_hermes_top_level_skills and the
                        // resolver key used for mixed-layout skills.
                        PathType::CategoryDir { category } => Some(category.clone()),
                        PathType::HermesMeta { name } => Some(name.clone()),
                        _ => None,
                    };
                    if let Some(ref id) = notify_id {
                        if let Some(ref staging_ctrl) = self.staging_controller {
                            staging_ctrl.emit_staging_rename_notify(id);
                        }
                        // I4: start post-publish grace session after staging rename.
                        if let Some(ref pp_ctrl) = self.post_publish_controller {
                            pp_ctrl.start_session(
                                id,
                                crate::security::PostPublishSessionKind::StagingRename,
                            );
                        }
                    }
                }

                // D1.3-demo: rename observes both old and new owning
                // skill. If they're identical the controller's
                // per-skill debounce coalesces both relative paths; if
                // they differ the controller schedules independent
                // refreshes for each side.
                // L1: cross-namespace renames between inbox and
                // `/skills` were rejected above, so the old/new sides
                // here are either both inbox or both non-inbox. Inbox
                // renames feed `inbox_observe_install_complete` so
                // they only enqueue a refresh when the leaf is the
                // install-complete sentinel; non-inbox renames keep
                // the D1.3 per-mutation refresh.
                let old_skill_path = match &old_type {
                    PathType::SkillMd { skill_name } => {
                        Some((skill_name.clone(), Some(PathBuf::from("SKILL.md")), false))
                    }
                    PathType::Passthrough {
                        skill_name,
                        relative_path,
                    } => Some((skill_name.clone(), Some(relative_path.clone()), false)),
                    PathType::SkillDir { skill_name } => Some((skill_name.clone(), None, false)),
                    PathType::InboxPassthrough {
                        skill_name,
                        relative_path,
                    } => Some((skill_name.clone(), Some(relative_path.clone()), true)),
                    PathType::InboxSkillDir { skill_name } => {
                        Some((skill_name.clone(), None, true))
                    }
                    // H3: Hermes nested paths.
                    PathType::NestedSkillMd {
                        category,
                        skill_name,
                    } => Some((
                        Self::hermes_skill_id(category, skill_name),
                        Some(PathBuf::from("SKILL.md")),
                        false,
                    )),
                    PathType::NestedPassthrough {
                        category,
                        skill_name,
                        relative_path,
                    } => Some((
                        Self::hermes_skill_id(category, skill_name),
                        Some(relative_path.clone()),
                        false,
                    )),
                    PathType::NestedSkillDir {
                        category,
                        skill_name,
                    } => Some((Self::hermes_skill_id(category, skill_name), None, false)),
                    _ => None,
                };
                let new_skill_path = match &new_type {
                    PathType::SkillMd { skill_name } => {
                        Some((skill_name.clone(), Some(PathBuf::from("SKILL.md")), false))
                    }
                    PathType::Passthrough {
                        skill_name,
                        relative_path,
                    } => Some((skill_name.clone(), Some(relative_path.clone()), false)),
                    PathType::SkillDir { skill_name } => Some((skill_name.clone(), None, false)),
                    PathType::InboxPassthrough {
                        skill_name,
                        relative_path,
                    } => Some((skill_name.clone(), Some(relative_path.clone()), true)),
                    PathType::InboxSkillDir { skill_name } => {
                        Some((skill_name.clone(), None, true))
                    }
                    PathType::NestedSkillMd {
                        category,
                        skill_name,
                    } => Some((
                        Self::hermes_skill_id(category, skill_name),
                        Some(PathBuf::from("SKILL.md")),
                        false,
                    )),
                    PathType::NestedPassthrough {
                        category,
                        skill_name,
                        relative_path,
                    } => Some((
                        Self::hermes_skill_id(category, skill_name),
                        Some(relative_path.clone()),
                        false,
                    )),
                    PathType::NestedSkillDir {
                        category,
                        skill_name,
                    } => Some((Self::hermes_skill_id(category, skill_name), None, false)),
                    _ => None,
                };
                let observe_pair = |fs: &Self, skill: &str, rel: Option<&Path>, is_inbox: bool| {
                    if is_inbox {
                        if let Some(rel_path) = rel {
                            fs.inbox_observe_install_complete(
                                skill,
                                rel_path,
                                MutationKind::Rename,
                            );
                        }
                        // Inbox-skill-dir renames have no relative
                        // path; they intentionally do not enqueue a
                        // refresh on their own (the install-complete
                        // sentinel remains the trigger).
                    } else {
                        fs.observe_mutation(skill, rel, MutationKind::Rename);
                    }
                };
                // I2: staging renames already emitted exactly one
                // install-complete above; skip the generic pair.
                if !is_staging_rename {
                    if let Some((skill, rel, is_inbox)) = &old_skill_path {
                        observe_pair(self, skill, rel.as_deref(), *is_inbox);
                    }
                    if let Some((new_skill, new_rel, is_inbox)) = &new_skill_path {
                        observe_pair(self, new_skill, new_rel.as_deref(), *is_inbox);
                    }
                }
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Rename)
                        .with_optional_skill_name(event_skill)
                        .with_optional_relative_path(event_relative)
                        .with_action(SkillEventAction::Allowed)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(new_path.clone()),
                );
                reply.ok();
            }
            Err(e) => {
                warn!(
                    op = "rename", old = %old_path, new = %new_path,
                    error = %e, "rename failed"
                );
                let err = errno(&e);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Rename)
                        .with_optional_skill_name(event_skill)
                        .with_optional_relative_path(event_relative)
                        .with_action(SkillEventAction::Failed)
                        .with_errno(err)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(new_path.clone()),
                );
                reply.error(err);
            }
        }
    }

    /// Synchronously refresh the store after a skill-directory rename.
    ///
    /// Drops the old store key and re-parses the `SKILL.md` under its new
    /// path; the *directory* name becomes the store key regardless of what
    /// the frontmatter says (the user may not have updated the `name:`
    /// field yet). Adoption goes through the shared
    /// [`skillfs_core::store::adopt_directory_name`], so renaming a valid
    /// skill to a non-conforming directory (e.g. `foo_bar`) degrades the
    /// entry exactly like the initial scan would, instead of inserting a
    /// cleanly parsed entry under an illegal name.
    fn update_store_after_skill_rename(&self, old_name: &str, new_name: &str, new_physical: &Path) {
        self.store.write().remove(old_name);
        let md_path = new_physical.join("SKILL.md");
        let mut new_entry = match parser::parse_skill_file(&md_path) {
            Ok(entry) => entry,
            Err(_) => {
                // SKILL.md not readable yet — insert a placeholder so the
                // directory appears in readdir immediately.
                use skillfs_core::{ParseStatus, SkillEntry, SkillMetadata};
                SkillEntry {
                    metadata: SkillMetadata::default(),
                    parameters: vec![],
                    returns: vec![],
                    body: String::new(),
                    parse_status: ParseStatus::Degraded(
                        "renamed, awaiting SKILL.md update".to_string(),
                    ),
                    source_path: md_path,
                    last_modified: std::time::SystemTime::now(),
                }
            }
        };
        adopt_directory_name(&mut new_entry, new_name);
        self.store.write().upsert(new_entry);
    }

    /// Whether the store entry keyed by `skill_name` demonstrably
    /// originates from the source directory addressed by `identity`.
    ///
    /// For [`RenameSourceIdentity::ExactChain`] the entry's recorded
    /// source must be the `SKILL.md` living under exactly that chain
    /// below the source root — the FULL relative path, never a
    /// trailing-segment match. The store keys skills by bare leaf
    /// name, so the entry held under the old name may belong to a
    /// different directory that merely shares the leaf: a real
    /// `beta/docs` skill owns the key "docs" while a plain `source/docs`
    /// inbox candidate (no SKILL.md, one-component chain) is being
    /// renamed — `beta/docs` does not strip to `docs`, so that pair is
    /// rejected instead of the rename deleting the real skill's entry
    /// and fabricating a placeholder. Hermes nested renames likewise
    /// must match the whole category chain.
    ///
    /// For [`RenameSourceIdentity::FlatSlot`] any origin below the
    /// source root is accepted, because the flat slot resolves through
    /// the store entry itself (see the enum).
    ///
    /// In either mode an entry whose origin does not strip below this
    /// source root at all (a different root entirely) never backs the
    /// renamed directory. Whole-path equality is unusable verbatim —
    /// in-place mounts address the source through `/proc/self/fd/<n>`
    /// while an initial scan records the real source prefix (and the
    /// mkdir placeholder / the rename re-parse record the fd alias) —
    /// so both spellings of the source root are accepted when
    /// stripping, the same alias pair `snapshot_read_dir` reconciles.
    fn rename_source_backs_store_entry(
        &self,
        skill_name: &str,
        identity: RenameSourceIdentity<'_>,
    ) -> bool {
        let store = self.store.read();
        let Some(entry) = store.get(skill_name) else {
            return false;
        };
        if entry.source_path.file_name() != Some(std::ffi::OsStr::new("SKILL.md")) {
            return false;
        }
        let Some(origin) = entry.source_path.parent() else {
            return false;
        };
        // Accept both spellings of the source root: the real source
        // path (initial scan) and the in-place `/proc/self/fd/<n>`
        // alias (mkdir placeholder / post-mount re-parse).
        let mut source_roots = vec![self.source.clone()];
        if let Some(fd) = &self.source_dirfd {
            use std::os::unix::io::AsRawFd;
            source_roots.push(PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd())));
        }
        source_roots.iter().any(|root| {
            origin
                .strip_prefix(root)
                .map(|relative| match identity {
                    RenameSourceIdentity::FlatSlot => !relative.as_os_str().is_empty(),
                    RenameSourceIdentity::ExactChain(chain) => {
                        !chain.is_empty() && relative == chain.iter().collect::<PathBuf>()
                    }
                })
                .unwrap_or(false)
        })
    }
}

/// `EROFS` gate for the always-read-only `skill-discover` virtual
/// namespace on the namespace-mutation callbacks, mirroring the guard
/// `write.rs` already applies to `write`/`create`/`setattr` and
/// `link.rs` to `symlink`/`link`. `resolve_physical_path` maps every
/// skill-discover FUSE path onto `source/skill-discover/...`, so an
/// unguarded mkdir/unlink/rmdir/rename would mutate that physical tree
/// through the read-only virtual view.
fn enforce_skill_discover_readonly(path_type: &PathType) -> Option<i32> {
    match path_type {
        PathType::SkillMd { skill_name }
        | PathType::SkillDir { skill_name }
        | PathType::Passthrough { skill_name, .. }
            if is_skill_discover_path(skill_name) =>
        {
            Some(libc::EROFS)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, ParseStatus, store::SkillStore};

    use super::*;

    fn write_skill(dir: &Path, frontmatter_name: &str) {
        std::fs::create_dir_all(dir).expect("skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {frontmatter_name}\ndescription: demo\n---\nbody\n"),
        )
        .expect("SKILL.md");
    }

    #[test]
    fn skill_dir_rename_to_invalid_name_degrades_entry() {
        let source = tempfile::tempdir().expect("source tempdir");
        write_skill(&source.path().join("good-skill"), "good-skill");
        // The post-rename tree: the same valid frontmatter now sits under
        // a directory whose name violates the grammar.
        write_skill(&source.path().join("foo_bar"), "good-skill");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        assert!(
            store
                .get("good-skill")
                .expect("loaded before rename")
                .parse_status
                .is_ok()
        );

        let shared = Arc::new(RwLock::new(store));
        let fs = SkillFs::new(
            source.path().join("mount"),
            source.path().to_path_buf(),
            shared.clone(),
            false,
        );

        fs.update_store_after_skill_rename("good-skill", "foo_bar", &source.path().join("foo_bar"));

        let guard = shared.read();
        assert!(guard.get("good-skill").is_none(), "old store key removed");
        let entry = guard.get("foo_bar").expect("renamed store entry");
        assert_eq!(entry.metadata.name, "foo_bar");
        assert!(
            matches!(&entry.parse_status, ParseStatus::Degraded(msg)
                if msg.contains("foo_bar") && msg.contains("kebab")),
            "rename to a non-conforming directory must degrade the entry, got {:?}",
            entry.parse_status
        );
    }

    #[test]
    fn skill_dir_rename_without_skill_md_keeps_degradation() {
        let source = tempfile::tempdir().expect("source tempdir");
        write_skill(&source.path().join("good-skill"), "good-skill");
        // SKILL.md not yet present under the renamed directory: the
        // placeholder must still adopt the directory identity and carry
        // the directory-name degradation alongside its own reason.
        std::fs::create_dir_all(source.path().join("foo_bar")).expect("renamed dir");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());

        let shared = Arc::new(RwLock::new(store));
        let fs = SkillFs::new(
            source.path().join("mount"),
            source.path().to_path_buf(),
            shared.clone(),
            false,
        );

        fs.update_store_after_skill_rename("good-skill", "foo_bar", &source.path().join("foo_bar"));

        let guard = shared.read();
        let entry = guard.get("foo_bar").expect("placeholder store entry");
        assert_eq!(entry.metadata.name, "foo_bar");
        assert!(
            matches!(&entry.parse_status, ParseStatus::Degraded(msg)
                if msg.contains("awaiting SKILL.md") && msg.contains("foo_bar")),
            "placeholder must merge the directory-name issue, got {:?}",
            entry.parse_status
        );
    }
}
