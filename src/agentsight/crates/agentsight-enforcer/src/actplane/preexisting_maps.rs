//! Fail-closed preflight for pre-existing shared file mappings.
//!
//! A shared mapping created before a binding bypasses content mediation:
//! stores through a writable mapping never reach `file_permission`, and a
//! read-only shared mapping can still be escalated with `mprotect` in
//! profiles that do not attach the mapping hooks. The kernel cannot revoke
//! such mappings retroactively, so apply rejects a target process tree that
//! already holds one over a protected path instead of silently claiming
//! content protection it cannot enforce.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;

use crate::BackendError;

use super::inode_policy::userspace_dev_to_kernel;

/// One protected path matched against `/proc/<pid>/maps` entries.
pub(super) struct ProtectedEntry {
    /// Protected path as authored; reported in the rejection error.
    pub(super) path: String,
    /// How map entries are matched against this target.
    pub(super) scope: ProtectedScope,
}

/// Matching strategy for one protected target.
pub(super) enum ProtectedScope {
    /// Matches map entries by device and inode identity.
    Exact {
        /// Kernel-encoded device id of the protected file.
        dev: u32,
        /// Inode number of the protected file.
        ino: u64,
    },
    /// Matches map entries whose path lies under the target tree, except the
    /// authored exclusions.
    Tree {
        /// Lexical directory-subtree exclusion prefixes inside the tree.
        subtree_prefixes: Vec<String>,
        /// Exact entry paths excluded from the tree scope.
        exact_entries: Vec<String>,
    },
}

struct Mapping {
    dev: u32,
    ino: u64,
    path: String,
}

/// Rejects the apply when any process in the `root_pid` tree already holds
/// write access to a protected path: a shared mapping (stores bypass every
/// hook, and read-only shared mappings can be escalated) or a writable file
/// descriptor (which can be mapped shared-writable after binding without any
/// further open).
///
/// # Errors
///
/// Returns [`BackendError::PreexistingWritableMap`] or
/// [`BackendError::PreexistingWritableFd`] naming the first offending process
/// and path. Unreadable `/proc` entries are skipped: a process that hides its
/// maps or descriptors from the enforcer's own UID cannot be bound anyway.
pub(super) fn reject_preexisting_write_access(
    root_pid: i32,
    protected: &[ProtectedEntry],
) -> Result<(), BackendError> {
    if protected.is_empty() {
        return Ok(());
    }
    for pid in process_tree(root_pid) {
        reject_writable_shared_mappings(pid, protected)?;
        reject_writable_file_descriptors(pid, protected)?;
    }
    Ok(())
}

fn reject_writable_shared_mappings(
    pid: i32,
    protected: &[ProtectedEntry],
) -> Result<(), BackendError> {
    let Ok(maps) = fs::read_to_string(format!("/proc/{pid}/maps")) else {
        return Ok(());
    };
    for line in maps.lines() {
        let Some(mapping) = parse_shared_mapping(line) else {
            continue;
        };
        for entry in protected {
            if matches_entry(&mapping, entry) {
                return Err(BackendError::PreexistingWritableMap {
                    pid,
                    path: mapping.path,
                });
            }
        }
    }
    Ok(())
}

fn reject_writable_file_descriptors(
    pid: i32,
    protected: &[ProtectedEntry],
) -> Result<(), BackendError> {
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Ok(());
    };
    for fd in fds.flatten() {
        let name = fd.file_name();
        let Some(number) = name.to_str() else {
            continue;
        };
        let link = format!("/proc/{pid}/fd/{number}");
        let Ok(metadata) = fs::metadata(&link) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let dev = userspace_dev_to_kernel(metadata.dev());
        let ino = metadata.ino();
        let Some(target) = protected.iter().find(|target| match &target.scope {
            ProtectedScope::Exact {
                dev: tdev,
                ino: tino,
            } => *tdev == dev && *tino == ino,
            ProtectedScope::Tree {
                subtree_prefixes,
                exact_entries,
            } => {
                let Ok(linked) = fs::read_link(&link) else {
                    return false;
                };
                let path: String = linked.to_string_lossy().into_owned();
                let under_tree =
                    path == target.path || path.starts_with(&format!("{}/", target.path));
                under_tree
                    && !exact_entries.contains(&path)
                    && !subtree_prefixes
                        .iter()
                        .any(|prefix| path.starts_with(prefix))
            }
        }) else {
            continue;
        };
        let Ok(info) = fs::read_to_string(format!("/proc/{pid}/fdinfo/{number}")) else {
            continue;
        };
        if fd_flags_are_writable(&info) {
            return Err(BackendError::PreexistingWritableFd {
                pid,
                path: target.path.clone(),
            });
        }
    }
    Ok(())
}

