use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{render_inline_guidance, shell_has_active_foreground_command};
use crate::runtime::prelude::{
    default_builtin_hooks, AdapterInstance, ExternalHookConfig, ExternalHookSource,
    FakeAgentAdapter, HookEngine, HookMatcher, HookTrigger, ShellEvent, ShellEventKind,
};
use crate::runtime::state::InlineState;

const TOP_MEMORY_PRESSURE_OUTPUT: &str = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
MiB Mem :  32768.0 total,   1400.0 free,  30200.0 used,   2188.0 buff/cache
MiB Swap:   8192.0 total,   2992.0 free,   5200.0 used.   1400.0 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 5120000   2.3g  100m S  12.0  45.2   1:23.45 java
";

const PS_HIGH_MEMORY_OUTPUT: &str = "\
USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
root      1234  3.1 45.2 5120000 2376420 ?     Sl   10:00   1:23 java -jar app.jar
";

fn state_with_builtin_hooks() -> InlineState {
    let mut state = InlineState::default();
    let mut hook_engine = HookEngine::new();
    for hook in default_builtin_hooks() {
        hook_engine.register(hook);
    }
    state.hooks.engine = hook_engine;
    state
}

fn write_hook_output(name: &str, content: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-hook-{name}-{}-{}.txt",
        std::process::id(),
        content.len()
    ));
    fs::write(&path, content).expect("write hook output");
    path.to_string_lossy().to_string()
}

#[cfg(unix)]
fn unique_hook_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "cosh-shell-runtime-hook-{name}-{}-{nanos}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn write_executable_hook_at(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).expect("create hook dir");
    let path = dir.join("hook.sh");
    fs::write(&path, body).expect("write hook script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod hook script");
    path
}

#[cfg(unix)]
fn write_executable_hook(name: &str, body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = unique_hook_dir(name);
    let path = write_executable_hook_at(&dir, body);
    (dir, path)
}

fn command_events(command: &str, output_ref: &str, output_bytes: u64) -> Vec<ShellEvent> {
    let mut finished = ShellEvent::command_finished(
        ShellEventKind::CommandCompleted,
        "test-session",
        "cmd-1",
        0,
        200,
        output_ref,
    );
    finished.terminal_output_bytes = Some(output_bytes);
    vec![
        ShellEvent::command_started("test-session", "cmd-1", command, "/tmp", 100),
        finished,
    ]
}

#[test]
fn inline_natural_language_intercept_waits_for_open_command_to_finish() {
    let mut intercept = ShellEvent::user_input_intercepted("test-session", "你好");
    intercept.component = Some("natural_language".to_string());
    intercept.started_at_ms = Some(120);
    let mut events = vec![
        ShellEvent::command_started("test-session", "cmd-1", "sleep 30", "/tmp", 100),
        intercept,
    ];

    let mut state = InlineState::default();
    let mut output = Vec::new();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("render inline guidance");

    assert!(output.is_empty());
    assert!(shell_has_active_foreground_command(&events));

    events.push(ShellEvent::command_finished(
        ShellEventKind::CommandCompleted,
        "test-session",
        "cmd-1",
        0,
        200,
        "terminal://test/cmd-1",
    ));
    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("render inline guidance after command");
    std::thread::sleep(Duration::from_millis(20));
    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("poll inline guidance after adapter event");

    let rendered = String::from_utf8(output).expect("utf8 output");
    assert!(rendered.contains("Thinking..."));
    let expected = match crate::language_config_status().effective {
        crate::Language::EnUs => "Received shell prompt request: 你好",
        crate::Language::ZhCn => "已收到 Shell 提示请求：你好",
    };
    assert!(rendered.contains(expected));
    assert!(!shell_has_active_foreground_command(&events));
}

#[test]
fn correlated_natural_language_intercept_closes_open_command() {
    let mut intercept = ShellEvent::user_input_intercepted("test-session", "Who are you");
    intercept.command_id = Some("cmd-1".to_string());
    intercept.component = Some("natural_language".to_string());
    let events = vec![
        ShellEvent::command_started("test-session", "cmd-1", "Who are you", "/tmp", 100),
        intercept,
    ];

    assert!(!shell_has_active_foreground_command(&events));
}

#[test]
fn smart_mode_top_memory_finding_uses_insight_owner_without_consultation() {
    let output_ref = write_hook_output("top-card", TOP_MEMORY_PRESSURE_OUTPUT);
    let events = command_events(
        "top -b -n1",
        &output_ref,
        TOP_MEMORY_PRESSURE_OUTPUT.len() as u64,
    );
    let mut state = state_with_builtin_hooks();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("record memory insight");

    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.pending_consultation_queue.is_empty());
    assert!(state.hooks.findings.is_empty());
    assert!(state.pending_command_insight.is_none());
    assert!(matches!(
        state.pending_input_ghost_route,
        crate::raw_input::PromptGhostRoute::AgentIntercept { .. }
    ));
    assert!(state.pending_input_ghost_binding.is_some());
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("Insight:"));
}

