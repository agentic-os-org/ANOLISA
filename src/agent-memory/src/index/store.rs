use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use rusqlite::{Connection, params};

use crate::error::{MemoryError, Result};

use super::SearchHit;

/// SQLite FTS5 BM25 backend used by IndexWorker. All access goes through
/// the inner Connection — guarded by an external Mutex in IndexHandle,
/// which is why mutating methods take `&mut self` (the MutexGuard
/// already provides exclusive access; we use it to drive `transaction`).
pub struct BM25Store {
    conn: Connection,
    /// Time decay lambda for recency-based ranking. `exp(-lambda * age_days)`.
    /// When 0.0, time decay is disabled (default behavior).
    time_decay_lambda: f64,
    /// Time decay alpha: weight of time factor added to search scores.
    time_decay_alpha: f64,
    /// Whether normal search excludes cold files.
    exclude_cold_on_search: bool,
    /// Mount root — derived from the db path so `supersede()` can safely
    /// update on-disk frontmatter without trusting environment variables.
    mount_root: PathBuf,
}

/// Latest schema version this binary knows how to produce.
/// On open, an older DB is upgraded step-by-step until it reaches this
/// version; a newer DB causes the open to fail so a downgraded binary
/// doesn't silently corrupt rows it doesn't understand.
pub(crate) const SCHEMA_VERSION: i64 = 5;

impl BM25Store {
    pub fn open(
        path: &Path,
        time_decay_lambda: f64,
        time_decay_alpha: f64,
        exclude_cold_on_search: bool,
    ) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        // Modest sensible defaults: WAL gives concurrent readers while a
        // writer is committing (today everything is serialised through
        // IndexHandle's Mutex but it costs nothing); busy_timeout shields
        // against external SQLite tools probing the file. NORMAL synchronous
        // is the WAL-recommended setting (full fsync per checkpoint, not
        // per commit).
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        // Derive mount_root from db_path: bm25.db lives at
        // <mount_root>/.anolisa/index/bm25.db, so mount_root is three
        // parents up. Fall back to cwd — the disk frontmatter update
        // is best-effort.
        let mount_root = path
            .parent() // index/
            .and_then(|p| p.parent()) // .anolisa/
            .and_then(|p| p.parent()) // <mount_root>
            .map(|p| p.to_path_buf())
            .unwrap_or_default();

        Self::ensure_schema(&mut conn)?;
        Ok(Self {
            conn,
            time_decay_lambda,
            time_decay_alpha,
            exclude_cold_on_search,
            mount_root,
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::open_in_memory_with(0.01, 0.3, true)
    }

    #[cfg(test)]
    pub fn open_in_memory_with(
        time_decay_lambda: f64,
        time_decay_alpha: f64,
        exclude_cold: bool,
    ) -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        Self::ensure_schema(&mut conn)?;
        Ok(Self {
            conn,
            time_decay_lambda,
            time_decay_alpha,
            exclude_cold_on_search: exclude_cold,
            mount_root: PathBuf::new(),
        })
    }

    #[cfg(test)]
    fn open_for_test(path: &Path) -> Result<Self> {
        Self::open(path, 0.01, 0.3, true)
    }

    /// Ensure the open connection's schema is at SCHEMA_VERSION.
    /// - Fresh DB (version 0) → apply the v1 baseline.
    /// - Older DB → step through `migrate_<N>_to_<N+1>` until current.
    /// - Newer DB → fail loudly (refuse to operate on unknown schema).
    fn ensure_schema(conn: &mut Connection) -> Result<()> {
        let current: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);

        if current > SCHEMA_VERSION {
            return Err(MemoryError::Other(format!(
                "index db schema is at v{current}, binary only supports up to v{SCHEMA_VERSION}; \
                 downgrade is not safe"
            )));
        }

        if current == SCHEMA_VERSION {
            return Ok(());
        }

        // Each migration runs inside its own transaction so a crash mid-
        // upgrade either leaves the DB at the previous version or the next.
        let mut at = current;
        while at < SCHEMA_VERSION {
            let tx = conn.transaction()?;
            match at {
                0 => Self::migrate_0_to_1(&tx)?,
                1 => Self::migrate_1_to_2(&tx)?,
                2 => Self::migrate_2_to_3(&tx)?,
                3 => Self::migrate_3_to_4(&tx)?,
                4 => Self::migrate_4_to_5(&tx)?,
                // Future steps insert here, each bumping `at`.
                n => {
                    return Err(MemoryError::Other(format!(
                        "no migration registered from schema v{n} to v{}",
                        n + 1
                    )));
                }
            }
            at += 1;
            tx.pragma_update(None, "user_version", at)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Initial schema (v1): file metadata table + FTS5 BM25 over body.
    fn migrate_0_to_1(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS files (
                rowid       INTEGER PRIMARY KEY,
                path        TEXT NOT NULL UNIQUE,
                mtime_ms    INTEGER NOT NULL,
                size        INTEGER NOT NULL,
                indexed_at  TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
                path UNINDEXED,
                body,
                tokenize='trigram'
            );
            "#,
        )?;
        Ok(())
    }