/// Reads the octal `flags:` field of `/proc/<pid>/fdinfo` and reports a
/// write-capable access mode (O_WRONLY or O_RDWR).
fn fd_flags_are_writable(fdinfo: &str) -> bool {
    let Some(flags) = fdinfo.lines().find_map(|line| line.strip_prefix("flags:")) else {
        return false;
    };
    let Ok(flags) = u32::from_str_radix(flags.trim(), 8) else {
        return false;
    };
    flags & 0o3 != 0
}

fn matches_entry(mapping: &Mapping, entry: &ProtectedEntry) -> bool {
    match &entry.scope {
        ProtectedScope::Exact { dev, ino } => mapping.dev == *dev && mapping.ino == *ino,
        ProtectedScope::Tree {
            subtree_prefixes,
            exact_entries,
        } => {
            let under_tree =
                mapping.path == entry.path || mapping.path.starts_with(&format!("{}/", entry.path));
            // ExactEntry exclusions match one entry only; a prefix match
            // would silently fail open for siblings such as "<name>2".
            under_tree
                && !exact_entries.contains(&mapping.path)
                && !subtree_prefixes
                    .iter()
                    .any(|prefix| mapping.path.starts_with(prefix))
        }
    }
}

/// Parses one shared (`*-s`) file map line; private mappings cannot damage
/// file content, and read-only shared ones can still be escalated later.
///
/// The pathname is taken as the remainder of the line: `/proc/<pid>/maps`
/// does not escape spaces, so whitespace-splitting it would truncate paths
/// and fail open on tree-prefix matches. The four kernel octal escapes are
/// reversed by `unescape_maps_path`.
fn parse_shared_mapping(line: &str) -> Option<Mapping> {
    let mut fields = line.splitn(6, ' ');
    let _range = fields.next()?;
    let perms = fields.next()?;
    let _offset = fields.next()?;
    let dev = fields.next()?;
    let ino = fields.next()?;
    let path = fields.next()?.trim_end();
    let perms = perms.as_bytes();
    if perms.len() < 4 || perms[3] != b's' {
        return None;
    }
    if path.is_empty() || path.starts_with('[') {
        return None;
    }
    let (major, minor) = dev.split_once(':')?;
    let major = u32::from_str_radix(major, 16).ok()?;
    let minor = u32::from_str_radix(minor, 16).ok()?;
    Some(Mapping {
        dev: (major << 20) | minor,
        ino: ino.parse().ok()?,
        path: unescape_maps_path(path),
    })
}