#[test]
fn smart_mode_success_finding_after_next_input_is_silent_without_card() {
    let output_ref = write_hook_output("top-continued-input", TOP_MEMORY_PRESSURE_OUTPUT);
    let mut events = command_events(
        "top -b -n1",
        &output_ref,
        TOP_MEMORY_PRESSURE_OUTPUT.len() as u64,
    );
    events.push(ShellEvent::user_input_intercepted(
        "test-session",
        "what happened",
    ));
    let mut state = state_with_builtin_hooks();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("record silent hook finding");

    let rendered = String::from_utf8(output).expect("utf8 output");
    assert!(!rendered.contains("Hook finding"), "{rendered}");
    assert!(!rendered.contains("[Analyze] [Ignore]"), "{rendered}");
    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.findings.is_empty());
    assert!(state.pending_command_insight.is_none());
}

#[test]
fn smart_mode_memory_finding_after_component_input_is_silent() {
    let output_ref = write_hook_output("top-component-input", TOP_MEMORY_PRESSURE_OUTPUT);
    let mut events = command_events(
        "top -b -n1",
        &output_ref,
        TOP_MEMORY_PRESSURE_OUTPUT.len() as u64,
    );
    let mut continued = ShellEvent::user_input_intercepted("test-session", "approve");
    continued.component = Some("shell_input".to_string());
    events.push(continued);
    let mut state = state_with_builtin_hooks();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut Vec::new())
        .expect("record silent memory finding");

    assert!(state.pending_command_insight.is_none());
}

#[test]
fn builtin_memory_insight_does_not_offer_legacy_analyze_action() {
    let output_ref = write_hook_output("top-analyze", TOP_MEMORY_PRESSURE_OUTPUT);
    let events = command_events(
        "top -b -n1",
        &output_ref,
        TOP_MEMORY_PRESSURE_OUTPUT.len() as u64,
    );
    let mut state = state_with_builtin_hooks();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("record memory insight");

    let rendered = String::from_utf8(output).expect("utf8 output");
    assert!(!rendered.contains("[Analyze] [Ignore]"), "{rendered}");
    assert!(state.agent_run.active.is_none());
    assert!(state.hooks.findings.is_empty());
    assert!(state.pending_command_insight.is_none());
    assert!(matches!(
        state.pending_input_ghost_route,
        crate::raw_input::PromptGhostRoute::AgentIntercept { .. }
    ));
    assert!(state.pending_input_ghost_binding.is_some());
}

#[test]
fn builtin_memory_insight_does_not_offer_legacy_ignore_action() {
    let output_ref = write_hook_output("top-ignore", TOP_MEMORY_PRESSURE_OUTPUT);
    let events = command_events(
        "top -b -n1",
        &output_ref,
        TOP_MEMORY_PRESSURE_OUTPUT.len() as u64,
    );
    let mut state = state_with_builtin_hooks();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("record memory insight");

    assert!(state.agent_run.active.is_none());
    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.pending_consultation_queue.is_empty());
    assert!(state.pending_command_insight.is_none());
    assert!(matches!(
        state.pending_input_ghost_route,
        crate::raw_input::PromptGhostRoute::AgentIntercept { .. }
    ));
    assert!(state.pending_input_ghost_binding.is_some());
}

#[test]
fn smart_mode_ps_warning_uses_insight_owner_without_hook_hint() {
    let output_ref = write_hook_output("ps-hint", PS_HIGH_MEMORY_OUTPUT);
    let events = command_events(
        "ps aux --sort=-%mem",
        &output_ref,
        PS_HIGH_MEMORY_OUTPUT.len() as u64,
    );
    let mut state = state_with_builtin_hooks();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("render hook hint");

    let rendered = String::from_utf8(output).expect("utf8 output");
    assert!(!rendered.contains("Hook finding"), "{rendered}");
    assert!(!rendered.contains("[Analyze] [Ignore]"), "{rendered}");
    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.findings.is_empty());
    assert!(state.pending_command_insight.is_none());
    assert!(matches!(
        state.pending_input_ghost_route,
        crate::raw_input::PromptGhostRoute::AgentIntercept { .. }
    ));
    assert!(state.pending_input_ghost_binding.is_some());
}

