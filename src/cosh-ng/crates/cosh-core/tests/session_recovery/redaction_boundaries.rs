use super::*;

const PROMPT: &str = "ordinary-marker\n--password 'canary multi words'\nSECRET_KEY=canary-secret-material\nprivate_key=canary-private-material\naccess_key=canary-access-material\ntrace Cookie: sid=canary-cookie-material; mode=canary-cookie-tail\nregion=ordinary-value";
const CANARIES: [&str; 7] = [
    "canary multi",
    "words",
    "canary-secret-material",
    "canary-private-material",
    "canary-access-material",
    "canary-cookie-material",
    "canary-cookie-tail",
];
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

fn checked_lines(output: &Output) -> Vec<Value> {
    assert!(output.status.success(), "Core binary exit failed");
    json_lines(output)
}

fn result_id(messages: &[Value]) -> &str {
    messages
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("session result id")
}

fn envelope_path(store: &Path, session_id: &str) -> PathBuf {
    fs::read_dir(store)
        .expect("read session root")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(format!("{session_id}.json")))
        .find(|path| path.is_file())
        .expect("session envelope")
}

fn assert_masked(text: &str) {
    let leaked = CANARIES
        .iter()
        .filter(|value| text.contains(**value))
        .count();
    assert!(
        leaked == 0,
        "Core binary masking failed: {leaked} canaries remain"
    );
    assert!(
        text.contains("ordinary-marker") && text.contains("region=ordinary-value"),
        "Core binary ordinary context changed"
    );
    assert!(
        text.contains("<redacted>"),
        "Core binary redaction marker missing"
    );
}

fn resumed_lines(home: &Path, workspace: &Path, session_id: &str) -> Vec<Value> {
    checked_lines(&run_protocol(
        home,
        workspace,
        &["--resume", session_id],
        &[
            json!({"type": "control_request", "request_id": "redaction-init",
            "request": {"subtype": "initialize"}}),
            json!({"type": "user", "message": {"role": "user", "content": "ordinary continuation"}}),
        ],
    ))
}

fn assert_restored_context(messages: &[Value], session_id: &str) {
    assert!(
        result_id(messages) == session_id,
        "Core binary resume identity changed"
    );
    let assistant = messages
        .iter()
        .find(|message| message["type"] == "assistant")
        .and_then(|message| message["message"]["content"][0]["text"].as_str())
        .expect("mock history response");
    assert_masked(assistant);
    assert!(
        assistant.contains("ordinary continuation"),
        "resumed turn missing"
    );
}

#[test]
fn redaction_boundaries_binary_persist_then_resume_mock_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&workspace).expect("workspace");
    configure(&home, &store);
    let first = checked_lines(&run_prompt(&home, &workspace, &[PROMPT]));
    let session_id = result_id(&first);
    let live = first
        .iter()
        .find(|message| message["type"] == "assistant")
        .and_then(|message| message["message"]["content"][0]["text"].as_str())
        .expect("live mock history response");
    assert!(
        CANARIES.iter().all(|value| live.contains(value)),
        "live context unexpectedly changed"
    );
    let path = envelope_path(&store, session_id);
    assert_masked(&fs::read_to_string(&path).expect("persisted envelope"));
    assert_restored_context(&resumed_lines(&home, &workspace, session_id), session_id);
    assert_masked(&fs::read_to_string(&path).expect("resumed envelope"));
}

#[test]
fn redaction_boundaries_binary_resume_external_envelope_and_arguments() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&workspace).expect("workspace");
    configure(&home, &store);
    let first = checked_lines(&run_prompt(&home, &workspace, &["ordinary seed"]));
    let session_id = result_id(&first);
    let path = envelope_path(&store, session_id);
    let mut envelope: Value =
        serde_json::from_slice(&fs::read(&path).expect("envelope bytes")).expect("envelope JSON");
    let mut arguments = json!({"tokenizer": "ordinary-value", "region": "ordinary-value"});
    for key in KEYS {
        arguments[key] = json!(format!("canary-json-{key}"));
    }
    envelope["messages"] = json!([
        {"role": "user", "content": PROMPT},
        {"role": "assistant", "content": "ordinary call", "tool_calls": [{
            "id": "call-redaction", "type": "function", "function": {
                "name": "inspect", "arguments": arguments.to_string()
            }
        }]},
        {"role": "tool", "tool_call_id": "call-redaction", "content": "ordinary result"},
        {"role": "assistant", "content": "ordinary completion"}
    ]);
    fs::write(
        &path,
        serde_json::to_vec(&envelope).expect("serialize external envelope"),
    )
    .expect("write external envelope");
    assert_restored_context(&resumed_lines(&home, &workspace, session_id), session_id);
    let disk = fs::read_to_string(&path).expect("resumed envelope");
    assert_masked(&disk);
    let envelope: Value = serde_json::from_str(&disk).expect("resumed JSON");
    let arguments: Value = serde_json::from_str(
        envelope["messages"][1]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("restored arguments string"),
    )
    .expect("restored arguments JSON");
    let masked = KEYS
        .iter()
        .filter(|key| arguments[**key] == "<redacted>")
        .count();
    assert!(
        masked == KEYS.len(),
        "Core binary arguments masking failed: {masked} of 8 masked"
    );
    assert!(
        arguments["tokenizer"] == "ordinary-value" && arguments["region"] == "ordinary-value",
        "Core binary ordinary arguments changed"
    );
}
