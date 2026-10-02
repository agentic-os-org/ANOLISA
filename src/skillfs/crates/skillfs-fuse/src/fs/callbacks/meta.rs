//! FUSE metadata callbacks: `lookup`, `getattr`, `access`, `statfs`.

use std::path::Path;
use std::time::Duration;

use fuser::{FUSE_ROOT_ID, FileType, ReplyAttr, ReplyEmpty, ReplyEntry, ReplyStatfs, Request};
use tracing::debug;

use super::super::SkillFs;
use super::super::paths::FlatLiveDirKind;
use super::super::read_resolution::ReadResolution;
use crate::attr::{file_attr_from_metadata, file_attr_from_stat};
use crate::path::{PathType, is_skill_discover_path};
use crate::security::{SkillEventKind, lifecycle::is_reserved_lifecycle_name};
use crate::sys::{errno, fstatat_leaf};

impl SkillFs {
    pub(in crate::fs) fn lookup_impl(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: ReplyEntry,
    ) {
        let name_str = name.to_string_lossy();
        debug!(parent, name = %name_str, "lookup");

        let parent_path = match self.inodes.get_path(parent) {
            Some(p) => p,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let path_str = if parent_path == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent_path, name_str)
        };
        let path = Path::new(&path_str);

        match self.parse_fuse_path(path) {
            PathType::Root => {
                let attr = self.dir_attr();
                self.inodes.remember(FUSE_ROOT_ID);
                reply.entry(&Duration::from_secs(1), &attr, 0);
            }
            PathType::SkillsDir => {
                // In-place mode: root acts as skills dir — return root attrs.
                let ino = if self.in_place {
                    FUSE_ROOT_ID
                } else {
                    self.inodes
                        .lookup_by_path(&path_str)
                        .unwrap_or(FUSE_ROOT_ID)
                };
                let mut attr = self.dir_attr();
                attr.ino = ino;
                self.inodes.remember(ino);
                reply.entry(&Duration::from_secs(1), &attr, 0);
            }
            PathType::SkillDir { skill_name } => {
                // S3: lifecycle namespaces are hidden from ordinary lookup.
                if is_reserved_lifecycle_name(&skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                // I2: staging roots are installer-private workspaces.
                // They are hidden from /skills readdir but must remain
                // accessible for lookup so installers can write inside
                // them through the FUSE mount. Check the physical dir
                // directly instead of the store.
                if let Some(ref matcher) = self.staging_matcher {
                    if matcher.is_staging_root(&skill_name) {
                        let physical = self.source_base().join(&skill_name);
                        if physical.is_dir() {
                            let ino = self.inodes.allocate(&path_str, FileType::Directory, parent);
                            self.inodes.remember(ino);
                            let mut attr = self.dir_attr();
                            attr.ino = ino;
                            reply.entry(&Duration::from_secs(1), &attr, 0);
                        } else {
                            reply.error(libc::ENOENT);
                        }
                        return;
                    }
                }
                // Pending installs bypass the active resolver so
                // installers can continue writing via exact path.
                if self.is_pending_install(&skill_name) {
                    let physical = self.source_base().join(&skill_name);
                    if physical.is_dir() {
                        let ino = self.inodes.allocate(&path_str, FileType::Directory, parent);
                        self.inodes.remember(ino);
                        let mut attr = self.dir_attr();
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    } else {
                        reply.error(libc::ENOENT);
                    }
                    return;
                }
                // D1.1: ledger-hidden skills surface as ENOENT on direct
                // probes, exactly as if the skill did not exist.
                // I4: post-publish grace bypasses the hidden gate so
                // installers can traverse the skill directory to reach
                // whitelisted files within the grace window.
                // Trusted writers can traverse hidden skill dirs so
                // exact-path `.skill-meta` access works.
                if matches!(self.resolve_skill_read(&skill_name), ReadResolution::Hidden)
                    && !self.is_post_publish_grace_allowed(&skill_name, None)
                    && !self.evaluate_trusted_writer(_req).is_allowed()
                {
                    // A direct lookup of a ledger-hidden skill is a real
                    // security denial (best-effort runtime metric).
                    self.metric_policy_denied();
                    reply.error(libc::ENOENT);
                    return;
                }
                let exists = skill_name == "skill-discover" || {
                    let store = self.store.read();
                    store.get(&skill_name).is_some()
                };
                if exists {
                    let ino = self.inodes.allocate(&path_str, FileType::Directory, parent);
                    self.inodes.remember(ino);
                    let mut attr = self.dir_attr();
                    attr.ino = ino;
                    reply.entry(&Duration::from_secs(1), &attr, 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }
            PathType::SkillMd { skill_name } => {
                // S3: lifecycle namespaces are hidden, including their
                // virtual `SKILL.md`. The boundary holds even if a caller
                // bypasses readdir and probes the path directly.
                if is_reserved_lifecycle_name(&skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                // I2: staging roots serve SKILL.md as a raw physical
                // file, not through the compiler/resolver.
                // Pending installs use the same raw physical path.
                if self.is_staging_skill_root(&skill_name) || self.is_pending_install(&skill_name) {
                    let physical = self.source_base().join(&skill_name).join("SKILL.md");
                    match std::fs::symlink_metadata(&physical) {
                        Ok(meta) => {
                            let ino =
                                self.inodes
                                    .allocate(&path_str, FileType::RegularFile, parent);
                            self.inodes.remember(ino);
                            let mut attr = file_attr_from_metadata(&meta);
                            attr.ino = ino;
                            reply.entry(&Duration::from_secs(1), &attr, 0);
                        }
                        Err(e) => reply.error(errno(&e)),
                    }
                    return;
                }
                match self.transformed_skill_attr(&skill_name, None) {
                    Some(mut attr) => {
                        let ino = self
                            .inodes
                            .allocate(&path_str, FileType::RegularFile, parent);
                        self.inodes.remember(ino);
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    }
                    None => reply.error(libc::ENOENT),
                }
            }
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => {
                // S3: lifecycle namespaces are hidden — every descendant
                // of a reserved root is treated as if it does not exist,
                // even if it is physically present in the source tree.
                if is_reserved_lifecycle_name(&skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                let pt = PathType::Passthrough {
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&pt, _req) {
                    Some(false) => {
                        reply.error(libc::ENOENT);
                        return;
                    }
                    Some(true) => {
                        let physical_path =
                            self.skill_physical_dir(&skill_name).join(&relative_path);
                        match std::fs::symlink_metadata(&physical_path) {
                            Ok(meta) => {
                                let mut attr = file_attr_from_metadata(&meta);
                                let ino = self.inodes.allocate(&path_str, attr.kind, parent);
                                self.inodes.remember(ino);
                                attr.ino = ino;
                                reply.entry(&Duration::from_secs(1), &attr, 0);
                            }
                            Err(e) => reply.error(errno(&e)),
                        }
                        return;
                    }
                    None => {}
                }
                // I2: staging roots use the physical source dir
                // directly, bypassing the active resolver.
                // Pending installs use the same bypass.
                // D1.1: hidden skills also hide their passthrough
                // descendants. For fallback skills we resolve against
                // the snapshot dir so the kernel sees the snapshot's
                // tree (sizes, types, file presence) — direct lookups
                // of files that exist only in the live source surface
                // as ENOENT, matching the contract that snapshots are
                // the read-side ground truth.
                let read_dir = if self.is_staging_skill_root(&skill_name)
                    || self.is_pending_install(&skill_name)
                {
                    self.source_base().join(&skill_name)
                } else {
                    match self.skill_read_dir(&skill_name) {
                        Some(d) => {
                            // I4: grace bypass — if the skill is on a
                            // fallback snapshot but the path matches the
                            // post-publish grace whitelist, resolve against
                            // physical source so the installer can
                            // create/stat files that don't exist in the
                            // snapshot.
                            if self.is_post_publish_grace_allowed(&skill_name, Some(&relative_path))
                            {
                                self.flat_live_dir(&skill_name, FlatLiveDirKind::ExistingSkill)
                            } else {
                                d
                            }
                        }
                        None => {
                            // I4: grace bypass — if the skill is hidden but
                            // the path matches the post-publish grace
                            // whitelist, resolve against physical source.
                            if self.is_post_publish_grace_allowed(&skill_name, Some(&relative_path))
                            {
                                self.flat_live_dir(&skill_name, FlatLiveDirKind::ExistingSkill)
                            } else {
                                reply.error(libc::ENOENT);
                                return;
                            }
                        }
                    }
                };
                let physical_path = read_dir.join(&relative_path);
                // Use symlink_metadata so a passthrough symlink is reported
                // with FileType::Symlink rather than collapsed onto its
                // target. The kernel issues readlink() afterwards if it
                // wants to follow the link.
                match std::fs::symlink_metadata(&physical_path) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        let ino = self.inodes.allocate(&path_str, attr.kind, parent);
                        self.inodes.remember(ino);
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    }
                    Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                        // Long-path fallback: the leaf's absolute path
                        // exceeds PATH_MAX so `symlink_metadata` can't
                        // resolve it, but the parent fits and `fstatat`
                        // with just the leaf component succeeds (or
                        // returns the real errno such as ENOENT/ENOTDIR).
                        match self.open_parent_dir_for(&path_str) {
                            Ok((parent_fd, leaf)) => match fstatat_leaf(&parent_fd, &leaf, false) {
                                Ok(st) => {
                                    let mut attr = file_attr_from_stat(&st);
                                    let ino = self.inodes.allocate(&path_str, attr.kind, parent);
                                    self.inodes.remember(ino);
                                    attr.ino = ino;
                                    reply.entry(&Duration::from_secs(1), &attr, 0);
                                }
                                Err(e2) => reply.error(errno(&e2)),
                            },
                            Err(_) => reply.error(errno(&e)),
                        }
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::InboxDir => {
                // Virtual inbox root. Always present, regardless of
                // active resolver state, so installers can repair
                // hidden / new skills even when `/skills` does not
                // expose them.
                let ino = self.inodes.lookup_by_path(&path_str).unwrap_or_else(|| {
                    self.inodes.allocate(&path_str, FileType::Directory, parent)
                });
                self.inodes.remember(ino);
                let mut attr = self.dir_attr();
                attr.ino = ino;
                reply.entry(&Duration::from_secs(1), &attr, 0);
            }
            PathType::InboxSkillDir { skill_name } => {
                if !Self::is_inbox_skill_name_allowed(&skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = self.inbox_skill_dir(&skill_name);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) if meta.is_dir() => {
                        let ino = self.inodes.allocate(&path_str, FileType::Directory, parent);
                        self.inodes.remember(ino);
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    }
                    Ok(_) => reply.error(libc::ENOTDIR),
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::InboxPassthrough {
                skill_name,
                relative_path,
            } => {
                if !Self::is_inbox_skill_name_allowed(&skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = self.inbox_skill_dir(&skill_name).join(&relative_path);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        let ino = self.inodes.allocate(&path_str, attr.kind, parent);
                        self.inodes.remember(ino);
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::HermesMeta { name } => {
                let physical = self.source_base().join(&name);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        let ino = self.inodes.allocate(&path_str, attr.kind, parent);
                        self.inodes.remember(ino);
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::HermesMetaChild {
                name,
                relative_path,
            }
            | PathType::CategoryPassthrough {
                name,
                relative_path,
            } => {
                let physical = self.source_base().join(&name).join(&relative_path);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        let ino = self.inodes.allocate(&path_str, attr.kind, parent);
                        self.inodes.remember(ino);
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::CategoryDir { category } => {
                let physical = self.source_base().join(&category);
                if physical.is_dir() {
                    let ino = self.inodes.allocate(&path_str, FileType::Directory, parent);
                    self.inodes.remember(ino);
                    let mut attr = self.dir_attr();
                    attr.ino = ino;
                    reply.entry(&Duration::from_secs(1), &attr, 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }
            PathType::NestedSkillDir {
                ref category,
                ref skill_name,
            } => {
                // H3: staging and pending install bypass for nested
                // skill lookup.
                let nested_id = Self::hermes_skill_id(category, skill_name);
                if !self.is_staging_skill_root(&nested_id)
                    && !self.is_pending_install(&nested_id)
                    && matches!(
                        self.resolve_hermes_nested_read(category, skill_name),
                        crate::fs::read_resolution::ReadResolution::Hidden
                    )
                    && !self.evaluate_trusted_writer(_req).is_allowed()
                {
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = self
                    .hermes_nested_skill_read_dir(category, skill_name)
                    .unwrap_or_else(|| self.source_base().join(category).join(skill_name));
                if physical.is_dir() {
                    let ino = self.inodes.allocate(&path_str, FileType::Directory, parent);
                    self.inodes.remember(ino);
                    let mut attr = self.dir_attr();
                    attr.ino = ino;
                    reply.entry(&Duration::from_secs(1), &attr, 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }
            PathType::NestedSkillMd {
                ref category,
                ref skill_name,
            } => {
                let nested_id = Self::hermes_skill_id(category, skill_name);
                // Staging / pending nested SKILL.md is a raw physical file.
                if self.is_staging_skill_root(&nested_id) || self.is_pending_install(&nested_id) {
                    let physical = self
                        .source_base()
                        .join(category)
                        .join(skill_name)
                        .join("SKILL.md");
                    match std::fs::symlink_metadata(&physical) {
                        Ok(meta) => {
                            let ino =
                                self.inodes
                                    .allocate(&path_str, FileType::RegularFile, parent);
                            self.inodes.remember(ino);
                            let mut attr = file_attr_from_metadata(&meta);
                            attr.ino = ino;
                            reply.entry(&Duration::from_secs(1), &attr, 0);
                        }
                        Err(e) => reply.error(errno(&e)),
                    }
                    return;
                }
                match self.transformed_skill_attr(skill_name, Some(category)) {
                    Some(mut attr) => {
                        let ino = self
                            .inodes
                            .allocate(&path_str, FileType::RegularFile, parent);
                        self.inodes.remember(ino);
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    }
                    None => reply.error(libc::ENOENT),
                }
            }
            PathType::NestedPassthrough {
                ref category,
                ref skill_name,
                ref relative_path,
            } => {
                let npt = PathType::NestedPassthrough {
                    category: category.clone(),
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&npt, _req) {
                    Some(false) => {
                        reply.error(libc::ENOENT);
                        return;
                    }
                    Some(true) => {
                        let physical_path = self
                            .hermes_skill_physical_dir(category, skill_name)
                            .join(relative_path);
                        match std::fs::symlink_metadata(&physical_path) {
                            Ok(meta) => {
                                let mut attr = file_attr_from_metadata(&meta);
                                let ino = self.inodes.allocate(&path_str, attr.kind, parent);
                                self.inodes.remember(ino);
                                attr.ino = ino;
                                reply.entry(&Duration::from_secs(1), &attr, 0);
                            }
                            Err(e) => reply.error(errno(&e)),
                        }
                        return;
                    }
                    None => {}
                }
                let nested_id_lu = Self::hermes_skill_id(category, skill_name);
                if !self.is_staging_skill_root(&nested_id_lu)
                    && !self.is_pending_install(&nested_id_lu)
                    && matches!(
                        self.resolve_hermes_nested_read(category, skill_name),
                        crate::fs::read_resolution::ReadResolution::Hidden
                    )
                {
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = match self.resolve_hermes_nested_read(category, skill_name) {
                    crate::fs::read_resolution::ReadResolution::Snapshot { dir, .. } => {
                        dir.join(relative_path)
                    }
                    _ => self
                        .source_base()
                        .join(category)
                        .join(skill_name)
                        .join(relative_path),
                };
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        let ino = self.inodes.allocate(&path_str, attr.kind, parent);
                        self.inodes.remember(ino);
                        attr.ino = ino;
                        reply.entry(&Duration::from_secs(1), &attr, 0);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::Invalid => reply.error(libc::ENOENT),
        }
    }
    pub(in crate::fs) fn getattr_impl(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: Option<u64>,
        reply: ReplyAttr,
    ) {
        debug!(ino, ?fh, "getattr");

        if ino == FUSE_ROOT_ID {
            reply.attr(&Duration::from_secs(1), &self.dir_attr());
            return;
        }

        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => {
                // Open-after-unlink fast path. The path mapping was torn
                // down by `unlink()` but the FUSE handle still references
                // a valid open fd. The kernel's `vfs_fstat` path does NOT
                // set `FUSE_GETATTR_FH`, so the `fh` argument is `None`
                // here even when the caller invoked `fstat` on an open
                // descriptor; we therefore scan handles by ino as well.
                // `file.metadata()` is `fstat(fd)` and works post-unlink,
                // so SKILL.md virtual-size handling is preserved by the
                // path-based branches below for any caller that still has
                // a live path mapping.
                let by_fh = fh.and_then(|h| {
                    self.handles
                        .with_handle(h, |entry| entry.file.as_ref().map(|f| f.metadata()))
                });
                let meta_result = match by_fh {
                    Some(Some(r)) => Some(r),
                    _ => self.handles.with_handle_for_ino(ino, |f| f.metadata()),
                };
                if let Some(meta_result) = meta_result {
                    match meta_result {
                        Ok(meta) => {
                            let mut attr = file_attr_from_metadata(&meta);
                            attr.ino = ino;
                            reply.attr(&Duration::from_secs(1), &attr);
                            return;
                        }
                        Err(e) => {
                            reply.error(errno(&e));
                            return;
                        }
                    }
                }
                reply.error(libc::ENOENT);
                return;
            }
        };

        match self.parse_fuse_path(Path::new(&path)) {
            PathType::Root | PathType::SkillsDir => {
                reply.attr(&Duration::from_secs(1), &self.dir_attr());
            }
            PathType::SkillDir { skill_name } => {
                // I2: staging roots are accessible for getattr so
                // installers can stat the directory during writes.
                // Pending installs use the same bypass.
                if let Some(ref matcher) = self.staging_matcher {
                    if matcher.is_staging_root(&skill_name) {
                        reply.attr(&Duration::from_secs(1), &self.dir_attr());
                        return;
                    }
                }
                if self.is_pending_install(&skill_name) {
                    reply.attr(&Duration::from_secs(1), &self.dir_attr());
                    return;
                }
                // I4: grace bypass for getattr on skill dir.
                // Trusted writers can stat hidden skill dirs for
                // `.skill-meta` exact-path traversal.
                if matches!(self.resolve_skill_read(&skill_name), ReadResolution::Hidden)
                    && !self.is_post_publish_grace_allowed(&skill_name, None)
                    && !self.evaluate_trusted_writer(_req).is_allowed()
                {
                    reply.error(libc::ENOENT);
                    return;
                }
                reply.attr(&Duration::from_secs(1), &self.dir_attr());
            }
            PathType::SkillMd { skill_name } => {
                // I2: staging roots serve SKILL.md as a raw physical file.
                // Pending installs use the same bypass.
                if self.is_staging_skill_root(&skill_name) || self.is_pending_install(&skill_name) {
                    let physical = self.source_base().join(&skill_name).join("SKILL.md");
                    match std::fs::metadata(&physical) {
                        Ok(meta) => {
                            let mut attr = file_attr_from_metadata(&meta);
                            attr.ino = ino;
                            reply.attr(&Duration::from_secs(1), &attr);
                        }
                        Err(e) => reply.error(errno(&e)),
                    }
                    return;
                }
                match self.transformed_skill_attr(&skill_name, None) {
                    Some(mut attr) => {
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    None => reply.error(libc::ENOENT),
                }
            }
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => {
                let pt = PathType::Passthrough {
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&pt, _req) {
                    Some(false) => {
                        reply.error(libc::ENOENT);
                        return;
                    }
                    Some(true) => {
                        let physical_path =
                            self.skill_physical_dir(&skill_name).join(&relative_path);
                        match std::fs::symlink_metadata(&physical_path) {
                            Ok(meta) => {
                                let mut attr = file_attr_from_metadata(&meta);
                                attr.ino = ino;
                                reply.attr(&Duration::from_secs(1), &attr);
                            }
                            Err(e) => reply.error(errno(&e)),
                        }
                        return;
                    }
                    None => {}
                }
                // I2: staging roots use the physical source dir
                // directly, bypassing the active resolver.
                // Pending installs use the same bypass.
                // D1.1: hidden / snapshot resolution. Hidden surfaces
                // ENOENT; snapshot redirects attr resolution to the
                // snapshot tree so size/type/mtime match the bytes the
                // kernel will receive on the next read.
                let read_dir = if self.is_staging_skill_root(&skill_name)
                    || self.is_pending_install(&skill_name)
                {
                    self.source_base().join(&skill_name)
                } else {
                    match self.skill_read_dir(&skill_name) {
                        Some(d) => {
                            if self.is_post_publish_grace_allowed(&skill_name, Some(&relative_path))
                            {
                                self.flat_live_dir(&skill_name, FlatLiveDirKind::ExistingSkill)
                            } else {
                                d
                            }
                        }
                        None => {
                            // I4: grace bypass for getattr on passthrough path.
                            if self.is_post_publish_grace_allowed(&skill_name, Some(&relative_path))
                            {
                                self.flat_live_dir(&skill_name, FlatLiveDirKind::ExistingSkill)
                            } else {
                                reply.error(libc::ENOENT);
                                return;
                            }
                        }
                    }
                };
                let physical_path = read_dir.join(&relative_path);
                // symlink_metadata preserves FileType::Symlink for passthrough
                // links; regular file/directory attrs remain unchanged.
                match std::fs::symlink_metadata(&physical_path) {
                    Ok(meta) => {
                        // file_attr_from_metadata sets ino=0; the kernel
                        // matches getattr to the inode it cached from
                        // lookup, so we must restore the SkillFS-allocated
                        // inode here to avoid (dev, ino) collisions that
                        // confuse tools like `rm -r` ("Circular directory
                        // structure" warnings).
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    Err(e) if e.raw_os_error() == Some(libc::ENAMETOOLONG) => {
                        // Long-path fallback: see `lookup` for the same
                        // pattern. We fstatat against the parent dir fd
                        // so the leaf is the only string the syscall
                        // sees.
                        match self.open_parent_dir_for(&path) {
                            Ok((parent_fd, leaf)) => match fstatat_leaf(&parent_fd, &leaf, false) {
                                Ok(st) => {
                                    let mut attr = file_attr_from_stat(&st);
                                    attr.ino = ino;
                                    reply.attr(&Duration::from_secs(1), &attr);
                                }
                                Err(e2) => reply.error(errno(&e2)),
                            },
                            Err(_) => reply.error(errno(&e)),
                        }
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::InboxDir => {
                reply.attr(&Duration::from_secs(1), &self.dir_attr());
            }
            PathType::InboxSkillDir { skill_name } => {
                if !Self::is_inbox_skill_name_allowed(&skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = self.inbox_skill_dir(&skill_name);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::InboxPassthrough {
                skill_name,
                relative_path,
            } => {
                if !Self::is_inbox_skill_name_allowed(&skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = self.inbox_skill_dir(&skill_name).join(&relative_path);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::HermesMeta { name } => {
                let physical = self.source_base().join(&name);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::HermesMetaChild {
                name,
                relative_path,
            }
            | PathType::CategoryPassthrough {
                name,
                relative_path,
            } => {
                let physical = self.source_base().join(&name).join(&relative_path);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::CategoryDir { category } => {
                let physical = self.source_base().join(&category);
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::NestedSkillDir {
                ref category,
                ref skill_name,
            } => {
                let nid = Self::hermes_skill_id(category, skill_name);
                if !self.is_staging_skill_root(&nid)
                    && !self.is_pending_install(&nid)
                    && matches!(
                        self.resolve_hermes_nested_read(category, skill_name),
                        crate::fs::read_resolution::ReadResolution::Hidden
                    )
                {
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = self
                    .hermes_nested_skill_read_dir(category, skill_name)
                    .unwrap_or_else(|| self.source_base().join(category).join(skill_name));
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::NestedSkillMd {
                ref category,
                ref skill_name,
            } => {
                let nid = Self::hermes_skill_id(category, skill_name);
                // Staging / pending nested SKILL.md is a raw physical file.
                if self.is_staging_skill_root(&nid) || self.is_pending_install(&nid) {
                    let physical = self
                        .source_base()
                        .join(category)
                        .join(skill_name)
                        .join("SKILL.md");
                    match std::fs::metadata(&physical) {
                        Ok(meta) => {
                            let mut attr = file_attr_from_metadata(&meta);
                            attr.ino = ino;
                            reply.attr(&Duration::from_secs(1), &attr);
                        }
                        Err(e) => reply.error(errno(&e)),
                    }
                    return;
                }
                match self.transformed_skill_attr(skill_name, Some(category)) {
                    Some(mut attr) => {
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    None => reply.error(libc::ENOENT),
                }
            }
            PathType::NestedPassthrough {
                ref category,
                ref skill_name,
                ref relative_path,
            } => {
                let npt = PathType::NestedPassthrough {
                    category: category.clone(),
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&npt, _req) {
                    Some(false) => {
                        reply.error(libc::ENOENT);
                        return;
                    }
                    Some(true) => {
                        let physical_path = self
                            .hermes_skill_physical_dir(category, skill_name)
                            .join(relative_path);
                        match std::fs::symlink_metadata(&physical_path) {
                            Ok(meta) => {
                                let mut attr = file_attr_from_metadata(&meta);
                                attr.ino = ino;
                                reply.attr(&Duration::from_secs(1), &attr);
                            }
                            Err(e) => reply.error(errno(&e)),
                        }
                        return;
                    }
                    None => {}
                }
                let nid = Self::hermes_skill_id(category, skill_name);
                if !self.is_staging_skill_root(&nid)
                    && !self.is_pending_install(&nid)
                    && matches!(
                        self.resolve_hermes_nested_read(category, skill_name),
                        crate::fs::read_resolution::ReadResolution::Hidden
                    )
                {
                    reply.error(libc::ENOENT);
                    return;
                }
                let physical = match self.resolve_hermes_nested_read(category, skill_name) {
                    crate::fs::read_resolution::ReadResolution::Snapshot { dir, .. } => {
                        dir.join(relative_path)
                    }
                    _ => self
                        .source_base()
                        .join(category)
                        .join(skill_name)
                        .join(relative_path),
                };
                match std::fs::symlink_metadata(&physical) {
                    Ok(meta) => {
                        let mut attr = file_attr_from_metadata(&meta);
                        attr.ino = ino;
                        reply.attr(&Duration::from_secs(1), &attr);
                    }
                    Err(e) => reply.error(errno(&e)),
                }
            }
            PathType::Invalid => reply.error(libc::ENOENT),
        }
    }
    pub(in crate::fs) fn statfs_impl(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        let source = self.source_base();
        let c_path = match std::ffi::CString::new(source.to_string_lossy().into_owned()) {
            Ok(p) => p,
            Err(_) => return reply.error(libc::EINVAL),
        };

        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };

        if ret != 0 {
            let e = std::io::Error::last_os_error();
            return reply.error(errno(&e));
        }

        reply.statfs(
            stat.f_blocks,
            stat.f_bfree,
            stat.f_bavail,
            stat.f_files,
            stat.f_ffree,
            stat.f_bsize as u32,
            stat.f_namemax as u32,
            stat.f_frsize as u32,
        );
    }

    fn check_activated_access_result(
        &self,
        read_path: &Path,
        write_path: &Path,
        mask: i32,
        req: &Request,
    ) -> i32 {
        if mask == libc::F_OK {
            return self.check_physical_access_result(read_path, mask, req);
        }

        let read_mask = mask & (libc::R_OK | libc::X_OK);
        if read_mask != 0 {
            let result = self.check_physical_access_result(read_path, read_mask, req);
            if result != 0 {
                return result;
            }
        }

        if (mask & libc::W_OK) != 0 {
            return self.check_physical_access_result(write_path, libc::W_OK, req);
        }

        0
    }

    fn flat_access_read_path(
        &self,
        skill_name: &str,
        relative_path: Option<&Path>,
    ) -> Option<std::path::PathBuf> {
        let source_root_candidate =
            self.is_staging_skill_root(skill_name) || self.is_pending_install(skill_name);
        let grace_path = relative_path
            .is_some_and(|relative| self.is_post_publish_grace_allowed(skill_name, Some(relative)));
        if source_root_candidate || grace_path {
            let kind = if source_root_candidate {
                FlatLiveDirKind::SourceRootCandidate
            } else {
                FlatLiveDirKind::ExistingSkill
            };
            let live_dir = self.flat_live_dir(skill_name, kind);
            return Some(match relative_path {
                Some(relative) => live_dir.join(relative),
                None => live_dir,
            });
        }

        self.skill_read_dir(skill_name)
            .map(|dir| match relative_path {
                Some(relative) => dir.join(relative),
                None => dir,
            })
    }

    fn nested_access_read_path(
        &self,
        category: &str,
        skill_name: &str,
        relative_path: Option<&Path>,
    ) -> Option<std::path::PathBuf> {
        let nested_id = Self::hermes_skill_id(category, skill_name);
        let live_dir = self.source_base().join(category).join(skill_name);
        if self.is_staging_skill_root(&nested_id)
            || self.is_pending_install(&nested_id)
            || relative_path.is_some_and(|relative| {
                self.is_post_publish_grace_allowed(&nested_id, Some(relative))
            })
        {
            return Some(match relative_path {
                Some(relative) => live_dir.join(relative),
                None => live_dir,
            });
        }

        self.hermes_nested_skill_read_dir(category, skill_name)
            .map(|dir| match relative_path {
                Some(relative) => dir.join(relative),
                None => dir,
            })
    }

    pub(in crate::fs) fn access_impl(
        &mut self,
        req: &Request,
        ino: u64,
        mask: i32,
        reply: ReplyEmpty,
    ) {
        let valid_bits = libc::F_OK | libc::R_OK | libc::W_OK | libc::X_OK;
        if mask & !valid_bits != 0 {
            return reply.error(libc::EINVAL);
        }

        let path = match self.inodes.get_path(ino) {
            Some(p) => p,
            None => return reply.error(libc::ENOENT),
        };

        let path_type = self.parse_fuse_path(Path::new(&path));

        match path_type {
            PathType::Root | PathType::SkillsDir | PathType::InboxDir => {
                if (mask & libc::W_OK) != 0 {
                    reply.error(libc::EACCES);
                } else {
                    reply.ok();
                }
            }
            PathType::SkillDir { .. } => {
                // All visible skill directories: virtual semantics
                // F_OK/R_OK/X_OK succeed, W_OK denied
                if (mask & libc::W_OK) != 0 {
                    reply.error(libc::EACCES);
                } else {
                    reply.ok();
                }
            }
            PathType::InboxSkillDir { ref skill_name } => {
                if !Self::is_inbox_skill_name_allowed(skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                let dir_path = self.inbox_skill_dir(skill_name);
                let result = self.check_physical_access_result(&dir_path, mask, req);
                if result == 0 {
                    reply.ok();
                } else {
                    reply.error(result);
                }
            }
            PathType::InboxPassthrough {
                ref skill_name,
                ref relative_path,
            } => {
                if !Self::is_inbox_skill_name_allowed(skill_name) {
                    reply.error(libc::ENOENT);
                    return;
                }
                let ipt = PathType::InboxPassthrough {
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&ipt, req) {
                    Some(false) => {
                        reply.error(libc::ENOENT);
                        return;
                    }
                    Some(true) => {
                        let file_path = self.inbox_skill_dir(skill_name).join(relative_path);
                        let result = self.check_physical_access_result(&file_path, mask, req);
                        if result == 0 {
                            reply.ok();
                        } else {
                            reply.error(result);
                        }
                        return;
                    }
                    None => {}
                }
                if (mask & libc::W_OK) != 0 {
                    if let Some(errno) = self.enforce_skill_meta(
                        &ipt,
                        SkillEventKind::Metadata,
                        req,
                        Some(format!("access mask=0x{:x}", mask)),
                    ) {
                        reply.error(errno);
                        return;
                    }
                }
                let file_path = self.inbox_skill_dir(skill_name).join(relative_path);
                let result = self.check_physical_access_result(&file_path, mask, req);
                if result == 0 {
                    reply.ok();
                } else {
                    reply.error(result);
                }
            }
            PathType::SkillMd { ref skill_name } => {
                if is_skill_discover_path(skill_name) {
                    if (mask & (libc::W_OK | libc::X_OK)) != 0 {
                        reply.error(libc::EACCES);
                    } else {
                        reply.ok();
                    }
                } else {
                    let live_path = self.skill_physical_dir(skill_name).join("SKILL.md");
                    let read_path =
                        match self.flat_access_read_path(skill_name, Some(Path::new("SKILL.md"))) {
                            Some(path) => path,
                            None => return reply.error(libc::ENOENT),
                        };
                    let result =
                        self.check_activated_access_result(&read_path, &live_path, mask, req);
                    if result == 0 {
                        reply.ok();
                    } else {
                        reply.error(result);
                    }
                }
            }
            PathType::Passthrough {
                ref skill_name,
                ref relative_path,
            } => {
                if is_skill_discover_path(skill_name) {
                    if (mask & (libc::W_OK | libc::X_OK)) != 0 {
                        reply.error(libc::EACCES);
                    } else {
                        reply.ok();
                    }
                } else {
                    let pt = PathType::Passthrough {
                        skill_name: skill_name.clone(),
                        relative_path: relative_path.clone(),
                    };
                    match self.is_trusted_skill_meta_access(&pt, req) {
                        Some(false) => {
                            reply.error(libc::ENOENT);
                            return;
                        }
                        Some(true) => {
                            let file_path = self.skill_physical_dir(skill_name).join(relative_path);
                            let result = self.check_physical_access_result(&file_path, mask, req);
                            if result == 0 {
                                reply.ok();
                            } else {
                                reply.error(result);
                            }
                            return;
                        }
                        None => {}
                    }
                    // S1: deny W_OK on `.skill-meta/**`. R_OK/X_OK/F_OK
                    // still defer to the underlying physical permissions.
                    if (mask & libc::W_OK) != 0 {
                        if let Some(errno) = self.enforce_skill_meta(
                            &pt,
                            SkillEventKind::Metadata,
                            req,
                            Some(format!("access mask=0x{:x}", mask)),
                        ) {
                            reply.error(errno);
                            return;
                        }
                    }
                    let live_path = self.skill_physical_dir(skill_name).join(relative_path);
                    let read_path = match self
                        .flat_access_read_path(skill_name, Some(relative_path.as_path()))
                    {
                        Some(path) => path,
                        None => return reply.error(libc::ENOENT),
                    };
                    let result =
                        self.check_activated_access_result(&read_path, &live_path, mask, req);
                    if result == 0 {
                        reply.ok();
                    } else {
                        reply.error(result);
                    }
                }
            }
            PathType::HermesMeta { .. }
            | PathType::HermesMetaChild { .. }
            | PathType::CategoryPassthrough { .. }
            | PathType::CategoryDir { .. } => {
                let physical = match self.resolve_physical_path(&path) {
                    Some(p) => p,
                    None => return reply.error(libc::ENOENT),
                };
                let result = self.check_physical_access_result(&physical, mask, req);
                if result == 0 {
                    reply.ok();
                } else {
                    reply.error(result);
                }
            }
            PathType::NestedSkillDir {
                ref category,
                ref skill_name,
            } => {
                let live_path = self.source_base().join(category).join(skill_name);
                let read_path = match self.nested_access_read_path(category, skill_name, None) {
                    Some(path) => path,
                    None => return reply.error(libc::ENOENT),
                };
                let result = self.check_activated_access_result(&read_path, &live_path, mask, req);
                if result == 0 {
                    reply.ok();
                } else {
                    reply.error(result);
                }
            }
            PathType::NestedSkillMd {
                ref category,
                ref skill_name,
            } => {
                let relative_path = Path::new("SKILL.md");
                let live_path = self
                    .source_base()
                    .join(category)
                    .join(skill_name)
                    .join(relative_path);
                let read_path =
                    match self.nested_access_read_path(category, skill_name, Some(relative_path)) {
                        Some(path) => path,
                        None => return reply.error(libc::ENOENT),
                    };
                let result = self.check_activated_access_result(&read_path, &live_path, mask, req);
                if result == 0 {
                    reply.ok();
                } else {
                    reply.error(result);
                }
            }
            PathType::NestedPassthrough {
                ref category,
                ref skill_name,
                ref relative_path,
            } => {
                let npt = PathType::NestedPassthrough {
                    category: category.clone(),
                    skill_name: skill_name.clone(),
                    relative_path: relative_path.clone(),
                };
                match self.is_trusted_skill_meta_access(&npt, req) {
                    Some(false) => {
                        reply.error(libc::ENOENT);
                        return;
                    }
                    Some(true) => {
                        let file_path = self
                            .hermes_skill_physical_dir(category, skill_name)
                            .join(relative_path);
                        let result = self.check_physical_access_result(&file_path, mask, req);
                        if result == 0 {
                            reply.ok();
                        } else {
                            reply.error(result);
                        }
                        return;
                    }
                    None => {}
                }
                if (mask & libc::W_OK) != 0 {
                    if let Some(errno) = self.enforce_skill_meta(
                        &npt,
                        SkillEventKind::Metadata,
                        req,
                        Some(format!("access mask=0x{:x}", mask)),
                    ) {
                        reply.error(errno);
                        return;
                    }
                }
                let live_path = self
                    .source_base()
                    .join(category)
                    .join(skill_name)
                    .join(relative_path);
                let read_path = match self.nested_access_read_path(
                    category,
                    skill_name,
                    Some(relative_path.as_path()),
                ) {
                    Some(path) => path,
                    None => return reply.error(libc::ENOENT),
                };
                let result = self.check_activated_access_result(&read_path, &live_path, mask, req);
                if result == 0 {
                    reply.ok();
                } else {
                    reply.error(result);
                }
            }
            PathType::Invalid => {
                reply.error(libc::ENOENT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, store::SkillStore};

    use super::*;
    use crate::security::{
        PostPublishGraceController, PostPublishSessionKind, PostPublishWritePattern, StagingConfig,
        StagingMatcher, StagingPattern,
    };

    #[test]
    fn flat_access_separates_categorized_grace_from_staging_candidates() {
        let source = tempfile::tempdir().expect("source tempdir");
        let skill_dir = source.path().join("catalog/demo");
        std::fs::create_dir_all(skill_dir.join(".clawhub")).expect("categorized skill directories");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: categorized\n---\nbody\n",
        )
        .expect("categorized SKILL.md");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let grace = PostPublishGraceController::new(
            Duration::from_secs(5),
            vec![PostPublishWritePattern::PrefixRecursive(
                ".clawhub".to_string(),
            )],
        );
        grace.start_session("demo", PostPublishSessionKind::StagingRename);
        let staging = Arc::new(StagingMatcher::new(StagingConfig {
            patterns: vec![StagingPattern::Exact(".staging-demo".to_string())],
            ..StagingConfig::default()
        }));
        let fs = SkillFs::new(
            source.path().join("mount"),
            source.path().to_path_buf(),
            Arc::new(RwLock::new(store)),
            false,
        )
        .with_post_publish_controller(grace.clone())
        .with_staging_matcher(staging);

        assert_eq!(
            fs.flat_access_read_path("demo", Some(Path::new(".clawhub/origin.json"))),
            Some(skill_dir.join(".clawhub/origin.json"))
        );
        assert_eq!(
            fs.flat_access_read_path(".staging-demo", Some(Path::new("SKILL.md"))),
            Some(source.path().join(".staging-demo/SKILL.md"))
        );

        grace.shutdown();
    }
}
