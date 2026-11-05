//! Supported instruction roles must survive the normal producer/query/export path.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::rc::Rc;

use agentsight::aggregator::ConnectionId;
use agentsight::analyzer::message::MessageParser;
use agentsight::analyzer::{AnalysisResult, HttpRecord};
use agentsight::atif::{convert_session_to_atif, convert_trace_to_atif};
use agentsight::config::PeriodicStoragePolicy;
use agentsight::database::{
    DatabaseAccess, DatabaseCoverage, DatabaseId, DatabaseManager, DatabaseRole, DatabaseSpec,
};
use agentsight::genai::semantic::{input_messages_from_column, latest_round_input_messages};
use agentsight::genai::{GenAIBuilder, GenAISemanticEvent, InputMessage, MessagePart};
use agentsight::parser::ParsedRequest;
use agentsight::probes::sslsniff::SslEvent;
use agentsight::response_map::ResponseSessionMapper;
use agentsight::storage::sqlite::{GenAISqliteStore, PendingOrigin};
use agentsight_atif::{AtifTrajectory, StepSource};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const PID: u32 = 2_000_000_001;
const START: u64 = 1_800_000_000_000_000_000;
const PATH: &str = "/v1/chat/completions";

struct Captured {
    call_id: String,
    start_ns: u64,
    end_ns: Option<u64>,
    session_id: String,
    trace_id: Option<String>,
    conversation_id: Option<String>,
    event_json: String,
    system: Option<String>,
    input: Option<String>,
    output: Option<String>,
    user_query: Option<String>,
    tokens: (i64, i64, i64),
    pending_origin: String,
    pending: bool,
}

fn assert_producer_messages(messages: &[InputMessage], expected: &[Value]) {
    assert_eq!(messages.len(), expected.len());
    for (message, expected) in messages.iter().zip(expected) {
        assert_eq!(message.role, expected["role"].as_str().unwrap());
        let text = message
            .parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text, expected["content"].as_str().unwrap());
    }
}

fn request(messages: Vec<Value>) -> Value {
    json!({"model": "synthetic-model", "messages": messages})
}

fn completed(messages: Vec<Value>, typed: bool) -> Captured {
    completed_at(
        &GenAIBuilder::new(),
        messages,
        typed,
        "synthetic-response",
        START,
    )
}

fn completed_at(
    builder: &GenAIBuilder,
    messages: Vec<Value>,
    typed: bool,
    response_id: &str,
    start_ns: u64,
) -> Captured {
    let request = request(messages);
    let response = json!({
        "id": response_id, "object": "chat.completion", "created": 1_800_000_000,
        "model": "synthetic-model", "choices": [{"index": 0,
            "message": {"role": "assistant", "content": "Synthetic answer."},
            "finish_reason": "stop"}]
    });
    let http = HttpRecord {
        timestamp_ns: start_ns,
        pid: PID,
        comm: "synthetic-agent".into(),
        method: "POST".into(),
        path: PATH.into(),
        status_code: 200,
        request_headers: "{}".into(),
        request_body: Some(request.to_string()),
        response_headers: "{}".into(),
        response_body: Some(response.to_string()),
        duration_ns: 100_000_000,
        first_output_timestamp_ns: None,
        is_sse: false,
        sse_event_count: 0,
    };
    let mut results = vec![AnalysisResult::Http(http)];
    if typed {
        results.push(AnalysisResult::Message(
            MessageParser::new()
                .parse_by_path(PATH, Some(&request), Some(&response))
                .expect("ordinary synthetic OpenAI exchange parses"),
        ));
    }
    let cache = HashMap::from([(PID, "SyntheticAgent".to_string())]);
    let (built, _) =
        builder.build_with_pending(&results, &ResponseSessionMapper::disabled(), &cache);
    let event = built.events.first().expect("normal producer emits a call");
    let GenAISemanticEvent::LLMCall(call) = event else {
        panic!("expected the normal semantic call");
    };
    assert_producer_messages(
        &call.request.messages,
        request["messages"].as_array().unwrap(),
    );
    // These are the writer's message representations; only the metadata-enriching
    // writer itself is bypassed, because it calls the real ECS metadata service.
    let system: Vec<_> = call
        .request
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .collect();
    let input = latest_round_input_messages(&call.request.messages);
    Captured {
        call_id: call.call_id.clone(),
        start_ns: call.start_timestamp_ns,
        end_ns: Some(call.end_timestamp_ns),
        session_id: call.metadata["session_id"].clone(),
        trace_id: call.metadata.get("response_id").cloned(),
        conversation_id: call.metadata.get("conversation_id").cloned(),
        event_json: serde_json::to_string(event).unwrap(),
        system: (!system.is_empty()).then(|| serde_json::to_string(&system).unwrap()),
        input: (!input.is_empty()).then(|| serde_json::to_string(&input).unwrap()),
        output: Some(serde_json::to_string(&call.response.messages).unwrap()),
        user_query: call.metadata.get("user_query").cloned(),
        tokens: call.token_usage.as_ref().map_or((0, 0, 0), |usage| {
            (
                i64::from(usage.input_tokens),
                i64::from(usage.output_tokens),
                i64::from(usage.total_tokens),
            )
        }),
        pending_origin: PendingOrigin::default().as_str().into(),
        pending: false,
    }
}

