//! Inode allocation and lookup table for the FUSE layer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use fuser::{FUSE_ROOT_ID, FileType};
use parking_lot::RwLock;

/// Manages inode-to-path mappings for the FUSE filesystem.
pub(crate) struct InodeManager {
    next_ino: AtomicU64,
    state: RwLock<InodeState>,
}

struct InodeState {
    inodes: HashMap<u64, InodeEntry>,
    paths: HashMap<String, u64>,
    lookup_counts: HashMap<u64, u64>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct InodeEntry {
    pub(crate) ino: u64,
    pub(crate) path: String,
    pub(crate) kind: FileType,
    pub(crate) parent: u64,
}

impl InodeManager {
    pub(crate) fn new() -> Self {
        let mut inodes = HashMap::new();
        let mut paths = HashMap::new();
        let mut lookup_counts = HashMap::new();

        inodes.insert(
            FUSE_ROOT_ID,
            InodeEntry {
                ino: FUSE_ROOT_ID,
                path: "/".to_string(),
                kind: FileType::Directory,
                parent: FUSE_ROOT_ID,
            },
        );
        paths.insert("/".to_string(), FUSE_ROOT_ID);
        // Root always has a permanent reference.
        lookup_counts.insert(FUSE_ROOT_ID, u64::MAX);

        Self {
            next_ino: AtomicU64::new(2),
            state: RwLock::new(InodeState {
                inodes,
                paths,
                lookup_counts,
            }),
        }
    }

    pub(crate) fn allocate(&self, path: &str, kind: FileType, parent: u64) -> u64 {
        let mut st = self.state.write();
        if let Some(&ino) = st.paths.get(path) {
            return ino;
        }
        let ino = self.next_ino.fetch_add(1, Ordering::SeqCst);
        let entry = InodeEntry {
            ino,
            path: path.to_string(),
            kind,
            parent,
        };
        st.inodes.insert(ino, entry);
        st.paths.insert(path.to_string(), ino);
        ino
    }

    /// Return a stable inode for `path` without creating a persistent
    /// mapping. If the path already has an inode, return it. Otherwise
    /// return a fresh, unpersisted ino so readdir entries don't grow
    /// the map. The kernel treats readdir inos as advisory hints and
    /// will call lookup to obtain the real mapping.
    pub(crate) fn readdir_ino(&self, path: &str) -> u64 {
        let st = self.state.read();
        if let Some(&ino) = st.paths.get(path) {
            return ino;
        }
        self.next_ino.fetch_add(1, Ordering::SeqCst)
    }

    /// Increment the lookup count for an inode. Call this from FUSE
    /// lookup/readdirplus when returning an inode to the kernel.
    pub(crate) fn remember(&self, ino: u64) {
        let mut st = self.state.write();
        if ino == FUSE_ROOT_ID {
            return;
        }
        *st.lookup_counts.entry(ino).or_insert(0) += 1;
    }

    /// Decrement the lookup count by `nlookup`. When the count reaches
    /// zero the inode/path mapping is released. Root is never released.
    pub(crate) fn forget(&self, ino: u64, nlookup: u64) {
        if ino == FUSE_ROOT_ID {
            return;
        }
        let mut st = self.state.write();
        Self::forget_inner(&mut st, ino, nlookup);
    }

    /// Batch forget: decrement lookup counts for multiple inodes.
    pub(crate) fn batch_forget(&self, items: &[(u64, u64)]) {
        let mut st = self.state.write();
        for &(ino, nlookup) in items {
            if ino == FUSE_ROOT_ID {
                continue;
            }
            Self::forget_inner(&mut st, ino, nlookup);
        }
    }

    fn forget_inner(st: &mut InodeState, ino: u64, nlookup: u64) {
        let count = st.lookup_counts.get(&ino).copied().unwrap_or(0);
        let new_count = count.saturating_sub(nlookup);
        if new_count == 0 {
            st.lookup_counts.remove(&ino);
            if let Some(entry) = st.inodes.remove(&ino) {
                st.paths.remove(&entry.path);
            }
        } else {
            st.lookup_counts.insert(ino, new_count);
        }
    }

    pub(crate) fn get(&self, ino: u64) -> Option<InodeEntry> {
        self.state.read().inodes.get(&ino).cloned()
    }

