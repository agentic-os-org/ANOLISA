//! FUSE read-path callbacks: `open`, `read`, `release`, `flush`, `fsync`.

use std::os::unix::fs::FileExt;
use std::path::Path;

use fuser::consts::FOPEN_DIRECT_IO;
use fuser::{FUSE_ROOT_ID, ReplyData, ReplyEmpty, ReplyOpen, Request};
use tracing::{debug, warn};

use super::super::SkillFs;
use super::super::read_resolution::ReadResolution;
use crate::handles::open_options_from_flags;
use crate::path::{PathType, is_skill_discover_path};
use crate::security::{SkillEventAction, SkillEventKind};
use crate::sync::SyncEvent;
use crate::sys::{errno, openat_leaf};

impl SkillFs {
    pub(in crate::fs) fn read_impl(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        debug!(ino, offset, size, "read");

        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        // Captured reads must survive source and namespace changes after open.
        if let Some(content) = self
            .handles
            .with_handle(fh, |entry| entry.transformed.clone())
            .flatten()
        {
            let start = (offset as usize).min(content.len());
            let end = start.saturating_add(size as usize).min(content.len());
            reply.data(&content.as_bytes()[start..end]);
            return;
        }

        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => {
                // Open-after-unlink fast path. The inode → path mapping was
                // torn down by `unlink`, but POSIX guarantees the open fd
                // remains usable until last close. If the handle still owns
                // a real file, serve the read directly from it. Reads of
                // virtual SKILL.md content reach this branch only if the
                // inode was forcibly evicted (it cannot be `unlink`ed
                // through FUSE because `unlink` removes the path mapping
                // synchronously) so the handle's `file = None` correctly
                // returns ENOENT here.
                let handle_read = self.handles.with_handle(fh, |entry| {
                    entry.file.as_ref().map(|file| {
                        let mut buf = vec![0u8; size as usize];
                        file.read_at(&mut buf, offset as u64)
                            .map(|n| buf[..n].to_vec())
                    })
                });
                match handle_read {
                    Some(Some(Ok(data))) => reply.data(&data),
                    Some(Some(Err(e))) => reply.error(errno(&e)),
                    _ => reply.error(libc::ENOENT),
                }
                return;
            }
        };

        let path_type = self.parse_fuse_path(Path::new(&path));

