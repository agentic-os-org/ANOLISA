//! FUSE file-mutation callbacks: `write`, `create`, `mknod`, `setattr`.

use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{FileAttr, FileType, ReplyAttr, ReplyEntry, Request};
use tracing::{debug, warn};

use super::super::SkillFs;
use crate::attr::{file_attr_from_metadata, file_attr_from_stat};
use crate::handles::open_options_from_flags;
use crate::path::{PathType, is_skill_discover_path};
use crate::security::{MutationKind, SkillEventAction, SkillEventKind};
use crate::sync::SyncEvent;
use crate::sys::{errno, fstatat_leaf, openat_leaf};

impl SkillFs {
    pub(in crate::fs) fn write_impl(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => {
                // Open-after-unlink: the path mapping is gone but a write
                // arriving through the same fh must still land on the open
                // file descriptor (POSIX `unlink` leaves an open fd usable
                // until last close). S1/S3 defense-in-depth re-checks are
                // skipped because the path is no longer in any protected
                // zone — protection at unlink time already gated the move.
                let result = self.handles.with_handle_mut(fh, |entry| {
                    let access = entry.flags & libc::O_ACCMODE;
                    if access == libc::O_RDONLY {
                        return Err(libc::EBADF);
                    }
                    if let Some(ref file) = entry.file {
                        if entry.append_mode {
                            use std::io::Write;
                            let mut file_ref = file;
                            file_ref.write(data).map_err(|e| errno(&e))
                        } else {
                            file.write_at(data, offset as u64).map_err(|e| errno(&e))
                        }
                    } else {
                        Err(libc::EBADF)
                    }
                });
                match result {
                    Some(Ok(n)) => {
                        reply.written(n as u32);
                    }
                    Some(Err(e)) => {
                        reply.error(e);
                    }
                    None => {
                        reply.error(libc::ENOENT);
                    }
                }
                return;
            }
        };

        let path_type = self.parse_fuse_path(Path::new(&path));

        // skill-discover namespace is always read-only
        match &path_type {
            PathType::SkillMd { skill_name }
            | PathType::SkillDir { skill_name }
            | PathType::Passthrough { skill_name, .. }
                if is_skill_discover_path(skill_name) =>
            {
                reply.error(libc::EROFS);
                return;
            }
            _ => {}
        }

