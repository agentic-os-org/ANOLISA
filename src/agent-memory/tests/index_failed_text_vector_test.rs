//! Keep keyword and vector content consistent after per-file text write errors.
use agent_memory::embedding::{Embedding, EmbeddingProvider};
use agent_memory::index::IndexHandle;
use agent_memory::{config::AppConfig, mount::MountStrategyKind, service::MemoryService};
use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

const OLD: &str = "oldquartz previous text";
const NEW: &str = "newcobalt replacement with a different length";
struct Provider {
    new_calls: AtomicUsize,
}
#[async_trait]
impl EmbeddingProvider for Provider {
    async fn embed(&self, text: &str) -> agent_memory::error::Result<Embedding> {
        let new = text.contains("newcobalt");
        if new {
            self.new_calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Embedding {
            vector: if new { vec![0.0, 1.0] } else { vec![1.0, 0.0] },
        })
    }
    fn dimensions(&self) -> usize {
        2
    }
}
fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "failed-text-vector-test".into();
    cfg.memory.paths.base_dir = tmp.path().join("data").to_string_lossy().into_owned();
    cfg.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into_owned();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    cfg.memory.git.auto_commit = false;
    let svc = MemoryService::new(cfg).unwrap();
    svc.write("note.md", OLD, false).unwrap();
    (tmp, svc)
}
fn provider() -> Arc<Provider> {
    Arc::new(Provider {
        new_calls: AtomicUsize::new(0),
    })
}
fn inject(db: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_text_update BEFORE UPDATE OF mtime_ms,size ON files WHEN OLD.path = 'note.md' BEGIN SELECT RAISE(ABORT, 'controlled per-file text failure'); END;").unwrap();
    conn
}
async fn until(mut f: impl FnMut() -> bool, message: &str) {
    let end = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < end, "timed out: {message}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
fn note_score(index: &IndexHandle, q: &[f32]) -> Option<f64> {
    index
        .search_vec(q, 10)
        .unwrap()
        .into_iter()
        .find(|h| h.path == "note.md")
        .map(|h| h.score)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_text_failure_must_not_replace_preserved_vector() {
    let (_tmp, svc) = setup();
    let emb = provider();
    let index = IndexHandle::open(&svc.mount, Some(emb.clone()), 0.0, 0.0, false).unwrap();
    assert!(note_score(&index, &[1.0, 0.0]).unwrap() > 0.9);
    let conn = inject(index.db_path());
    std::fs::write(svc.mount.root.join("note.md"), NEW).unwrap();
    until(
        || emb.new_calls.load(Ordering::SeqCst) > 0,
        "watcher must embed changed note",
    )
    .await;
    std::fs::write(svc.mount.root.join("barrier.md"), "barrierfinish").unwrap();
    until(
        || !index.search("barrierfinish", 10).unwrap().is_empty(),
        "later normal write proves watcher progressed",
    )
    .await;
    let old = index.search("oldquartz", 10).unwrap();
    let new = index.search("newcobalt", 10).unwrap();
    let old_score = note_score(&index, &[1.0, 0.0]);
    let new_score = note_score(&index, &[0.0, 1.0]);
    println!(
        "incremental: old_keyword={} new_keyword={} old_vector={old_score:?} new_vector={new_score:?}",
        old.len(),
        new.len()
    );
    assert_eq!(
        old.len(),
        1,
        "trigger must leave the old text transaction intact"
    );
    assert!(new.is_empty());
    conn.execute_batch("DROP TRIGGER fail_text_update;")
        .unwrap();
    std::fs::write(svc.mount.root.join("note.md"), format!("{NEW} recovered")).unwrap();
    until(
        || {
            !index.search("newcobalt", 10).unwrap().is_empty()
                && note_score(&index, &[0.0, 1.0]).is_some_and(|s| s > 0.9)
        },
        "a later successful event must reconcile both sides",
    )
    .await;
    drop(index);
    assert!(
        old_score.is_some_and(|s| s > 0.9) && new_score.is_some_and(|s| s < 0.1),
        "a rolled-back text update must not overwrite its valid old vector"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_text_failure_must_not_backfill_new_body_into_old_row() {
    let (_tmp, svc) = setup();
    svc.write("healthy.md", "unchanged healthy memory", false)
        .unwrap();
    let initial = IndexHandle::open(&svc.mount, None, 0.0, 0.0, false).unwrap();
    let db = initial.db_path().to_owned();
    assert_eq!(initial.search("oldquartz", 10).unwrap().len(), 1);
    drop(initial);
    let _conn = inject(&db);
    std::fs::write(svc.mount.root.join("note.md"), NEW).unwrap();
    let index = IndexHandle::open(&svc.mount, Some(provider()), 0.0, 0.0, false).unwrap();
    let old = index.search("oldquartz", 10).unwrap();
    let new = index.search("newcobalt", 10).unwrap();
    let vector = note_score(&index, &[0.0, 1.0]);
    assert!(
        index
            .search_vec(&[1.0, 0.0], 10)
            .unwrap()
            .iter()
            .any(|hit| hit.path == "healthy.md" && hit.score > 0.9),
        "unaffected rows must still receive their missing vectors"
    );
    println!(
        "startup: old_keyword={} new_keyword={} new_vector={vector:?}",
        old.len(),
        new.len()
    );
    assert_eq!(old.len(), 1);
    assert!(new.is_empty());
    drop(index);
    assert!(
        vector.is_none(),
        "failed text reconciliation must not publish the new content's vector beside the old text"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_update_changes_both_text_and_vector() {
    let (_tmp, svc) = setup();
    let index = IndexHandle::open(&svc.mount, Some(provider()), 0.0, 0.0, false).unwrap();
    assert!(note_score(&index, &[1.0, 0.0]).unwrap() > 0.9);
    std::fs::write(svc.mount.root.join("note.md"), NEW).unwrap();
    until(
        || {
            !index.search("newcobalt", 10).unwrap().is_empty()
                && note_score(&index, &[0.0, 1.0]).is_some_and(|s| s > 0.9)
        },
        "healthy watcher updates both indices",
    )
    .await;
    assert!(index.search("oldquartz", 10).unwrap().is_empty());
    println!(
        "healthy: old_keyword=0 new_keyword=1 new_vector={:?}",
        note_score(&index, &[0.0, 1.0])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_text_insert_must_not_publish_a_standalone_vector() {
    let (_tmp, svc) = setup();
    let emb = provider();
    let index = IndexHandle::open(&svc.mount, Some(emb.clone()), 0.0, 0.0, false).unwrap();
    let conn = rusqlite::Connection::open(index.db_path()).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_text_insert BEFORE INSERT ON files WHEN NEW.path = 'fresh.md' BEGIN SELECT RAISE(ABORT, 'controlled per-file insert failure'); END;").unwrap();
    std::fs::write(svc.mount.root.join("fresh.md"), NEW).unwrap();
    until(
        || emb.new_calls.load(Ordering::SeqCst) > 0,
        "watcher must attempt the new file",
    )
    .await;
    std::fs::write(svc.mount.root.join("barrier.md"), "barrierfinish").unwrap();
    until(
        || !index.search("barrierfinish", 10).unwrap().is_empty(),
        "unaffected files must still be indexed",
    )
    .await;
    assert!(index.search("newcobalt", 10).unwrap().is_empty());
    let had_vector = index
        .search_vec(&[0.0, 1.0], 10)
        .unwrap()
        .iter()
        .any(|h| h.path == "fresh.md");
    conn.execute_batch("DROP TRIGGER fail_text_insert;")
        .unwrap();
    std::fs::write(svc.mount.root.join("fresh.md"), format!("{NEW} recovered")).unwrap();
    until(
        || {
            !index.search("newcobalt", 10).unwrap().is_empty()
                && index
                    .search_vec(&[0.0, 1.0], 10)
                    .unwrap()
                    .iter()
                    .any(|h| h.path == "fresh.md" && h.score > 0.9)
        },
        "a later successful event must index both representations",
    )
    .await;
    drop(index);
    assert!(
        !had_vector,
        "a failed text insert must not publish an orphan vector"
    );
}
