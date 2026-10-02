//! FUSE link callbacks: `readlink`, `symlink`, `link`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fuser::{FileType, ReplyData, ReplyEntry, Request};
use tracing::{debug, warn};

use super::super::SkillFs;
use crate::attr::file_attr_from_metadata;
use crate::path::{PathType, is_skill_discover_path};
use crate::security::{
    self, SkillEvent, SkillEventAction, SkillEventKind, lifecycle::is_reserved_lifecycle_name,
};
use crate::symlink_policy;
use crate::sys::errno;

impl SkillFs {
    pub(in crate::fs) fn readlink_impl(&mut self, req: &Request, ino: u64, reply: ReplyData) {
        debug!(ino, "readlink");
        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => return reply.error(libc::ENOENT),
        };

        match self.parse_fuse_path(Path::new(&path)) {
            // Virtual directories are never symlinks; readlink on a
            // non-symlink returns EINVAL on Linux.
            PathType::Root
            | PathType::SkillsDir
            | PathType::SkillDir { .. }
            | PathType::InboxDir
            | PathType::InboxSkillDir { .. } => {
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Readlink)
                        .with_action(SkillEventAction::Failed)
                        .with_errno(libc::EINVAL)
                        .with_caller(req.uid(), req.gid()),
                );
                reply.error(libc::EINVAL);
            }
            // Compiled SKILL.md is a virtual regular file, not a symlink.
            PathType::SkillMd { skill_name } => {
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Readlink)
                        .with_skill_name(skill_name)
                        .with_relative_path("SKILL.md")
                        .with_action(SkillEventAction::Failed)
                        .with_errno(libc::EINVAL)
                        .with_caller(req.uid(), req.gid()),
                );
                reply.error(libc::EINVAL);
            }
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => {
                if is_skill_discover_path(&skill_name) {
                    // skill-discover virtual namespace contains no symlinks.
                    self.emit_event(
                        SkillEvent::new(SkillEventKind::Readlink)
                            .with_skill_name(&skill_name)
                            .with_relative_path(&relative_path)
                            .with_action(SkillEventAction::Failed)
                            .with_errno(libc::EINVAL)
                            .with_caller(req.uid(), req.gid()),
                    );
                    reply.error(libc::EINVAL);
                    return;
                }
                let physical = self.skill_physical_dir(&skill_name).join(&relative_path);
                match std::fs::read_link(&physical) {
                    Ok(target) => {
                        use std::os::unix::ffi::OsStrExt;
                        let bytes = target.as_os_str().as_bytes();
                        self.emit_event(
                            SkillEvent::new(SkillEventKind::Readlink)
                                .with_skill_name(&skill_name)
                                .with_relative_path(&relative_path)
                                .with_action(SkillEventAction::Allowed)
                                .with_bytes(bytes.len() as u64)
                                .with_caller(req.uid(), req.gid()),
                        );
                        reply.data(bytes);
                    }
                    Err(e) => {
                        let err = errno(&e);
                        self.emit_event(
                            SkillEvent::new(SkillEventKind::Readlink)
                                .with_skill_name(&skill_name)
                                .with_relative_path(&relative_path)
                                .with_action(SkillEventAction::Failed)
                                .with_errno(err)
                                .with_caller(req.uid(), req.gid()),
                        );
                        reply.error(err);
                    }
                }
            }
            PathType::InboxPassthrough {
                skill_name,
                relative_path,
            } => {
                if !Self::is_inbox_skill_name_allowed(&skill_name) {
                    self.emit_event(
                        SkillEvent::new(SkillEventKind::Readlink)
                            .with_action(SkillEventAction::Failed)
                            .with_errno(libc::ENOENT)
                            .with_caller(req.uid(), req.gid()),
                    );
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = self.inbox_skill_dir(&skill_name).join(&relative_path);
                match std::fs::read_link(&physical) {
                    Ok(target) => {
                        use std::os::unix::ffi::OsStrExt;
                        let bytes = target.as_os_str().as_bytes();
                        self.emit_event(
                            SkillEvent::new(SkillEventKind::Readlink)
                                .with_skill_name(&skill_name)
                                .with_relative_path(&relative_path)
                                .with_action(SkillEventAction::Allowed)
                                .with_bytes(bytes.len() as u64)
                                .with_caller(req.uid(), req.gid()),
                        );
                        reply.data(bytes);
                    }
                    Err(e) => {
                        let err = errno(&e);
                        self.emit_event(
                            SkillEvent::new(SkillEventKind::Readlink)
                                .with_skill_name(&skill_name)
                                .with_relative_path(&relative_path)
                                .with_action(SkillEventAction::Failed)
                                .with_errno(err)
                                .with_caller(req.uid(), req.gid()),
                        );
                        reply.error(err);
                    }
                }
            }
            PathType::HermesMeta { .. }
            | PathType::CategoryDir { .. }
            | PathType::NestedSkillDir { .. } => {
                reply.error(libc::EINVAL);
            }
            PathType::HermesMetaChild {
                name,
                relative_path,
            }
            | PathType::CategoryPassthrough {
                name,
                relative_path,
            }
            | PathType::NestedPassthrough {
                category: name,
                skill_name: _,
                relative_path,
            } => {
                let physical = match self.resolve_physical_path(&path) {
                    Some(p) => p,
                    None => return reply.error(libc::ENOENT),
                };
                let _ = (&name, &relative_path);
                match std::fs::read_link(&physical) {
                    Ok(target) => {
                        use std::os::unix::ffi::OsStrExt;
                        reply.data(target.as_os_str().as_bytes());
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::NestedSkillMd { .. } => {
                reply.error(libc::EINVAL);
            }
            PathType::Invalid => {
                self.emit_event(
                    SkillEvent::new(SkillEventKind::Readlink)
                        .with_action(SkillEventAction::Failed)
                        .with_errno(libc::ENOENT)
                        .with_caller(req.uid(), req.gid()),
                );
                reply.error(libc::ENOENT);
            }
        }
    }
    pub(in crate::fs) fn symlink_impl(
        &mut self,
        req: &Request,
        parent: u64,
        link_name: &std::ffi::OsStr,
        target: &std::path::Path,
        reply: ReplyEntry,
    ) {
        let path_str = match self.build_fuse_path(parent, link_name) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let path_type = self.parse_fuse_path(Path::new(&path_str));
        let target_str = target.display().to_string();

        // Only Passthrough leaves under an ordinary skill may host a new
        // symlink. Virtual paths keep their existing virtual semantics,
        // which means SymlinkDir / SymlinkMd / Root / SkillsDir / Invalid
        // remain EROFS as in S0.
        let (skill_name, relative_path) = match &path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => (skill_name.clone(), relative_path.clone()),
            _ => {
                self.ro_warn("symlink", &path_str);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::SymlinkAttempt)
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::EROFS)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!("class=virtual_link target={}", target_str)),
                );
                reply.error(libc::EROFS);
                return;
            }
        };

        // skill-discover is virtual and read-only regardless of the
        // physical layout, so refuse before any classifier work.
        if is_skill_discover_path(&skill_name) {
            self.emit_event(
                SkillEvent::new(SkillEventKind::SymlinkAttempt)
                    .with_skill_name(&skill_name)
                    .with_relative_path(&relative_path)
                    .with_action(SkillEventAction::Rejected)
                    .with_errno(libc::EROFS)
                    .with_caller(req.uid(), req.gid())
                    .with_detail(format!("class=skill_discover target={}", target_str)),
            );
            reply.error(libc::EROFS);
            return;
        }

        // S3 lifecycle namespace and S1 `.skill-meta` gates apply to the
        // link path itself before any physical resolution.
        if let Some(errno) = self.enforce_lifecycle_reservation(
            &path_type,
            SkillEventKind::SymlinkAttempt,
            req,
            Some(format!("target={}", target_str)),
        ) {
            reply.error(errno);
            return;
        }
        if let Some(errno) = self.enforce_skill_meta(
            &path_type,
            SkillEventKind::SymlinkAttempt,
            req,
            Some(format!("target={}", target_str)),
        ) {
            reply.error(errno);
            return;
        }

        // T2 default policy: only **relative** same-skill symlink targets
        // are accepted. Absolute targets are rejected even when they land
        // inside the same skill — in non-in-place mounts an absolute
        // `<source>/<skill>/...` target points at the *physical* source
        // path, so following the link from userspace bypasses the FUSE
        // layer and any audit/policy enforcement attached to it. A
        // future package may relax this for `--security-mode` /
        // in-place-only mounts where the resolved path still flows
        // through SkillFS. Until then, refuse with `EACCES` so callers
        // get a deterministic, audited rejection rather than a silent
        // bypass.
        if target.is_absolute() {
            warn!(
                op = "symlink",
                link = %path_str,
                target = %target_str,
                "absolute symlink target rejected (T2 default policy)"
            );
            self.emit_event(
                SkillEvent::new(SkillEventKind::SymlinkAttempt)
                    .with_skill_name(&skill_name)
                    .with_relative_path(&relative_path)
                    .with_action(SkillEventAction::Rejected)
                    .with_errno(libc::EACCES)
                    .with_caller(req.uid(), req.gid())
                    .with_detail(format!(
                        "class=absolute_target_disallowed target={}",
                        target_str
                    )),
            );
            reply.error(libc::EACCES);
            return;
        }

        // Lexical target boundary classification (Package I helper). The
        // classifier needs an absolute source root and absolute link
        // parent in the same coordinate system; `self.source` (the real
        // source path, not the `/proc/self/fd/{n}` proxy) is what user
        // space sees when it constructs an absolute target, so we use it
        // for both. Relative targets are resolved against the parent of
        // the link path.
        let source_root = self
            .source
            .canonicalize()
            .unwrap_or_else(|_| self.source.clone());
        let link_parent_for_classifier = source_root
            .join(&skill_name)
            .join(relative_path.parent().unwrap_or(Path::new("")));
        let store_guard = self.store.read();
        let known_skill_names: Vec<&str> = store_guard.list();
        let class = symlink_policy::classify_symlink_target(
            &source_root,
            &skill_name,
            &known_skill_names,
            &link_parent_for_classifier,
            target,
        );
        drop(store_guard);

        let class_label = symlink_policy::symlink_class_label(&class);
        if !matches!(class, symlink_policy::SymlinkTargetClass::SameSkill) {
            warn!(
                op = "symlink",
                link = %path_str,
                target = %target_str,
                class = class_label,
                "symlink target boundary check rejected"
            );
            self.emit_event(
                SkillEvent::new(SkillEventKind::SymlinkAttempt)
                    .with_skill_name(&skill_name)
                    .with_relative_path(&relative_path)
                    .with_action(SkillEventAction::Rejected)
                    .with_errno(libc::EACCES)
                    .with_caller(req.uid(), req.gid())
                    .with_detail(format!("class={} target={}", class_label, target_str)),
            );
            reply.error(libc::EACCES);
            return;
        }

        // Even when the target classifies as SameSkill, refuse if the
        // lexical resolution lands inside `.skill-meta/**` or under any
        // lifecycle reserved root (`.staging`, `.certified`,
        // `.quarantine`, `.archive`). The link path itself is gated
        // earlier, but a same-skill target could still point a fresh
        // link at protected metadata or a hidden lifecycle namespace —
        // following such a link from userspace would expose the
        // protected payload to readers that only see the unprotected
        // link path.
        let link_relative_parent = relative_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(PathBuf::new);
        if let Some(target_in_skill) =
            symlink_policy::resolve_same_skill_relative(&link_relative_parent, target)
        {
            let first_component = target_in_skill.components().next().and_then(|c| match c {
                std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            });
            let lands_in_skill_meta = security::is_skill_meta_path(&target_in_skill);
            let lands_in_lifecycle = first_component
                .as_deref()
                .map(is_reserved_lifecycle_name)
                .unwrap_or(false);
            if lands_in_skill_meta || lands_in_lifecycle {
                let sensitive_label = if lands_in_skill_meta {
                    "same_skill_sensitive_target_skill_meta"
                } else {
                    "same_skill_sensitive_target_lifecycle"
                };
                warn!(
                    op = "symlink",
                    link = %path_str,
                    target = %target_str,
                    resolved = %target_in_skill.display(),
                    class = sensitive_label,
                    "same-skill symlink target lands in protected namespace; rejected"
                );
                self.emit_event(
                    SkillEvent::new(SkillEventKind::SymlinkAttempt)
                        .with_skill_name(&skill_name)
                        .with_relative_path(&relative_path)
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::EACCES)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!(
                            "class={} target={} resolved={}",
                            sensitive_label,
                            target_str,
                            target_in_skill.display()
                        )),
                );
                reply.error(libc::EACCES);
                return;
            }
        }

        let physical = match self.resolve_physical_path(&path_str) {
            Some(p) => p,
            None => {
                reply.error(libc::EROFS);
                return;
            }
        };

        match std::os::unix::fs::symlink(target, &physical) {
            Ok(()) => {
                let ino = self.inodes.allocate(&path_str, FileType::Symlink, parent);
                self.inodes.remember(ino);
                let attr = match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut a = file_attr_from_metadata(&meta);
                        a.ino = ino;
                        a
                    }
                    Err(_) => {
                        let mut a = self.virtual_file_attr(0);
                        a.kind = FileType::Symlink;
                        a.ino = ino;
                        a
                    }
                };
                self.observe_mutation(
                    &skill_name,
                    Some(&relative_path),
                    security::MutationKind::Create,
                );
                self.emit_event(
                    SkillEvent::new(SkillEventKind::SymlinkAttempt)
                        .with_skill_name(&skill_name)
                        .with_relative_path(&relative_path)
                        .with_action(SkillEventAction::Allowed)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!("class={} target={}", class_label, target_str)),
                );
                reply.entry(&Duration::from_secs(1), &attr, 0);
            }
            Err(e) => {
                let err = errno(&e);
                warn!(op = "symlink", link = %path_str, target = %target_str, error = %e, "symlink failed");
                self.emit_event(
                    SkillEvent::new(SkillEventKind::SymlinkAttempt)
                        .with_skill_name(&skill_name)
                        .with_relative_path(&relative_path)
                        .with_action(SkillEventAction::Failed)
                        .with_errno(err)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!("class={} target={}", class_label, target_str)),
                );
                reply.error(err);
            }
        }
    }
    pub(in crate::fs) fn link_impl(
        &mut self,
        req: &Request,
        ino: u64,
        newparent: u64,
        newname: &std::ffi::OsStr,
        reply: ReplyEntry,
    ) {
        let new_path_str = match self.build_fuse_path(newparent, newname) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let new_path_type = self.parse_fuse_path(Path::new(&new_path_str));

        // Resolve the source FUSE path from its inode. Without a path
        // mapping we cannot reason about same-skill / cross-skill — the
        // kernel only handed us a number, and the policy decision is
        // boundary-by-name. Bail with ENOENT in that case so audit logs
        // can tell it apart from a refused-by-policy outcome.
        let source_path_str = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => {
                self.emit_event(
                    SkillEvent::new(SkillEventKind::HardlinkAttempt)
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::ENOENT)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!("dst={} src_ino={} unmapped", new_path_str, ino)),
                );
                reply.error(libc::ENOENT);
                return;
            }
        };
        let source_path_type = self.parse_fuse_path(Path::new(&source_path_str));

        // Destination must be a passthrough leaf.
        let (dst_skill, dst_rel) = match &new_path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => (skill_name.clone(), relative_path.clone()),
            _ => {
                self.ro_warn("link", &new_path_str);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::HardlinkAttempt)
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::EROFS)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!(
                            "src={} dst={} class=virtual_dst",
                            source_path_str, new_path_str
                        )),
                );
                reply.error(libc::EROFS);
                return;
            }
        };

        // Source must also be a passthrough leaf (not SKILL.md, not a
        // virtual /skills entry). Hardlinks pointing at a virtual file
        // would either pin compiled content to a real inode or duplicate
        // a virtual file that has no on-disk identity.
        let (src_skill, src_rel) = match &source_path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => (skill_name.clone(), relative_path.clone()),
            _ => {
                self.emit_event(
                    SkillEvent::new(SkillEventKind::HardlinkAttempt)
                        .with_skill_name(&dst_skill)
                        .with_relative_path(&dst_rel)
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::EPERM)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!(
                            "src={} dst={} class=virtual_src",
                            source_path_str, new_path_str
                        )),
                );
                reply.error(libc::EPERM);
                return;
            }
        };

        if is_skill_discover_path(&src_skill) || is_skill_discover_path(&dst_skill) {
            self.emit_event(
                SkillEvent::new(SkillEventKind::HardlinkAttempt)
                    .with_skill_name(&dst_skill)
                    .with_relative_path(&dst_rel)
                    .with_action(SkillEventAction::Rejected)
                    .with_errno(libc::EROFS)
                    .with_caller(req.uid(), req.gid())
                    .with_detail(format!(
                        "src={} dst={} class=skill_discover",
                        source_path_str, new_path_str
                    )),
            );
            reply.error(libc::EROFS);
            return;
        }

        if src_skill != dst_skill {
            warn!(
                op = "link",
                src = %source_path_str,
                dst = %new_path_str,
                src_skill = %src_skill,
                dst_skill = %dst_skill,
                "cross-skill hardlink rejected"
            );
            self.emit_event(
                SkillEvent::new(SkillEventKind::HardlinkAttempt)
                    .with_skill_name(&dst_skill)
                    .with_relative_path(&dst_rel)
                    .with_action(SkillEventAction::Rejected)
                    .with_errno(libc::EACCES)
                    .with_caller(req.uid(), req.gid())
                    .with_detail(format!(
                        "src={} dst={} class=cross_skill src_skill={}",
                        source_path_str, new_path_str, src_skill
                    )),
            );
            reply.error(libc::EACCES);
            return;
        }

        // Lifecycle reservation on the destination link path.
        if let Some(errno) = self.enforce_lifecycle_reservation(
            &new_path_type,
            SkillEventKind::HardlinkAttempt,
            req,
            Some(format!("src={}", source_path_str)),
        ) {
            reply.error(errno);
            return;
        }
        // `.skill-meta` gate on the destination (link must not appear
        // under `.skill-meta`).
        if let Some(errno) = self.enforce_skill_meta(
            &new_path_type,
            SkillEventKind::HardlinkAttempt,
            req,
            Some(format!("src={}", source_path_str)),
        ) {
            reply.error(errno);
            return;
        }
        // `.skill-meta` gate on the source — hardlinking a protected
        // file out from under `.skill-meta` would leak the inode under
        // an unprotected name, so refuse before touching the filesystem.
        if let Some(errno) = self.enforce_skill_meta(
            &source_path_type,
            SkillEventKind::HardlinkAttempt,
            req,
            Some(format!("dst={}", new_path_str)),
        ) {
            reply.error(errno);
            return;
        }

        let src_physical = self.skill_physical_dir(&src_skill).join(&src_rel);
        let dst_physical = self.skill_physical_dir(&dst_skill).join(&dst_rel);

        // T2 hardlink scope: same-skill **ordinary regular files only**.
        // `symlink_metadata` deliberately does NOT follow symlinks, so a
        // symlink source surfaces as `is_symlink()` and is refused
        // here rather than being silently followed to its target. Every
        // non-regular kind (directory, symlink, FIFO, socket, block /
        // char device, or any other special file) is rejected with
        // `EPERM` and a `class=non_regular_source` audit event so
        // operators can tell the rejection apart from an unimplemented
        // surface.  `ENOENT` and other stat errors fall through to a
        // `Failed` event preserving the underlying errno.
        match std::fs::symlink_metadata(&src_physical) {
            Ok(meta) if meta.file_type().is_file() => {
                // OK — proceed to `hard_link` below.
            }
            Ok(_) => {
                warn!(
                    op = "link",
                    src = %source_path_str,
                    dst = %new_path_str,
                    "non-regular hardlink source rejected (T2 scope)"
                );
                self.emit_event(
                    SkillEvent::new(SkillEventKind::HardlinkAttempt)
                        .with_skill_name(&dst_skill)
                        .with_relative_path(&dst_rel)
                        .with_action(SkillEventAction::Rejected)
                        .with_errno(libc::EPERM)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!(
                            "src={} dst={} class=non_regular_source",
                            source_path_str, new_path_str
                        )),
                );
                reply.error(libc::EPERM);
                return;
            }
            Err(e) => {
                let err = errno(&e);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::HardlinkAttempt)
                        .with_skill_name(&dst_skill)
                        .with_relative_path(&dst_rel)
                        .with_action(SkillEventAction::Failed)
                        .with_errno(err)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!(
                            "src={} dst={} class=src_stat_err",
                            source_path_str, new_path_str
                        )),
                );
                reply.error(err);
                return;
            }
        }

        match std::fs::hard_link(&src_physical, &dst_physical) {
            Ok(()) => {
                let dst_ino = self
                    .inodes
                    .allocate(&new_path_str, FileType::RegularFile, newparent);
                self.inodes.remember(dst_ino);
                let attr = match std::fs::symlink_metadata(&dst_physical) {
                    Ok(meta) => {
                        let mut a = file_attr_from_metadata(&meta);
                        a.ino = dst_ino;
                        a
                    }
                    Err(_) => {
                        let mut a = self.virtual_file_attr(0);
                        a.ino = dst_ino;
                        a
                    }
                };
                self.observe_mutation(&dst_skill, Some(&dst_rel), security::MutationKind::Create);
                self.emit_event(
                    SkillEvent::new(SkillEventKind::HardlinkAttempt)
                        .with_skill_name(&dst_skill)
                        .with_relative_path(&dst_rel)
                        .with_action(SkillEventAction::Allowed)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!(
                            "src={} dst={} class=same_skill",
                            source_path_str, new_path_str
                        )),
                );
                reply.entry(&Duration::from_secs(1), &attr, 0);
            }
            Err(e) => {
                let err = errno(&e);
                warn!(op = "link", src = %source_path_str, dst = %new_path_str, error = %e, "hard_link failed");
                self.emit_event(
                    SkillEvent::new(SkillEventKind::HardlinkAttempt)
                        .with_skill_name(&dst_skill)
                        .with_relative_path(&dst_rel)
                        .with_action(SkillEventAction::Failed)
                        .with_errno(err)
                        .with_caller(req.uid(), req.gid())
                        .with_detail(format!(
                            "src={} dst={} class=same_skill",
                            source_path_str, new_path_str
                        )),
                );
                reply.error(err);
            }
        }
    }
}
