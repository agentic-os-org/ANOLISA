//! Background store-sync worker: receives debounced events from the
//! FUSE write path and re-parses affected `SKILL.md` files into the
//! shared store.

use std::collections::HashMap;
use std::path::PathBuf;

use skillfs_core::{SharedSkillStore, parser};
use tracing::{info, warn};

/// Events sent from FUSE write callbacks to the background sync task.
#[derive(Debug)]
pub(crate) enum SyncEvent {
    /// Re-parse a skill's SKILL.md after write/create.
    Reparse {
        skill_name: String,
        source_path: PathBuf,
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
                    } => {
                        match parser::parse_skill_file(source_path) {
                            Ok(mut entry) => {
                                // The directory name is the authoritative store key.
                                // Override metadata.name so that a stale frontmatter
                                // `name:` field (e.g. after a rename) can never
                                // re-insert an entry under the old name.
                                entry.metadata.name = skill_name.clone();
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
}
