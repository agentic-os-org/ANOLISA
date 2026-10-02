use super::*;
use crate::provider::mock::MockProvider;
use crate::tool::{Tool, ToolResult};
use async_trait::async_trait;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::io::BufReader;

async fn empty_reader() -> tokio::io::Lines<BufReader<&'static [u8]>> {
    BufReader::new(&b""[..]).lines()
}

fn make_core(provider: MockProvider) -> CoshCore {
    make_core_with_profile(provider, crate::cli::ExecutionProfile::Legacy)
}

fn make_core_with_profile(
    provider: MockProvider,
    execution_profile: crate::cli::ExecutionProfile,
) -> CoshCore {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::new();
    CoshCore::new_with_profile(config, Box::new(provider), tools, execution_profile)
}

#[test]
fn hook_failure_audit_is_distinct_from_real_allow() {
    let allow = crate::hook::PreToolUseResult {
        decision: crate::hook::HookDecision::Allow,
        tool_input_patch: None,
        notifications: Vec::new(),
        hook_failures: Vec::new(),
    };
    assert_eq!(
        pre_tool_hook_audit(&allow),
        (cosh_types::audit::AuditOutcomeStatus::Allowed, "allow")
    );

    let fail_open = crate::hook::PreToolUseResult {
        decision: crate::hook::HookDecision::Allow,
        tool_input_patch: None,
        notifications: Vec::new(),
        hook_failures: vec![crate::hook::HookFailure {
            hook_name: "probe".to_string(),
            kind: crate::hook::HookFailureKind::InvalidJson,
        }],
    };
    assert_eq!(
        pre_tool_hook_audit(&fail_open),
        (
            cosh_types::audit::AuditOutcomeStatus::Failed,
            "hook_failure"
        )
    );

    let blocked_with_fail_open_failure = crate::hook::PreToolUseResult {
        decision: crate::hook::HookDecision::Block("policy denied".to_string()),
        ..fail_open.clone()
    };
    assert_eq!(
        pre_tool_hook_audit(&blocked_with_fail_open_failure),
        (cosh_types::audit::AuditOutcomeStatus::Denied, "block")
    );

    let ask_with_fail_open_failure = crate::hook::PreToolUseResult {
        decision: crate::hook::HookDecision::Ask,
        ..fail_open
    };
    assert_eq!(
        pre_tool_hook_audit(&ask_with_fail_open_failure),
        (cosh_types::audit::AuditOutcomeStatus::Started, "ask")
    );
}

struct CountingShellTool {
    calls: Arc<AtomicUsize>,
}

struct ExternalTool;

/// Records the `GenerateConfig` of every agent-turn request it serves.
struct ConfigRecordingProvider {
    configs: Arc<Mutex<Vec<crate::provider::GenerateConfig>>>,
}

#[async_trait]
impl crate::provider::ContentGenerator for ConfigRecordingProvider {
    async fn generate(
        &self,
        _messages: &[crate::provider::Message],
        _tools: &[crate::provider::ToolDeclaration],
        config: &crate::provider::GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        self.configs.lock().unwrap().push(config.clone());
        Ok(Box::pin(futures::stream::iter([
            crate::provider::GenerateEvent::TextDelta("done".to_string()),
            crate::provider::GenerateEvent::MessageEnd,
        ])))
    }

    fn cancel(&self) {}
}

/// Runs one turn against `model` and returns the `max_tokens` actually sent
/// together with the output reserve the compaction budget charged for it.
async fn requested_and_reserved_output_tokens(
    model: &str,
    compaction: crate::config::CompactionConfig,
) -> (u32, u64) {
    let configs = Arc::new(Mutex::new(Vec::new()));
    let provider = ConfigRecordingProvider {
        configs: Arc::clone(&configs),
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.session.compaction = compaction.clone();
    let session_token_limit = config.agent.session_token_limit;
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());
    core.model = model.to_string();
    let mut reader = empty_reader().await;
    let mut output = Vec::new();
    core.handle_user_message("hello", &mut reader, &mut output)
        .await
        .expect("turn completes");

    let capability =
        crate::compaction::ModelCapability::resolve(&compaction, session_token_limit, model);
    let budget = crate::compaction::ContextBudget::compute(capability, 1_000, &compaction);
    let sent = configs.lock().unwrap();
    assert_eq!(sent.len(), 1, "exactly one provider request per turn");
    (sent[0].max_tokens, budget.output_reserve)
}

#[tokio::test]
async fn request_max_tokens_matches_the_reserved_output_budget() {
    // #2240: the request used to ask for the model's whole 65 536-token output
    // capability while the budget reserved the same amount, leaving a
    // 131 072-token window with only ~52K of usable history. Both sides now read
    // one resolver, so they can never disagree.
    let (requested, reserved) = requested_and_reserved_output_tokens(
        "qwen3.7-max",
        crate::config::CompactionConfig::default(),
    )
    .await;
    assert_eq!(requested, 16_384, "max_tokens actually sent");
    assert_eq!(u64::from(requested), reserved);

    // An unknown model shares the conservative default on both sides.
    let (requested, reserved) = requested_and_reserved_output_tokens(
        "brand-new-model",
        crate::config::CompactionConfig::default(),
    )
    .await;
    assert_eq!(requested, 4_096);
    assert_eq!(u64::from(requested), reserved);

    // An explicit override raises both, never only one.
    let overridden = crate::config::CompactionConfig {
        model_max_output_tokens: Some(24_000),
        ..Default::default()
    };
    let (requested, reserved) =
        requested_and_reserved_output_tokens("qwen3.7-max", overridden).await;
    assert_eq!(requested, 24_000);
    assert_eq!(u64::from(requested), reserved);
}

#[derive(Default)]
struct RecordingProvider {
    messages: Arc<Mutex<Vec<crate::provider::Message>>>,
    tools: Arc<Mutex<Vec<crate::provider::ToolDeclaration>>>,
}

#[async_trait]
impl crate::provider::ContentGenerator for RecordingProvider {
    async fn generate(
        &self,
        messages: &[crate::provider::Message],
        tools: &[crate::provider::ToolDeclaration],
        _config: &crate::provider::GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        *self.messages.lock().unwrap() = messages.to_vec();
        *self.tools.lock().unwrap() = tools.to_vec();
        Ok(Box::pin(futures::stream::iter([
            crate::provider::GenerateEvent::TextDelta("done".to_string()),
            crate::provider::GenerateEvent::MessageEnd,
        ])))
    }

    fn cancel(&self) {}
}

#[async_trait]
impl Tool for ExternalTool {
    fn name(&self) -> &str {
        "example.ops/mcp/server/tool"
    }

    fn description(&self) -> &str {
        "external tool"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }

    fn kind(&self) -> ToolKind {
        ToolKind::External
    }

    async fn invoke(
        &self,
        _params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        Ok(ToolResult::success("unused"))
    }
}

#[test]
fn allowlisted_tools_bypass_strict_approval() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    config.agent.allowed_tools.insert("shell".to_string());
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let core = CoshCore::new_legacy(config, Box::new(MockProvider::new(Vec::new())), tools);

    assert_eq!(
        core.classify_tool("shell", &serde_json::json!({})),
        Outcome::Allow
    );
}

#[test]
fn sensitive_write_requires_auto_approval_but_preserves_bypass_modes() {
    let sensitive = serde_json::json!({
        "path": "settings.env",
        "content": "AWS_ACCESS_KEY_ID=AKIA1234567890ABCDEF"
    });
    let ordinary = serde_json::json!({"path": "settings.env", "content": "safe=true"});

    for (mode, expected) in [
        ("trust", Outcome::Allow),
        ("auto", Outcome::RequireApproval),
        ("balanced", Outcome::RequireApproval),
        ("suggest", Outcome::RequireApproval),
        ("strict", Outcome::RequireApproval),
    ] {
        let mut config = CoreConfig::default();
        config.agent.approval_mode = ApprovalMode::from_config(mode);
        let core = CoshCore::new_legacy(
            config,
            Box::new(MockProvider::new(Vec::new())),
            ToolRegistry::with_defaults_for_test(),
        );

        assert_eq!(
            core.classify_tool("write_file", &sensitive),
            expected,
            "unexpected sensitive write policy in {mode} mode"
        );
    }

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Auto;
    let core = CoshCore::new_legacy(
        config,
        Box::new(MockProvider::new(Vec::new())),
        ToolRegistry::with_defaults_for_test(),
    );
    assert_eq!(
        core.classify_tool("write_file", &ordinary),
        Outcome::Allow,
        "ordinary auto-mode writes remain allowed"
    );

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    config.agent.allowed_tools.insert("write_file".to_string());
    let core = CoshCore::new_legacy(
        config,
        Box::new(MockProvider::new(Vec::new())),
        ToolRegistry::with_defaults_for_test(),
    );
    assert_eq!(
        core.classify_tool("write_file", &sensitive),
        Outcome::Allow,
        "explicit allowlist entries remain authoritative"
    );
}

#[tokio::test]
async fn sensitive_write_audit_uses_generic_execution_path() {
    let dir = tempfile::tempdir().unwrap();
    let secret = "AKIA1234567890ABCDEF";
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-sensitive-write".to_string(),
                name: "write_file".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: format!(
                    r#"{{"path":"settings.env","content":"AWS_ACCESS_KEY_ID={secret}"}}"#
                ),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("done".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut core = CoshCore::new_legacy(
        config,
        Box::new(provider),
        ToolRegistry::with_defaults_for_test(),
    );
    core.project_root = dir.path().to_path_buf();
    core.workspace = crate::tool::SessionWorkspace::new(dir.path());
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);

    let mut reader = empty_reader().await;
    let mut output = Vec::new();
    core.handle_user_message("write the file", &mut reader, &mut output)
        .await
        .expect("sensitive write turn");

    let event = core
        .audit
        .captured_events()
        .iter()
        .find(|event| {
            event.event_type.as_str() == "tool.requested"
                && event.identity.tool_use_id.as_deref() == Some("call-sensitive-write")
        })
        .expect("sensitive write audit event");
    let serialized = serde_json::to_value(event).unwrap();
    assert_eq!(serialized["data"]["execution_path"], "sensitive_write");
    assert!(!serialized.to_string().contains(secret));
}

#[test]
fn mcp_tools_require_approval_outside_trust_mode() {
    for mode in [ApprovalMode::Auto, ApprovalMode::Recommend] {
        let mut config = CoreConfig::default();
        config.agent.approval_mode = mode;
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(TestMcpTool));
        let core = CoshCore::new_legacy(config, Box::new(MockProvider::new(Vec::new())), tools);

        assert_eq!(
            core.classify_tool("mcp__remote__search", &serde_json::json!({})),
            Outcome::RequireApproval,
            "MCP tool should require approval in {mode} mode"
        );
    }
}

#[test]
fn exact_mcp_allowlist_entry_bypasses_approval() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    config
        .agent
        .allowed_tools
        .insert("mcp__remote__search".to_string());
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(TestMcpTool));
    let core = CoshCore::new_legacy(config, Box::new(MockProvider::new(Vec::new())), tools);

    assert_eq!(
        core.classify_tool("mcp__remote__search", &serde_json::json!({})),
        Outcome::Allow
    );
}

#[test]
fn external_tools_require_approval_outside_trust_mode() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ExternalTool));
    let mut core = CoshCore::new_legacy(config, Box::new(MockProvider::text_only("unused")), tools);
    for mode in [ApprovalMode::Auto, ApprovalMode::Recommend] {
        core.config.agent.approval_mode = mode;
        assert_eq!(
            core.classify_tool("example.ops/mcp/server/tool", &serde_json::json!({})),
            Outcome::RequireApproval
        );
    }
    core.config.agent.approval_mode = ApprovalMode::Trust;
    assert_eq!(
        core.classify_tool("example.ops/mcp/server/tool", &serde_json::json!({})),
        Outcome::Allow
    );
}

#[test]
fn approval_mode_covers_every_registered_tool_kind() {
    for mode in [
        ApprovalMode::Recommend,
        ApprovalMode::Auto,
        ApprovalMode::Trust,
    ] {
        let mut config = CoreConfig::default();
        config.agent.approval_mode = mode;
        let mut tools = ToolRegistry::with_defaults_for_test().with_shell_evidence();
        tools.register(Box::new(TestMcpTool));
        tools.register(Box::new(ExternalTool));
        let core = CoshCore::new_legacy(config, Box::new(MockProvider::new(Vec::new())), tools);

        for (name, kind, recommend, auto) in [
            (
                "read_file",
                ToolKind::ReadOnly,
                Outcome::Allow,
                Outcome::Allow,
            ),
            ("grep", ToolKind::ReadOnly, Outcome::Allow, Outcome::Allow),
            (
                "web_fetch",
                ToolKind::Network,
                Outcome::RequireApproval,
                Outcome::RequireApproval,
            ),
            (
                "edit",
                ToolKind::FileEdit,
                Outcome::RequireApproval,
                Outcome::Allow,
            ),
            (
                "save_memory",
                ToolKind::FileEdit,
                Outcome::RequireApproval,
                Outcome::Allow,
            ),
            (
                "shell",
                ToolKind::ShellExec,
                Outcome::RequireApproval,
                Outcome::RequireApproval,
            ),
            (
                "cosh_shell_evidence",
                ToolKind::ShellEvidence,
                Outcome::RequireApproval,
                Outcome::Allow,
            ),
            (
                "mcp__remote__search",
                ToolKind::Mcp,
                Outcome::RequireApproval,
                Outcome::RequireApproval,
            ),
            (
                "example.ops/mcp/server/tool",
                ToolKind::External,
                Outcome::RequireApproval,
                Outcome::RequireApproval,
            ),
            (
                "todo",
                ToolKind::Other,
                Outcome::RequireApproval,
                Outcome::Allow,
            ),
            (
                "skill",
                ToolKind::Other,
                Outcome::RequireApproval,
                Outcome::Allow,
            ),
        ] {
            assert_eq!(core.tools.get(name).expect("registered tool").kind(), kind);
            let expected = match mode {
                ApprovalMode::Recommend => recommend,
                ApprovalMode::Auto => auto,
                ApprovalMode::Trust => Outcome::Allow,
            };
            assert_eq!(
                core.classify_tool(name, &serde_json::json!({})),
                expected,
                "unexpected {kind:?} policy in {mode} mode"
            );
        }
    }
}

