//! Fact writer — writes consolidated facts to the filesystem.
//!
//! Produces two outputs per fact:
//! 1. `facts/<category>/<ulid>.md` — markdown with YAML frontmatter
//! 2. `facts/facts.jsonl` — JSONL line appended to the same directory
//!
//! All writes use `safe_fs` (openat2 + RESOLVE_BENEATH) when a root_fd
//! is provided, ensuring namespace sandbox containment.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::index::store::{BM25Store, ConflictCandidate};

use super::fact::ConsolidatedFact;

/// Writes facts to a given base directory.
pub struct FactWriter {
    facts_dir: PathBuf,
    jsonl_path: PathBuf,
    /// Mount root fd for sandboxed writes via safe_fs (openat2 + RESOLVE_BENEATH).
    /// None falls back to std::fs (used in tests with temp dirs).
    root_fd: Option<Arc<OwnedFd>>,
    /// Held file handle for fallback (non-sandboxed) JSONL writes.
    jsonl_file: Mutex<Option<std::fs::File>>,
    /// Optional BM25 store for conflict detection.
    index: Option<Arc<Mutex<BM25Store>>>,
    /// BM25 threshold for conflict detection.
    conflict_threshold: f64,
}

impl FactWriter {
    pub fn new(base_dir: &Path) -> Self {
        let facts_dir = base_dir.join("facts");
        let jsonl_path = facts_dir.join("facts.jsonl");
        Self {
            facts_dir,
            jsonl_path,
            root_fd: None,
            jsonl_file: Mutex::new(None),
            index: None,
            conflict_threshold: -2.0,
        }
    }

    /// Attach a root fd for sandboxed writes.
    pub fn with_root_fd(mut self, fd: Arc<OwnedFd>) -> Self {
        self.root_fd = Some(fd);
        self
    }

    pub fn with_index(mut self, index: Arc<Mutex<BM25Store>>, conflict_threshold: f64) -> Self {
        self.index = Some(index);
        self.conflict_threshold = conflict_threshold;
        self
    }

