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
    pub fn list_sessions(
        &self,
        start_ns: i64,
        end_ns: i64,
        include_auxiliary: bool,
    ) -> Result<Vec<SessionSummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let call_kind_filter = if include_auxiliary {
            ""
        } else {
            " AND (call_kind = 'main' OR call_kind IS NULL)"
        };
        // Same filter for the correlated user-query subqueries (g2-qualified).
        let sub_call_kind_filter = if include_auxiliary {
            ""
        } else {
            " AND (g2.call_kind = 'main' OR g2.call_kind IS NULL)"
        };
        let sql = format!(
            concat!(
                "SELECT session_id,
                    COUNT(DISTINCT conversation_id) AS conversation_count,
                    MIN(start_timestamp_ns)  AS first_seen_ns,
                    MAX(start_timestamp_ns)  AS last_seen_ns,
                    COALESCE(SUM(",
                billed_input_col!(),
                "), 0)  AS total_input,
                    COALESCE(SUM(output_tokens), 0) AS total_output,
                    MAX(model)               AS model,
                    MAX(agent_name)          AS agent_name,
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
            call_kind_filter = call_kind_filter
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
                    MAX(agent_name)                  AS agent_name,
                    COALESCE(SUM(",
                billed_input_col!(),
                "), 0)   AS total_input,
                    COALESCE(SUM(output_tokens), 0)  AS total_output,
                    COUNT(*)                         AS request_count
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND session_id IS NOT NULL
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND agent_name = ?3
             GROUP BY session_id
             ORDER BY MAX(start_timestamp_ns) DESC"
            )
        } else {
            concat!(
                "SELECT session_id,
                    MAX(agent_name)                  AS agent_name,
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
                    MAX(agent_name)                  AS agent_name,
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
                let call_id: String = row.get(0)?;
                Ok(call_id)
            })?;

            for (idx, row) in rows.enumerate() {
                let call_id: String = row?;
                // 1-based turn index
                result.insert(call_id, idx + 1);
            }
        }

        Ok(result)
    }

    /// Build a mapping from `tool_call_id` to the turn index and session of
    /// the LLM call that issued it.
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
            let sql = "SELECT call_id, tool_call_ids FROM genai_events \
                       WHERE event_type = 'llm_call' AND session_id = ?1 \
                       ORDER BY start_timestamp_ns ASC";
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(params![sid], |row| {
                let call_id: String = row.get(0)?;
                let tool_call_ids: Option<String> = row.get(1)?;
                Ok((call_id, tool_call_ids))
            })?;

            for (idx, row) in rows.enumerate() {
                let (call_id, tool_call_ids_json) = row?;
                let turn = idx + 1; // 1-based
                let session_id = sid.to_string();

                // Also map the call_id itself (for backward compat with
                // stats.db that may still store call_id as tool_use_id)
                result.insert(
                    call_id.clone(),
                    ToolCallTurnInfo {
                        turn_index: turn,
                        session_id: session_id.clone(),
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

        let call_kind_filter = if include_auxiliary {
            ""
        } else {
            " AND (call_kind = 'main' OR call_kind IS NULL)"
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
                        MIN(user_query)                 AS user_query
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
                        MIN(user_query)                 AS user_query
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
                        MIN(user_query)                 AS user_query
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
                        MIN(user_query)                 AS user_query
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

    /// List all distinct agent_name values observed in the given time window.
    pub fn list_agent_names(
        &self,
        start_ns: i64,
        end_ns: i64,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT DISTINCT agent_name
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND agent_name IS NOT NULL
               AND start_timestamp_ns BETWEEN ?1 AND ?2
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
