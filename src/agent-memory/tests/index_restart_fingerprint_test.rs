//! Persisted indexes must refresh resized files even when their mtime is retained.

use std::fs::{self, FileTimes};
use std::path::Path;
use std::sync::{Arc, Mutex};

use agent_memory::embedding::{Embedding, EmbeddingProvider};
use agent_memory::error::Result;
use agent_memory::index::IndexHandle;
use agent_memory::ns::{MountPoint, Namespace};
use async_trait::async_trait;

const OLD: &str = "cobaltzebra original";
const NEW: &str = "heliotrope replacement with a different length";
const NOTE: &str = "restart.md";

fn setup() -> (tempfile::TempDir, MountPoint) {
    let tmp = tempfile::tempdir().unwrap();
    let mount = MountPoint::ensure(Namespace::user("restart-test").unwrap(), tmp.path()).unwrap();
    fs::write(mount.root.join(NOTE), OLD).unwrap();
    (tmp, mount)
}

fn open(mount: &MountPoint, embedding: Option<Arc<dyn EmbeddingProvider>>) -> IndexHandle {
    IndexHandle::open(mount, embedding, 0.0, 0.0, false).unwrap()
}

fn replace_preserving_mtime(path: &Path) {
    let before = path.metadata().unwrap();
    fs::write(path, NEW).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(before.modified().unwrap()))
        .unwrap();
    let after = path.metadata().unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_ne!(before.len(), after.len());
}

#[test]
fn restart_refreshes_same_mtime_resized_text_and_drops_stale_vector_without_provider() {
    let (_tmp, mount) = setup();
    let index = open(&mount, None);
    assert_eq!(index.search("cobaltzebra", 10).unwrap()[0].path, NOTE);
    index
        .store_arc()
        .lock()
        .unwrap()
        .upsert_vec(NOTE, &[1.0, 0.0])
        .unwrap();
    drop(index);

    replace_preserving_mtime(&mount.root.join(NOTE));
    let index = open(&mount, None);
    assert!(index.search("cobaltzebra", 10).unwrap().is_empty());
    assert_eq!(index.search("heliotrope", 10).unwrap()[0].path, NOTE);
    let store = index.store_arc();
    let store = store.lock().unwrap();
    let stat = store.index_stat_for(NOTE).unwrap();
    assert_eq!(stat.size, NEW.len() as u64);
    assert!(!stat.has_vector);
    assert!(
        store
            .search_vec(&[1.0, 0.0], 10)
            .unwrap()
            .iter()
            .all(|(path, _)| path != NOTE)
    );
}

#[derive(Default)]
struct ContentEmbedding {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl EmbeddingProvider for ContentEmbedding {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        self.calls.lock().unwrap().push(text.to_owned());
        Ok(Embedding {
            vector: if text == OLD {
                vec![1.0, 0.0]
            } else if text == NEW {
                vec![0.0, 1.0]
            } else {
                vec![-1.0, 0.0]
            },
        })
    }

    fn dimensions(&self) -> usize {
        2
    }
}

fn note_score(index: &IndexHandle, vector: &[f32]) -> f64 {
    index
        .search_vec(vector, 10)
        .unwrap()
        .into_iter()
        .find(|hit| hit.path == NOTE)
        .unwrap()
        .score
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_replaces_same_mtime_resized_vector_with_current_content_embedding() {
    let (_tmp, mount) = setup();
    let embedding = Arc::new(ContentEmbedding::default());
    let index = open(&mount, Some(embedding.clone()));
    assert!((note_score(&index, &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    drop(index);

    replace_preserving_mtime(&mount.root.join(NOTE));
    let index = open(&mount, Some(embedding.clone()));
    assert!((note_score(&index, &[0.0, 1.0]) - 1.0).abs() < 1e-6);
    assert!(note_score(&index, &[1.0, 0.0]).abs() < 1e-6);
    assert_eq!(
        embedding
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|text| *text == NEW)
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_backfills_missing_vectors_without_reindexing_unchanged_files() {
    let (_tmp, mount) = setup();
    let index = open(&mount, None);
    assert!(
        !index
            .store_arc()
            .lock()
            .unwrap()
            .index_stat_for(NOTE)
            .unwrap()
            .has_vector
    );
    drop(index);

    let embedding = Arc::new(ContentEmbedding::default());
    let index = open(&mount, Some(embedding.clone()));
    assert!((note_score(&index, &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    drop(index);
    let calls = embedding.calls.lock().unwrap().len();
    assert!(calls > 0);

    // A redundant upsert preserves the vector but rewrites indexed_at. Seed
    // an old timestamp so that rewrite is visible without timing sleeps.
    let connection = rusqlite::Connection::open(mount.meta_dir.join("index/bm25.db")).unwrap();
    let indexed_at = "2000-01-01T00:00:00+00:00";
    assert_eq!(
        connection
            .execute(
                "UPDATE files SET indexed_at = ?1 WHERE path = ?2",
                [indexed_at, NOTE],
            )
            .unwrap(),
        1
    );

    let index = open(&mount, Some(embedding.clone()));
    let after: String = connection
        .query_row(
            "SELECT indexed_at FROM files WHERE path = ?1",
            [NOTE],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, indexed_at, "unchanged files must not be reindexed");
    assert!((note_score(&index, &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert_eq!(embedding.calls.lock().unwrap().len(), calls);
}
