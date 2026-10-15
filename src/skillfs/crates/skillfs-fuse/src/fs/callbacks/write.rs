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
use crate::sys::{errno, fchownat_leaf, fstatat_leaf, openat_leaf, utimensat_leaf};

/// Convert a FUSE `TimeOrNow` into the `timespec` the kernel expects;
/// `None` becomes `UTIME_OMIT`. Pre-epoch instants are normalized so
/// `tv_nsec` stays non-negative.
fn timespec_from_time_or_now(value: Option<fuser::TimeOrNow>) -> libc::timespec {
    match value {
        Some(fuser::TimeOrNow::Now) => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
        Some(fuser::TimeOrNow::SpecificTime(t)) => match t.duration_since(UNIX_EPOCH) {
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
        },
        None => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
    }
}

/// Clone the fd backing an unlinked inode's setattr out of the handle
/// table: the request's `fh` when the kernel attaches one, otherwise any
/// live handle for the inode (the kernel does not attach the handle to
/// every fd-based setattr — `fchmod(2)` sends mode without `FATTR_FH` —
/// and POSIX keeps the inode alive until the last close). A failed clone
/// keeps its own errno (EMFILE/ENFILE, ...) instead of collapsing into
/// the ENOENT "no handle" reply; `Ok(None)` means no handle at all.
fn clone_unlinked_handle_file(
    handles: &crate::handles::HandleManager,
    fh: Option<u64>,
    ino: u64,
) -> std::io::Result<Option<std::fs::File>> {
    clone_unlinked_handle_file_with(handles, fh, ino, std::fs::File::try_clone)
}