fn interrupted_body(body: Value, path: &str, parts_columns: bool) -> Captured {
    let expected: Vec<_> = body
        .get("messages")
        .or_else(|| body.get("input"))
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|message| message["role"] != "system")
        .cloned()
        .collect();
    let body = body.to_string();
    let source = Rc::new(SslEvent {
        source: 0,
        timestamp_ns: START,
        delta_ns: 0,
        pid: PID,
        tid: PID,
        uid: 0,
        len: body.len() as u32,
        rw: 1,
        comm: "synthetic-agent".into(),
        buf: body.into_bytes(),
        is_handshake: false,
        ssl_ptr: 1,
    });
    let request = ParsedRequest {
        method: "POST".into(),
        path: path.into(),
        version: 11,
        headers: HashMap::new(),
        body_offset: 0,
        body_len: source.buf.len(),
        source_event: source,
        reassembled_body: None,
    };
    let cache = HashMap::from([(PID, "SyntheticAgent".to_string())]);
    let info = GenAIBuilder::new()
        .build_pending_from_request(
            &request,
            &ConnectionId {
                pid: PID,
                ssl_ptr: 1,
            },
            &ResponseSessionMapper::disabled(),
            &cache,
        )
        .expect("ordinary synthetic pending request parses");
    assert_eq!(
        serde_json::from_str::<Vec<Value>>(info.input_messages.as_ref().unwrap()).unwrap(),
        expected
    );
    assert_producer_messages(
        &input_messages_from_column(info.input_messages.as_ref().unwrap()).unwrap(),
        &expected,
    );
    let normalize = |column: Option<String>| {
        column.map(|raw| {
            if parts_columns {
                serde_json::to_string(
                    &input_messages_from_column(&raw).expect("column format parses"),
                )
                .unwrap()
            } else {
                raw
            }
        })
    };
    Captured {
        call_id: info.call_id,
        start_ns: info.start_timestamp_ns,
        end_ns: None,
        session_id: info
            .session_id
            .expect("producer supplies a pending session identity"),
        trace_id: info.trace_id,
        conversation_id: info.conversation_id,
        // insert_pending uses this exact placeholder until a completed event exists.
        event_json: "{}".into(),
        system: normalize(info.system_instructions),
        input: normalize(info.input_messages),
        output: None,
        user_query: info.user_query,
        tokens: (0, 0, 0),
        pending_origin: info.pending_origin.as_str().into(),
        pending: true,
    }
}

fn interrupted(messages: Vec<Value>, parts_columns: bool) -> Captured {
    interrupted_body(request(messages), PATH, parts_columns)
}

