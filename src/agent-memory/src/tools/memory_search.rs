use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::index::SearchHit;
use crate::service::MemoryService;

const TOOL: &str = "memory_search";

/// Tier B: search the memory store.
///
/// `mode` controls the search algorithm:
/// - `"bm25"` (default): FTS5 keyword search.
/// - `"vector"`: dense embedding cosine similarity (requires embedding config).
/// - `"hybrid"`: reciprocal rank fusion of BM25 + vector results.
///
/// `category` filters results to a specific fact category (e.g. "lesson",
/// "interest"). When empty, all categories are included.
///
/// Returns up to `top_k` ranked snippets. Errors with `NotImplemented` if the
/// index worker isn't running, or if `mode=vector|hybrid` is requested without
/// an embedding provider.
pub fn memory_search(
    svc: &MemoryService,
    query: &str,
    top_k: usize,
    mode: Option<&str>,
    category: Option<&str>,
    agent_scope: Option<&str>,
) -> Result<Vec<SearchHit>> {
    let mode = mode.unwrap_or("bm25");
    let index = match svc.index.as_ref() {
        Some(i) => i,
        None => {
            let err = MemoryError::NotImplemented(
                "index disabled; enable [memory.index].enabled or use mem_grep instead",
            );
            svc.audit_log(AuditEntry::new(TOOL).error(err.to_string()));
            return Err(err);
        }
    };

    // Determine effective agent scope from parameter or config.
    // We never silently widen the visibility domain: an explicitly configured
    // `isolated`/`filter` scope with no `MCP_CLIENT_NAME` is a misconfiguration,
    // so we warn and run the search *unscoped* (shared semantics) rather than
    // erroring on every request — the operator-facing warn surfaces the
    // misconfiguration, while still letting the agent read its shared memory.
    let config_scope_owned;
    let scope_ref = if let Some(s) = agent_scope {
        Some(s)
    } else {
        let config_scope = &svc.config.memory.agent_scope;
        if config_scope.is_empty() || config_scope == "shared" {
            None
        } else if !matches!(config_scope.as_str(), "isolated" | "filter") {
            tracing::warn!(
                "memory.agent_scope={config_scope:?} is not a recognised value \
                 (expected \"shared\", \"isolated\", or \"filter\"); \
                 falling back to shared (unscoped) search."
            );
            None
        } else if let Ok(agent_id) = std::env::var("MCP_CLIENT_NAME") {
            config_scope_owned = format!("{config_scope}:{agent_id}");
            Some(config_scope_owned.as_str())
        } else {
            tracing::warn!(
                "memory.agent_scope={config_scope:?} requires MCP_CLIENT_NAME but it is unset; \
                 falling back to shared (unscoped) search. Set MCP_CLIENT_NAME or switch \
                 agent_scope to \"shared\" to silence this warning."
            );
            None
        }
    };

    match mode {
        "bm25" => {
            let hits = index.search_scoped(query, top_k.max(1), scope_ref)?;
            let hits = filter_by_category(hits, category);
            let tokens = hits
                .iter()
                .map(|h| h.snippet.len() as u64 / 4 + h.path.len() as u64 / 4)
                .sum();
            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(format!("bm25:len={}", query.len()))
                    .bytes(hits.len() as u64)
                    .tokens(tokens),
            );
            Ok(hits)
        }
        "vector" | "hybrid" => {
            // Vector and hybrid paths do not yet apply agent_scope: `files_vec`
            // has no `agent_id` column and `search_vec` is a full-table scan.
            // Rather than silently returning unscoped results (which would
            // break the isolation contract), we degrade to a *scoped* BM25
            // search so the agent never sees another agent's memories. This
            // mirrors the "no embedding provider" fallback below.
            let emb = if scope_ref.is_some() {
                tracing::warn!(
                    "{mode} search requested with agent_scope; vector/hybrid paths do not \
                     yet enforce agent scoping — falling back to scoped BM25 to preserve \
                     the isolation boundary."
                );
                None
            } else {
                svc.embedding.as_ref()
            };
            let emb = match emb {
                Some(e) => e,
                None => {
                    // Graceful fallback: when no embedding provider is
                    // configured (or scope requires BM25), vector/hybrid
                    // silently degrades to BM25 so that callers (auto-recall,
                    // corpus supplement) can safely request hybrid without
                    // checking config first. Uses search_scoped for consistent
                    // agent scope filtering.
                    tracing::debug!(
                        "{mode} requested but no embedding provider — falling back to bm25"
                    );
                    let hits = index.search_scoped(query, top_k.max(1), scope_ref)?;
                    svc.audit_log(
                        AuditEntry::new(TOOL)
                            .path(format!("bm25(fallback from {mode}):len={}", query.len()))
                            .bytes(hits.len() as u64),
                    );
                    return Ok(hits);
                }
            };

            // Drive the async embedding call from this sync tool handler.
            // We are on a tokio worker thread (the MCP handler is async),
            // so `Handle::block_on` on the *current* runtime would panic
            // ("Cannot start a runtime from within a runtime").
            // `block_in_place` moves this thread out of the scheduler,
            // making a nested `Handle::current().block_on` legal. Requires
            // a multi-thread runtime (the server uses one; tests covering
            // this path must use `#[tokio::test(flavor = "multi_thread")]`).
            // Outside any runtime, fall back to a clean NotImplemented.
            let embedding = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::try_current()
                    .map_err(|_| {
                        MemoryError::NotImplemented(
                            "embedding requires a tokio runtime; tests should use #[tokio::test]",
                        )
                    })?
                    .block_on(emb.embed(query))
            })
            .map_err(|e| {
                svc.audit_log(
                    AuditEntry::new(TOOL)
                        .path(format!("embed:len={}", query.len()))
                        .error(e.to_string()),
                );
                MemoryError::Other(format!("embedding failed: {e}"))
            })?;

            let hits = if mode == "vector" {
                index.search_vec(&embedding.vector, top_k.max(1))?
            } else {
                index.search_hybrid(query, &embedding.vector, top_k.max(1))?
            };
            let hits = filter_by_category(hits, category);
            let tokens = hits
                .iter()
                .map(|h| h.snippet.len() as u64 / 4 + h.path.len() as u64 / 4)
                .sum();

            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(format!("{mode}:len={}", query.len()))
                    .bytes(hits.len() as u64)
                    .tokens(tokens),
            );
            Ok(hits)
        }
        unknown => Err(MemoryError::InvalidArgument(format!(
            "unknown search mode '{unknown}'; expected bm25, vector, or hybrid"
        ))),
    }
}

/// Filter search hits by category, based on the path prefix.
/// Facts are stored under facts/<category>/<ulid>.md.
fn filter_by_category(
    hits: Vec<crate::index::SearchHit>,
    category: Option<&str>,
) -> Vec<crate::index::SearchHit> {
    let Some(cat) = category else { return hits };
    hits.into_iter()
        .filter(|h| {
            // Path like "facts/lesson/ulid.md" → category is the second component.
            h.path.split('/').nth(1) == Some(cat)
        })
        .collect()
}
