//! Session and trace query methods for GenAI SQLite store.

use rusqlite::params;

use super::{GenAISqliteStore, billed_input_col};

// ─── Query result types ────────────────────────────────────────────────────────

/// Summary of a single gen_ai.session_id within a time window
#[derive(Debug, serde::Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub conversation_count: i64,
    pub first_seen_ns: i64,
    pub last_seen_ns: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub model: Option<String>,
    pub agent_name: Option<String>,
    /// Earliest user query in the window (≤ 200 chars, best-effort)
    pub first_user_query: Option<String>,
    /// Latest user query in the window (≤ 200 chars, best-effort)
    pub last_user_query: Option<String>,
}

/// Session summary for the Token Savings page
#[derive(Debug, serde::Serialize)]
pub struct SavingsSessionSummary {
    pub session_id: String,
    pub agent_name: Option<String>,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub request_count: i64,
}

/// Turn info for a tool_call_id, including which session it belongs to.
#[derive(Debug, Clone)]
pub struct ToolCallTurnInfo {
    pub turn_index: usize,
    pub session_id: String,
    /// Start of the parent LLM call. Callers that query a sub-window of the
    /// session need it to restrict turn indices to that window.
    pub start_timestamp_ns: i64,
}

/// Summary of a single conversation (user query) within a session
#[derive(Debug, serde::Serialize)]
pub struct TraceSummary {
    pub conversation_id: String,
    pub call_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
    pub model: Option<String>,
    /// The first user_query string recorded in this conversation (best-effort)
    pub user_query: Option<String>,
}

impl GenAISqliteStore {
    // ─── Query methods ───────────────────────────────────────────────────────

