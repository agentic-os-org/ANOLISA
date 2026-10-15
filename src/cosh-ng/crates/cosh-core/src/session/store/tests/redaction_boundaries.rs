use super::*;
use crate::provider::{ToolCallFunction, ToolCallInfo};
use serde_json::{json, Value};

const KEYS: [&str; 8] = [
    "auth_token",
    "session_token",
    "secret_key",
    "private_key",
    "access_key",
    "credentials",
    "credential",
    "bearer",
];
const TEXT: &str = "--password 'canary multi words'\nSECRET_KEY=canary-secret-material\nprivate_key=canary-private-material\naccess_key=canary-access-material\ntrace Cookie: sid=canary-cookie-material; mode=canary-cookie-tail\n--auth-token=canary-auth-material\n--session-token=canary-session-material\n--access-key=canary-flag-access\n--credentials=canary-credentials-material\nregion=ordinary-value\n中文 ordinary-text\n-----BEGIN PRIVATE KEY-----\ncanary-pem-material\n-----END PRIVATE KEY-----";
const TEXT_CANARIES: [&str; 12] = [
    "canary multi",
    "words",
    "canary-secret-material",
    "canary-private-material",
    "canary-access-material",
    "canary-cookie-material",
    "canary-cookie-tail",
    "canary-auth-material",
    "canary-session-material",
    "canary-flag-access",
    "canary-credentials-material",
    "canary-pem-material",
];

fn arguments() -> Value {
    let mut value = json!({"tokenizer": "ordinary-value", "region": "ordinary-value"});
    for key in KEYS {
        value[key] = json!(format!("canary-json-{key}"));
    }
    value
}

fn boundary_session(store: &SessionStore) -> PersistedSession {
    let message = Message::assistant_with_tool_calls(
        TEXT,
        vec![ToolCallInfo {
            id: "call-redaction".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "inspect".to_string(),
                arguments: arguments().to_string(),
            },
        }],
    );
    PersistedSession::new(
        ProviderSessionId::new(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![Message::user("ordinary prompt"), message],
    )
}

fn assert_masked(messages: &[Message]) {
    let text = messages[1].content.as_text();
    let leaked = TEXT_CANARIES
        .iter()
        .filter(|value| text.contains(**value))
        .count();
    assert!(leaked == 0, "text masking failed: {leaked} canaries remain");
    assert!(
        text.contains("region=ordinary-value") && text.contains("中文 ordinary-text"),
        "ordinary text changed"
    );
    let calls = messages[1].tool_calls.as_ref().expect("tool call retained");
    let value: Value = serde_json::from_str(&calls[0].function.arguments).expect("arguments JSON");
    let masked = KEYS
        .iter()
        .filter(|key| value[**key] == "<redacted>")
        .count();
    assert!(
        masked == KEYS.len(),
        "argument masking failed: {masked} of 8 masked"
    );
    assert!(
        value["tokenizer"] == "ordinary-value" && value["region"] == "ordinary-value",
        "ordinary argument changed"
    );
}

#[test]
fn redaction_boundaries_persist_load_preserve_live_copy() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = boundary_session(&store);
    let original = serde_json::to_string(&session.messages).unwrap();
    store.persist(&mut session).unwrap();
    assert!(
        serde_json::to_string(&session.messages).unwrap() == original,
        "persistence changed live context"
    );
    let disk = envelope_json(&store, &session.session_id);
    let disk_messages: Vec<Message> = serde_json::from_value(disk["messages"].clone()).unwrap();
    assert_masked(&disk_messages);
    assert_masked(&store.load(&session.session_id).unwrap().messages);
}

#[test]
fn redaction_boundaries_load_external_envelope() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = boundary_session(&store);
    store.persist(&mut session).unwrap();
    let mut disk = envelope_json(&store, &session.session_id);
    disk["messages"] = serde_json::to_value(boundary_session(&store).messages).unwrap();
    fs::write(
        store.session_file(&session.session_id),
        serde_json::to_vec(&disk).unwrap(),
    )
    .unwrap();
    assert_masked(&store.load(&session.session_id).unwrap().messages);
}

