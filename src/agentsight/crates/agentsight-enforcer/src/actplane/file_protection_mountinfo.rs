//! Bounded parser for Linux mountinfo topology snapshots.

use std::collections::HashSet;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Read;

use crate::BackendError;

pub(super) const MAX_MOUNTINFO_LINE_BYTES: usize = 16 * 1024;
const MAX_MOUNTINFO_BYTES: u64 = 4 * 1024 * 1024;
const MAX_MOUNTS: usize = 4096;
const MAX_MOUNT_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MountInfo {
    pub(super) id: u32,
    pub(super) parent_id: u32,
    pub(super) device: (u32, u32),
    pub(super) root: String,
    pub(super) mountpoint: String,
    pub(super) fs_type: String,
}

pub(super) fn read_mountinfo(pid: i32) -> Result<(Vec<MountInfo>, u64), BackendError> {
    let path = format!("/proc/{pid}/mountinfo");
    let file = File::open(&path).map_err(|error| kernel(format!("open {path}: {error}")))?;
    let mut bytes = Vec::new();
    file.take(MAX_MOUNTINFO_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| kernel(format!("read {path}: {error}")))?;
    if bytes.len() as u64 > MAX_MOUNTINFO_BYTES {
        return Err(invalid("mountinfo exceeds 4 MiB limit"));
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok((parse_mountinfo(&bytes)?, hasher.finish()))
}

pub(super) fn parse_mountinfo(bytes: &[u8]) -> Result<Vec<MountInfo>, BackendError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("mountinfo is not UTF-8"))?;
    let mut mounts = Vec::new();
    let mut ids = HashSet::new();
    for line in text.lines() {
        if line.len() > MAX_MOUNTINFO_LINE_BYTES {
            return Err(invalid("mountinfo line exceeds 16 KiB limit"));
        }
        if mounts.len() == MAX_MOUNTS {
            return Err(invalid("mountinfo exceeds 4096-entry limit"));
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        let separator = fields
            .iter()
            .position(|field| *field == "-")
            .ok_or_else(|| invalid("mountinfo line lacks separator"))?;
        if separator < 6 || fields.len() < separator + 3 {
            return Err(invalid("mountinfo line has too few fields"));
        }
        let id = fields[0]
            .parse()
            .map_err(|_| invalid("mountinfo mount ID is invalid"))?;
        let parent_id = fields[1]
            .parse()
            .map_err(|_| invalid("mountinfo parent ID is invalid"))?;
        let (major, minor) = fields[2]
            .split_once(':')
            .ok_or_else(|| invalid("mountinfo device is invalid"))?;
        let device = (
            major
                .parse()
                .map_err(|_| invalid("mountinfo device major is invalid"))?,
            minor
                .parse()
                .map_err(|_| invalid("mountinfo device minor is invalid"))?,
        );
        if !ids.insert(id) {
            return Err(invalid(format!(
                "mountinfo contains duplicate mount ID {id}"
            )));
        }
        mounts.push(MountInfo {
            id,
            parent_id,
            device,
            root: decode_mount_path(fields[3])?,
            mountpoint: decode_mount_path(fields[4])?,
            fs_type: fields[separator + 1].into(),
        });
    }
    let all_ids: HashSet<_> = mounts.iter().map(|mount| mount.id).collect();
    for mount in &mounts {
        // The namespace root can name a parent hidden outside this namespace.
        if !all_ids.contains(&mount.parent_id) && mount.mountpoint != "/" {
            return Err(invalid(format!(
                "mount {} references unknown parent {}",
                mount.id, mount.parent_id
            )));
        }
    }
    Ok(mounts)
}

fn decode_mount_path(value: &str) -> Result<String, BackendError> {
    if value.len() > MAX_MOUNT_PATH_BYTES {
        return Err(invalid("mountinfo path exceeds 4096-byte limit"));
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 3 >= bytes.len() {
            return Err(invalid("mountinfo path has truncated escape"));
        }
        let byte = match &bytes[index + 1..index + 4] {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return Err(invalid("mountinfo path has unknown escape")),
        };
        decoded.push(byte);
        index += 4;
    }
    if decoded.first() != Some(&b'/') || decoded.contains(&0) {
        return Err(invalid("mountinfo path must be absolute and NUL-free"));
    }
    String::from_utf8(decoded).map_err(|_| invalid("decoded mountinfo path is not UTF-8"))
}

pub(super) fn underlying_mount_path(
    mounts: &[MountInfo],
    path: &str,
) -> Result<String, BackendError> {
    let covering = mounts
        .iter()
        .filter(|mount| path_in_scope(&mount.mountpoint, path))
        .max_by_key(|mount| mount.mountpoint.len())
        .ok_or_else(|| invalid(format!("mountinfo has no mount covering {path:?}")))?;
    join_mount_path(covering, path).ok_or_else(|| invalid("mountpoint component relation changed"))
}

pub(super) fn visible_paths_for_mount_root(
    mounts: &[MountInfo],
    mounted: &MountInfo,
) -> Vec<String> {
    let mut candidates: Vec<_> = mounts
        .iter()
        .filter(|candidate| {
            candidate.id != mounted.id
                && candidate.device == mounted.device
                && path_in_scope(&candidate.root, &mounted.root)
        })
        .filter_map(|candidate| join_root_path(candidate, &mounted.root))
        .filter(|path| path != &mounted.mountpoint)
        .collect();
    candidates.sort_by_key(|path| std::cmp::Reverse(path.len()));
    candidates.dedup();
    candidates
}

fn join_mount_path(mount: &MountInfo, path: &str) -> Option<String> {
    let suffix = if mount.mountpoint == "/" {
        path
    } else {
        path.strip_prefix(&mount.mountpoint)?
    };
    join_component_paths(&mount.root, suffix)
}

fn join_root_path(mount: &MountInfo, root: &str) -> Option<String> {
    let suffix = if mount.root == "/" {
        root
    } else {
        root.strip_prefix(&mount.root)?
    };
    join_component_paths(&mount.mountpoint, suffix)
}

fn join_component_paths(base: &str, suffix: &str) -> Option<String> {
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return None;
    }
    Some(if suffix.is_empty() {
        base.into()
    } else if base == "/" {
        suffix.into()
    } else {
        format!("{base}{suffix}")
    })
}

fn path_in_scope(root: &str, candidate: &str) -> bool {
    root == candidate
        || root == "/" && candidate != "/"
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn invalid(message: impl Into<String>) -> BackendError {
    BackendError::CompileFailure(message.into())
}

fn kernel(message: impl Into<String>) -> BackendError {
    BackendError::KernelFailure(message.into())
}

#[cfg(test)]
#[path = "file_protection_mountinfo_tests.rs"]
mod tests;
