use super::*;
use std::path::Path;

const ORIGINAL: &str = "4e4e0c042a55e2dab8462d8e767b6eda\n";
const FORBIDDEN: &str = "FORBIDDEN-4e4e0c042a55e2dab8462d8e767b6eda\n";

fn call(name: &str, args: serde_json::Value) -> Vec<GenerateEvent> {
    vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: "call".into(),
            name: name.into(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: args.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 0 },
        GenerateEvent::MessageEnd,
    ]
}

fn core_for_calls(dir: &Path, calls: Vec<Vec<GenerateEvent>>) -> CoshCore {
    core_for_calls_with_tools(dir, calls, ToolRegistry::with_defaults_for_test())
}

fn core_for_calls_with_tools(
    dir: &Path,
    calls: Vec<Vec<GenerateEvent>>,
    tools: ToolRegistry,
) -> CoshCore {
    let mut responses = Vec::new();
    for (index, mut events) in calls.into_iter().enumerate() {
        if let Some(GenerateEvent::ToolCallStart { id, .. }) = events.first_mut() {
            *id = format!("call-{index}");
        }
        responses.push(events);
        responses.push(vec![GenerateEvent::MessageEnd]);
    }
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Auto;
    let mut core = CoshCore::new_legacy(config, Box::new(MockProvider::new(responses)), tools);
    core.project_root = dir.to_path_buf();
    core.workspace = SessionWorkspace::new(dir);
    core
}

fn shell_failure() -> Vec<GenerateEvent> {
    call(
        "shell",
        serde_json::json!({"command": "printf '%s\n' 4e4e0c042a55e2dab8462d8e767b6eda > effect; exit 23"}),
    )
}

fn file_write(content: &str) -> Vec<GenerateEvent> {
    call(
        "write_file",
        serde_json::json!({"path":"effect", "content":content}),
    )
}

fn decision(id: u32, behavior: &str) -> String {
    serde_json::json!({"type":"control_response", "response":{"subtype":"success", "request_id":format!("req-{id}"), "response":{"behavior":behavior, "message":"test decision"}}}).to_string()+"\n"
}

async fn turn(core: &mut CoshCore, input: &str) -> String {
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();
    core.handle_user_message("continue the current task", &mut reader, &mut output)
        .await
        .unwrap();
    String::from_utf8(output).unwrap()
}

#[tokio::test]
async fn failed_effect_reapproves_later_turns_and_ordinary_file_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(
        dir.path(),
        vec![
            shell_failure(),
            shell_failure(),
            call(
                "shell",
                serde_json::json!({"command":"printf changed > effect"}),
            ),
            file_write(FORBIDDEN),
        ],
    );
    turn(&mut core, &decision(0, "allow")).await;
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        ORIGINAL.as_bytes()
    );
    for id in 1..=3 {
        let output = turn(&mut core, &decision(id, "deny")).await;
        assert_eq!(
            std::fs::read(dir.path().join("effect")).unwrap(),
            ORIGINAL.as_bytes(),
            "req-{id}: {output}"
        );
        assert!(output.contains(&format!("req-{id}")), "{output}");
        assert!(output.contains("can_use_tool"), "{output}");
    }
}

#[tokio::test]
async fn failed_effect_allows_only_the_freshly_approved_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(
        dir.path(),
        vec![
            shell_failure(),
            file_write("approved\n"),
            file_write("after-confirmation\n"),
        ],
    );
    turn(&mut core, &decision(0, "allow")).await;
    let output = turn(&mut core, &decision(1, "allow")).await;
    assert!(output.contains("can_use_tool"), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        b"approved\n"
    );
    let output = turn(&mut core, "").await;
    assert!(!output.contains("can_use_tool"), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        b"after-confirmation\n"
    );
}

#[tokio::test]
async fn failed_effect_read_only_success_does_not_disarm_reapproval() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(
        dir.path(),
        vec![
            shell_failure(),
            call("read_file", serde_json::json!({"path":"effect"})),
            file_write(FORBIDDEN),
        ],
    );
    turn(&mut core, &decision(0, "allow")).await;
    let first_new_message = core.messages.len();
    let output = turn(&mut core, "").await;
    assert!(!output.contains("can_use_tool"), "{output}");
    assert!(core.messages[first_new_message..]
        .iter()
        .any(|m| m.role == "tool" && format!("{:?}", m.content).contains(ORIGINAL.trim())));
    let output = turn(&mut core, &decision(1, "deny")).await;
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        ORIGINAL.as_bytes(),
        "{output}"
    );
}

#[tokio::test]
async fn failed_effect_cancellation_never_executes_the_followup_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(dir.path(), vec![shell_failure(), file_write(FORBIDDEN)]);
    turn(&mut core, &decision(0, "allow")).await;
    let output = turn(&mut core, "{\"type\":\"control_request\",\"request_id\":\"cancel\",\"request\":{\"subtype\":\"interrupt\"}}\n").await;
    assert!(output.contains("can_use_tool"), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        ORIGINAL.as_bytes()
    );
    assert_eq!(
        core.classify_tool(
            "write_file",
            &serde_json::json!({"path":"effect", "content":FORBIDDEN})
        ),
        Outcome::RequireApproval
    );
}

