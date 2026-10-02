use super::hooks::{group_agent_hooks, render_hooks_command, shell_hook_entries};
use crate::hooks::engine::{HookSourceInfo, RegisteredHookInfo};
use crate::hooks::state::{RuntimeHookDisplay, RuntimeHookFinding};
use crate::runtime::prelude::*;
use std::path::PathBuf;

#[test]
fn agent_hooks_group_by_event_in_lifecycle_order() {
    // Mirrors the #1713 reproduction: pii-checker registers five events and
    // observability-hook seven; the flat rendering scattered them across the
    // panel. Grouping must list each event once, hooks beneath it.
    let data = serde_json::json!([
        {"name": "skill-ledger", "event": "PreToolUse", "extension": "agent-sec-core", "disabled": false},
        {"name": "pii-checker", "event": "PreToolUse", "extension": "agent-sec-core", "disabled": true},
        {"name": "pii-checker", "event": "AfterModel", "extension": "agent-sec-core", "disabled": true},
        {"name": "observability-hook", "event": "Stop", "extension": "agent-sec-core", "disabled": false},
        {"name": "observability-hook", "event": "PreToolUse", "extension": "agent-sec-core", "disabled": false},
        {"name": "tokenless-compress-schema", "event": "BeforeModel", "extension": "tokenless", "disabled": false},
        {"name": "mystery", "event": "CustomEvent", "extension": "x", "disabled": false},
    ]);

    let groups = group_agent_hooks(&data);

    // Event groups appear exactly once, in lifecycle order, unknown trailing.
    let events: Vec<&str> = groups.iter().map(|group| group.event.as_str()).collect();
    assert_eq!(
        events,
        vec![
            "PreToolUse",
            "BeforeModel",
            "AfterModel",
            "Stop",
            "CustomEvent"
        ],
        "{events:?}"
    );

    // Hooks of one event sit contiguously in registry order with state kept.
    let pre_tool = &groups[0];
    let names: Vec<(&str, bool)> = pre_tool
        .hooks
        .iter()
        .map(|hook| (hook.name.as_str(), hook.disabled))
        .collect();
    assert_eq!(
        names,
        vec![
            ("skill-ledger", false),
            ("pii-checker", true),
            ("observability-hook", false)
        ]
    );
    assert_eq!(pre_tool.hooks[0].detail, "ext: agent-sec-core");
}

#[test]
fn shell_hook_entries_order_and_formatting() {
    let mut state = InlineState::default();
    state.hooks.disabled.insert("user-hook".to_string());

    let hooks = vec![
        RegisteredHookInfo {
            id: "proj-untrusted".to_string(),
            source: HookSourceInfo::ExternalProject,
            path: Some(PathBuf::from("/proj/.cosh/hooks/x.sh")),
            project_root: Some(PathBuf::from("/proj")),
            trusted: Some(false),
        },
        RegisteredHookInfo {
            id: "builtin-a".to_string(),
            source: HookSourceInfo::Builtin,
            path: None,
            project_root: None,
            trusted: None,
        },
        RegisteredHookInfo {
            id: "user-hook".to_string(),
            source: HookSourceInfo::ExternalUser,
            path: Some(PathBuf::from("/home/x/.cosh/hooks/user-hook.sh")),
            project_root: None,
            trusted: None,
        },
        RegisteredHookInfo {
            id: "proj-trusted".to_string(),
            source: HookSourceInfo::ExternalProject,
            path: Some(PathBuf::from("/proj/.cosh/hooks/y.sh")),
            project_root: Some(PathBuf::from("/proj")),
            trusted: Some(true),
        },
    ];

    let entries = shell_hook_entries(&state, &hooks);

    // Order is fixed: builtin, then user, then project; original order kept
    // within each source.
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].name, "builtin-a");
    assert_eq!(entries[0].detail, "builtin");
    assert!(!entries[0].disabled);

    assert_eq!(entries[1].name, "user-hook");
    assert_eq!(entries[1].detail, "user: user-hook.sh");
    assert!(entries[1].disabled);

    assert_eq!(entries[2].name, "proj-untrusted");
    assert_eq!(entries[2].detail, "project: x.sh, untrusted");
    assert!(
        !entries[2].disabled,
        "untrusted trust state must not imply the disabled marker"
    );

    assert_eq!(entries[3].name, "proj-trusted");
    assert_eq!(entries[3].detail, "project: y.sh");
    assert!(!entries[3].disabled);
}

