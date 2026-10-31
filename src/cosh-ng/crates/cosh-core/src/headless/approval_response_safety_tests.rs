use super::*;
use serde_json::{json, Value};
use std::collections::VecDeque;

struct HostResponses {
    frames: VecDeque<(String, bool)>,
    current: Vec<u8>,
    offset: usize,
    effect: PathBuf,
}

impl tokio::io::AsyncRead for HostResponses {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        output: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        if self.offset == self.current.len() {
            let Some((frame, executed)) = self.frames.pop_front() else {
                return std::task::Poll::Ready(Ok(()));
            };
            if executed {
                std::fs::write(&self.effect, "host-effect").unwrap();
            }
            self.current = frame.into_bytes();
            self.offset = 0;
        }
        let count = output.remaining().min(self.current.len() - self.offset);
        output.put_slice(&self.current[self.offset..self.offset + count]);
        self.offset += count;
        std::task::Poll::Ready(Ok(()))
    }
}

fn response_for(id: &str, body: Value) -> String {
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":body}}).to_string()+"\n"
}

async fn host_turn(engine: &mut CoshCore, effect: PathBuf, frames: Vec<(String, bool)>) {
    let mut lines = BufReader::new(HostResponses {
        frames: frames.into(),
        current: Vec::new(),
        offset: 0,
        effect,
    })
    .lines();
    engine
        .handle_user_message("perform effect", &mut lines, &mut Vec::new())
        .await
        .unwrap();
}

fn two_effects() -> Vec<Vec<GenerateEvent>> {
    vec![
        tool_call("shell", json!({"command":"printf local-replay > effect"})),
        tool_call(
            "write_file",
            json!({"path":"effect","content":"unapproved-followup"}),
        ),
    ]
}

#[tokio::test]
async fn result_bearing_denials_cannot_disarm_or_allow_an_automatic_followup() {
    for field in ["result", "checkpointResult", "checkpointError"] {
        for value in [
            Value::Null,
            json!(42),
            json!({"llmContent":"host completed","metadata":{"exit_code":0}}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut config = CoreConfig::default();
            config.agent.approval_mode = ApprovalMode::Auto;
            let (mut runtime, mut engine) = fixture(
                dir.path(),
                config,
                ExecutionProfile::Legacy,
                ToolRegistry::with_defaults_for_test(),
                two_effects(),
            );
            runtime.persist(&mut engine).unwrap();
            let mut contradictory =
                json!({"behavior":"deny","message":"decision conflicts with execution"});
            contradictory[field] = value;
            host_turn(
                &mut engine,
                dir.path().join("effect"),
                vec![
                    (response(contradictory), true),
                    (response(json!({"behavior":"deny"})), false),
                ],
            )
            .await;
            runtime.persist(&mut engine).unwrap();
            turn(
                &mut engine,
                &response_for("req-1", json!({"behavior":"deny"})),
            )
            .await
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path().join("effect")).unwrap(),
                "host-effect",
                "{field}: unapproved followup overwrote the observed Host effect"
            );
            assert_durable_pending(&runtime, &engine);
        }
    }
}

#[tokio::test]
async fn malformed_correlated_frame_cannot_be_overwritten_by_later_bare_denial() {
    for first in [
        response(json!({"behavior":"deny","result":42})),
        response(json!({"behavior":"deny","message":42})),
        response(json!({"behavior":"deny"}))
            .replace("\"subtype\":\"success\"", "\"subtype\":\"error\""),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.agent.approval_mode = ApprovalMode::Auto;
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            config,
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            two_effects(),
        );
        runtime.persist(&mut engine).unwrap();
        host_turn(
            &mut engine,
            dir.path().join("effect"),
            vec![(first, true), (response(json!({"behavior":"deny"})), false)],
        )
        .await;
        runtime.persist(&mut engine).unwrap();
        turn(
            &mut engine,
            &response_for("req-1", json!({"behavior":"deny"})),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("effect")).unwrap(),
            "host-effect"
        );
        assert_durable_pending(&runtime, &engine);
    }
}