    /// Schema v2: add `files_vec` for dense embeddings alongside FTS5.
    fn migrate_1_to_2(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS files_vec (
                path TEXT PRIMARY KEY,
                embedding BLOB NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    /// Schema v3: add cold tracking columns to `files`.
    fn migrate_2_to_3(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        tx.execute_batch(
            r#"
            ALTER TABLE files ADD COLUMN access_count INTEGER DEFAULT 0;
            ALTER TABLE files ADD COLUMN last_accessed_ms INTEGER DEFAULT 0;
            ALTER TABLE files ADD COLUMN is_cold INTEGER DEFAULT 0;
            "#,
        )?;
        Ok(())
    }

    /// Schema v4: add `is_superseded` for conflict resolution.
    fn migrate_3_to_4(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        tx.execute(
            "ALTER TABLE files ADD COLUMN is_superseded INTEGER DEFAULT 0",
            [],
        )?;
        Ok(())
    }

    /// Schema v5: add agent_id column for per-agent memory scoping.
    fn migrate_4_to_5(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        tx.execute(
            "ALTER TABLE files ADD COLUMN agent_id TEXT DEFAULT NULL",
            [],
        )?;
        Ok(())
    }

    /// Insert or replace a file's index entry. `body` is the extracted
    /// text. All writes happen inside one transaction so a crash mid-
    /// upsert can't leave `files` and `files_fts` out of sync.
    pub fn upsert(
        &mut self,
        rel_path: &str,
        mtime_ms: i64,
        size: u64,
        body: &str,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction()?;
        let existing_rowid: Option<i64> = tx
            .query_row(
                "SELECT rowid FROM files WHERE path = ?1",
                params![rel_path],
                |r| r.get(0),
            )
            .ok();

        match existing_rowid {
            Some(rowid) => {
                tx.execute(
                    "UPDATE files SET mtime_ms=?1, size=?2, indexed_at=?3, agent_id=COALESCE(agent_id, ?4) WHERE rowid=?5",
                    params![mtime_ms, size as i64, now, agent_id, rowid],
                )?;
                tx.execute("DELETE FROM files_fts WHERE rowid = ?1", params![rowid])?;
                tx.execute(
                    "INSERT INTO files_fts(rowid, path, body) VALUES (?1, ?2, ?3)",
                    params![rowid, rel_path, body],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO files (path, mtime_ms, size, indexed_at, agent_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![rel_path, mtime_ms, size as i64, now, agent_id],
                )?;
                let rowid = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO files_fts(rowid, path, body) VALUES (?1, ?2, ?3)",
                    params![rowid, rel_path, body],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove a file's index entry. Returns true if any row existed.
    ///
    /// Cascade semantics: if `rel_path` matches a stored row exactly, that
    /// row is removed. Additionally, any descendant whose path starts with
    /// `rel_path + "/"` is removed too — this matters when a *directory* is
    /// renamed or moved out of the tree, in which case notify may not emit
    /// per-file unlinks for every leaf. Without the cascade those rows
    /// would linger as stale FTS hits forever.
    ///
    /// Wraps everything in one transaction so `files` and `files_fts` stay
    /// consistent on partial failure.
    pub fn remove(&mut self, rel_path: &str) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let prefix = format!("{rel_path}/");
        let rowids: Vec<i64> = {
            let mut stmt =
                tx.prepare("SELECT rowid FROM files WHERE path = ?1 OR path LIKE ?2 || '%'")?;
            let rows = stmt.query_map(params![rel_path, prefix], |r| r.get::<_, i64>(0))?;
            rows.flatten().collect()
        };
        let existed = !rowids.is_empty();
        for rid in rowids {
            tx.execute("DELETE FROM files_fts WHERE rowid = ?1", params![rid])?;
            tx.execute("DELETE FROM files WHERE rowid = ?1", params![rid])?;
        }
        // Cascade: remove corresponding vector embeddings.
        tx.execute(
            "DELETE FROM files_vec WHERE path = ?1 OR path LIKE ?2 || '%'",
            params![rel_path, prefix],
        )?;
        tx.commit()?;
        Ok(existed)
    }

    pub fn search(&self, query: &str, top_k: usize, exclude_cold: bool) -> Result<Vec<SearchHit>> {
        self.search_scoped(query, top_k, exclude_cold, None)
    }

    /// Search with optional agent scope filter.
    /// `agent_scope` can be:
    /// - None: return all results (shared mode, default)
    /// - Some("isolated:<agent_id>"): only results tagged with this agent_id
    /// - Some("filter:<agent_id>"): results tagged with this agent_id plus
    ///   any unscoped (agent_id IS NULL) memories
    ///
    /// Returns `InvalidArgument` when the scope prefix is recognised but the
    /// agent_id contains characters that would let it escape the parameterised
    /// binding path (`'`, `"`, `;`, `\`, `/`, control bytes). Callers must
    /// surface the error rather than silently falling back to shared mode,
    /// otherwise a misconfigured `MCP_CLIENT_NAME` would silently widen the
    /// visibility domain.
    pub fn search_scoped(
        &self,
        query: &str,
        top_k: usize,
        exclude_cold: bool,
        agent_scope: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        if query.trim().is_empty() {
            return Err(MemoryError::InvalidArgument("empty search query".into()));
        }
        let fts_q = sanitize_fts_query(query);
        if fts_q.is_empty() {
            return Ok(Vec::new());
        }

        let cold_filter = if exclude_cold {
            "AND f.is_cold = 0"
        } else {
            ""
        };
        let superseded_filter = "AND f.is_superseded = 0";

        let scope = resolve_agent_scope(agent_scope)?;

        // The FTS5 `trigram` tokenizer emits one token per 3-character
        // window. A query term shorter than 3 chars (common for CJK words
        // like "花名" / "小云") produces zero tokens and MATCH silently
        // returns nothing. When any term is that short we fall back to a
        // `body LIKE '%term%'` substring scan — trigram can't help here,
        // but a linear scan over the (small) indexed corpus still recalls
        // the right rows. Long-token-only queries keep the BM25 path.
        let tokens: Vec<&str> = fts_q.split_whitespace().collect();
        let needs_like_fallback = tokens.iter().any(|t| t.chars().count() < 3);
        if needs_like_fallback {
            return self.search_like(&tokens, top_k, cold_filter, superseded_filter, &scope);
        }

        // ── BM25 / FTS5 MATCH path (all terms ≥ 3 chars) ──────────────
        // Agent scope filter: when set, only return results from the specified agent.
        // Use parameterised binding (`?3`) rather than `format!()` so an
        // attacker-controlled `MCP_CLIENT_NAME` cannot escape the literal.
        // `/` and `\` are rejected so a client name like `org/team/agent`
        // cannot be confused with a path component elsewhere in the query.
        let (agent_filter, agent_param) = agent_scope_sql_match(&scope);

        // Join with files to get mtime for time decay.
        let sql = format!(
            r#"
            SELECT f.path,
                   snippet(files_fts, 1, '«', '»', '…', 16) AS snip,
                   bm25(files_fts) AS rank,
                   body,
                   f.mtime_ms
            FROM files_fts
            JOIN files f ON f.rowid = files_fts.rowid
            WHERE files_fts MATCH ?1 {cold_filter} {superseded_filter} {agent_filter}
            ORDER BY rank
            LIMIT ?2
        "#
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<(String, String, f64, String, i64)> = if let Some(ref agent_id) = agent_param
        {
            stmt.query_map(params![fts_q, top_k as i64, agent_id], |row| {
                let body: String = row.get(3)?;
                let mtime_ms: i64 = row.get(4)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    body,
                    mtime_ms,
                ))
            })?
            .flatten()
            .collect()
        } else {
            stmt.query_map(params![fts_q, top_k as i64], |row| {
                let body: String = row.get(3)?;
                let mtime_ms: i64 = row.get(4)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    body,
                    mtime_ms,
                ))
            })?
            .flatten()
            .collect()
        };

        // OR fallback: FTS5 implicit-AND semantics return 0 results
        // when any single query term is absent from the corpus.
        // Natural-language prompts (e.g. auto-recall queries) routinely
        // contain common words ("What", "is", "Answer") that don't
        // appear in any memory file, causing every AND query to fail.
        // When the AND path returns nothing and we have multiple tokens,
        // retry with OR-joined quoted tokens so partial matches still
        // surface.
        let rows: Vec<(String, String, f64, String, i64)> = if rows.is_empty() && tokens.len() > 1 {
            let or_q = tokens
                .iter()
                .map(|t| format!("\"{}\"", t))
                .collect::<Vec<_>>()
                .join(" OR ");
            let or_sql = format!(
                r#"
                SELECT f.path,
                       snippet(files_fts, 1, '«', '»', '…', 16) AS snip,
                       bm25(files_fts) AS rank,
                       body,
                       f.mtime_ms
                FROM files_fts
                JOIN files f ON f.rowid = files_fts.rowid
                WHERE files_fts MATCH ?1 {cold_filter} {superseded_filter} {agent_filter}
                ORDER BY rank
                LIMIT ?2
                "#,
                cold_filter = cold_filter,
                superseded_filter = superseded_filter,
                agent_filter = agent_filter,
            );
            let mut or_stmt = self.conn.prepare(&or_sql)?;
            if let Some(ref agent_id) = agent_param {
                or_stmt
                    .query_map(params![or_q, top_k as i64, agent_id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, f64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    })?
                    .flatten()
                    .collect()
            } else {
                or_stmt
                    .query_map(params![or_q, top_k as i64], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, f64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    })?
                    .flatten()
                    .collect()
            }
        } else {
            rows
        };
        let mut out: Vec<SearchHit> = rows
            .into_iter()
            .map(|(path, snippet, bm25_score, _body, mtime_ms)| {
                let decay = time_decay(mtime_ms, self.time_decay_lambda);
                // FTS5's `bm25()` is negative and *more negative is a better
                // match* — which is why the SQL above orders by `rank`
                // ascending to keep the best `top_k` rows. Negate it so
                // `SearchHit::score` means "higher is better" like the two
                // sibling scorers (`search_like`, `search_vec`) and so the
                // additive recency boost lifts a good match instead of being
                // subtracted from it: sorted descending, the raw value ranked
                // the weakest matched row first and handed
                // `search_hybrid_inner`'s RRF the BM25 ranks in reverse. FTS5
                // clamps a negative IDF to ~0, so relevance is never below
                // zero on this path.
                let relevance = -bm25_score;
                let adjusted_score = relevance + self.time_decay_alpha * decay;
                let suspicious =
                    crate::safety::looks_like_prompt_injection(&strip_snippet_markers(&snippet));
                SearchHit {
                    path,
                    snippet,
                    score: adjusted_score,
                    suspicious,
                }
            })
            .collect();

        // Best match first — `score` is "higher is better" on every path.
        out.sort_by(|a, b| b.score.total_cmp(&a.score));

        Ok(out)
    }

    /// LIKE-based substring fallback for queries that the trigram tokenizer
    /// cannot serve (any term < 3 chars, e.g. short CJK words). AND-joins
    /// one `body LIKE ? ESCAPE '\'` clause per token so multi-term queries
    /// keep "all terms must appear" semantics; when the AND query returns
    /// zero rows and there are multiple tokens, retries with OR-joined
    /// clauses so partial matches still surface (mirroring the BM25 path's
    /// OR fallback). `_` and `%` surviving `sanitize_fts_query` are
    /// backslash-escaped so they match literally. Scoring is a coarse
    /// term-frequency sum plus time decay — no BM25 is available off the
    /// MATCH path, but recall (not ranking) is the goal.
    fn search_like(
        &self,
        tokens: &[&str],
        top_k: usize,
        cold_filter: &str,
        superseded_filter: &str,
        scope: &AgentScope,
    ) -> Result<Vec<SearchHit>> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let (agent_filter, agent_param) = agent_scope_sql_like(scope);

        let run_query =
            |joiner: &str, pool: usize, rank_matches: bool| -> Result<Vec<(String, String, i64)>> {
                let like_clause = tokens
                    .iter()
                    .map(|_| "files_fts.body LIKE ? ESCAPE '\\'")
                    .collect::<Vec<_>>()
                    .join(joiner);

                // OR-joined queries match a superset of rows, so the pool cap
                // would otherwise truncate arbitrarily (SQLite returns rows in
                // unspecified order without ORDER BY) and could drop a strong
                // multi-token match past the LIMIT. Rank by matched-token
                // count (LIKE evaluates to 0/1 in SQLite) so LIMIT keeps the
                // strongest rows; exact frequency scoring still happens in
                // Rust below. The AND path skips this: every row already
                // matches all tokens, so the count is a constant.
                let order_clause = if rank_matches {
                    let match_sum = tokens
                        .iter()
                        .map(|_| "(files_fts.body LIKE ? ESCAPE '\\')")
                        .collect::<Vec<_>>()
                        .join(" + ");
                    format!("ORDER BY ({match_sum}) DESC")
                } else {
                    // No SQL ORDER BY on the AND path: the LIKE path has no
                    // rank, and ordering by mtime would discard high-frequency
                    // old documents before Rust scoring.
                    String::new()
                };

                // Parenthesise the LIKE clause so OR-joined terms don't bind
                // looser than the trailing AND filters.
                let sql = format!(
                    r#"
                SELECT f.path, files_fts.body, f.mtime_ms
                FROM files_fts
                JOIN files f ON f.rowid = files_fts.rowid
                WHERE ({like_clause}) {cold_filter} {superseded_filter} {agent_filter}
                {order_clause}
                LIMIT ?
                "#
                );

                let like_patterns: Vec<String> = tokens.iter().map(|t| like_pattern(t)).collect();
                let mut bind: Vec<rusqlite::types::Value> = like_patterns
                    .iter()
                    .map(|s| rusqlite::types::Value::Text(s.clone()))
                    .collect();
                if let Some(ref agent_id) = agent_param {
                    bind.push(rusqlite::types::Value::Text(agent_id.clone()));
                }
                // ORDER BY comes after WHERE in the SQL text, so its pattern
                // params bind after the WHERE + agent params.
                if rank_matches {
                    for s in &like_patterns {
                        bind.push(rusqlite::types::Value::Text(s.clone()));
                    }
                }
                bind.push(rusqlite::types::Value::Integer(pool.max(1) as i64));

                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(bind.iter()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
                Ok(rows.flatten().collect())
            };

        let rows = run_query(" AND ", top_k * 4, false)?;
        // OR fallback mirroring the BM25 MATCH path: AND semantics return
        // zero rows when any single token is absent from the corpus, which
        // is routine for auto-recall keyword queries carrying filler tokens
        // (e.g. "um"). Retry with OR so partial matches surface; SQL-side
        // ranking keeps the strongest rows within the pool and the
        // frequency scoring below decides the final order. Pool widened to
        // at least 50 rows (the `detect_conflicts_like` precedent) so the
        // Rust scorer sees a broad candidate set.
        let rows = if rows.is_empty() && tokens.len() > 1 {
            run_query(" OR ", (top_k * 4).max(50), true)?
        } else {
            rows
        };

        let mut out: Vec<SearchHit> = Vec::new();
        for (path, body, mtime_ms) in rows {
            // Anchor the snippet on the shortest query token that actually
            // occurs in this body. OR-fallback rows may lack the globally
            // shortest token, which would degrade the snippet to the first
            // 24 chars of the body — useless to the auto-recall adapter,
            // which injects only the snippet. Matching is ASCII
            // case-insensitive to mirror SQLite LIKE, and the needle is the
            // body-cased occurrence so `make_snippet` can anchor on it.
            let needle = tokens
                .iter()
                .filter_map(|t| ascii_ci_find(&body, t).map(|pos| &body[pos..pos + t.len()]))
                .min_by_key(|m| m.chars().count())
                .unwrap_or("");
            let snippet = make_snippet(&body, needle, 24);
            let suspicious =
                crate::safety::looks_like_prompt_injection(&strip_snippet_markers(&snippet));
            // Case-insensitive frequency: SQLite LIKE is ASCII
            // case-insensitive by default, so a case-sensitive `matches()`
            // would under-count (to zero) for mixed-case ASCII matches,
            // collapsing the score to pure time decay. CJK is unaffected
            // (LIKE is case-sensitive for non-ASCII).
            let body_lower = body.to_lowercase();
            let freq: f64 = tokens
                .iter()
                .map(|t| body_lower.matches(&t.to_lowercase()).count() as f64)
                .sum();
            let decay = time_decay(mtime_ms, self.time_decay_lambda);
            out.push(SearchHit {
                path,
                snippet,
                score: freq + self.time_decay_alpha * decay,
                suspicious,
            });
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out.truncate(top_k);
        Ok(out)
    }

    /// Deep search: include cold files too.
    pub fn search_deep(&self, query: &str, top_k: usize) -> Result<Vec<SearchHit>> {
        self.search(query, top_k, false)
    }

    /// Compact the index: mark old, never-accessed files as cold and
    /// remove them from the FTS index. Returns the number of files compacted.
    ///
    /// Cold criteria: `access_count == 0 AND age > cold_after_days`.
    /// Files with `access_count > 0` are never compacted (warm protection).
    pub fn compact(&mut self, cold_after_days: u64) -> Result<usize> {
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let cutoff_ms = now_ms - (cold_after_days as i64 * 86_400_000);

        let tx = self.conn.transaction()?;

        // Find files eligible for cold marking.
        let cold_paths: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT path FROM files WHERE access_count = 0 \
                 AND mtime_ms < ?1 AND is_cold = 0",
            )?;
            let rows = stmt.query_map(params![cutoff_ms], |r| r.get::<_, String>(0))?;
            rows.flatten().collect()
        };

        // Mark them as cold.
        {
            let mut stmt = tx.prepare("UPDATE files SET is_cold = 1 WHERE path = ?1")?;
            for path in &cold_paths {
                let _ = stmt.execute(params![path]);
            }
        }

        tx.commit()?;
        Ok(cold_paths.len())
    }