#[test]
fn shell_hook_entries_empty_list() {
    let state = InlineState::default();
    assert!(shell_hook_entries(&state, &[]).is_empty());
}

#[test]
fn shell_hook_entries_missing_path_fallback() {
    let state = InlineState::default();
    let hooks = vec![
        RegisteredHookInfo {
            id: "no-path-user".to_string(),
            source: HookSourceInfo::ExternalUser,
            path: None,
            project_root: None,
            trusted: None,
        },
        RegisteredHookInfo {
            id: "no-path-project".to_string(),
            source: HookSourceInfo::ExternalProject,
            path: None,
            project_root: Some(PathBuf::from("/proj")),
            trusted: Some(false),
        },
    ];

    let entries = shell_hook_entries(&state, &hooks);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].detail, "user: ?");
    assert_eq!(entries[1].detail, "project: ?, untrusted");
    // Trust is orthogonal to the disable marker.
    assert!(!entries[1].disabled);
}

#[test]
fn hooks_root_renders_status_with_source_paths_and_trust() {
    let user_hook = TestHookFixture::new("hooks-status-user");
    let project_hook = TestHookFixture::new("hooks-status-project");
    let mut state = InlineState::default();
    // Session-disable the user hook to cover the session-disable
    // dimension. The untrusted project hook keeps the baseline
    // orthogonal semantics: counted as enabled, trust conveyed via
    // the suffix only.
    state.hooks.disabled.insert("user-hook".to_string());
    state.hooks.engine.register_external(ExternalHookConfig {
        path: user_hook.path.clone(),
        matcher: HookMatcher {
            id: "user-hook".to_string(),
            commands: vec!["echo".to_string()],
            command_patterns: Vec::new(),
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
        timeout_ms: 1000,
        source: ExternalHookSource::User,
        project_root: None,
        trusted: true,
    });
    state.hooks.engine.register_external(ExternalHookConfig {
        path: project_hook.path.clone(),
        matcher: HookMatcher {
            id: "project-hook".to_string(),
            commands: vec!["echo".to_string()],
            command_patterns: Vec::new(),
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
        timeout_ms: 1000,
        source: ExternalHookSource::Project,
        project_root: Some(project_hook.root.clone()),
        trusted: false,
    });

    let rendered = render_hooks_test_command(None, None, None, &mut state);

    assert!(rendered.contains("Hook status"), "{rendered}");
    assert!(
        rendered.contains("Registered: 2; enabled: 1; disabled: 1."),
        "aggregate keeps session-disable-only counting: {rendered}"
    );
    assert!(
        rendered.contains("Sources: builtin=0; user=1; project=1."),
        "{rendered}"
    );
    assert!(
        rendered.contains("Project trust: trusted=0; untrusted=1."),
        "{rendered}"
    );
    // UX contract: each shell hook entry shows only its script file name
    // plus trust state — the basename identifies the hook without
    // leaking home/project directory layout, mirroring how agent-hook
    // entries show the extension name only. Disable and trust stay
    // orthogonal: only the session-disabled user hook carries
    // [disabled], while the untrusted project hook's trust state
    // lives in its suffix.
    assert!(
        rendered.contains("user: project.sh"),
        "expected user hook file name: {rendered}"
    );
    assert!(
        rendered.contains("project: project.sh, untrusted"),
        "expected project hook file name and trust state: {rendered}"
    );
    assert!(
        rendered.contains("(user: project.sh) [disabled]"),
        "session-disabled entry must carry the disabled marker: {rendered}"
    );
    assert!(
        !rendered.contains("untrusted) [disabled]"),
        "untrusted trust state must not imply the disabled marker: {rendered}"
    );
    assert!(
        !rendered.contains(".cosh/hooks"),
        "full source paths must not leak into the panel: {rendered}"
    );
    assert!(
        !rendered.contains("hooks-status-user"),
        "fixture directory name must not leak: {rendered}"
    );
}

#[test]
fn agent_hooks_grouping_handles_empty_and_malformed_payloads() {
    assert!(group_agent_hooks(&serde_json::json!([])).is_empty());
    assert!(group_agent_hooks(&serde_json::json!({"not": "an array"})).is_empty());
    // Records without a name are skipped entirely.
    assert!(group_agent_hooks(&serde_json::json!([{"event": "PreToolUse"}])).is_empty());
}

struct EnvLock {
    path: std::path::PathBuf,
}

pub(super) struct TestHookFixture {
    pub(super) root: std::path::PathBuf,
    pub(super) path: std::path::PathBuf,
}

impl TestHookFixture {
    pub(super) fn new(label: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("cosh-shell-slash-{label}-{}", std::process::id()));
        let hooks_dir = root.join(".cosh/hooks");
        let path = hooks_dir.join("project.sh");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&hooks_dir).expect("create test hook directory");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write test hook");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make test hook executable");
        Self { root, path }
    }
}

