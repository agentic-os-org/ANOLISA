//! File changes during startup embedding must reach the real index watcher.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use agent_memory::config::AppConfig;
use agent_memory::embedding::{Embedding, EmbeddingProvider};
use agent_memory::index::IndexHandle;
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;
use async_trait::async_trait;

struct StartupWriter {
    root: PathBuf,
    changed: AtomicBool,
}

#[async_trait]
impl EmbeddingProvider for StartupWriter {
    async fn embed(&self, text: &str) -> agent_memory::error::Result<Embedding> {
        // Backfill starts only after the initial keyword scan has finished.
        // These writes therefore model another client updating the shared
        // store while the startup embedding request is outstanding.
        if !self.changed.swap(true, Ordering::SeqCst) {
            std::fs::write(self.root.join("changed.md"), "newcobalt")?;
            std::fs::remove_file(self.root.join("deleted.md"))?;
            std::fs::create_dir(self.root.join("incoming"))?;
            std::fs::write(self.root.join("incoming/created.md"), "newheliotrope")?;
        }
        Ok(Embedding {
            vector: if text.starts_with("new") {
                vec![0.0, 1.0]
            } else {
                vec![1.0, 0.0]
            },
        })
    }

    fn dimensions(&self) -> usize {
        2
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_backfill_does_not_lose_file_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.global.user_id = "tester".into();
    config.memory.paths.base_dir = tmp.path().to_string_lossy().into_owned();
    config.memory.session.base_dir = tmp
        .path()
        .join("__sessions__")
        .to_string_lossy()
        .into_owned();
    config.memory.mount.strategy = MountStrategyKind::Userland;
    config.memory.index.enabled = false;
    let svc = MemoryService::new(config).unwrap();
    svc.write("changed.md", "oldcobalt", false).unwrap();
    svc.write("deleted.md", "oldheliotrope", false).unwrap();

    let provider = Arc::new(StartupWriter {
        root: svc.mount.root.clone(),
        changed: AtomicBool::new(false),
    });
    let index = IndexHandle::open(&svc.mount, Some(provider.clone()), 0.0, 0.0, false).unwrap();
    assert!(provider.changed.load(Ordering::SeqCst));
    assert_eq!(svc.read("changed.md").unwrap(), "newcobalt");
    assert_eq!(svc.read("incoming/created.md").unwrap(), "newheliotrope");

    // No later filesystem write or explicit rescan can repair a missed event.
    // Poll the production search interface across the normal debounce window.
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let mut old = index.search("oldcobalt", 10).unwrap();
        old.extend(index.search("oldheliotrope", 10).unwrap());
        let changed = index.search("newcobalt", 10).unwrap();
        let created = index.search("newheliotrope", 10).unwrap();
        let vectors = index.search_vec(&[0.0, 1.0], 10).unwrap();
        if old.is_empty()
            && changed.iter().any(|hit| hit.path == "changed.md")
            && created.iter().any(|hit| hit.path == "incoming/created.md")
            && vectors
                .iter()
                .any(|hit| hit.path == "changed.md" && hit.score > 0.9)
            && vectors
                .iter()
                .any(|hit| hit.path == "incoming/created.md" && hit.score > 0.9)
            && !vectors.iter().any(|hit| hit.path == "deleted.md")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "startup changes were lost: old={old:?}, changed={changed:?}, created={created:?}, vectors={vectors:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