    /// Return counts of warm vs cold files.
    pub fn warm_cold_counts(&self) -> Result<(usize, usize)> {
        let warm: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM files WHERE is_cold = 0", [], |r| {
                    r.get(0)
                })?;
        let cold: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM files WHERE is_cold = 1", [], |r| {
                    r.get(0)
                })?;
        Ok((warm as usize, cold as usize))
    }

    /// Detect potential conflicts: search for files similar to the given
    /// text and return those with BM25 score above the threshold.
    pub fn detect_conflicts(&self, text: &str, threshold: f64) -> Result<Vec<(String, f64)>> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let fts_q = sanitize_fts_query(text);
        if fts_q.is_empty() {
            return Ok(Vec::new());
        }

        // Same short-CJK gap as `search_scoped`: a trigram query term < 3
        // chars produces zero tokens and MATCH silently returns nothing,
        // so duplicates/contradictions with short-CJK keywords would slip
        // past conflict detection. Fall back to a LIKE substring scan.
        let tokens: Vec<&str> = fts_q.split_whitespace().collect();
        if tokens.iter().any(|t| t.chars().count() < 3) {
            return self.detect_conflicts_like(&tokens, threshold);
        }

        // Search excluding cold and superseded files.
        let sql = r#"
            SELECT f.path, bm25(files_fts) AS rank
            FROM files_fts
            JOIN files f ON f.rowid = files_fts.rowid
            WHERE files_fts MATCH ?1 AND f.is_cold = 0 AND f.is_superseded = 0
            ORDER BY rank
            LIMIT 5
        "#;
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![fts_q], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?;

        let results: Vec<(String, f64)> = rows
            .flatten()
            .filter(|(_, score)| *score >= threshold)
            .collect();

        Ok(results)
    }

    /// LIKE-based conflict detection fallback for short-CJK queries (any term
    /// < 3 chars). No BM25 is available off the MATCH path, so the score is a
    /// term-frequency sum plus time decay — always ≥ 0, hence ≥ the
    /// typically-negative BM25 `threshold`. This is deliberately
    /// recall-oriented: when we can't rank, flag every substring match as a
    /// potential conflict rather than silently miss a duplicate/contradiction.
    /// The caller still reviews flagged conflicts before superseding.
    fn detect_conflicts_like(&self, tokens: &[&str], threshold: f64) -> Result<Vec<(String, f64)>> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let like_clause = tokens
            .iter()
            .map(|_| "files_fts.body LIKE ? ESCAPE '\\'")
            .collect::<Vec<_>>()
            .join(" AND ");
        // No ORDER BY: the MATCH path orders by BM25 rank, but the LIKE path
        // has no rank, and ordering by mtime would cut high-frequency old
        // documents before scoring. Fetch a generous pool, score in Rust,
        // then sort + truncate.
        let sql = format!(
            r#"
            SELECT f.path, files_fts.body, f.mtime_ms
            FROM files_fts
            JOIN files f ON f.rowid = files_fts.rowid
            WHERE {like_clause} AND f.is_cold = 0 AND f.is_superseded = 0
            LIMIT 50
            "#
        );
        let like_patterns: Vec<String> = tokens.iter().map(|t| like_pattern(t)).collect();
        let bind: Vec<rusqlite::types::Value> = like_patterns
            .iter()
            .map(|s| rusqlite::types::Value::Text(s.clone()))
            .collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;

        let mut out: Vec<(String, f64)> = Vec::new();
        for row in rows.flatten() {
            let (path, body, mtime_ms) = row;
            // Case-insensitive frequency: SQLite LIKE is ASCII
            // case-insensitive, so a Rust `matches()` (case-sensitive) would
            // under-count and zero the score for mixed-case matches.
            let body_lower = body.to_lowercase();
            let freq: f64 = tokens
                .iter()
                .map(|t| body_lower.matches(&t.to_lowercase()).count() as f64)
                .sum();
            let decay = time_decay(mtime_ms, self.time_decay_lambda);
            let score = freq + self.time_decay_alpha * decay;
            if score >= threshold {
                out.push((path, score));
            }
        }
        // Highest score first, then cap at 5 to match the MATCH path's LIMIT.
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out.truncate(5);
        Ok(out)
    }

    /// Mark a file as superseded by another file. The superseded file
    /// remains on disk but is excluded from normal search.
    pub fn supersede(&mut self, old_path: &str, new_id: &str) -> Result<()> {
        // Update the database flag.
        self.conn.execute(
            "UPDATE files SET is_superseded = 1 WHERE path = ?1",
            params![old_path],
        )?;

        // Update the frontmatter in the file on disk.
        // This is best-effort — the DB flag is the authoritative source.
        // mount_root is derived from the db path (not an env var), and we
        // canonicalize before checking containment to guard against path
        // traversal via `..` segments.
        if !self.mount_root.as_os_str().is_empty() {
            let file_path = self.mount_root.join(old_path);
            // Resolve symlinks and `..` before checking containment.
            let canonical = file_path.canonicalize().unwrap_or(file_path.clone());
            if canonical.starts_with(&self.mount_root) && canonical.is_file() {
                let _ = add_superseded_frontmatter(&canonical, new_id);
            }
        }

        Ok(())
    }

    /// Store a dense embedding vector for `rel_path`. The vector is
    /// serialised as a little-endian f32 BLOB.
    pub fn upsert_vec(&mut self, rel_path: &str, embedding: &[f32]) -> Result<()> {
        let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.conn.execute(
            "INSERT OR REPLACE INTO files_vec (path, embedding) VALUES (?1, ?2)",
            params![rel_path, blob],
        )?;
        Ok(())
    }

    /// Vector-only search: returns `(path, cosine_similarity)` ordered
    /// by descending similarity with time decay boost.
    pub fn search_vec(&self, query_vec: &[f32], top_k: usize) -> Result<Vec<(String, f64)>> {
        let q_norm = l2_normalise(query_vec);

        // JOIN with files to get mtime in a single query (avoids N+1).
        let mut stmt = self.conn.prepare(
            "SELECT v.path, v.embedding, f.mtime_ms \
             FROM files_vec v LEFT JOIN files f ON f.path = v.path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?;

        let mut scores: Vec<(String, f64)> = Vec::new();
        for row in rows {
            let (path, blob, mtime_opt) = match row {
                Ok(r) => r,
                Err(_) => continue,
            };
            let stored = blob_to_f32(&blob);
            if stored.len() != q_norm.len() {
                continue;
            }
            let similarity = dot_product(&q_norm, &stored) as f64;
            // Filter non-finite scores (from zero-norm or degenerate embeddings).
            if !similarity.is_finite() {
                tracing::debug!("skipping non-finite similarity for {path}: {similarity}");
                continue;
            }
            let decay = time_decay(mtime_opt.unwrap_or(0), self.time_decay_lambda);
            let adjusted = similarity * (1.0 + self.time_decay_alpha * decay);
            scores.push((path, adjusted));
        }

        // total_cmp is well-defined and gives a deterministic order even for
        // edge-case values (subnormals, -0.0).
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        scores.truncate(top_k);
        Ok(scores)
    }

    /// Hybrid search: combines BM25 keyword ranking with vector cosine
    /// similarity using reciprocal rank fusion (RRF, k=60).
    ///
    /// This method is the one callers should use when both the index and
    /// an embedding provider are available.
    pub fn search_hybrid(
        &self,
        query: &str,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchHit>> {
        self.search_hybrid_inner(query, query_vec, top_k, self.exclude_cold_on_search)
    }

    /// Hybrid search with explicit cold control.
    pub fn search_hybrid_with_cold(
        &self,
        query: &str,
        query_vec: &[f32],
        top_k: usize,
        exclude_cold: bool,
    ) -> Result<Vec<SearchHit>> {
        self.search_hybrid_inner(query, query_vec, top_k, exclude_cold)
    }

    fn search_hybrid_inner(
        &self,
        query: &str,
        query_vec: &[f32],
        top_k: usize,
        exclude_cold: bool,
    ) -> Result<Vec<SearchHit>> {
        // Run both search strategies.
        let bm25_hits = self.search(query, top_k * 2, exclude_cold);
        let vec_hits = self.search_vec(query_vec, top_k * 2);

        let (bm25_hits, vec_hits): (Vec<SearchHit>, Vec<(String, f64)>) =
            match (bm25_hits, vec_hits) {
                (Ok(b), Ok(v)) => (b, v),
                (Err(e), Ok(v)) => {
                    tracing::warn!("hybrid search: BM25 failed ({e}); falling back to vector-only");
                    (Vec::new(), v)
                }
                (Ok(b), Err(e)) => {
                    tracing::warn!("hybrid search: vector failed ({e}); falling back to BM25-only");
                    (b, Vec::new())
                }
                (Err(bm25_err), Err(vec_err)) => {
                    tracing::warn!(
                        "hybrid search: both BM25 ({bm25_err}) and vector ({vec_err}) failed"
                    );
                    return Ok(Vec::new());
                }
            };

        if bm25_hits.is_empty() && vec_hits.is_empty() {
            return Ok(Vec::new());
        }
        if vec_hits.is_empty() {
            return Ok(bm25_hits.into_iter().take(top_k).collect());
        }
        if bm25_hits.is_empty() {
            // Reconstruct SearchHit from vector-only results.
            return Ok(vec_hits
                .into_iter()
                .take(top_k)
                .map(|(path, score)| SearchHit {
                    path,
                    snippet: String::new(),
                    score,
                    suspicious: false,
                })
                .collect());
        }

        // RRF: score = Σ 1/(k + rank_i) for each result set.
        const RRF_K: f64 = 60.0;
        let mut rrf: std::collections::HashMap<String, (f64, i64)> =
            std::collections::HashMap::new(); // (rrf_score, mtime_ms)
        let mut snippets: std::collections::HashMap<String, (String, bool)> =
            std::collections::HashMap::new();

        for (rank, hit) in bm25_hits.iter().enumerate() {
            let rrf_score = 1.0 / (RRF_K + (rank as f64 + 1.0));
            let entry = rrf.entry(hit.path.clone()).or_insert((0.0, 0));
            entry.0 += rrf_score;
            if entry.1 == 0 {
                // BM25 hits don't have mtime; look it up.
                entry.1 = self.mtime_for(&hit.path).unwrap_or(0);
            }
            snippets
                .entry(hit.path.clone())
                .or_insert((hit.snippet.clone(), hit.suspicious));
        }
        for (rank, (path, _)) in vec_hits.iter().enumerate() {
            let rrf_score = 1.0 / (RRF_K + (rank as f64 + 1.0));
            let entry = rrf.entry(path.clone()).or_insert((0.0, 0));
            entry.0 += rrf_score;
            if entry.1 == 0 {
                entry.1 = self.mtime_for(path).unwrap_or(0);
            }
        }

        // Apply time decay to each merged result.
        let mut merged: Vec<(String, f64)> = rrf
            .into_iter()
            .map(|(path, (rrf_score, mtime_ms))| {
                let decay = time_decay(mtime_ms, self.time_decay_lambda);
                let final_score = rrf_score + self.time_decay_alpha * decay;
                (path, final_score)
            })
            .collect();
        merged.sort_by(|a, b| b.1.total_cmp(&a.1));
        merged.truncate(top_k);

        Ok(merged
            .into_iter()
            .map(|(path, score)| {
                let (snippet, suspicious) = snippets.remove(&path).unwrap_or_default();
                SearchHit {
                    path,
                    snippet,
                    score,
                    suspicious,
                }
            })
            .collect())
    }

    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn known_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM files")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let out: Vec<String> = rows.flatten().collect();
        Ok(out)
    }

    /// Paths present in the `files` (BM25) table that have no matching row
    /// in `files_vec`. Used by full_scan's backfill pass to compute vectors
    /// for files that exist on disk and are BM25-indexed but were never
    /// embedded — e.g. written before an embedding provider was configured,
    /// or recovered after an inotify overflow.
    pub fn paths_without_vec(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.path FROM files f \
             LEFT JOIN files_vec v ON v.path = f.path \
             WHERE v.path IS NULL",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let out: Vec<String> = rows.flatten().collect();
        Ok(out)
    }

    pub fn mtime_for(&self, rel_path: &str) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT mtime_ms FROM files WHERE path = ?1",
                params![rel_path],
                |r| r.get(0),
            )
            .ok()
    }
}