#[test]
fn unknown_tools_are_denied_in_every_approval_mode() {
    for mode in [
        ApprovalMode::Recommend,
        ApprovalMode::Auto,
        ApprovalMode::Trust,
    ] {
        let mut config = CoreConfig::default();
        config.agent.approval_mode = mode;
        config
            .agent
            .allowed_tools
            .insert("unknown_provider_tool".to_string());
        let core = CoshCore::new_legacy(
            config,
            Box::new(MockProvider::new(Vec::new())),
            ToolRegistry::with_defaults_for_test(),
        );

        assert_eq!(
            core.classify_tool("unknown_provider_tool", &serde_json::json!({})),
            Outcome::Deny,
            "unknown tool must fail closed in {mode} mode"
        );
    }
}

#[test]
fn safe_reload_rebinds_the_complete_snapshot_before_the_next_run() {
    let mut core = make_core(MockProvider::text_only("unused"));
    let previous = core.extension_generation.current().generation.id;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ExternalTool));
    let candidate = RuntimeSnapshot::bootstrap(
        RuntimeGeneration::healthy(previous + 1, "candidate"),
        Arc::new(tools),
    );

    core.extension_generation.stage(candidate);
    assert_eq!(
        core.extension_generation.reload(),
        crate::extension::generation::ReloadOutcome::Activated
    );
    assert!(core.tools.get("example.ops/mcp/server/tool").is_none());

    core.bind_current_extension_snapshot();

    assert_eq!(core.bound_extension_generation, previous + 1);
    assert!(core.tools.get("example.ops/mcp/server/tool").is_some());
    assert_eq!(core.extension_generation.take_retired().len(), 1);
}

#[test]
fn web_fetch_requires_approval_outside_trust_mode() {
    for (mode, expected) in [
        (ApprovalMode::Trust, Outcome::Allow),
        (ApprovalMode::Auto, Outcome::RequireApproval),
        (ApprovalMode::Recommend, Outcome::RequireApproval),
    ] {
        let mut config = CoreConfig::default();
        config.agent.approval_mode = mode;
        let tools = ToolRegistry::with_defaults_for_test();
        let core = CoshCore::new_legacy(config, Box::new(MockProvider::new(Vec::new())), tools);

        assert_eq!(
            core.classify_tool("web_fetch", &serde_json::json!({})),
            expected,
            "unexpected web_fetch policy in {mode} mode"
        );
    }
}

#[tokio::test]
async fn project_context_reaches_the_provider_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let context_dir = dir.path().join(".copilot-shell");
    std::fs::create_dir(&context_dir).unwrap();
    std::fs::write(context_dir.join("CONTEXT.md"), "provider-visible marker").unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        messages: Arc::clone(&captured),
        ..RecordingProvider::default()
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());
    core.shell_context = Some(ShellContext {
        cwd: dir.path().to_path_buf(),
        env: std::collections::HashMap::new(),
        last_exit_code: 0,
    });
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("hello", &mut reader, &mut output)
        .await
        .unwrap();

    let messages = captured.lock().unwrap();
    let system = messages.first().expect("provider system message");
    assert_eq!(system.role, "system");
    assert!(system.content.as_text().contains("# Context"));
    assert!(system
        .content
        .as_text()
        .contains("## Project Context\nprovider-visible marker"));
}

#[tokio::test]
async fn provider_session_identity_stays_out_of_the_system_prompt() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        messages: Arc::clone(&captured),
        ..RecordingProvider::default()
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());
    let provider_session_id = core.session_id.clone();
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("hello", &mut reader, &mut output)
        .await
        .unwrap();

    let messages = captured.lock().unwrap();
    let prompt = messages
        .first()
        .expect("provider system message")
        .content
        .as_text();
    assert!(!prompt.contains(&provider_session_id));
    assert!(!prompt.contains("provider_session_id"));
}

#[tokio::test]
async fn runtime_context_tool_reads_live_core_state_on_demand() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    config.hooks.enabled = true;
    config.ai.active_provider = Some("coding".to_string());
    config.ai.providers.insert(
        "coding".to_string(),
        crate::config::ProviderConfig {
            provider_type: Some("dashscope".to_string()),
            model: Some("qwen-test".to_string()),
            ..Default::default()
        },
    );
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(MockProvider::new(Vec::new())), tools);
    core.set_session_resumed(true);
    core.compaction.load_state(None, 7);
    // Config reload does not rebuild the bound provider or hook system. The
    // runtime contract therefore omits those config-only identities.
    core.config.ai.active_provider = Some("reloaded-provider".to_string());
    core.config.hooks.enabled = false;
    let context = ToolContext::with_runtime(
        core.cwd(),
        core.session_id.clone(),
        core.project_root.clone(),
        core.workspace.clone(),
        core.tool_runtime_context(),
    );

    let result = core
        .tools
        .get("runtime_context")
        .expect("default runtime_context tool")
        .invoke(serde_json::json!({}), &context)
        .await
        .expect("runtime context output");
    let output: serde_json::Value =
        serde_json::from_str(&result.output).expect("runtime context JSON");

    assert_eq!(output["provider_session_id"], core.session_id);
    assert_eq!(output["model"], "qwen-test");
    assert!(output.get("provider").is_none());
    assert_eq!(output["approval_mode"], "recommend");
    assert_eq!(output["session"]["resumed"], true);
    assert_eq!(output["compaction"]["revision"], 7);
    assert!(output["capabilities"]["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|name| name == "runtime_context")));
    assert!(output["capabilities"].get("hooks_enabled").is_none());
}

#[tokio::test]
async fn raw_shell_input_reaches_prompt_hook_without_changing_provider_content() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        messages: Arc::clone(&captured),
        ..RecordingProvider::default()
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.hooks.enabled = true;
    config.hooks.user_prompt_submit = vec![crate::config::HookDefinition {
        command: r#"python3 -c 'import json,sys; p=json.load(sys.stdin)["prompt"]; expected="user_input: marker\nruntime_frame: marker\ncosh-shell Agent contract: marker\napi_key=<redacted>"; print(json.dumps({"decision":"allow" if p == expected and not p.startswith("Handle this natural-language shell prompt") else "block"}))'"#
            .to_string(),
        name: Some("raw-input-probe".to_string()),
        matcher: None,
        timeout: Some(5_000),
        sequential: None,
        fail_open: false,
        env: Default::default(),
    }];
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());
    let envelope = "Handle this natural-language shell prompt.\n\nuser_input: marker\nruntime_frame: marker\ncosh-shell Agent contract: marker";
    let raw = "user_input: marker\nruntime_frame: marker\ncosh-shell Agent contract: marker\napi_key=sk-raw-hook-secret";
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message_with_raw_input(envelope, Some(raw), &mut reader, &mut output)
        .await
        .expect("raw-input turn");

    let messages = captured.lock().unwrap();
    let user_message = messages
        .iter()
        .find(|message| message.role == "user")
        .expect("provider user message");
    assert_eq!(user_message.content.as_text(), envelope);
}

#[tokio::test]
async fn prompt_hook_falls_back_to_content_without_raw_shell_input() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        messages: Arc::clone(&captured),
        ..RecordingProvider::default()
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.hooks.enabled = true;
    config.hooks.user_prompt_submit = vec![crate::config::HookDefinition {
        command: r#"python3 -c 'import json,sys; p=json.load(sys.stdin)["prompt"]; print(json.dumps({"decision":"allow" if p == "legacy\nuser_input: marker" else "block"}))'"#
            .to_string(),
        name: Some("legacy-input-probe".to_string()),
        matcher: None,
        timeout: Some(5_000),
        sequential: None,
        fail_open: false,
        env: Default::default(),
    }];
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());
    let content = "legacy\nuser_input: marker";
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message(content, &mut reader, &mut output)
        .await
        .expect("legacy-input turn");

    let messages = captured.lock().unwrap();
    let user_message = messages
        .iter()
        .find(|message| message.role == "user")
        .expect("provider user message");
    assert_eq!(user_message.content.as_text(), content);
}

#[test]
fn shell_cwd_does_not_replace_the_fixed_project_root() {
    let mut core = CoshCore::new_legacy(
        CoreConfig::default(),
        Box::new(MockProvider::new(Vec::new())),
        ToolRegistry::new(),
    );
    let project_root = core.project_root.clone();
    let directory = tempfile::tempdir().unwrap();

    core.shell_context = Some(ShellContext {
        cwd: directory.path().to_path_buf(),
        env: std::collections::HashMap::new(),
        last_exit_code: 0,
    });

    assert_eq!(core.project_root, project_root);
    assert_eq!(core.cwd(), directory.path());
}

#[test]
fn fixed_project_root_is_the_cwd_without_shell_context() {
    let mut core = CoshCore::new_legacy(
        CoreConfig::default(),
        Box::new(MockProvider::new(Vec::new())),
        ToolRegistry::new(),
    );
    let project_root = tempfile::tempdir().unwrap();
    core.project_root = project_root.path().to_path_buf();
    core.shell_context = None;

    assert_eq!(core.cwd(), project_root.path());
}

#[tokio::test]
async fn user_provided_secret_reaches_the_provider_boundary() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        messages: Arc::clone(&captured),
        ..RecordingProvider::default()
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());
    let mut reader = empty_reader().await;
    let mut output = Vec::new();
    let secret = "sk-user-provided-secret-value";

    core.handle_user_message(
        &format!("write api_key={secret} to the config"),
        &mut reader,
        &mut output,
    )
    .await
    .unwrap();

    let messages = captured.lock().unwrap();
    let user_message = messages
        .iter()
        .find(|message| message.role == "user")
        .expect("provider user message");
    assert!(user_message.content.as_text().contains(secret));
}

fn find_declaration<'a>(
    declarations: &'a [crate::provider::ToolDeclaration],
    name: &str,
) -> &'a crate::provider::ToolDeclaration {
    declarations
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("missing '{name}' declaration"))
}

/// A BeforeModel hook that rewrites every tool `description` to `compressed`
/// and strips schema properties, mirroring tokenless schema compression.
fn compress_schema_hook(command: &str) -> crate::config::HookDefinition {
    crate::config::HookDefinition {
        command: command.to_string(),
        name: Some("compress-schema".to_string()),
        matcher: None,
        timeout: Some(10_000),
        sequential: None,
        fail_open: false,
        env: Default::default(),
    }
}

#[tokio::test]
async fn before_model_hook_rewrites_tool_declarations_for_one_provider_call() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        tools: Arc::clone(&captured),
        ..RecordingProvider::default()
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.hooks.enabled = true;
    config.hooks.before_model = vec![compress_schema_hook(
        r#"python3 -c '
import json, sys
payload = json.load(sys.stdin)
tools = payload["llm_request"]["config"]["tools"]
for tool in tools:
    tool["description"] = "compressed"
    tool["parameters"] = {"type": "object", "properties": {"api_key": {"type": "string"}}}
print(json.dumps({"hookSpecificOutput": {"llm_request": {"config": {"tools": tools}}}}))
'"#,
    )];
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("hello", &mut reader, &mut output)
        .await
        .unwrap();

    let recorded = captured.lock().unwrap();
    let shell = find_declaration(&recorded, "shell");
    assert_eq!(shell.description, "compressed");
    // The schema property named `api_key` is a declaration, not a secret:
    // redaction must not have collapsed it into a "<redacted>" string.
    assert_eq!(shell.parameters["properties"]["api_key"]["type"], "string");

    // The registry is the source of truth for the next turn and stays intact.
    let declarations = core.tools.declarations();
    let original = find_declaration(&declarations, "shell");
    assert_eq!(original.description, "counting shell");
    assert!(original.parameters["properties"].get("command").is_some());
}

#[tokio::test]
async fn before_model_hook_rejecting_tool_set_changes_keeps_originals() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        tools: Arc::clone(&captured),
        ..RecordingProvider::default()
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.hooks.enabled = true;
    // Appending an undeclared tool changes tool-selection semantics, so the
    // whole array is discarded rather than partially applied.
    config.hooks.before_model = vec![compress_schema_hook(
        r#"python3 -c '
import json, sys
payload = json.load(sys.stdin)
tools = payload["llm_request"]["config"]["tools"]
tools.append({"name": "smuggled", "description": "x", "parameters": {"type": "object"}})
print(json.dumps({"hookSpecificOutput": {"llm_request": {"config": {"tools": tools}}}}))
'"#,
    )];
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("hello", &mut reader, &mut output)
        .await
        .unwrap();

    let recorded = captured.lock().unwrap();
    assert_eq!(
        find_declaration(&recorded, "shell").description,
        "counting shell"
    );
    assert!(recorded.iter().all(|tool| tool.name != "smuggled"));
}

#[async_trait]
impl Tool for CountingShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "counting shell"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ShellExec
    }

    async fn invoke(
        &self,
        _params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success("provider-native shell executed"))
    }
}

struct TestMcpTool;

#[async_trait]
impl Tool for TestMcpTool {
    fn name(&self) -> &str {
        "mcp__remote__search"
    }

    fn description(&self) -> &str {
        "test MCP tool"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mcp
    }

    async fn invoke(
        &self,
        _params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        Ok(ToolResult::success("called"))
    }
}

struct CountingMcpTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingMcpTool {
    fn name(&self) -> &str {
        "mcp__remote__search"
    }

    fn description(&self) -> &str {
        "counting MCP tool"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mcp
    }

    async fn invoke(
        &self,
        _params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success("called"))
    }
}