#[test]
fn redaction_boundaries_hook_payload_preserves_schema() {
    let properties: serde_json::Map<String, Value> = KEYS
        .iter()
        .map(|key| {
            (
                key.to_string(),
                json!({"type": "string", "description": "ordinary value"}),
            )
        })
        .collect();
    let tools =
        json!([{"name": "inspect", "parameters": {"type": "object", "properties": properties}}]);
    let payload = json!({
        "hook_data": {"nested": [arguments()]},
        "llm_request": {"config": {"tools": tools.clone()}, "tools": tools.clone()}
    });
    let output: Value = serde_json::from_str(&crate::redaction::to_redacted_json_with_schemas(
        &payload,
        crate::redaction::TOOL_DECLARATION_PATHS,
    ))
    .unwrap();
    let masked = KEYS
        .iter()
        .filter(|key| output["hook_data"]["nested"][0][**key] == "<redacted>")
        .count();
    assert!(
        masked == KEYS.len(),
        "hook masking failed: {masked} of 8 masked"
    );
    assert!(
        output["hook_data"]["nested"][0]["tokenizer"] == "ordinary-value",
        "ordinary hook value changed"
    );
    assert!(
        output["llm_request"]["config"]["tools"] == tools
            && output["llm_request"]["tools"] == tools,
        "schema declaration changed"
    );
}

#[test]
fn redaction_boundaries_load_compaction_summary() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = new_session(&store, "ordinary prompt");
    store.persist(&mut session).unwrap();
    let mut state = projection(1);
    state.compacted_through = session.messages.len();
    state.summary = TEXT.to_string();
    state.source_digest = crate::compaction::source_digest(&session.messages);
    let mut disk = envelope_json(&store, &session.session_id);
    disk["compaction"] = serde_json::to_value(state).unwrap();
    fs::write(
        store.session_file(&session.session_id),
        serde_json::to_vec(&disk).unwrap(),
    )
    .unwrap();
    let loaded = store.load(&session.session_id).unwrap();
    let summary = loaded
        .compaction
        .expect("valid projection retained")
        .summary;
    let leaked = TEXT_CANARIES
        .iter()
        .filter(|value| summary.contains(**value))
        .count();
    assert!(
        leaked == 0,
        "loaded summary masking failed: {leaked} canaries remain"
    );
    assert!(
        summary.contains("region=ordinary-value"),
        "ordinary summary changed"
    );
}

#[test]
fn redaction_boundaries_persist_complete_words_and_catalog() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    for text in [r#"tool --password "canary multi words" --region cn"#, r"tool --password canary\ multi\ words --region cn", "tool --password 'unterminated canary tail", "SECRET_ACCESS_KEY=canary-material ACCESS_KEY=canary-access --private-key=canary-private --secret-key=canary-secret --bearer=canary-bearer"] {
        let mut session = new_session(&store, text);
        store.persist(&mut session).unwrap();
        let loaded = store.load(&session.session_id).unwrap();
        let safe = loaded.messages[0].content.as_text();
        assert!(!safe.contains("canary") && !safe.contains("words") && !safe.contains("tail"), "complete word masking failed");
        if text.contains("--region cn") { assert!(safe.contains("--region cn"), "ordinary flag changed"); }
    }
    let ordinary = "tokenizer=ordinary api_key_id_shape=ordinary 中文 ordinary-text";
    let mut session = new_session(&store, ordinary);
    store.persist(&mut session).unwrap();
    assert!(
        store.load(&session.session_id).unwrap().messages[0]
            .content
            .as_text()
            == ordinary,
        "ordinary persisted text changed"
    );
}

#[test]
fn redaction_boundaries_persist_escaped_quoted_flags() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    for text in [
        r#"tool --password "canary\" multi words" --region cn"#,
        r#"tool --password "canary\\ multi words" --region cn"#,
        r#"tool --password 'canary part'" other words" --region cn"#,
    ] {
        let mut session = new_session(&store, text);
        store.persist(&mut session).unwrap();
        let loaded = store.load(&session.session_id).unwrap();
        let safe = loaded.messages[0].content.as_text();
        assert!(
            !safe.contains("canary") && !safe.contains("words") && !safe.contains("multi"),
            "escaped quoted flag masking failed"
        );
        assert!(
            safe.contains("--region cn"),
            "ordinary escaped-word flag changed"
        );
    }
}

#[test]
fn redaction_boundaries_persist_json_text_preserves_ordinary_fields() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let text = r#"{"password":"canary-material","region":"ordinary-region","nested":[{"secret_key":"canary-nested","tokenizer":"ordinary-tokenizer"},{"auth_token":"canary-second","region":"ordinary-second"}]}"#;
    let mut session = new_session(&store, text);
    store.persist(&mut session).unwrap();
    let loaded = store.load(&session.session_id).unwrap();
    let safe = loaded.messages[0].content.as_text();
    assert!(!safe.contains("canary"), "JSON text masking failed");
    let value: Value = serde_json::from_str(&safe).expect("masked JSON remains structured");
    assert!(
        value["region"] == "ordinary-region"
            && value["nested"][0]["tokenizer"] == "ordinary-tokenizer"
            && value["nested"][1]["region"] == "ordinary-second",
        "ordinary JSON fields changed"
    );
    let ordinary = "{ \"region\" : \"ordinary-region\", \"tokenizer\" : \"ordinary-tokenizer\" }";
    let mut session = new_session(&store, ordinary);
    store.persist(&mut session).unwrap();
    assert!(
        store.load(&session.session_id).unwrap().messages[0]
            .content
            .as_text()
            == ordinary,
        "ordinary JSON bytes changed"
    );
}