/// Strip FTS5 snippet highlight markers («, », …) so that prompt-injection
/// detection runs against the cleaned text rather than the decorated snippet.
fn strip_snippet_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '«' | '»' | '…' => {}
            other => out.push(other),
        }
    }
    out
}

// ── agent scope helpers (shared by MATCH and LIKE search paths) ───────

#[derive(Clone, Copy)]
enum ScopeKind {
    /// Only the agent's own memories.
    Isolated,
    /// Agent's own plus unscoped (agent_id IS NULL) memories.
    Filter,
}

/// A resolved agent scope: `None` is shared (no filter), `Some((kind, id))`
/// constrains results to the named agent. Centralising this here keeps the
/// agent_id validation in one place — both the MATCH and LIKE search paths
/// must reject the same set of SQL-meta / path-separator characters so a
/// misconfigured `MCP_CLIENT_NAME` can't widen the visibility domain.
type AgentScope = Option<(ScopeKind, String)>;

fn resolve_agent_scope(agent_scope: Option<&str>) -> Result<AgentScope> {
    let Some(scope) = agent_scope else {
        return Ok(None);
    };
    if !scope.starts_with("isolated:") && !scope.starts_with("filter:") {
        return Ok(None);
    }
    let agent_id = scope.split_once(':').map(|x| x.1).unwrap_or("");
    if agent_id.contains(|c: char| {
        c == '\''
            || c == '"'
            || c == ';'
            || c == '\\'
            || c == '/'
            || c == '\0'
            || c == '\n'
            || c == '\r'
    }) {
        return Err(MemoryError::InvalidArgument(format!(
            "agent_scope contains invalid characters: {agent_id:?}"
        )));
    }
    let kind = if scope.starts_with("isolated:") {
        ScopeKind::Isolated
    } else {
        ScopeKind::Filter
    };
    Ok(Some((kind, agent_id.to_string())))
}

