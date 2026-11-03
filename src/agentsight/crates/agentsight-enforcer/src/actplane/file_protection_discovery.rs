//! Mount discovery through namespace-resolved directory identities.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;

use super::file_protection_mountinfo::{MountInfo, visible_paths_for_mount_root};
use super::file_protection_resolver::{FileIdentity, HeldPath, open_held, open_held_directory};
use crate::BackendError;

#[derive(Debug)]
pub(super) struct DiscoveredSubmounts {
    pub(super) indices: Vec<usize>,
    pub(super) target_spellings: HashSet<String>,
    pub(super) aliases: Vec<HeldPath>,
}

pub(super) fn discover_submounts(
    root: &File,
    mounts: &[MountInfo],
    authored_target: &str,
    target_identity: FileIdentity,
) -> Result<DiscoveredSubmounts, BackendError> {
    let mut descendants = Vec::new();
    let mut target_spellings = HashSet::from([authored_target.to_string()]);
    let mut aliases = Vec::new();
    let mut identities = HashMap::<String, FileIdentity>::new();

    for (index, mount) in mounts.iter().enumerate() {
        let mut below_identity = false;
        if let Some(parent) = Path::new(&mount.mountpoint).parent().and_then(Path::to_str) {
            for ancestor in lexical_ancestors(parent).into_iter().rev() {
                let identity = if let Some(identity) = identities.get(ancestor) {
                    *identity
                } else {
                    let Ok(path) = open_held_directory(root, ancestor) else {
                        continue;
                    };
                    identities.insert(ancestor.into(), path.identity);
                    path.identity
                };
                if identity.ino == target_identity.ino && identity.dev == target_identity.dev {
                    target_spellings.insert(ancestor.into());
                    below_identity = true;
                    break;
                }
            }
        }
        let below = is_component_descendant(authored_target, &mount.mountpoint) || below_identity;
        let mounted = match open_held(root, &mount.mountpoint) {
            Ok(mounted) => mounted,
            Err(error) if below || mount.mountpoint == authored_target => {
                return Err(BackendError::CompileFailure(error.to_string()));
            }
            Err(_) => continue,
        };
        identities.insert(mount.mountpoint.clone(), mounted.identity);
        if mounted.identity.ino == target_identity.ino
            && mounted.identity.dev == target_identity.dev
        {
            target_spellings.insert(mount.mountpoint.clone());
            if mounted.identity != target_identity {
                aliases.push(mounted);
            }
            continue;
        }
        if mount.mountpoint != authored_target && below {
            descendants.push(index);
        }
    }
    Ok(DiscoveredSubmounts {
        indices: descendants,
        target_spellings,
        aliases,
    })
}

pub(super) fn underlying_submount_paths(
    root: &File,
    mounts: &[MountInfo],
    mount: &MountInfo,
    mounted: &HeldPath,
) -> Vec<HeldPath> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for candidate in visible_paths_for_mount_root(mounts, mount) {
        let Ok(path) = open_held(root, &candidate) else {
            continue;
        };
        if path.identity.mode & libc::S_IFMT as u16 == libc::S_IFDIR as u16
            && path.identity.ino == mounted.identity.ino
            && path.identity.dev == mounted.identity.dev
            && seen.insert(path.identity)
        {
            paths.push(path);
        }
    }
    paths
}

pub(super) fn lexical_ancestors(path: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut current = path;
    loop {
        result.push(current);
        let Some(index) = current.rfind('/') else {
            break;
        };
        if index == 0 {
            break;
        }
        current = &current[..index];
    }
    result
}

pub(super) fn path_in_scope(root: &str, candidate: &str) -> bool {
    root == candidate || is_component_descendant(root, candidate)
}

pub(super) fn is_component_descendant(root: &str, candidate: &str) -> bool {
    root == "/" && candidate != "/"
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
#[path = "file_protection_discovery_tests.rs"]
mod tests;