#[cfg(unix)]
#[test]
fn smart_mode_external_warning_finding_uses_interruption_policy() {
    let (dir, hook_path) = write_executable_hook(
            "external-warning",
            "#!/bin/sh\nprintf '{\"hook_id\":\"external-warning\",\"severity\":\"warning\",\"title\":\"External warning\",\"description\":\"External warning description\",\"suggestion\":\"Inspect external warning\"}'\n",
        );
    let output_ref = write_hook_output("external-warning-output", "ok\n");
    let events = command_events("echo hi", &output_ref, 3);
    let mut state = InlineState::default();
    let mut hook_engine = HookEngine::new();
    hook_engine.register_external(ExternalHookConfig {
        path: hook_path,
        matcher: HookMatcher {
            id: "external-warning".to_string(),
            commands: vec!["echo".to_string()],
            command_patterns: Vec::new(),
            command_regex: None,
            exit_codes: Some(vec![0]),
            min_output_bytes: None,
            trigger: HookTrigger::OnSuccess,
        },
        timeout_ms: 3000,
        source: ExternalHookSource::User,
        project_root: None,
        trusted: true,
    });
    state.hooks.engine = hook_engine;
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("render external hook hint");

    let rendered = String::from_utf8(output).expect("utf8 output");
    assert!(rendered.contains("Hook finding"), "{rendered}");
    assert!(rendered.contains("external-warning"), "{rendered}");
    assert!(!rendered.contains("[Analyze] [Ignore]"), "{rendered}");
    assert_eq!(state.hooks.findings.len(), 1);
    let hint = &state.hooks.findings[0];
    assert_eq!(hint.topic, "external");
    assert_eq!(hint.display.label(), "hint");
    assert_eq!(hint.display_reason, "allowed");

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn smart_mode_untrusted_project_hook_is_not_executed() {
    let dir = unique_hook_dir("project-untrusted-warning");
    let marker = dir.join("executed.marker");
    let body = format!(
        "#!/bin/sh\ntouch '{}'\nprintf '{{\"hook_id\":\"project-warning\",\"severity\":\"warning\",\"title\":\"Project warning\",\"description\":\"Project warning description\",\"suggestion\":\"Inspect project warning\"}}'\n",
        marker.display()
    );
    let hook_path = write_executable_hook_at(&dir, &body);
    let output_ref = write_hook_output("project-untrusted-output", "ok\n");
    let events = command_events("echo hi", &output_ref, 3);
    let mut state = InlineState::default();
    let mut hook_engine = HookEngine::new();
    hook_engine.register_external(ExternalHookConfig {
        path: hook_path,
        matcher: HookMatcher {
            id: "project-warning".to_string(),
            commands: vec!["echo".to_string()],
            command_patterns: Vec::new(),
            command_regex: None,
            exit_codes: Some(vec![0]),
            min_output_bytes: None,
            trigger: HookTrigger::OnSuccess,
        },
        timeout_ms: 3000,
        source: ExternalHookSource::Project,
        project_root: Some(dir.clone()),
        trusted: false,
    });
    state.hooks.engine = hook_engine;
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("render untrusted project hook");

    let rendered = String::from_utf8(output).expect("utf8 output");
    assert!(!rendered.contains("Hook finding"), "{rendered}");
    assert!(state.hooks.findings.is_empty());
    assert!(!marker.exists());

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn smart_mode_trusted_project_warning_finding_uses_interruption_policy() {
    let dir = unique_hook_dir("project-trusted-warning");
    let marker = dir.join("executed.marker");
    let body = format!(
        "#!/bin/sh\ntouch '{}'\nprintf '{{\"hook_id\":\"project-warning\",\"severity\":\"warning\",\"title\":\"Project warning\",\"description\":\"Project warning description\",\"suggestion\":\"Inspect project warning\"}}'\n",
        marker.display()
    );
    let hook_path = write_executable_hook_at(&dir, &body);
    let output_ref = write_hook_output("project-trusted-output", "ok\n");
    let events = command_events("echo hi", &output_ref, 3);
    let mut state = InlineState::default();
    let mut hook_engine = HookEngine::new();
    hook_engine.register_external(ExternalHookConfig {
        path: hook_path,
        matcher: HookMatcher {
            id: "project-warning".to_string(),
            commands: vec!["echo".to_string()],
            command_patterns: Vec::new(),
            command_regex: None,
            exit_codes: Some(vec![0]),
            min_output_bytes: None,
            trigger: HookTrigger::OnSuccess,
        },
        timeout_ms: 3000,
        source: ExternalHookSource::Project,
        project_root: Some(dir.clone()),
        trusted: true,
    });
    state.hooks.engine = hook_engine;
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("render trusted project hook");

    let rendered = String::from_utf8(output).expect("utf8 output");
    assert!(rendered.contains("Hook finding"), "{rendered}");
    assert!(rendered.contains("project-warning"), "{rendered}");
    assert!(!rendered.contains("[Analyze] [Ignore]"), "{rendered}");
    assert_eq!(state.hooks.findings.len(), 1);
    let hint = &state.hooks.findings[0];
    assert_eq!(hint.topic, "external");
    assert_eq!(hint.display.label(), "hint");
    assert_eq!(hint.display_reason, "allowed");
    assert!(marker.exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disabled_hook_is_not_evaluated() {
    let output_ref = write_hook_output("ps-disabled", PS_HIGH_MEMORY_OUTPUT);
    let events = command_events(
        "ps aux --sort=-%mem | head",
        &output_ref,
        PS_HIGH_MEMORY_OUTPUT.len() as u64,
    );
    let mut state = state_with_builtin_hooks();
    state
        .hooks
        .disabled
        .insert("high-memory-process".to_string());
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    render_inline_guidance(&events, &adapter, "bash", &mut state, &mut output)
        .expect("render with disabled hook");

    assert!(state.hooks.findings.is_empty());
    let rendered = String::from_utf8(output).expect("utf8 output");
    assert!(!rendered.contains("Hook finding"), "{rendered}");
}
