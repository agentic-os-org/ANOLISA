//! FUSE extended-attribute callbacks: `getxattr`, `listxattr`, `setxattr`, `removexattr`.

use std::path::Path;

use fuser::{ReplyEmpty, ReplyXattr, Request};

use super::super::SkillFs;
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

        let physical = match self.resolve_physical_path(&path) {
            Some(p) => p,
            None => return reply.error(libc::EOPNOTSUPP),
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
        let _ = req;
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

        let physical = match self.resolve_physical_path(&path) {
            Some(p) => p,
            None => return reply.error(libc::EOPNOTSUPP),
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
}