        // S3 defense-in-depth: refuse writes against a reserved lifecycle
        // namespace even if a handle for it predates the boundary.
        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Write, req, None)
        {
            reply.error(errno);
            return;
        }

        // S1 defense-in-depth: even if a handle for `.skill-meta` slipped
        // past the open gate, refuse the write.
        if let Some(errno) = self.enforce_skill_meta(&path_type, SkillEventKind::Write, req, None) {
            reply.error(errno);
            return;
        }

        debug!(ino, offset, len = data.len(), "write");

        // Must go through fh lookup
        let result = self.handles.with_handle_mut(fh, |entry| {
            // Check writable
            let access = entry.flags & libc::O_ACCMODE;
            if access == libc::O_RDONLY {
                return Err(libc::EBADF);
            }
            if let Some(ref file) = entry.file {
                if entry.append_mode {
                    // O_APPEND: use write() (kernel guarantees seek-to-end)
                    use std::io::Write;
                    let mut file_ref = file;
                    match file_ref.write(data) {
                        Ok(n) => Ok(n),
                        Err(e) => Err(errno(&e)),
                    }
                } else {
                    match file.write_at(data, offset as u64) {
                        Ok(n) => Ok(n),
                        Err(e) => Err(errno(&e)),
                    }
                }
            } else {
                Err(libc::EBADF)
            }
        });

        match result {
            Some(Ok(written)) => {
                // Trigger async re-parse if this is a SKILL.md.
                // L1: inbox writes share the physical SKILL.md path,
                // so the store needs to re-parse from the same source
                // file regardless of which namespace the caller used.
                if let PathType::SkillMd { skill_name } = &path_type {
                    self.send_sync(SyncEvent::Reparse {
                        skill_name: skill_name.clone(),
                        source_path: self.skill_physical_dir(skill_name).join("SKILL.md"),
                    });
                }
                if let PathType::InboxPassthrough {
                    skill_name,
                    relative_path,
                } = &path_type
                {
                    if relative_path == Path::new("SKILL.md") {
                        self.send_sync(SyncEvent::Reparse {
                            skill_name: skill_name.clone(),
                            source_path: self.inbox_skill_dir(skill_name).join("SKILL.md"),
                        });
                    }
                }
                // D1.3-demo: enqueue a debounced refresh. Write is the
                // chunk-callback path, so we **never** run the resolve
                // here — the controller runs on a separate worker.
                //
                // L1: inbox writes only enqueue when the leaf is the
                // install-complete sentinel (multi-file installs
                // otherwise debounce-coalesce too aggressively for
                // the demo to render usefully).
                match &path_type {
                    PathType::SkillMd { skill_name } => self.observe_mutation(
                        skill_name,
                        Some(Path::new("SKILL.md")),
                        MutationKind::Write,
                    ),
                    PathType::Passthrough {
                        skill_name,
                        relative_path,
                    } => self.observe_mutation(
                        skill_name,
                        Some(relative_path.as_path()),
                        MutationKind::Write,
                    ),
                    PathType::InboxPassthrough {
                        skill_name,
                        relative_path,
                    } => self.inbox_observe_install_complete(
                        skill_name,
                        relative_path.as_path(),
                        MutationKind::Write,
                    ),
                    PathType::NestedSkillMd {
                        category,
                        skill_name,
                    } => {
                        let nested_id = Self::hermes_skill_id(category, skill_name);
                        self.observe_mutation(
                            &nested_id,
                            Some(Path::new("SKILL.md")),
                            MutationKind::Write,
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
                            MutationKind::Write,
                        );
                    }
                    _ => {}
                }
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Write,
                    SkillEventAction::Allowed,
                    None,
                    Some(written as u64),
                );
                reply.written(written as u32);
            }
            Some(Err(e)) => {
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Write,
                    SkillEventAction::Failed,
                    Some(e),
                    None,
                );
                reply.error(e);
            }
            None => {
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Write,
                    SkillEventAction::Failed,
                    Some(libc::EBADF),
                    None,
                );
                reply.error(libc::EBADF);
            }
        }
    }
    pub(in crate::fs) fn create_impl(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        let path_str = match self.build_fuse_path(parent, name) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let path_type = self.parse_fuse_path(Path::new(&path_str));

        // L1: the inbox virtual root is a directory; refuse to shadow it
        // with a regular file (e.g. a stray `touch /.skillfs-inbox` from
        // an unaware tool). The inbox skill candidate itself is also a
        // directory — `touch /.skillfs-inbox/<name>` would otherwise
        // resolve to a `source/<name>` regular file via
        // `resolve_physical_path`, which is not a valid skill candidate
        // and silently breaks the L1 contract that the candidate must
        // be a directory created via `mkdir`. Refuse both before any
        // physical resolution. `is_inbox_skill_name_allowed` keeps the
        // shape gate consistent with the rest of the inbox surface.
        match &path_type {
            PathType::InboxDir => {
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Create,
                    SkillEventAction::Rejected,
                    Some(libc::EEXIST),
                    None,
                );
                reply.error(libc::EEXIST);
                return;
            }
            PathType::InboxSkillDir { skill_name } => {
                let errno = if !Self::is_inbox_skill_name_allowed(skill_name) {
                    libc::EACCES
                } else {
                    // The candidate must be a directory; refuse plain
                    // file creation at this level. Use `EISDIR` so
                    // POSIX tools see "this name is a directory slot,
                    // not a regular-file slot" — the same code path
                    // `mkdir /.skillfs-inbox/<skill>` would have
                    // exercised against an existing dir.
                    libc::EISDIR
                };
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Create,
                    SkillEventAction::Rejected,
                    Some(errno),
                    None,
                );
                reply.error(errno);
                return;
            }
            PathType::InboxPassthrough { skill_name, .. } => {
                if !Self::is_inbox_skill_name_allowed(skill_name) {
                    self.emit_op_event(
                        req,
                        &path_type,
                        SkillEventKind::Create,
                        SkillEventAction::Rejected,
                        Some(libc::ENOENT),
                        None,
                    );
                    reply.error(libc::ENOENT);
                    return;
                }
            }
            _ => {}
        }

        // S3: refuse to create entries beneath a reserved lifecycle
        // namespace before any physical I/O.
        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Create, req, None)
        {
            reply.error(errno);
            return;
        }

        // S1: `.skill-meta/**` is mutation-protected. Reject before touching
        // the underlying filesystem so no partial state is left behind.
        if let Some(errno) = self.enforce_skill_meta(&path_type, SkillEventKind::Create, req, None)
        {
            reply.error(errno);
            return;
        }

        // I4/H3: reject create on hidden skills unless the path
        // matches the post-publish grace whitelist.
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
                reply.error(libc::ENOENT);
                return;
            }
        }

        let physical = match self.resolve_physical_path(&path_str) {
            Some(p) => p,
            None => {
                self.ro_warn("create", &path_str);
                reply.error(libc::EROFS);
                return;
            }
        };

        debug!(parent, name = %name.to_string_lossy(), ?physical, "create");

        // skill-discover namespace is read-only
        if let PathType::Passthrough { ref skill_name, .. } = path_type {
            if is_skill_discover_path(skill_name) {
                reply.error(libc::EROFS);
                return;
            }
        }

        // Build open options: reuse open_options_from_flags and add create semantics
        let mut opts = open_options_from_flags(flags);
        if (flags & libc::O_EXCL) != 0 {
            opts.create_new(true);
        } else {
            opts.create(true);
        }
        // Physical create requires write capability on the fd; however the handle's
        // flags preserve the original access mode requested by the caller (O_RDONLY).
        let access = flags & libc::O_ACCMODE;
        if access == libc::O_RDONLY {
            opts.write(true);
        }
        // POSIX: file permission bits of the new file shall be initialized
        // from mode and then masked by the process file-mode creation mask.
        // The FUSE protocol passes both the requested mode and the caller's
        // umask, so we apply them here rather than letting the FUSE daemon's
        // own umask (typically 0o022) shadow the caller's intent.
        let effective_mode = mode & !umask & 0o7777;
        opts.mode(effective_mode);
        let file_result = match opts.open(&physical) {
            Ok(f) => Ok(f),
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                // Long-path fallback (see mkdir for the same pattern). We
                // re-derive the open flags from the requested access so the
                // *at syscall behaves identically to the OpenOptions path.
                match self.open_parent_dir_for(&path_str) {
                    Ok((parent_fd, leaf)) => {
                        let mut creat_flags = flags;
                        if (creat_flags & libc::O_EXCL) != 0 {
                            // openat respects O_EXCL natively when O_CREAT is set
                        }
                        creat_flags |= libc::O_CREAT;
                        // Mirror the OpenOptions tweak above: read-only opens
                        // still need write capability to create the file.
                        let access = creat_flags & libc::O_ACCMODE;
                        if access == libc::O_RDONLY {
                            creat_flags = (creat_flags & !libc::O_ACCMODE) | libc::O_RDWR;
                        }
                        openat_leaf(&parent_fd, &leaf, creat_flags, effective_mode)
                    }
                    Err(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        };

        match file_result {
            Ok(file) => {
                let ino = self
                    .inodes
                    .allocate(&path_str, FileType::RegularFile, parent);
                self.inodes.remember(ino);
                // Pull metadata directly off the freshly opened fd so this
                // path works even when the absolute physical path exceeds
                // PATH_MAX (where `std::fs::metadata(&physical)` would fail
                // with ENAMETOOLONG even though the create itself just
                // succeeded via openat).
                let attr = match file.metadata() {
                    Ok(meta) => {
                        let mut a = file_attr_from_metadata(&meta);
                        a.ino = ino;
                        a
                    }
                    Err(_) => {
                        let mut a = self.virtual_file_attr(0);
                        a.ino = ino;
                        a
                    }
                };
                let fh = self.handles.allocate(ino, flags, Some(file), None);

                // Trigger re-parse if creating a SKILL.md (either
                // through `/skills/<skill>` or the L1 inbox; both
                // share the physical source candidate dir, so the
                // store has to learn about the new manifest either
                // way).
                if let PathType::SkillMd { skill_name } = &path_type {
                    self.send_sync(SyncEvent::Reparse {
                        skill_name: skill_name.clone(),
                        source_path: physical.clone(),
                    });
                }
                if let PathType::InboxPassthrough {
                    skill_name,
                    relative_path,
                } = &path_type
                {
                    if relative_path == Path::new("SKILL.md") {
                        self.send_sync(SyncEvent::Reparse {
                            skill_name: skill_name.clone(),
                            source_path: physical.clone(),
                        });
                    }
                }

                // D1.3-demo: a freshly-created SKILL.md or passthrough
                // file inside a skill should re-run resolve. New
                // skills come in through `mkdir` of the top-level
                // directory; here we only see leaf creation.
                //
                // L1: an inbox-side `create` observes the candidate
                // skill *only* when the leaf is the install-complete
                // sentinel — multi-file installs would otherwise
                // re-trigger scan/resolve dozens of times before the
                // installer is done writing.
                match &path_type {
                    PathType::SkillMd { skill_name } => self.observe_mutation(
                        skill_name,
                        Some(Path::new("SKILL.md")),
                        MutationKind::Create,
                    ),
                    PathType::Passthrough {
                        skill_name,
                        relative_path,
                    } => self.observe_mutation(
                        skill_name,
                        Some(relative_path.as_path()),
                        MutationKind::Create,
                    ),
                    PathType::InboxPassthrough {
                        skill_name,
                        relative_path,
                    } => self.inbox_observe_install_complete(
                        skill_name,
                        relative_path.as_path(),
                        MutationKind::Create,
                    ),
                    PathType::NestedSkillMd {
                        category,
                        skill_name,
                    } => {
                        let nested_id = Self::hermes_skill_id(category, skill_name);
                        self.observe_mutation(
                            &nested_id,
                            Some(Path::new("SKILL.md")),
                            MutationKind::Create,
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
                            MutationKind::Create,
                        );
                    }
                    _ => {}
                }

                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Create,
                    SkillEventAction::Allowed,
                    None,
                    None,
                );
                reply.created(&Duration::from_secs(1), &attr, 0, fh, 0);
            }
            Err(e) => {
                warn!(op = "create", path = %path_str, error = %e, "create failed");
                let err = errno(&e);
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Create,
                    SkillEventAction::Failed,
                    Some(err),
                    None,
                );
                reply.error(err);
            }
        }
    }
    pub(in crate::fs) fn mknod_impl(
        &mut self,
        req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        umask: u32,
        _rdev: u32,
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

        // T2 mknod policy: FIFO is the only special file SkillFS creates.
        // Sockets, block/char devices, and any other S_IFMT bit are
        // rejected with `EPERM` — matching the deterministic Linux errno
        // an unprivileged caller would see, and giving auditors a clear
        // signal that the request was a policy denial rather than an
        // unimplemented surface (`ENOSYS`) or a real `EROFS`. Regular
        // files come through `create()` in normal Linux FUSE clients;
        // an `S_IFREG` mknod here is therefore unexpected and refused
        // through the same `EPERM` path.
        let file_type_bits = mode & libc::S_IFMT;
        if file_type_bits != libc::S_IFIFO {
            warn!(
                op = "mknod",
                path = %path_str,
                file_type = format!("0o{:o}", file_type_bits),
                "non-FIFO mknod rejected by policy"
            );
            self.emit_op_event(
                req,
                &path_type,
                SkillEventKind::Create,
                SkillEventAction::Rejected,
                Some(libc::EPERM),
                None,
            );
            reply.error(libc::EPERM);
            return;
        }

        // Only Passthrough leaves under an ordinary skill can host a
        // freshly created FIFO. Virtual paths (Root, SkillsDir, SkillDir,
        // SkillMd, Invalid) are rejected before any physical I/O.
        let (skill_name, _relative_path) = match &path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => (skill_name.clone(), relative_path.clone()),
            _ => {
                self.ro_warn("mknod", &path_str);
                self.emit_op_event(
                    req,
                    &path_type,
                    SkillEventKind::Create,
                    SkillEventAction::Rejected,
                    Some(libc::EROFS),
                    None,
                );
                reply.error(libc::EROFS);
                return;
            }
        };

        if is_skill_discover_path(&skill_name) {
            self.emit_op_event(
                req,
                &path_type,
                SkillEventKind::Create,
                SkillEventAction::Rejected,
                Some(libc::EROFS),
                None,
            );
            reply.error(libc::EROFS);
            return;
        }

        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Create, req, None)
        {
            reply.error(errno);
            return;
        }
        if let Some(errno) = self.enforce_skill_meta(&path_type, SkillEventKind::Create, req, None)
        {
            reply.error(errno);
            return;
        }

        let physical = match self.resolve_physical_path(&path_str) {
            Some(p) => p,
            None => {
                self.ro_warn("mknod", &path_str);
                reply.error(libc::EROFS);
                return;
            }
        };

        let effective_mode = mode & !umask & 0o7777;
        use std::os::unix::ffi::OsStrExt as _;
        let c_path = match std::ffi::CString::new(physical.as_os_str().as_bytes()) {
            Ok(p) => p,
            Err(_) => {
                reply.error(libc::EINVAL);
                return;
            }
        };
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), effective_mode as libc::mode_t) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            let err = errno(&e);
            warn!(op = "mknod", path = %path_str, error = %e, "mkfifo failed");
            self.emit_op_event(
                req,
                &path_type,
                SkillEventKind::Create,
                SkillEventAction::Failed,
                Some(err),
                None,
            );
            reply.error(err);
            return;
        }

        let ino = self.inodes.allocate(&path_str, FileType::NamedPipe, parent);
        self.inodes.remember(ino);
        let attr = match std::fs::symlink_metadata(&physical) {
            Ok(meta) => {
                let mut a = file_attr_from_metadata(&meta);
                a.ino = ino;
                a
            }
            Err(_) => {
                let mut a = self.virtual_file_attr(0);
                a.kind = FileType::NamedPipe;
                a.ino = ino;
                a
            }
        };
        match &path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => {
                self.observe_mutation(
                    skill_name,
                    Some(relative_path.as_path()),
                    MutationKind::Create,
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
                    MutationKind::Create,
                );
            }
            _ => {}
        }
        self.emit_op_event(
            req,
            &path_type,
            SkillEventKind::Create,
            SkillEventAction::Allowed,
            None,
            None,
        );
        reply.entry(&Duration::from_secs(1), &attr, 0);
    }
    pub(in crate::fs) fn setattr_impl(
        &mut self,
        req: &Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        // NOTE: Permission enforcement for setattr mutations relies on the underlying
        // filesystem (kernel) rather than checking req.uid()/req.gid() in userspace.
        // This is acceptable for single-user FUSE mounts but may deviate from caller's
        // POSIX permission expectations under allow_other or privileged daemon scenarios.
        // Full per-caller permission emulation would require reimplementing the kernel's
        // permission model, which is deferred to a future hardening pass. The S1
        // `.skill-meta` policy still uses `req` for caller attribution in
        // `PolicyDenied` events.

        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let path_type = self.parse_fuse_path(Path::new(&path));

        // Determine whether any mutation is requested.
        let has_mutation = size.is_some()
            || mode.is_some()
            || uid.is_some()
            || gid.is_some()
            || atime.is_some()
            || mtime.is_some();

        // Virtual paths: Root, SkillsDir, SkillDir, skill-discover
        match &path_type {
            PathType::Root | PathType::SkillsDir | PathType::InboxDir => {
                if has_mutation {
                    reply.error(libc::EROFS);
                } else {
                    reply.attr(&Duration::from_secs(1), &self.dir_attr());
                }
                return;
            }
            PathType::SkillDir { skill_name } => {
                // Pure stat keeps the virtual directory façade.
                if !has_mutation {
                    reply.attr(&Duration::from_secs(1), &self.dir_attr());
                    return;
                }
                // `cp -a` preserves mode/timestamps on the freshly-created
                // top-level skill directory. Route those metadata mutations
                // to the physical source dir instead of rejecting with
                // EROFS. Pending installs are the CLI's direct final-skill
                // authoring path, so they must preserve metadata too. Keep
                // the read-only façade for the always-virtual skill-discover
                // and staging roots; hide hidden skills; lifecycle reserved
                // names are rejected by the shared gate below.
                // `.skill-meta/**` never parses as a SkillDir, so
                // trusted-writer policy is unaffected.
                if is_skill_discover_path(skill_name) || self.is_staging_skill_root(skill_name) {
                    reply.error(libc::EROFS);
                    return;
                }
                if self.should_reject_hidden_write(skill_name, None) {
                    reply.error(libc::ENOENT);
                    return;
                }
                // A directory has no size to truncate.
                if size.is_some() {
                    reply.error(libc::EISDIR);
                    return;
                }
                // Ownership changes on the virtual skill directory stay
                // restricted to privileged / trusted-writer callers. cp -a
                // from an ordinary user only needs mode/atime/mtime
                // preservation, so we do not widen the daemon-side chown
                // surface for unprivileged callers.
                if (uid.is_some() || gid.is_some())
                    && req.uid() != 0
                    && !self.evaluate_trusted_writer(req).is_allowed()
                {
                    reply.error(libc::EPERM);
                    return;
                }
                // Fall through to the shared physical setattr handling.
            }
            PathType::InboxSkillDir { skill_name } => {
                // L1: the inbox skill candidate dir is the live source
                // dir, so metadata reads project the physical dir's
                // attrs and mutations are rejected with EROFS to match
                // the existing `SkillDir` behavior. Lifecycle / xattr
                // mutation policy stays unchanged.
                if !Self::is_inbox_skill_name_allowed(skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                if has_mutation {
                    reply.error(libc::EROFS);
                } else {
                    let physical = self.inbox_skill_dir(skill_name);
                    match std::fs::symlink_metadata(&physical) {
                        Ok(meta) => {
                            let mut attr = file_attr_from_metadata(&meta);
                            attr.ino = ino;
                            reply.attr(&Duration::from_secs(1), &attr);
                        }
                        Err(e) => reply.error(errno(&e)),
                    }
                }
                return;
            }
            PathType::SkillMd { skill_name } | PathType::Passthrough { skill_name, .. } => {
                if is_skill_discover_path(skill_name) {
                    if has_mutation {
                        reply.error(libc::EROFS);
                    } else {
                        // Return virtual file attr for skill-discover
                        match self.compiled_skill_md(skill_name) {
                            Some(compiled) => {
                                let attr = self.virtual_file_attr(compiled.len() as u64);
                                reply.attr(&Duration::from_secs(1), &attr);
                            }
                            None => reply.error(libc::ENOENT),
                        }
                    }
                    return;
                }
                // Non skill-discover: fall through to physical mutation
            }
            PathType::InboxPassthrough { skill_name, .. } => {
                if !Self::is_inbox_skill_name_allowed(skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                // Fall through to physical mutation; lifecycle and
                // `.skill-meta` gates run below.
            }
            PathType::HermesMeta { .. }
            | PathType::HermesMetaChild { .. }
            | PathType::CategoryPassthrough { .. }
            | PathType::CategoryDir { .. }
            | PathType::NestedSkillDir { .. }
            | PathType::NestedSkillMd { .. }
            | PathType::NestedPassthrough { .. } => {
                // Hermes paths fall through to physical mutation.
            }
            PathType::Invalid => {
                reply.error(libc::ENOENT);
                return;
            }
        }

        // S3: deny metadata mutations on a reserved lifecycle namespace.
        // SkillDir is already rejected with EROFS above; this gate covers
        // SkillMd and Passthrough paths whose top-level segment matches a
        // reserved name.
        if has_mutation {
            if let Some(errno) =
                self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Metadata, req, None)
            {
                reply.error(errno);
                return;
            }
        }

        // S1: deny chmod/chown/utimens/truncate-size on `.skill-meta/**`.
        // Pure stat (no mutation requested) still succeeds via the physical
        // metadata fall-through below.
        if has_mutation {
            if let Some(errno) =
                self.enforce_skill_meta(&path_type, SkillEventKind::Metadata, req, None)
            {
                reply.error(errno);
                return;
            }
        }

        // I4/H3: reject setattr mutations on hidden skills unless
        // the path matches the post-publish grace whitelist.
        if has_mutation {
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
                reply.error(libc::ENOENT);
                return;
            }
        }

        // Physical path handling
        let physical = match self.resolve_physical_path(&path) {
            Some(p) => p,
            None => {
                reply.error(libc::EROFS);
                return;
            }
        };

        debug!(ino, ?size, ?mode, ?uid, ?gid, ?physical, "setattr");

        // 1. Handle size (truncate) — preserve existing logic
        if let Some(new_size) = size {
            let open_result = match std::fs::OpenOptions::new().write(true).open(&physical) {
                Ok(f) => Ok(f),
                Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                    match self.open_parent_dir_for(&path) {
                        Ok((parent_fd, leaf)) => openat_leaf(&parent_fd, &leaf, libc::O_WRONLY, 0),
                        Err(_) => Err(e),
                    }
                }
                Err(e) => Err(e),
            };
            match open_result {
                Ok(f) => {
                    if let Err(e) = f.set_len(new_size) {
                        reply.error(errno(&e));
                        return;
                    }
                    // SKILL.md truncate triggers store reparse
                    if let PathType::SkillMd { ref skill_name } = path_type {
                        self.send_sync(SyncEvent::Reparse {
                            skill_name: skill_name.clone(),
                            source_path: physical.clone(),
                        });
                    }
                    // D1.3-demo: truncate is the only setattr
                    // mutation that materially changes file content,
                    // so it is the only one we propagate to the
                    // refresh controller. mode/uid/gid/atime/mtime
                    // changes are intentionally ignored.
                    //
                    // L1: inbox truncates only enqueue when the leaf
                    // is the install-complete sentinel.
                    match &path_type {
                        PathType::SkillMd { skill_name } => self.observe_mutation(
                            skill_name,
                            Some(Path::new("SKILL.md")),
                            MutationKind::SetattrTruncate,
                        ),
                        PathType::Passthrough {
                            skill_name,
                            relative_path,
                        } => self.observe_mutation(
                            skill_name,
                            Some(relative_path.as_path()),
                            MutationKind::SetattrTruncate,
                        ),
                        PathType::InboxPassthrough {
                            skill_name,
                            relative_path,
                        } => self.inbox_observe_install_complete(
                            skill_name,
                            relative_path.as_path(),
                            MutationKind::SetattrTruncate,
                        ),
                        PathType::NestedSkillMd {
                            category,
                            skill_name,
                        } => {
                            let nested_id = Self::hermes_skill_id(category, skill_name);
                            self.observe_mutation(
                                &nested_id,
                                Some(Path::new("SKILL.md")),
                                MutationKind::SetattrTruncate,
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
                                MutationKind::SetattrTruncate,
                            );
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    reply.error(errno(&e));
                    return;
                }
            }
        }

        // 2. Handle mode (chmod)
        if let Some(new_mode) = mode {
            let perms = std::fs::Permissions::from_mode(new_mode);
            if let Err(e) = std::fs::set_permissions(&physical, perms) {
                reply.error(errno(&e));
                return;
            }
        }

        // 3. Handle uid/gid (chown)
        if uid.is_some() || gid.is_some() {
            let c_path = match std::ffi::CString::new(physical.to_string_lossy().into_owned()) {
                Ok(p) => p,
                Err(_) => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };
            // -1 means "don't change" — on Linux (uid_t)-1 == u32::MAX
            let new_uid = uid.map(|u| u as libc::uid_t).unwrap_or(u32::MAX);
            let new_gid = gid.map(|g| g as libc::gid_t).unwrap_or(u32::MAX);
            let ret = unsafe { libc::chown(c_path.as_ptr(), new_uid, new_gid) };
            if ret != 0 {
                let e = std::io::Error::last_os_error();
                reply.error(errno(&e));
                return;
            }
        }

        // 4. Handle atime/mtime (utimensat)
        if atime.is_some() || mtime.is_some() {
            let c_path = match std::ffi::CString::new(physical.to_string_lossy().into_owned()) {
                Ok(p) => p,
                Err(_) => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };

            let atime_spec = match atime {
                Some(fuser::TimeOrNow::Now) => libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_NOW,
                },
                Some(fuser::TimeOrNow::SpecificTime(t)) => {
                    match t.duration_since(UNIX_EPOCH) {
                        Ok(d) => libc::timespec {
                            tv_sec: d.as_secs() as i64,
                            tv_nsec: d.subsec_nanos() as i64,
                        },
                        Err(e) => {
                            // Pre-epoch time: negative seconds
                            let d = e.duration();
                            let mut sec = -(d.as_secs() as i64);
                            let mut nsec = -(d.subsec_nanos() as i64);
                            // Normalize: nsec should be non-negative for timespec
                            if nsec < 0 {
                                sec -= 1;
                                nsec += 1_000_000_000;
                            }
                            libc::timespec {
                                tv_sec: sec,
                                tv_nsec: nsec,
                            }
                        }
                    }
                }
                None => libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
            };

            let mtime_spec = match mtime {
                Some(fuser::TimeOrNow::Now) => libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_NOW,
                },
                Some(fuser::TimeOrNow::SpecificTime(t)) => {
                    match t.duration_since(UNIX_EPOCH) {
                        Ok(d) => libc::timespec {
                            tv_sec: d.as_secs() as i64,
                            tv_nsec: d.subsec_nanos() as i64,
                        },
                        Err(e) => {
                            // Pre-epoch time: negative seconds
                            let d = e.duration();
                            let mut sec = -(d.as_secs() as i64);
                            let mut nsec = -(d.subsec_nanos() as i64);
                            // Normalize: nsec should be non-negative for timespec
                            if nsec < 0 {
                                sec -= 1;
                                nsec += 1_000_000_000;
                            }
                            libc::timespec {
                                tv_sec: sec,
                                tv_nsec: nsec,
                            }
                        }
                    }
                }
                None => libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
            };

            let times = [atime_spec, mtime_spec];
            let ret =
                unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
            if ret != 0 {
                let e = std::io::Error::last_os_error();
                reply.error(errno(&e));
                return;
            }
        }

        // 5. Return updated attributes. Long-path fallback mirrors the
        // truncate branch above: if the absolute physical path exceeds
        // `PATH_MAX`, refetch via `fstatat` against the parent fd.
        // Without this, a successful truncate (which already changed the
        // on-disk size via openat fallback) would still reply with
        // `ENAMETOOLONG` here, and the kernel would surface that errno to
        // the caller while keeping the stale attr cache (`stat` after the
        // failed reply would still report the pre-truncate size).
        let final_attr: std::io::Result<FileAttr> = match std::fs::metadata(&physical) {
            Ok(meta) => Ok(file_attr_from_metadata(&meta)),
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                match self.open_parent_dir_for(&path) {
                    Ok((parent_fd, leaf)) => match fstatat_leaf(&parent_fd, &leaf, true) {
                        Ok(st) => Ok(file_attr_from_stat(&st)),
                        Err(e2) => Err(e2),
                    },
                    Err(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        match final_attr {
            Ok(mut attr) => {
                attr.ino = ino;
                // For SKILL.md, override size with compiled content length (consistent with getattr)
                if let PathType::SkillMd { ref skill_name } = path_type {
                    if let Some(compiled) = self.compiled_skill_md(skill_name) {
                        attr.size = compiled.len() as u64;
                    }
                }
                if has_mutation {
                    self.emit_op_event(
                        req,
                        &path_type,
                        SkillEventKind::Metadata,
                        SkillEventAction::Allowed,
                        None,
                        size,
                    );
                }
                reply.attr(&Duration::from_secs(1), &attr);
            }
            Err(e) => {
                let err = errno(&e);
                if has_mutation {
                    self.emit_op_event(
                        req,
                        &path_type,
                        SkillEventKind::Metadata,
                        SkillEventAction::Failed,
                        Some(err),
                        None,
                    );
                }
                reply.error(err);
            }
        }
    }
}