fn export(capture: Captured) -> AtifTrajectory {
    export_many(vec![capture])
}

fn export_many(captures: Vec<Captured>) -> AtifTrajectory {
    let session_id = captures.first().unwrap().session_id.clone();
    assert!(
        captures
            .iter()
            .all(|capture| capture.session_id == session_id)
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic-genai.db");
    let manager = DatabaseManager::new(
        DatabaseRole::Server,
        [DatabaseSpec::new(
            DatabaseId::GenAi,
            &path,
            DatabaseAccess::ReadWrite,
            DatabaseCoverage::Full,
        )],
    )
    .unwrap();
    let store = manager
        .open_read_write(DatabaseId::GenAi, |path| {
            GenAISqliteStore::new_with_path(path, PeriodicStoragePolicy::default())
        })
        .unwrap();
    // This test-only fixture is populated exclusively from the actual producer.
    // The normal typed schema and read/query path remain unchanged. No exporter
    // writer, metadata HTTP, maintenance worker, model or recorded action is run.
    let conn = Connection::open(&path).unwrap();
    for capture in &captures {
        conn.execute(
        "INSERT INTO genai_events (event_type, status, call_id, trace_id, conversation_id,
            session_id, instance, start_timestamp_ns, end_timestamp_ns, pid,
            process_name, agent_name, provider, model, input_tokens, output_tokens, total_tokens,
            system_instructions, input_messages, output_messages, user_query, event_json, pending_origin)
         VALUES ('llm_call', ?1, ?2, ?3, ?4, ?5, 'synthetic-instance', ?6, ?7, ?8,
            'synthetic-agent', 'SyntheticAgent', 'openai', 'synthetic-model', ?14, ?15, ?16,
            ?9, ?10, ?11, ?12, ?13, ?17)",
        params![
            if capture.pending {
                "pending"
            } else {
                "complete"
            },
            capture.call_id,
            capture.trace_id,
            capture.conversation_id,
            capture.session_id,
            capture.start_ns as i64,
            capture.end_ns.map(|ns| ns as i64),
            PID as i32,
            capture.system,
            capture.input,
            capture.output,
            capture.user_query,
            capture.event_json,
            capture.tokens.0,
            capture.tokens.1,
            capture.tokens.2,
            capture.pending_origin
        ],
    )
    .unwrap();
        if capture.pending {
            assert_eq!(
                store
                    .mark_pending_interrupted_for_pid(PID as i32, "agent_crash")
                    .unwrap(),
                1
            );
        }
    }
    let events = store.get_events_by_session(&session_id).unwrap();
    assert_eq!(
        events.len(),
        captures.len(),
        "the real query must read every synthetic row"
    );
    for (event, capture) in events.iter().zip(&captures) {
        assert_eq!(
            event.status.as_deref(),
            Some(if capture.pending {
                "interrupted"
            } else {
                "complete"
            })
        );
    }
    let trace_id = if events.len() == 1 {
        events[0].trace_id.clone()
    } else {
        None
    };
    let document = if let Some(trace_id) = trace_id {
        convert_trace_to_atif(&trace_id, store.get_trace_events(&trace_id).unwrap()).unwrap()
    } else {
        convert_session_to_atif(&session_id, events).unwrap()
    };
    agentsight_atif::validate_trajectory_str(&serde_json::to_string(&document).unwrap()).unwrap()
}

fn instructions(document: &AtifTrajectory) -> Vec<&str> {
    document
        .steps
        .iter()
        .filter(|step| step.source == StepSource::System)
        .map(|step| step.message.as_str())
        .collect()
}

fn developer() -> Vec<Value> {
    vec![
        json!({"role": "developer", "content": "Synthetic developer instruction."}),
        json!({"role": "user", "content": "Synthetic question."}),
    ]
}

fn mixed() -> Vec<Value> {
    vec![
        json!({"role": "developer", "content": "D1"}),
        json!({"role": "system", "content": "S1"}),
        json!({"role": "developer", "content": "D1"}),
        json!({"role": "system", "content": "S2"}),
        json!({"role": "user", "content": "Synthetic question."}),
    ]
}