    pub(crate) fn lookup_by_path(&self, path: &str) -> Option<u64> {
        self.state.read().paths.get(path).copied()
    }

    pub(crate) fn get_path(&self, ino: u64) -> Option<String> {
        self.state.read().inodes.get(&ino).map(|e| e.path.clone())
    }

    #[allow(dead_code)]
    pub(crate) fn remove(&self, ino: u64) {
        let mut st = self.state.write();
        if let Some(entry) = st.inodes.remove(&ino) {
            st.paths.remove(&entry.path);
        }
        st.lookup_counts.remove(&ino);
    }

    /// Remove an inode and all children whose path starts with `path_prefix/`.
    pub(crate) fn remove_recursive(&self, path_prefix: &str) {
        let mut st = self.state.write();
        let to_remove: Vec<u64> = st
            .inodes
            .iter()
            .filter(|(_, e)| {
                e.path == path_prefix || e.path.starts_with(&format!("{}/", path_prefix))
            })
            .map(|(&ino, _)| ino)
            .collect();
        for ino in to_remove {
            if let Some(entry) = st.inodes.remove(&ino) {
                st.paths.remove(&entry.path);
            }
            st.lookup_counts.remove(&ino);
        }
    }

    /// Rename an inode's path and all children paths that start with old_path.
    pub(crate) fn rename_path(&self, old_path: &str, new_path: &str) {
        let mut st = self.state.write();
        let to_rename: Vec<(u64, String)> = st
            .inodes
            .iter()
            .filter(|(_, e)| e.path == old_path || e.path.starts_with(&format!("{}/", old_path)))
            .map(|(&ino, e)| (ino, e.path.clone()))
            .collect();
        for (ino, old) in to_rename {
            let new = old.replacen(old_path, new_path, 1);
            st.paths.remove(&old);
            st.paths.insert(new.clone(), ino);
            if let Some(entry) = st.inodes.get_mut(&ino) {
                entry.path = new;
            }
        }
    }

