//! Phase 4: Index Worker + Tier B structured search.
//!
//! - `BM25Store`: SQLite FTS5 wrapper, the only place that touches the DB
//! - `IndexWorker`: notify-driven background task that keeps the store in
//!   sync with the on-disk mount tree
//! - `IndexHandle`: thread-safe entry point handed to MemoryService /
//!   Tier B tools. Drop = stop worker + close DB.

pub mod extractor;
pub mod store;
pub mod worker;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::embedding::EmbeddingProvider;
use crate::error::Result;
use crate::ns::MountPoint;

pub use store::BM25Store;
pub use worker::IndexWorker;

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub snippet: String,
    /// Ranking score: **higher is a better match** on every path (BM25, the
    /// LIKE fallback, vector, hybrid RRF). Only that ordering is portable.
    /// The magnitude is path-specific — BM25 relevance and LIKE term
    /// frequency are unbounded and non-negative (FTS5 clamps a negative IDF
    /// to ~0), while `search_vec` scales a cosine in `[-1, 1]` and can
    /// legitimately return zero or less — so compare hits within one result
    /// set, never across queries or paths, and never against an absolute
    /// threshold.
    pub score: f64,
    /// Whether the snippet contains prompt-injection patterns.  Callers
    /// in the adapter layer can use this flag to decide whether to
    /// surface the hit, surface it with extra isolation, or suppress it
    /// entirely.
    #[serde(default)]
    pub suspicious: bool,
}

/// Owning handle: spawn an IndexWorker that watches `mount`, expose
/// thread-safe search via the embedded BM25Store. Dropping the handle
/// shuts down the worker and closes the DB.
pub struct IndexHandle {
    store: Arc<Mutex<BM25Store>>,
    worker: Option<IndexWorker>,
    db_path: PathBuf,
    pub embedding: Option<Arc<dyn EmbeddingProvider>>,
    /// Whether normal search excludes cold files.
    exclude_cold: bool,
}

impl IndexHandle {
    pub fn open(
        mount: &MountPoint,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        time_decay_lambda: f64,
        time_decay_alpha: f64,
        exclude_cold: bool,
    ) -> Result<Self> {
        let db_path = mount.meta_dir.join("index").join("bm25.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let store = BM25Store::open(&db_path, time_decay_lambda, time_decay_alpha, exclude_cold)?;
        let store = Arc::new(Mutex::new(store));

        // Initial full scan + watcher in one worker.
        let emb_clone = embedding.clone();
        let worker = IndexWorker::spawn(mount.clone_lite(), Arc::clone(&store), emb_clone)?;

        Ok(Self {
            store,
            worker: Some(worker),
            db_path,
            embedding,
            exclude_cold,
        })
    }

    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchHit>> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.search(query, top_k, self.exclude_cold)
    }

    /// Search with agent scope filter.
    pub fn search_scoped(
        &self,
        query: &str,
        top_k: usize,
        agent_scope: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.search_scoped(query, top_k, self.exclude_cold, agent_scope)
    }

    /// Deep search: include cold files too.
    pub fn search_deep(&self, query: &str, top_k: usize) -> Result<Vec<SearchHit>> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.search(query, top_k, false)
    }

    pub fn search_vec(&self, query_vec: &[f32], top_k: usize) -> Result<Vec<SearchHit>> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let raw = store.search_vec(query_vec, top_k)?;
        Ok(raw
            .into_iter()
            .map(|(path, score)| SearchHit {
                path,
                snippet: String::new(),
                score,
                suspicious: false,
            })
            .collect())
    }

    pub fn search_hybrid(
        &self,
        query: &str,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchHit>> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.search_hybrid(query, query_vec, top_k)
    }

    /// Synchronously upsert a single file into the BM25 index. Used by
    /// `memory_observe` to make freshly written memories immediately
    /// searchable, closing the race window between the file write and the
    /// notify watcher's next debounce flush (~200 ms).
    ///
    /// The notify watcher will later process the same inotify event and
    /// issue a redundant upsert — this is harmless because `upsert` is
    /// idempotent (DELETE + INSERT on the same rowid).
    ///
    /// Cold-file strategy: `upsert` inserts with `is_cold = 0` (warm) by
    /// default, which is correct — a freshly observed file should be warm.
    /// This matches the watcher's own upsert path, so there is no
    /// behavioural divergence. The `compact()` method only marks files
    /// cold when `access_count = 0 AND mtime_ms < cutoff`, so a newly
    /// indexed file is unaffected regardless of which path inserts it.
    pub fn reindex_file(&self, rel_path: &str, body: &str, mtime_ms: i64, size: u64) -> Result<()> {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.upsert(rel_path, mtime_ms, size, body, None)
    }

    /// Compact the index: mark old, never-accessed files as cold.
    pub fn compact(&self, cold_after_days: u64) -> Result<usize> {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.compact(cold_after_days)
    }

    /// Return counts of warm vs cold files.
    pub fn warm_cold_counts(&self) -> Result<(usize, usize)> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.warm_cold_counts()
    }

    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

    /// Return a clone of the inner BM25Store Arc so that conflict detection
    /// (in FactWriter) can share the same index without a separate open.
    pub fn store_arc(&self) -> Arc<Mutex<BM25Store>> {
        Arc::clone(&self.store)
    }

    pub fn count(&self) -> Result<usize> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.count()
    }

    /// Synchronously wait until at least `expected_min` files are indexed,
    /// up to `deadline_ms` milliseconds. Test helper — production callers
    /// should not need this since search is best-effort eventually-consistent.
    #[doc(hidden)]
    pub fn wait_until_at_least(&self, expected_min: usize, deadline_ms: u64) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < deadline_ms as u128 {
            if let Ok(n) = self.count() {
                if n >= expected_min {
                    return true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }
}

impl Drop for IndexHandle {
    fn drop(&mut self) {
        if let Some(w) = self.worker.take() {
            w.shutdown_blocking();
        }
    }
}