#[test]
fn completed_typed_developer_instruction_survives() {
    let document = export(completed(developer(), true));
    assert_eq!(
        instructions(&document),
        ["Synthetic developer instruction."]
    );
}

#[test]
fn completed_raw_developer_instruction_survives() {
    let document = export(completed(developer(), false));
    assert_eq!(
        instructions(&document),
        ["Synthetic developer instruction."]
    );
}

#[test]
fn completed_typed_mixed_instructions_keep_producer_order_and_repetitions() {
    let document = export(completed(mixed(), true));
    assert_eq!(instructions(&document), ["D1\nS1\nD1\nS2"]);
}

#[test]
fn completed_raw_mixed_instructions_keep_producer_order_and_repetitions() {
    let document = export(completed(mixed(), false));
    assert_eq!(instructions(&document), ["D1\nS1\nD1\nS2"]);
}

#[test]
fn interrupted_raw_developer_instruction_survives() {
    let document = export(interrupted(developer(), false));
    assert_eq!(
        instructions(&document),
        ["Synthetic developer instruction."]
    );
}

#[test]
fn interrupted_parts_developer_instruction_survives() {
    let document = export(interrupted(developer(), true));
    assert_eq!(
        instructions(&document),
        ["Synthetic developer instruction."]
    );
}

#[test]
fn interrupted_raw_split_columns_keep_available_instructions() {
    let document = export(interrupted(mixed(), false));
    // The historical split discarded cross-role interleaving: stable available
    // source order is system column followed by developer text from input.
    assert_eq!(instructions(&document), ["S1\nS2\nD1\nD1"]);
}

#[test]
fn interrupted_parts_split_columns_keep_available_instructions() {
    let document = export(interrupted(mixed(), true));
    assert_eq!(instructions(&document), ["S1\nS2\nD1\nD1"]);
}

#[test]
fn pending_plain_system_instruction_keeps_the_developer_context() {
    let document = export(interrupted_body(
        json!({"model": "synthetic-model",
        "instructions": "Synthetic top-level instruction.", "input": developer()}),
        "/v1/responses",
        false,
    ));
    assert_eq!(
        instructions(&document),
        ["Synthetic top-level instruction.\nSynthetic developer instruction."]
    );
}

#[test]
fn completed_system_only_control_keeps_existing_shape() {
    for typed in [false, true] {
        let document = export(completed(
            vec![
                json!({"role": "system", "content": "System."}),
                json!({"role": "user", "content": "Question."}),
            ],
            typed,
        ));
        assert_eq!(instructions(&document), ["System."]);
    }
}

#[test]
fn completed_user_only_control_does_not_invent_instructions() {
    for typed in [false, true] {
        let document = export(completed(
            vec![json!({"role": "user", "content": "Question."})],
            typed,
        ));
        assert!(instructions(&document).is_empty());
    }
}

#[test]
fn interrupted_system_only_control_keeps_existing_shape() {
    for parts in [false, true] {
        let document = export(interrupted(
            vec![
                json!({"role": "system", "content": "System."}),
                json!({"role": "user", "content": "Question."}),
            ],
            parts,
        ));
        assert_eq!(instructions(&document), ["System."]);
    }
}

#[test]
fn interrupted_user_only_control_does_not_invent_instructions() {
    for parts in [false, true] {
        let document = export(interrupted(
            vec![json!({"role": "user", "content": "Question."})],
            parts,
        ));
        assert!(instructions(&document).is_empty());
    }
}

#[test]
fn parsed_user_only_preserves_legacy_system_column_fallback() {
    for system in [
        json!("Legacy system.").to_string(),
        json!([{"role": "system", "content": "Legacy system."}]).to_string(),
        json!([{"role": "system", "parts": [{"type": "text", "content": "Legacy system."}]}])
            .to_string(),
    ] {
        let mut capture = completed(vec![json!({"role": "user", "content": "Question."})], true);
        // A legacy/partial semantic event may lack context retained in columns.
        // This is the converter's existing Strategy2 compatibility contract,
        // separate from normal writer-ingestion validation.
        capture.system = Some(system);
        assert_eq!(instructions(&export(capture)), ["Legacy system."]);
    }
}

