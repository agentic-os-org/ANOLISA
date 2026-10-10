//! Background store-sync worker: receives debounced events from the
//! FUSE write path and re-parses affected `SKILL.md` files into the
//! shared store.

use std::collections::HashMap;
use std::path::PathBuf;

use skillfs_core::{SharedSkillStore, parser, store::adopt_directory_name};
use tracing::{info, warn};

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
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            // Collect more events within a 50 ms window (debounce).
            let mut pending: HashMap<String, SyncEvent> = HashMap::new();
            match &first {
                SyncEvent::Reparse { skill_name, .. } => {
                    pending.insert(skill_name.clone(), first);
                }
            }
            while let Ok(ev) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
                match &ev {
                    SyncEvent::Reparse { skill_name, .. } => {
                        pending.insert(skill_name.clone(), ev);
                    }
                }
            }

            // Process the batch.
            for (_skill_name, event) in pending {
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
                                info!(
                                    name = %skill_name,
                                    "sync: re-parsed SKILL.md"
                                );
                                store.write().upsert(entry);
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
        let worker = spawn_sync_worker(rx, store.clone());
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
        let worker = spawn_sync_worker(rx, shared.clone());
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
            let worker = spawn_sync_worker(rx, store.clone());
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