        // Read events are high-volume so we emit only on failure to keep the
        // audit stream useful without flooding it with per-syscall successes.
        // The byte-count signal still exists on the Write side.
        let pinned = self
            .handles
            .with_handle(fh, |entry| entry.pinned_target.clone())
            .flatten();
        let content = match path_type.clone() {
            PathType::SkillMd { skill_name }
                if self.is_staging_skill_root(&skill_name)
                    || self.is_pending_install(&skill_name) =>
            {
                // I2/pending: staging and pending install SKILL.md reads
                // from the physical handle.
                let result = self.handles.with_handle(fh, |entry| {
                    if let Some(ref file) = entry.file {
                        let mut buf = vec![0u8; size as usize];
                        match file.read_at(&mut buf, offset as u64) {
                            Ok(n) => Ok(buf[..n].to_vec()),
                            Err(e) => Err(errno(&e)),
                        }
                    } else {
                        Err(libc::EBADF)
                    }
                });
                match result {
                    Some(Ok(data)) => {
                        reply.data(&data);
                        return;
                    }
                    Some(Err(e)) => {
                        reply.error(e);
                        return;
                    }
                    None => {
                        reply.error(libc::EBADF);
                        return;
                    }
                }
            }
            PathType::SkillMd { skill_name } => {
                match self.compiled_skill_md_pinned(&skill_name, pinned.as_ref()) {
                    Some(c) => c,
                    None => {
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Read,
                            SkillEventAction::Failed,
                            Some(libc::ENOENT),
                            None,
                        );
                        reply.error(libc::ENOENT);
                        return;
                    }
                }
            }
            PathType::NestedSkillMd {
                ref category,
                ref skill_name,
            } if self.is_staging_skill_root(&Self::hermes_skill_id(category, skill_name))
                || self.is_pending_install(&Self::hermes_skill_id(category, skill_name)) =>
            {
                // Staging / pending nested SKILL.md is served raw from the
                // physical handle, mirroring the flat SkillMd behavior.
                let result = self.handles.with_handle(fh, |entry| {
                    if let Some(ref file) = entry.file {
                        let mut buf = vec![0u8; size as usize];
                        match file.read_at(&mut buf, offset as u64) {
                            Ok(n) => Ok(buf[..n].to_vec()),
                            Err(e) => Err(errno(&e)),
                        }
                    } else {
                        Err(libc::EBADF)
                    }
                });
                match result {
                    Some(Ok(data)) => {
                        reply.data(&data);
                        return;
                    }
                    Some(Err(e)) => {
                        reply.error(e);
                        return;
                    }
                    None => {
                        reply.error(libc::EBADF);
                        return;
                    }
                }
            }
            PathType::NestedSkillMd {
                ref category,
                ref skill_name,
            } => {
                // Nested SKILL.md runs the same pipeline as the flat SkillMd
                // and honors the handle's pinned activation target so a later
                // resolver change cannot re-point an open read.
                match self.compiled_hermes_nested_skill_md_pinned(
                    category,
                    skill_name,
                    pinned.as_ref(),
                ) {
                    Some(c) => c,
                    None => {
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Read,
                            SkillEventAction::Failed,
                            Some(libc::ENOENT),
                            None,
                        );
                        reply.error(libc::ENOENT);
                        return;
                    }
                }
            }
            PathType::Passthrough { .. }
            | PathType::InboxPassthrough { .. }
            | PathType::HermesMeta { .. }
            | PathType::HermesMetaChild { .. }
            | PathType::CategoryPassthrough { .. }
            | PathType::NestedPassthrough { .. } => {
                // Use fd-backed read via handle
                let result = self.handles.with_handle(fh, |entry| {
                    if let Some(ref file) = entry.file {
                        let mut buf = vec![0u8; size as usize];
                        match file.read_at(&mut buf, offset as u64) {
                            Ok(n) => Ok(buf[..n].to_vec()),
                            Err(e) => Err(errno(&e)),
                        }
                    } else {
                        Err(libc::EBADF)
                    }
                });
                match result {
                    Some(Ok(data)) => {
                        reply.data(&data);
                        return;
                    }
                    Some(Err(e)) => {
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Read,
                            SkillEventAction::Failed,
                            Some(e),
                            None,
                        );
                        reply.error(e);
                        return;
                    }
                    None => {
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Read,
                            SkillEventAction::Failed,
                            Some(libc::EBADF),
                            None,
                        );
                        reply.error(libc::EBADF);
                        return;
                    }
                }
            }
            _ => {
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Read,
                    SkillEventAction::Failed,
                    Some(libc::EISDIR),
                    None,
                );
                reply.error(libc::EISDIR);
                return;
            }
        };

        let offset = offset as usize;
        if offset >= content.len() {
            reply.data(&[]);
            return;
        }
        let end = (offset + size as usize).min(content.len());
        reply.data(&content.as_bytes()[offset..end]);
    }
    pub(in crate::fs) fn open_impl(
        &mut self,
        req: &Request,
        ino: u64,
        flags: i32,
        reply: ReplyOpen,
    ) {
        // Test-only: scope every resolver read this `open` performs so a unit
        // test can assert the activation target is read exactly once (a single
        // snapshot drives both the decision and the handle's pinned target).
        #[cfg(test)]
        let _open_scope = crate::security::active::open_decision_scope();
        debug!(ino, flags, "open");
        if self.inodes.get(ino).is_none() && ino != FUSE_ROOT_ID {
            reply.error(libc::ENOENT);
            return;
        }

        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let path_type = self.parse_fuse_path(Path::new(&path));
        let access_mode = flags & libc::O_ACCMODE;
        let is_write = access_mode == libc::O_WRONLY || access_mode == libc::O_RDWR;

        // Virtual directory types: return EISDIR for file open
        match &path_type {
            PathType::Root | PathType::SkillsDir | PathType::InboxDir => {
                reply.error(libc::EISDIR);
                return;
            }
            PathType::SkillDir { skill_name } => {
                // skill-discover dir opened for write → EROFS
                if skill_name == "skill-discover" && is_write {
                    reply.error(libc::EROFS);
                    return;
                }
                reply.error(libc::EISDIR);
                return;
            }
            PathType::InboxSkillDir { skill_name } => {
                if !Self::is_inbox_skill_name_allowed(skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                reply.error(libc::EISDIR);
                return;
            }
            PathType::InboxPassthrough { skill_name, .. } => {
                if !Self::is_inbox_skill_name_allowed(skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
            }
            PathType::CategoryDir { .. } => {
                reply.error(libc::EISDIR);
                return;
            }
            PathType::NestedSkillDir {
                category,
                skill_name,
            } => {
                if matches!(
                    self.resolve_hermes_nested_read(category, skill_name),
                    ReadResolution::Hidden
                ) {
                    reply.error(libc::ENOENT);
                    return;
                }
                reply.error(libc::EISDIR);
                return;
            }
            PathType::HermesMeta { name } => {
                let physical = self.source_base().join(name);
                if physical.is_dir() {
                    reply.error(libc::EISDIR);
                    return;
                }
            }
            _ => {}
        }

        // skill-discover/SKILL.md is always read-only
        if let PathType::SkillMd { ref skill_name } = path_type {
            if skill_name == "skill-discover" {
                if is_write {
                    reply.error(libc::EROFS);
                    return;
                }
                // Read-only open for virtual skill-discover SKILL.md
                let fh = self.handles.allocate(ino, flags, None, None);
                reply.opened(fh, 0);
                return;
            }
        }

        // Trusted `.skill-meta` read-path gate. Untrusted callers are
        // denied; trusted read-only opens route to the live source
        // directory. Mutating opens fall through to enforce_skill_meta
        // so the existing audit/policy path is preserved.
        let is_mutating_open = is_write || (flags & libc::O_TRUNC) != 0;
        match self.is_trusted_skill_meta_access(&path_type, req) {
            Some(false) => {
                reply.error(libc::ENOENT);
                return;
            }
            Some(true) if !is_mutating_open => {
                let physical = match path_type {
                    PathType::Passthrough {
                        ref skill_name,
                        ref relative_path,
                    } => Some(self.skill_physical_dir(skill_name).join(relative_path)),
                    PathType::InboxPassthrough {
                        ref skill_name,
                        ref relative_path,
                    } => Some(self.inbox_skill_dir(skill_name).join(relative_path)),
                    PathType::NestedPassthrough {
                        ref category,
                        ref skill_name,
                        ref relative_path,
                    } => Some(
                        self.hermes_skill_physical_dir(category, skill_name)
                            .join(relative_path),
                    ),
                    _ => None,
                };
                if let Some(physical) = physical {
                    match open_options_from_flags(flags).open(&physical) {
                        Ok(file) => {
                            let fh = self.handles.allocate(ino, flags, Some(file), None);
                            reply.opened(fh, 0);
                        }
                        Err(e) => reply.error(errno(&e)),
                    }
                    return;
                }
            }
            Some(true) => {
                // Mutating open on trusted .skill-meta — fall through
                // to enforce_skill_meta for audit/policy.
            }
            None => {}
        }

        // S1: deny mutating opens (write modes, O_APPEND with write, or
        // O_TRUNC even with O_RDONLY) on `.skill-meta/**` before any I/O.
        // O_RDONLY without O_TRUNC stays allowed so directory traversal
        // and read-only manifest inspection still work.
        if is_mutating_open {
            // S3: deny mutating opens on a reserved lifecycle namespace.
            // Read-only opens are blocked earlier by `lookup` returning
            // ENOENT, so this gate only matters for callers that already
            // hold an inode for a lifecycle path (defense in depth).
            if let Some(errno) = self.enforce_lifecycle_reservation(
                &path_type,
                SkillEventKind::Write,
                req,
                Some(format!("flags=0x{:x}", flags)),
            ) {
                reply.error(errno);
                return;
            }
            if let Some(errno) = self.enforce_skill_meta(
                &path_type,
                SkillEventKind::Write,
                req,
                Some(format!("flags=0x{:x}", flags)),
            ) {
                reply.error(errno);
                return;
            }
        }

        // For non-virtual paths, resolve physical path
        let mut physical = match self.resolve_physical_path(&path) {
            Some(p) => p,
            None => {
                reply.error(libc::EROFS);
                return;
            }
        };

        // D1.1: ledger-aware read redirection. Hidden skills refuse to
        // open even on a stale inode; snapshot skills serve passthrough
        // reads from the snapshot directory.
        //
        // Crucially, the redirect only kicks in for **non-mutating**
        // opens. `O_RDONLY | O_TRUNC` is mutating from the kernel's
        // perspective (the access mode is read-only but the open also
        // truncates the file), so `is_write == false` is *not* a safe
        // signal — we must use `is_mutating_open`. If we redirected on
        // `!is_write` alone, the truncate branch further down would
        // truncate the snapshot file, violating the D1.1 invariant
        // "snapshots are read-only; writes/create/rename/unlink/setattr
        // still target live source". Mutating opens stay pointed at the
        // live source physical path (write to live, read from snapshot
        // is enforced separately via lookup/getattr/read).
        let mut pinned_target: Option<crate::security::ActiveTarget> = None;
        match &path_type {
            PathType::SkillMd { skill_name } | PathType::Passthrough { skill_name, .. } => {
                // I2: staging roots bypass the active resolver entirely.
                // Pending installs use the same bypass.
                if !self.is_staging_skill_root(skill_name) && !self.is_pending_install(skill_name) {
                    // TOCTOU-safe: read the activation target exactly once and
                    // drive both the open-time decision and the handle's pinned
                    // target from that single snapshot, so a `Current -> Snapshot`
                    // change cannot let the open be judged against one target
                    // while reads serve another.
                    let (target, resolution) = self.resolve_skill_read_pinned(skill_name);
                    pinned_target = target;
                    let grace_rel = match &path_type {
                        PathType::Passthrough { relative_path, .. } => {
                            Some(relative_path.as_path())
                        }
                        // SkillMd is never grace-allowed: use the literal
                        // "SKILL.md" path so the whitelist check rejects it.
                        PathType::SkillMd { .. } => Some(Path::new("SKILL.md")),
                        _ => None,
                    };
                    // Best-effort runtime metrics. `skill_hit` fires for any
                    // agent-visible read open of a real skill file; `policy_*`
                    // fires only when an active resolver actually made a
                    // security decision — plain non-security passthrough (no
                    // resolver) is NOT counted as an allow. `.skill-meta/**` and
                    // skill-discover are excluded.
                    let is_discover = skill_name == "skill-discover";
                    let is_meta = matches!(
                        &path_type,
                        PathType::Passthrough { relative_path, .. }
                            if crate::security::is_skill_meta_path(relative_path)
                    );
                    let record_hit = !is_discover && !is_meta && !is_mutating_open;
                    let policy_decided = self.active_resolver.is_some() && !is_discover && !is_meta;
                    match resolution {
                        ReadResolution::Hidden
                            if !self.is_post_publish_grace_allowed(skill_name, grace_rel) =>
                        {
                            if policy_decided {
                                self.metric_policy_denied();
                            }
                            reply.error(libc::ENOENT);
                            return;
                        }
                        ReadResolution::Hidden => {
                            // I4: grace bypass — serve from source (allowed).
                            if record_hit {
                                self.metric_skill_hit();
                            }
                            if policy_decided {
                                self.metric_policy_allow();
                            }
                        }
                        ReadResolution::Snapshot { dir, .. } if !is_mutating_open => {
                            if record_hit {
                                self.metric_skill_hit();
                            }
                            if policy_decided {
                                self.metric_policy_fallback();
                            }
                            if matches!(&path_type, PathType::SkillMd { .. }) {
                                physical = dir.join("SKILL.md");
                            }
                            if let PathType::Passthrough { relative_path, .. } = &path_type {
                                // I4: grace bypass — if the path matches the
                                // post-publish whitelist, open from physical
                                // source instead of the snapshot.
                                if !self.is_post_publish_grace_allowed(
                                    skill_name,
                                    Some(relative_path.as_path()),
                                ) {
                                    physical = dir.join(relative_path);
                                }
                            }
                        }
                        ReadResolution::Snapshot { .. } => {
                            // Mutating open resolving to a snapshot: the write
                            // still targets live source; count the fallback
                            // decision but not a read hit.
                            if policy_decided {
                                self.metric_policy_fallback();
                            }
                        }
                        ReadResolution::Source => {
                            if record_hit {
                                self.metric_skill_hit();
                            }
                            if policy_decided {
                                self.metric_policy_allow();
                            }
                        }
                    }
                }
            }
            PathType::NestedSkillMd {
                category,
                skill_name,
            }
            | PathType::NestedPassthrough {
                category,
                skill_name,
                ..
            } => {
                // H3: staging and pending install bypass for nested skills.
                let nested_id = Self::hermes_skill_id(category, skill_name);
                if !self.is_staging_skill_root(&nested_id) && !self.is_pending_install(&nested_id) {
                    // TOCTOU-safe: read the nested skill's activation target once
                    // and use that single snapshot for both the open-time decision
                    // and the handle's pinned target — mirrors the flat contract.
                    let (target, resolution) =
                        self.resolve_hermes_nested_read_pinned(category, skill_name);
                    pinned_target = target;
                    let grace_rel = match &path_type {
                        PathType::NestedPassthrough { relative_path, .. } => {
                            Some(relative_path.as_path())
                        }
                        PathType::NestedSkillMd { .. } => Some(Path::new("SKILL.md")),
                        _ => None,
                    };
                    match resolution {
                        ReadResolution::Hidden
                            if !self.is_post_publish_grace_allowed(&nested_id, grace_rel) =>
                        {
                            reply.error(libc::ENOENT);
                            return;
                        }
                        ReadResolution::Hidden => {
                            // H3: grace bypass — let the open proceed against source.
                        }
                        ReadResolution::Snapshot { dir, .. } if !is_mutating_open => {
                            match &path_type {
                                PathType::NestedSkillMd { .. } => {
                                    physical = dir.join("SKILL.md");
                                }
                                PathType::NestedPassthrough { relative_path, .. } => {
                                    physical = dir.join(relative_path);
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        // O_NOFOLLOW check first (higher priority than O_DIRECTORY):
        // If path is a symlink and O_NOFOLLOW is set, block the open.
        // When O_DIRECTORY is also set, kernel sees symlink as non-directory → ENOTDIR.
        // Otherwise, O_NOFOLLOW alone → ELOOP.
        if (flags & libc::O_NOFOLLOW) != 0 {
            if let Ok(m) = std::fs::symlink_metadata(&physical) {
                if m.file_type().is_symlink() {
                    if (flags & libc::O_DIRECTORY) != 0 {
                        // O_NOFOLLOW|O_DIRECTORY on symlink: kernel sees symlink as non-directory
                        reply.error(libc::ENOTDIR);
                    } else {
                        reply.error(libc::ELOOP);
                    }
                    return;
                }
            }
        }

        if (flags & libc::O_DIRECTORY) != 0 {
            if let Ok(meta) = std::fs::metadata(&physical) {
                if !meta.is_dir() {
                    reply.error(libc::ENOTDIR);
                    return;
                }
            }
        }

        // Directory file open: O_DIRECTORY + read-only is allowed (allocate empty handle);
        // write modes on directories return EISDIR (matching Linux semantics).
        if let PathType::Passthrough { .. } = &path_type {
            if let Ok(meta) = std::fs::metadata(&physical) {
                if meta.is_dir() {
                    if (flags & libc::O_DIRECTORY) != 0 {
                        // O_DIRECTORY on actual directory: only O_RDONLY is permitted
                        let access_mode = flags & libc::O_ACCMODE;
                        if access_mode == libc::O_RDONLY {
                            let fh = self.handles.allocate(ino, flags, None, None);
                            reply.opened(fh, 0);
                        } else {
                            // Write mode on directory -> EISDIR
                            reply.error(libc::EISDIR);
                        }
                        return;
                    } else {
                        reply.error(libc::EISDIR);
                        return;
                    }
                }
            }
        }

        // skill-discover passthrough paths are read-only
        if let PathType::Passthrough { ref skill_name, .. } = path_type {
            if is_skill_discover_path(skill_name) && is_write {
                reply.error(libc::EROFS);
                return;
            }
        }

        let transformed_skill = match &path_type {
            PathType::SkillMd { skill_name } => Some(skill_name.clone()),
            PathType::NestedSkillMd {
                category,
                skill_name,
            } => Some(Self::hermes_skill_id(category, skill_name)),
            _ => None,
        };
        if let Some(skill_name) = transformed_skill.filter(|name| {
            !is_mutating_open
                && !self.transform_pipeline.is_empty()
                && !self.is_staging_skill_root(name)
                && !self.is_pending_install(name)
        }) {
            let content =
                match self.capture_transformed(&skill_name, &physical, pinned_target.as_ref()) {
                    Ok(content) => content,
                    Err(error) => {
                        let err = errno(&error);
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Open,
                            SkillEventAction::Failed,
                            Some(err),
                            None,
                        );
                        reply.error(err);
                        return;
                    }
                };
            let fh = match self
                .handles
                .allocate_captured(ino, flags, pinned_target, content)
            {
                Ok(fh) => fh,
                Err(err) => {
                    self.emit_op_event(
                        req,
                        &path_type,
                        SkillEventKind::Open,
                        SkillEventAction::Failed,
                        Some(err),
                        None,
                    );
                    reply.error(err);
                    return;
                }
            };
            self.emit_op_event_with_detail(
                req,
                &path_type,
                SkillEventKind::Open,
                SkillEventAction::Allowed,
                None,
                None,
                self.os_adapter_open_detail(),
            );
            // An inode's page cache cannot represent different per-open versions.
            reply.opened(fh, FOPEN_DIRECT_IO);
            return;
        }

        // SKILL.md: virtual read, physical write
        if let PathType::SkillMd { ref skill_name } = path_type {
            let is_trunc = (flags & libc::O_TRUNC) != 0;

            // O_TRUNC always truncates source, regardless of access mode
            if is_trunc {
                if let Err(e) = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&physical)
                {
                    warn!(op = "open", ?physical, error = %e, "SKILL.md O_TRUNC failed");
                    reply.error(errno(&e));
                    return;
                }
                self.send_sync(SyncEvent::Reparse {
                    skill_name: skill_name.clone(),
                    source_path: physical.clone(),
                });
                self.observe_mutation(
                    skill_name,
                    Some(Path::new("SKILL.md")),
                    crate::security::MutationKind::SetattrTruncate,
                );
            }

            if is_write {
                // Open physical file for writing
                match open_options_from_flags(flags).open(&physical) {
                    Ok(file) => {
                        let fh =
                            self.handles
                                .allocate(ino, flags, Some(file), pinned_target.clone());
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Open,
                            SkillEventAction::Allowed,
                            None,
                            None,
                        );
                        reply.opened(fh, 0);
                    }
                    Err(e) => {
                        warn!(op = "open", ?physical, error = %e, "open failed");
                        let err = errno(&e);
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Open,
                            SkillEventAction::Failed,
                            Some(err),
                            None,
                        );
                        reply.error(err);
                    }
                }
            } else if self.is_staging_skill_root(skill_name) || self.is_pending_install(skill_name)
            {
                // I2/pending: staging and pending install SKILL.md is
                // served as raw physical file.
                match open_options_from_flags(flags).open(&physical) {
                    Ok(file) => {
                        let fh = self.handles.allocate(ino, flags, Some(file), None);
                        reply.opened(fh, 0);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            } else {
                // Read-only open for SKILL.md: virtual content, no physical fd needed
                let fh = self
                    .handles
                    .allocate(ino, flags, None, pinned_target.clone());
                let detail = if is_mutating_open {
                    None
                } else {
                    self.os_adapter_open_detail()
                };
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Open,
                    SkillEventAction::Allowed,
                    None,
                    None,
                    detail,
                );
                reply.opened(fh, 0);
            }
            return;
        }

        // Nested SKILL.md: virtual compiled read, physical write. Mirrors
        // the flat SkillMd handling so nested skills strip frontmatter on
        // read while writes still target the live source file.
        if let PathType::NestedSkillMd {
            ref category,
            ref skill_name,
        } = path_type
        {
            let nested_id = Self::hermes_skill_id(category, skill_name);
            let is_trunc = (flags & libc::O_TRUNC) != 0;

            if is_trunc {
                if let Err(e) = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&physical)
                {
                    warn!(op = "open", ?physical, error = %e, "nested SKILL.md O_TRUNC failed");
                    reply.error(errno(&e));
                    return;
                }
                self.observe_mutation(
                    &nested_id,
                    Some(Path::new("SKILL.md")),
                    crate::security::MutationKind::SetattrTruncate,
                );
            }

            if is_write {
                match open_options_from_flags(flags).open(&physical) {
                    Ok(file) => {
                        let fh = self.handles.allocate(ino, flags, Some(file), None);
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Open,
                            SkillEventAction::Allowed,
                            None,
                            None,
                        );
                        reply.opened(fh, 0);
                    }
                    Err(e) => {
                        warn!(op = "open", ?physical, error = %e, "open failed");
                        let err = errno(&e);
                        self.emit_op_event(
                            req,
                            &path_type,
                            SkillEventKind::Open,
                            SkillEventAction::Failed,
                            Some(err),
                            None,
                        );
                        reply.error(err);
                    }
                }
            } else if self.is_staging_skill_root(&nested_id) || self.is_pending_install(&nested_id)
            {
                // Staging / pending nested SKILL.md is served raw.
                match open_options_from_flags(flags).open(&physical) {
                    Ok(file) => {
                        let fh = self.handles.allocate(ino, flags, Some(file), None);
                        reply.opened(fh, 0);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            } else {
                // Read-only: compiled content served virtually, no fd. Pin the
                // resolved activation target so the read stays stable.
                let fh = self
                    .handles
                    .allocate(ino, flags, None, pinned_target.clone());
                let detail = if is_mutating_open {
                    None
                } else {
                    self.os_adapter_open_detail()
                };
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Open,
                    SkillEventAction::Allowed,
                    None,
                    None,
                    detail,
                );
                reply.opened(fh, 0);
            }
            return;
        }

        // Passthrough file: open with real fd
        // O_RDONLY|O_TRUNC: truncate first, then open read-only
        let is_trunc = (flags & libc::O_TRUNC) != 0;
        if is_trunc && access_mode == libc::O_RDONLY {
            // Perform truncation as a separate operation (Linux allows O_RDONLY|O_TRUNC to truncate)
            let trunc_result = match std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&physical)
            {
                Ok(f) => Ok(f),
                Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                    match self.open_parent_dir_for(&path) {
                        Ok((parent_fd, leaf)) => {
                            openat_leaf(&parent_fd, &leaf, libc::O_WRONLY | libc::O_TRUNC, 0)
                        }
                        Err(_) => Err(e),
                    }
                }
                Err(e) => Err(e),
            };
            if let Err(e) = trunc_result {
                let err = errno(&e);
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Open,
                    SkillEventAction::Failed,
                    Some(err),
                    None,
                );
                reply.error(err);
                return;
            }
            match &path_type {
                PathType::Passthrough {
                    skill_name,
                    relative_path,
                } => self.observe_mutation(
                    skill_name,
                    Some(relative_path.as_path()),
                    crate::security::MutationKind::SetattrTruncate,
                ),
                PathType::InboxPassthrough {
                    skill_name,
                    relative_path,
                } => self.inbox_observe_install_complete(
                    skill_name,
                    relative_path.as_path(),
                    crate::security::MutationKind::SetattrTruncate,
                ),
                _ => {}
            }
        }

        // Then open with the requested access mode (open_options_from_flags handles non-RDONLY truncate)
        let opts = open_options_from_flags(flags);
        let primary_open = opts.open(&physical);
        let final_open = match primary_open {
            Ok(f) => Ok(f),
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                match self.open_parent_dir_for(&path) {
                    Ok((parent_fd, leaf)) => openat_leaf(&parent_fd, &leaf, flags, 0),
                    Err(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        match final_open {
            Ok(file) => {
                let fh = self.handles.allocate(ino, flags, Some(file), pinned_target);
                if is_trunc && access_mode != libc::O_RDONLY {
                    match &path_type {
                        PathType::Passthrough {
                            skill_name,
                            relative_path,
                        } => self.observe_mutation(
                            skill_name,
                            Some(relative_path.as_path()),
                            crate::security::MutationKind::SetattrTruncate,
                        ),
                        PathType::InboxPassthrough {
                            skill_name,
                            relative_path,
                        } => self.inbox_observe_install_complete(
                            skill_name,
                            relative_path.as_path(),
                            crate::security::MutationKind::SetattrTruncate,
                        ),
                        _ => {}
                    }
                }
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Open,
                    SkillEventAction::Allowed,
                    None,
                    None,
                );
                reply.opened(fh, 0);
            }
            Err(e) => {
                warn!(op = "open", ?physical, error = %e, "open failed");
                let err = errno(&e);
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Open,
                    SkillEventAction::Failed,
                    Some(err),
                    None,
                );
                reply.error(err);
            }
        }
    }
    pub(in crate::fs) fn release_impl(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.handles.remove(fh);
        reply.ok();
    }
    pub(in crate::fs) fn flush_impl(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        let exists = self.handles.with_handle(fh, |_| ()).is_some();
        if exists {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }
    pub(in crate::fs) fn fsync_impl(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        let result = self.handles.with_handle(fh, |entry| {
            if let Some(ref file) = entry.file {
                if datasync {
                    file.sync_data()
                } else {
                    file.sync_all()
                }
            } else {
                Ok(()) // virtual path
            }
        });
        match result {
            Some(Ok(())) => reply.ok(),
            Some(Err(e)) => reply.error(errno(&e)),
            None => reply.error(libc::EBADF),
        }
    }
}