#[tokio::test]
async fn failed_effect_host_failed_or_unknown_completion_never_replays_shell() {
    for exit_code in [Some(23), None] {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tools = ToolRegistry::with_defaults_for_test();
        tools.register(Box::new(CountingShellTool {
            calls: Arc::clone(&calls),
        }));
        let mut core = core_for_calls_with_tools(
            dir.path(),
            vec![shell_failure(), file_write(FORBIDDEN)],
            tools,
        );
        let status = std::process::Command::new("sh")
            .args([
                "-c",
                "printf '%s\n' 4e4e0c042a55e2dab8462d8e767b6eda > effect; exit 23",
            ])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(23));
        let input = serde_json::json!({"type":"control_response", "response":{"subtype":"success", "request_id":"req-0", "response":{"behavior":"host_executed_shell", "result":{"llmContent":"observed host side effect", "metadata":{"exit_code":exit_code}}}}}).to_string()+"\n";
        turn(&mut core, &input).await;
        let output = turn(&mut core, &decision(1, "deny")).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read(dir.path().join("effect")).unwrap(),
            ORIGINAL.as_bytes(),
            "{exit_code:?}: {output}"
        );
    }
}

#[tokio::test]
async fn ordinary_auto_write_without_failed_effect_stays_automatic() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(dir.path(), vec![file_write("ordinary\n")]);
    let output = turn(&mut core, "").await;
    assert!(!output.contains("can_use_tool"), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        b"ordinary\n"
    );
}

#[tokio::test]
async fn failed_read_only_tool_does_not_force_write_reapproval() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(
        dir.path(),
        vec![
            call("read_file", serde_json::json!({"path":"does-not-exist"})),
            file_write("ordinary\n"),
        ],
    );
    turn(&mut core, "").await;
    let output = turn(&mut core, "").await;
    assert!(!output.contains("can_use_tool"), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        b"ordinary\n"
    );
}

#[tokio::test]
async fn denied_unexecuted_shell_does_not_force_write_reapproval() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(dir.path(), vec![shell_failure(), file_write("ordinary\n")]);
    turn(&mut core, &decision(0, "deny")).await;
    assert!(!dir.path().join("effect").exists());
    let output = turn(&mut core, "").await;
    assert!(!output.contains("can_use_tool"), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        b"ordinary\n"
    );
}

#[tokio::test]
async fn failed_effect_wrong_host_identity_cannot_clear_reapproval() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(
        dir.path(),
        vec![
            shell_failure(),
            call(
                "shell",
                serde_json::json!({"command":"printf changed > effect"}),
            ),
            file_write(FORBIDDEN),
        ],
    );
    turn(&mut core, &decision(0, "allow")).await;
    let wrong = serde_json::json!({"type":"control_response", "response":{"subtype":"success", "request_id":"wrong-request", "response":{"behavior":"host_executed_shell", "result":{"llmContent":"must not be adopted", "metadata":{"exit_code":0}}}}}).to_string()+"\n";
    turn(&mut core, &(wrong + &decision(1, "deny"))).await;
    let output = turn(&mut core, &decision(2, "deny")).await;
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        ORIGINAL.as_bytes(),
        "{output}"
    );
    assert!(!format!("{:?}", core.messages).contains("must not be adopted"));
}

#[tokio::test]
async fn failed_effect_other_read_does_not_disarm_reapproval() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_for_calls(
        dir.path(),
        vec![
            shell_failure(),
            call("todo", serde_json::json!({"action":"list"})),
            file_write(FORBIDDEN),
        ],
    );
    turn(&mut core, &decision(0, "allow")).await;
    let first_new_message = core.messages.len();
    let output = turn(&mut core, "").await;
    assert!(!output.contains("can_use_tool"), "{output}");
    assert!(core.messages[first_new_message..]
        .iter()
        .any(|m| m.role == "tool" && format!("{:?}", m.content).contains("No items.")));
    let output = turn(&mut core, &decision(1, "deny")).await;
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        ORIGINAL.as_bytes(),
        "{output}"
    );
}

#[tokio::test]
async fn new_session_does_not_inherit_failed_effect_reapproval() {
    let dir = tempfile::tempdir().unwrap();
    let mut previous = core_for_calls(dir.path(), vec![shell_failure()]);
    turn(&mut previous, &decision(0, "allow")).await;
    assert_eq!(
        previous.classify_tool(
            "write_file",
            &serde_json::json!({"path":"effect", "content":FORBIDDEN})
        ),
        Outcome::RequireApproval
    );

    let mut fresh = core_for_calls(dir.path(), vec![file_write("fresh-session\n")]);
    assert_ne!(previous.session_id, fresh.session_id);
    let output = turn(&mut fresh, "").await;
    assert!(!output.contains("can_use_tool"), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        b"fresh-session\n"
    );
}