#[test]
fn redaction_boundaries_persist_json_legacy_keys_and_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    for text in [
        r#"{"aws_session_token":"canary-material","region":"ordinary-region"}"#,
        r#"{"ALIBABA_CLOUD_ACCESS_KEY_ID":"canary-material","region":"ordinary-region"}"#,
        r#"{"prefixed_token":"canary-material","region":"ordinary-region"}"#,
        r#"{"password":"canary-material","password":"<redacted>","region":"ordinary-region"}"#,
        r#"{"region":"token=canary-material","region":"ordinary-region"}"#,
        r#"{"api\u005fkey":"canary-material","api_key":"<redacted>","region":"ordinary-region"}"#,
    ] {
        let mut session = new_session(&store, text);
        store.persist(&mut session).unwrap();
        let loaded = store.load(&session.session_id).unwrap();
        let safe = loaded.messages[0].content.as_text();
        assert!(
            !safe.contains("canary"),
            "JSON legacy or duplicate masking failed"
        );
        let value: Value = serde_json::from_str(&safe).expect("JSON remains structured");
        assert!(
            value["region"] == "ordinary-region",
            "ordinary duplicate JSON field changed"
        );
    }
    let ordinary =
        r#"{ "region":"first ordinary", "region":"last ordinary", "tokenizer":"ordinary" }"#;
    let mut session = new_session(&store, ordinary);
    store.persist(&mut session).unwrap();
    assert!(
        store.load(&session.session_id).unwrap().messages[0]
            .content
            .as_text()
            == ordinary,
        "ordinary duplicate JSON bytes changed"
    );
}

#[test]
fn redaction_boundaries_persist_json_shape_keys_preserve_values() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let text = json!({format!("AKIA{}", "A".repeat(16)): "ordinary-one", format!("ghp_{}", "a".repeat(36)): "ordinary-two", "<redacted>": "ordinary-reserved", "region": "ordinary-region"}).to_string();
    let mut session = new_session(&store, &text);
    store.persist(&mut session).unwrap();
    let loaded = store.load(&session.session_id).unwrap();
    let safe = loaded.messages[0].content.as_text();
    assert!(
        !safe.contains(&format!("AKIA{}", "A".repeat(16)))
            && !safe.contains(&format!("ghp_{}", "a".repeat(36))),
        "JSON key shapes masking failed"
    );
    let value: Value = serde_json::from_str(&safe).expect("JSON remains structured");
    let fields = value.as_object().expect("object retained");
    assert!(
        fields.len() == 4
            && fields.values().any(|v| v == "ordinary-one")
            && fields.values().any(|v| v == "ordinary-two")
            && fields["<redacted>"] == "ordinary-reserved"
            && fields["region"] == "ordinary-region",
        "ordinary values or reserved key changed"
    );
}

#[test]
fn redaction_boundaries_persist_json_private_key_span() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    for text in [json!({"begin": "-----BEGIN PRIVATE KEY-----", "body": "canary-private-material", "end": "-----END PRIVATE KEY-----", "region": "ordinary-region"}).to_string(), json!({"pem": "-----BEGIN PRIVATE KEY-----\ncanary-private-material\n-----END PRIVATE KEY-----", "region": "ordinary-region"}).to_string()] {
        let mut session = new_session(&store, &text);
        store.persist(&mut session).unwrap();
        let loaded = store.load(&session.session_id).unwrap();
        let safe = loaded.messages[0].content.as_text();
        assert!(!safe.contains("canary"), "JSON private-key span masking failed");
        assert!(safe.contains("ordinary-region"), "ordinary text outside private-key span changed");
    }
}