impl Drop for TestHookFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Drop for EnvLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn zh_state() -> InlineState {
    InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    }
}

fn env_lock() -> EnvLock {
    let path =
        std::env::temp_dir().join(format!("cosh-shell-test-env-{}.lock", std::process::id()));
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return EnvLock { path },
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(err) => panic!("create env test lock failed: {err}"),
        }
    }
}

fn register_project_hook(state: &mut InlineState) -> TestHookFixture {
    let fixture = TestHookFixture::new("hooks-trust");
    state.hooks.engine.register_external(ExternalHookConfig {
        path: fixture.path.clone(),
        matcher: HookMatcher {
            id: "project-hook".to_string(),
            commands: vec!["echo".to_string()],
            command_patterns: Vec::new(),
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
        timeout_ms: 1000,
        source: ExternalHookSource::Project,
        project_root: Some(fixture.root.clone()),
        trusted: false,
    });
    fixture
}

fn hook_hint() -> RuntimeHookFinding {
    RuntimeHookFinding {
        id: "hook-cmd-1-memory-pressure".to_string(),
        command_block_id: "cmd-1".to_string(),
        command: "free -m".to_string(),
        output_ref: Some("/tmp/out".to_string()),
        ended_at_ms: 200,
        prompt_hint: "hook_finding=memory-pressure".to_string(),
        finding_markdown: None,
        hook_finding: None,
        recommended_skill: Some("memory-analysis".to_string()),
        display: RuntimeHookDisplay::Hint,
        display_reason: "allowed".to_string(),
        related_hook_ids: Vec::new(),
        topic: "memory".to_string(),
        entity_key: "system-memory".to_string(),
        effective_severity: FindingSeverity::Critical,
        confidence: "high".to_string(),
        suppression_key: "memory:memory-pressure:free".to_string(),
    }
}

fn render_hooks_test_command(
    sub: Option<&str>,
    arg: Option<&str>,
    extra: Option<&str>,
    state: &mut InlineState,
) -> String {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    render_hooks_command(sub, arg, extra, &[], &adapter, state, &mut output)
        .expect("render hooks command");
    String::from_utf8(output).expect("utf8 output")
}

#[test]
fn hooks_empty_list_uses_zh_catalog_text() {
    let mut state = zh_state();

    let output = render_hooks_test_command(None, None, None, &mut state);

    assert!(output.contains("Hook 状态"), "{output}");
    assert!(output.contains("未注册 Hook。"), "{output}");
    assert!(output.contains("已注册 0 个 Hook。"), "{output}");
    assert!(!output.contains("Hook status"), "{output}");
    assert!(!output.contains("No hooks registered"), "{output}");
}

#[test]
fn hooks_session_actions_use_zh_catalog_text() {
    let mut state = zh_state();

    let muted = render_hooks_test_command(Some("mute"), Some("memory"), None, &mut state);
    assert!(muted.contains("Hook 目标已静音"), "{muted}");
    assert!(
        muted.contains("本会话已静音 Hook 目标 'memory'。"),
        "{muted}"
    );
    assert!(!muted.contains("Hook target muted"), "{muted}");

    let unmuted = render_hooks_test_command(Some("unmute"), Some("memory"), None, &mut state);
    assert!(unmuted.contains("Hook 目标已取消静音"), "{unmuted}");
    assert!(
        unmuted.contains("已取消静音 Hook 目标 'memory'。"),
        "{unmuted}"
    );
    assert!(!unmuted.contains("Hook target unmuted"), "{unmuted}");

    let enabled = render_hooks_test_command(Some("enable"), Some("linux-memory"), None, &mut state);
    assert!(enabled.contains("Hook 已启用"), "{enabled}");
    assert!(
        enabled.contains("Hook 'linux-memory' 已启用。"),
        "{enabled}"
    );
    assert!(!enabled.contains("Hook enabled"), "{enabled}");

    let disabled =
        render_hooks_test_command(Some("disable"), Some("linux-memory"), None, &mut state);
    assert!(disabled.contains("Hook 已禁用"), "{disabled}");
    assert!(
        disabled.contains("Hook 'linux-memory' 已禁用。"),
        "{disabled}"
    );
    assert!(!disabled.contains("Hook disabled"), "{disabled}");
}

#[test]
fn hooks_history_and_events_empty_state_use_zh_catalog_text() {
    let mut state = zh_state();

    let history = render_hooks_test_command(Some("history"), None, None, &mut state);
    assert!(history.contains("Hook 历史"), "{history}");
    assert!(history.contains("本会话未记录 Hook finding。"), "{history}");
    assert!(!history.contains("No hook findings recorded"), "{history}");

    let events = render_hooks_test_command(Some("events"), None, None, &mut state);
    assert!(events.contains("Hook 显示事件"), "{events}");
    assert!(events.contains("本会话未记录 Hook 显示事件。"), "{events}");
    assert!(
        !events.contains("No hook display events recorded"),
        "{events}"
    );
}

#[test]
fn hooks_help_and_unknown_command_list_subcommands_in_zh() {
    for subcommand in ["--help", "bogus"] {
        let mut state = zh_state();
        let output = render_hooks_test_command(Some(subcommand), None, None, &mut state);

        assert!(output.contains("用法"), "{output}");
        assert!(
            output.contains("/hooks                - 显示 Hook 状态"),
            "{output}"
        );
        assert!(output.contains("/hooks history"), "{output}");
        assert!(output.contains("/hooks enable <id>"), "{output}");
        assert!(output.contains("/hooks disable <id>"), "{output}");
        assert!(output.contains("/hooks clear-project-trust"), "{output}");
        assert!(
            output.contains("/hooks feedback noisy|useful <id>"),
            "{output}"
        );
        assert!(!output.contains("show hook status"), "{output}");
        assert!(
            !output.contains("clear project hook trust store"),
            "{output}"
        );
    }
}

#[test]
fn hooks_project_trust_empty_state_uses_zh_catalog_text() {
    let mut state = zh_state();

    let output = render_hooks_test_command(Some("trust-project"), None, None, &mut state);

    assert!(output.contains("项目 Hook 已信任"), "{output}");
    assert!(output.contains("本会话未注册项目 Hook。"), "{output}");
    assert!(output.contains("信任状态未变更。"), "{output}");
    assert!(!output.contains("Project hooks trusted"), "{output}");
    assert!(
        !output.contains("No project hooks are registered"),
        "{output}"
    );
}

#[test]
fn hooks_feedback_errors_use_zh_catalog_text() {
    let mut state = zh_state();

    let usage = render_hooks_test_command(Some("feedback"), Some("bad"), Some("id-1"), &mut state);
    assert!(usage.contains("用法"), "{usage}");
    assert!(
        usage.contains("/hooks feedback noisy|useful <finding_id>"),
        "{usage}"
    );

    let missing =
        render_hooks_test_command(Some("feedback"), Some("noisy"), Some("missing"), &mut state);
    assert!(missing.contains("Hook 反馈"), "{missing}");
    assert!(
        missing.contains("本会话未找到 finding 'missing'。"),
        "{missing}"
    );
    assert!(
        missing.contains("使用 /hooks history 复制最近的 finding id。"),
        "{missing}"
    );
    assert!(
        !missing.contains("Finding 'missing' was not found"),
        "{missing}"
    );
}

#[test]
fn hooks_project_trust_uses_zh_catalog_text() {
    let _env_lock = env_lock();
    let store = std::env::temp_dir().join(format!(
        "cosh-shell-slash-hooks-trust-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store);
    std::env::set_var("COSH_SHELL_PROJECT_TRUST_STORE", &store);
    let mut state = zh_state();
    let _hook = register_project_hook(&mut state);

    let trusted = render_hooks_test_command(Some("trust-project"), None, None, &mut state);
    assert!(trusted.contains("项目 Hook 已信任"), "{trusted}");
    assert!(
        trusted.contains("已将 1 个项目 Hook 标记为 trusted。"),
        "{trusted}"
    );
    assert!(
        trusted.contains("信任已持久化；已禁用 Hook 保持禁用。"),
        "{trusted}"
    );
    assert!(!trusted.contains("Project hooks trusted"), "{trusted}");

    let cleared = render_hooks_test_command(Some("clear-project-trust"), None, None, &mut state);
    assert!(cleared.contains("项目 Hook 信任已清除"), "{cleared}");
    assert!(
        cleared.contains("已将 1 个项目 Hook 标记为 untrusted。"),
        "{cleared}"
    );
    assert!(cleared.contains("项目 Hook 信任存储已清除"), "{cleared}");
    assert!(!cleared.contains("Project hook trust cleared"), "{cleared}");

    std::env::remove_var("COSH_SHELL_PROJECT_TRUST_STORE");
    let _ = std::fs::remove_file(&store);
}

#[test]
fn hooks_feedback_uses_zh_catalog_text() {
    let _env_lock = env_lock();
    let store = std::env::temp_dir().join(format!(
        "cosh-shell-slash-hooks-feedback-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store);
    std::env::set_var("COSH_SHELL_HOOK_FEEDBACK_STORE", &store);
    let mut state = zh_state();

    let usage = render_hooks_test_command(
        Some("feedback"),
        Some("bad"),
        Some("finding-id"),
        &mut state,
    );
    assert!(usage.contains("用法"), "{usage}");
    assert!(
        usage.contains("/hooks feedback noisy|useful <finding_id>"),
        "{usage}"
    );

    let missing = render_hooks_test_command(
        Some("feedback"),
        Some("noisy"),
        Some("missing-id"),
        &mut state,
    );
    assert!(missing.contains("Hook 反馈"), "{missing}");
    assert!(
        missing.contains("本会话未找到 finding 'missing-id'。"),
        "{missing}"
    );
    assert!(
        !missing.contains("Finding 'missing-id' was not found"),
        "{missing}"
    );

    state.hooks.findings.push(hook_hint());
    let recorded = render_hooks_test_command(
        Some("feedback"),
        Some("noisy"),
        Some("hook-cmd-1-memory-pressure"),
        &mut state,
    );
    assert!(recorded.contains("Hook 反馈已记录"), "{recorded}");
    assert!(
        recorded.contains("已为 finding 'hook-cmd-1-memory-pressure' 记录反馈 'noisy'。"),
        "{recorded}"
    );
    assert!(
        recorded.contains("反馈已持久化，仅影响展示策略。"),
        "{recorded}"
    );
    assert!(!recorded.contains("Hook feedback recorded"), "{recorded}");

    let cleared = render_hooks_test_command(Some("clear-feedback"), None, None, &mut state);
    assert!(cleared.contains("Hook 反馈已清除"), "{cleared}");
    assert!(
        cleared.contains("已从本会话清除 1 条反馈偏好。"),
        "{cleared}"
    );
    assert!(cleared.contains("Hook 反馈偏好已清除。"), "{cleared}");
    assert!(!cleared.contains("Hook feedback cleared"), "{cleared}");

    std::env::remove_var("COSH_SHELL_HOOK_FEEDBACK_STORE");
    let _ = std::fs::remove_file(&store);
}
