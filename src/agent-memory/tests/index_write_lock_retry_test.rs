//! Writer timeouts must recover without another file event or server restart.

use std::time::{Duration, Instant};

use agent_memory::{
    config::AppConfig, index::IndexHandle, mount::MountStrategyKind, service::MemoryService,
};

#[test]
fn startup_and_incremental_writer_timeouts_retry_without_file_changes() {
    for startup in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.global.user_id = "write-lock-retry".into();
        config.memory.paths.base_dir = tmp.path().join("data").to_string_lossy().into_owned();
        config.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into_owned();
        config.memory.mount.strategy = MountStrategyKind::Userland;
        config.memory.index.enabled = false;
        config.memory.git.enabled = false;
        config.memory.git.auto_commit = false;
        let mut svc = MemoryService::new(config).unwrap();
        svc.index = Some(
            IndexHandle::open(&svc.mount, None, 0.0, 0.0, false)
                .unwrap()
                .into(),
        );
        let db = svc.index.as_ref().unwrap().db_path().to_owned();
        if startup {
            drop(svc.index.take());
        }
        let holder = rusqlite::Connection::open(db).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = Instant::now();
        std::fs::write(svc.mount.root.join("locked.md"), "quartzretry").unwrap();
        if startup {
            svc.index = Some(
                IndexHandle::open(&svc.mount, None, 0.0, 0.0, false)
                    .unwrap()
                    .into(),
            );
        }

        // Keep the real writer locked past SQLite's five-second busy timeout.
        let deadline = started + Duration::from_secs(15);
        loop {
            if let Err(error) = svc.memory_search("quartzretry", 10, Some("bm25"), None, None) {
                assert!(error.to_string().contains("index is incomplete"), "{error}");
                break;
            }
            assert!(Instant::now() < deadline, "writer failure was not reported");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(started.elapsed() >= Duration::from_secs(5));
        holder.execute_batch("ROLLBACK").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(hits) = svc.memory_search("quartzretry", 10, Some("bm25"), None, None) {
                assert_eq!(hits.len(), 1, "recovery must not report partial success");
                assert_eq!(hits[0].path, "locked.md");
                break;
            }
            assert!(Instant::now() < deadline, "unlocked file was never retried");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(svc.read("locked.md").unwrap(), "quartzretry");
    }
}