#[test]
fn redaction_boundaries_persist_json_pem_with_decoded_keys() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    for (case, text) in [
        r#"{"pem":"-----BEGIN PRIVATE KEY-----\ncanary-pem\n-----END PRIVATE KEY-----","api\u005fkey":"canary-credential","region":"ordinary-region"}"#,
        r#"{"api\u005fkey":"canary-credential","pem":"-----BEGIN PRIVATE KEY-----\ncanary-pem\n-----END PRIVATE KEY-----","region":"ordinary-region"}"#,
        r#"{"begin":"-----BEGIN PRIVATE KEY-----","nested":{"body":"canary-pem","end":"-----END PRIVATE KEY-----"},"api\u005fkey":"canary-credential","region":"ordinary-region"}"#,
        r#"{"api_key":"-----BEGIN PRIVATE KEY-----","body":"canary-pem","end":"-----END PRIVATE KEY-----","api\u005fkey":"canary-credential","region":"ordinary-region"}"#,
        r#"{"begin":"-----BEGIN PRIVATE KEY-----","body":"canary-pem","api_key":"-----END PRIVATE KEY-----","auth\u005ftoken":"canary-credential","region":"ordinary-region"}"#,
        r#"{"text":"ordinary-prefix api_key=canary-before -----BEGIN PRIVATE KEY----- canary-pem -----END PRIVATE KEY----- auth_token=canary-after ordinary-suffix","region":"ordinary-region"}"#,
        r#"{"api_key":"canary-credential","pem":"-----BEGIN PRIVATE KEY-----\ncanary-pem\n-----END PRIVATE KEY-----","region":"ordinary-region"}"#,
        r#"[{"pem":"-----BEGIN PRIVATE KEY----- canary-pem -----END PRIVATE KEY-----"},{"api\u005fkey":{"nested":"canary-credential"},"region":"ordinary-region"}]"#,
    ].into_iter().enumerate() {
        let mut session = new_session(&store, text);
        store.persist(&mut session).unwrap();
        let envelope = std::fs::read_to_string(store.session_file(&session.session_id)).unwrap();
        assert!(!envelope.contains("canary"), "combined JSON PEM credential escaped masking");
        assert!(envelope.contains("ordinary-region"), "ordinary persisted field changed in case {case}");
        let loaded = store.load(&session.session_id).unwrap();
        assert!(loaded.messages[0].content.as_text().contains("ordinary-region"), "ordinary field outside PEM changed in case {case}");
        if case == 5 {
            assert!(loaded.messages[0].content.as_text().contains("ordinary-prefix") && loaded.messages[0].content.as_text().contains("ordinary-suffix"), "ordinary token fragments changed");
        }
        assert_eq!(session.messages[0].content.as_text(), text, "live context changed");
    }
}

#[test]
fn redaction_boundaries_persist_many_private_key_fields_has_linear_visits() {
    let count = 1024;
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut fields = serde_json::Map::new();
    let mut spanning = serde_json::Map::new();
    spanning.insert("begin".to_string(), json!("-----BEGIN PRIVATE KEY-----"));
    for index in 0..count {
        fields.insert(
            format!("field-{index:04}"),
            json!(
                "-----BEGIN PRIVATE KEY-----\ncanary-private-material\n-----END PRIVATE KEY-----"
            ),
        );
        spanning.insert(format!("body-{index:04}"), json!("canary-private-material"));
    }
    fields.insert("region".to_string(), json!("ordinary-region"));
    spanning.insert("end".to_string(), json!("-----END PRIVATE KEY-----"));
    spanning.insert("region".to_string(), json!("ordinary-region"));
    let one_token = json!({"text": format!("ordinary-prefix 中文 {} ordinary-suffix β", "-----BEGIN PRIVATE KEY----- canary-private-material -----END PRIVATE KEY----- ".repeat(count)), "region": "ordinary-region"}).to_string();
    for (case, text) in [
        serde_json::to_string_pretty(&Value::Object(fields)).unwrap(),
        serde_json::to_string_pretty(&Value::Object(spanning)).unwrap(),
        one_token,
    ]
    .into_iter()
    .enumerate()
    {
        let mut session = new_session(&store, &text);
        crate::redaction::redaction_work(true);
        store.persist(&mut session).unwrap();
        let safe = store.load(&session.session_id).unwrap().messages[0]
            .content
            .as_text();
        let (visits, probes, _) = crate::redaction::redaction_work(false);
        assert!(!safe.contains("canary"), "large PEM masking failed");
        assert!(
            safe.contains("ordinary-region"),
            "large ordinary region changed"
        );
        if case == 0 {
            let values: Value = serde_json::from_str(&safe).unwrap();
            assert!(
                values.as_object().unwrap().len() == count + 1,
                "large ordinary field structure changed"
            );
        }
        if case == 2 {
            assert!(
                safe.contains("ordinary-prefix 中文") && safe.contains("ordinary-suffix β"),
                "large ordinary token fragments changed"
            );
        }
        assert!(
            session.messages[0].content.as_text() == text,
            "large live context changed"
        );
        assert!(
            visits > 0 && visits <= 8 * (3 * count + 2),
            "PEM/token visits exceed linear bound"
        );
        assert!(
            probes <= 8 * (count + 3),
            "PEM masked-key probes exceed linear bound"
        );
    }
}