#[tokio::test]
async fn failed_effect_marks_followup_as_requiring_explicit_client_approval() {
    for mode in [ApprovalMode::Trust, ApprovalMode::Auto] {
        for next in [shell_failure(), file_write(FORBIDDEN)] {
            let dir = tempfile::tempdir().unwrap();
            let mut core = core_for_calls(dir.path(), vec![shell_failure(), next]);
            core.config.agent.approval_mode = mode;
            core.config.agent.allowed_tools.insert("write_file".into());
            core.client_capabilities = fully_capable_client();
            let failed = serde_json::json!({"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"host_executed_shell","result":{"llmContent":"failed after effect","metadata":{"exit_code":23}}}}}).to_string()+"\n";
            let first = turn(&mut core, &failed).await;
            let first_request = first
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find(|message| message["request"]["subtype"] == "can_use_tool")
                .unwrap();
            assert_ne!(first_request["request"]["hook_requires_approval"], true);
            let output = turn(&mut core, &decision(1, "deny")).await;
            let request = output
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find(|message| message["request"]["subtype"] == "can_use_tool")
                .unwrap();
            assert_eq!(
                request["request"]["hook_requires_approval"], true,
                "the Shell must not auto-approve a failed-effect followup: {request}"
            );
            assert!(!dir.path().join("effect").exists());
        }
    }
}

struct EffectWithoutCompletion {
    path: Option<std::path::PathBuf>,
    eof: bool,
}

impl tokio::io::AsyncRead for EffectWithoutCompletion {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(path) = self.path.take() {
            std::fs::write(path, ORIGINAL).unwrap();
        }
        if self.eof {
            std::task::Poll::Ready(Ok(()))
        } else {
            std::task::Poll::Pending
        }
    }
}

async fn assert_unknown_completion_requires_reapproval(eof: bool) {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::with_defaults_for_test();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&calls),
    }));
    let mut core = core_for_calls_with_tools(dir.path(), Vec::new(), tools);
    core.provider = Box::new(MockProvider::new(vec![
        shell_failure(),
        file_write(FORBIDDEN),
        vec![GenerateEvent::MessageEnd],
    ]));
    core.approval_response_timeout_default = Duration::from_millis(1);
    let mut reader = BufReader::new(EffectWithoutCompletion {
        path: Some(dir.path().join("effect")),
        eof,
    })
    .lines();
    let mut output = Vec::new();
    let result = core
        .handle_user_message("perform effect", &mut reader, &mut output)
        .await;
    assert_eq!(result.is_ok(), eof);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        ORIGINAL.as_bytes()
    );
    let output = turn(&mut core, &decision(1, "deny")).await;
    assert!(
        output.contains("can_use_tool"),
        "unknown completion silently reused automatic permission: {output}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        ORIGINAL.as_bytes()
    );
}

#[tokio::test]
async fn first_host_effect_with_eof_completion_requires_reapproval() {
    assert_unknown_completion_requires_reapproval(true).await;
}

#[tokio::test]
async fn first_host_effect_with_timed_out_completion_requires_reapproval() {
    assert_unknown_completion_requires_reapproval(false).await;
}

#[tokio::test]
async fn uncertain_approval_delivery_requires_reapproval() {
    for fail_on in [FailStep::PartialThenWrite, FailStep::Flush] {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core_for_calls(dir.path(), vec![shell_failure()]);
        let mut reader = BufReader::new(NeverReadStdin).lines();
        let result = core
            .handle_user_message(
                "perform effect",
                &mut reader,
                &mut FailingWriter::new(fail_on),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(
            core.classify_tool(
                "write_file",
                &serde_json::json!({"path":"effect", "content":FORBIDDEN})
            ),
            Outcome::RequireApproval
        );
        assert!(!dir.path().join("effect").exists());
    }
}

#[tokio::test]
async fn invalid_durable_checkpoint_prevents_tool_execution() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::with_defaults_for_test();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&calls),
    }));
    let mut core = core_for_calls_with_tools(dir.path(), vec![shell_failure()], tools);
    let store = crate::session::SessionStore::for_test_workspace(
        dir.path().join("sessions").to_str().unwrap(),
        dir.path(),
    )
    .unwrap();
    let id = crate::session::ProviderSessionId::parse(&core.session_id).unwrap();
    let lease = Arc::new(store.acquire_live_lease(&id).unwrap().unwrap());
    core.reapproval_checkpoint = Some(store.reapproval_checkpoint(&id, lease).unwrap());
    let path = store
        .session_file(&id)
        .parent()
        .unwrap()
        .join(format!(".{id}.live"));
    std::fs::rename(&path, path.with_extension("retained")).unwrap();
    std::fs::write(&path, [0]).unwrap();
    let input = decision(0, "allow");
    let mut lines = BufReader::new(input.as_bytes()).lines();
    let result = core
        .handle_user_message("perform effect", &mut lines, &mut Vec::new())
        .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!dir.path().join("effect").exists());
}