fn mcp_tool_provider() -> MockProvider {
    MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "mcp__remote__search".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: "{}".to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Done.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ])
}

#[tokio::test]
async fn mcp_tools_do_not_execute_before_approval() {
    for mode in [ApprovalMode::Auto, ApprovalMode::Recommend] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut config = CoreConfig::default();
        config.agent.approval_mode = mode;
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(CountingMcpTool {
            calls: Arc::clone(&calls),
        }));
        let mut core = CoshCore::new_legacy(config, Box::new(mcp_tool_provider()), tools);
        let deny = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"deny"}}}"#;
        let mut reader = BufReader::new(deny.as_bytes()).lines();
        let mut output = Vec::new();

        core.handle_user_message("search", &mut reader, &mut output)
            .await
            .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "MCP tool ran in {mode} mode"
        );
        assert!(String::from_utf8(output).unwrap().contains("can_use_tool"));
    }
}

#[tokio::test]
async fn text_only_response() {
    let provider = MockProvider::text_only("Hello from AI!");
    let mut core = make_core(provider);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    core.handle_user_message("hi", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains("Hello from AI!"));
    assert_eq!(core.messages.len(), 2);
}

#[tokio::test]
async fn provider_eof_without_terminal_fails_the_request_and_turn() {
    let provider = MockProvider::new(vec![vec![GenerateEvent::TextDelta("partial".to_string())]]);
    let mut core = make_core(provider);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    let result = core
        .handle_user_message("hi", &mut reader, &mut output)
        .await;

    assert!(result.is_err());
    let event_types = core.audit.captured_event_types();
    assert!(event_types.contains(&"provider.request.failed"));
    assert!(event_types.contains(&"turn.failed"));
    assert!(!event_types.contains(&"provider.request.completed"));
}

/// Pending-call state is sized from the provider's index, so an out-of-range
/// index must fail the turn rather than allocate a slot per reported position.
#[tokio::test]
async fn out_of_range_tool_call_index_fails_the_turn() {
    for index in [MAX_TOOL_CALL_INDEX + 1, u32::MAX] {
        let provider = MockProvider::new(vec![vec![
            GenerateEvent::ToolCallStart {
                index,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index,
                arguments_delta: r#"{"command":"ls"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index },
            GenerateEvent::MessageEnd,
        ]]);
        let mut core = make_core(provider);
        core.audit = CoreAuditRecorder::test_capture(&core.session_id);
        let mut output = Vec::new();
        let mut reader = empty_reader().await;

        let error = core
            .handle_user_message("hi", &mut reader, &mut output)
            .await
            .expect_err("index {index} must fail the turn");
        assert!(error.contains(&index.to_string()), "{error}");
        assert!(
            error.contains(&MAX_TOOL_CALL_INDEX.to_string()),
            "the limit must be named: {error}"
        );
        assert!(
            core.audit.captured_event_types().contains(&"turn.failed"),
            "index {index} must be audited as a failed turn"
        );
    }
}

#[tokio::test]
async fn unknown_tool_returns_error_result() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::TextDelta("Let me try.".to_string()),
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "nonexistent".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"x":1}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Sorry, that didn't work.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut core = make_core(provider);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    core.handle_user_message("do something", &mut reader, &mut output)
        .await
        .unwrap();

    assert!(core.messages.len() >= 4);
    let tool_result_msg = &core.messages[2];
    assert_eq!(tool_result_msg.role, "tool");
}

#[tokio::test]
async fn multi_turn_with_tool() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo hello"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("The command output was: hello".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    core.handle_user_message("run echo hello", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains("hello"));
    assert!(
        output_str.find(r#""type":"user""#) < output_str.find("The command output was: hello"),
        "{output_str}"
    );
    assert!(
        output_str.contains(r#""type":"tool_result""#),
        "{output_str}"
    );
    assert!(core.messages.len() >= 4);
}

#[tokio::test]
async fn incomplete_tool_call_stops_without_consuming_turn_budget() {
    let provider = MockProvider::new(vec![vec![
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: r#"{"command":"pwd"}"#.to_string(),
        },
        GenerateEvent::MessageEnd,
    ]]);
    let mut core = make_core(provider);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    let error = core
        .handle_user_message("inspect this project", &mut reader, &mut output)
        .await
        .expect_err("an unnamed tool call must fail immediately");

    assert_eq!(
        error,
        "Provider emitted an incomplete tool call without a function name"
    );
    assert_eq!(core.messages.len(), 1, "must not append an empty turn");
}

#[tokio::test]
async fn mixed_tool_calls_stop_when_any_call_is_incomplete() {
    let provider = MockProvider::new(vec![vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: "call-valid".to_string(),
            name: "shell".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: r#"{"command":"pwd"}"#.to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 1,
            arguments_delta: r#"{"command":"id"}"#.to_string(),
        },
        GenerateEvent::MessageEnd,
    ]]);
    let mut core = make_core(provider);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    let error = core
        .handle_user_message("inspect this project", &mut reader, &mut output)
        .await
        .expect_err("any unnamed tool call with arguments must fail the turn");

    assert_eq!(
        error,
        "Provider emitted an incomplete tool call without a function name"
    );
    assert_eq!(core.messages.len(), 1, "must not execute the named tool");
}

#[tokio::test]
async fn text_after_tool_call_is_not_visible_before_tool_result() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::TextDelta("Preparing to run the command.".to_string()),
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo hello"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::TextDelta("SHOULD NOT BE VISIBLE BEFORE TOOL RESULT".to_string()),
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("The command output was: hello".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    core.handle_user_message("run echo hello", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains("Preparing to run the command."),
        "{output_str}"
    );
    assert!(
        !output_str.contains("SHOULD NOT BE VISIBLE BEFORE TOOL RESULT"),
        "{output_str}"
    );
    assert!(
        output_str.find(r#""type":"tool_result""#)
            < output_str.find("The command output was: hello"),
        "{output_str}"
    );
}

#[tokio::test]
async fn tool_call_block_is_closed_when_stream_ends_without_tool_call_end() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo hello"}"#.to_string(),
            },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("done".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    core.handle_user_message("run echo hello", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains(r#""type":"content_block_stop","index":0"#));
    assert!(
        output_str.find(r#""type":"content_block_stop","index":0"#)
            < output_str.find(r#""type":"tool_result""#),
        "{output_str}"
    );
}

#[tokio::test]
async fn multiple_tool_call_blocks_are_closed_with_distinct_indexes_without_tool_call_end() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "first_unknown".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"value":1}"#.to_string(),
            },
            GenerateEvent::ToolCallStart {
                index: 1,
                id: "call-2".to_string(),
                name: "second_unknown".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 1,
                arguments_delta: r#"{"value":2}"#.to_string(),
            },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("done".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::new();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    core.handle_user_message("run two tools", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    let first_message = output_str
        .split(r#"{"type":"stream_event","event":{"type":"message_stop"}}"#)
        .next()
        .expect("first stream message");
    assert_eq!(
        first_message
            .matches(r#""type":"content_block_start","index":0"#)
            .count(),
        1,
        "{output_str}"
    );
    assert_eq!(
        first_message
            .matches(r#""type":"content_block_start","index":1"#)
            .count(),
        1,
        "{output_str}"
    );
    assert_eq!(
        first_message
            .matches(r#""type":"content_block_stop","index":0"#)
            .count(),
        1,
        "{output_str}"
    );
    assert_eq!(
        first_message
            .matches(r#""type":"content_block_stop","index":1"#)
            .count(),
        1,
        "{output_str}"
    );
    assert!(
        output_str.find(r#""type":"content_block_stop","index":1"#)
            < output_str.find(r#""type":"tool_result""#),
        "{output_str}"
    );
}

#[tokio::test]
async fn approval_flow_allow() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo approved"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Done.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let allow_response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"allow"}}}"#;
    let input = format!("{allow_response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("run echo approved", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains("can_use_tool"));
    assert!(core.messages.len() >= 4);
}

#[tokio::test]
async fn approval_flow_deny() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"rm -rf /"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("I understand, the command was denied.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let tools = ToolRegistry::with_defaults_for_test();

    let deny_response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"deny","message":"Too dangerous"}}}"#;
    let input = format!("{deny_response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();

    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut output = Vec::new();

    core.handle_user_message("delete everything", &mut reader, &mut output)
        .await
        .unwrap();

    let tool_result = core.messages.iter().find(|m| m.role == "tool").unwrap();
    if let crate::provider::MessageContent::Blocks(blocks) = &tool_result.content {
        if let crate::provider::MessageContentBlock::ToolResult {
            content, is_error, ..
        } = &blocks[0]
        {
            assert!(is_error);
            assert!(content.contains("denied"));
        }
    }
}

#[tokio::test]
async fn request_id_skips_mismatched() {
    let core = make_core(MockProvider::text_only(""));
    let mismatched = r#"{"type":"control_response","response":{"subtype":"success","request_id":"wrong-id","response":{"behavior":"allow"}}}"#;
    let correct = r#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"deny","message":"denied"}}}"#;
    let input = format!("{mismatched}\n{correct}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();

    let result = core
        .wait_for_approval("expected-id", None, &mut reader)
        .await;
    assert!(matches!(result, ApprovalResult::Denied(_)));
}

#[test]
fn brokered_profile_denies_the_removed_checkpoint_tool() {
    let core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(MockProvider::text_only("")),
        ToolRegistry::gateway_brokered_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredV1,
    );

    assert_eq!(
        core.classify_tool("workspace_checkpoint_create", &serde_json::json!({})),
        Outcome::Deny
    );
    assert!(!core
        .tool_names()
        .iter()
        .any(|name| name == "workspace_checkpoint_create"));
}

#[test]
fn checkpoint_profile_only_brokers_an_empty_typed_request() {
    let core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(MockProvider::text_only("")),
        ToolRegistry::gateway_brokered_checkpoint_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1,
    );

    assert_eq!(
        core.classify_tool("workspace_checkpoint_create", &serde_json::json!({})),
        Outcome::RequireApproval
    );
    assert_eq!(
        core.classify_tool(
            "workspace_checkpoint_create",
            &serde_json::json!({"checkpoint_id": "runtime-chosen"}),
        ),
        Outcome::Deny
    );
    assert_eq!(
        core.tool_names(),
        vec!["ask_user_question", "workspace_checkpoint_create"]
    );
}

#[test]
fn workspace_write_profile_only_approves_the_exact_file_tool() {
    let core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(MockProvider::text_only("")),
        ToolRegistry::gateway_brokered_workspace_write_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1,
    );

    assert_eq!(
        core.classify_tool(
            "write_file",
            &serde_json::json!({"path": "1.txt", "content": "one"}),
        ),
        Outcome::RequireApproval
    );
    for forbidden in ["shell", "edit", "read_file", "workspace_checkpoint_create"] {
        assert_eq!(
            core.classify_tool(forbidden, &serde_json::json!({})),
            Outcome::Deny
        );
    }
    assert_eq!(core.tool_names(), vec!["ask_user_question", "write_file"]);
}

#[tokio::test]
async fn workspace_write_profile_executes_only_after_allow() {
    let dir = tempfile::tempdir().unwrap();
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "write-call".to_string(),
                name: "write_file".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"path":"1.txt","content":"one"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![GenerateEvent::MessageEnd],
    ]);
    let mut core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(provider),
        ToolRegistry::gateway_brokered_workspace_write_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1,
    );
    core.project_root = dir.path().to_path_buf();
    core.workspace = crate::tool::SessionWorkspace::new(dir.path());
    let input = r#"{"type":"approval_receipt","request_id":"req-0"}
{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"allow"}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("create 1.txt", &mut reader, &mut output)
        .await
        .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains(r#""subtype":"can_use_tool""#));
    assert!(output.contains(r#""tool_name":"write_file""#));
    let content = std::fs::read_to_string(dir.path().join("1.txt"))
        .unwrap_or_else(|error| panic!("{error}; tool transcript: {:?}", core.messages));
    assert_eq!(content, "one");
}

#[tokio::test]
async fn workspace_write_profile_denial_leaves_workspace_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "write-call".to_string(),
                name: "write_file".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"path":"1.txt","content":"one"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![GenerateEvent::MessageEnd],
    ]);
    let mut core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(provider),
        ToolRegistry::gateway_brokered_workspace_write_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1,
    );
    core.project_root = dir.path().to_path_buf();
    core.workspace = crate::tool::SessionWorkspace::new(dir.path());
    let input = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"deny","message":"not approved"}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("create 1.txt", &mut reader, &mut output)
        .await
        .unwrap();

    assert!(!dir.path().join("1.txt").exists());
}

#[tokio::test]
async fn workspace_write_profile_rejects_an_approved_external_path() {
    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("workspace");
    let outside = parent.path().join("outside.txt");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(&outside, "sentinel").unwrap();
    let arguments = serde_json::json!({"path": outside, "content": "changed"}).to_string();
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "write-call".to_string(),
                name: "write_file".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: arguments,
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![GenerateEvent::MessageEnd],
    ]);
    let mut core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(provider),
        ToolRegistry::gateway_brokered_workspace_write_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1,
    );
    core.project_root = workspace.clone();
    core.workspace = crate::tool::SessionWorkspace::new(&workspace);
    let input = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"allow"}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("write outside", &mut reader, &mut output)
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(outside).unwrap(), "sentinel");
}

#[tokio::test]
async fn brokered_profile_rejects_a_checkpoint_execution_result() {
    let core = make_core_with_profile(
        MockProvider::text_only(""),
        crate::cli::ExecutionProfile::GatewayBrokeredV1,
    );
    let input = r#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"host_executed_checkpoint_create","checkpointResult":{"checkpoint_id":"ckpt-123"}}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();

    let result = core
        .wait_for_approval("expected-id", Some(ToolKind::HostedSideEffect), &mut reader)
        .await;
    assert!(matches!(
        result,
        ApprovalResult::Denied(Some(reason)) if reason == "unknown response"
    ));
}