#[test]
fn redaction_boundaries_persist_masked_aliases_has_linear_probes() {
    let count = 1024;
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut fields = serde_json::Map::new();
    for index in 0..count {
        fields.insert(format!("ghp_{index:036x}"), json!("ordinary-value"));
        fields.insert(format!("<redacted>-{}", index + 1), json!("reserved-value"));
    }
    fields.insert("<redacted>".to_string(), json!("reserved-value"));
    fields.insert("region".to_string(), json!("ordinary-region"));
    let text = Value::Object(fields).to_string();
    let mut session = new_session(&store, &text);
    crate::redaction::redaction_work(true);
    store.persist(&mut session).unwrap();
    let safe = store.load(&session.session_id).unwrap().messages[0]
        .content
        .as_text();
    let (_, probes, _) = crate::redaction::redaction_work(false);
    assert!(!safe.contains("ghp_"), "large shape keys were not masked");
    let values: Value = serde_json::from_str(&safe).unwrap();
    assert!(
        values.as_object().unwrap().len() == 2 * count + 2
            && values["region"] == "ordinary-region"
            && values["<redacted>"] == "reserved-value"
            && values
                .as_object()
                .unwrap()
                .values()
                .filter(|value| *value == "ordinary-value")
                .count()
                == count
            && values
                .as_object()
                .unwrap()
                .values()
                .filter(|value| *value == "reserved-value")
                .count()
                == count + 1,
        "large ordinary key values changed"
    );
    assert!(
        session.messages[0].content.as_text() == text,
        "large live context changed"
    );
    assert!(
        probes > 0 && probes <= 8 * (2 * count + 2),
        "masked-key probes exceed linear bound"
    );
}

#[test]
fn redaction_boundaries_persist_sensitive_parent_parses_once() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut value = json!(format!(
        "-----BEGIN PRIVATE KEY-----\n{}",
        "canary-inner".repeat(8192)
    ));
    for _ in 0..32 {
        value = json!({"credentials": value});
    }
    let text = format!("{{\"credentials\":{value},\"body\":\"canary-body\",\"end\":\"-----END PRIVATE KEY-----\",\"region\":\"ordinary-region\"}}");
    let mut session = new_session(&store, &text);
    crate::redaction::redaction_work(true);
    store.persist(&mut session).unwrap();
    let safe = store.load(&session.session_id).unwrap().messages[0]
        .content
        .as_text();
    let (_, _, bytes) = crate::redaction::redaction_work(false);
    assert!(
        !safe.contains("canary"),
        "sensitive parent or cross-parent PEM leaked"
    );
    assert!(
        safe.contains("ordinary-region"),
        "ordinary field outside sensitive parent changed"
    );
    assert!(
        session.messages[0].content.as_text() == text,
        "nested live context changed"
    );
    assert!(
        bytes > 0 && bytes <= text.len(),
        "sensitive subtree was parsed repeatedly"
    );
}

#[test]
fn redaction_boundaries_persist_near_envelope_limit_private_value() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let value = "canary-private-material".repeat(30 * 1024 * 1024 / 23);
    let text = json!({"pem": format!("ordinary-prefix 中文 -----BEGIN PRIVATE KEY-----\n{value}\n-----END PRIVATE KEY----- ordinary-suffix β"), "region": "ordinary-region"}).to_string();
    assert!(
        text.len() > 29 * 1024 * 1024 && text.len() < MAX_SESSION_FILE_BYTES as usize,
        "size boundary fixture invalid"
    );
    let mut session = new_session(&store, &text);
    crate::redaction::redaction_work(true);
    store.persist(&mut session).unwrap();
    let safe = store.load(&session.session_id).unwrap().messages[0]
        .content
        .as_text();
    let (visits, probes, bytes) = crate::redaction::redaction_work(false);
    assert!(
        !safe.contains("canary")
            && safe.contains("ordinary-region")
            && safe.contains("ordinary-prefix 中文")
            && safe.contains("ordinary-suffix β"),
        "large private/ordinary byte boundary failed"
    );
    assert!(
        safe.len() < 1024 && session.messages[0].content.as_text() == text,
        "large value or live context changed"
    );
    assert!(
        visits > 0 && visits < 32 && probes < 32 && bytes <= text.len(),
        "large scalar visits exceed constant bound"
    );
}
