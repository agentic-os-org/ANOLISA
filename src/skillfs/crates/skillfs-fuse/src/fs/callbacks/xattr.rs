//! FUSE extended-attribute callbacks: `getxattr`, `listxattr`, `setxattr`, `removexattr`.

use std::path::Path;

use fuser::{ReplyEmpty, ReplyXattr, Request};

use super::super::SkillFs;
use crate::path::PathType;
use crate::security::{SkillEventAction, SkillEventKind};
use crate::xattr::{
    XattrNamespace, filter_user_xattr_list, path_type_supports_xattr_passthrough, xattr_lget,
    xattr_llist, xattr_lremove, xattr_lset, xattr_namespace,
};

impl SkillFs {
    pub(in crate::fs) fn getxattr_impl(
        &mut self,
        req: &Request,
        ino: u64,
        name: &std::ffi::OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => return reply.error(libc::ENOENT),
        };
        let path_type = self.parse_fuse_path(Path::new(&path));

        if !path_type_supports_xattr_passthrough(&path_type) {
            return reply.error(libc::EOPNOTSUPP);
        }
        if Self::lifecycle_reservation(&path_type).is_some() {
            return reply.error(libc::EOPNOTSUPP);
        }
        if matches!(xattr_namespace(name), XattrNamespace::Disallowed) {
            return reply.error(libc::EOPNOTSUPP);
        }

