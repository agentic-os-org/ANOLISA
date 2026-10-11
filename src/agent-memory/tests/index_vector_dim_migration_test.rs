//! Switching the embedding model must not permanently disable vector search.
//!
//! Stored vectors carry the dimensionality of the model that produced them.
//! `search_vec` silently skips rows whose length does not match the query
//! vector, so after the operator swaps the embedding model every stored row
//! becomes invisible to vector search — and hybrid search silently degrades
//! to BM25-only. The full-scan backfill only ever filled *missing* rows, so
//! the mismatch could never heal: the rows exist, they are just the wrong
//! shape. These tests pin the migration: a full scan with the new provider
//! must re-embed the stale rows.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::embedding::{Embedding, EmbeddingProvider};
use agent_memory::index::IndexHandle;
use agent_memory::service::MemoryService;

/// Deterministic embedding: maps a keyword to a basis vector so cosine
/// similarity cleanly separates "rust" docs from "python" docs. The
/// dimensionality is configurable so a test can simulate a model switch
/// (dims used here are ≥ 3 so all basis indexes exist).
struct KeywordEmbedding {
    dim: usize,
}

#[async_trait]
impl EmbeddingProvider for KeywordEmbedding {
    async fn embed(&self, text: &str) -> agent_memory::Result<Embedding> {
        let mut v = vec![0.0_f32; self.dim];
        let t = text.to_lowercase();
        if t.contains("rust") {
            v[0] = 1.0;
        } else if t.contains("python") {
            v[1] = 1.0;
        } else {
            v[2] = 1.0;
        }
        Ok(Embedding { vector: v })
    }

    fn dimensions(&self) -> usize {
        self.dim
    }
}

fn base_config(tmp: &tempfile::TempDir) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    // Use a sub-temp for sessions so /run/anolisa isn't required
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg
}

fn wait_for_index(svc: &MemoryService, expected_min: usize) -> bool {
    svc.index
        .as_ref()
        .map(|h| h.wait_until_at_least(expected_min, 4000))
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_embedding_model_reembeds_stale_vectors() {
    let tmp = tempdir().unwrap();
    let cfg = base_config(&tmp);

    // Phase 1: an 8-dim model embeds the corpus.
    {
        let mut svc = MemoryService::new(cfg.clone()).unwrap();
        svc.write("notes/a.md", "rust ownership system", false)
            .unwrap();
        svc.write("notes/b.md", "python garbage collector", false)
            .unwrap();
        // README is auto-created by MountPoint::ensure → 3 files
        assert!(wait_for_index(&svc, 3), "BM25 index did not reach 3 rows");

        let mock = Arc::new(KeywordEmbedding { dim: 8 });
        svc.embedding = Some(mock.clone());
        let index = IndexHandle::open(&svc.mount, Some(mock), 0.01, 0.3, true).unwrap();
        svc.index = Some(Arc::new(index));

        // Fixture guard: the 8-dim vectors serve vector search.
        let hits = svc
            .memory_search("rust", 5, Some("vector"), None, None)
            .unwrap();
        assert!(
            !hits.is_empty(),
            "fixture: 8-dim vectors must serve vector search"
        );
        assert_eq!(hits[0].path, "notes/a.md");
    } // drop → worker joined, bm25.db (with 8-dim vectors) persists on disk

    // Phase 2: reopen on the same base_dir with a 4-dim model. Every stored
    // vector is now length-mismatched and `search_vec` skips such rows
    // silently; without migration the backfill returns early (no *missing*
    // rows) and vector search stays empty forever.
    let mut svc = MemoryService::new(cfg).unwrap();
    let mock = Arc::new(KeywordEmbedding { dim: 4 });
    svc.embedding = Some(mock.clone());
    let index = IndexHandle::open(&svc.mount, Some(mock), 0.01, 0.3, true).unwrap();
    svc.index = Some(Arc::new(index));

    // The startup full_scan's backfill runs synchronously inside open();
    // poll briefly so a slow machine cannot flake the assertion.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let hits = svc
            .memory_search("rust", 5, Some("vector"), None, None)
            .unwrap();
        if hits.iter().any(|h| h.path == "notes/a.md") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "vector search must recover after the embedding model changed; got {hits:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Hybrid fusion also sees the refreshed vectors again.
    let hits = svc
        .memory_search("rust", 5, Some("hybrid"), None, None)
        .unwrap();
    assert!(!hits.is_empty(), "hybrid search must recover too");
    assert_eq!(hits[0].path, "notes/a.md");
}