/// Agent filter SQL for the FTS5 MATCH path. Uses explicit `?3` because the
/// MATCH query numbers its parameters (`?1` = query, `?2` = limit).
fn agent_scope_sql_match(scope: &AgentScope) -> (String, Option<String>) {
    match scope {
        None => (String::new(), None),
        Some((ScopeKind::Isolated, id)) => ("AND f.agent_id = ?3".to_string(), Some(id.clone())),
        Some((ScopeKind::Filter, id)) => (
            "AND (f.agent_id = ?3 OR f.agent_id IS NULL)".to_string(),
            Some(id.clone()),
        ),
    }
}

/// Agent filter SQL for the LIKE fallback path. Uses bare positional `?`
/// because the LIKE query binds a variable number of leading pattern params,
/// so fixed `?3` numbering would be wrong.
fn agent_scope_sql_like(scope: &AgentScope) -> (String, Option<String>) {
    match scope {
        None => (String::new(), None),
        Some((ScopeKind::Isolated, id)) => ("AND f.agent_id = ?".to_string(), Some(id.clone())),
        Some((ScopeKind::Filter, id)) => (
            "AND (f.agent_id = ? OR f.agent_id IS NULL)".to_string(),
            Some(id.clone()),
        ),
    }
}

/// Build a `LIKE` pattern matching `token` as a substring, backslash-escaping
/// the `%` / `_` / `\` wildcards so they match literally. `sanitize_fts_query`
/// keeps `_` (and drops `%`), but escaping all three is defensive against
/// future sanitisation changes.
fn like_pattern(token: &str) -> String {
    let mut s = String::with_capacity(token.len() + 4);
    s.push('%');
    for c in token.chars() {
        match c {
            '_' | '%' | '\\' => {
                s.push('\\');
                s.push(c);
            }
            other => s.push(other),
        }
    }
    s.push('%');
    s
}

/// Byte-wise ASCII case-insensitive substring search, mirroring SQLite
/// LIKE semantics (ASCII case-insensitive, exact bytes otherwise).
/// Returned offsets are always char boundaries: a valid UTF-8 needle
/// never starts or ends with a continuation byte, and ASCII case
/// folding never maps to or from non-ASCII bytes, so a byte-level
/// match cannot begin or end mid-character.
fn ascii_ci_find(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    h.windows(n.len()).position(|w| w.eq_ignore_ascii_case(n))
}

/// Produce a snippet window of `radius` chars on each side of the first
/// occurrence of `needle` in `body`, wrapped in `«»` and elided with `…` so
/// it is shaped like the FTS5 `snippet()` output the MATCH path returns.
/// Falls back to the first `radius` chars of the body if `needle` is absent.
fn make_snippet(body: &str, needle: &str, radius: usize) -> String {
    if needle.is_empty() {
        return body.chars().take(radius).collect();
    }
    let npos = match body.find(needle) {
        Some(p) => p,
        None => return body.chars().take(radius).collect(),
    };
    let before: Vec<char> = body[..npos].chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let after_start = (npos + needle.len()).min(body.len());
    let after: Vec<char> = body[after_start..].chars().collect();

    let start = before.len().saturating_sub(radius);
    let end = radius.min(after.len());

    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.extend(before[start..].iter());
    s.push('«');
    s.extend(needle_chars.iter());
    s.push('»');
    s.extend(after[..end].iter());
    if end < after.len() {
        s.push('…');
    }
    s
}

/// Convert a raw query into something safe for FTS5: drop quotes /
/// punctuation that confuse the parser, AND-join surviving tokens.
/// `-` is dropped because FTS5 interprets a leading `-` as the NOT
/// operator, so naïvely keeping it would silently invert match intent
/// (`hello-world` → match docs containing "hello" but NOT "world").
fn sanitize_fts_query(q: &str) -> String {
    q.split_whitespace()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '.'))
                .collect::<String>()
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn mtime_ms_of(meta: &std::fs::Metadata) -> i64 {
    let dur = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok());
    match dur {
        Some(d) => d.as_millis() as i64,
        None => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    }
}