#[tokio::test]
async fn checkpoint_profile_accepts_only_typed_terminal_results() {
    let core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(MockProvider::text_only("")),
        ToolRegistry::gateway_brokered_checkpoint_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1,
    );
    let input = r#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"host_executed_checkpoint_create","checkpointResult":{"checkpoint_id":"ckp_123e4567-e89b-12d3-a456-426614174000","outcome":{"status":"created","snapshot_id":"snap-1"}}}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();

    let result = core
        .wait_for_approval("expected-id", Some(ToolKind::HostedSideEffect), &mut reader)
        .await;
    assert!(matches!(
        result,
        ApprovalResult::HostExecutedCheckpoint { result }
            if !result.is_error
                && result.output.contains("ckp_123e4567-e89b-12d3-a456-426614174000")
                && result.output.contains("snap-1")
    ));

    let input = r#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"host_executed_checkpoint_error","checkpointError":{"outcome":"uncertain","code":"checkpoint_unknown","message":"Exact evidence was not available"}}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let result = core
        .wait_for_approval("expected-id", Some(ToolKind::HostedSideEffect), &mut reader)
        .await;
    assert!(matches!(
        result,
        ApprovalResult::HostExecutedCheckpoint { result }
            if result.is_error
                && result.output.contains("uncertain")
                && result.output.contains("checkpoint_unknown")
                && result
                    .output
                    .contains("Do not retry without Gateway reconciliation.")
    ));

    let input = r#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"host_executed_checkpoint_create","checkpointResult":{"checkpoint_id":"not-a-checkpoint-id","outcome":{"status":"created","snapshot_id":"snap-1"}}}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let result = core
        .wait_for_approval("expected-id", Some(ToolKind::HostedSideEffect), &mut reader)
        .await;
    assert!(matches!(result, ApprovalResult::Denied(Some(_))));

    let input = r#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"host_executed_checkpoint_create","checkpointResult":{"checkpoint_id":"ckp_123e4567-e89b-12d3-a456-426614174000","outcome":{"status":"created","snapshot_id":"snap-1"}},"hiddenAuthority":"forbidden"}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let result = core
        .wait_for_approval("expected-id", Some(ToolKind::HostedSideEffect), &mut reader)
        .await;
    assert!(matches!(result, ApprovalResult::Denied(Some(_))));
}

#[tokio::test]
async fn checkpoint_profile_never_executes_the_hosted_tool_locally() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "checkpoint-call".to_string(),
                name: "workspace_checkpoint_create".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: "{}".to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![GenerateEvent::MessageEnd],
    ]);
    let mut core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(provider),
        ToolRegistry::gateway_brokered_checkpoint_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredCheckpointV1,
    );
    let input = r#"{"type":"approval_receipt","request_id":"req-0"}
{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"host_executed_checkpoint_create","checkpointResult":{"checkpoint_id":"ckp_123e4567-e89b-12d3-a456-426614174000","outcome":{"status":"skipped","reason":"An exact checkpoint already exists"}}}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("checkpoint", &mut reader, &mut output)
        .await
        .unwrap();

    let messages = String::from_utf8(output).unwrap();
    assert!(messages.contains(r#""subtype":"can_use_tool""#));
    assert!(messages.contains(r#""tool_name":"workspace_checkpoint_create""#));
    let tool_result = core
        .messages
        .iter()
        .find(|message| message.role == "tool")
        .unwrap();
    assert!(matches!(
        &tool_result.content,
        crate::provider::MessageContent::Text(content)
            if content.contains("safely skipped")
    ));
}

/// Serializes the two tests that mutate the process-wide
/// `COSH_CORE_APPROVAL_TIMEOUT_SECS`; without it a concurrent
/// `remove_var` could send the hanging test back to the 6h default.
static APPROVAL_TIMEOUT_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn one_shot_approval_uses_a_short_default_timeout() {
    let mut core = make_core(MockProvider::text_only(""));
    assert_eq!(
        core.approval_response_timeout_default,
        Duration::from_secs(APPROVAL_RESPONSE_TIMEOUT_DEFAULT_SECS)
    );

    core.use_one_shot_approval_timeout();

    assert_eq!(
        core.approval_response_timeout_default,
        Duration::from_secs(APPROVAL_RESPONSE_TIMEOUT_ONE_SHOT_DEFAULT_SECS)
    );
}

#[tokio::test]
async fn unanswered_approval_times_out_instead_of_hanging_forever() {
    // #1940 residual guard: hours-scale by default, overridden here so the
    // wait ends quickly. A peer that never answers and never closes the
    // channel must not hang the turn forever.
    let _guard = APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let core = make_core(MockProvider::text_only(""));
    let (client, _server) = tokio::io::duplex(64);
    let mut reader = BufReader::new(client).lines();

    let result = core
        .wait_for_approval("expected-id", None, &mut reader)
        .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");
    assert!(matches!(result, ApprovalResult::TimedOut));
}

#[tokio::test]
async fn answered_approval_beats_the_residual_timeout() {
    // A response that arrives normally must win over the deadline: the
    // guard only fires when nothing ever comes back.
    let _guard = APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let core = make_core(MockProvider::text_only(""));
    let (mut client, server) = tokio::io::duplex(256);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut server = server;
        server
            .write_all(
                br#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"allow"}}}
"#,
            )
            .await
            .expect("write response");
    });
    let mut reader = BufReader::new(&mut client).lines();

    let result = core
        .wait_for_approval("expected-id", None, &mut reader)
        .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");
    assert!(matches!(result, ApprovalResult::Allowed));
}

#[tokio::test]
async fn approval_receipt_disarms_the_residual_timeout_for_a_pending_card() {
    // #1940 receipt protocol: once the shell acknowledges the request, a
    // card waiting on the user may outlive the residual deadline — the
    // shell owns the terminal state from that point on.
    let _guard = APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let core = make_core(MockProvider::text_only(""));
    let (mut client, server) = tokio::io::duplex(512);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut server = server;
        server
            .write_all(
                br#"{"type":"approval_receipt","request_id":"expected-id"}
"#,
            )
            .await
            .expect("write receipt");
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        server
            .write_all(
                br#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"allow"}}}
"#,
            )
            .await
            .expect("write response");
    });
    let mut reader = BufReader::new(&mut client).lines();

    let result = core
        .wait_for_approval("expected-id", None, &mut reader)
        .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");
    assert!(matches!(result, ApprovalResult::Allowed));
}

#[tokio::test]
async fn approval_receipt_disarms_the_residual_timeout_for_a_slow_host_command() {
    // Same disarm for a host-executed command finishing after the residual
    // deadline: the executed result must reach the model, never a phantom
    // "not executed" timeout.
    let _guard = APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let core = make_core(MockProvider::text_only(""));
    let (mut client, server) = tokio::io::duplex(512);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut server = server;
        server
            .write_all(
                br#"{"type":"approval_receipt","request_id":"expected-id"}
"#,
            )
            .await
            .expect("write receipt");
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        server
            .write_all(
                br#"{"type":"control_response","response":{"subtype":"success","request_id":"expected-id","response":{"behavior":"host_executed_shell","result":{"llmContent":"command output","metadata":{"exit_code":0}}}}}
"#,
            )
            .await
            .expect("write response");
    });
    let mut reader = BufReader::new(&mut client).lines();

    let result = core
        .wait_for_approval("expected-id", Some(ToolKind::ShellExec), &mut reader)
        .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");
    assert!(matches!(
        result,
        ApprovalResult::HostExecutedShell {
            exit_code: Some(0),
            ..
        }
    ));
}

#[tokio::test]
async fn approval_receipt_for_a_different_request_keeps_the_residual_timeout() {
    // A receipt only disarms the wait for its own request id; an unrelated
    // receipt observed on the shared reader must not leak the disarm.
    let _guard = APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let core = make_core(MockProvider::text_only(""));
    let (mut client, server) = tokio::io::duplex(256);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut server = server;
        server
            .write_all(
                br#"{"type":"approval_receipt","request_id":"other-id"}
"#,
            )
            .await
            .expect("write receipt");
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    });
    let mut reader = BufReader::new(&mut client).lines();

    let result = core
        .wait_for_approval("expected-id", None, &mut reader)
        .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");
    assert!(matches!(result, ApprovalResult::TimedOut));
}

#[tokio::test]
async fn approval_timeout_fails_the_turn_without_a_second_generation() {
    // #1940: a timed-out approval must end the turn. The mock peer keeps
    // the channel open but never answers; after the residual deadline the
    // turn fails — no second provider generation, the gated tool never
    // runs, and a late response written afterwards is never consumed.
    let _guard = APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let shell_calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo must-not-run"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("second generation must never happen".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&shell_calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let (client, mut server) = tokio::io::duplex(256);
    let mut reader = BufReader::new(client).lines();
    let mut output = Vec::new();

    let result = core
        .handle_user_message("run it", &mut reader, &mut output)
        .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");

    // A late response written after the timeout has no waiter left: it must
    // never turn into an execution.
    use tokio::io::AsyncWriteExt;
    server
        .write_all(
            br#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"allow"}}}
"#,
        )
        .await
        .expect("write late response");

    let error = result.expect_err("a timed-out approval must fail the turn");
    assert!(
        error.contains("timed out"),
        "the failure must name the timeout: {error}"
    );
    let output_str = String::from_utf8(output).unwrap();
    assert!(
        !output_str.contains("second generation must never happen"),
        "the turn must fail before another provider generation: {output_str}"
    );
    assert_eq!(
        shell_calls.load(Ordering::SeqCst),
        0,
        "the gated tool must never execute"
    );
    let tool_result = core
        .messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call-1"))
        .expect("the declared tool call keeps a paired result");
    match &tool_result.content {
        crate::provider::MessageContent::Text(content) => {
            assert!(content.contains("approval timed out"), "{content}");
        }
        _ => panic!("expected text tool result"),
    }
}

/// A post_tool_use_failure hook that requests a sandbox bypass and touches a
/// marker file so the test can prove the hook actually ran.
fn sandbox_bypass_hook(marker: &std::path::Path) -> crate::config::HookDefinition {
    crate::config::HookDefinition {
        command: format!(
            "touch '{}' && printf '%s\\n' '{}'",
            marker.display(),
            r#"{"hookSpecificOutput":{"sandbox_bypass_request":{"original_command":"echo must-not-run","reason":"sandbox blocked"}}}"#
        ),
        name: Some("sandbox-bypass-hook".to_string()),
        matcher: None,
        timeout: Some(10_000),
        sequential: None,
        fail_open: false,
        env: Default::default(),
    }
}

#[tokio::test]
async fn approval_timeout_suppresses_the_sandbox_bypass_reprompt() {
    // #1940: once the policy approval times out the turn is fatal, so a
    // post_tool_use_failure hook's sandbox-bypass request must not open a
    // second approval — its Allowed arm would execute the tool behind the
    // recorded "not executed" result.
    let _guard = APPROVAL_TIMEOUT_ENV_LOCK.lock().await;
    std::env::set_var("COSH_CORE_APPROVAL_TIMEOUT_SECS", "1");
    let hook_marker = std::env::temp_dir().join(format!(
        "cosh-bypass-hook-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&hook_marker);
    let shell_calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo must-not-run"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("second generation must never happen".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    config.hooks.enabled = true;
    config.hooks.post_tool_use_failure = vec![sandbox_bypass_hook(&hook_marker)];
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&shell_calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let (client, _server) = tokio::io::duplex(256);
    let mut reader = BufReader::new(client).lines();
    let mut output = Vec::new();

    let result = core
        .handle_user_message("run it", &mut reader, &mut output)
        .await;
    std::env::remove_var("COSH_CORE_APPROVAL_TIMEOUT_SECS");

    let error = result.expect_err("a timed-out approval must fail the turn");
    assert!(error.contains("timed out"), "{error}");
    assert!(
        hook_marker.exists(),
        "the failure hook must have run, otherwise this test proves nothing"
    );
    let output_str = String::from_utf8(output).unwrap();
    assert!(
        !output_str.contains("second generation must never happen"),
        "the turn must fail before another provider generation: {output_str}"
    );
    assert_eq!(
        output_str.matches("can_use_tool").count(),
        1,
        "only the policy approval may be emitted; the bypass reprompt must be suppressed: {output_str}"
    );
    assert_eq!(
        shell_calls.load(Ordering::SeqCst),
        0,
        "the tool must never execute, not even behind a bypass approval"
    );
    let _ = std::fs::remove_file(&hook_marker);
}

#[tokio::test]
async fn approval_flow_host_executed_shell_uses_tool_result() {
    let shell_calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"df -h"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Received shell evidence.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&shell_calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"host_executed_shell","result":{"llmContent":"ShellCommandCompleted evidence\ncommand: df -h\nstatus: completed","returnDisplay":"df -h completed","metadata":{"command":"df -h","status":"completed","exit_code":0}}}}}"#;
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("check disk", &mut reader, &mut output)
        .await
        .unwrap();

    assert_eq!(
        shell_calls.load(Ordering::SeqCst),
        0,
        "host-executed result must not run provider-native shell executor"
    );
    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains("Received shell evidence."),
        "{output_str}"
    );
    assert!(
        !output_str.contains(r#""type":"tool_result""#),
        "{output_str}"
    );
    let tool_result = core
        .messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call-1"))
        .expect("tool result");
    match &tool_result.content {
        crate::provider::MessageContent::Text(content) => {
            assert!(content.contains("ShellCommandCompleted evidence"));
            assert!(content.contains("command: df -h"));
        }
        _ => panic!("expected text tool result"),
    }
}

