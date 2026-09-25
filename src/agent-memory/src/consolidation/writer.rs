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
use crate::index::store::BM25Store;

use super::fact::ConsolidatedFact;

/// Mount-relative directory that holds consolidated facts
/// (`facts/<category>/<ulid>.md`). Conflict detection is scoped to it: the
/// rows it flags get handed to `BM25Store::supersede`, which hides them from
/// *every* search path and keeps hiding them across re-indexing, so a derived
/// fact may only ever replace another fact — never a user memory file that
/// happens to cover the same topic.
pub const FACTS_DIR_NAME: &str = "facts";

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
        let facts_dir = base_dir.join(FACTS_DIR_NAME);
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
    /// If conflict detection is enabled, marks similar existing *facts* as
    /// superseded — the scan is scoped to `facts/` so a memory file the fact
    /// was derived from is never hidden by it.
    /// Facts are organized into category subdirectories under facts/.
    pub fn write(&self, fact: &ConsolidatedFact) -> Result<()> {
        std::fs::create_dir_all(&self.facts_dir)?;

        // Conflict detection: search for similar facts before writing.
        if let Some(ref store) = self.index {
            let search_text = format!(
                "{} {}",
                fact.title,
                fact.content.chars().take(100).collect::<String>()
            );
            let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
            let scope = format!("{FACTS_DIR_NAME}/");
            match s.detect_conflicts(&search_text, self.conflict_threshold, Some(&scope)) {
                Ok(conflicts) => {
                    for (old_path, score) in &conflicts {
                        tracing::info!(
                            "conflict detected: new fact '{}' conflicts with '{}' (score={:.2})",
                            fact.title,
                            old_path,
                            score
                        );
                        let _ = s.supersede(old_path, &fact.id);
                    }
                }
                Err(e) => tracing::warn!("conflict detection failed: {e}"),
            }
        }

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

        tracing::debug!("wrote fact: {}", md_path.display());
        Ok(())
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
    use std::sync::{Arc, Mutex};

    /// Build the corpus the two conflict-detection tests share: a user note
    /// that the fact below was derived from (long body, the statement appears
    /// once) and an older fact stating the same thing twice.
    fn conflict_store(content: &str) -> Arc<Mutex<BM25Store>> {
        let store = Arc::new(Mutex::new(BM25Store::open_in_memory().unwrap()));
        let now = 1_700_000_000_000;
        let note_body = format!(
            "meeting notes covering the snapshot and rollback requirement boundary, the kernel \
             memory management design document, cgroup hierarchy and page table reclaim \
             strategy, the todo list for fixing the build scripts and packaging the rpm spec, \
             ownership and borrow checker lifetime notes, data processing scripts, cargo lock \
             versions, index worker incremental scanning, review comments, release scheduling, \
             canary rollout planning, stress test reports, metric dashboards, alert thresholds, \
             capacity planning, migration strategy, compatibility test coverage statistics, the \
             on-call rotation, the dependency audit backlog, the benchmark harness rewrite, the \
             vendored libgit2 bump, the sandbox escape review, the journald fan-out toggle, the \
             snapshot retention policy, the cold tier thresholds, the embedding provider budget, \
             the plugin capability consent flow, the installer rollback path, the docs lint \
             gate, and one passing remark that {content} before returning to the engineering \
             agenda for the remaining items"
        );
        let mut s = store.lock().unwrap();
        s.upsert(
            "notes/kernel.md",
            now,
            200,
            "kernel memory management design document cgroup hierarchy and page table reclaim",
            None,
        )
        .unwrap();
        s.upsert(
            "notes/todo.md",
            now,
            200,
            "todo list for fixing the build scripts and packaging the rpm spec",
            None,
        )
        .unwrap();
        s.upsert(
            "notes/meetings.md",
            now,
            200,
            "meeting notes covering the snapshot and rollback boundary and the review comments",
            None,
        )
        .unwrap();
        s.upsert("notes/prefs.md", now, 900, &note_body, None)
            .unwrap();
        s.upsert(
            "facts/interest/old.md",
            now,
            200,
            &format!("{content} {content}"),
            None,
        )
        .unwrap();
        drop(s);
        store
    }

    fn visible_paths(store: &Arc<Mutex<BM25Store>>, query: &str) -> Vec<String> {
        store
            .lock()
            .unwrap()
            .search(query, 20, true)
            .unwrap()
            .into_iter()
            .map(|h| h.path)
            .collect()
    }

    #[test]
    fn write_supersedes_a_duplicate_fact_not_the_memory_file() {
        // Regression (BM25 MATCH path — every query token ≥ 3 chars). FTS5's
        // `bm25()` is negative and *more negative is more similar*, but the
        // threshold used to be applied as `score >= threshold`, so the
        // duplicate fact below (bm25 ≈ -7.4) was never flagged: consolidation
        // kept re-writing the same fact while the row it superseded nothing.
        // The scan is now also scoped to `facts/`, so the user note the fact
        // was derived from can no longer be superseded by it.
        let content =
            "user prefers rust for systems programming and dislikes garbage collected runtimes";
        let store = conflict_store(content);
        let tmp = tempfile::tempdir().unwrap();
        let writer = FactWriter::new(tmp.path()).with_index(store.clone(), -2.0);
        let fact = ConsolidatedFact::new(
            "sid",
            FactCategory::Interest,
            "user prefers rust for systems programming".into(),
            content.into(),
            "mem_write".into(),
            vec![],
            0.8,
        );
        writer.write(&fact).unwrap();

        let visible = visible_paths(&store, content);
        assert!(
            !visible.iter().any(|p| p == "facts/interest/old.md"),
            "the duplicate fact must be superseded: {visible:?}"
        );
        assert!(
            visible.iter().any(|p| p == "notes/prefs.md"),
            "consolidation must not hide the memory file it was derived from: {visible:?}"
        );
    }

    #[test]
    fn write_scopes_conflict_detection_to_facts_on_the_like_fallback() {
        // Same invariant on the LIKE fallback (a 2-char token — `gc` here,
        // any short CJK word in practice — pulls the query off the trigram
        // path). That branch scores by term frequency, so every substring
        // match clears the negative default threshold: before the scope fix
        // writing this fact superseded `notes/prefs.md` too, hiding a user
        // memory from *every* search path — and `upsert` never clears
        // `is_superseded`, so re-indexing did not bring it back.
        let content = "user prefers rust for systems programming and dislikes gc runtimes";
        let store = conflict_store(content);
        let tmp = tempfile::tempdir().unwrap();
        let writer = FactWriter::new(tmp.path()).with_index(store.clone(), -2.0);
        let fact = ConsolidatedFact::new(
            "sid",
            FactCategory::Interest,
            "user prefers rust for systems programming".into(),
            content.into(),
            "mem_write".into(),
            vec![],
            0.8,
        );

        // Guard the premise: unscoped, both rows are flagged on this path.
        let search_text = format!(
            "{} {}",
            fact.title,
            fact.content.chars().take(100).collect::<String>()
        );
        let unscoped = store
            .lock()
            .unwrap()
            .detect_conflicts(&search_text, -2.0, None)
            .unwrap();
        let unscoped_paths: Vec<&str> = unscoped.iter().map(|(p, _)| p.as_str()).collect();
        assert!(
            unscoped_paths.contains(&"notes/prefs.md"),
            "test premise: the unscoped scan reaches the user note ({unscoped_paths:?})"
        );

        writer.write(&fact).unwrap();

        let visible = visible_paths(&store, content);
        assert!(
            visible.iter().any(|p| p == "notes/prefs.md"),
            "the user note must survive consolidation: {visible:?}"
        );
        assert!(
            !visible.iter().any(|p| p == "facts/interest/old.md"),
            "the duplicate fact must still be superseded: {visible:?}"
        );
    }

    #[test]
    fn write_supersedes_a_duplicate_fact_in_a_fresh_namespace() {
        // P1 review finding, end to end. In a namespace that holds only the
        // earlier copy of the fact and the note it came from, every probe
        // term occurs in every indexed row: FTS5 derives its IDF factor from
        // the whole corpus, so `bm25()` collapses to a few millionths below
        // zero and the -2.0 threshold flags nothing at all. Consolidation
        // then wrote the same fact again, session after session, in exactly
        // the namespaces that are newest and smallest.
        let title = "用户偏好 rust 系统编程";
        let content = "用户在多次会话中提到偏好使用 rust 编写系统编程相关的工具 尤其是内核态与输入输出密集场景";
        let first = ConsolidatedFact::new(
            "sid-old",
            FactCategory::Interest,
            title.into(),
            content.into(),
            "mem_write".into(),
            vec![],
            0.8,
        );
        let store = Arc::new(Mutex::new(BM25Store::open_in_memory().unwrap()));
        let now = 1_700_000_000_000;
        let old_path = format!("facts/interest/{}.md", first.id);
        {
            let mut s = store.lock().unwrap();
            // Indexed the way the indexer stores it: the whole file,
            // frontmatter included.
            let body = first.to_markdown();
            s.upsert(&old_path, now, body.len() as u64, &body, None)
                .unwrap();
            let note = format!("会议记录里顺带提到 {content} 然后继续讨论别的议程");
            s.upsert("notes/prefs.md", now, note.len() as u64, &note, None)
                .unwrap();
        }

        // Premise: the earlier copy is flagged, and not by the bm25 test.
        let search_text = format!("{title} {}", content.chars().take(100).collect::<String>());
        let flagged = store
            .lock()
            .unwrap()
            .detect_conflicts(&search_text, -2.0, Some("facts/"))
            .unwrap();
        assert_eq!(flagged.len(), 1, "test premise: {flagged:?}");
        assert!(
            flagged[0].1 > -2.0,
            "test premise: bm25 carries no signal on a corpus this small, got {:?}",
            flagged[0].1
        );

        let second = ConsolidatedFact::new(
            "sid-new",
            FactCategory::Interest,
            title.into(),
            content.into(),
            "mem_write".into(),
            vec![],
            0.8,
        );
        let tmp = tempfile::tempdir().unwrap();
        let writer = FactWriter::new(tmp.path()).with_index(store.clone(), -2.0);
        writer.write(&second).unwrap();

        let visible = visible_paths(&store, content);
        assert!(
            !visible.contains(&old_path),
            "the duplicate fact must drop out of search: {visible:?}"
        );
        assert!(
            visible.iter().any(|p| p == "notes/prefs.md"),
            "the note the fact was derived from must survive: {visible:?}"
        );
    }

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
}