/// Reverses the kernel's octal escapes for space, tab, newline and backslash.
fn unescape_maps_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 4 <= bytes.len() {
            match &bytes[index + 1..index + 4] {
                b"040" => {
                    out.push(b' ');
                    index += 4;
                    continue;
                }
                b"011" => {
                    out.push(b'\t');
                    index += 4;
                    continue;
                }
                b"012" => {
                    out.push(b'\n');
                    index += 4;
                    continue;
                }
                b"134" => {
                    out.push(b'\\');
                    index += 4;
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Collects `root_pid` and every descendant via `/proc` parent links.
fn process_tree(root_pid: i32) -> Vec<i32> {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    let Ok(proc_dir) = fs::read_dir("/proc") else {
        return vec![root_pid];
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        let Some(ppid) = parent_of(pid) else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
    }
    let mut tree = Vec::new();
    let mut queue = vec![root_pid];
    while let Some(pid) = queue.pop() {
        tree.push(pid);
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids.iter().copied());
        }
    }
    tree
}

fn parent_of(pid: i32) -> Option<i32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    // Fields after comm: state, ppid, ...
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_shared_file_lines_parse() {
        let line = "7f0000000000-7f0000001000 rw-s 00000000 103:02 1234567 /srv/data/keep.txt";
        let mapping = parse_shared_mapping(line).expect("shared writable file line");
        assert_eq!(mapping.dev, (0x103 << 20) | 0x02);
        assert_eq!(mapping.ino, 1234567);
        assert_eq!(mapping.path, "/srv/data/keep.txt");
        assert!(
            parse_shared_mapping(
                "7f0000000000-7f0000001000 r--s 00000000 103:02 1 /srv/data/keep.txt"
            )
            .is_some(),
            "read-only shared mappings can be escalated"
        );
        for denied in [
            "7f0000000000-7f0000001000 rw-p 00000000 103:02 1 /srv/data/keep.txt",
            "7f0000000000-7f0000001000 rw-s 00000000 00:00 0 [anon_shm]",
        ] {
            assert!(parse_shared_mapping(denied).is_none(), "{denied}");
        }
    }

    #[test]
    fn tree_scope_honors_exclusions() {
        let entry = ProtectedEntry {
            path: "/srv/data".into(),
            scope: ProtectedScope::Tree {
                subtree_prefixes: vec!["/srv/data/cache/".into()],
                exact_entries: vec!["/srv/data/keep2.txt".into()],
            },
        };
        let inside = Mapping {
            dev: 1,
            ino: 2,
            path: "/srv/data/keep.txt".into(),
        };
        let excluded = Mapping {
            dev: 1,
            ino: 3,
            path: "/srv/data/cache/tmp.bin".into(),
        };
        let exact_excluded = Mapping {
            dev: 1,
            ino: 6,
            path: "/srv/data/keep2.txt".into(),
        };
        let sibling_of_subtree = Mapping {
            dev: 1,
            ino: 7,
            path: "/srv/data/cache2/tmp.bin".into(),
        };
        let outside = Mapping {
            dev: 1,
            ino: 4,
            path: "/srv/database.txt".into(),
        };
        assert!(matches_entry(&inside, &entry));
        assert!(!matches_entry(&excluded, &entry));
        assert!(!matches_entry(&exact_excluded, &entry));
        assert!(
            matches_entry(&sibling_of_subtree, &entry),
            "a subtree exclusion must not swallow sibling directories"
        );
        assert!(!matches_entry(&outside, &entry));
    }

    #[test]
    fn maps_paths_with_spaces_and_escapes_parse_whole() {
        let mapping = parse_shared_mapping(
            "7f0000000000-7f0000001000 rw-s 00000000 103:02 1 /srv/my data/keep\\040name.txt",
        )
        .expect("space-bearing path stays intact");
        assert_eq!(mapping.path, "/srv/my data/keep name.txt");
    }

    #[test]
    fn current_process_is_in_its_own_tree() {
        let pid = std::process::id() as i32;
        assert!(process_tree(pid).contains(&pid));
    }

    #[test]
    fn fdinfo_flags_parse_write_modes_only() {
        assert!(fd_flags_are_writable("pos:\t0\nflags:\t0100002\n"));
        assert!(fd_flags_are_writable("flags: 01"));
        assert!(!fd_flags_are_writable("flags: 00"));
        assert!(!fd_flags_are_writable("pos: 0\n"));
    }

    #[test]
    fn held_writable_descriptor_of_protected_file_is_rejected() {
        let dir = std::env::temp_dir().join(format!("fg-fd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("fixture dir");
        let path = dir.join("keep.txt");
        std::fs::write(&path, b"fixture").expect("fixture file");
        let metadata = std::fs::metadata(&path).expect("fixture identity");
        let protected = vec![ProtectedEntry {
            path: path.to_string_lossy().into_owned(),
            scope: ProtectedScope::Exact {
                dev: userspace_dev_to_kernel(metadata.dev()),
                ino: metadata.ino(),
            },
        }];
        let pid = std::process::id() as i32;
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("held writable descriptor");
        let rejection = reject_preexisting_write_access(pid, &protected)
            .expect_err("held writable descriptor must reject the apply");
        assert!(
            matches!(rejection, BackendError::PreexistingWritableFd { .. }),
            "{rejection}"
        );
        drop(held);
        reject_preexisting_write_access(pid, &protected).expect("clean tree applies");
        std::fs::remove_dir_all(&dir).expect("cleanup fixture");
    }
}