#[test]
fn parsed_blank_instructions_preserve_legacy_system_column_fallback() {
    for typed in [false, true] {
        let mut capture = completed(
            vec![
                json!({"role": "system", "content": ""}),
                json!({"role": "developer", "content": ""}),
                json!({"role": "user", "content": "Question."}),
            ],
            typed,
        );
        capture.system = Some(json!("Legacy system.").to_string());
        assert_eq!(instructions(&export(capture)), ["Legacy system."]);
    }
}

#[test]
fn parsed_empty_instruction_text_still_reads_the_developer_column() {
    for parts in [false, true] {
        for leading in [None, Some("system"), Some("developer")] {
            let mut messages = Vec::new();
            if let Some(role) = leading {
                messages.push(json!({"role": role, "content": ""}));
            }
            messages.push(json!({"role": "user", "content": "Question."}));
            let mut capture = completed(messages, true);
            capture.input = interrupted(developer(), parts).input;
            assert_eq!(
                instructions(&export(capture)),
                ["Synthetic developer instruction."]
            );
        }
    }
}

fn two_turn_session(second_instruction: &str, typed: bool) -> AtifTrajectory {
    let builder = GenAIBuilder::new();
    let first = completed_at(
        &builder,
        vec![
            json!({"role": "developer", "content": "Instruction A."}),
            json!({"role": "user", "content": "First question."}),
        ],
        typed,
        "synthetic-first-response",
        START,
    );
    let second = completed_at(
        &builder,
        vec![
            json!({"role": "developer", "content": second_instruction}),
            json!({"role": "user", "content": "First question."}),
            json!({"role": "assistant", "content": "Synthetic prior answer."}),
            json!({"role": "user", "content": "Second question."}),
        ],
        typed,
        "synthetic-second-response",
        START + 1_000_000_000,
    );
    assert_eq!(
        first.session_id, second.session_id,
        "the real producer keeps this run together"
    );
    assert_ne!(
        first.conversation_id, second.conversation_id,
        "the real producer identifies different turns"
    );
    export_many(vec![first, second])
}

#[test]
fn session_keeps_one_unchanged_developer_context_across_turns() {
    for typed in [false, true] {
        let document = two_turn_session("Instruction A.", typed);
        assert_eq!(instructions(&document), ["Instruction A."]);
        assert_eq!(
            document
                .steps
                .iter()
                .map(|step| step.source)
                .collect::<Vec<_>>(),
            [
                StepSource::System,
                StepSource::User,
                StepSource::Agent,
                StepSource::User,
                StepSource::Agent
            ]
        );
        assert_eq!(
            document
                .steps
                .iter()
                .filter(|step| step.source == StepSource::User)
                .map(|step| step.message.as_str())
                .collect::<Vec<_>>(),
            ["First question.", "Second question."]
        );
    }
}

#[test]
fn session_emits_changed_developer_context_before_the_next_turn() {
    for typed in [false, true] {
        let document = two_turn_session("Instruction B.", typed);
        assert_eq!(
            instructions(&document),
            ["Instruction A.", "Instruction B."]
        );
        assert_eq!(
            document
                .steps
                .iter()
                .map(|step| step.source)
                .collect::<Vec<_>>(),
            [
                StepSource::System,
                StepSource::User,
                StepSource::Agent,
                StepSource::System,
                StepSource::User,
                StepSource::Agent
            ]
        );
        assert_eq!(
            document
                .steps
                .iter()
                .filter(|step| step.source == StepSource::User)
                .map(|step| step.message.as_str())
                .collect::<Vec<_>>(),
            ["First question.", "Second question."]
        );
    }
}