    /// Current lookup count for an inode (test helper).
    #[cfg(test)]
    fn lookup_count(&self, ino: u64) -> u64 {
        self.state
            .read()
            .lookup_counts
            .get(&ino)
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_never_released() {
        let mgr = InodeManager::new();
        assert!(mgr.get(FUSE_ROOT_ID).is_some());
        mgr.forget(FUSE_ROOT_ID, u64::MAX);
        assert!(mgr.get(FUSE_ROOT_ID).is_some(), "root must survive forget");
        mgr.batch_forget(&[(FUSE_ROOT_ID, u64::MAX)]);
        assert!(
            mgr.get(FUSE_ROOT_ID).is_some(),
            "root must survive batch_forget"
        );
    }

    #[test]
    fn allocate_returns_same_ino_for_same_path() {
        let mgr = InodeManager::new();
        let a = mgr.allocate("/skills/foo", FileType::Directory, FUSE_ROOT_ID);
        let b = mgr.allocate("/skills/foo", FileType::Directory, FUSE_ROOT_ID);
        assert_eq!(a, b);
    }

    #[test]
    fn remember_increments_lookup_count() {
        let mgr = InodeManager::new();
        let ino = mgr.allocate("/skills/foo", FileType::Directory, FUSE_ROOT_ID);
        assert_eq!(mgr.lookup_count(ino), 0);
        mgr.remember(ino);
        assert_eq!(mgr.lookup_count(ino), 1);
        mgr.remember(ino);
        assert_eq!(mgr.lookup_count(ino), 2);
    }

    #[test]
    fn forget_decrements_and_releases_at_zero() {
        let mgr = InodeManager::new();
        let ino = mgr.allocate("/skills/bar", FileType::RegularFile, FUSE_ROOT_ID);
        mgr.remember(ino);
        mgr.remember(ino);
        assert_eq!(mgr.lookup_count(ino), 2);

        mgr.forget(ino, 1);
        assert_eq!(mgr.lookup_count(ino), 1);
        assert!(mgr.get(ino).is_some(), "still alive at count=1");

        mgr.forget(ino, 1);
        assert_eq!(mgr.lookup_count(ino), 0);
        assert!(mgr.get(ino).is_none(), "released at count=0");
        assert!(mgr.lookup_by_path("/skills/bar").is_none());
    }

    #[test]
    fn forget_saturates_at_zero() {
        let mgr = InodeManager::new();
        let ino = mgr.allocate("/skills/sat", FileType::RegularFile, FUSE_ROOT_ID);
        mgr.remember(ino);
        mgr.forget(ino, 100);
        assert!(mgr.get(ino).is_none());
    }

    #[test]
    fn batch_forget_releases_multiple() {
        let mgr = InodeManager::new();
        let a = mgr.allocate("/a", FileType::RegularFile, FUSE_ROOT_ID);
        let b = mgr.allocate("/b", FileType::RegularFile, FUSE_ROOT_ID);
        mgr.remember(a);
        mgr.remember(b);
        mgr.batch_forget(&[(a, 1), (b, 1)]);
        assert!(mgr.get(a).is_none());
        assert!(mgr.get(b).is_none());
    }

    #[test]
    fn rename_recursive_updates_paths_and_inodes_consistently() {
        let mgr = InodeManager::new();
        let parent = mgr.allocate("/skills/old", FileType::Directory, FUSE_ROOT_ID);
        let child = mgr.allocate("/skills/old/file.md", FileType::RegularFile, parent);

        mgr.rename_path("/skills/old", "/skills/new");

        assert!(mgr.lookup_by_path("/skills/old").is_none());
        assert!(mgr.lookup_by_path("/skills/old/file.md").is_none());
        assert_eq!(mgr.lookup_by_path("/skills/new"), Some(parent));
        assert_eq!(mgr.lookup_by_path("/skills/new/file.md"), Some(child));
        assert_eq!(mgr.get_path(parent).as_deref(), Some("/skills/new"));
        assert_eq!(mgr.get_path(child).as_deref(), Some("/skills/new/file.md"));
    }

    #[test]
    fn remove_recursive_cleans_up_counts() {
        let mgr = InodeManager::new();
        let parent = mgr.allocate("/skills/rm", FileType::Directory, FUSE_ROOT_ID);
        let child = mgr.allocate("/skills/rm/x", FileType::RegularFile, parent);
        mgr.remember(parent);
        mgr.remember(child);

        mgr.remove_recursive("/skills/rm");

        assert!(mgr.get(parent).is_none());
        assert!(mgr.get(child).is_none());
        assert_eq!(mgr.lookup_count(parent), 0);
        assert_eq!(mgr.lookup_count(child), 0);
    }

    #[test]
    fn readdir_ino_returns_existing_without_growing_map() {
        let mgr = InodeManager::new();
        let ino = mgr.allocate("/skills/foo", FileType::Directory, FUSE_ROOT_ID);
        let readdir_result = mgr.readdir_ino("/skills/foo");
        assert_eq!(readdir_result, ino, "existing path returns same ino");
    }

    #[test]
    fn readdir_ino_does_not_persist_new_path() {
        let mgr = InodeManager::new();
        let ino = mgr.readdir_ino("/skills/ephemeral");
        assert!(ino >= 2, "returns a valid ino");
        assert!(
            mgr.get(ino).is_none(),
            "readdir_ino must not persist the mapping"
        );
        assert!(mgr.lookup_by_path("/skills/ephemeral").is_none());
    }

    #[test]
    fn concurrent_operations_do_not_deadlock() {
        use std::sync::Arc;

        let mgr = Arc::new(InodeManager::new());
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let mut handles = Vec::new();

        for t in 0..4 {
            let mgr = Arc::clone(&mgr);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for i in 0..200 {
                    let path = format!("/t{t}/item{i}");
                    let ino = mgr.allocate(&path, FileType::RegularFile, FUSE_ROOT_ID);
                    mgr.remember(ino);
                    let _ = mgr.get(ino);
                    let _ = mgr.get_path(ino);
                    let _ = mgr.lookup_by_path(&path);
                    if i % 3 == 0 {
                        mgr.forget(ino, 1);
                    }
                    if i % 7 == 0 {
                        let old = format!("/t{t}/item{i}");
                        let new = format!("/t{t}/renamed{i}");
                        mgr.rename_path(&old, &new);
                    }
                    if i % 11 == 0 {
                        mgr.remove_recursive(&format!("/t{t}/item{i}"));
                    }
                }
            }));
        }

        for h in handles {
            h.join()
                .unwrap_or_else(|_| panic!("thread panicked or deadlocked"));
        }

        // Verify root still intact.
        assert!(mgr.get(FUSE_ROOT_ID).is_some());
    }
}