#[tokio::test]
async fn approval_flow_rejects_host_executed_for_non_shell_tool() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-write".to_string(),
                name: "write_file".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta:
                    r#"{"file_path":"/tmp/cosh-host-executed-non-shell","content":"bad"}"#
                        .to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Rejected.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"host_executed_shell","result":{"llmContent":"should not be accepted","returnDisplay":null,"metadata":{"command":"echo bad","status":"completed","exit_code":0}}}}}"#;
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("write file", &mut reader, &mut output)
        .await
        .unwrap();

    let tool_result = core
        .messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call-write"))
        .expect("tool result");
    match &tool_result.content {
        crate::provider::MessageContent::Text(content) => {
            assert!(content.contains("host_executed_shell is only valid for shell tools"));
            assert!(!content.contains("should not be accepted"));
        }
        _ => panic!("expected text tool result"),
    }
}

#[tokio::test]
async fn ask_user_question_flow() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_string(),
                name: "ask_user_question".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"question":"Which language?","options":[{"label":"Rust"},{"label":"Python"}]}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Great, you chose Rust!".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let answer_response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"answer":"Rust"}}}"#;
    let input = format!("{answer_response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("what language?", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains("ask_user"));

    let tool_result = core.messages.iter().find(|m| m.role == "tool").unwrap();
    if let crate::provider::MessageContent::Blocks(blocks) = &tool_result.content {
        if let crate::provider::MessageContentBlock::ToolResult { content, .. } = &blocks[0] {
            assert!(content.contains("Rust"));
        }
    }
}

#[tokio::test]
async fn brokered_ask_user_binds_the_private_request_to_the_tool_call() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "question-call".to_string(),
                name: "ask_user_question".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"question":"Choose a branch","options":[{"label":"main"}],"allow_free_text":false}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![GenerateEvent::MessageEnd],
    ]);
    let mut core = CoshCore::new_with_profile(
        CoreConfig::default(),
        Box::new(provider),
        ToolRegistry::gateway_brokered_v1(),
        crate::cli::ExecutionProfile::GatewayBrokeredV1,
    );
    let input = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"answer","answer":"main"}}}
"#;
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("choose", &mut reader, &mut output)
        .await
        .unwrap();

    let request = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|message| {
            message
                .pointer("/request/subtype")
                .and_then(|value| value.as_str())
                == Some("ask_user")
        })
        .expect("brokered ask_user request");
    assert_eq!(
        request
            .pointer("/request/tool_use_id")
            .and_then(|value| value.as_str()),
        Some("question-call")
    );
    assert_eq!(core.tool_names(), vec!["ask_user_question"]);
}

#[tokio::test]
async fn cosh_shell_evidence_read_output_uses_control_protocol_result() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-evidence".to_string(),
                name: "cosh_shell_evidence".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"action":"read_output","output_id":"terminal-output://raw-session-a1b2/cmd-1","direction":"tail","lines":42}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("I can see the captured output.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::new().with_shell_evidence();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceExcerpt\noutput_id: terminal-output://raw-session-a1b2/cmd-1\nexcerpt_status: available\nstdout","returnDisplay":"captured output","metadata":{"action":"read_output","output_id":"terminal-output://raw-session-a1b2/cmd-1","excerpt_status":"available","is_error":false}}}}}"#;
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("read output", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains(r#""subtype":"shell_evidence""#),
        "{output_str}"
    );
    assert!(
        output_str.contains(r#""action":"read_output""#),
        "{output_str}"
    );
    assert!(
        output_str.contains(r#""tool_use_id":"call-evidence""#),
        "{output_str}"
    );
    assert!(output_str.contains(r#""lines":42"#), "{output_str}");
    assert!(
        !output_str.contains(r#""bypass_recent_filter""#),
        "{output_str}"
    );
    assert!(
        output_str.contains(r#""type":"tool_result""#),
        "{output_str}"
    );
    assert!(
        output_str.contains("I can see the captured output."),
        "{output_str}"
    );

    let tool_result = core
        .messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call-evidence"))
        .expect("tool result");
    match &tool_result.content {
        crate::provider::MessageContent::Text(content) => {
            assert!(content.contains("ShellEvidenceExcerpt"));
            assert!(content.contains("excerpt_status: available"));
        }
        _ => panic!("expected text tool result"),
    }
}

#[tokio::test]
async fn cosh_shell_evidence_uses_its_control_protocol_in_every_mode() {
    for mode in [
        ApprovalMode::Recommend,
        ApprovalMode::Auto,
        ApprovalMode::Trust,
    ] {
        let provider = MockProvider::new(vec![
            vec![
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-evidence".to_string(),
                    name: "cosh_shell_evidence".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"action":"list_commands","limit":1}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::MessageEnd,
            ],
            vec![GenerateEvent::MessageEnd],
        ]);
        let mut config = CoreConfig::default();
        config.agent.approval_mode = mode;
        let tools = ToolRegistry::new().with_shell_evidence();
        let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
        let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceCommandIndex\ncommand_id: cmd-1","returnDisplay":null,"metadata":{"action":"list_commands","is_error":false}}}}}"#;
        let mut reader = BufReader::new(response.as_bytes()).lines();
        let mut output = Vec::new();

        core.handle_user_message("list commands", &mut reader, &mut output)
            .await
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains(r#""subtype":"shell_evidence""#),
            "{mode}: {output}"
        );
        assert!(
            !output.contains(r#""subtype":"can_use_tool""#),
            "{mode}: {output}"
        );
    }
}

#[tokio::test]
async fn cosh_shell_evidence_list_commands_uses_control_protocol_result() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-evidence".to_string(),
                name: "cosh_shell_evidence".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"action":"list_commands","limit":2}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("I can see the command index.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::new().with_shell_evidence();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceCommandIndex\ncommand_id: cmd-1\noutput_available: true","returnDisplay":null,"metadata":{"action":"list_commands","scope":"current_ledger","limit":2,"next_cursor":null,"is_error":false}}}}}"#;
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("list commands", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains(r#""subtype":"shell_evidence""#),
        "{output_str}"
    );
    assert!(
        output_str.contains(r#""action":"list_commands""#),
        "{output_str}"
    );
    assert!(output_str.contains(r#""limit":2"#), "{output_str}");
    assert!(
        output_str.contains("I can see the command index."),
        "{output_str}"
    );

    let tool_result = core
        .messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call-evidence"))
        .expect("tool result");
    match &tool_result.content {
        crate::provider::MessageContent::Text(content) => {
            assert!(content.contains("ShellEvidenceCommandIndex"));
            assert!(content.contains("output_available: true"));
        }
        _ => panic!("expected text tool result"),
    }
}

#[tokio::test]
async fn cosh_shell_evidence_preserves_error_result() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-evidence".to_string(),
                name: "cosh_shell_evidence".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta:
                    r#"{"action":"read_output","output_id":"terminal-output://old-session/cmd-1"}"#
                        .to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("The output is stale.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::new().with_shell_evidence();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceExcerpt\noutput_id: terminal-output://old-session/cmd-1\nexcerpt_status: unavailable\nreason: stale_session","returnDisplay":"stale output","metadata":{"action":"read_output","output_id":"terminal-output://old-session/cmd-1","excerpt_status":"unavailable","is_error":true,"reason":"stale_session"}}}}}"#;
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("read output", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains(r#""is_error":true"#), "{output_str}");
    let tool_result = core
        .messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call-evidence"))
        .expect("tool result");
    match &tool_result.content {
        crate::provider::MessageContent::Text(content) => {
            assert!(content.contains("excerpt_status: unavailable"));
            assert!(content.contains("reason: stale_session"));
        }
        _ => panic!("expected text tool result"),
    }
}

#[tokio::test]
async fn cosh_shell_evidence_read_output_forwards_bypass_recent_filter() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-evidence".to_string(),
                name: "cosh_shell_evidence".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"action":"read_output","output_id":"terminal-output://raw-session-a1b2/cmd-1","bypass_recent_filter":true}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![GenerateEvent::MessageEnd],
    ]);

    let tools = ToolRegistry::new().with_shell_evidence();
    let mut core = CoshCore::new_legacy(CoreConfig::default(), Box::new(provider), tools);

    let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceExcerpt\noutput_id: terminal-output://raw-session-a1b2/cmd-1\nexcerpt_status: available\nstdout","returnDisplay":"captured output","metadata":{"action":"read_output","output_id":"terminal-output://raw-session-a1b2/cmd-1","excerpt_status":"available","is_error":false}}}}}"#;
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("read output", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains(r#""bypass_recent_filter":true"#),
        "{output_str}"
    );
}

#[tokio::test]
async fn cosh_shell_evidence_already_delivered_is_not_error() {
    let core = make_core(MockProvider::new(vec![]));
    let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceExcerpt\noutput_id: terminal-output://raw-session/cmd-1\nexcerpt_status: already_delivered\nreason: already_delivered_recent_shell_tool_output","returnDisplay":null,"metadata":{"action":"read_output","output_id":"terminal-output://raw-session/cmd-1","excerpt_status":"already_delivered","is_error":false,"reason":"already_delivered_recent_shell_tool_output"}}}}}"#;
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();

    let result = core.wait_for_shell_evidence("req-0", &mut reader).await;

    assert!(!result.is_error, "{}", result.output);
    assert!(result.output.contains("excerpt_status: already_delivered"));
}

