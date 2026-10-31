use super::*;
use crate::cli::ExecutionProfile;
use serde_json::{json, Value};

fn frame(body: Value) -> String {
    json!({"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":body}}).to_string()+"\n"
}

fn shell(metadata: Value) -> Value {
    json!({"behavior":"host_executed_shell","result":{"llmContent":"host evidence","returnDisplay":null,"metadata":metadata}})
}

fn checkpoint() -> Value {
    json!({"checkpoint_id":"ckp_123e4567-e89b-12d3-a456-426614174000","outcome":{"status":"created","snapshot_id":"snapshot"}})
}

fn classification(result: ApprovalResult) -> &'static str {
    match result {
        ApprovalResult::Allowed => "allow",
        ApprovalResult::Denied(_) => "deny",
        ApprovalResult::Unconfirmed(_) => "unconfirmed",
        ApprovalResult::HostExecutedShell {
            exit_code: Some(0), ..
        } => "shell_success",
        ApprovalResult::HostExecutedShell {
            exit_code: Some(_), ..
        } => "shell_failure",
        ApprovalResult::HostExecutedShell {
            exit_code: None, ..
        } => "shell_unknown",
        ApprovalResult::HostExecutedCheckpoint { result } if result.is_error => {
            "checkpoint_failure"
        }
        ApprovalResult::HostExecutedCheckpoint { .. } => "checkpoint_success",
        ApprovalResult::Interrupted => "interrupted",
        ApprovalResult::TimedOut => "timeout",
    }
}

