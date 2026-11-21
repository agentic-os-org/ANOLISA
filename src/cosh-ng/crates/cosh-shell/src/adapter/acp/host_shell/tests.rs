//! Pure-logic tests for host-shell v1 validation and the mandatory-ask rule.

use super::*;

fn request() -> AcpHostExecuteRequest {
    AcpHostExecuteRequest {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        command: "echo hi".to_string(),
        cwd: "/tmp".to_string(),
        expected_cwd: None,
        pty_instance: "pty-1".to_string(),
        hook_requires_approval: false,
        approval_id: None,
    }
}

fn outcome() -> AcpHostExecuteOutcome {
    AcpHostExecuteOutcome {
        command: "echo hi".to_string(),
        status: "success".to_string(),
        exit_code: Some(0),
        signal: None,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        duration_ms: 1,
        output_ref: None,
        redaction_status: "none".to_string(),
        approval_id: None,
        tool_use_id: None,
        input_wait: None,
        llm_content: "hi".to_string(),
        llm_content_truncated: false,
    }
}

#[test]
fn unknown_fields_and_missing_mandatory_ask_are_deserialization_errors() {
    let base =
        r#"{"sessionId":"s","toolCallId":"t","command":"echo hi","cwd":"/tmp","ptyInstance":"p""#;
    let missing = format!("{base}}}");
    assert!(serde_json::from_str::<AcpHostExecuteRequest>(&missing).is_err());
    let unknown = format!("{base},\"hookRequiresApproval\":false,\"allow\":true}}");
    assert!(serde_json::from_str::<AcpHostExecuteRequest>(&unknown).is_err());
    let mistyped = format!("{base},\"hookRequiresApproval\":\"false\"}}");
    assert!(serde_json::from_str::<AcpHostExecuteRequest>(&mistyped).is_err());
    let ok = format!("{base},\"hookRequiresApproval\":false}}");
    assert!(serde_json::from_str::<AcpHostExecuteRequest>(&ok).is_ok());
}

#[test]
fn bounds_and_absolute_paths_are_enforced() {
    assert!(validate(&request()).is_ok());

    let mut oversized = request();
    oversized.command = "x".repeat(MAX_COMMAND_BYTES + 1);
    assert_eq!(
        validate(&oversized).expect_err("oversized command").code,
        CODE_INVALID_PARAMS
    );

    let mut relative = request();
    relative.cwd = "tmp".to_string();
    assert_eq!(
        validate(&relative).expect_err("relative cwd").code,
        CODE_INVALID_PARAMS
    );

    let mut expected = request();
    expected.expected_cwd = Some("relative".to_string());
    assert_eq!(
        validate(&expected).expect_err("relative expectedCwd").code,
        CODE_INVALID_PARAMS
    );

    let mut long_id = request();
    long_id.tool_call_id = "t".repeat(MAX_ID_BYTES + 1);
    assert_eq!(
        validate(&long_id).expect_err("oversized toolCallId").code,
        CODE_INVALID_PARAMS
    );

    let mut empty = request();
    empty.command = "   ".to_string();
    assert_eq!(
        validate(&empty).expect_err("blank command").code,
        CODE_INVALID_PARAMS
    );
}

#[test]
fn mandatory_ask_accepts_only_a_human_decision() {
    let mut asked = request();
    asked.hook_requires_approval = true;
    assert_eq!(
        review(&asked, AcpHostDecision::Allow)
            .expect_err("policy allow must not satisfy mustAsk")
            .code,
        CODE_CONSTRAINT
    );
    assert!(review(&asked, AcpHostDecision::HumanAllow).is_ok());
    assert_eq!(
        review(&asked, AcpHostDecision::Refuse)
            .expect_err("refusal stays refusal")
            .code,
        CODE_REFUSED
    );
}

#[test]
fn without_a_mandatory_ask_policy_still_decides() {
    let plain = request();
    assert!(review(&plain, AcpHostDecision::Allow).is_ok());
    assert_eq!(
        review(&plain, AcpHostDecision::Refuse)
            .expect_err("local policy may refuse")
            .code,
        CODE_REFUSED
    );
}

#[test]
fn oversized_model_content_is_truncated_with_a_coverage_fact() {
    let mut large = outcome();
    large.llm_content = "a".repeat(MAX_LLM_CONTENT_BYTES + 16);
    let bounded = bound_content(large);
    assert!(bounded.llm_content_truncated);
    assert_eq!(bounded.llm_content.len(), MAX_LLM_CONTENT_BYTES);

    let mut multi_byte = outcome();
    multi_byte.llm_content = format!("{}é", "a".repeat(MAX_LLM_CONTENT_BYTES - 1));
    let bounded = bound_content(multi_byte);
    assert!(bounded.llm_content_truncated);
    assert!(bounded.llm_content.len() < MAX_LLM_CONTENT_BYTES);
    assert!(bounded
        .llm_content
        .is_char_boundary(bounded.llm_content.len()));

    let small = bound_content(outcome());
    assert!(!small.llm_content_truncated);
    let value = serde_json::to_value(&small).expect("serialize outcome");
    assert!(value.get("llm_content_truncated").is_none());
    assert!(value.get("input_wait").is_none());
    assert_eq!(value["llmContent"], "hi");
    assert_eq!(value["exit_code"], 0);
}

#[test]
fn owned_admission_refuses_a_duplicate_id() {
    let mut owned = HashSet::new();
    owned.insert("call-1".to_string());
    let refusal = owned_admission(&owned, "call-1").expect_err("duplicate id");
    assert_eq!(refusal.code, CODE_CONSTRAINT);
    assert!(refusal.message.contains("already owned"));
}

#[test]
fn owned_admission_refuses_a_new_id_at_capacity() {
    let mut owned = HashSet::new();
    for i in 0..MAX_OWNED_TOOL_CALLS {
        owned.insert(format!("call-{i}"));
    }
    let refusal = owned_admission(&owned, "call-new").expect_err("capacity reached");
    assert_eq!(refusal.code, CODE_CONSTRAINT);
    assert!(refusal.message.contains("capacity reached"));
    // Existing ids stay non-replayable even at capacity; no eviction happened.
    assert_eq!(owned.len(), MAX_OWNED_TOOL_CALLS);
    assert!(owned.contains("call-0"));
    let dup = owned_admission(&owned, "call-0").expect_err("still owned");
    assert!(dup.message.contains("already owned"));
}

#[test]
fn owned_admission_admits_a_new_id_below_capacity() {
    let mut owned = HashSet::new();
    for i in 0..MAX_OWNED_TOOL_CALLS - 1 {
        owned.insert(format!("call-{i}"));
    }
    assert!(owned_admission(&owned, "call-new").is_ok());
}
