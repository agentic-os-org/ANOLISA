//! Background store-sync worker: receives debounced events from the
//! FUSE write path and re-parses affected `SKILL.md` files into the
//! shared store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skillfs_core::{SharedSkillStore, parser, store::adopt_directory_name};
use tracing::{info, warn};

/// The in-place mount's source-address alias pair: re-parse I/O goes
/// through the `/proc/self/fd/{n}` dirfd (bypassing the FUSE over-mount)
/// while the store's `source_path` identity and ordering use the
/// canonical source-root spelling the initial scan recorded.
///
/// Without the rewrite one manifest would look like two different
/// sources — `insert_discovered` compares spellings — and the
/// duplicate-name rule would drop or steal the post-write refresh by
/// alias ordering, diverging the live store from a fresh scan.
#[derive(Clone)]
pub(crate) struct SourceAlias {
    alias_root: PathBuf,
    canonical_root: PathBuf,
}

impl SourceAlias {
    pub(crate) fn new(alias_root: PathBuf, canonical_root: PathBuf) -> Self {
        Self {
            alias_root,
            canonical_root,
        }
    }

    /// Canonical spelling of a physical path: the fd-alias prefix is
    /// rewritten to the source root; every other spelling passes through.
    fn canonicalize(&self, physical: &Path) -> PathBuf {
        match physical.strip_prefix(&self.alias_root) {
            Ok(rest) => self.canonical_root.join(rest),
            Err(_) => physical.to_path_buf(),
        }
    }
}

/// Events sent from FUSE write callbacks to the background sync task.
#[derive(Debug)]
pub(crate) enum SyncEvent {
    /// Re-parse a skill's SKILL.md after write/create.
    Reparse {
        skill_name: String,
        source_path: PathBuf,
        /// The mount's `max_skill_size`, so a reparse refuses exactly what
        /// the initial load and the read paths refuse.
        max_skill_size: u64,
    },
}

