//! Check for existing mappings which can bypass file mediation.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use asc_file_policy::{FileProtectionExclusionKind, FileProtectionTargetKind, FileProtections};
use asc_lua_lsm_protocol::LuaLsmPlan;
use asc_policy_types::target::Failure;

use super::file_targets::{resolve_target, target_error};
use super::{MAX_OBJECTS, rejected, retryable};

pub(super) struct MappingPreflight {
    identities: BTreeMap<(u64, u64), FileProtections>,
}

type MatchQuery<'a> = &'a dyn Fn(usize, &File) -> Result<bool, Failure>;

impl MappingPreflight {
    pub(super) fn collect(
        plan: &LuaLsmPlan,
        matches: Option<MatchQuery<'_>>,
    ) -> Result<Self, Failure> {
        let mut identities: BTreeMap<(u64, u64), FileProtections> = BTreeMap::new();
        let mut count = 0;
        for (index, target) in plan.policy.targets.iter().enumerate() {
            if !target.protections.read && !target.protections.ordinary_write {
                continue;
            }
            // The registered module evaluates the same path predicate used by its hooks.
            let query = if plan.patterns.contains_key(&index) {
                let Some(query) = matches else { continue };
                Some(query)
            } else {
                None
            };
            let Some(root) = resolve_target(Path::new(&target.path))? else {
                continue;
            };
            let mut pending = vec![root.clone()];
            while let Some(path) = pending.pop() {
                count += 1;
                if count > MAX_OBJECTS {
                    return Err(rejected("LUA_LSM_OBJECT_LIMIT_EXCEEDED"));
                }
                let metadata = fs::symlink_metadata(&path).map_err(target_error)?;
                let relative = path
                    .strip_prefix(&root)
                    .map_err(|_| rejected("LUA_LSM_INVALID_PATH"))?;
                let authored = PathBuf::from(&target.path).join(relative);
                let excluded = target.exclusions.iter().any(|exclusion| {
                    let path = Path::new(&exclusion.path);
                    authored == path
                        || exclusion.kind == FileProtectionExclusionKind::DirectorySubtree
                            && authored.starts_with(path)
                });
                let is_directory = metadata.is_dir();
                if metadata.is_file() && (!excluded || query.is_some()) {
                    let metadata = if let Some(matches) = query {
                        let pinned = OpenOptions::new()
                            .read(true)
                            .custom_flags(
                                (rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW)
                                    .bits()
                                    .cast_signed(),
                            )
                            .open(&path)
                            .map_err(target_error)?;
                        if !matches(index + 1, &pinned)? {
                            continue;
                        }
                        pinned.metadata().map_err(target_error)?
                    } else {
                        metadata
                    };
                    let protections = identities
                        .entry((metadata.dev(), metadata.ino()))
                        .or_insert(target.protections);
                    protections.read |= target.protections.read;
                    protections.ordinary_write |= target.protections.ordinary_write;
                }
                if is_directory && target.kind == FileProtectionTargetKind::DirectoryTree {
                    for entry in fs::read_dir(&path).map_err(target_error)? {
                        pending.push(entry.map_err(target_error)?.path());
                    }
                }
            }
        }
        Ok(Self { identities })
    }

    pub(super) fn check(&self, pid: u32) -> Result<(), Failure> {
        if self.identities.is_empty() {
            return Ok(());
        }
        let maps = match fs::read_to_string(format!("/proc/{pid}/maps")) {
            Ok(maps) => maps,
            // Subject identity is fenced by the caller; exited descendants no
            // longer hold mappings. Other inspection failures must block apply.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(retryable("LUA_LSM_MAPPING_INSPECTION_FAILED")),
        };
        for line in maps.lines() {
            let mut fields = line.split_whitespace();
            let parse_error = || retryable("LUA_LSM_INVALID_PROCESS_MAPS");
            let _address = fields.next().ok_or_else(parse_error)?;
            let permissions = fields.next().ok_or_else(parse_error)?.as_bytes();
            let _offset = fields.next().ok_or_else(parse_error)?;
            let (major, minor) = fields
                .next()
                .and_then(|dev| dev.split_once(':'))
                .ok_or_else(parse_error)?;
            let major = u32::from_str_radix(major, 16).map_err(|_| parse_error())?;
            let minor = u32::from_str_radix(minor, 16).map_err(|_| parse_error())?;
            let inode: u64 = fields
                .next()
                .ok_or_else(parse_error)?
                .parse()
                .map_err(|_| parse_error())?;
            if permissions.len() != 4 {
                return Err(parse_error());
            }
            let identity = (rustix::fs::makedev(major, minor), inode);
            if let Some(protections) = self.identities.get(&identity) {
                if protections.read {
                    return Err(rejected("LUA_LSM_PREEXISTING_READ_MAPPING"));
                }
                if protections.ordinary_write && permissions[1] == b'w' && permissions[3] == b's' {
                    return Err(rejected("LUA_LSM_PREEXISTING_WRITABLE_MAPPING"));
                }
            }
        }
        Ok(())
    }
}
