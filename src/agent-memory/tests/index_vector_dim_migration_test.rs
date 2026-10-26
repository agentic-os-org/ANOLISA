//! Switching the embedding model must not permanently disable vector search.
//!
//! Stored vectors carry the dimensionality of the model that produced them.
//! `search_vec` silently skips rows whose length does not match the query
//! vector, so after the operator swaps the embedding model every stored row
//! becomes invisible to vector search — and hybrid search silently degrades
//! to BM25-only. The full-scan backfill only ever filled *missing* rows, so
//! the mismatch could never heal: the rows exist, they are just the wrong
//! shape. These tests pin the migration: a full scan with the new provider
//! must re-embed the stale rows — including when the provider's
//! `dimensions()` estimate happens to agree with the stale width, when the
//! corpus contains blank or unreadable files, and when a broken provider
//! answers with no components at all.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{KeywordEmbedding, base_config, wait_for_index};
use tempfile::tempdir;

use agent_memory::index::IndexHandle;
use agent_memory::service::MemoryService;

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

        let mock = Arc::new(KeywordEmbedding::honest(8));
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
    let mock = Arc::new(KeywordEmbedding::honest(4));
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Hybrid fusion also sees the refreshed vectors again.
    let hits = svc
        .memory_search("rust", 5, Some("hybrid"), None, None)
        .unwrap();
    assert!(!hits.is_empty(), "hybrid search must recover too");
    assert_eq!(hits[0].path, "notes/a.md");
}

