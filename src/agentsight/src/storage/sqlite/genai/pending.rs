//! Pending-call lifecycle methods for GenAI SQLite store.

use rusqlite::{TransactionBehavior, params};

use super::GenAISqliteStore;
use crate::genai::semantic::GenAISemanticEvent;

// ─── Query result types ────────────────────────────────────────────────────────

/// Source that created a pending GenAI row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PendingOrigin {
    /// Request was captured before the response arrived.
    #[default]
    RequestCapture,
    /// In-flight request was drained after its owner PID disappeared.
    DeadPidDrain,
    /// In-flight stream was snapshotted after an idle timeout while the PID lived.
    IdleDrain,
}

impl PendingOrigin {
    /// Stable string stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequestCapture => "request_capture",
            Self::DeadPidDrain => "dead_pid_drain",
            Self::IdleDrain => "idle_drain",
        }
    }
}

/// Lightweight info needed to write a PENDING record when a request is first seen
pub struct PendingCallInfo {
    /// Unique call ID (same one that will be used in the complete record)
    pub call_id: String,
    /// Trace ID (LLM API response_id, e.g. chatcmpl-xxx)
    pub trace_id: Option<String>,
    /// Conversation ID (user query fingerprint)
    pub conversation_id: Option<String>,
    /// Session ID
    pub session_id: Option<String>,
    /// Request start timestamp (nanoseconds)
    pub start_timestamp_ns: u64,
    /// Process ID
    pub pid: i32,
    /// Process name / comm
    pub process_name: String,
    /// Resolved agent name
    pub agent_name: Option<String>,
    /// HTTP method
    pub http_method: Option<String>,
    /// HTTP path
    pub http_path: Option<String>,
    /// Serialised input messages (JSON)
    pub input_messages: Option<String>,
    /// Serialised system instructions (JSON)
    pub system_instructions: Option<String>,
    /// User query extracted from request
    pub user_query: Option<String>,
    /// Whether this is an SSE streaming request
    pub is_sse: bool,
    /// Model name (extracted from request body)
    pub model: Option<String>,
    /// Provider name (extracted from request path)
    pub provider: Option<String>,
    /// Call kind classification: "main" | "recap" | "web_search"
    pub call_kind: String,
    /// Source that created this pending record.
    pub pending_origin: PendingOrigin,
    /// Deterministic key used to reconcile idle snapshots with resumed streams.
    pub pending_match_key: Option<String>,
}

/// Data extracted from captured SSE events for enriching a pending record.
pub struct SseEnrichment {
    pub model: Option<String>,
    pub trace_id: Option<String>,
    pub provider: Option<String>,
    pub output_messages: Option<String>,
    pub sse_event_count: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

impl GenAISqliteStore {
    // ─── Pending-call lifecycle methods ────────────────────────────────────────