#[tokio::test]
async fn cosh_shell_evidence_bypasses_normal_tool_hooks() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-list".to_string(),
                name: "cosh_shell_evidence".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"action":"list_commands","limit":2}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-read".to_string(),
                name: "cosh_shell_evidence".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta:
                    r#"{"action":"read_output","output_id":"terminal-output://raw-session/cmd-1"}"#
                        .to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("evidence hooks bypassed".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.hooks = config::HooksConfig {
        enabled: true,
        pre_tool_use: vec![config::HookDefinition {
            command: "echo '{\"decision\":\"block\",\"reason\":\"pre hook should not run\"}'"
                .to_string(),
            name: Some("block-evidence".to_string()),
            matcher: Some("cosh_shell_evidence".to_string()),
            timeout: Some(5000),
            sequential: None,
            fail_open: false,
            env: Default::default(),
        }],
        post_tool_use: vec![config::HookDefinition {
            command: "echo '{\"decision\":\"block\",\"reason\":\"post hook should not run\"}'"
                .to_string(),
            name: Some("deny-evidence".to_string()),
            matcher: Some("cosh_shell_evidence".to_string()),
            timeout: Some(5000),
            sequential: None,
            fail_open: false,
            env: Default::default(),
        }],
        ..Default::default()
    };
    let tools = ToolRegistry::new().with_shell_evidence();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let list_response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceCommandIndex\ncommand_id: cmd-1","returnDisplay":null,"metadata":{"action":"list_commands","is_error":false}}}}}"#;
    let read_response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceExcerpt\noutput_id: terminal-output://raw-session/cmd-1\nstdout","returnDisplay":"stdout","metadata":{"action":"read_output","is_error":false}}}}}"#;
    let input = format!("{list_response}\n{read_response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("inspect shell evidence", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains(r#""action":"list_commands""#),
        "{output_str}"
    );
    assert!(
        output_str.contains(r#""action":"read_output""#),
        "{output_str}"
    );
    assert!(
        output_str.contains("evidence hooks bypassed"),
        "{output_str}"
    );
    assert!(!output_str.contains("hook_notification"), "{output_str}");
    assert!(!output_str.contains("Blocked by hook"), "{output_str}");
    assert!(
        !output_str.contains("Post-tool hook denied"),
        "{output_str}"
    );
    assert!(
        !output_str.contains("pre hook should not run"),
        "{output_str}"
    );
    assert!(
        !output_str.contains("post hook should not run"),
        "{output_str}"
    );
}

#[tokio::test]
async fn cosh_shell_evidence_rejects_read_output_without_output_id() {
    let core = make_core(MockProvider::new(vec![]));
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    let result = core
        .handle_shell_evidence(
            "call-evidence",
            &serde_json::json!({"action":"read_output"}),
            &mut reader,
            &mut output,
        )
        .await;

    assert!(result.is_error);
    assert!(result.output.contains("missing required output_id"));
    assert!(String::from_utf8(output).unwrap().is_empty());
}

#[tokio::test]
async fn cosh_shell_evidence_rejects_list_commands_read_output_fields() {
    let core = make_core(MockProvider::new(vec![]));
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    let result = core
        .handle_shell_evidence(
            "call-evidence",
            &serde_json::json!({
                "action":"list_commands",
                "output_id":"terminal-output://raw-session/cmd-1"
            }),
            &mut reader,
            &mut output,
        )
        .await;

    assert!(result.is_error);
    assert!(result.output.contains("accepts only limit and cursor"));
    assert!(String::from_utf8(output).unwrap().is_empty());
}

#[tokio::test]
async fn cosh_shell_evidence_list_commands_ignores_direction_hint() {
    let core = make_core(MockProvider::new(vec![]));
    let response = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"shell_evidence","result":{"llmContent":"ShellEvidenceCommandIndex\ncommand_id: cmd-1","returnDisplay":null,"metadata":{"action":"list_commands","scope":"current_ledger","limit":10,"next_cursor":null,"is_error":false}}}}}"#;
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    let result = core
        .handle_shell_evidence(
            "call-evidence",
            &serde_json::json!({
                "action":"list_commands",
                "direction":"tail",
                "limit":10
            }),
            &mut reader,
            &mut output,
        )
        .await;

    assert!(!result.is_error, "{}", result.output);
    assert!(result.output.contains("ShellEvidenceCommandIndex"));
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains(r#""action":"list_commands""#), "{output}");
    assert!(output.contains(r#""limit":10"#), "{output}");
    assert!(!output.contains(r#""direction""#), "{output}");
}

#[tokio::test]
async fn cosh_shell_evidence_rejects_invalid_limit_type() {
    let core = make_core(MockProvider::new(vec![]));
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    let result = core
        .handle_shell_evidence(
            "call-evidence",
            &serde_json::json!({"action":"list_commands","limit":"many"}),
            &mut reader,
            &mut output,
        )
        .await;

    assert!(result.is_error);
    assert!(result.output.contains("limit must be an integer"));
    assert!(String::from_utf8(output).unwrap().is_empty());
}

#[tokio::test]
async fn cosh_shell_evidence_rejects_invalid_bypass_recent_filter_type() {
    let core = make_core(MockProvider::new(vec![]));
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    let result = core
        .handle_shell_evidence(
            "call-evidence",
            &serde_json::json!({
                "action":"read_output",
                "output_id":"terminal-output://raw-session/cmd-1",
                "bypass_recent_filter":"true"
            }),
            &mut reader,
            &mut output,
        )
        .await;

    assert!(result.is_error);
    assert!(result
        .output
        .contains("bypass_recent_filter must be a boolean"));
    assert!(String::from_utf8(output).unwrap().is_empty());
}

#[tokio::test]
async fn thinking_delta_emits_stream_event() {
    let provider = MockProvider::new(vec![vec![
        GenerateEvent::ThinkingDelta("Step 1: analyze...".to_string()),
        GenerateEvent::ThinkingDelta("Step 2: conclude.".to_string()),
        GenerateEvent::TextDelta("The answer is 42.".to_string()),
        GenerateEvent::MessageEnd,
    ]]);
    let mut core = make_core(provider);
    let mut output = Vec::new();
    let mut reader = empty_reader().await;

    core.handle_user_message("think about this", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert!(output_str.contains("thinking_delta"));
    assert!(output_str.contains("Step 1: analyze..."));
    assert!(output_str.contains("The answer is 42."));
    let thinking_line = output_str
        .lines()
        .find(|l| l.contains("thinking_delta"))
        .expect("should have thinking_delta line");
    let v: serde_json::Value = serde_json::from_str(thinking_line).unwrap();
    assert_eq!(
        v.pointer("/event/delta/thinking").and_then(|t| t.as_str()),
        Some("Step 1: analyze...")
    );
}

// ---------------------------------------------------------------------------
// malformed tool arguments: visibility and retry budget
// ---------------------------------------------------------------------------

/// One turn calling `shell` with arguments that are terminated but unparseable.
///
/// Each turn uses a fresh id, as a real provider would: the retry budget must
/// count the tool and the failure, not the call id.
fn unparseable_shell_turn(call_id: &str) -> Vec<GenerateEvent> {
    vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: call_id.to_string(),
            name: "shell".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: r#"{"command":"echo hello"#.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 0 },
        GenerateEvent::MessageEnd,
    ]
}

async fn run_shell_turns(
    turns: Vec<Vec<GenerateEvent>>,
) -> (Result<AgentTurnOutcome, String>, String) {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(MockProvider::new(turns)), tools);
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    let result = core
        .handle_user_message("write the file", &mut reader, &mut output)
        .await;

    (result, String::from_utf8(output).unwrap())
}

#[tokio::test]
async fn rejected_tool_arguments_are_reported_to_the_shell_as_a_failed_tool() {
    let (result, output) = run_shell_turns(vec![
        unparseable_shell_turn("call-1"),
        vec![
            GenerateEvent::TextDelta("I will stop here.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ])
    .await;

    result.expect("one rejection is recoverable");
    // Without this the Shell keeps a pending tool on screen forever: the
    // rejection only ever reached the model's context.
    assert!(
        output.contains(r#""type":"tool_result""#),
        "the rejection must close the pending tool in the UI: {output}"
    );
    assert!(output.contains("attempt 1/3"), "{output}");
    assert!(output.contains("code=invalid_json"), "{output}");
    // The rejected payload can hold session content, so the result the user sees
    // must describe the failure without quoting any of it.
    let rejection = output
        .lines()
        .find(|line| line.contains(r#""type":"tool_result""#))
        .expect("a tool result on the wire");
    assert!(!rejection.contains("echo hello"), "{rejection}");
}

#[tokio::test]
async fn three_consecutive_argument_rejections_stop_the_run() {
    let (result, output) = run_shell_turns(vec![
        unparseable_shell_turn("call-1"),
        unparseable_shell_turn("call-2"),
        unparseable_shell_turn("call-3"),
        unparseable_shell_turn("call-4"),
    ])
    .await;

    let error = result.expect_err("the run stops once the budget is spent");
    assert!(error.contains("shell"), "{error}");
    assert!(error.contains("code=invalid_json"), "{error}");
    assert!(error.contains("never executed"), "{error}");

    // The third rejection is still delivered, so the last thing on screen is a
    // failed tool rather than one that looks like it is still generating.
    assert!(output.contains("attempt 3/3"), "{output}");
    assert_eq!(
        output.matches(r#""type":"tool_result""#).count(),
        3,
        "exactly three attempts may be spent: {output}"
    );
    assert!(
        !output.contains("call-4"),
        "the fourth turn must never be requested: {output}"
    );
}

/// The assistant message declares every call in the batch up front, so ending
/// the run on the third rejection must not leave a later call unanswered:
/// headless persists the session and reuses it for the next user message, and an
/// unpaired `tool_use` id makes that request malformed.
#[tokio::test]
async fn stopping_on_exhaustion_still_answers_every_call_in_the_batch() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(
        config,
        Box::new(MockProvider::new(vec![
            unparseable_shell_turn("call-1"),
            unparseable_shell_turn("call-2"),
            // The fatal third rejection shares its message with a second call.
            vec![
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-fatal".to_string(),
                    name: "shell".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"command":"echo hello"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::ToolCallStart {
                    index: 1,
                    id: "call-trailing".to_string(),
                    name: "shell".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 1,
                    arguments_delta: r#"{"command":"echo trailing"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 1 },
                GenerateEvent::MessageEnd,
            ],
        ])),
        tools,
    );
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("write the file", &mut reader, &mut output)
        .await
        .expect_err("the run stops once the budget is spent");

    for call_id in ["call-1", "call-2", "call-fatal", "call-trailing"] {
        let results = core
            .messages
            .iter()
            .filter(|message| {
                message.role == "tool" && message.tool_call_id.as_deref() == Some(call_id)
            })
            .count();
        assert_eq!(results, 1, "{call_id} must have exactly one tool result");
    }

    // The trailing call was answered, not run: nothing may execute after the
    // budget is spent.
    let trailing = tool_result_text(&core, "call-trailing");
    assert!(trailing.contains("was not executed"), "{trailing}");

    // A skipped call is still part of the transcript, so it owes the audit trace
    // one `tool.requested` and one terminal event like any other call — and it is
    // counted, so the turn metrics do not under-report the failures.
    assert_eq!(
        core.audit.captured_tool_event_types("call-trailing"),
        vec!["tool.requested", "tool.failed"]
    );
    assert_eq!(core.metrics.tool_calls_total, 4);
    assert_eq!(core.metrics.tool_calls_fail, 4);
    assert!(!core
        .audit
        .captured_tool_event_types("call-trailing")
        .contains(&"tool.execution.started"));

    // And the Shell was told, so its pending tool closes instead of hanging.
    let output = String::from_utf8(output).unwrap();
    let emitted = output
        .lines()
        .find(|line| line.contains(r#""tool_use_id":"call-trailing""#))
        .expect("a tool result for the trailing call on the wire");
    assert!(emitted.contains(r#""is_error":true"#), "{emitted}");
    assert!(emitted.contains("was not executed"), "{emitted}");
}

#[tokio::test]
async fn a_recovered_tool_call_clears_the_rejection_budget() {
    let (result, output) = run_shell_turns(vec![
        unparseable_shell_turn("call-1"),
        unparseable_shell_turn("call-2"),
        // The model recovers, which must forget the streak entirely...
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-good".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"echo recovered"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        // ...so this rejection is attempt 1 again, not the fatal third.
        unparseable_shell_turn("call-3"),
        vec![
            GenerateEvent::TextDelta("Done.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ])
    .await;

    result.expect("a recovered call in between keeps the run alive");
    assert!(output.contains("attempt 1/3"), "{output}");
    assert!(!output.contains("attempt 3/3"), "{output}");
}

// ---------------------------------------------------------------------------
// ask_user_question argument validation
// ---------------------------------------------------------------------------

/// One malformed `ask_user_question` call, as it would arrive from a provider.
struct AskUserRejectionCase {
    label: &'static str,
    /// `None` models a `ToolCallStart` that never received argument deltas.
    arguments: Option<&'static str>,
    expected_code: &'static str,
}

/// Drive one turn that issues `ask_user_question` with `arguments`, followed by
/// a plain-text turn. Returns the emitted stdout and the resulting core.
async fn run_ask_user_turn(arguments: Option<&str>) -> (String, CoshCore) {
    let mut first_turn = vec![GenerateEvent::ToolCallStart {
        index: 0,
        id: "call-ask".to_string(),
        name: "ask_user_question".to_string(),
    }];
    if let Some(arguments) = arguments {
        first_turn.push(GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: arguments.to_string(),
        });
    }
    first_turn.push(GenerateEvent::ToolCallEnd { index: 0 });
    first_turn.push(GenerateEvent::MessageEnd);

    let provider = MockProvider::new(vec![
        first_turn,
        vec![
            GenerateEvent::TextDelta("Recovered without a question.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("what now?", &mut reader, &mut output)
        .await
        .expect("turn completes");

    (String::from_utf8(output).unwrap(), core)
}

fn tool_result_text(core: &CoshCore, tool_call_id: &str) -> String {
    core.messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(tool_call_id))
        .expect("tool result appended to the provider conversation")
        .content
        .as_text()
}

#[tokio::test]
async fn malformed_ask_user_arguments_never_reach_the_user() {
    let cases = [
        AskUserRejectionCase {
            label: "no argument delta after tool call start",
            arguments: None,
            expected_code: "empty_arguments",
        },
        AskUserRejectionCase {
            label: "empty arguments",
            arguments: Some(""),
            expected_code: "empty_arguments",
        },
        AskUserRejectionCase {
            label: "truncated json",
            arguments: Some(r#"{"question":"How should local chan"#),
            expected_code: "invalid_json",
        },
        AskUserRejectionCase {
            label: "non-object root",
            arguments: Some(r#"["How should local changes be handled?"]"#),
            expected_code: "root_not_object",
        },
        AskUserRejectionCase {
            label: "empty object",
            arguments: Some("{}"),
            expected_code: "missing_question",
        },
        AskUserRejectionCase {
            label: "null question",
            arguments: Some(r#"{"question":null}"#),
            expected_code: "question_wrong_type",
        },
        AskUserRejectionCase {
            label: "null options",
            arguments: Some(r#"{"question":"Pick one","options":null}"#),
            expected_code: "options_wrong_type",
        },
        AskUserRejectionCase {
            label: "null allow_free_text",
            arguments: Some(r#"{"question":"Pick one","allow_free_text":null}"#),
            expected_code: "allow_free_text_wrong_type",
        },
        AskUserRejectionCase {
            label: "number question",
            arguments: Some(r#"{"question":7}"#),
            expected_code: "question_wrong_type",
        },
        AskUserRejectionCase {
            label: "array question",
            arguments: Some(r#"{"question":["one","two"]}"#),
            expected_code: "question_wrong_type",
        },
        AskUserRejectionCase {
            label: "object question",
            arguments: Some(r#"{"question":{"text":"pick one"}}"#),
            expected_code: "question_wrong_type",
        },
        AskUserRejectionCase {
            label: "empty question string",
            arguments: Some(r#"{"question":""}"#),
            expected_code: "empty_question",
        },
        AskUserRejectionCase {
            label: "whitespace question string",
            arguments: Some(r#"{"question":"   "}"#),
            expected_code: "empty_question",
        },
        AskUserRejectionCase {
            label: "claude-style nested questions",
            arguments: Some(
                r#"{"questions":[{"question":"How should local changes be handled?","header":"Local changes","options":[{"label":"Stash"}],"multiSelect":false}]}"#,
            ),
            expected_code: "unsupported_nested_questions",
        },
        AskUserRejectionCase {
            label: "options wrong type",
            arguments: Some(r#"{"question":"Pick one","options":{"label":"Stash"}}"#),
            expected_code: "options_wrong_type",
        },
        AskUserRejectionCase {
            label: "option label wrong type",
            arguments: Some(r#"{"question":"Pick one","options":[{"label":42}]}"#),
            expected_code: "option_invalid",
        },
        AskUserRejectionCase {
            label: "option description wrong type",
            arguments: Some(
                r#"{"question":"Pick one","options":[{"label":"Stash","description":[]}]}"#,
            ),
            expected_code: "option_invalid",
        },
        AskUserRejectionCase {
            label: "allow_free_text wrong type",
            arguments: Some(r#"{"question":"Pick one","allow_free_text":"true"}"#),
            expected_code: "allow_free_text_wrong_type",
        },
        AskUserRejectionCase {
            label: "multi_select wrong type",
            arguments: Some(r#"{"question":"Pick one","multi_select":"no"}"#),
            expected_code: "multi_select_wrong_type",
        },
        AskUserRejectionCase {
            label: "no answer path",
            arguments: Some(r#"{"question":"Pick one","allow_free_text":false,"options":[]}"#),
            expected_code: "no_answer_path",
        },
    ];

    for case in cases {
        let (output, core) = run_ask_user_turn(case.arguments).await;

        assert!(
            !output.contains(r#""subtype":"ask_user""#),
            "case {}: no ask_user control request may be emitted, got {output}",
            case.label
        );
        assert!(
            !output.contains("control_request"),
            "case {}: rejected arguments must not open any control request, got {output}",
            case.label
        );
        assert!(
            !output.contains("Agent needs your input"),
            "case {}: generic fallback leaked into output",
            case.label
        );

        let tool_text = tool_result_text(&core, "call-ask");
        assert!(
            tool_text.contains(&format!("code={}", case.expected_code)),
            "case {}: expected code={} in {tool_text}",
            case.label,
            case.expected_code
        );

        let event_types = core.audit.captured_event_types();
        assert!(
            event_types.contains(&"tool.requested"),
            "case {}: rejection must still be audited as requested",
            case.label
        );
        assert!(
            event_types.contains(&"tool.failed"),
            "case {}: rejection must be audited as failed",
            case.label
        );
        assert!(
            !event_types.contains(&"tool.execution.started"),
            "case {}: rejected arguments must not start an execution",
            case.label
        );

        assert_eq!(
            (
                core.metrics.tool_calls_total,
                core.metrics.tool_calls_fail,
                core.metrics.tool_calls_success
            ),
            (1, 1, 0),
            "case {}: a rejected question counts once, as a failure",
            case.label
        );

        let last = core.messages.last().expect("assistant reply");
        assert_eq!(last.role, "assistant", "case {}", case.label);
        assert!(
            last.content
                .as_text()
                .contains("Recovered without a question."),
            "case {}: the provider turn after the tool error must still run",
            case.label
        );
    }
}

#[tokio::test]
async fn valid_ask_user_arguments_still_produce_a_question_and_answer() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-ask".to_string(),
                name: "ask_user_question".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"question":"How should local changes be handled?","options":[{"label":"Stash","description":"git stash"},{"label":"Discard"}],"allow_free_text":false,"multi_select":true}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Stashing then.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);
    let input = "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"req-0\",\"response\":{\"answer\":\"Stash\"}}}\n";
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("what now?", &mut reader, &mut output)
        .await
        .expect("turn completes");

    let output_str = String::from_utf8(output).unwrap();
    let request_line = output_str
        .lines()
        .find(|line| line.contains("\"subtype\":\"ask_user\""))
        .expect("ask_user control request");
    let request: serde_json::Value = serde_json::from_str(request_line).unwrap();
    assert_eq!(
        request
            .pointer("/request/question")
            .and_then(|v| v.as_str()),
        Some("How should local changes be handled?")
    );
    assert_eq!(
        request
            .pointer("/request/options/0/description")
            .and_then(|v| v.as_str()),
        Some("git stash")
    );
    assert_eq!(
        request
            .pointer("/request/allow_free_text")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        request
            .pointer("/request/multi_select")
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    assert_eq!(tool_result_text(&core, "call-ask"), "Stash");
    let event_types = core.audit.captured_event_types();
    assert!(event_types.contains(&"tool.execution.started"));
    assert!(event_types.contains(&"tool.completed"));
    // Answered questions count like any other tool call, so a single rejected
    // question cannot make the tool look like it always fails.
    assert_eq!(core.metrics.tool_calls_total, 1);
    assert_eq!(core.metrics.tool_calls_success, 1);
    assert_eq!(core.metrics.tool_calls_fail, 0);
}

#[tokio::test]
async fn malformed_tool_arguments_fail_without_executing_the_tool() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-shell".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"ls -l"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Retrying with valid arguments.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("list files", &mut reader, &mut output)
        .await
        .expect("turn completes");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "malformed arguments must not reach the tool"
    );
    let tool_text = tool_result_text(&core, "call-shell");
    assert!(
        tool_text.contains("code=invalid_json"),
        "expected a diagnosable tool error, got {tool_text}"
    );
    let event_types = core.audit.captured_event_types();
    assert!(event_types.contains(&"tool.requested"));
    assert!(event_types.contains(&"tool.failed"));
    assert!(!event_types.contains(&"tool.execution.started"));
}

/// Tools that take no parameters legitimately arrive with empty arguments, which
/// must stay executable after the malformed-argument tightening.
#[tokio::test]
async fn empty_arguments_still_invoke_a_regular_tool() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-shell".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Done.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("run it", &mut reader, &mut output)
        .await
        .expect("turn completes");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// The in-band `COSH_QUESTION:` text protocol shares the tool's validation, so a
/// schema-incompatible payload must not become a question — and because the
/// marker suppresses the assistant text, the turn must fail visibly instead of
/// ending as an ordinary reply the user never saw.
#[tokio::test]
async fn cosh_question_text_with_unsupported_schema_fails_visibly() {
    for (label, payload, expected_code) in [
        (
            "unsupported schema",
            r#"{"prompt":"How should local changes be handled?"}"#,
            "missing_question",
        ),
        (
            "explicit null question",
            r#"{"question":null}"#,
            "question_wrong_type",
        ),
        ("truncated json", r#"{"question":"How sho"#, "invalid_json"),
        ("no payload", "", "empty_arguments"),
        (
            "unanswerable question",
            r#"{"question":"Pick one","allow_free_text":false,"options":[]}"#,
            "no_answer_path",
        ),
    ] {
        let provider = MockProvider::new(vec![vec![
            GenerateEvent::TextDelta(format!("COSH_QUESTION:{payload}")),
            GenerateEvent::MessageEnd,
        ]]);

        let mut config = CoreConfig::default();
        config.agent.approval_mode = ApprovalMode::Trust;
        let tools = ToolRegistry::with_defaults_for_test();
        let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
        core.audit = CoreAuditRecorder::test_capture(&core.session_id);
        let mut reader = empty_reader().await;
        let mut output = Vec::new();

        let error = core
            .handle_user_message("what now?", &mut reader, &mut output)
            .await
            .expect_err("an invalid in-band question must fail the turn");

        assert!(
            error.contains(&format!("code={expected_code}")),
            "case {label}: expected code={expected_code} in {error}"
        );
        assert!(
            !error.contains(payload) || payload.is_empty(),
            "case {label}: the rejected payload must not be echoed: {error}"
        );

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            !output_str.contains(r#""subtype":"ask_user""#),
            "case {label}: {output_str}"
        );
        assert!(
            !output_str.contains("Agent needs your input"),
            "case {label}: {output_str}"
        );
        // The marker suppressed the text, so nothing may be presented as a
        // finished assistant answer either.
        assert!(
            !output_str.contains(r#""type":"assistant""#),
            "case {label}: suppressed text must not surface as an answer: {output_str}"
        );
    }
}

/// With the question tool disabled the marker cannot become a question, so the
/// text must stay visible instead of being suppressed with nothing to replace it.
#[tokio::test]
async fn cosh_question_text_stays_visible_when_questions_are_disabled() {
    let provider = MockProvider::new(vec![vec![
        GenerateEvent::TextDelta(
            "COSH_QUESTION:{\"prompt\":\"How should local changes be handled?\"}".to_string(),
        ),
        GenerateEvent::MessageEnd,
    ]]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    tools
        .retain_selected_tools("shell")
        .expect("selection drops the question tool");
    assert!(!tools.supports_ask_user_question());
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut reader = empty_reader().await;
    let mut output = Vec::new();

    core.handle_user_message("what now?", &mut reader, &mut output)
        .await
        .expect("turn completes as an ordinary reply");

    let output_str = String::from_utf8(output).unwrap();
    assert!(
        output_str.contains(r#""type":"assistant""#),
        "the reply must not be swallowed: {output_str}"
    );
    assert!(
        !output_str.contains(r#""subtype":"ask_user""#),
        "{output_str}"
    );
}

#[tokio::test]
async fn cosh_question_text_with_valid_schema_still_asks() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::TextDelta(
                "COSH_QUESTION:{\"question\":\"Which branch?\",\"options\":[{\"label\":\"main\"}]}"
                    .to_string(),
            ),
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("Using main.".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);

    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let tools = ToolRegistry::with_defaults_for_test();
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let input = "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"req-0\",\"response\":{\"answer\":\"main\"}}}\n";
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();

    core.handle_user_message("what now?", &mut reader, &mut output)
        .await
        .expect("turn completes");

    let output_str = String::from_utf8(output).unwrap();
    let request_line = output_str
        .lines()
        .find(|line| line.contains(r#""subtype":"ask_user""#))
        .expect("ask_user control request");
    let request: serde_json::Value = serde_json::from_str(request_line).unwrap();
    assert_eq!(
        request
            .pointer("/request/question")
            .and_then(|v| v.as_str()),
        Some("Which branch?")
    );
}

// ─── #1994: control transport failures must never precede a blocking read ───

/// How a control-request write fails.
#[derive(Clone, Copy)]
enum FailStep {
    /// The first `write` call fails outright.
    Write,
    /// The first `write` accepts a prefix, the next one fails: `write_all` has
    /// already put bytes on the wire, so delivery is genuinely unknown.
    PartialThenWrite,
    /// Writes succeed, `flush` fails.
    Flush,
}

/// A stdout whose write or flush fails, standing in for a broken pipe.
struct FailingWriter {
    fail_on: FailStep,
    written: Vec<u8>,
    writes: usize,
}

impl FailingWriter {
    fn new(fail_on: FailStep) -> Self {
        Self {
            fail_on,
            written: Vec::new(),
            writes: 0,
        }
    }

    fn broken_pipe(detail: &'static str) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, detail)
    }
}

impl std::io::Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writes += 1;
        match self.fail_on {
            FailStep::Write => Err(Self::broken_pipe("broken pipe")),
            FailStep::PartialThenWrite => {
                if self.writes == 1 && buf.len() > 1 {
                    let accepted = buf.len() / 2;
                    self.written.extend_from_slice(&buf[..accepted]);
                    Ok(accepted)
                } else {
                    Err(Self::broken_pipe("broken pipe after partial write"))
                }
            }
            FailStep::Flush => {
                self.written.extend_from_slice(buf);
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.fail_on {
            FailStep::Write | FailStep::PartialThenWrite => Ok(()),
            FailStep::Flush => Err(Self::broken_pipe("flush failed")),
        }
    }
}

#[test]
fn fatal_diagnostic_is_best_effort() {
    let mut failing = FailingWriter::new(FailStep::Write);
    emit_fatal_diagnostic(&mut failing, "transport failed");

    let mut output = Vec::new();
    emit_fatal_diagnostic(&mut output, "transport failed");
    assert_eq!(
        String::from_utf8(output).expect("diagnostic is UTF-8"),
        "cosh-core fatal: transport failed\n"
    );
}

/// A stdin that fails the test if the core reads it at all.
///
/// This is the #1994 assertion: once a control request could not be sent, the
/// core must return, not park on a read that only a dead peer could end.
struct NeverReadStdin;

impl tokio::io::AsyncRead for NeverReadStdin {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        panic!("core read stdin after a control request it failed to send");
    }
}

/// One shell call that needs approval, so the turn reaches the control request.
fn approval_provider() -> MockProvider {
    MockProvider::new(vec![vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: "call-1".to_string(),
            name: "shell".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: r#"{"command":"echo hi"}"#.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 0 },
        GenerateEvent::MessageEnd,
    ]])
}

fn approval_core(calls: Arc<AtomicUsize>) -> CoshCore {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool { calls }));
    let mut core = CoshCore::new_legacy(config, Box::new(approval_provider()), tools);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);
    core
}

async fn assert_approval_emit_failure_is_session_fatal(fail_on: FailStep, expected_reason: &str) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut core = approval_core(Arc::clone(&calls));
    let mut reader = tokio::io::BufReader::new(NeverReadStdin).lines();
    let mut writer = FailingWriter::new(fail_on);

    let error = core
        .handle_user_message("run echo hi", &mut reader, &mut writer)
        .await
        .expect_err("an unsent approval request must fail the turn");

    assert!(
        error.contains("control transport"),
        "turn error must name the transport: {error}"
    );
    assert!(
        core.control_transport_failure().is_some(),
        "the failure must be session-fatal so the process exits non-zero"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a tool whose approval request could not be sent must not run"
    );
    assert_eq!(
        core.metrics.approval_count, 0,
        "a request that never entered the wait is not an approval interaction"
    );
    assert_eq!(core.metrics.approval_allow, 0);
    assert_eq!(core.metrics.approval_deny, 0);

    // The approval still owes a terminal audit event, and it must be
    // distinguishable from a user decision.
    let events = core.audit.captured_events();
    let resolved = events
        .iter()
        .find(|event| event.event_type.as_str() == "approval.resolved")
        .expect("an unsent approval must still be audited as resolved");
    assert_eq!(
        resolved.data().get("decision").and_then(|v| v.as_str()),
        Some("emit_failed")
    );
    assert_eq!(
        resolved.data().get("reason_code").and_then(|v| v.as_str()),
        Some(expected_reason)
    );
    assert!(
        resolved.identity.request_id.is_some(),
        "the audit record must carry the request id"
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type.as_str() == "approval.requested"),
        "the request must still be audited before its failure"
    );
    // The turn owes a terminal event too, or audit shows a run that never ended.
    let turn_failed = events
        .iter()
        .find(|event| event.event_type.as_str() == "turn.failed")
        .expect("a transport-killed turn must be audited as failed");
    assert_eq!(
        turn_failed
            .data()
            .get("reason_code")
            .and_then(|v| v.as_str()),
        Some("control_transport_failed")
    );
    assert_eq!(
        events.last().map(|event| event.event_type.as_str()),
        Some("turn.failed"),
        "the turn terminal must follow every child lifecycle event"
    );
}

#[tokio::test]
async fn approval_write_failure_never_waits_for_a_response() {
    assert_approval_emit_failure_is_session_fatal(FailStep::Write, "control_transport_write").await;
}

#[tokio::test]
async fn approval_partial_write_then_failure_never_waits_for_a_response() {
    // Bytes did reach the wire, so delivery is unknown rather than absent. The
    // core must still stop instead of waiting for a decision it cannot get.
    assert_approval_emit_failure_is_session_fatal(
        FailStep::PartialThenWrite,
        "control_transport_write",
    )
    .await;
}

#[tokio::test]
async fn approval_flush_failure_never_waits_for_a_response() {
    assert_approval_emit_failure_is_session_fatal(FailStep::Flush, "control_transport_flush").await;
}

/// A PreToolUse hook that answers `ask`, so only the hook demands approval.
fn ask_hook(name: &str) -> crate::config::HookDefinition {
    crate::config::HookDefinition {
        command: r#"python3 -c 'print("""{"decision":"ask","reason":"needs review"}""")'"#
            .to_string(),
        name: Some(name.to_string()),
        matcher: None,
        timeout: Some(10_000),
        sequential: None,
        fail_open: false,
        env: Default::default(),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn hook_ask_writes_a_complete_can_use_tool_line_to_a_real_pipe() {
    // The #1994 report claimed the request never reached stdout. Over a real
    // kernel transport, with a stdin that never answers, the full JSONL record
    // is there.
    let provider = approval_provider();
    let mut config = CoreConfig::default();
    // Trust mode: only the hook asks, so this also pins that a hook `ask`
    // cannot be auto-approved away.
    config.agent.approval_mode = ApprovalMode::Trust;
    config.hooks.enabled = true;
    config.hooks.pre_tool_use = vec![ask_hook("ask-hook")];
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    // A socket pair rather than `std::io::pipe`: same kernel-buffered
    // byte stream, without raising the toolchain this crate needs.
    let (pipe_reader, pipe_writer) = std::os::unix::net::UnixStream::pair().expect("socket pair");
    let mut writer = std::io::BufWriter::new(pipe_writer);
    // No approval response ever arrives: EOF, not a decision.
    let mut reader = empty_reader().await;

    core.handle_user_message("run echo hi", &mut reader, &mut writer)
        .await
        .expect("an unanswered approval ends the turn as interrupted, not as an error");

    drop(writer);
    let mut lines = std::io::BufRead::lines(std::io::BufReader::new(pipe_reader));
    let request = lines
        .by_ref()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        .find(|value| value["request"]["subtype"] == "can_use_tool")
        .expect("can_use_tool must reach the pipe even though stdin never answers");
    assert_eq!(request["request"]["tool_name"], "shell");
    assert_eq!(
        request["request"]["hook_requires_approval"], true,
        "a hook ask must be shown, not auto-approved in trust mode"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no decision arrived, so the tool must not have run"
    );
    assert!(
        core.control_transport_failure().is_none(),
        "a healthy pipe is not a transport failure"
    );
}

#[tokio::test]
async fn evidence_emit_failure_never_waits_for_a_response() {
    let core = make_core(MockProvider::new(vec![]));
    let mut reader = tokio::io::BufReader::new(NeverReadStdin).lines();
    let mut writer = FailingWriter::new(FailStep::Write);

    let result = core
        .handle_shell_evidence(
            "call-evidence",
            &serde_json::json!({"action":"list_commands"}),
            &mut reader,
            &mut writer,
        )
        .await;

    assert!(result.is_error);
    assert!(
        result.output.contains("delivery could not be confirmed"),
        "{}",
        result.output
    );
    assert!(
        core.control_transport_failure().is_some(),
        "the transport failure must end the session, not just this tool call"
    );
}

#[tokio::test]
async fn question_emit_failure_never_waits_for_an_answer() {
    let core = make_core(MockProvider::new(vec![]));
    let mut reader = tokio::io::BufReader::new(NeverReadStdin).lines();
    let mut writer = FailingWriter::new(FailStep::Flush);

    let result = core
        .handle_ask_user(
            &crate::tool::ask_user_question::AskUserQuestionParams {
                question: "Which branch?".to_string(),
                options: vec![],
                allow_free_text: true,
                multi_select: false,
            },
            None,
            &mut reader,
            &mut writer,
        )
        .await;

    assert!(result.is_error);
    assert!(
        result.output.contains("delivery could not be confirmed"),
        "{}",
        result.output
    );
    assert!(core.control_transport_failure().is_some());
}

#[tokio::test]
async fn evidence_emit_failure_ends_the_turn_and_pairs_the_history() {
    let provider = MockProvider::new(vec![vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: "call-evidence".to_string(),
            name: "cosh_shell_evidence".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: r#"{"action":"list_commands"}"#.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 0 },
        GenerateEvent::MessageEnd,
    ]]);
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(crate::tool::shell_evidence::ShellEvidenceTool));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    let mut reader = tokio::io::BufReader::new(NeverReadStdin).lines();
    let mut writer = FailingWriter::new(FailStep::Write);

    let error = core
        .handle_user_message("what ran?", &mut reader, &mut writer)
        .await
        .expect_err("a dead transport must end the turn");

    assert!(error.contains("control transport"), "{error}");
    // Every declared call still owes a result, or the persisted transcript
    // cannot be replayed.
    let tool_results = core
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .count();
    assert_eq!(tool_results, 1);
}

#[tokio::test]
async fn user_prompt_submit_ask_emit_failure_never_waits_for_a_response() {
    // The prompt-level hook panel gates a wait exactly like the tool-level one,
    // and it happens before any transcript entry exists.
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.hooks.enabled = true;
    config.hooks.user_prompt_submit = vec![ask_hook("prompt-ask-hook")];
    let mut core = CoshCore::new_legacy(
        config,
        Box::new(MockProvider::text_only("must never be reached")),
        ToolRegistry::new(),
    );
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);
    let mut reader = tokio::io::BufReader::new(NeverReadStdin).lines();
    let mut writer = FailingWriter::new(FailStep::Write);

    let error = core
        .handle_user_message("do something", &mut reader, &mut writer)
        .await
        .expect_err("an unsent prompt approval must fail the turn");

    assert!(error.contains("control transport"), "{error}");
    assert!(core.control_transport_failure().is_some());
    assert!(
        core.messages.is_empty(),
        "the prompt was never approved, so it must not enter the transcript"
    );
    let resolved = core
        .audit
        .captured_events()
        .iter()
        .find(|event| event.event_type.as_str() == "approval.resolved")
        .expect("the prompt approval owes a terminal event")
        .clone();
    assert_eq!(
        resolved.data().get("decision").and_then(|v| v.as_str()),
        Some("emit_failed")
    );
}

#[tokio::test]
async fn transport_failure_on_one_call_skips_the_rest_of_the_batch() {
    // The evidence path can only report the failure through the session flag,
    // so without promoting it at the loop boundary the second call would run.
    let provider = MockProvider::new(vec![vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: "call-evidence".to_string(),
            name: "cosh_shell_evidence".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: r#"{"action":"list_commands"}"#.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 0 },
        GenerateEvent::ToolCallStart {
            index: 1,
            id: "call-shell".to_string(),
            name: "shell".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 1,
            arguments_delta: r#"{"command":"echo hi"}"#.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 1 },
        GenerateEvent::MessageEnd,
    ]]);
    let mut config = CoreConfig::default();
    // Trust mode: the shell call would otherwise stop at an approval instead of
    // proving that a skipped call is what kept it from running.
    config.agent.approval_mode = ApprovalMode::Trust;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(crate::tool::shell_evidence::ShellEvidenceTool));
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);
    let mut reader = tokio::io::BufReader::new(NeverReadStdin).lines();
    let mut writer = FailingWriter::new(FailStep::Write);

    let error = core
        .handle_user_message("what ran?", &mut reader, &mut writer)
        .await
        .expect_err("a dead transport must end the turn");

    assert!(error.contains("control transport"), "{error}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the second call must be skipped, not executed on a dead transport"
    );
    // Both declared calls still owe a result.
    assert_eq!(
        core.messages
            .iter()
            .filter(|message| message.role == "tool")
            .count(),
        2
    );
    assert_eq!(
        core.audit
            .captured_events()
            .last()
            .map(|event| event.event_type.as_str()),
        Some("turn.failed"),
        "all skipped tool terminals must precede the turn terminal"
    );
}

#[tokio::test]
async fn audit_failure_after_transport_failure_still_pairs_the_history() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut core = approval_core(Arc::clone(&calls));
    // Required mode with a sink that always fails: the terminal approval record
    // cannot be persisted either.
    core.audit = CoreAuditRecorder::test_capture_except(
        &core.session_id,
        cosh_types::audit::KnownAuditEventType::ApprovalResolved,
    );
    let mut reader = tokio::io::BufReader::new(NeverReadStdin).lines();
    let mut writer = FailingWriter::new(FailStep::Write);

    let error = core
        .handle_user_message("run echo hi", &mut reader, &mut writer)
        .await
        .expect_err("both failures are session-fatal");

    assert!(
        error.contains("control transport") && error.contains("audit record failed"),
        "the turn error must report both failures: {error}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // The point of the fix: the audit error must not escape before the declared
    // tool call has its result, because headless persists this transcript.
    assert_eq!(
        core.messages
            .iter()
            .filter(|message| message.role == "tool")
            .count(),
        1,
        "every declared tool call must still be answered: {:?}",
        core.messages
    );
}

// ─── #2067: trust-mode shell handoff reroute & blocked-call release ───

fn trust_shell_core(calls: Arc<AtomicUsize>, provider: MockProvider) -> CoshCore {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool { calls }));
    CoshCore::new_legacy(config, Box::new(provider), tools)
}

fn fully_capable_client() -> ClientControlCapabilities {
    ClientControlCapabilities {
        can_handle_can_use_tool: true,
        can_handle_host_executed_shell: true,
    }
}

#[test]
fn trust_classify_reroutes_shell_for_fully_capable_client() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut core = trust_shell_core(calls, MockProvider::new(vec![]));
    core.client_capabilities = fully_capable_client();
    assert!(matches!(
        core.classify_tool("shell", &serde_json::json!({"command":"echo hi"})),
        Outcome::RequireApproval
    ));
}

#[test]
fn trust_classify_keeps_shell_local_without_full_capabilities() {
    let params = serde_json::json!({"command":"echo hi"});

    // Legacy client: no initialize capabilities at all.
    let calls = Arc::new(AtomicUsize::new(0));
    let core = trust_shell_core(calls, MockProvider::new(vec![]));
    assert!(matches!(
        core.classify_tool("shell", &params),
        Outcome::Allow
    ));

    // Half-capable is not capable: both halves of the exchange are required.
    let calls = Arc::new(AtomicUsize::new(0));
    let mut core = trust_shell_core(calls, MockProvider::new(vec![]));
    core.client_capabilities = ClientControlCapabilities {
        can_handle_can_use_tool: true,
        can_handle_host_executed_shell: false,
    };
    assert!(matches!(
        core.classify_tool("shell", &params),
        Outcome::Allow
    ));
}

#[test]
fn trust_classify_keeps_non_shell_tools_local_for_capable_client() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ExternalTool));
    let mut core = CoshCore::new_legacy(config, Box::new(MockProvider::new(vec![])), tools);
    core.client_capabilities = fully_capable_client();
    assert!(matches!(
        core.classify_tool("example.ops/mcp/server/tool", &serde_json::json!({})),
        Outcome::Allow
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn trust_capable_client_shell_call_requests_approval_without_any_hook() {
    // No hooks configured: in trust mode the reroute alone must raise the
    // approval request, and it must not be flagged as hook-driven.
    let provider = approval_provider();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut core = trust_shell_core(Arc::clone(&calls), provider);
    core.client_capabilities = fully_capable_client();

    let (pipe_reader, pipe_writer) = std::os::unix::net::UnixStream::pair().expect("socket pair");
    let mut writer = std::io::BufWriter::new(pipe_writer);
    let mut reader = empty_reader().await;

    core.handle_user_message("run echo hi", &mut reader, &mut writer)
        .await
        .expect("an unanswered approval ends the turn as interrupted, not as an error");

    drop(writer);
    let mut lines = std::io::BufRead::lines(std::io::BufReader::new(pipe_reader));
    let request = lines
        .by_ref()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        .find(|value| value["request"]["subtype"] == "can_use_tool")
        .expect("trust-mode shell call must be rerouted to can_use_tool for a capable client");
    assert_eq!(request["request"]["tool_name"], "shell");
    assert_eq!(request["request"]["tool_use_id"], "call-1");
    // `hook_requires_approval` skips serialization when false, so the wire
    // field must be absent here: the reroute is policy-driven, not hook-driven.
    assert!(request["request"]["hook_requires_approval"].is_null());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no decision arrived, so the tool must not have run"
    );
}

#[tokio::test]
async fn hook_block_releases_staged_call_with_provider_native_result() {
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-block".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"touch /tmp/should-not-exist"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        vec![
            GenerateEvent::TextDelta("blocked acknowledged".to_string()),
            GenerateEvent::MessageEnd,
        ],
    ]);
    let calls = Arc::new(AtomicUsize::new(0));
    // Hooks are bound into the HookSystem at construction time, so the block
    // hook must be in the config handed to `CoshCore::new_legacy`.
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.hooks = config::HooksConfig {
        enabled: true,
        pre_tool_use: vec![config::HookDefinition {
            command: "echo '{\"decision\":\"block\",\"reason\":\"no touch\"}'".to_string(),
            name: Some("block-shell".to_string()),
            matcher: Some("shell".to_string()),
            timeout: Some(5000),
            sequential: None,
            fail_open: false,
            env: Default::default(),
        }],
        ..Default::default()
    };
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShellTool {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let mut reader = empty_reader().await;
    let mut output = Vec::new();
    core.handle_user_message("touch it", &mut reader, &mut output)
        .await
        .unwrap();

    let output_str = String::from_utf8(output).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a blocked command must never execute"
    );
    assert!(
        output_str.contains(r#""type":"tool_result""#),
        "the blocked call must be released with a provider-native tool result: {output_str}"
    );
    assert!(
        output_str.contains(r#""tool_use_id":"call-block""#),
        "{output_str}"
    );
    assert!(
        output_str.contains("Blocked by hook: no touch"),
        "{output_str}"
    );
    assert!(
        output_str.contains(r#""cosh_hook_verdict":"blocked""#),
        "the blocked release must carry the machine-readable verdict marker: {output_str}"
    );
    assert!(
        !output_str.contains("can_use_tool"),
        "a hook block is a verdict, not an approval request: {output_str}"
    );
    assert!(
        output_str.contains("blocked acknowledged"),
        "the turn must continue after the blocked result reaches the LLM: {output_str}"
    );
    let blocked_results = core
        .messages
        .iter()
        .filter(|m| m.tool_call_id.as_deref() == Some("call-block"))
        .count();
    assert_eq!(
        blocked_results, 1,
        "the LLM must see the blocked result exactly once"
    );
}