/// Spawn the background store-sync worker thread.
///
/// Collects events from the FUSE write path, batches them with a 50 ms
/// debounce window, then re-parses the affected SKILL.md files and updates
/// the shared store.
pub(crate) fn spawn_sync_worker(
    rx: std::sync::mpsc::Receiver<SyncEvent>,
    store: SharedSkillStore,
    alias: Option<SourceAlias>,
    staging_matcher: Option<std::sync::Arc<crate::security::StagingMatcher>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // I2: a staging-private incumbent never legitimately owns its
        // leaf name — see `upsert_checked_replacing_staging`.
        let is_staging_root = |name: &str| {
            staging_matcher
                .as_ref()
                .is_some_and(|m| m.is_staging_root(name))
        };
        while let Ok(first) = rx.recv() {
            // Collect more events within a 50 ms window (debounce).
            // The batch keys every event by its manifest source path:
            // nested candidates share their leaf name across categories,
            // and coalescing by the leaf alone overwrote one candidate's
            // event with another's before either reached the collision
            // check - the live store then selected the wrong candidate,
            // or an existing winner's refresh was dropped and the entry
            // kept serving its pre-write body, while a fresh scan of the
            // same tree disagrees. The source path identifies the
            // manifest, so every distinct candidate reaches the
            // collision check; repeated events for the same manifest
            // still coalesce (the re-parse reads the file from disk, so
            // the last one suffices).
            let mut pending: HashMap<PathBuf, SyncEvent> = HashMap::new();
            match &first {
                SyncEvent::Reparse { source_path, .. } => {
                    pending.insert(source_path.clone(), first);
                }
            }
            while let Ok(ev) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
                match &ev {
                    SyncEvent::Reparse { source_path, .. } => {
                        pending.insert(source_path.clone(), ev);
                    }
                }
            }

            // Process the batch.
            for (_source_path, event) in pending {
                match event {
                    SyncEvent::Reparse {
                        ref skill_name,
                        ref source_path,
                        max_skill_size,
                    } => {
                        let limit = usize::try_from(max_skill_size).unwrap_or(usize::MAX);
                        match parser::parse_skill_file_with_limit(source_path, limit) {
                            Ok(mut entry) => {
                                // The directory name is the authoritative store
                                // key, and adopting it goes through the same
                                // shared validation as the loaders: a stale
                                // frontmatter `name:` (e.g. after a rename) can
                                // never re-insert an entry under the old name,
                                // and a write under a non-conforming directory
                                // cannot overwrite the Degraded entry the
                                // initial scan produced with a clean one.
                                adopt_directory_name(&mut entry, skill_name);
                                // The event's source_path addresses the
                                // source through the in-place fd alias
                                // (I/O must bypass the FUSE over-mount);
                                // the store entry carries the canonical
                                // source-root spelling so identity and
                                // ordering match the initial scan.
                                if let Some(alias) = &alias {
                                    entry.source_path = alias.canonicalize(&entry.source_path);
                                }
                                info!(
                                    name = %skill_name,
                                    "sync: re-parsed SKILL.md"
                                );
                                if let Some(error) = store
                                    .write()
                                    .upsert_checked_replacing_staging(entry, &is_staging_root)
                                {
                                    warn!(
                                        name = %skill_name,
                                        dropped = %error.path.display(),
                                        "sync: duplicate skill name — kept the \
                                         lexicographically smaller source path"
                                    );
                                }
                            }
                            Err(e) => {
                                warn!(
                                    name = %skill_name,
                                    error = %e,
                                    "sync: re-parse failed"
                                );
                            }
                        }
                    }
                }
            }
        }
        info!("sync worker exiting");
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::RwLock;
    use skillfs_core::store::SkillStore;

    use super::*;

    #[test]
    fn reparse_uses_the_event_source_path() {
        let source = tempfile::tempdir().expect("source tempdir");
        let md_path = source.path().join("catalog/demo/SKILL.md");
        std::fs::create_dir_all(md_path.parent().expect("skill parent"))
            .expect("categorized skill dir");
        std::fs::write(
            &md_path,
            "---\nname: stale\ndescription: categorized\n---\nupdated body\n",
        )
        .expect("categorized SKILL.md");

        let store = Arc::new(RwLock::new(SkillStore::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = spawn_sync_worker(rx, store.clone(), None, None);
        tx.send(SyncEvent::Reparse {
            skill_name: "demo".to_string(),
            source_path: md_path.clone(),
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send reparse");
        drop(tx);
        worker.join().expect("sync worker");

        let guard = store.read();
        let entry = guard.get("demo").expect("reparsed store entry");
        assert_eq!(entry.metadata.name, "demo");
        assert_eq!(entry.source_path, md_path);
        assert!(entry.body.contains("updated body"));
    }

    #[test]
    fn reparse_rewrites_the_fd_alias_to_the_canonical_root() {
        // In an in-place mount the write paths enqueue the re-parse with
        // the `/proc/self/fd/{n}` alias spelling, while the initial scan
        // recorded the real source root. The worker must insert under the
        // canonical spelling, or the store's duplicate-name rule would
        // compare one manifest's two spellings as colliding sources and
        // drop or steal the post-write refresh by alias ordering.
        let source = tempfile::tempdir().expect("source tempdir");
        let md_path = source.path().join("notes/SKILL.md");
        std::fs::create_dir_all(md_path.parent().expect("skill parent")).expect("skill dir");
        std::fs::write(
            &md_path,
            "---\nname: notes\ndescription: demo\n---\nupdated body\n",
        )
        .expect("SKILL.md");

        let mut initial = SkillStore::new();
        initial.load_from_directory(source.path(), &skillfs_core::ParseConfig::default());
        let store = Arc::new(RwLock::new(initial));

        // A real fd alias over the source root, as the in-place mount
        // pre-opens it. The dirfd must outlive the worker's parse.
        let dirfd = std::fs::File::open(source.path()).expect("source dirfd");
        use std::os::unix::io::AsRawFd;
        let alias_root = PathBuf::from(format!("/proc/self/fd/{}", dirfd.as_raw_fd()));
        let alias = SourceAlias::new(alias_root.clone(), source.path().to_path_buf());

        let (tx, rx) = std::sync::mpsc::channel();
        let worker = spawn_sync_worker(rx, store.clone(), Some(alias), None);
        tx.send(SyncEvent::Reparse {
            skill_name: "notes".to_string(),
            source_path: alias_root.join("notes/SKILL.md"),
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send reparse");
        drop(tx);
        worker.join().expect("sync worker");

        let guard = store.read();
        let entry = guard.get("notes").expect("reparsed store entry");
        assert_eq!(
            entry.source_path, md_path,
            "the fd alias must not leak into the store's source identity"
        );
        assert!(entry.body.contains("updated body"));
    }

    #[test]
    fn reparse_of_a_colliding_leaf_keeps_the_discovery_winner() {
        // The sync worker re-parses SKILL.md on write events and inserts
        // the entry under the event's bare leaf name. When a different
        // directory shares that leaf, the blind upsert silently steals the
        // discovered skill's store key — the same collision the loaders
        // report (ee514e2b4), which must be decided by the same rule here.
        let source = tempfile::tempdir().expect("source tempdir");
        let winner_md = source.path().join("apple/notes/SKILL.md");
        std::fs::create_dir_all(winner_md.parent().expect("skill parent"))
            .expect("category skill dir");
        std::fs::write(
            &winner_md,
            "---\nname: notes\ndescription: from apple\n---\nbody\n",
        )
        .expect("discovered SKILL.md");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &skillfs_core::ParseConfig::default());
        assert_eq!(
            store.get("notes").expect("discovered").source_path,
            winner_md
        );

        // A different directory with the same leaf appears later (a write
        // through the mount lands here as a Reparse event).
        let loser_md = source.path().join("banana/notes/SKILL.md");
        std::fs::create_dir_all(loser_md.parent().expect("skill parent"))
            .expect("colliding skill dir");
        std::fs::write(
            &loser_md,
            "---\nname: notes\ndescription: from banana\n---\nbody\n",
        )
        .expect("colliding SKILL.md");

        let shared = Arc::new(RwLock::new(store));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = spawn_sync_worker(rx, shared.clone(), None, None);
        tx.send(SyncEvent::Reparse {
            skill_name: "notes".to_string(),
            source_path: loser_md,
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send reparse");
        drop(tx);
        worker.join().expect("sync worker");

        let guard = shared.read();
        assert_eq!(
            guard.get("notes").expect("key still held").source_path,
            winner_md,
            "a re-parse of a different directory with the same leaf must \
             not silently steal the store key"
        );
    }

    #[test]
    fn same_batch_same_leaf_events_both_reach_the_collision_check() {
        // Two nested candidates share the leaf `newbie`. Their events land
        // in one debounce window, and coalescing by the leaf name alone
        // overwrote the alpha event with the beta one before either
        // reached the collision check - the live store then kept
        // beta/newbie while a fresh scan keeps alpha/newbie. The batch
        // must key every event by its manifest source path instead.
        let source = tempfile::tempdir().expect("source tempdir");
        for (dir, desc) in [("alpha/newbie", "alpha"), ("beta/newbie", "beta")] {
            std::fs::create_dir_all(source.path().join(dir)).expect("skill dir");
            std::fs::write(
                source.path().join(dir).join("SKILL.md"),
                format!("---\nname: newbie\ndescription: {desc}\n---\n{desc} body\n"),
            )
            .expect("SKILL.md");
        }

        let shared = Arc::new(RwLock::new(SkillStore::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = spawn_sync_worker(rx, shared.clone(), None, None);
        tx.send(SyncEvent::Reparse {
            skill_name: "newbie".to_string(),
            source_path: source.path().join("alpha/newbie/SKILL.md"),
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send the alpha reparse");
        tx.send(SyncEvent::Reparse {
            skill_name: "newbie".to_string(),
            source_path: source.path().join("beta/newbie/SKILL.md"),
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send the beta reparse");
        drop(tx);
        worker.join().expect("sync worker");

        let guard = shared.read();
        let entry = guard.get("newbie").expect("leaf entry");
        assert_eq!(
            entry.source_path,
            source.path().join("alpha/newbie/SKILL.md"),
            "both same-batch candidates must reach the collision check, \
             which keeps the fresh-scan winner"
        );
        assert!(entry.body.contains("alpha body"));
    }

    #[test]
    fn same_batch_winner_and_loser_edits_refresh_the_winner() {
        // The discovered winner alpha/newbie sits in the store with its
        // old body; one batch carries the winner's refresh and the
        // loser's edit. Coalescing by leaf dropped the winner's refresh,
        // leaving the live entry serving its pre-write body while the
        // manifest on disk had moved on. Keying by source path keeps
        // both: the winner refreshes, the loser still loses the
        // collision check.
        let source = tempfile::tempdir().expect("source tempdir");
        let winner_md = source.path().join("alpha/newbie/SKILL.md");
        std::fs::create_dir_all(winner_md.parent().expect("winner dir")).expect("winner dir");
        std::fs::write(
            &winner_md,
            "---\nname: newbie\ndescription: alpha\n---\noriginal body\n",
        )
        .expect("winner SKILL.md");
        let loser_md = source.path().join("beta/newbie/SKILL.md");
        std::fs::create_dir_all(loser_md.parent().expect("loser dir")).expect("loser dir");
        std::fs::write(
            &loser_md,
            "---\nname: newbie\ndescription: beta\n---\nbeta body\n",
        )
        .expect("loser SKILL.md");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &skillfs_core::ParseConfig::default());
        assert_eq!(
            store.get("newbie").expect("discovered").source_path,
            winner_md
        );

        let shared = Arc::new(RwLock::new(store));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = spawn_sync_worker(rx, shared.clone(), None, None);
        // The winner's manifest gained a line on disk...
        std::fs::write(
            &winner_md,
            "---\nname: newbie\ndescription: alpha\n---\nrefreshed body\n",
        )
        .expect("refresh the winner SKILL.md");
        tx.send(SyncEvent::Reparse {
            skill_name: "newbie".to_string(),
            source_path: winner_md.clone(),
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send the winner reparse");
        tx.send(SyncEvent::Reparse {
            skill_name: "newbie".to_string(),
            source_path: loser_md,
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send the loser reparse");
        drop(tx);
        worker.join().expect("sync worker");

        let guard = shared.read();
        let entry = guard.get("newbie").expect("winner entry");
        assert_eq!(
            entry.source_path, winner_md,
            "the winner keeps the key through the same-batch edits"
        );
        assert!(
            entry.body.contains("refreshed body"),
            "the winner's same-batch refresh must not be coalesced away, \
             body is {:?}",
            entry.body
        );
    }

    #[test]
    fn reparse_cannot_clear_invalid_dir_name_degradation() {
        let source = tempfile::tempdir().expect("source tempdir");
        // Valid frontmatter under a directory whose name violates the
        // grammar: the initial scan leaves this entry Degraded, and a
        // write through the mount point (which lands here as a Reparse
        // event) must not overwrite it with a clean entry.
        let md_path = source.path().join("foo_bar/SKILL.md");
        std::fs::create_dir_all(md_path.parent().expect("skill parent")).expect("skill dir");
        std::fs::write(
            &md_path,
            "---\nname: foo-bar\ndescription: valid frontmatter\n---\nbody\n",
        )
        .expect("SKILL.md");
        // Control case: a conforming directory re-parses clean.
        let ok_path = source.path().join("good-skill/SKILL.md");
        std::fs::create_dir_all(ok_path.parent().expect("skill parent")).expect("skill dir");
        std::fs::write(
            &ok_path,
            "---\nname: good-skill\ndescription: fine\n---\nbody\n",
        )
        .expect("SKILL.md");

        let shared = Arc::new(RwLock::new(SkillStore::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = spawn_sync_worker(rx, shared.clone(), None, None);
        tx.send(SyncEvent::Reparse {
            skill_name: "foo_bar".to_string(),
            source_path: md_path,
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send reparse");
        tx.send(SyncEvent::Reparse {
            skill_name: "good-skill".to_string(),
            source_path: ok_path,
            max_skill_size: skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64,
        })
        .expect("send reparse");
        drop(tx);
        worker.join().expect("sync worker");

        let guard = shared.read();
        let degraded = guard.get("foo_bar").expect("reparsed entry");
        assert_eq!(degraded.metadata.name, "foo_bar");
        assert!(
            degraded.parse_status.is_degraded(),
            "sync re-parse must keep the invalid directory name degraded, got {:?}",
            degraded.parse_status
        );
        assert!(degraded.parse_status.message().contains("foo_bar"));
        let clean = guard.get("good-skill").expect("control entry");
        assert!(
            clean.parse_status.is_ok(),
            "conforming directory must re-parse clean, got {:?}",
            clean.parse_status
        );
    }

    /// The reparse applies the mount's limit carried by the event, not the
    /// parser default: a 2 KiB SKILL.md under a 1 KiB mount limit is refused
    /// (the store keeps nothing), the same file under the default is loaded.
    #[test]
    fn reparse_applies_the_event_size_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let md_path = tmp.path().join("big").join("SKILL.md");
        std::fs::create_dir_all(md_path.parent().expect("skill parent")).expect("skill dir");
        let mut content = String::from("---\nname: big\ndescription: sized\n---\n");
        content.push_str(&"a".repeat(2048));
        std::fs::write(&md_path, &content).expect("SKILL.md");

        for (limit, loaded) in [
            (1024, false),
            (skillfs_core::DEFAULT_MAX_SKILL_SIZE as u64, true),
        ] {
            let store = Arc::new(RwLock::new(SkillStore::new()));
            let (tx, rx) = std::sync::mpsc::channel();
            let worker = spawn_sync_worker(rx, store.clone(), None, None);
            tx.send(SyncEvent::Reparse {
                skill_name: "big".to_string(),
                source_path: md_path.clone(),
                max_skill_size: limit,
            })
            .expect("send reparse");
            drop(tx);
            worker.join().expect("sync worker");
            assert_eq!(store.read().get("big").is_some(), loaded, "limit {limit}");
        }
    }
}