/// `clone_unlinked_handle_file` with the fd copy itself injected, so a test
/// can exercise the error mapping without having to exhaust the process's
/// real descriptor table.
fn clone_unlinked_handle_file_with(
    handles: &crate::handles::HandleManager,
    fh: Option<u64>,
    ino: u64,
    clone: impl Fn(&std::fs::File) -> std::io::Result<std::fs::File>,
) -> std::io::Result<Option<std::fs::File>> {
    let cloned = fh
        .and_then(|fh| handles.with_handle(fh, |entry| entry.file.as_ref().map(&clone)))
        .flatten()
        .or_else(|| handles.with_handle_for_ino(ino, |f| clone(f)));
    match cloned {
        Some(result) => result.map(Some),
        None => Ok(None),
    }
}

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

        // I4/H3: hidden-skill write gate. An fd opened while the skill
        // resolved `current` keeps its inode -> path mapping after the
        // ledger flips the skill to `hidden`, so the write dispatches
        // with a live mapping and must be refused here exactly like
        // every other mutating callback — and like the #5183 xattr
        // gate, which rejects `fsetxattr` on the very same stale fd
        // with ENOENT. Without this arm the fd kept a mutation channel
        // into the hidden skill's live source. The open-after-unlink
        // branch above deliberately skips this gate: POSIX keeps a raw
        // fd writable until last close, and the unlink already passed
        // the protection gates when it dropped the mapping.
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
                // Audit the rejection with the `hidden_skill` class the
                // xattr gate established (#5183): a stale-fd write probe
                // against a hidden skill must leave the same
                // `Rejected` trace as `fsetxattr` on the same fd.
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Write,
                    SkillEventAction::Rejected,
                    Some(libc::ENOENT),
                    None,
                    Some("class=hidden_skill".to_string()),
                );
                reply.error(libc::ENOENT);
                return;
            }
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
                        max_skill_size: self.max_skill_size,
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
                            max_skill_size: self.max_skill_size,
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
        // namespace before any physical I/O — and before the virtual
        // slot rejection below, so `create /skills/.staging` keeps the
        // historical `EACCES` + `PolicyDenied` audit (and the
        // policy_denied metric) instead of the generic virtual-slot
        // `EROFS`/`Create` record.
        if let Some(errno) =
            self.enforce_lifecycle_reservation(&path_type, SkillEventKind::Create, req, None)
        {
            reply.error(errno);
            return;
        }

        // Virtual-path type confusion: only file-capable leaves may
        // host a freshly created file — passthrough leaves, the
        // `SKILL.md` manifest slots, and the Hermes passthrough labels.
        // Virtual directory slots (Root, SkillsDir, SkillDir,
        // CategoryDir, Invalid) resolve onto `source/<name>` and would
        // materialize a plain regular file that `create` reports as a
        // RegularFile while later lookup/getattr answer ENOENT/Directory
        // — the same confusion `mknod` and `symlink` reject with EROFS.
        // `NestedSkillDir` stays allowed: a depth-2 name that does not
        // exist yet is lexically a nested-skill dir, but creating a
        // plain file there (`apple/README.md`) is the ordinary new
        // category-file flow, and once created the child re-parses as
        // `CategoryPassthrough` so lookups agree with the created type.
        match &path_type {
            PathType::SkillMd { .. }
            | PathType::Passthrough { .. }
            | PathType::NestedSkillDir { .. }
            | PathType::NestedSkillMd { .. }
            | PathType::NestedPassthrough { .. }
            | PathType::HermesMeta { .. }
            | PathType::HermesMetaChild { .. }
            | PathType::CategoryPassthrough { .. }
            | PathType::InboxPassthrough { .. } => {}
            _ => {
                self.ro_warn("create", &path_str);
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Create,
                    SkillEventAction::Rejected,
                    Some(libc::EROFS),
                    None,
                    Some(format!("class=virtual_dir_slot path={path_str}")),
                );
                reply.error(libc::EROFS);
                return;
            }
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
                self.ro_warn("create", &path_str);
                reply.error(libc::EROFS);
                return;
            }
        };

        debug!(parent, name = %name.to_string_lossy(), ?physical, "create");

        // skill-discover namespace is read-only. Emit the
        // `Rejected`/`EROFS` record the `symlink`/`link` gates
        // (link.rs) already leave for the same namespace so `create`
        // probes against the read-only tree are audited too.
        if let PathType::Passthrough { ref skill_name, .. } = path_type {
            if is_skill_discover_path(skill_name) {
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Create,
                    SkillEventAction::Rejected,
                    Some(libc::EROFS),
                    None,
                    Some("class=skill_discover".to_string()),
                );
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
                        max_skill_size: self.max_skill_size,
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
                            max_skill_size: self.max_skill_size,
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

        // Passthrough leaves under an ordinary skill — flat or Hermes
        // nested — can host a freshly created FIFO. Virtual paths (Root,
        // SkillsDir, SkillDir, SkillMd, Invalid) are rejected before any
        // physical I/O. The nested case is identified by its hermes id so
        // the skill-discover and observe paths below stay unchanged.
        let (skill_name, _relative_path) = match &path_type {
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => (skill_name.clone(), relative_path.clone()),
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } => (
                Self::hermes_skill_id(category, skill_name),
                relative_path.clone(),
            ),
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

        // I4/H3: reject FIFO creation inside a hidden skill unless the
        // path matches the post-publish grace whitelist — the same gate
        // `create`/`symlink`/`link` apply. The kernel keeps the skill
        // directory's dentries warm across a ledger flip, so without
        // this arm a stale dentry let `mkfifo` inject a new entry into a
        // hidden skill whose content is otherwise unreachable.
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
    /// Apply a setattr request through an open file handle whose inode no
    /// longer has a path mapping (the file was unlinked). The kernel's fd
    /// keeps the inode alive, so ftruncate/fchmod/fchown/futimens must work.
    #[allow(clippy::too_many_arguments)]
    fn setattr_unlinked_handle(
        &self,
        ino: u64,
        fh: Option<u64>,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        reply: ReplyAttr,
    ) {
        use std::os::unix::io::AsRawFd;

        let file = match clone_unlinked_handle_file(&self.handles, fh, ino) {
            Ok(Some(file)) => file,
            // Captured virtual content has no fd to fall back to; the inode
            // is gone, exactly as before.
            Ok(None) => {
                reply.error(libc::ENOENT);
                return;
            }
            // A failed fd copy (EMFILE/ENFILE, ...) means the handle is
            // still alive but the fd could not be borrowed; report its own
            // errno instead of the misleading ENOENT.
            Err(e) => {
                reply.error(errno(&e));
                return;
            }
        };
        let fd = file.as_raw_fd();

        if let Some(new_size) = size {
            if unsafe { libc::ftruncate(fd, new_size as libc::off_t) } != 0 {
                reply.error(errno(&std::io::Error::last_os_error()));
                return;
            }
        }
        if let Some(new_mode) = mode {
            if unsafe { libc::fchmod(fd, new_mode & 0o7777) } != 0 {
                reply.error(errno(&std::io::Error::last_os_error()));
                return;
            }
        }
        if uid.is_some() || gid.is_some() {
            let new_uid = uid.map(|u| u as libc::uid_t).unwrap_or(u32::MAX);
            let new_gid = gid.map(|g| g as libc::gid_t).unwrap_or(u32::MAX);
            if unsafe { libc::fchown(fd, new_uid, new_gid) } != 0 {
                reply.error(errno(&std::io::Error::last_os_error()));
                return;
            }
        }
        if atime.is_some() || mtime.is_some() {
            let times = [
                timespec_from_time_or_now(atime),
                timespec_from_time_or_now(mtime),
            ];
            if unsafe { libc::futimens(fd, times.as_ptr()) } != 0 {
                reply.error(errno(&std::io::Error::last_os_error()));
                return;
            }
        }

        match file.metadata() {
            Ok(meta) => {
                let mut attr = file_attr_from_metadata(&meta);
                attr.ino = ino;
                reply.attr(&Duration::from_secs(1), &attr);
            }
            Err(e) => reply.error(errno(&e)),
        }
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
        fh: Option<u64>,
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
                // The inode was unlinked while an fd is still open. POSIX
                // keeps the inode alive until the last close, so mutations
                // through that fd must still work — use the handle's file
                // instead of failing with ENOENT.
                return self
                    .setattr_unlinked_handle(ino, fh, mode, uid, gid, size, atime, mtime, reply);
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
                    // Audit the rejection with the `hidden_skill` class the
                    // xattr gate established (#5183) so a hidden skill's
                    // metadata probes leave a trace — the same convention
                    // the other mutating callbacks follow.
                    self.emit_op_event_with_detail(
                        req,
                        &path_type,
                        SkillEventKind::Metadata,
                        SkillEventAction::Rejected,
                        Some(libc::ENOENT),
                        None,
                        Some("class=hidden_skill".to_string()),
                    );
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
                // Audit the rejection with the `hidden_skill` class the
                // xattr gate established (#5183) so a hidden skill's
                // chmod/chown/truncate/utimens probes leave a trace — the
                // same convention the other mutating callbacks follow.
                self.emit_op_event_with_detail(
                    req,
                    &path_type,
                    SkillEventKind::Metadata,
                    SkillEventAction::Rejected,
                    Some(libc::ENOENT),
                    None,
                    Some("class=hidden_skill".to_string()),
                );
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

        // A setattr delivered on a symlink inode comes from a no-follow
        // syscall (lchown / lutimes / fchmodat2 with AT_SYMLINK_NOFOLLOW).
        // chown(2) and utimensat(..., 0) would follow the link and mutate
        // whatever it points at — possibly outside the skill tree — instead
        // of the link itself. Detect the link once and use the no-follow
        // syscall/flag for every branch below. A physical path beyond
        // PATH_MAX defeats path-based typing; fall back to the parent-fd
        // leaf stat so such a file is still typed no-follow and routed
        // through the *at syscalls below, while any other stat error
        // keeps its errno.
        let long_path;
        let is_symlink = match std::fs::symlink_metadata(&physical) {
            Ok(m) => {
                long_path = false;
                m.file_type().is_symlink()
            }
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                match self.open_parent_dir_for(&path) {
                    Ok((parent_fd, leaf)) => match fstatat_leaf(&parent_fd, &leaf, false) {
                        Ok(st) => {
                            long_path = true;
                            st.st_mode & libc::S_IFMT == libc::S_IFLNK
                        }
                        Err(e2) => {
                            reply.error(errno(&e2));
                            return;
                        }
                    },
                    Err(_) => {
                        reply.error(errno(&e));
                        return;
                    }
                }
            }
            Err(e) => {
                reply.error(errno(&e));
                return;
            }
        };

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
                            max_skill_size: self.max_skill_size,
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
            // Linux does not support changing a symlink's own mode; only
            // fchmodat2(AT_SYMLINK_NOFOLLOW) delivers mode here for a link,
            // and the kernel answers EOPNOTSUPP for it. set_permissions
            // would silently chmod the target instead.
            if is_symlink {
                reply.error(libc::EOPNOTSUPP);
                return;
            }
            let perms = std::fs::Permissions::from_mode(new_mode);
            if let Err(e) = std::fs::set_permissions(&physical, perms) {
                reply.error(errno(&e));
                return;
            }
        }

        // 3. Handle uid/gid (chown)
        if uid.is_some() || gid.is_some() {
            // Raw OS bytes, not a lossy UTF-8 view: `chown` must address the
            // exact physical path even when it contains non-UTF-8 bytes.
            let c_path = match crate::sys::cstring_from_os_str(physical.as_os_str()) {
                Ok(p) => p,
                Err(_) => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };
            // -1 means "don't change" — on Linux (uid_t)-1 == u32::MAX
            let new_uid = uid.map(|u| u as libc::uid_t).unwrap_or(u32::MAX);
            let new_gid = gid.map(|g| g as libc::gid_t).unwrap_or(u32::MAX);
            let chown_result = if long_path {
                // chown(2)/lchown(2) cannot name a path beyond PATH_MAX;
                // reach the leaf through the open parent directory,
                // preserving the no-follow choice above.
                match self.open_parent_dir_for(&path) {
                    Ok((parent_fd, leaf)) => {
                        fchownat_leaf(&parent_fd, &leaf, new_uid, new_gid, !is_symlink).map(|_| 0)
                    }
                    Err(e) => Err(std::io::Error::from_raw_os_error(e)),
                }
            } else {
                let ret = if is_symlink {
                    unsafe { libc::lchown(c_path.as_ptr(), new_uid, new_gid) }
                } else {
                    unsafe { libc::chown(c_path.as_ptr(), new_uid, new_gid) }
                };
                if ret != 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(0)
                }
            };
            if let Err(e) = chown_result {
                reply.error(errno(&e));
                return;
            }
        }

        // 4. Handle atime/mtime (utimensat)
        if atime.is_some() || mtime.is_some() {
            // Same raw-byte path requirement as the chown branch above.
            let c_path = match crate::sys::cstring_from_os_str(physical.as_os_str()) {
                Ok(p) => p,
                Err(_) => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };

            let atime_spec = timespec_from_time_or_now(atime);
            let mtime_spec = timespec_from_time_or_now(mtime);

            let times = [atime_spec, mtime_spec];
            let utimensat_result = if long_path {
                // utimensat cannot name a path beyond PATH_MAX; reach the
                // leaf through the open parent directory, preserving the
                // no-follow choice.
                match self.open_parent_dir_for(&path) {
                    Ok((parent_fd, leaf)) => utimensat_leaf(&parent_fd, &leaf, &times, is_symlink),
                    Err(e) => Err(std::io::Error::from_raw_os_error(e)),
                }
            } else {
                // Same as chown above: times set through a no-follow syscall
                // belong to the link, not to its target.
                let flags = if is_symlink {
                    libc::AT_SYMLINK_NOFOLLOW
                } else {
                    0
                };
                let ret = unsafe {
                    libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), flags)
                };
                if ret != 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            };
            if let Err(e) = utimensat_result {
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
        let final_attr: std::io::Result<FileAttr> = match if is_symlink {
            std::fs::symlink_metadata(&physical)
        } else {
            std::fs::metadata(&physical)
        } {
            Ok(meta) => Ok(file_attr_from_metadata(&meta)),
            Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                match self.open_parent_dir_for(&path) {
                    Ok((parent_fd, leaf)) => match fstatat_leaf(&parent_fd, &leaf, !is_symlink) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fd that cannot be duplicated must surface its own errno from the
    /// setattr fd lookup, not be swallowed into the ENOENT "no handle"
    /// reply: the kernel handle is alive and the setattr syscalls would
    /// work given the fd.
    #[test]
    fn unlinked_setattr_clone_failure_keeps_its_errno() {
        let handles = crate::handles::HandleManager::new();
        let file = std::fs::File::open("/dev/null").expect("open /dev/null");
        let fh = handles.allocate(7, libc::O_RDWR, Some(file), None);

        // The injected copy fails the way an exhausted descriptor table
        // fails (EMFILE) while the handle itself stays usable; the error
        // must propagate verbatim. Injecting it keeps the test off the
        // process's real descriptor budget.
        let err = clone_unlinked_handle_file_with(&handles, Some(fh), 7, |_| {
            Err(std::io::Error::from_raw_os_error(libc::EMFILE))
        })
        .expect_err("a failed clone must surface its errno");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EMFILE),
            "the clone error must propagate verbatim, got {err:?}"
        );

        // No handle for the inode stays `Ok(None)`, which the caller maps
        // to ENOENT.
        assert!(
            clone_unlinked_handle_file(&handles, None, 999)
                .unwrap()
                .is_none()
        );
    }
}