    /// Insert a PENDING record as soon as a request is captured.
    ///
    /// The record is later promoted to 'complete' via [`complete_pending`] once
    /// the full response arrives, or marked 'interrupted' by the stale-scan thread
    /// if the agent crashes before the response is received. A request capture adopts
    /// a unique unfinished idle snapshot with the same match key; replaying an
    /// existing call ID leaves its row unchanged.
    pub fn insert_pending(&self, info: &PendingCallInfo) -> Result<(), Box<dyn std::error::Error>> {
        // Cold instance-ID resolution may perform I/O; keep it outside the writer lock.
        let instance = crate::genai::instance_id::get_instance_id();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| format!("GenAI pending insert mutex poisoned: {e}"))?;
        // Reserve the writer before checking, including across store connections.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM genai_events
             WHERE event_type = 'llm_call' AND call_id = ?1)",
            params![info.call_id],
            |row| row.get(0),
        )?;
        if exists {
            tx.commit()?;
            return Ok(());
        }

        if info.pending_origin == PendingOrigin::RequestCapture {
            if let Some(match_key) = info.pending_match_key.as_deref() {
                let candidates = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM genai_events
                         WHERE event_type = 'llm_call'
                           AND status IN ('pending', 'interrupted')
                           AND pending_origin = 'idle_drain'
                           AND pending_match_key = ?1
                         LIMIT 2",
                    )?;
                    stmt.query_map(params![match_key], |row| row.get::<_, i64>(0))?
                        .collect::<Result<Vec<_>, _>>()?
                };
                if let [id] = candidates.as_slice() {
                    // Keep request evidence and row identity; RequestCapture also
                    // keeps deferred calls visible to crash recovery.
                    tx.execute(
                        "UPDATE genai_events SET
                            call_id = ?1, trace_id = ?2, conversation_id = ?3,
                            session_id = ?4, pending_origin = 'request_capture'
                         WHERE id = ?5",
                        params![
                            info.call_id,
                            info.trace_id,
                            info.conversation_id,
                            info.session_id,
                            id,
                        ],
                    )?;
                    tx.commit()?;
                    return Ok(());
                }
                if candidates.len() > 1 {
                    log::warn!(
                        "Ambiguous idle snapshots for pending call {}; preserving snapshots",
                        info.call_id
                    );
                }
            }
        }
        tx.execute(
            "INSERT INTO genai_events (
                event_type, status, call_id, trace_id, conversation_id, session_id, instance,
                start_timestamp_ns, pid, process_name, agent_name,
                http_method, http_path, is_sse,
                input_messages, system_instructions, user_query,
                model, provider, call_kind, pending_origin, pending_match_key,
                event_json
            ) VALUES (
                'llm_call', 'pending', ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?12,
                ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20,
                '{}'
            )",
            params![
                info.call_id,
                info.trace_id,
                info.conversation_id,
                info.session_id,
                instance,
                info.start_timestamp_ns as i64,
                info.pid,
                info.process_name,
                info.agent_name,
                info.http_method,
                info.http_path,
                if info.is_sse { 1i64 } else { 0 },
                info.input_messages,
                info.system_instructions,
                info.user_query,
                info.model,
                info.provider,
                info.call_kind,
                info.pending_origin.as_str(),
                info.pending_match_key,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Promote an existing PENDING record to 'complete' by updating all response fields.
    ///
    /// If no matching PENDING row exists (e.g. because the DB was restarted), the
    /// call falls back to a plain INSERT so data is never silently dropped.
    pub fn complete_pending(
        &self,
        event: &GenAISemanticEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let GenAISemanticEvent::LLMCall(call) = event {
            {
                let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
                let event_json = serde_json::to_string(event)?;

                let (input_tokens, output_tokens, total_tokens) = call
                    .token_usage
                    .as_ref()
                    .map(|u| {
                        (
                            u.input_tokens as i64,
                            u.output_tokens as i64,
                            u.total_tokens as i64,
                        )
                    })
                    .unwrap_or((0, 0, 0));
                let cache_creation = call
                    .token_usage
                    .as_ref()
                    .and_then(|u| u.cache_creation_input_tokens.map(|v| v as i64));
                let cache_read = call
                    .token_usage
                    .as_ref()
                    .and_then(|u| u.cache_read_input_tokens.map(|v| v as i64));

                let system_instructions: Option<String> = {
                    let sys: Vec<_> = call
                        .request
                        .messages
                        .iter()
                        .filter(|m| m.role == "system")
                        .collect();
                    if sys.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&sys).ok()
                    }
                };
                let input_messages: Option<String> = {
                    let latest =
                        crate::genai::semantic::latest_round_input_messages(&call.request.messages);
                    if latest.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&latest).ok()
                    }
                };
                let output_messages: Option<String> = if call.response.messages.is_empty() {
                    None
                } else {
                    serde_json::to_string(&call.response.messages).ok()
                };
                let finish_reasons: Option<String> = {
                    let reasons: Vec<_> = call
                        .response
                        .messages
                        .iter()
                        .filter_map(|m| m.finish_reason.as_deref())
                        .collect();
                    if reasons.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&reasons).ok()
                    }
                };

                let tool_call_ids: Option<String> = {
                    let ids: Vec<String> = call
                        .response
                        .messages
                        .iter()
                        .flat_map(|m| m.parts.iter())
                        .filter_map(|p| {
                            if let crate::genai::semantic::MessagePart::ToolCall {
                                id: Some(tc_id),
                                ..
                            } = p
                            {
                                Some(tc_id.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if ids.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&ids).ok()
                    }
                };

                let updated = conn.execute(
                    "UPDATE genai_events SET
                        status = 'complete',
                        trace_id            = ?1,
                        conversation_id     = ?2,
                        session_id          = ?3,
                        end_timestamp_ns    = ?4,
                        duration_ns         = ?5,
                        provider            = ?6,
                        model               = ?7,
                        request_model       = ?8,
                        response_model      = ?9,
                        temperature         = ?10,
                        max_tokens          = ?11,
                        top_p               = ?12,
                        frequency_penalty   = ?13,
                        presence_penalty    = ?14,
                        finish_reasons      = ?15,
                        server_address      = ?16,
                        input_tokens        = ?17,
                        output_tokens       = ?18,
                        total_tokens        = ?19,
                        cache_creation_tokens = ?20,
                        cache_read_tokens   = ?21,
                        system_instructions = ?22,
                        input_messages      = ?23,
                        output_messages     = ?24,
                        status_code         = ?25,
                        sse_event_count     = ?26,
                        event_json          = ?27,
                        tool_call_ids       = ?28,
                        call_kind           = ?29,
                        first_output_timestamp_ns = ?30,
                        is_sse = COALESCE(?32, is_sse)
                    WHERE call_id = ?31 AND status IN ('pending', 'interrupted')",
                    params![
                        call.metadata.get("response_id"),
                        call.metadata.get("conversation_id"),
                        call.metadata.get("session_id"),
                        call.end_timestamp_ns as i64,
                        call.duration_ns as i64,
                        call.provider.as_str(),
                        call.model.as_str(),
                        call.model.as_str(),
                        call.model.as_str(),
                        call.request.temperature,
                        call.request.max_tokens.map(|v| v as i64),
                        call.request.top_p,
                        call.request.frequency_penalty,
                        call.request.presence_penalty,
                        finish_reasons.as_deref(),
                        call.metadata.get("server.address"),
                        input_tokens,
                        output_tokens,
                        total_tokens,
                        cache_creation,
                        cache_read,
                        system_instructions.as_deref(),
                        input_messages.as_deref(),
                        output_messages.as_deref(),
                        call.metadata
                            .get("status_code")
                            .and_then(|s| s.parse::<i64>().ok()),
                        call.metadata
                            .get("sse_event_count")
                            .and_then(|s| s.parse::<i64>().ok()),
                        event_json.as_str(),
                        tool_call_ids.as_deref(),
                        call.metadata
                            .get("call_kind")
                            .map(|s| s.as_str())
                            .unwrap_or("main"),
                        call.metadata
                            .get("first_output_timestamp_ns")
                            .and_then(|value| value.parse::<i64>().ok()),
                        call.call_id.as_str(),
                        // Backfill from the observed value so protocols whose
                        // streaming switch lives in request headers (not the
                        // body "stream" field) are recorded correctly (#3129).
                        call.metadata
                            .get("is_sse")
                            .map(|s| if s == "true" { 1i64 } else { 0 }),
                    ],
                )?;

                if updated > 0 {
                    log::debug!(
                        "[GenAI] Promoted pending/interrupted→complete for call_id={}",
                        call.call_id
                    );
                    return Ok(());
                }
                // Row exists with status='complete' — already done, skip.
                let exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM genai_events WHERE call_id = ?1)",
                    params![call.call_id],
                    |row| row.get(0),
                )?;
                if exists {
                    log::debug!(
                        "[GenAI] Row already exists for call_id={} (complete), skipping",
                        call.call_id
                    );
                    return Ok(());
                }
                log::debug!(
                    "[GenAI] No row for call_id={}, inserting directly",
                    call.call_id
                );

                if let Some(match_key) = call.metadata.get("pending_match_key") {
                    let updated = conn.execute(
                        "UPDATE genai_events SET
                            status = 'complete',
                            trace_id            = ?1,
                            conversation_id     = ?2,
                            session_id          = ?3,
                            end_timestamp_ns    = ?4,
                            duration_ns         = ?5,
                            provider            = ?6,
                            model               = ?7,
                            request_model       = ?8,
                            response_model      = ?9,
                            temperature         = ?10,
                            max_tokens          = ?11,
                            top_p               = ?12,
                            frequency_penalty   = ?13,
                            presence_penalty    = ?14,
                            finish_reasons      = ?15,
                            server_address      = ?16,
                            input_tokens        = ?17,
                            output_tokens       = ?18,
                            total_tokens        = ?19,
                            cache_creation_tokens = ?20,
                            cache_read_tokens   = ?21,
                            system_instructions = ?22,
                            input_messages      = ?23,
                            output_messages     = ?24,
                            status_code         = ?25,
                            sse_event_count     = ?26,
                            event_json          = ?27,
                            tool_call_ids       = ?28,
                            call_kind           = ?29,
                            first_output_timestamp_ns = ?30,
                            call_id             = ?31,
                            is_sse              = COALESCE(?33, is_sse)
                         WHERE id = (
                            SELECT id FROM genai_events
                            WHERE event_type = 'llm_call'
                              AND status IN ('pending', 'interrupted')
                              AND pending_origin = 'idle_drain'
                              AND pending_match_key = ?32
                            ORDER BY start_timestamp_ns DESC
                            LIMIT 1
                         )",
                        params![
                            call.metadata.get("response_id"),
                            call.metadata.get("conversation_id"),
                            call.metadata.get("session_id"),
                            call.end_timestamp_ns as i64,
                            call.duration_ns as i64,
                            call.provider.as_str(),
                            call.model.as_str(),
                            call.model.as_str(),
                            call.model.as_str(),
                            call.request.temperature,
                            call.request.max_tokens.map(|v| v as i64),
                            call.request.top_p,
                            call.request.frequency_penalty,
                            call.request.presence_penalty,
                            finish_reasons.as_deref(),
                            call.metadata.get("server.address"),
                            input_tokens,
                            output_tokens,
                            total_tokens,
                            cache_creation,
                            cache_read,
                            system_instructions.as_deref(),
                            input_messages.as_deref(),
                            output_messages.as_deref(),
                            call.metadata
                                .get("status_code")
                                .and_then(|s| s.parse::<i64>().ok()),
                            call.metadata
                                .get("sse_event_count")
                                .and_then(|s| s.parse::<i64>().ok()),
                            event_json.as_str(),
                            tool_call_ids.as_deref(),
                            call.metadata
                                .get("call_kind")
                                .map(|s| s.as_str())
                                .unwrap_or("main"),
                            call.metadata
                                .get("first_output_timestamp_ns")
                                .and_then(|value| value.parse::<i64>().ok()),
                            call.call_id.as_str(),
                            match_key.as_str(),
                            call.metadata
                                .get("is_sse")
                                .map(|s| if s == "true" { 1i64 } else { 0 }),
                        ],
                    )?;

                    if updated > 0 {
                        log::debug!(
                            "[GenAI] Promoted idle pending snapshot→complete for call_id={}",
                            call.call_id
                        );
                        return Ok(());
                    }
                }
            }
            // Fallback: store_event handles the full INSERT path
            self.store_event(event)
        } else {
            // Non-LLMCall events have no pending lifecycle, store directly
            self.store_event(event)
        }
    }

    /// Mark stale PENDING records as 'interrupted'.
    ///
    /// Called by the background scanner.  Any `llm_call` row that has been in
    /// 'pending' state for longer than `timeout_secs` is assumed to have been
    /// lost (agent crash / network cut) and is updated to 'interrupted'.
    ///
    /// Returns the number of rows updated.
    pub fn mark_interrupted_stale(
        &self,
        timeout_secs: u64,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let cutoff_ns = {
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            now_ns - (timeout_secs as i64 * 1_000_000_000)
        };
        let updated = conn.execute(
            "UPDATE genai_events
             SET status = 'interrupted'
             WHERE event_type = 'llm_call'
               AND status = 'pending'
               AND start_timestamp_ns < ?1",
            params![cutoff_ns],
        )?;
        if updated > 0 {
            log::info!("[GenAI] Marked {updated} stale pending call(s) as interrupted");
        }
        Ok(updated)
    }

    /// Set the interruption_type for a specific call_id.
    ///
    /// Called by the online InterruptionDetector after detecting an anomaly.
    pub fn update_interruption_type(
        &self,
        call_id: &str,
        itype: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE genai_events SET interruption_type = ?1 WHERE call_id = ?2",
            params![itype, call_id],
        )?;
        Ok(())
    }

    pub fn count_interruption_type_for_conversation(
        &self,
        conversation_id: &str,
        itype: &str,
    ) -> u32 {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM genai_events WHERE conversation_id = ?1 AND interruption_type = ?2",
            params![conversation_id, itype],
            |row| row.get(0),
        )
        .unwrap_or(0)
    }

    /// Fetch the most recent N LLM calls for a conversation (for loop detection).
    ///
    /// Returns lightweight summaries ordered oldest-first (ascending timestamp).
    /// Used by LoopDetector to analyze repetitive patterns across calls.
    pub fn get_recent_calls_for_conversation(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Vec<crate::interruption::RecentCallSummary> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // Subquery fetches latest N rows desc, outer query reverses to asc order
        let sql = "SELECT call_id, output_messages, COALESCE(input_tokens, 0), COALESCE(output_tokens, 0) \
                   FROM (SELECT call_id, output_messages, input_tokens, output_tokens, start_timestamp_ns \
                         FROM genai_events \
                         WHERE event_type = 'llm_call' \
                           AND conversation_id = ?1 \
                           AND status != 'pending' \
                         ORDER BY start_timestamp_ns DESC \
                         LIMIT ?2) \
                   ORDER BY start_timestamp_ns ASC";
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match stmt.query_map(params![conversation_id, limit as i64], |row| {
            let call_id: String = row.get(0)?;
            let output_messages_json: Option<String> = row.get(1)?;
            let input_tokens: i64 = row.get(2)?;
            let output_tokens: i64 = row.get(3)?;
            Ok((call_id, output_messages_json, input_tokens, output_tokens))
        }) {
            Ok(r) => r,
            Err(_) => return vec![],
        };

        rows.filter_map(|r| r.ok())
            .map(|(call_id, output_json, input_tokens, output_tokens)| {
                let (tool_calls, output_text_snippet) =
                    parse_output_messages_for_loop_detection(output_json.as_deref());
                crate::interruption::RecentCallSummary {
                    call_id,
                    tool_calls,
                    output_text_snippet,
                    input_tokens,
                    output_tokens,
                }
            })
            .collect()
    }

    /// List all pending calls for a specific PID.
    ///
    /// Returns (call_id, session_id, trace_id, conversation_id) tuples for all
    /// PENDING records matching the given PID. Used by HealthChecker to link
    /// agent_crash events to their associated LLM calls.
    pub fn list_pending_for_pid(
        &self,
        pid: i32,
    ) -> Result<
        Vec<(String, Option<String>, Option<String>, Option<String>)>,
        Box<dyn std::error::Error>,
    > {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT call_id, session_id, trace_id, conversation_id
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND status = 'pending'
               AND pid = ?1
               AND pending_origin != 'idle_drain'",
        )?;
        let rows = stmt
            .query_map(params![pid], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Mark all pending calls for a PID as interrupted.
    ///
    /// Called by HealthChecker when it detects an agent process has gone offline.
    /// Sets status='interrupted' and interruption_type to the provided value.
    pub fn mark_pending_interrupted_for_pid(
        &self,
        pid: i32,
        itype: &str,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let updated = conn.execute(
            "UPDATE genai_events
             SET status = 'interrupted', interruption_type = ?1
             WHERE event_type = 'llm_call'
               AND status = 'pending'
               AND pid = ?2
               AND pending_origin != 'idle_drain'",
            params![itype, pid],
        )?;
        if updated > 0 {
            log::info!("Marked {updated} pending call(s) as interrupted for pid={pid}");
        }
        Ok(updated)
    }

    /// Like `list_pending_for_pid` but accepts multiple PIDs at once.
    pub fn list_pending_for_pids(
        &self,
        pids: &[i32],
    ) -> Result<
        Vec<(String, Option<String>, Option<String>, Option<String>)>,
        Box<dyn std::error::Error>,
    > {
        if pids.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let placeholders: String = pids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT call_id, session_id, trace_id, conversation_id
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND status = 'pending'
               AND pending_origin != 'idle_drain'
               AND pid IN ({placeholders})"
        );
        let params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = pids
            .iter()
            .map(|p| Box::new(*p) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Enrich a pending record with data extracted from captured SSE events.
    /// Updates model, trace_id, provider, output_messages, sse_event_count, and token counts.
    pub fn enrich_pending_from_sse(
        &self,
        call_id: &str,
        enrichment: &SseEnrichment,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE genai_events SET
                model            = COALESCE(?2, model),
                trace_id         = COALESCE(?3, trace_id),
                provider         = COALESCE(?4, provider),
                output_messages  = COALESCE(?5, output_messages),
                sse_event_count  = COALESCE(?6, sse_event_count),
                input_tokens     = COALESCE(?7, input_tokens),
                output_tokens    = COALESCE(?8, output_tokens),
                total_tokens     = COALESCE(?7, input_tokens, 0)
                                 + COALESCE(?8, output_tokens, 0)
             WHERE call_id = ?1",
            params![
                call_id,
                enrichment.model,
                enrichment.trace_id,
                enrichment.provider,
                enrichment.output_messages,
                enrichment.sse_event_count,
                enrichment.input_tokens,
                enrichment.output_tokens,
            ],
        )?;
        Ok(())
    }

    /// Retroactively replace a fallback session_id after the real session
    /// UUID became known via a late FileWrite mapping (issue #2059).
    ///
    /// Only rows still carrying the 32-char lowercase-hex fallback shape are
    /// touched — both `crash_fallback_id` and `domain_hash` (id_resolver.rs)
    /// emit `format!("{digest:x}")`, i.e. lowercase hex, truncated to 32.
    /// Anything else — 36-char mapper or metadata UUIDs, or a future
    /// non-UUID "real" session id — must not be overwritten by a later,
    /// possibly different, pid mapping.
    ///
    /// Returns the number of rows updated (0 or 1 — call_id is unique).
    ///
    /// Named distinctly from `update_session_id` (session.rs), which is the
    /// unguarded variant used by the drain-path reconciliation.
    ///
    /// # Errors
    ///
    /// Returns an error when the SQLite connection lock cannot be acquired
    /// or the UPDATE statement fails to execute.
    pub fn update_fallback_session_id(
        &self,
        call_id: &str,
        new_session_id: &str,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // GLOB is case-sensitive (unlike LIKE), so uppercase hex fails the
        // guard by design; a NULL session_id yields NULL and never matches.
        let updated = conn.execute(
            "UPDATE genai_events
             SET session_id = ?1
             WHERE call_id = ?2
               AND session_id != ?1
               AND length(session_id) = 32
               AND session_id NOT GLOB '*[^0-9a-f]*'",
            params![new_session_id, call_id],
        )?;
        Ok(updated)
    }
}