// ── vector helpers ─────────────────────────────────────────────

/// Compute exponential time decay: `exp(-lambda * age_days)`.
/// Returns 1.0 for very recent files, approaching 0 for old files.
/// When `lambda` is 0, always returns 1.0 (no decay).
pub(crate) fn time_decay(mtime_ms: i64, lambda: f64) -> f64 {
    if lambda == 0.0 {
        return 1.0;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let age_days = ((now_ms - mtime_ms).max(0) as f64) / 86_400_000.0;
    (-lambda * age_days).exp()
}

fn l2_normalise(vec: &[f32]) -> Vec<f32> {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return vec.to_vec();
    }
    vec.iter().map(|x| x / norm).collect()
}

fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn blob_to_f32(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Add `superseded_by` to the frontmatter of an existing markdown file.
fn add_superseded_frontmatter(path: &std::path::Path, new_id: &str) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;
    if content.contains("superseded_by:") {
        return Ok(()); // Already superseded.
    }
    // Insert superseded_by after the first --- line if it exists.
    if let Some(pos) = content.find("---\n") {
        let after_first = pos + 4;
        if let Some(second_pos) = content[after_first..].find("---\n") {
            // Insert before the closing ---.
            let insert_point = after_first + second_pos;
            let new_content = format!(
                "{}superseded_by: {}\n{}",
                &content[..insert_point],
                new_id,
                &content[insert_point..]
            );
            std::fs::write(path, new_content)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_search_remove_roundtrip() {
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("notes/a.md", 100, 10, "rust loves ownership", None)
            .unwrap();
        s.upsert("notes/b.md", 100, 10, "python uses gc", None)
            .unwrap();

        let hits = s.search("rust", 5, true).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "notes/a.md");

        s.remove("notes/a.md").unwrap();
        let hits = s.search("rust", 5, true).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_handles_chinese() {
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("a.md", 0, 0, "你好世界 hello", None).unwrap();
        let hits = s.search("hello", 5, true).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_two_char_cjk_query_recalls_via_like_fallback() {
        // Regression for issue #1254: the trigram tokenizer needs ≥3 chars
        // per term, so a 2-char CJK word like "花名" silently returned
        // zero hits via the MATCH path. The LIKE fallback must recall it.
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("notes/observed/x.md", 0, 0, "用户的花名是\"小云\"。", None)
            .unwrap();
        s.upsert("notes/other.md", 0, 0, "无关内容 weather report", None)
            .unwrap();

        // 2-char term that previously returned 0 hits.
        let hits = s.search("花名", 5, true).unwrap();
        assert_eq!(hits.len(), 1, "花名 should recall the observed note");
        assert_eq!(hits[0].path, "notes/observed/x.md");
        // Snippet should bracket the matched term.
        assert!(hits[0].snippet.contains("«花名»"));

        // The other 2-char term in the same body also recalls.
        let hits = s.search("小云", 5, true).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "notes/observed/x.md");
    }

    #[test]
    fn search_three_char_cjk_still_uses_trigram() {
        // 3-char terms must keep working through the BM25/trigram path
        // (the fallback must not swallow queries trigram can serve).
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("a.md", 0, 0, "用户的花名是小云", None).unwrap();
        let hits = s.search("花名是", 5, true).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_mixed_short_and_long_tokens_and_joins() {
        // "花名 小云" — both terms 2 chars → LIKE fallback, AND-joined:
        // only bodies containing both substrings should match.
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("both.md", 0, 0, "花名叫做小云没错", None).unwrap();
        s.upsert("only_one.md", 0, 0, "这里只有花名没有别的", None)
            .unwrap();
        let hits = s.search("花名 小云", 5, true).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["both.md"]);
    }

    #[test]
    fn search_like_respects_agent_scope() {
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("own.md", 0, 0, "花名是小云", Some("alpha"))
            .unwrap();
        s.upsert("other.md", 0, 0, "花名是大云", Some("beta"))
            .unwrap();
        s.upsert("legacy.md", 0, 0, "花名是老云", None).unwrap();

        // isolated:alpha sees only its own — not beta, not the unscoped row.
        let hits = s
            .search_scoped("花名", 10, true, Some("isolated:alpha"))
            .unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["own.md"]);

        // filter:alpha sees its own + unscoped, never beta.
        let hits = s
            .search_scoped("花名", 10, true, Some("filter:alpha"))
            .unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert!(paths.contains(&"own.md"));
        assert!(paths.contains(&"legacy.md"));
        assert!(!paths.contains(&"other.md"));
    }

    #[test]
    fn search_like_skips_cold_and_superseded() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        s.upsert("warm.md", now_ms, 10, "花名是小云", None).unwrap();
        s.upsert("cold.md", now_ms - 100 * 86_400_000, 10, "花名是老云", None)
            .unwrap();
        s.compact(30).unwrap();
        // Normal (exclude_cold) search hides the cold row.
        let hits = s.search("花名", 10, true).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["warm.md"]);
        // Deep search (include cold) finds both.
        let hits = s.search("花名", 10, false).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn search_like_handles_underscore_literal() {
        // `_` survives sanitize_fts_query and is a LIKE wildcard — it must
        // be escaped so "a_" doesn't match "ax". Use a 2-char token so the
        // query takes the LIKE path (trigram needs ≥ 3 chars); a 3-char
        // "a_b" would go through BM25/MATCH and not exercise the LIKE
        // escaping at all.
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("lit.md", 0, 0, "the token is a_b here", None)
            .unwrap();
        s.upsert("wild.md", 0, 0, "the token is axb here", None)
            .unwrap();
        let hits = s.search("a_", 10, true).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["lit.md"], "underscore must match literally");
    }

    #[test]
    fn empty_query_errors() {
        let s = BM25Store::open_in_memory().unwrap();
        assert!(matches!(
            s.search("   ", 5, true),
            Err(MemoryError::InvalidArgument(_))
        ));
    }

    #[test]
    fn remove_cascades_to_dir_children() {
        // Regression: pre-fix `remove("notes")` only deleted a row with
        // exact path "notes" and left `notes/a.md` + `notes/sub/b.md`
        // behind as stale FTS hits. With the cascade, removing the dir
        // prefix nukes every descendant in one transaction.
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("notes/a.md", 0, 0, "alpha", None).unwrap();
        s.upsert("notes/sub/b.md", 0, 0, "beta", None).unwrap();
        s.upsert("other/c.md", 0, 0, "gamma", None).unwrap();

        let existed = s.remove("notes").unwrap();
        assert!(existed, "removing a populated prefix must report true");

        let paths = s.known_paths().unwrap();
        assert_eq!(paths, vec!["other/c.md".to_string()]);
        // FTS row for the cascaded body is also gone.
        let hits = s.search("alpha", 5, true).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn ensure_schema_is_idempotent() {
        // Re-opening an existing on-disk DB must be a no-op once schema
        // is at SCHEMA_VERSION; ensure_schema reads user_version and
        // returns early instead of re-running migrations.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        {
            let mut s = BM25Store::open_for_test(path).unwrap();
            s.upsert("a.md", 1, 1, "x", None).unwrap();
        }
        // Second open must succeed and preserve data.
        let s = BM25Store::open_for_test(path).unwrap();
        assert_eq!(s.count().unwrap(), 1);
    }

    #[test]
    fn ensure_schema_rejects_newer_db() {
        // Simulate a DB written by a future binary (user_version > SCHEMA_VERSION).
        // ensure_schema must refuse to operate rather than risk corrupting
        // rows it doesn't understand.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        {
            let conn = Connection::open(path).unwrap();
            conn.execute_batch("PRAGMA user_version = 999;").unwrap();
        }
        // BM25Store doesn't impl Debug (Connection isn't Debug), so we
        // collect the error message by hand for the assertion.
        let err_msg = match BM25Store::open_for_test(path) {
            Ok(_) => "Ok(BM25Store)".to_string(),
            Err(e) => format!("Err({e})"),
        };
        assert!(
            err_msg.contains("downgrade"),
            "expected downgrade-refusal error, got: {err_msg}"
        );
    }

    #[test]
    fn upsert_replaces_fts_row_atomically() {
        // Regression: pre-fix the files / files_fts updates ran outside
        // a transaction. A crash between the two left files with the
        // new mtime but no FTS row (or vice versa). With the transaction
        // wrap, a successful upsert always has both, and a successful
        // remove always has neither.
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("doc.md", 1, 5, "alpha", None).unwrap();
        // Re-upsert with new body; FTS row should match the new body.
        s.upsert("doc.md", 2, 5, "omega", None).unwrap();
        let hits = s.search("omega", 5, true).unwrap();
        assert_eq!(hits.len(), 1);
        let hits = s.search("alpha", 5, true).unwrap();
        assert!(hits.is_empty(), "old FTS body should be gone");
    }

    #[test]
    fn time_decay_function() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Recent file (0 days old) → decay ≈ 1.0
        let recent = super::time_decay(now_ms, 0.01);
        assert!(recent > 0.99);

        // 7 days old → exp(-0.01 * 7) ≈ 0.93
        let week_old = super::time_decay(now_ms - 7 * 86_400_000, 0.01);
        assert!((week_old - 0.932).abs() < 0.01);

        // 69 days old (half-life) → exp(-0.01 * 69) ≈ 0.50
        let half_life = super::time_decay(now_ms - 69 * 86_400_000, 0.01);
        assert!((half_life - 0.50).abs() < 0.02);

        // 365 days old → exp(-0.01 * 365) ≈ 0.026
        let year_old = super::time_decay(now_ms - 365 * 86_400_000, 0.01);
        assert!(year_old < 0.03);

        // Lambda = 0 → no decay, always 1.0
        let no_decay = super::time_decay(now_ms - 1000 * 86_400_000, 0.0);
        assert_eq!(no_decay, 1.0);
    }

    #[test]
    fn search_ranks_recent_higher() {
        // Two files with the same content but different mtimes.
        // The more recent one should rank higher (identical bm25 relevance,
        // so the recency boost is the only thing that can separate them).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        // Old file: mtime 100 days ago
        s.upsert(
            "old.md",
            now_ms - 100 * 86_400_000,
            20,
            "rust ownership rules",
            None,
        )
        .unwrap();
        // New file: mtime just now
        s.upsert("new.md", now_ms, 20, "rust ownership rules", None)
            .unwrap();

        let hits = s.search("rust", 5, true).unwrap();
        assert_eq!(hits.len(), 2);
        // The new file should rank higher (same relevance, larger decay boost)
        assert_eq!(hits[0].path, "new.md");
        assert_eq!(hits[1].path, "old.md");
    }

    /// Corpus for the relevance-ordering tests below.
    ///
    /// Four unrelated documents keep `walrus` a discriminative term: with
    /// only the two matching rows in the table its IDF goes negative and
    /// every bm25 value collapses towards zero, which hides ordering bugs.
    /// `lambda = alpha = 0` disables the recency boost and every row shares
    /// one mtime, so whatever order comes out is produced by relevance
    /// alone.
    fn relevance_corpus() -> BM25Store {
        let mut s = BM25Store::open_in_memory_with(0.0, 0.0, true).unwrap();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // Same term six times in a short document: the strongest match.
        s.upsert(
            "strong.md",
            now_ms,
            20,
            "walrus walrus walrus walrus walrus walrus",
            None,
        )
        .unwrap();
        // Same term once in a long document: a real but much weaker match.
        s.upsert(
            "weak.md",
            now_ms,
            20,
            &format!("walrus {}", "plain filler words ".repeat(40)),
            None,
        )
        .unwrap();
        for (i, body) in [
            "notes about the compiler and its borrow checker",
            "a recipe for tomato soup with basil",
            "meeting notes on the quarterly roadmap",
            "an unrelated essay on medieval bookbinding",
        ]
        .iter()
        .enumerate()
        {
            s.upsert(&format!("filler{i}.md"), now_ms, 20, body, None)
                .unwrap();
        }
        s
    }

    #[test]
    fn search_ranks_the_stronger_bm25_match_first() {
        // Regression: the scorer used to add the recency boost to the raw
        // (negative-is-better) `bm25()` value and sort descending, so the
        // weakest matched row was reported first. `memory_search`,
        // `memory_about` and the OpenClaw auto-recall hook all present
        // results in the order they arrive, and `search_hybrid_inner`
        // derives its RRF weight from that position.
        let s = relevance_corpus();

        let hits = s.search("walrus", 5, true).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["strong.md", "weak.md"],
            "the document that repeats the query term must outrank the one \
             that mentions it once in passing; got {paths:?} with scores {:?}",
            hits.iter().map(|h| h.score).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_reports_one_score_convention_on_every_path() {
        // `search_like` (any term < 3 chars) and `search_vec` both report
        // "higher is better". The BM25 path must not hand the same
        // `SearchHit::score` field the opposite convention, or a client
        // cannot order hits at all — the auto-recall hook fuses results from
        // several queries by rank. The two keyword paths are also
        // non-negative, which `search_vec`'s cosine is not.
        let s = relevance_corpus();

        let bm25_hits = s.search("walrus", 5, true).unwrap();
        assert_eq!(bm25_hits.len(), 2);
        assert!(
            bm25_hits[0].score > 0.0,
            "a discriminative term has a positive IDF, so its BM25 relevance \
             must land above zero; got {}",
            bm25_hits[0].score
        );
        assert!(
            bm25_hits[0].score > bm25_hits[1].score,
            "scores must descend with relevance: {} vs {}",
            bm25_hits[0].score,
            bm25_hits[1].score
        );

        // "wa" is below the trigram tokenizer's 3-char floor, so the same
        // corpus is served by the LIKE fallback: substring frequency 6 vs 1.
        let like_hits = s.search("wa", 5, true).unwrap();
        assert_eq!(like_hits.len(), 2);
        assert_eq!(like_hits[0].path, "strong.md");
        assert!(like_hits[0].score > 0.0);
        assert!(
            like_hits[0].score > like_hits[1].score,
            "scores must descend with relevance: {} vs {}",
            like_hits[0].score,
            like_hits[1].score
        );
    }

    #[test]
    fn search_no_decay_behaves_same() {
        // With lambda=0, time decay is disabled — all files get decay=1.0.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let mut s = BM25Store::open_in_memory_with(0.0, 0.0, true).unwrap();
        s.upsert(
            "old.md",
            now_ms - 365 * 86_400_000,
            20,
            "ancient rust facts",
            None,
        )
        .unwrap();
        s.upsert("new.md", now_ms, 20, "modern python tricks", None)
            .unwrap();

        // Both searches should still work; the scores just don't discriminate by time.
        let hits1 = s.search("ancient", 5, true).unwrap();
        assert_eq!(hits1.len(), 1);
        assert_eq!(hits1[0].path, "old.md");
        let hits2 = s.search("python", 5, true).unwrap();
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].path, "new.md");
    }

    #[test]
    fn compact_marks_old_files_cold() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        // Fresh file (5 days old, access_count=0)
        s.upsert(
            "fresh.md",
            now_ms - 5 * 86_400_000,
            20,
            "recent knowledge",
            None,
        )
        .unwrap();
        // Old file (60 days old, access_count=0)
        s.upsert(
            "old.md",
            now_ms - 60 * 86_400_000,
            20,
            "ancient wisdom",
            None,
        )
        .unwrap();

        // Compact with 30-day threshold.
        let compacted = s.compact(30).unwrap();
        assert_eq!(compacted, 1); // only old.md should be compacted

        // Normal search should not see old.md (excluded by is_cold filter).
        let hits = s.search("ancient", 5, true).unwrap();
        assert!(
            hits.is_empty(),
            "cold file should not appear in normal search"
        );

        // Deep search should still find it.
        let hits = s.search("ancient", 5, false).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "old.md");

        // Fresh file should still be visible.
        let hits = s.search("recent", 5, true).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "fresh.md");
    }

    #[test]
    fn warm_cold_counts() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        s.upsert("warm.md", now_ms, 20, "warm content", None)
            .unwrap();
        s.upsert("old1.md", now_ms - 50 * 86_400_000, 20, "old1", None)
            .unwrap();
        s.upsert("old2.md", now_ms - 100 * 86_400_000, 20, "old2", None)
            .unwrap();

        let (warm, cold) = s.warm_cold_counts().unwrap();
        assert_eq!(warm, 3);
        assert_eq!(cold, 0);

        s.compact(30).unwrap();
        let (warm, cold) = s.warm_cold_counts().unwrap();
        assert_eq!(warm, 1); // warm.md
        assert_eq!(cold, 2); // old1.md + old2.md
    }

    #[test]
    fn compact_excludes_cold_from_normal_search() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        s.upsert(
            "old.md",
            now_ms - 50 * 86_400_000,
            20,
            "very old unique keyword",
            None,
        )
        .unwrap();

        // Before compact: visible in search.
        let hits = s.search("unique", 5, true).unwrap();
        assert_eq!(hits.len(), 1);

        // Compact.
        s.compact(30).unwrap();

        // After compact: not visible in normal search (cold filter).
        let hits = s.search("unique", 5, true).unwrap();
        assert!(hits.is_empty());

        // But visible in deep search.
        let hits = s.search("unique", 5, false).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "old.md");
    }

    #[test]
    fn detect_conflicts_finds_similar_files() {
        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        s.upsert("user-pref.md", 100, 50, "用户偏好 rust 系统编程", None)
            .unwrap();

        // Search for similar content (shares key terms).
        let conflicts = s.detect_conflicts("用户偏好 rust", -2.0).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].0, "user-pref.md");
    }

    #[test]
    fn detect_conflicts_no_match() {
        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        s.upsert("cooking.md", 100, 50, "如何制作意大利面", None)
            .unwrap();

        // Unrelated query should not match.
        let conflicts = s.detect_conflicts("rust ownership rules", -2.0).unwrap();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn detect_conflicts_short_cjk_uses_like_fallback() {
        // Regression: a 2-char CJK query term produces zero trigram tokens,
        // so MATCH silently returned nothing and a duplicate memory with
        // that keyword slipped past conflict detection. The LIKE fallback
        // must catch it (see review finding #1).
        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        s.upsert("dup.md", 100, 50, "花名登记为小云", None).unwrap();
        s.upsert("other.md", 100, 50, "完全无关的内容", None)
            .unwrap();
        let conflicts = s.detect_conflicts("花名", -2.0).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].0, "dup.md");
    }

    #[test]
    fn search_like_mixed_short_and_long_tokens() {
        // A single short token pulls the whole query into the LIKE path; long
        // tokens keep AND semantics (all terms must appear). This is the
        // documented recall-over-ranking tradeoff, here pinned as a test
        // (see review finding #5).
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert("both.md", 0, 0, "花名 reversible compression", None)
            .unwrap();
        s.upsert("short_only.md", 0, 0, "花名 unrelated content", None)
            .unwrap();
        s.upsert("long_only.md", 0, 0, "reversible compression no cjk", None)
            .unwrap();
        let hits = s.search("花名 reversible", 10, true).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["both.md"],
            "LIKE path AND-joins short + long tokens"
        );
    }

    #[test]
    fn search_like_or_fallback_recalls_partial_matches() {
        // Regression for issue #2040: auto-recall keyword queries like
        // "um figure runs" contain a 2-char filler token ("um") that pulls
        // the query into the LIKE path, and "um" appears in no memory —
        // the AND query returns zero rows. The OR fallback must still
        // recall the document matching the remaining tokens.
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert(
            "billing.md",
            0,
            0,
            "Billing system runs on PostgreSQL. The slow query on invoices \
             was optimized with a composite index.",
            None,
        )
        .unwrap();
        s.upsert("other.md", 0, 0, "rate limiter token bucket", None)
            .unwrap();

        let hits = s.search("um figure runs query", 10, true).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["billing.md"],
            "OR fallback must surface partial matches when AND finds none"
        );

        // Single-token miss: no OR retry possible, stays empty.
        let hits = s.search("um", 10, true).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_like_or_fallback_ranks_before_pool_truncation() {
        // Regression for review finding on #2040 fix: the OR retry has no
        // natural SQL order, so LIMIT could drop a strong multi-token match
        // while keeping weak single-token rows. With 60 weak rows (only
        // "is" matches) and one strong row inserted last, the ORDER BY
        // matched-token-count must pull the strong row into the pool.
        let mut s = BM25Store::open_in_memory().unwrap();
        for i in 0..60 {
            s.upsert(
                &format!("weak-{i:02}.md"),
                0,
                0,
                "this is filler content without the rare terms",
                None,
            )
            .unwrap();
        }
        s.upsert(
            "target.md",
            0,
            0,
            "is needle one, needle two, needle three",
            None,
        )
        .unwrap();

        // "zz" is absent everywhere → AND returns 0 → OR retry. "is" is
        // 2 chars so the query takes the LIKE path.
        let hits = s.search("is needle zz", 5, true).unwrap();
        assert_eq!(hits.len(), 5);
        assert_eq!(
            hits[0].path, "target.md",
            "strong multi-token match must survive the OR pool limit"
        );
    }

    #[test]
    fn search_like_or_fallback_snippet_anchors_on_present_token() {
        // Regression for review finding on #2040 fix: OR-fallback rows may
        // lack the globally shortest token ("um"), and the old
        // shortest-token needle degraded the snippet to the first 24 chars
        // of the body. The needle must come from a token present in the
        // row, so the auto-recall adapter (which injects only the snippet)
        // shows matched content.
        let mut s = BM25Store::open_in_memory().unwrap();
        let preamble = "unrelated preamble text that goes on and on for quite a while before anything relevant shows up. ";
        s.upsert(
            "late-match.md",
            0,
            0,
            &format!("{preamble}The Needle appears only here."),
            None,
        )
        .unwrap();

        let hits = s.search("um needle absentword", 5, true).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0].snippet.contains("«Needle»"),
            "snippet must anchor on the matched token (case-insensitive), got: {}",
            hits[0].snippet
        );
    }

    #[test]
    fn superseded_files_excluded_from_search() {
        let mut s = BM25Store::open_in_memory_with(0.01, 0.3, true).unwrap();
        s.upsert("old.md", 100, 50, "unique old content here", None)
            .unwrap();
        s.upsert("new.md", 200, 50, "unique new content here", None)
            .unwrap();

        // Both visible before superseding.
        let hits = s.search("unique", 10, true).unwrap();
        assert_eq!(hits.len(), 2);

        // Supersede old file.
        s.supersede("old.md", "new-id").unwrap();

        // Only new file visible in search.
        let hits = s.search("unique", 10, true).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "new.md");
    }

    #[test]
    fn agent_scope_isolated_only_returns_own_memories() {
        let mut s = BM25Store::open_in_memory().unwrap();
        s.upsert(
            "a/own.md",
            100,
            10,
            "agent alpha owns this note",
            Some("alpha"),
        )
        .unwrap();
        s.upsert(
            "a/shared.md",
            100,
            10,
            "agent alpha note tagged alpha",
            Some("alpha"),
        )
        .unwrap();
        s.upsert(
            "b/other.md",
            100,
            10,
            "agent beta owns this note",
            Some("beta"),
        )
        .unwrap();
        s.upsert("u/legacy.md", 100, 10, "agent alpha legacy note", None)
            .unwrap();

        // isolated:alpha sees ONLY alpha's own — not beta, not legacy.
        let hits = s
            .search_scoped("agent alpha", 10, true, Some("isolated:alpha"))
            .unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths.len(), 2, "isolated:alpha should see 2 own memories");
        assert!(paths.iter().all(|p| p.starts_with("a/")));

        // filter:alpha sees alpha's own + unscoped (NULL) — legacy included,
        // but never beta's.
        let hits = s
            .search_scoped("agent alpha", 10, true, Some("filter:alpha"))
            .unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert!(paths.contains(&"u/legacy.md"));
        assert!(!paths.iter().any(|p| p.starts_with("b/")));
    }

    #[test]
    fn agent_scope_rejects_invalid_agent_id() {
        let s = BM25Store::open_in_memory().unwrap();
        // SQL-meta chars and path separators must be refused, not silently
        // widened to shared mode.
        for bad in ["isolated:foo'bar", "filter:a;b", "isolated:with/slash"] {
            let err = s
                .search_scoped("agent alpha", 10, true, Some(bad))
                .unwrap_err();
            assert!(
                matches!(err, MemoryError::InvalidArgument(_)),
                "expected InvalidArgument for {bad:?}, got {err:?}"
            );
        }
    }
}