        // xattr *reads* observe the same directory as every other read: the
        // D1.1 resolver's snapshot for a fallback skill, the live source for
        // a current skill, staging/pending/grace candidates from source, and
        // nothing for a hidden skill. The trusted `.skill-meta` management
        // view is the exception (live source, matching lookup/getattr/open),
        // and mutations below stay live-source by design.
        let physical = match &path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => {
                let pt = PathType::Passthrough {
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&pt, req) {
                    // Untrusted callers keep the metadata isolation.
                    Some(false) => return reply.error(libc::ENOENT),
                    // Trusted callers read `.skill-meta` from the live
                    // physical source even when the regular view is a
                    // fallback snapshot or hidden.
                    Some(true) => self.skill_physical_dir(skill_name).join(relative_path),
                    None => match self.flat_access_read_path(skill_name, Some(relative_path)) {
                        Some(p) => p,
                        None => return reply.error(libc::ENOENT),
                    },
                }
            }
            // Hermes nested counterpart: the trusted `.skill-meta` view is
            // decided *before* the activation mapping, so a trusted caller
            // keeps reading management metadata from the live nested source
            // even while the skill is hidden — the same order readlink,
            // lookup and open apply. Ordinary content follows the nested
            // read directory, which yields nothing for a hidden skill.
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } => {
                let npt = PathType::NestedPassthrough {
                    category: category.clone(),
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&npt, req) {
                    Some(false) => return reply.error(libc::ENOENT),
                    Some(true) => self
                        .hermes_skill_physical_dir(category, skill_name)
                        .join(relative_path),
                    None => match self.nested_access_read_path(
                        category,
                        skill_name,
                        Some(relative_path),
                    ) {
                        Some(p) => p,
                        None => return reply.error(libc::ENOENT),
                    },
                }
            }
            _ => return reply.error(libc::EOPNOTSUPP),
        };

        let res = xattr_lget(&physical, name, size as usize);
        match res {
            Ok(buf) => {
                if size == 0 {
                    reply.size(buf.len() as u32);
                } else {
                    reply.data(&buf);
                }
            }
            Err(err) => {
                let _ = req;
                reply.error(err);
            }
        }
    }
    pub(in crate::fs) fn listxattr_impl(
        &mut self,
        req: &Request,
        ino: u64,
        size: u32,
        reply: ReplyXattr,
    ) {
        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => return reply.error(libc::ENOENT),
        };
        let path_type = self.parse_fuse_path(Path::new(&path));

        if !path_type_supports_xattr_passthrough(&path_type) {
            return reply.error(libc::EOPNOTSUPP);
        }
        if Self::lifecycle_reservation(&path_type).is_some() {
            return reply.error(libc::EOPNOTSUPP);
        }

        // Same resolver-aware directory as `getxattr`: a fallback skill's
        // xattr list must describe the snapshot the bytes come from, with
        // the same trusted `.skill-meta` live-source exception.
        let physical = match &path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => {
                let pt = PathType::Passthrough {
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&pt, req) {
                    Some(false) => return reply.error(libc::ENOENT),
                    Some(true) => self.skill_physical_dir(skill_name).join(relative_path),
                    None => match self.flat_access_read_path(skill_name, Some(relative_path)) {
                        Some(p) => p,
                        None => return reply.error(libc::ENOENT),
                    },
                }
            }
            // Nested counterpart of the arm above, trusted view first.
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } => {
                let npt = PathType::NestedPassthrough {
                    category: category.clone(),
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&npt, req) {
                    Some(false) => return reply.error(libc::ENOENT),
                    Some(true) => self
                        .hermes_skill_physical_dir(category, skill_name)
                        .join(relative_path),
                    None => match self.nested_access_read_path(
                        category,
                        skill_name,
                        Some(relative_path),
                    ) {
                        Some(p) => p,
                        None => return reply.error(libc::ENOENT),
                    },
                }
            }
            _ => return reply.error(libc::EOPNOTSUPP),
        };

        // Always fetch the full physical list first so we can filter to the
        // `user.*` namespace before honoring the caller-supplied `size`. The
        // filter is conservative — T3 only exposes `user.*`, so listing
        // anything else would contradict the get/set/remove namespace gate.
        let full = match xattr_llist(&physical) {
            Ok(v) => v,
            Err(err) => return reply.error(err),
        };
        let filtered = filter_user_xattr_list(&full);

        if size == 0 {
            reply.size(filtered.len() as u32);
        } else if (filtered.len() as u32) > size {
            reply.error(libc::ERANGE);
        } else {
            reply.data(&filtered);
        }
    }
    pub(in crate::fs) fn setxattr_impl(
        &mut self,
        req: &Request,
        ino: u64,
        name: &std::ffi::OsStr,
        value: &[u8],
        flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => return reply.error(libc::ENOENT),
        };
        let path_type = self.parse_fuse_path(Path::new(&path));

        if !path_type_supports_xattr_passthrough(&path_type) {
            self.emit_xattr_event(
                req,
                &path_type,
                "set",
                name,
                SkillEventAction::Rejected,
                Some(libc::EOPNOTSUPP),
                Some("virtual_xattr_path"),
            );
            return reply.error(libc::EOPNOTSUPP);
        }
        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Metadata, req, None)
        {
            return reply.error(errno);
        }
        if let Some(errno) =
            self.enforce_skill_meta(&path_type, SkillEventKind::Metadata, req, None)
        {
            return reply.error(errno);
        }
        if matches!(xattr_namespace(name), XattrNamespace::Disallowed) {
            self.emit_xattr_event(
                req,
                &path_type,
                "set",
                name,
                SkillEventAction::Rejected,
                Some(libc::EOPNOTSUPP),
                Some("unsupported_xattr_namespace"),
            );
            return reply.error(libc::EOPNOTSUPP);
        }
        if self.reject_hidden_xattr_write(&path_type) {
            self.emit_xattr_event(
                req,
                &path_type,
                "set",
                name,
                SkillEventAction::Rejected,
                Some(libc::ENOENT),
                Some("hidden_skill"),
            );
            return reply.error(libc::ENOENT);
        }

        let physical = match self.resolve_physical_path(&path) {
            Some(p) => p,
            None => {
                self.emit_xattr_event(
                    req,
                    &path_type,
                    "set",
                    name,
                    SkillEventAction::Rejected,
                    Some(libc::EOPNOTSUPP),
                    Some("unresolved_physical_path"),
                );
                return reply.error(libc::EOPNOTSUPP);
            }
        };

        match xattr_lset(&physical, name, value, flags) {
            Ok(()) => {
                self.emit_xattr_event(
                    req,
                    &path_type,
                    "set",
                    name,
                    SkillEventAction::Allowed,
                    None,
                    None,
                );
                reply.ok();
            }
            Err(err) => {
                self.emit_xattr_event(
                    req,
                    &path_type,
                    "set",
                    name,
                    SkillEventAction::Failed,
                    Some(err),
                    None,
                );
                reply.error(err);
            }
        }
    }
    pub(in crate::fs) fn removexattr_impl(
        &mut self,
        req: &Request,
        ino: u64,
        name: &std::ffi::OsStr,
        reply: ReplyEmpty,
    ) {
        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => return reply.error(libc::ENOENT),
        };
        let path_type = self.parse_fuse_path(Path::new(&path));

        if !path_type_supports_xattr_passthrough(&path_type) {
            self.emit_xattr_event(
                req,
                &path_type,
                "remove",
                name,
                SkillEventAction::Rejected,
                Some(libc::EOPNOTSUPP),
                Some("virtual_xattr_path"),
            );
            return reply.error(libc::EOPNOTSUPP);
        }
        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Metadata, req, None)
        {
            return reply.error(errno);
        }
        if let Some(errno) =
            self.enforce_skill_meta(&path_type, SkillEventKind::Metadata, req, None)
        {
            return reply.error(errno);
        }
        if matches!(xattr_namespace(name), XattrNamespace::Disallowed) {
            self.emit_xattr_event(
                req,
                &path_type,
                "remove",
                name,
                SkillEventAction::Rejected,
                Some(libc::EOPNOTSUPP),
                Some("unsupported_xattr_namespace"),
            );
            return reply.error(libc::EOPNOTSUPP);
        }
        if self.reject_hidden_xattr_write(&path_type) {
            self.emit_xattr_event(
                req,
                &path_type,
                "remove",
                name,
                SkillEventAction::Rejected,
                Some(libc::ENOENT),
                Some("hidden_skill"),
            );
            return reply.error(libc::ENOENT);
        }

        let physical = match self.resolve_physical_path(&path) {
            Some(p) => p,
            None => {
                self.emit_xattr_event(
                    req,
                    &path_type,
                    "remove",
                    name,
                    SkillEventAction::Rejected,
                    Some(libc::EOPNOTSUPP),
                    Some("unresolved_physical_path"),
                );
                return reply.error(libc::EOPNOTSUPP);
            }
        };

        match xattr_lremove(&physical, name) {
            Ok(()) => {
                self.emit_xattr_event(
                    req,
                    &path_type,
                    "remove",
                    name,
                    SkillEventAction::Allowed,
                    None,
                    None,
                );
                reply.ok();
            }
            Err(err) => {
                self.emit_xattr_event(
                    req,
                    &path_type,
                    "remove",
                    name,
                    SkillEventAction::Failed,
                    Some(err),
                    None,
                );
                reply.error(err);
            }
        }
    }

    /// I4: hidden-skill write gate for the xattr mutators.
    ///
    /// `write`/`create`/`rename`/`unlink`/`mkdir`/`setattr` all refuse to
    /// mutate a ledger-hidden skill's live source; the xattr mutators must
    /// refuse too, or an fd opened while the skill resolved `current` keeps
    /// a channel to mutate the hidden source through a stale inode (the
    /// kernel dispatches `fsetxattr`/`fremovexattr` directly on the fd,
    /// without a fresh lookup that hiding would fail). Reuses the shared
    /// gate so the staging/pending/post-publish-grace bypasses keep working.
    ///
    /// The Hermes nested twin mirrors `write.rs`/`mutate.rs`. It is
    /// unreachable today — T3 restricts xattr passthrough to flat
    /// `Passthrough` leaves — but keeps the gate whole if passthrough is
    /// ever extended to nested leaves.
    fn reject_hidden_xattr_write(&self, path_type: &PathType) -> bool {
        match path_type {
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
        }
    }
}
