//! Mount-local, bounded reuse of exact transformed SKILL.md results.

use std::collections::VecDeque;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::security::ActiveTarget;

#[derive(Clone, PartialEq, Eq)]
struct SourceIdentity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl From<&Metadata> for SourceIdentity {
    fn from(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

#[derive(PartialEq, Eq)]
struct Key {
    skill: String,
    path: PathBuf,
    target: Option<ActiveTarget>,
    source: SourceIdentity,
    source_digest: [u8; 32],
    pipeline: [u8; 32],
}

struct Entry {
    key: Key,
    content: Arc<str>,
}

#[derive(Default)]
struct State {
    entries: VecDeque<Entry>,
    bytes: usize,
    #[cfg(test)]
    events: [usize; 4],
}

impl State {
    fn event(&mut self, event: usize) {
        let name = ["hit", "miss", "invalidation", "eviction"][event];
        debug!(
            cache = "transformed_skill_md",
            event = name,
            entries = self.entries.len(),
            bytes = self.bytes
        );
        #[cfg(test)]
        {
            self.events[event] += 1;
        }
    }
}

pub(super) struct TransformCache {
    state: Mutex<State>,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for TransformCache {
    fn default() -> Self {
        Self::new(128, 8 * 1024 * 1024)
    }
}

impl TransformCache {
    #[cfg(test)]
    pub(super) fn events(&self) -> [usize; 4] {
        self.state.lock().events
    }

    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(State::default()),
            max_entries,
            max_bytes,
        }
    }

    pub(super) fn load(
        &self,
        skill: &str,
        path: &Path,
        target: Option<&ActiveTarget>,
        pipeline: [u8; 32],
        transform: impl FnOnce(&str) -> String,
    ) -> io::Result<(Arc<str>, Metadata)> {
        // Open before checking the key: atomic replacement must select a new inode.
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        // Timestamps can collide even at nanosecond API precision. Read and
        // hash the selected bytes before reuse, rather than trusting stat alone.
        let mut raw = String::new();
        file.read_to_string(&mut raw)?;
        let key = Key {
            skill: skill.into(),
            path: path.into(),
            target: target.cloned(),
            source: SourceIdentity::from(&metadata),
            source_digest: Sha256::digest(raw.as_bytes()).into(),
            pipeline,
        };
        {
            let mut state = self.state.lock();
            // ponytail: linear lookup is bounded to 128 entries; index if this grows.
            if let Some(index) = state.entries.iter().position(|entry| entry.key == key) {
                if let Some(entry) = state.entries.remove(index) {
                    let content = entry.content.clone();
                    state.entries.push_back(entry);
                    state.event(0);
                    return Ok((content, metadata));
                }
            }
            let before = state.entries.len();
            state.entries.retain(|entry| {
                !(entry.key.skill == key.skill
                    && entry.key.path == key.path
                    && entry.key.target == key.target)
            });
            if state.entries.len() != before {
                state.bytes = state.entries.iter().map(|entry| entry.content.len()).sum();
                state.event(2);
            }
            state.event(1);
        }
        // No cache lock is held across file I/O or transformation.
        let content: Arc<str> = transform(&raw).into();
        let unchanged = SourceIdentity::from(&file.metadata()?) == key.source
            && std::fs::metadata(path).is_ok_and(|meta| SourceIdentity::from(&meta) == key.source);
        let mut state = self.state.lock();
        if !unchanged {
            state.event(2);
        } else if self.max_entries > 0 && content.len() <= self.max_bytes {
            // A concurrent miss may already have populated this exact key.
            if let Some(entry) = state.entries.iter().find(|entry| entry.key == key) {
                return Ok((entry.content.clone(), metadata));
            }
            while state.entries.len() >= self.max_entries
                || state.bytes > self.max_bytes - content.len()
            {
                if let Some(entry) = state.entries.pop_front() {
                    state.bytes -= entry.content.len();
                    state.event(3);
                }
            }
            state.bytes += content.len();
            state.entries.push_back(Entry {
                key,
                content: content.clone(),
            });
        }
        Ok((content, metadata))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn exact_reuse_and_every_key_dimension() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(&path, "original").unwrap();
        let cache = TransformCache::default();
        let calls = Cell::new(0);
        let run = |raw: &str| {
            calls.set(calls.get() + 1);
            raw.to_uppercase()
        };
        let original = cache.load("web", &path, None, [1; 32], run).unwrap().0;
        let again = cache.load("web", &path, None, [1; 32], run).unwrap().0;
        assert!(Arc::ptr_eq(&original, &again));
        assert_eq!(calls.get(), 1);
        cache.load("other", &path, None, [1; 32], run).unwrap();
        cache.load("web", &path, None, [2; 32], run).unwrap();
        let current = ActiveTarget::Current {
            source_dir: tmp.path().into(),
        };
        cache
            .load("web", &path, Some(&current), [1; 32], run)
            .unwrap();
        for version in ["v1", "v2"] {
            let target = ActiveTarget::Snapshot {
                snapshot_dir: tmp.path().into(),
                version: version.into(),
            };
            cache
                .load("web", &path, Some(&target), [1; 32], run)
                .unwrap();
        }
        let alias = tmp.path().join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        cache.load("web", &alias, None, [1; 32], run).unwrap();
        assert_eq!(calls.get(), 7);
        assert_eq!(&*original, "ORIGINAL");
        let state = cache.state.lock();
        assert_eq!(state.events[0], 1);
        assert_eq!(state.events[1], 7);
        assert_eq!(state.events[2], 1);
    }

    #[test]
    fn content_changes_invalidate_even_when_all_stat_fields_match() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(&path, "old").unwrap();
        let cache = TransformCache::default();
        let calls = Cell::new(0);
        let run = |raw: &str| {
            calls.set(calls.get() + 1);
            raw.to_uppercase()
        };
        let old = cache.load("web", &path, None, [1; 32], run).unwrap().0;
        std::fs::write(&path, "new").unwrap();
        // Model the timestamp collision observed on CI deterministically,
        // without depending on the host filesystem's timestamp resolution.
        cache.state.lock().entries[0].key.source =
            SourceIdentity::from(&std::fs::metadata(&path).unwrap());
        let new = cache.load("web", &path, None, [1; 32], run).unwrap().0;
        assert_eq!(&*old, "OLD");
        assert_eq!(&*new, "NEW");
        assert_eq!(calls.get(), 2);
        let again = cache.load("web", &path, None, [1; 32], run).unwrap().0;
        assert!(Arc::ptr_eq(&new, &again));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn source_updates_and_population_races_do_not_publish_stale_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(&path, "old").unwrap();
        let cache = TransformCache::default();
        let read = || {
            cache
                .load("web", &path, None, [1; 32], str::to_owned)
                .unwrap()
                .0
        };
        let old = read();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "new").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        assert_eq!(
            &*read(),
            "new",
            "same-size edits with restored mtime must invalidate reuse"
        );
        let next = tmp.path().join("next");
        std::fs::write(&next, "replacement").unwrap();
        std::fs::rename(&next, &path).unwrap();
        assert_eq!(&*read(), "replacement");
        assert_eq!(&*old, "old");
        for atomic in [false, true] {
            let raced = TransformCache::default();
            let captured = raced
                .load("web", &path, None, [1; 32], |raw| {
                    if atomic {
                        std::fs::write(&next, "atomic").unwrap();
                        std::fs::rename(&next, &path).unwrap();
                    } else {
                        std::fs::write(&path, "in-place").unwrap();
                    }
                    raw.to_owned()
                })
                .unwrap()
                .0;
            assert!(raced.state.lock().entries.is_empty());
            assert_eq!(raced.state.lock().events[2], 1);
            let fresh = raced
                .load("web", &path, None, [1; 32], str::to_owned)
                .unwrap()
                .0;
            assert_ne!(captured, fresh);
        }
        std::fs::remove_file(&path).unwrap();
        assert!(
            cache
                .load("web", &path, None, [1; 32], str::to_owned)
                .is_err()
        );
    }

    #[test]
    fn lru_limits_oversize_and_handle_lifetime() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(&path, "1234").unwrap();
        for (entries, bytes) in [(2, 100), (10, 8)] {
            let cache = TransformCache::new(entries, bytes);
            let get = |id| {
                cache
                    .load(id, &path, None, [0; 32], str::to_owned)
                    .unwrap()
                    .0
            };
            let first = get("a");
            let weak = Arc::downgrade(&first);
            get("b");
            get("a");
            get("c");
            let state = cache.state.lock();
            assert_eq!(
                state
                    .entries
                    .iter()
                    .map(|e| e.key.skill.as_str())
                    .collect::<Vec<_>>(),
                ["a", "c"]
            );
            assert_eq!(state.bytes, 8);
            assert_eq!(state.events[3], 1);
            drop(state);
            get("b");
            get("d");
            assert_eq!(&*first, "1234", "eviction does not revoke captured handles");
            drop(first);
            assert!(weak.upgrade().is_none());
        }
        for (entries, bytes) in [(0, 8), (2, 0), (2, 3)] {
            let cache = TransformCache::new(entries, bytes);
            let result = cache
                .load("a", &path, None, [0; 32], str::to_owned)
                .unwrap()
                .0;
            assert_eq!(&*result, "1234");
            assert!(cache.state.lock().entries.is_empty());
        }
        let cache = TransformCache::default();
        let content = cache
            .load("a", &path, None, [0; 32], str::to_owned)
            .unwrap()
            .0;
        let weak = Arc::downgrade(&content);
        drop(content);
        assert!(weak.upgrade().is_some());
        drop(cache);
        assert!(weak.upgrade().is_none(), "unmount drops retained content");
    }

    #[test]
    fn concurrent_population_keeps_limits_and_deduplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(&path, "1234").unwrap();
        let cache = TransformCache::new(3, 12);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..8 {
                workers.push(scope.spawn(|| {
                    cache
                        .load("shared", &path, None, [0; 32], |raw| {
                            barrier.wait();
                            raw.to_owned()
                        })
                        .unwrap()
                        .0
                }));
            }
            let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
            assert!(results.iter().all(|r| Arc::ptr_eq(r, &results[0])));
        });
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let cache = &cache;
                let path = &path;
                scope.spawn(move || {
                    for i in 0..20 {
                        cache
                            .load(&format!("{worker}/{i}"), path, None, [0; 32], str::to_owned)
                            .unwrap();
                        let state = cache.state.lock();
                        assert!(state.entries.len() <= 3 && state.bytes <= 12);
                        assert_eq!(
                            state.bytes,
                            state.entries.iter().map(|e| e.content.len()).sum::<usize>()
                        );
                    }
                });
            }
        });
    }
}