#[tokio::test]
async fn allow_with_host_result_never_reexecutes_the_command_locally() {
    let dir = tempfile::tempdir().unwrap();
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        CoreConfig::default(),
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        vec![tool_call(
            "shell",
            json!({"command":"printf local-replay > effect"}),
        )],
    );
    runtime.persist(&mut engine).unwrap();
    host_turn(&mut engine,dir.path().join("effect"),vec![(response(json!({"behavior":"allow","result":{"llmContent":"host already executed","metadata":{"exit_code":0}}})),true)]).await;
    runtime.persist(&mut engine).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("effect")).unwrap(),
        "host-effect",
        "an execution-bearing response must not authorize local replay"
    );
    assert_durable_pending(&runtime, &engine);
}

#[tokio::test]
async fn shell_zero_with_contradictory_checkpoint_error_keeps_prior_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        CoreConfig::default(),
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        vec![tool_call(
            "shell",
            json!({"command":"printf local-replay > effect"}),
        )],
    );
    runtime.persist(&mut engine).unwrap();
    engine.failed_side_effect_pending_approval = true;
    engine
        .reapproval_checkpoint
        .as_ref()
        .unwrap()
        .set_pending(true)
        .unwrap();
    host_turn(&mut engine,dir.path().join("effect"),vec![(response(json!({"behavior":"host_executed_shell","result":{"llmContent":"host completed","metadata":{"exit_code":0}},"checkpointError":{"outcome":"uncertain","code":"unknown","message":"outcome unknown"}})),true)]).await;
    runtime.persist(&mut engine).unwrap();
    assert_durable_pending(&runtime, &engine);
}

#[tokio::test]
async fn contradictory_bypass_denial_cannot_undo_new_guard() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = CoreConfig::default();
    config.hooks.enabled = true;
    config.hooks.post_tool_use_failure=vec![crate::config::HookDefinition {
        command:r#"printf '%s\n' '{"hookSpecificOutput":{"sandbox_bypass_request":{"original_command":"printf local-replay > effect","reason":"test bypass classification"}}}'"#.into(),
        name:Some("response-classification-bypass".into()),matcher:None,timeout:Some(10_000),sequential:None,fail_open:false,env:Default::default(),
    }];
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        config,
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        vec![tool_call(
            "shell",
            json!({"command":"printf local-replay > effect"}),
        )],
    );
    runtime.persist(&mut engine).unwrap();
    host_turn(&mut engine,dir.path().join("effect"),vec![(response(json!({"behavior":"deny"})),false),(response_for("req-1",json!({"behavior":"deny","result":{"llmContent":"host performed bypass","metadata":{"exit_code":0}}})),true)]).await;
    runtime.persist(&mut engine).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("effect")).unwrap(),
        "host-effect"
    );
    assert_durable_pending(&runtime, &engine);
}

async fn assert_non_decision_frame_keeps_host_execution_guard(kind: &str) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Auto;
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        config,
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        two_effects(),
    );
    runtime.persist(&mut engine).unwrap();
    let contradictory=json!({"type":kind,"request_id":"req-0","result":{"llmContent":"host performed effect","metadata":{"exit_code":0}}}).to_string()+"\n";
    host_turn(
        &mut engine,
        dir.path().join("effect"),
        vec![
            (contradictory, true),
            (response(json!({"behavior":"deny"})), false),
        ],
    )
    .await;
    runtime.persist(&mut engine).unwrap();
    turn(
        &mut engine,
        &response_for("req-1", json!({"behavior":"deny"})),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("effect")).unwrap(),
        "host-effect",
        "{kind}: receipt/unknown frame hid a Host effect and allowed automatic followup"
    );
    assert_durable_pending(&runtime, &engine);
}

#[tokio::test]
async fn receipt_cannot_hide_host_execution_before_bare_denial() {
    assert_non_decision_frame_keeps_host_execution_guard("approval_receipt").await;
}

#[tokio::test]
async fn unsupported_frame_cannot_hide_host_execution_before_bare_denial() {
    assert_non_decision_frame_keeps_host_execution_guard("unsupported_approval_frame").await;
}

#[path = "approval_exposure_tests.rs"]
mod approval_exposure_tests;