async fn matrix(
    cases: Vec<(&str, String, &str)>,
    profile: ExecutionProfile,
    kind: Option<ToolKind>,
) {
    let core = make_core_with_profile(MockProvider::text_only(""), profile);
    let mut failures = Vec::new();
    for (label, first, expected) in cases {
        let input = first + &frame(json!({"behavior":"deny"}));
        let mut lines = BufReader::new(input.as_bytes()).lines();
        let observed = classification(core.wait_for_approval("req-0", kind, &mut lines).await);
        if observed != expected {
            failures.push(format!("{label}: expected {expected}, got {observed}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn decisions_are_disjoint_from_execution_payloads() {
    let mut cases = Vec::new();
    for behavior in ["deny", "allow"] {
        for field in ["result", "checkpointResult", "checkpointError"] {
            for value in [
                Value::Null,
                json!(0),
                json!({"llmContent":"executed","metadata":{"exit_code":0}}),
            ] {
                let mut body = json!({"behavior":behavior});
                body[field] = value;
                cases.push((
                    "execution payload in plain decision",
                    frame(body),
                    "unconfirmed",
                ));
            }
        }
    }
    cases.extend([
        (
            "unknown deny field",
            frame(json!({"behavior":"deny","hiddenAuthority":null})),
            "unconfirmed",
        ),
        (
            "unknown allow field",
            frame(json!({"behavior":"allow","hiddenAuthority":true})),
            "unconfirmed",
        ),
        (
            "wrong deny message",
            frame(json!({"behavior":"deny","message":42})),
            "unconfirmed",
        ),
        (
            "wrong allow permission type",
            frame(json!({"behavior":"allow","updatedPermissions":"all"})),
            "unconfirmed",
        ),
        (
            "wrong allow tool id",
            frame(json!({"behavior":"allow","toolUseID":42})),
            "unconfirmed",
        ),
        ("bare deny", frame(json!({"behavior":"deny"})), "deny"),
        (
            "null diagnostic message",
            frame(json!({"behavior":"deny","message":null})),
            "deny",
        ),
        ("bare allow", frame(json!({"behavior":"allow"})), "allow"),
        (
            "legacy allow extras",
            frame(json!({"behavior":"allow","updatedPermissions":[],"toolUseID":"legacy-tool"})),
            "allow",
        ),
    ]);
    matrix(cases, ExecutionProfile::Legacy, Some(ToolKind::ShellExec)).await;
}

#[tokio::test]
async fn correlated_malformed_envelopes_cannot_be_replaced_by_later_denial() {
    let valid = frame(json!({"behavior":"deny"}));
    let duplicate_id = valid.replace(
        "\"request_id\":\"req-0\"",
        "\"request_id\":\"foreign\",\"request_id\":\"req-0\"",
    );
    let duplicate_behavior = valid.replace(
        "\"behavior\":\"deny\"",
        "\"behavior\":\"allow\",\"behavior\":\"deny\"",
    );
    let mut cases = vec![
        ("duplicate id", duplicate_id, "unconfirmed"),
        ("duplicate behavior", duplicate_behavior, "unconfirmed"),
        (
            "error envelope",
            valid.replace("\"subtype\":\"success\"", "\"subtype\":\"error\""),
            "unconfirmed",
        ),
        (
            "wrong request id type",
            valid.replace("\"request_id\":\"req-0\"", "\"request_id\":42"),
            "unconfirmed",
        ),
        (
            "missing request id",
            valid.replace("\"request_id\":\"req-0\",", ""),
            "unconfirmed",
        ),
        (
            "unknown behavior",
            frame(json!({"behavior":"unknown"})),
            "unconfirmed",
        ),
        (
            "wrong behavior type",
            frame(json!({"behavior":42})),
            "unconfirmed",
        ),
        (
            "foreign malformed ignored",
            frame(json!({"behavior":"deny","result":42})).replace("req-0", "foreign"),
            "deny",
        ),
        ("bare current receipt", json!({"type":"approval_receipt","request_id":"req-0"}).to_string()+"\n", "deny"),
        ("bare foreign receipt", json!({"type":"approval_receipt","request_id":"foreign"}).to_string()+"\n", "deny"),
        ("missing receipt id", json!({"type":"approval_receipt"}).to_string()+"\n", "unconfirmed"),
        ("empty receipt id", json!({"type":"approval_receipt","request_id":""}).to_string()+"\n", "unconfirmed"),
        ("valid unrelated user", json!({"type":"user","message":{"role":"user","content":"queued"}}).to_string()+"\n", "deny"),
        ("valid unrelated registry", json!({"type":"registry_request","request_id":"foreign","domain":"test","action":"list"}).to_string()+"\n", "deny"),
        ("current registry cannot replace approval", json!({"type":"registry_request","request_id":"req-0","domain":"test","action":"list","result":null}).to_string()+"\n", "unconfirmed"),
        ("nested current id cannot hide in user", json!({"type":"user","message":{"role":"user","content":"queued"},"response":{"request_id":"req-0"}}).to_string()+"\n", "unconfirmed"),
        ("valid interrupt", json!({"type":"control_request","request_id":"req-0","request":{"subtype":"interrupt"}}).to_string()+"\n", "interrupted"),
        ("unknown foreign kind", json!({"type":"unknown","request_id":"foreign"}).to_string()+"\n", "unconfirmed"),
        ("foreign control ignores unrelated top id shape", json!({"type":"control_response","request_id":42,"response":{"request_id":"foreign","response":42}}).to_string()+"\n", "deny"),
        ("current noninterrupt control request", json!({"type":"control_request","request_id":"req-0","request":{"subtype":"shutdown"}}).to_string()+"\n", "unconfirmed"),
        ("nested current id in unrelated registry", json!({"type":"registry_request","request_id":"foreign","domain":"test","action":"list","response":{"request_id":"req-0"}}).to_string()+"\n", "unconfirmed"),
        ("empty line", "\n".into(), "deny"),
        ("invalid json", "not-json\n".into(), "unconfirmed"),
    ];
    for kind in ["approval_receipt", "unsupported_approval_frame"] {
        cases.push(("execution-bearing non-decision frame",json!({"type":kind,"request_id":"req-0","result":{"llmContent":"host effect","metadata":{"exit_code":0}}}).to_string()+"\n","unconfirmed"));
    }
    let mut missing_subtype: Value = serde_json::from_str(&valid).unwrap();
    missing_subtype["response"]
        .as_object_mut()
        .unwrap()
        .remove("subtype");
    cases.push((
        "missing subtype",
        missing_subtype.to_string() + "\n",
        "unconfirmed",
    ));
    for body in [Value::Null, json!([])] {
        let mut malformed: Value = serde_json::from_str(&valid).unwrap();
        malformed["response"]["response"] = body;
        cases.push((
            "wrong body shape",
            malformed.to_string() + "\n",
            "unconfirmed",
        ));
    }
    let mut absent: Value = serde_json::from_str(&valid).unwrap();
    absent["response"]
        .as_object_mut()
        .unwrap()
        .remove("response");
    cases.push(("absent body", absent.to_string() + "\n", "unconfirmed"));
    cases.push((
        "empty request id",
        valid.replace("req-0", ""),
        "unconfirmed",
    ));
    cases.push((
        "duplicate type",
        valid.replace(
            "\"type\":\"control_response\"",
            "\"type\":\"user\",\"type\":\"control_response\"",
        ),
        "unconfirmed",
    ));
    cases.push((
        "duplicate subtype",
        valid.replace(
            "\"subtype\":\"success\"",
            "\"subtype\":\"error\",\"subtype\":\"success\"",
        ),
        "unconfirmed",
    ));
    matrix(cases, ExecutionProfile::Legacy, Some(ToolKind::ShellExec)).await;
}

#[tokio::test]
async fn shell_success_requires_consistent_terminal_evidence() {
    let mut mixed = shell(json!({"exit_code":0}));
    mixed["checkpointError"] = json!({"outcome":"uncertain","code":"unknown","message":"unknown"});
    let duplicate_exit = frame(shell(json!({"exit_code":0})))
        .replace("\"exit_code\":0", "\"exit_code\":1,\"exit_code\":0");
    let duplicate_status = frame(shell(json!({"exit_code":0,"status":"completed"}))).replace(
        "\"status\":\"completed\"",
        "\"status\":\"failed\",\"status\":\"completed\"",
    );
    let mut cases = vec![
        ("mixed family", frame(mixed), "unconfirmed"),
        ("duplicate exit", duplicate_exit, "unconfirmed"),
        ("duplicate status", duplicate_status, "unconfirmed"),
        (
            "legacy success",
            frame(shell(json!({"exit_code":0}))),
            "shell_success",
        ),
        (
            "complete success",
            frame(shell(
                json!({"exit_code":0,"status":"completed","signal":null,"input_wait":{"interrupted":false,"waited_secs":1},"output_ref":null,"approval_id":"shell-internal-id","tool_use_id":"provider-tool"}),
            )),
            "shell_success",
        ),
        (
            "unknown metadata",
            frame(shell(Value::Null)),
            "shell_unknown",
        ),
        ("missing exit", frame(shell(json!({}))), "shell_unknown"),
        (
            "null exit",
            frame(shell(json!({"exit_code":null}))),
            "shell_unknown",
        ),
        (
            "valid failed",
            frame(shell(json!({"exit_code":23,"status":"failed"}))),
            "shell_failure",
        ),
        (
            "failure diagnostic remains non-authoritative",
            frame(shell(
                json!({"exit_code":23,"status":{"future":"diagnostic"}}),
            )),
            "shell_failure",
        ),
        (
            "signal with zero",
            frame(shell(json!({"exit_code":0,"signal":"SIGINT"}))),
            "unconfirmed",
        ),
        (
            "input wait interrupted with zero",
            frame(shell(
                json!({"exit_code":0,"input_wait":{"interrupted":true}}),
            )),
            "unconfirmed",
        ),
    ];
    let mut invalid_display = shell(json!({"exit_code":0}));
    invalid_display["result"]["returnDisplay"] = json!(42);
    cases.push(("wrong returnDisplay", frame(invalid_display), "unconfirmed"));
    cases.push((
        "wrong interrupted type",
        frame(shell(
            json!({"exit_code":0,"input_wait":{"interrupted":"false"}}),
        )),
        "unconfirmed",
    ));
    for status in [
        json!("failed"),
        json!("interrupted"),
        json!("timed_out"),
        json!("not_executed"),
        json!("unknown"),
        Value::Null,
        json!(42),
    ] {
        cases.push((
            "zero contradicts status",
            frame(shell(json!({"exit_code":0,"status":status}))),
            "unconfirmed",
        ));
    }
    for code in [
        json!(4294967296_i64),
        json!(u64::MAX),
        json!("0"),
        json!(0.0),
    ] {
        cases.push((
            "invalid exit",
            frame(shell(json!({"exit_code":code}))),
            "unconfirmed",
        ));
    }
    matrix(cases, ExecutionProfile::Legacy, Some(ToolKind::ShellExec)).await;
}

#[tokio::test]
async fn checkpoint_variants_remain_typed_and_disjoint() {
    let created = frame(
        json!({"behavior":"host_executed_checkpoint_create","checkpointResult":checkpoint()}),
    );
    let duplicate_outcome = created.replace(
        "\"status\":\"created\"",
        "\"status\":\"skipped\",\"status\":\"created\"",
    );
    let duplicate_checkpoint = created.replace(
        "\"checkpoint_id\":",
        "\"checkpoint_id\":\"invalid\",\"checkpoint_id\":",
    );
    let mut mixed =
        json!({"behavior":"host_executed_checkpoint_create","checkpointResult":checkpoint()});
    mixed["checkpointError"] = Value::Null;
    let mut unknown = checkpoint();
    unknown["outcome"]["status"] = json!("unknown");
    matrix(vec![
        ("valid created",created,"checkpoint_success"),
        ("duplicate typed status",duplicate_outcome,"unconfirmed"),
        ("duplicate checkpoint id",duplicate_checkpoint,"unconfirmed"),
        ("mixed null error",frame(mixed),"unconfirmed"),
        ("unknown typed status",frame(json!({"behavior":"host_executed_checkpoint_create","checkpointResult":unknown})),"unconfirmed"),
        ("valid uncertain",frame(json!({"behavior":"host_executed_checkpoint_error","checkpointError":{"outcome":"uncertain","code":"unknown","message":"unconfirmed"}})),"checkpoint_failure"),
        ("typed plain deny",frame(json!({"behavior":"deny","message":"declined"})),"deny"),
        ("deny typed null",frame(json!({"behavior":"deny","checkpointResult":null})),"unconfirmed"),
        ("broker generic allow",frame(json!({"behavior":"allow"})),"unconfirmed"),
    ],ExecutionProfile::GatewayBrokeredCheckpointV1,Some(ToolKind::HostedSideEffect)).await;
    matrix(vec![
        ("typed wrong profile",frame(json!({"behavior":"host_executed_checkpoint_create","checkpointResult":checkpoint()})),"unconfirmed"),
        ("shell wrong tool",frame(shell(json!({"exit_code":0}))),"unconfirmed"),
    ],ExecutionProfile::Legacy,Some(ToolKind::FileEdit)).await;
}

#[tokio::test]
async fn prompt_approval_never_accepts_host_completion_as_plain_allow() {
    matrix(
        vec![
            (
                "prompt bare allow",
                frame(json!({"behavior":"allow"})),
                "allow",
            ),
            (
                "prompt bare deny",
                frame(json!({"behavior":"deny","message":null})),
                "deny",
            ),
            (
                "prompt execution-bearing allow",
                frame(json!({"behavior":"allow","result":null})),
                "unconfirmed",
            ),
            (
                "prompt host shell",
                frame(shell(json!({"exit_code":0}))),
                "unconfirmed",
            ),
        ],
        ExecutionProfile::Legacy,
        None,
    )
    .await;
}
