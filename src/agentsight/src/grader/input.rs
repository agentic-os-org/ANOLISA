//! Snapshot loading and stable input hashing for grader runs.

use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::types::{GraderError, RULE_GRADER_VERSION, TargetType};
use crate::storage::sqlite::genai::TraceEventDetail;
use crate::storage::sqlite::{GenAISqliteStore, InterruptionRecord, InterruptionStore};

/// Evidence snapshot used by a grader run.
pub struct EvaluationInput {
    /// Evaluated target kind.
    pub target_type: TargetType,
    /// Evaluated conversation id.
    pub target_id: String,
    /// Captured LLM call rows for the conversation.
    pub events: Vec<TraceEventDetail>,
    /// Captured interruption rows for the conversation.
    pub interruptions: Vec<InterruptionRecord>,
    /// Stable hash over the evaluated snapshot.
    pub input_hash: String,
    /// True when the snapshot contains pending calls and was forced.
    pub evaluated_with_pending: bool,
    /// Number of pending LLM calls in the snapshot.
    pub pending_call_count: usize,
}

/// Load a conversation snapshot and compute its stable input hash.
pub fn load_conversation_input(
    storage_path: &Path,
    interruption_store: Option<&InterruptionStore>,
    conversation_id: &str,
    force: bool,
) -> Result<EvaluationInput, GraderError> {
    let genai_store = GenAISqliteStore::new_with_path(
        storage_path,
        crate::config::InsertStoragePolicy::default(),
    )
    .map_err(|error| GraderError::Storage(error.to_string()))?;
    let events = genai_store
        .get_events_by_conversation(conversation_id)
        .map_err(|error| GraderError::Storage(error.to_string()))?;

    if events.is_empty() {
        return Err(GraderError::ConversationNotFound(
            conversation_id.to_string(),
        ));
    }

    let pending_call_count = events
        .iter()
        .filter(|event| event.status.as_deref() == Some("pending"))
        .count();
    if pending_call_count > 0 && !force {
        return Err(GraderError::ConversationNotReady {
            pending_count: pending_call_count,
        });
    }

    let interruptions = load_conversation_interruptions(interruption_store, conversation_id)?;
    let input_hash = compute_input_hash(conversation_id, &events, &interruptions)?;

    Ok(EvaluationInput {
        target_type: TargetType::Conversation,
        target_id: conversation_id.to_string(),
        events,
        interruptions,
        input_hash,
        evaluated_with_pending: pending_call_count > 0,
        pending_call_count,
    })
}

fn load_conversation_interruptions(
    interruption_store: Option<&InterruptionStore>,
    conversation_id: &str,
) -> Result<Vec<InterruptionRecord>, GraderError> {
    match interruption_store {
        Some(store) => store
            .list_by_conversation(conversation_id)
            .map_err(|error| GraderError::Storage(error.to_string())),
        None => Ok(Vec::new()),
    }
}

fn compute_input_hash(
    conversation_id: &str,
    events: &[TraceEventDetail],
    interruptions: &[InterruptionRecord],
) -> Result<String, GraderError> {
    #[derive(Serialize)]
    struct HashPayload<'a> {
        schema: &'static str,
        grader_version: &'static str,
        conversation_id: &'a str,
        events: Vec<serde_json::Value>,
        interruptions: Vec<serde_json::Value>,
    }

    let payload = HashPayload {
        schema: "agentsight-grader-input-v1",
        grader_version: RULE_GRADER_VERSION,
        conversation_id,
        events: events.iter().map(event_hash_value).collect(),
        interruptions: interruptions.iter().map(interruption_hash_value).collect(),
    };
    let bytes = serde_json::to_vec(&payload)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn event_hash_value(event: &TraceEventDetail) -> serde_json::Value {
    serde_json::json!({
        "id": event.id,
        "call_id": &event.call_id,
        "start_timestamp_ns": event.start_timestamp_ns,
        "end_timestamp_ns": &event.end_timestamp_ns,
        "model": &event.model,
        "input_tokens": event.input_tokens,
        "output_tokens": event.output_tokens,
        "total_tokens": event.total_tokens,
        "input_messages": &event.input_messages,
        "output_messages": &event.output_messages,
        "system_instructions": &event.system_instructions,
        "agent_name": &event.agent_name,
        "process_name": &event.process_name,
        "pid": &event.pid,
        "user_query": &event.user_query,
        "event_json": &event.event_json,
        "trace_id": &event.trace_id,
        "conversation_id": &event.conversation_id,
        "cache_read_tokens": &event.cache_read_tokens,
        "status": &event.status,
        "interruption_type": &event.interruption_type,
    })
}