    /// List all sessions within a nanosecond timestamp range.
    ///
    /// With `include_auxiliary=false` (the default for /api/sessions), recap
    /// calls may still *form* a session (recap-only sessions surface with zero
    /// tokens, #7271) but do not contribute tokens or conversation counts;
    /// query previews and model/agent aggregates fall back to recap rows only
    /// when the session has no main rows. Analyzer-only (cosh-ng background,
    /// #2750) and web_search-only sessions stay hidden. Note the sibling
    /// `list_traces_by_session` uses the same flag for trace visibility only —
    /// billing there is unfiltered.
    pub fn list_sessions(
        &self,
        start_ns: i64,
        end_ns: i64,
        include_auxiliary: bool,
    ) -> Result<Vec<SessionSummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // Session-formation predicate. Recap rows are allowed to *form* a
        // session (#7271): a qwen CLI headless call whose main LLM call was
        // missed by the capture path leaves only its follow-up managed-memory
        // recap row; excluding recap entirely made such a user session
        // permanently invisible in /api/sessions. The cosh-ng background
        // analyzer is classified as its own 'analyzer' kind (#2750, #7271) so
        // analyzer-only background sessions stay hidden; web_search-only
        // sessions stay hidden too.
        let call_kind_filter = if include_auxiliary {
            ""
        } else {
            " AND (call_kind = 'main' OR call_kind IS NULL OR call_kind = 'recap')"
        };
        // Numbers stay billed on main calls only so existing sessions' token
        // totals and conversation counts are unchanged; a recap-only session
        // surfaces with zero tokens.
        let aux_guard = if include_auxiliary {
            ""
        } else {
            "CASE WHEN (call_kind = 'main' OR call_kind IS NULL) THEN "
        };
        let aux_guard_close = if include_auxiliary { "" } else { " ELSE 0 END" };
        // Text aggregates guard with a NULL else (MAX skips NULLs); recap-only
        // sessions fall back to their recap rows via COALESCE so the required
        // last_seen_ns stays non-NULL. With include_auxiliary the guard is
        // empty and COALESCE(x, x) is a no-op.
        let text_aux_guard = if include_auxiliary {
            ""
        } else {
            "CASE WHEN (call_kind = 'main' OR call_kind IS NULL) THEN "
        };
        let text_aux_end = if include_auxiliary { "" } else { " END" };
        let conv_count_expr = if include_auxiliary {
            "conversation_id".to_string()
        } else {
            "CASE WHEN (call_kind = 'main' OR call_kind IS NULL) THEN conversation_id END"
                .to_string()
        };
        // Nullable-text fallbacks must only kick in when the session has no
        // main rows at all (#7271 review): if main rows exist but carry NULL
        // model/agent_name, falling back to a recap row would change what
        // pre-#7271 queries returned.
        // "The session has a main row" must be evaluated over the whole
        // session, not over the rows that survived the time-window filter.
        // The preview subqueries below already use the session-wide check, so
        // a windowed aggregate here reported recap model/agent for a session
        // whose main rows simply fell outside the requested window.
        let has_main = if include_auxiliary {
            ""
        } else {
            "EXISTS (SELECT 1 FROM genai_events gm \
             WHERE gm.session_id = g1.session_id \
               AND gm.event_type = 'llm_call' \
               AND (gm.call_kind = 'main' OR gm.call_kind IS NULL))"
        };
        let model_expr = if include_auxiliary {
            "MAX(model)".to_string()
        } else {
            format!(
                "CASE WHEN {has_main} THEN MAX({text_aux_guard}model{text_aux_end}) ELSE MAX(model) END"
            )
        };
        let agent_expr = if include_auxiliary {
            "COALESCE(MAX(agent_name), MAX(process_name))".to_string()
        } else {
            format!(
                "CASE WHEN {has_main} \
                 THEN COALESCE(MAX({text_aux_guard}agent_name{text_aux_end}), \
                            MAX({text_aux_guard}process_name{text_aux_end})) \
                 ELSE COALESCE(MAX(agent_name), MAX(process_name)) END"
            )
        };
        // Same filter for the correlated user-query subqueries (g2-qualified).
        // Recap rows become visible when the session has no main rows, so a
        // recap-only session's preview is inspectable instead of NULL (#7271
        // review P1); mixed sessions keep main-only previews.
        let sub_call_kind_filter = if include_auxiliary {
            ""
        } else {
            " AND ((g2.call_kind = 'main' OR g2.call_kind IS NULL) OR (g2.call_kind = 'recap' \
             AND NOT EXISTS (SELECT 1 FROM genai_events gm WHERE gm.session_id = g1.session_id \
             AND gm.event_type = 'llm_call' AND (gm.call_kind = 'main' OR gm.call_kind IS NULL))))"
        };
        let sql = format!(
            concat!(
                "SELECT session_id,
                    COUNT(DISTINCT {conv_count_expr}) AS conversation_count,
                    COALESCE(MIN({text_aux_guard}start_timestamp_ns{text_aux_end}), MIN(start_timestamp_ns)) AS first_seen_ns,
                    COALESCE(MAX({text_aux_guard}start_timestamp_ns{text_aux_end}), MAX(start_timestamp_ns)) AS last_seen_ns,
                    COALESCE(SUM({aux_guard}",
                billed_input_col!(),
                "{aux_close}), 0)  AS total_input,
                    COALESCE(SUM({aux_guard}output_tokens{aux_close}), 0) AS total_output,
                    {model_expr} AS model,
                    {agent_expr} AS agent_name,
                    (SELECT substr(g2.user_query, 1, 200) FROM genai_events g2
                      WHERE g2.session_id = g1.session_id
                        AND g2.event_type = 'llm_call'
                        AND g2.user_query IS NOT NULL AND g2.user_query != ''
                        AND g2.start_timestamp_ns BETWEEN ?1 AND ?2{sub_call_kind_filter}
                      ORDER BY g2.start_timestamp_ns ASC LIMIT 1) AS first_user_query,
                    (SELECT substr(g2.user_query, 1, 200) FROM genai_events g2
                      WHERE g2.session_id = g1.session_id
                        AND g2.event_type = 'llm_call'
                        AND g2.user_query IS NOT NULL AND g2.user_query != ''
                        AND g2.start_timestamp_ns BETWEEN ?1 AND ?2{sub_call_kind_filter}
                      ORDER BY g2.start_timestamp_ns DESC LIMIT 1) AS last_user_query
             FROM genai_events g1
             WHERE event_type = 'llm_call'
               AND session_id IS NOT NULL
               AND start_timestamp_ns BETWEEN ?1 AND ?2{call_kind_filter}
             GROUP BY session_id
             ORDER BY last_seen_ns DESC"
            ),
            sub_call_kind_filter = sub_call_kind_filter,
            call_kind_filter = call_kind_filter,
            aux_guard = aux_guard,
            aux_close = aux_guard_close,
            text_aux_guard = text_aux_guard,
            text_aux_end = text_aux_end,
            model_expr = model_expr,
            agent_expr = agent_expr,
            conv_count_expr = conv_count_expr
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![start_ns, end_ns], |row| {
            Ok(SessionSummary {
                session_id: row.get(0)?,
                conversation_count: row.get(1)?,
                first_seen_ns: row.get(2)?,
                last_seen_ns: row.get(3)?,
                total_input_tokens: row.get(4)?,
                total_output_tokens: row.get(5)?,
                model: row.get(6)?,
                agent_name: row.get(7)?,
                first_user_query: row.get(8)?,
                last_user_query: row.get(9)?,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// List sessions for the Token Savings page.
    ///
    /// Independent from `list_sessions()` to avoid affecting existing functionality.
    /// Supports optional agent_name filtering directly in SQL.
    pub fn list_sessions_for_savings(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
    ) -> Result<Vec<SavingsSessionSummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let sql: &str = if agent_name.is_some() {
            concat!(
                "SELECT session_id,
                    COALESCE(MAX(agent_name),
                             MAX(process_name))       AS agent_name,
                    COALESCE(SUM(",
                billed_input_col!(),
                "), 0)   AS total_input,
                    COALESCE(SUM(output_tokens), 0)  AS total_output,
                    COUNT(*)                         AS request_count
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND session_id IS NOT NULL
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND COALESCE(agent_name, process_name) COLLATE NOCASE = ?3 COLLATE NOCASE
             GROUP BY session_id
             ORDER BY MAX(start_timestamp_ns) DESC"
            )
        } else {
            concat!(
                "SELECT session_id,
                    COALESCE(MAX(agent_name),
                             MAX(process_name))       AS agent_name,
                    COALESCE(SUM(",
                billed_input_col!(),
                "), 0)   AS total_input,
                    COALESCE(SUM(output_tokens), 0)  AS total_output,
                    COUNT(*)                         AS request_count
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND session_id IS NOT NULL
               AND start_timestamp_ns BETWEEN ?1 AND ?2
             GROUP BY session_id
             ORDER BY MAX(start_timestamp_ns) DESC"
            )
        };

        let mut stmt = conn.prepare(sql)?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<SavingsSessionSummary> {
            Ok(SavingsSessionSummary {
                session_id: row.get(0)?,
                agent_name: row.get(1)?,
                total_input_tokens: row.get(2)?,
                total_output_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        };

        let rows = if let Some(name) = agent_name {
            stmt.query_map(params![start_ns, end_ns, name], map_row)?
        } else {
            stmt.query_map(params![start_ns, end_ns], map_row)?
        };

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Query a single session's savings summary by `session_id`.
    ///
    /// Unlike `list_sessions_for_savings` which scans a time range,
    /// this targets the index on `session_id` directly — O(1) lookup.
    pub fn get_session_for_savings(
        &self,
        session_id: &str,
    ) -> Result<Option<SavingsSessionSummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let sql = concat!(
            "SELECT session_id,
                    COALESCE(MAX(agent_name),
                             MAX(process_name))       AS agent_name,
                    COALESCE(SUM(",
            billed_input_col!(),
            "), 0)   AS total_input,
                    COALESCE(SUM(output_tokens), 0)  AS total_output,
                    COUNT(*)                         AS request_count
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND session_id = ?1
             GROUP BY session_id"
        );

        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok(SavingsSessionSummary {
                session_id: row.get(0)?,
                agent_name: row.get(1)?,
                total_input_tokens: row.get(2)?,
                total_output_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })?;

        match rows.next() {
            Some(Ok(summary)) => Ok(Some(summary)),
            Some(Err(e)) => Err(Box::new(e)),
            None => Ok(None),
        }
    }

    /// Get the turn index (1-based) for each llm_call in a session.
    ///
    /// Returns a map from `call_id` to its position in the time-ordered
    /// sequence of LLM calls within the session.
    pub fn get_call_turn_indices(
        &self,
        session_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, usize>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut result = std::collections::HashMap::new();

        for sid in session_ids {
            let sql = "SELECT call_id FROM genai_events \
                       WHERE event_type = 'llm_call' AND session_id = ?1 \
                       ORDER BY start_timestamp_ns ASC";
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(params![sid], |row| {
                // `call_id` is nullable in the schema; a NULL (only producible
                // by a foreign writer) must not error the whole map.
                let call_id: Option<String> = row.get(0)?;
                Ok(call_id)
            })?;

            for (idx, row) in rows.enumerate() {
                let call_id = row?;
                // A malformed row with a NULL call_id has no key to map, so
                // skip it instead of failing the turn lookup — same guard as
                // `get_tool_call_turn_indices` below.
                let Some(call_id) = call_id else { continue };
                // 1-based turn index
                result.insert(call_id, idx + 1);
            }
        }

        Ok(result)
    }

    /// Build a mapping from `tool_call_id` to the turn index, session and
    /// start timestamp of the LLM call that issued it.
    ///
    /// Reads the `tool_call_ids` JSON array column from `genai_events` and
    /// expands it so that each individual tool_call_id maps to its parent LLM
    /// call's turn index (1-based) and session_id.
    pub fn get_tool_call_turn_indices(
        &self,
        session_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, ToolCallTurnInfo>, Box<dyn std::error::Error>>
    {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut result = std::collections::HashMap::new();

        for sid in session_ids {
            let sql = "SELECT call_id, tool_call_ids, start_timestamp_ns FROM genai_events \
                       WHERE event_type = 'llm_call' AND session_id = ?1 \
                       ORDER BY start_timestamp_ns ASC";
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(params![sid], |row| {
                // `call_id` is nullable in the schema; a NULL (only producible
                // by a foreign writer) must not error the whole map.
                let call_id: Option<String> = row.get(0)?;
                let tool_call_ids: Option<String> = row.get(1)?;
                let start_timestamp_ns: i64 = row.get(2)?;
                Ok((call_id, tool_call_ids, start_timestamp_ns))
            })?;

            for (idx, row) in rows.enumerate() {
                let (call_id, tool_call_ids_json, start_timestamp_ns) = row?;
                // A malformed row with a NULL call_id has no key to map, so
                // skip it instead of failing every session's turn lookup.
                let Some(call_id) = call_id else { continue };
                let turn = idx + 1; // 1-based
                let session_id = sid.to_string();

                // Also map the call_id itself (for backward compat with
                // stats.db that may still store call_id as tool_use_id)
                result.insert(
                    call_id.clone(),
                    ToolCallTurnInfo {
                        turn_index: turn,
                        session_id: session_id.clone(),
                        start_timestamp_ns,
                    },
                );

                // Expand each tool_call_id in the JSON array
                if let Some(json_str) = tool_call_ids_json {
                    if let Ok(ids) = serde_json::from_str::<Vec<String>>(&json_str) {
                        for tc_id in ids {
                            result.insert(
                                tc_id,
                                ToolCallTurnInfo {
                                    turn_index: turn,
                                    session_id: session_id.clone(),
                                    start_timestamp_ns,
                                },
                            );
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    /// List all conversations under a given session, with aggregated token stats.
    ///
    /// If `start_ns`/`end_ns` are provided, only conversations whose
    /// `start_timestamp_ns` falls within the range are returned.
    pub fn list_traces_by_session(
        &self,
        session_id: &str,
        start_ns: Option<i64>,
        end_ns: Option<i64>,
        include_auxiliary: bool,
    ) -> Result<Vec<TraceSummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Trace visibility mirrors the session predicate (#7271 review P1):
        // recap rows are visible when the session has no main rows, so a
        // recap-only session drill-down is inspectable instead of rendering
        // "no traces" while the ATIF event view shows content. Mixed sessions
        // keep main-only traces. `genai_events.session_id` correlates to the
        // outer driving table in both usage contexts (outer WHERE and the
        // correlated user_query subquery, whose FROM aliases the inner table
        // as g2).
        let call_kind_filter = if include_auxiliary {
            ""
        } else {
            " AND ((call_kind = 'main' OR call_kind IS NULL) OR (call_kind = 'recap' \
             AND NOT EXISTS (SELECT 1 FROM genai_events gm WHERE gm.session_id = genai_events.session_id \
             AND gm.event_type = 'llm_call' AND (gm.call_kind = 'main' OR gm.call_kind IS NULL))))"
        };

        // When both start_ns and end_ns are present, rewrite with BETWEEN
        let sql = if start_ns.is_some() && end_ns.is_some() {
            format!(
                concat!(
                    "SELECT conversation_id,
                        COUNT(*)                        AS call_count,
                        COALESCE(SUM(",
                    billed_input_col!(),
                    "), 0)  AS total_input,
                        COALESCE(SUM(output_tokens), 0) AS total_output,
                        MIN(start_timestamp_ns)         AS start_ns,
                        MAX(end_timestamp_ns)           AS end_ns,
                        MAX(model)                      AS model,
(SELECT g2.user_query FROM genai_events g2
                          WHERE g2.event_type = 'llm_call'
                            AND g2.conversation_id = genai_events.conversation_id
                            AND g2.user_query IS NOT NULL AND g2.user_query != ''
                            AND g2.start_timestamp_ns BETWEEN ?2 AND ?3
                            {call_kind_filter}
                          ORDER BY g2.start_timestamp_ns ASC
                          LIMIT 1) AS user_query
                 FROM genai_events
                 WHERE event_type = 'llm_call'
                   AND session_id = ?1
                   AND conversation_id IS NOT NULL
                   AND start_timestamp_ns BETWEEN ?2 AND ?3{call_kind_filter}
                 GROUP BY conversation_id
                 ORDER BY start_ns DESC"
                ),
                call_kind_filter = call_kind_filter
            )
        } else if start_ns.is_some() {
            format!(
                concat!(
                    "SELECT conversation_id,
                        COUNT(*)                        AS call_count,
                        COALESCE(SUM(",
                    billed_input_col!(),
                    "), 0)  AS total_input,
                        COALESCE(SUM(output_tokens), 0) AS total_output,
                        MIN(start_timestamp_ns)         AS start_ns,
                        MAX(end_timestamp_ns)           AS end_ns,
                        MAX(model)                      AS model,
(SELECT g2.user_query FROM genai_events g2
                          WHERE g2.event_type = 'llm_call'
                            AND g2.conversation_id = genai_events.conversation_id
                            AND g2.user_query IS NOT NULL AND g2.user_query != ''
                            AND g2.start_timestamp_ns >= ?2
                            {call_kind_filter}
                          ORDER BY g2.start_timestamp_ns ASC
                          LIMIT 1) AS user_query
                 FROM genai_events
                 WHERE event_type = 'llm_call'
                   AND session_id = ?1
                   AND conversation_id IS NOT NULL
                   AND start_timestamp_ns >= ?2{call_kind_filter}
                 GROUP BY conversation_id
                 ORDER BY start_ns DESC"
                ),
                call_kind_filter = call_kind_filter
            )
        } else if end_ns.is_some() {
            format!(
                concat!(
                    "SELECT conversation_id,
                        COUNT(*)                        AS call_count,
                        COALESCE(SUM(",
                    billed_input_col!(),
                    "), 0)  AS total_input,
                        COALESCE(SUM(output_tokens), 0) AS total_output,
                        MIN(start_timestamp_ns)         AS start_ns,
                        MAX(end_timestamp_ns)           AS end_ns,
                        MAX(model)                      AS model,
(SELECT g2.user_query FROM genai_events g2
                          WHERE g2.event_type = 'llm_call'
                            AND g2.conversation_id = genai_events.conversation_id
                            AND g2.user_query IS NOT NULL AND g2.user_query != ''
                            AND g2.start_timestamp_ns <= ?2
                            {call_kind_filter}
                          ORDER BY g2.start_timestamp_ns ASC
                          LIMIT 1) AS user_query
                 FROM genai_events
                 WHERE event_type = 'llm_call'
                   AND session_id = ?1
                   AND conversation_id IS NOT NULL
                   AND start_timestamp_ns <= ?2{call_kind_filter}
                 GROUP BY conversation_id
                 ORDER BY start_ns DESC"
                ),
                call_kind_filter = call_kind_filter
            )
        } else {
            format!(
                concat!(
                    "SELECT conversation_id,
                        COUNT(*)                        AS call_count,
                        COALESCE(SUM(",
                    billed_input_col!(),
                    "), 0)  AS total_input,
                        COALESCE(SUM(output_tokens), 0) AS total_output,
                        MIN(start_timestamp_ns)         AS start_ns,
                        MAX(end_timestamp_ns)           AS end_ns,
                        MAX(model)                      AS model,
(SELECT g2.user_query FROM genai_events g2
                          WHERE g2.event_type = 'llm_call'
                            AND g2.conversation_id = genai_events.conversation_id
                            AND g2.user_query IS NOT NULL AND g2.user_query != ''
                            {call_kind_filter}
                          ORDER BY g2.start_timestamp_ns ASC
                          LIMIT 1) AS user_query
                 FROM genai_events
                 WHERE event_type = 'llm_call'
                   AND session_id = ?1
                   AND conversation_id IS NOT NULL{call_kind_filter}
                 GROUP BY conversation_id
                 ORDER BY start_ns DESC"
                ),
                call_kind_filter = call_kind_filter
            )
        };

        let mut stmt = conn.prepare(&sql)?;

        // Helper to map a row to TraceSummary — avoids closure-type mismatch
        fn map_row(row: &rusqlite::Row) -> rusqlite::Result<TraceSummary> {
            let cid: String = row.get(0)?;
            Ok(TraceSummary {
                conversation_id: cid,
                call_count: row.get(1)?,
                total_input_tokens: row.get(2)?,
                total_output_tokens: row.get(3)?,
                start_ns: row.get(4)?,
                end_ns: row.get(5)?,
                model: row.get(6)?,
                user_query: row.get(7)?,
            })
        }

        let rows: Vec<TraceSummary> = match (start_ns, end_ns) {
            (Some(s), Some(e)) => stmt
                .query_map(params![session_id, s, e], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (Some(s), None) => stmt
                .query_map(params![session_id, s], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (None, Some(e)) => stmt
                .query_map(params![session_id, e], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (None, None) => stmt
                .query_map(params![session_id], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };

        Ok(rows)
    }

    /// List the distinct agent labels observed in the given time window,
    /// merging case variants of one agent into a single entry (the filter
    /// consumers pass to every other agent-scoped query, which all match
    /// case-insensitively). The reported spelling is the group's `MIN`, the
    /// same stable-spelling rule `get_agent_token_summary` uses.
    pub fn list_agent_names(
        &self,
        start_ns: i64,
        end_ns: i64,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            // Same attribution rule as every other agent-scoped view
            // (`COALESCE(agent_name, process_name)`): a call recorded without
            // an agent name would otherwise be missing from the filter list
            // while the dashboard shows its sessions under that label.
            "SELECT MIN(COALESCE(agent_name, process_name)) AS agent_name
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND COALESCE(agent_name, process_name) IS NOT NULL
               AND start_timestamp_ns BETWEEN ?1 AND ?2
             GROUP BY COALESCE(agent_name, process_name) COLLATE NOCASE
             ORDER BY agent_name ASC",
        )?;
        let rows = stmt.query_map(params![start_ns, end_ns], |row| row.get::<_, String>(0))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Look up the real session_id from completed records for the same PID.
    /// Used in drain path to reconcile the response_id-based fallback session_id
    /// (`SHA256("session" + first_response_id)`) with the real agent UUID from
    /// ResponseSessionMapper.
    pub fn lookup_session_for_pid(
        &self,
        pid: i32,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let result = conn.query_row(
            "SELECT session_id FROM genai_events
             WHERE pid = ?1 AND status = 'complete' AND session_id IS NOT NULL
             ORDER BY start_timestamp_ns DESC LIMIT 1",
            params![pid],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(sid) => Ok(Some(sid)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Box::new(e)),
        }
    }

    /// Update the session_id of a pending record after reconciliation.
    pub fn update_session_id(
        &self,
        call_id: &str,
        session_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE genai_events SET session_id = ?2 WHERE call_id = ?1",
            params![call_id, session_id],
        )?;
        Ok(())
    }
}