// ─── Helper for loop detection ───────────────────────────────────────────────

/// Parse the `output_messages` JSON column to extract tool call keys and text snippets.
///
/// The JSON structure follows the OTel GenAI parts format stored by `store_event()`:
/// ```json
/// [{"role":"assistant","parts":[{"type":"tool_call","name":"read_file","arguments":{...}},{"type":"text","content":"..."}]}]
/// ```
///
/// Tool call arguments are reduced to a fingerprint so the loop detector can
/// distinguish the same tool invoked with different arguments (#2691).
pub(super) fn parse_output_messages_for_loop_detection(
    json_str: Option<&str>,
) -> (Vec<crate::interruption::ToolCallKey>, String) {
    let Some(json_str) = json_str else {
        return (vec![], String::new());
    };

    let messages: Vec<serde_json::Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return (vec![], String::new()),
    };

    let mut tool_calls = Vec::new();
    let mut text_parts = Vec::new();

    for msg in &messages {
        if let Some(parts) = msg.get("parts").and_then(|p| p.as_array()) {
            for part in parts {
                match part.get("type").and_then(|t| t.as_str()) {
                    Some("tool_call") => {
                        if let Some(name) = part.get("name").and_then(|n| n.as_str()) {
                            let args_fingerprint = part
                                .get("arguments")
                                .map(crate::interruption::ToolCallKey::fingerprint_args);
                            tool_calls.push(crate::interruption::ToolCallKey {
                                name: name.to_string(),
                                args_fingerprint,
                            });
                        }
                    }
                    Some("text") => {
                        if let Some(content) = part.get("content").and_then(|c| c.as_str()) {
                            text_parts.push(content);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Build a snippet from text parts (max 200 chars)
    let full_text = text_parts.join(" ");
    let snippet = if full_text.len() > 200 {
        full_text.chars().take(200).collect()
    } else {
        full_text
    };

    (tool_calls, snippet)
}