fn interruption_hash_value(record: &InterruptionRecord) -> serde_json::Value {
    serde_json::json!({
        "interruption_id": &record.interruption_id,
        "session_id": &record.session_id,
        "trace_id": &record.trace_id,
        "conversation_id": &record.conversation_id,
        "call_id": &record.call_id,
        "pid": &record.pid,
        "agent_name": &record.agent_name,
        "interruption_type": &record.interruption_type,
        "severity": &record.severity,
        "occurred_at_ns": record.occurred_at_ns,
        "detail": &record.detail,
        "resolved": record.resolved,
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::genai::GenAIExporter;
    use crate::genai::semantic::{GenAISemanticEvent, LLMCall, LLMRequest};
    use crate::interruption::{InterruptionEvent, InterruptionType};
    use crate::storage::sqlite::genai::TraceEventDetail;

    use super::*;

    #[test]
    fn load_conversation_input_uses_injected_interruption_store() {
        let root = temp_root("grader_input_interruption_store");
        let genai_path = root.join("genai").join("events.db");
        let interruption_path = root.join("interruptions").join("events.db");
        write_conversation_event(&genai_path, "conv-injected");

        let interruption_store = InterruptionStore::new_with_path(&interruption_path).unwrap();
        let event = InterruptionEvent::new(
            InterruptionType::NetworkTimeout,
            Some("session-1".to_string()),
            Some("trace-1".to_string()),
            Some("conv-injected".to_string()),
            Some("call-1".to_string()),
            Some(1234),
            Some("Codex".to_string()),
            1_700_000_000_000_000_100,
            None,
        );
        interruption_store.insert(&event).unwrap();

        let input = load_conversation_input(
            &genai_path,
            Some(&interruption_store),
            "conv-injected",
            false,
        )
        .unwrap();

        assert_eq!(input.events.len(), 1);
        assert_eq!(input.interruptions.len(), 1);
        assert_eq!(
            input.interruptions[0].interruption_type,
            InterruptionType::NetworkTimeout.as_str()
        );

        cleanup_db(&genai_path);
        cleanup_db(&interruption_path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn compute_input_hash_is_stable_and_changes_with_snapshot_inputs() {
        let base_events = vec![trace_event("call-1", "complete", 10)];
        let base_hash = compute_input_hash("conv-1", &base_events, &[]).unwrap();
        let repeated_hash =
            compute_input_hash("conv-1", &[trace_event("call-1", "complete", 10)], &[]).unwrap();

        let changed_conversation_hash =
            compute_input_hash("conv-2", &[trace_event("call-1", "complete", 10)], &[]).unwrap();
        let changed_event_hash =
            compute_input_hash("conv-1", &[trace_event("call-1", "complete", 11)], &[]).unwrap();
        let changed_interruption_hash = compute_input_hash(
            "conv-1",
            &[trace_event("call-1", "complete", 10)],
            &[interruption_record("rate_limit", false)],
        )
        .unwrap();

        assert_eq!(base_hash, repeated_hash);
        assert_ne!(base_hash, changed_conversation_hash);
        assert_ne!(base_hash, changed_event_hash);
        assert_ne!(base_hash, changed_interruption_hash);
    }

    fn trace_event(call_id: &str, status: &str, output_tokens: i64) -> TraceEventDetail {
        TraceEventDetail {
            id: 1,
            call_id: Some(call_id.to_string()),
            start_timestamp_ns: 1_700_000_000_000_000_000,
            end_timestamp_ns: Some(1_700_000_000_000_000_100),
            model: Some("claude".to_string()),
            input_tokens: 100,
            output_tokens,
            total_tokens: 100 + output_tokens,
            input_messages: Some(r#"[{"role":"user","content":"hello"}]"#.to_string()),
            output_messages: Some(r#"[{"role":"assistant","content":"done"}]"#.to_string()),
            system_instructions: None,
            agent_name: Some("claude".to_string()),
            process_name: Some("claude".to_string()),
            pid: Some(1234),
            user_query: Some("hello".to_string()),
            event_json: None,
            trace_id: Some("trace-1".to_string()),
            conversation_id: Some("conv-1".to_string()),
            cache_read_tokens: None,
            status: Some(status.to_string()),
            interruption_type: None,
        }
    }

    fn interruption_record(interruption_type: &str, resolved: bool) -> InterruptionRecord {
        InterruptionRecord {
            id: 1,
            interruption_id: format!("intr-{interruption_type}"),
            session_id: Some("session-1".to_string()),
            trace_id: Some("trace-1".to_string()),
            conversation_id: Some("conv-1".to_string()),
            call_id: Some("call-1".to_string()),
            pid: Some(1234),
            agent_name: Some("claude".to_string()),
            interruption_type: interruption_type.to_string(),
            severity: "medium".to_string(),
            occurred_at_ns: 1_700_000_000_000_000_200,
            detail: None,
            resolved,
        }
    }

    fn write_conversation_event(path: &Path, conversation_id: &str) {
        let store =
            GenAISqliteStore::new_with_path(path, crate::config::InsertStoragePolicy::default())
                .unwrap();
        let mut call = LLMCall::new(
            "call-1".to_string(),
            1_700_000_000_000_000_000,
            "anthropic".to_string(),
            "claude".to_string(),
            LLMRequest {
                messages: Vec::new(),
                temperature: None,
                max_tokens: None,
                frequency_penalty: None,
                presence_penalty: None,
                top_p: None,
                top_k: None,
                seed: None,
                stop_sequences: None,
                stream: false,
                tools: None,
                raw_body: None,
            },
            1234,
            "claude".to_string(),
        );
        call.metadata
            .insert("conversation_id".to_string(), conversation_id.to_string());
        call.metadata
            .insert("response_id".to_string(), "trace-1".to_string());
        call.metadata
            .insert("user_query".to_string(), "hello".to_string());

        store.export(&[GenAISemanticEvent::LLMCall(call)]);
        store.flush();
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agentsight_{label}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn cleanup_db(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