    /// Ensure the facts directory exists.
    pub fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.facts_dir)?;
        Ok(())
    }

    /// Write a single fact: creates `<category>/<ulid>.md` and appends to `facts.jsonl`.
    /// Uses safe_fs (openat2 + RESOLVE_BENEATH) when root_fd is available,
    /// falling back to std::fs for tests with temp dirs.
    /// If conflict detection is enabled, marks similar existing facts as superseded.
    /// Facts are organized into category subdirectories under facts/.
    ///
    /// The conflicting facts are retired only once the replacement is durable
    /// (markdown on disk + JSONL appended). `supersede()` runs an autocommitted
    /// `UPDATE files SET is_superseded = 1` and every search path filters on
    /// `is_superseded = 0`, so superseding first would drop the old fact out of
    /// recall whenever a later step fails and no replacement ever lands (#4406).
    ///
    /// Retiring them *later* opens the opposite hazard — the scan no longer
    /// holds the store lock across the writes, so a candidate path can come to
    /// name a different record — which is why the supersede re-checks each
    /// candidate's identity; see [`Self::supersede_conflicts`].
    pub fn write(&self, fact: &ConsolidatedFact) -> Result<()> {
        std::fs::create_dir_all(&self.facts_dir)?;

        // Conflict detection: search for similar facts before writing, but only
        // record what they are. The scan is read-only and has to stay ahead of
        // the write: once the new markdown is on disk the index worker can pick
        // it up, and a later scan could match the fact against itself.
        let conflicts = self.find_conflicts(fact);

        // Write markdown file under category subdirectory.
        let category_dir = self.facts_dir.join(fact.category.to_string());
        std::fs::create_dir_all(&category_dir)?;
        let md_path = category_dir.join(format!("{}.md", fact.id));

        if let Some(ref fd) = self.root_fd {
            // Sandboxed write via safe_fs (openat2 + RESOLVE_BENEATH).
            let md_rel = Path::new("facts")
                .join(fact.category.to_string())
                .join(format!("{}.md", fact.id));
            crate::safe_fs::write(fd.as_fd(), &md_rel, fact.to_markdown().as_bytes())?;
        } else {
            std::fs::write(&md_path, fact.to_markdown())?;
        }

        // Append JSONL line.
        let jsonl_str = match fact.to_jsonl() {
            Ok(s) => s,
            Err(e) => {
                // The markdown landed but the JSONL feed will not, so the
                // replacement is not durable: leave the conflicting facts
                // searchable instead of retiring them (#4406).
                tracing::warn!("failed to serialize fact {} to JSONL: {e}", fact.id);
                return Ok(());
            }
        };
        let jsonl_line = format!("{jsonl_str}\n");
        if let Some(ref fd) = self.root_fd {
            let jsonl_rel = Path::new("facts").join("facts.jsonl");
            crate::safe_fs::append(fd.as_fd(), &jsonl_rel, jsonl_line.as_bytes())?;
        } else {
            let mut guard = self.jsonl_file.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_none() {
                let f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.jsonl_path)?;
                *guard = Some(f);
            }
            let f = guard.as_mut().expect("just inserted");
            f.write_all(jsonl_line.as_bytes())?;
            f.sync_all()?;
        }

        // Both outputs are on disk now, so retiring the facts this one
        // replaces can no longer orphan the knowledge they held.
        self.supersede_conflicts(fact, &conflicts);

        tracing::debug!("wrote fact: {}", md_path.display());
        Ok(())
    }

    /// Read-only conflict scan: the existing facts that `fact` should replace.
    /// Returns nothing when conflict detection is disabled, and a failed scan
    /// degrades to "no conflicts" rather than blocking the write.
    ///
    /// Each candidate carries the identity of the index row it was scanned
    /// from, which [`Self::supersede_conflicts`] re-checks before retiring it.
    fn find_conflicts(&self, fact: &ConsolidatedFact) -> Vec<ConflictCandidate> {
        let Some(store) = self.index.as_ref() else {
            return Vec::new();
        };
        let search_text = format!(
            "{} {}",
            fact.title,
            fact.content.chars().take(100).collect::<String>()
        );
        let s = store.lock().unwrap_or_else(|e| e.into_inner());
        match s.detect_conflicts_identified(&search_text, self.conflict_threshold) {
            Ok(conflicts) => conflicts,
            Err(e) => {
                tracing::warn!("conflict detection failed: {e}");
                Vec::new()
            }
        }
    }

    /// Mark the facts found by [`Self::find_conflicts`] as superseded. Called
    /// only after the replacement has been persisted.
    ///
    /// The scan released the store lock before the filesystem writes — holding
    /// it across them is what let a slow mount block every search for the
    /// duration of a write — so a candidate's `path` is no longer guaranteed to
    /// name the record that was scanned. The index worker can have removed that
    /// row when the old file was unlinked and indexed *unrelated* content that
    /// reappeared at the same path. Superseding on `path` alone would then hide
    /// a file this fact never conflicted with and stamp a wrong `superseded_by`
    /// into its frontmatter, so each candidate is retired through the identity
    /// recorded at scan time and one that no longer matches is skipped.
    ///
    /// Skipping is the safe direction: nothing is hidden from recall, and the
    /// next consolidation run rescans and can retire whatever still conflicts.
    fn supersede_conflicts(&self, fact: &ConsolidatedFact, conflicts: &[ConflictCandidate]) {
        if conflicts.is_empty() {
            return;
        }
        let Some(store) = self.index.as_ref() else {
            return;
        };
        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
        for candidate in conflicts {
            tracing::info!(
                "conflict detected: new fact '{}' conflicts with '{}' (score={:.2})",
                fact.title,
                candidate.path,
                candidate.score
            );
            match s.supersede_identified(&candidate.path, candidate.identity, &fact.id) {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    "not superseding '{}': the index row no longer matches the record the \
                     conflict scan found (removed, re-indexed from different bytes, or already \
                     superseded)",
                    candidate.path
                ),
                Err(e) => tracing::warn!("failed to supersede '{}': {e}", candidate.path),
            }
        }
    }

    /// Write multiple facts in one batch.
    pub fn write_batch(&self, facts: &[ConsolidatedFact]) -> Result<usize> {
        if facts.is_empty() {
            return Ok(0);
        }
        self.ensure_dir()?;

        let mut written = 0;
        for fact in facts {
            if let Err(e) = self.write(fact) {
                tracing::warn!("failed to write fact {}: {e}", fact.id);
            } else {
                written += 1;
            }
        }

        tracing::info!("batch write: {written}/{} facts written", facts.len());
        Ok(written)
    }

    pub fn facts_dir(&self) -> &Path {
        &self.facts_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consolidation::fact::{ConsolidatedFact, FactCategory};

    #[test]
    fn write_single_fact() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = FactWriter::new(tmp.path());
        let fact = ConsolidatedFact::new(
            "test-sid",
            FactCategory::WorkingContext,
            "Test fact".into(),
            "Test content body".into(),
            "mem_write".into(),
            vec!["notes/a.md".into()],
            0.8,
        );
        writer.write(&fact).unwrap();

        let md_path = tmp
            .path()
            .join("facts")
            .join("working-context")
            .join(format!("{}.md", fact.id));
        assert!(md_path.exists());
        assert!(
            std::fs::read_to_string(&md_path)
                .unwrap()
                .contains("Test content")
        );

        let jsonl_path = tmp.path().join("facts").join("facts.jsonl");
        assert!(jsonl_path.exists());
        let content = std::fs::read_to_string(&jsonl_path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: ConsolidatedFact = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.id, fact.id);
    }

    #[test]
    fn write_batch_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = FactWriter::new(tmp.path());
        let facts = vec![
            ConsolidatedFact::new(
                "s1",
                FactCategory::Interest,
                "A".into(),
                "Content A".into(),
                "mem_search".into(),
                vec![],
                0.5,
            ),
            ConsolidatedFact::new(
                "s2",
                FactCategory::Lesson,
                "B".into(),
                "Content B".into(),
                "mem_edit".into(),
                vec![],
                0.6,
            ),
        ];
        let n = writer.write_batch(&facts).unwrap();
        assert_eq!(n, 2);
        let jsonl_path = tmp.path().join("facts").join("facts.jsonl");
        let content = std::fs::read_to_string(&jsonl_path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    /// A store holding one fact that a new `Lesson` fact conflicts with, plus a
    /// writer whose conflict threshold sits far below any BM25 score — these
    /// tests pin the supersede *ordering*, not the detection tuning.
    fn conflicting_store_and_writer(
        tmp: &std::path::Path,
    ) -> (Arc<Mutex<BM25Store>>, FactWriter, ConsolidatedFact) {
        let store = Arc::new(Mutex::new(BM25Store::open_in_memory().unwrap()));
        store
            .lock()
            .unwrap()
            .upsert(
                "notes/old-lesson.md",
                100,
                50,
                "borrow checker rejects nested closure capture",
                None,
            )
            .unwrap();

        let writer = FactWriter::new(tmp).with_index(Arc::clone(&store), -1_000_000.0);
        let fact = ConsolidatedFact::new(
            "test-sid",
            FactCategory::Lesson,
            "borrow checker rejects nested closure capture".into(),
            "borrow checker rejects nested closure capture".into(),
            "mem_edit".into(),
            vec!["src/lib.rs".into()],
            0.9,
        );
        (store, writer, fact)
    }

    /// #4406 regression: `supersede()` is an autocommitted
    /// `UPDATE files SET is_superseded = 1` and every search path filters on
    /// `is_superseded = 0`. Running it before the replacement was durable meant
    /// a failed write retired the conflicting fact without landing the new one,
    /// so the knowledge vanished from recall with nothing in its place.
    #[test]
    fn write_failure_keeps_conflicting_fact_searchable() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, writer, fact) = conflicting_store_and_writer(tmp.path());

        // Block `facts/lesson/` with a regular file so the markdown write fails.
        let facts_dir = tmp.path().join("facts");
        std::fs::create_dir_all(&facts_dir).unwrap();
        std::fs::write(facts_dir.join("lesson"), "not a directory").unwrap();

        // Sanity: the scan does find the old fact, so the assertion below can
        // only fail because of when supersede runs.
        let conflicts = writer.find_conflicts(&fact);
        assert_eq!(
            conflicts
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            vec!["notes/old-lesson.md"]
        );

        let res = writer.write(&fact);
        assert!(res.is_err(), "category dir is blocked, write must fail");

        let hits = store
            .lock()
            .unwrap()
            .search("borrow checker", 10, true)
            .unwrap();
        assert!(
            hits.iter().any(|h| h.path == "notes/old-lesson.md"),
            "old fact lost from recall after a failed write: {:?}",
            hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>()
        );
    }

    /// The other half of #4406: once the replacement *is* durable the
    /// conflicting fact must still be retired, or both would answer searches.
    #[test]
    fn successful_write_supersedes_conflicting_fact() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, writer, fact) = conflicting_store_and_writer(tmp.path());

        writer.write(&fact).unwrap();

        let md_path = tmp
            .path()
            .join("facts")
            .join("lesson")
            .join(format!("{}.md", fact.id));
        assert!(md_path.exists(), "replacement markdown must be on disk");

        let hits = store
            .lock()
            .unwrap()
            .search("borrow checker", 10, true)
            .unwrap();
        assert!(
            hits.iter().all(|h| h.path != "notes/old-lesson.md"),
            "old fact still searchable after a successful write: {:?}",
            hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>()
        );
    }

    /// The hazard the deferred supersede introduces (#8717 review): the scan no
    /// longer holds the store lock across the filesystem writes, so while
    /// `write()` is busy the index worker can drop the scanned row — the old
    /// file was unlinked — and index *unrelated* content that reappeared at the
    /// same path. Retiring by `path` alone then hides that unrelated record
    /// from every search and stamps a bogus `superseded_by` into its
    /// frontmatter.
    ///
    /// The interleaving is forced rather than raced: `facts/facts.jsonl` is a
    /// FIFO, so the append blocks with the new markdown already on disk and the
    /// store lock already released — exactly the window the replacement needs.
    /// Sandboxed (`root_fd`) write path, the one production uses.
    #[test]
    fn conflict_candidate_reindexed_mid_write_is_not_superseded() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        // Store on disk under `root`, so `mount_root` is `root` and a supersede
        // reaches the file's frontmatter as well as the index row.
        let index_dir = root.join(".anolisa").join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let store = Arc::new(Mutex::new(
            BM25Store::open_for_test(&index_dir.join("bm25.db")).unwrap(),
        ));

        // The record the scan is about to find: on disk, and in the index with
        // the mtime/size the worker would have recorded for it.
        let old_rel = "notes/old-lesson.md";
        let old_abs = root.join(old_rel);
        std::fs::create_dir_all(old_abs.parent().unwrap()).unwrap();
        std::fs::write(
            &old_abs,
            "---\ntitle: old\n---\n\nborrow checker rejects nested closure capture\n",
        )
        .unwrap();
        store
            .lock()
            .unwrap()
            .upsert(
                old_rel,
                1_000,
                10,
                "borrow checker rejects nested closure capture",
                None,
            )
            .unwrap();

        let root_fd = Arc::new(crate::safe_fs::open_root(&root).unwrap());
        let writer = Arc::new(
            FactWriter::new(&root)
                .with_root_fd(Arc::clone(&root_fd))
                .with_index(Arc::clone(&store), -1_000_000.0),
        );
        let fact = ConsolidatedFact::new(
            "test-sid",
            FactCategory::Lesson,
            "borrow checker rejects nested closure capture".into(),
            "borrow checker rejects nested closure capture".into(),
            "mem_edit".into(),
            vec!["src/lib.rs".into()],
            0.9,
        );

        // Park the JSONL append: opening a FIFO write-only blocks until a
        // reader turns up.
        let fifo = root.join("facts").join("facts.jsonl");
        std::fs::create_dir_all(fifo.parent().unwrap()).unwrap();
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();

        let md_path = root
            .join("facts")
            .join("lesson")
            .join(format!("{}.md", fact.id));
        let w = Arc::clone(&writer);
        let f = fact.clone();
        let handle = std::thread::spawn(move || w.write(&f));

        // Wait until the markdown has landed: `write()` is past the scan and
        // stuck in the append, holding no store lock.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !md_path.exists() {
            assert!(
                Instant::now() < deadline,
                "write() never reached the JSONL append"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        // Index worker, mid-write: the old file went away so its row went away,
        // then unrelated content appeared at the same path and got indexed
        // there — a different row identity under an identical `path`.
        let unrelated_body = "kubernetes pod affinity needs a topology key";
        store.lock().unwrap().remove(old_rel).unwrap();
        std::fs::write(
            &old_abs,
            format!("---\ntitle: k8s\n---\n\n{unrelated_body}\n"),
        )
        .unwrap();
        store
            .lock()
            .unwrap()
            .upsert(old_rel, 2_000, 42, unrelated_body, None)
            .unwrap();
        let searchable = |q: &str| {
            store
                .lock()
                .unwrap()
                .search(q, 10, true)
                .unwrap()
                .iter()
                .map(|h| h.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            searchable("kubernetes"),
            vec![old_rel],
            "the re-indexed record must answer searches before the append resumes"
        );

        // Release the append so `write()` runs on into the supersede.
        let reader = std::fs::File::open(&fifo).unwrap();
        let res = handle.join().unwrap();
        drop(reader);
        res.expect("write() must succeed once the FIFO has a reader");

        assert_eq!(
            searchable("kubernetes"),
            vec![old_rel],
            "unrelated content at a re-created conflict path was hidden from search"
        );
        assert!(
            !std::fs::read_to_string(&old_abs)
                .unwrap()
                .contains("superseded_by"),
            "unrelated content was stamped with a superseded_by it did not earn"
        );
    }
}