/// The estimate gate: `dimensions()` happens to equal the stored width
/// while the model actually answers wider — the migration must still run
/// (an estimate-gated probe returns "no stale rows" here and leaves vector
/// search dead forever). The corpus also carries a blank file (whose
/// zero-vector answer is sized from the estimate and must not serve as
/// the probe) and a file that turns non-UTF-8 between scans (which the
/// walk evicts from the index without breaking the scan).
#[tokio::test(flavor = "multi_thread")]
async fn migration_runs_when_the_dimension_estimate_matches_the_stored_rows() {
    let tmp = tempdir().unwrap();
    let cfg = base_config(&tmp);

    // Phase 1: a 4-dim model embeds the corpus (README included).
    {
        let mut svc = MemoryService::new(cfg.clone()).unwrap();
        svc.write("notes/a.md", "rust ownership system", false)
            .unwrap();
        svc.write("notes/b.md", "python garbage collector", false)
            .unwrap();
        svc.write("notes/empty.md", "", false).unwrap();
        assert!(wait_for_index(&svc, 4), "BM25 index did not reach 4 rows");

        let mock = Arc::new(KeywordEmbedding::honest(4));
        svc.embedding = Some(mock.clone());
        let index = IndexHandle::open(&svc.mount, Some(mock), 0.01, 0.3, true).unwrap();
        svc.index = Some(Arc::new(index));

        let hits = svc
            .memory_search("rust", 5, Some("vector"), None, None)
            .unwrap();
        assert!(
            !hits.is_empty(),
            "fixture: 4-dim vectors must serve vector search"
        );
    } // drop → 4-dim vectors persist on disk

    // Corrupt one body so its `extract_text` returns None between scans:
    // the walk must evict it from the index without breaking the scan.
    let mut corrupted = false;
    for entry in walk::walk_dir(tmp.path()) {
        if entry.ends_with("b.md") {
            std::fs::write(&entry, [0xff_u8, 0xfe, 0xfd]).unwrap();
            corrupted = true;
        }
    }
    assert!(corrupted, "fixture: notes/b.md must exist on disk");

    // Phase 2: reopen with a model whose estimate still says 4 while it
    // actually answers 8-wide.
    let mut svc = MemoryService::new(cfg).unwrap();
    let mock = Arc::new(KeywordEmbedding::lying(4, 8));
    svc.embedding = Some(mock.clone());
    let index = IndexHandle::open(&svc.mount, Some(mock), 0.01, 0.3, true).unwrap();
    svc.index = Some(Arc::new(index));

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
            "vector search must recover even when dimensions() matches the stale width; got {hits:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The empty answer: a provider that returns no components confirms
/// nothing. Reading it as dimensionality 0 flags every stored row as
/// stale and overwrites the corpus with empty blobs — permanently, since
/// the same broken provider keeps answering empty. The migration must
/// treat the answer as unconfirmed and leave the stored vectors intact.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_provider_embedding_is_unconfirmed_not_authoritative() {
    let tmp = tempdir().unwrap();
    let cfg = base_config(&tmp);

    // Phase 1: a healthy 4-dim model embeds the corpus.
    {
        let mut svc = MemoryService::new(cfg.clone()).unwrap();
        svc.write("notes/a.md", "rust ownership system", false)
            .unwrap();
        assert!(wait_for_index(&svc, 2), "BM25 index did not reach 2 rows");

        let mock = Arc::new(KeywordEmbedding::honest(4));
        svc.embedding = Some(mock.clone());
        let index = IndexHandle::open(&svc.mount, Some(mock), 0.01, 0.3, true).unwrap();
        svc.index = Some(Arc::new(index));

        let hits = svc
            .memory_search("rust", 5, Some("vector"), None, None)
            .unwrap();
        assert!(!hits.is_empty(), "fixture: vectors must serve search");
    }

    // Phase 2: reopen with a broken backend whose estimate (8) disagrees
    // with the stored rows (4) and whose every answer is empty. The probe
    // must bail out after exactly one attempt and rewrite nothing.
    let mut svc = MemoryService::new(cfg.clone()).unwrap();
    let mock = Arc::new(KeywordEmbedding::empty_payload(8));
    svc.embedding = Some(mock.clone());
    let index = IndexHandle::open(&svc.mount, Some(mock.clone()), 0.01, 0.3, true).unwrap();
    svc.index = Some(Arc::new(index));

    let deadline = Instant::now() + Duration::from_secs(5);
    while mock.embed_calls() < 1 {
        assert!(
            Instant::now() < deadline,
            "the dimensionality probe never ran"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        mock.embed_calls(),
        1,
        "an empty provider answer must abort the migration after the single probe, got {} embed calls",
        mock.embed_calls()
    );
    drop(svc);

    // Phase 3: a healthy provider at the stored width must still serve
    // vector search — the broken scan must not have destroyed the vectors.
    let mut svc = MemoryService::new(cfg).unwrap();
    let mock = Arc::new(KeywordEmbedding::honest(4));
    svc.embedding = Some(mock.clone());
    let index = IndexHandle::open(&svc.mount, Some(mock), 0.01, 0.3, true).unwrap();
    svc.index = Some(Arc::new(index));

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
            "stored vectors must survive an empty-payload provider scan; got {hits:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The steady state: with no model switch a full scan probes the
/// dimensionality exactly once and rewrites nothing (regression pin for
/// the probe cost and against corpus churn).
#[tokio::test(flavor = "multi_thread")]
async fn a_steady_state_scan_probes_once_and_reembeds_nothing() {
    let tmp = tempdir().unwrap();
    let cfg = base_config(&tmp);

    // Phase 1: embed the corpus at 4 dims.
    {
        let mut svc = MemoryService::new(cfg.clone()).unwrap();
        svc.write("notes/a.md", "rust ownership system", false)
            .unwrap();
        svc.write("notes/b.md", "python garbage collector", false)
            .unwrap();
        assert!(wait_for_index(&svc, 3), "BM25 index did not reach 3 rows");

        let mock = Arc::new(KeywordEmbedding::honest(4));
        svc.embedding = Some(mock.clone());
        let index = IndexHandle::open(&svc.mount, Some(mock), 0.01, 0.3, true).unwrap();
        svc.index = Some(Arc::new(index));

        let hits = svc
            .memory_search("rust", 5, Some("vector"), None, None)
            .unwrap();
        assert!(!hits.is_empty(), "fixture: vectors must serve search");
    }

    // Phase 2: same model, second scan. No missing rows, no stale rows —
    // the only embedding work is the one dimensionality probe.
    let mut svc = MemoryService::new(cfg).unwrap();
    let mock = Arc::new(KeywordEmbedding::honest(4));
    svc.embedding = Some(mock.clone());
    let index = IndexHandle::open(&svc.mount, Some(mock.clone()), 0.01, 0.3, true).unwrap();
    svc.index = Some(Arc::new(index));

    let deadline = Instant::now() + Duration::from_secs(5);
    while mock.embed_calls() < 1 {
        assert!(
            Instant::now() < deadline,
            "the dimensionality probe never ran"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        mock.embed_calls(),
        1,
        "a steady-state full scan must embed exactly the one dimensionality probe, got {} embed calls",
        mock.embed_calls()
    );

    // And the untouched vectors still serve search.
    let hits = svc
        .memory_search("rust", 5, Some("vector"), None, None)
        .unwrap();
    assert!(
        hits.iter().any(|h| h.path == "notes/a.md"),
        "vector search must keep working after a steady-state scan; got {hits:?}"
    );
}

/// Tiny recursive directory walk so the test can locate a file under the
/// userland mount without hard-coding its layout.
mod walk {
    use std::path::{Path, PathBuf};

    pub fn walk_dir(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for e in entries.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            stack.push(p);
                        } else {
                            out.push(p);
                        }
                    }
                }
                Err(_) => continue,
            }
        }
        out
    }
}
