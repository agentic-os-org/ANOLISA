use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::approval::handoff::trust_key_from_command;
use crate::config::parse_recommendations_environment_override;
use crate::diagnostics::health::{spawn_startup_health_scan, startup_health_scan_enabled_for_env};
use crate::hooks::{
    dirs_for_hook_loading, is_trusted_project_root, load_hook_feedback_preferences,
    project_hook_root_from_cwd,
};
use crate::recommendation::personal_analysis_runtime::AnalyzerCancellation;
use crate::recommendation::personal_runtime::PersonalRuntime;
use crate::runtime::cli_args::{LaunchOptions, RawShellKind, ResumeLaunch};
use crate::runtime::prelude::*;
use crate::runtime::startup::bootstrap_process_path_from_shell;
use crate::runtime::state::{AnalysisMode, InlineState};
use crate::shell_host::ShellIntegration;

use super::render_raw_inline_event_view;

fn build_adapter(kind: AdapterKind) -> AdapterInstance {
    match adapter_for_kind(kind) {
        AdapterInstance::ClaudeCode(adapter) => {
            AdapterInstance::ClaudeCode(adapter.with_model_call(true))
        }
        AdapterInstance::QwenCli(adapter) => {
            AdapterInstance::QwenCli(adapter.with_model_call(true))
        }
        AdapterInstance::CoshCore(adapter) => {
            AdapterInstance::CoshCore(adapter.with_model_call(true))
        }
        other => other,
    }
}

pub(crate) fn run_demo() -> i32 {
    let events = demo_events();
    render_loop_from_events(&events)
}

pub(crate) fn run_host_demo() -> i32 {
    let work_dir =
        std::env::temp_dir().join(format!("cosh-shell-host-demo-{}", std::process::id()));
    let _work_dir_cleanup = TempSessionDir::new(work_dir.clone());
    let config = ShellHostConfig::new("host-demo-session", work_dir)
        .with_integration(ShellIntegration::Enhanced);
    let inputs = vec![
        ScriptedInput::user_line("/explain last error"),
        ScriptedInput::user_line("echo ok"),
        ScriptedInput::user_line("please analyze the last failure"),
        ScriptedInput::user_line("ls /path/that/does/not/exist"),
    ];

    let output = match run_scripted_bash(&config, &inputs) {
        Ok(output) => output,
        Err(err) => {
            let err = crate::evidence::redact_sensitive_text(&err.to_string()).0;
            eprintln!("host demo failed: {err}");
            return 1;
        }
    };

    render_loop_from_events(&output.events)
}

pub(crate) fn run_raw(
    adapter_name: &str,
    shell_kind: RawShellKind,
    launch_options: LaunchOptions,
) -> i32 {
    // args_os: argv[0] may be arbitrary bytes (the classifier admits any
    // byte sequence whose basename is `cosh`), so String iteration would
    // panic here.
    let args = std::env::args_os().collect::<Vec<_>>();

    let Some(kind) = AdapterKind::parse(adapter_name) else {
        let adapter_name = crate::evidence::redact_sensitive_text(adapter_name).0;
        eprintln!("unknown adapter: {adapter_name}");
        return 2;
    };

    let work_dir =
        std::env::temp_dir().join(format!("cosh-shell-raw-session-{}", std::process::id()));
    let _work_dir_cleanup = TempSessionDir::new(work_dir.clone());
    let session_id = format!(
        "raw-session-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    );
    let mut config = ShellHostConfig::new(session_id, work_dir);
    config.bound_interactive_transcript();

    let isolated = args.iter().any(|a| a == "--isolated")
        || std::env::var("COSH_SHELL_ISOLATED").as_deref() == Ok("1");
    if isolated {
        config.native_mode = false;
        if let Ok(prompt) = std::env::var("COSH_POC_PS1") {
            if !prompt.is_empty() {
                config.prompt = prompt;
            }
        }
    }
    let login = args
        .first()
        .is_some_and(|argv0| crate::runtime::invocation::is_login_invocation(argv0, &args[1..]));
    config.login_shell = login;
    let cosh_config = load_config();
    config.status_symbols = cosh_config.status_symbols;
    let Some(configured_integration) =
        ShellIntegration::parse_config(&cosh_config.shell_integration)
    else {
        eprintln!(
            "invalid shell integration; expected shell.integration or \
             COSH_SHELL_INTEGRATION to be native or enhanced"
        );
        return 2;
    };
    // Resume is an explicit Agent action. It needs the ShellReady boundary
    // that opens the requested provider session even when Native is the
    // configured default.
    config.integration = if launch_options.resume.is_some() {
        ShellIntegration::Enhanced
    } else {
        configured_integration
    };
    let enhanced_integration = config.integration.uses_markers();
    if config.native_mode && enhanced_integration {
        bootstrap_process_path_from_shell(&shell_kind, login, &config.winsize);
    }
    let recommendations_environment_override = parse_recommendations_environment_override(
        std::env::var("COSH_RECOMMENDATIONS_ENABLED")
            .ok()
            .as_deref(),
    );
    config.input_classifier = config
        .input_classifier
        .with_ai_enabled(cosh_config.ai_enabled);

    let assistance_control = enhanced_integration.then(|| {
        crate::input::AssistanceControl::enabled(crate::shell_host::assistance_state_file(&config))
    });
    if let Some(control) = assistance_control.clone() {
        config.set_assistance_control(control);
    }

    let adapter = build_adapter(kind);
    let mut inline_state = InlineState::with_raw_session_dir(&config.work_dir);
    inline_state.assistance_control = assistance_control;
    inline_state.shell_session_id = Some(config.session_id.clone());
    // #2161: share the relay-side input-wait clock and productized timeout.
    inline_state.input_wait_status = config.input_wait_status.clone();
    inline_state.input_wait_timeout = (cosh_config.input_wait_timeout_secs > 0)
        .then(|| std::time::Duration::from_secs(cosh_config.input_wait_timeout_secs));
    config.input_wait_timeout_secs = cosh_config.input_wait_timeout_secs;
    inline_state.audit = Some(crate::journal::audit::ShellAuditRecorder::initialize(
        config.session_id.clone(),
    ));
    if let Some(resume) = launch_options.resume {
        inline_state
            .control
            .session_mut()
            .set_pending_launch(match resume {
                ResumeLaunch::Picker => crate::slash::session::SessionLaunchRequest::Picker,
                ResumeLaunch::Session(id) => {
                    crate::slash::session::SessionLaunchRequest::Resume(id)
                }
            });
    }
    inline_state.personalization.bash_history = cosh_config.recommendations.bash_history;
    inline_state.personalization.ai_disabled = !cosh_config.ai_enabled;
    if enhanced_integration {
        inline_state.personalization.analyzer_cancellation = Some(AnalyzerCancellation::new());
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let root = home.join(".copilot-shell/cosh/recommendations");
            let configured_enabled = cosh_config.recommendations.enabled;
            let environment_override = recommendations_environment_override;
            inline_state.personalization.store_root = Some(root.clone());
            inline_state.personalization.configured_enabled = configured_enabled;
            inline_state.personalization.environment_override = environment_override;
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            inline_state.personalization.writer_pending = Some(receiver);
            let _ = std::thread::Builder::new()
                .name("cosh-recommendation-load".to_string())
                .spawn(move || {
                    if let Ok(runtime) = PersonalRuntime::open_with_environment(
                        configured_enabled,
                        environment_override,
                        root,
                        now_hour_bucket(),
                    ) {
                        if let Ok(writer) = runtime.spawn_writer() {
                            let _ = sender.send(writer);
                        }
                    }
                });
        }
        if matches!(&shell_kind, RawShellKind::Bash)
            && config.native_mode
            && cosh_config.recommendations.enabled
            && recommendations_environment_override != Some(false)
            && cosh_config.recommendations.bash_history
        {
            let (sender, receiver) = std::sync::mpsc::channel();
            inline_state.personalization.history_file_pending = Some(receiver);
            config.set_shell_history_file_observer(move |path| {
                let _ = sender.send(path);
            });
        }
        let snapshot_publisher = inline_state.shell_rewrite.start_worker();
        config.set_shell_environment_observer(move |snapshot| {
            snapshot_publisher.publish(snapshot);
        });
        if startup_health_scan_enabled_for_env(&cosh_config.health) {
            inline_state.startup_health.pending =
                Some(spawn_startup_health_scan(cosh_config.health.clone()));
        }
        // The credential probe only feeds startup-banner surfaces; without a
        // banner it would just cost an extra cosh-core process per launch.
        if cosh_config.ai_enabled && crate::runtime::startup::startup_banner_enabled() {
            if let AdapterInstance::CoshCore(core) = &adapter {
                let core = core.clone();
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                inline_state.startup_auth.pending = Some(receiver);
                let _ = std::thread::Builder::new()
                    .name("cosh-startup-auth-probe".to_string())
                    .spawn(move || {
                        let _ = sender.send(core.ai_configured().ok());
                    });
            }
        }
    }
    let hook_feedback = load_hook_feedback_preferences();
    inline_state.hooks.feedback = hook_feedback.feedback;
    inline_state.hooks.noisy_groups = hook_feedback.noisy_groups;
    inline_state.language = parse_language_setting(&cosh_config.language)
        .map(resolve_language_setting)
        .unwrap_or_default();
    // #2025: the relay renders the input-wait hint card itself.
    config.hint_language = inline_state.language;
    // #2179: inject the panel-family framing so the relay-side hint card
    // shares the NoticePanel width contract, closed borders, and plain
    // fallback with every other panel. `for_terminal()` re-reads the
    // terminal width per emission, so resizes are picked up for free.
    let hint_card_language = inline_state.language;
    config.set_hint_card_renderer(move |title, body| {
        crate::ui::agent_render::RatatuiInlineRenderer::for_terminal()
            .with_language(hint_card_language)
            .notice_panel_lines(crate::ui::agent_render::NoticePanelModel {
                title,
                body,
                footer: None,
            })
    });
    match cosh_config.analysis_mode.as_str() {
        "auto" => inline_state.analysis_mode = AnalysisMode::Auto,
        "manual" => inline_state.analysis_mode = AnalysisMode::Manual,
        _ => {}
    }
    inline_state.debug = cosh_config.debug;
    inline_state.approval_mode = cosh_config.approval_mode;
    for cmd in &cosh_config.trusted_commands {
        if let Some(key) = trust_key_from_command(cmd) {
            inline_state.control.trust.trust_session_command(key);
        }
    }
    apply_readonly_config(&cosh_config);
    inline_state.hooks.engine = load_hook_engine(&cosh_config);

    let raw_result = match shell_kind {
        RawShellKind::Bash => {
            run_raw_interactive_bash_with_event_view(&config, |events, output| {
                render_raw_inline_event_view(events, output, &adapter, "bash", &mut inline_state)
            })
        }
        RawShellKind::Zsh => run_raw_interactive_zsh_with_event_view(&config, |events, output| {
            render_raw_inline_event_view(events, output, &adapter, "zsh", &mut inline_state)
        }),
        RawShellKind::MissingShellValue => {
            eprintln!("missing value for --shell; supported shells: bash, zsh");
            return 2;
        }
        RawShellKind::Unsupported(shell) => {
            let shell = crate::evidence::redact_sensitive_text(&shell).0;
            eprintln!("unsupported raw shell: {shell}; supported shells: bash, zsh");
            return 2;
        }
    };

    crate::auth::ecs_poll::shutdown(&mut inline_state);
    config.clear_shell_environment_observer();
    config.clear_shell_history_file_observer();
    inline_state.personalization.poll_ready();
    if let Some(cancellation) = inline_state.personalization.analyzer_cancellation.as_ref() {
        cancellation.cancel_current();
    }
    if let Some(mut writer) = inline_state.personalization.writer.take() {
        let _ = writer.shutdown(now_hour_bucket(), std::time::Duration::from_millis(100));
    }
    inline_state.shell_rewrite.shutdown();

    match raw_result {
        Ok(output) => output.exit_status.unwrap_or(0),
        Err(err) => {
            let err = crate::evidence::redact_sensitive_text(&err.to_string()).0;
            eprintln!("raw shell failed: {err}");
            1
        }
    }
}

fn now_hour_bucket() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 3600)
        .unwrap_or_default()
}

pub(crate) fn run_interactive(adapter_name: &str) -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    run_interactive_from_reader(
        "interactive-session",
        adapter_name,
        stdin.lock(),
        &mut stdout,
    )
}

pub(crate) fn run_interactive_demo(adapter_name: &str) -> i32 {
    let input = std::io::Cursor::new(
        "/explain last error\n\
         echo ok\n\
         please analyze the last failure\n\
         ls /path/that/does/not/exist\n",
    );
    let mut output = Vec::new();
    run_interactive_from_reader("interactive-demo-session", adapter_name, input, &mut output)
}

fn run_interactive_from_reader<R, W>(
    session_id: &str,
    adapter_name: &str,
    input: R,
    output: &mut W,
) -> i32
where
    R: std::io::BufRead,
    W: std::io::Write,
{
    let Some(kind) = AdapterKind::parse(adapter_name) else {
        let adapter_name = crate::evidence::redact_sensitive_text(adapter_name).0;
        eprintln!("unknown adapter: {adapter_name}");
        return 2;
    };

    let work_dir =
        std::env::temp_dir().join(format!("cosh-shell-{session_id}-{}", std::process::id()));
    let _work_dir_cleanup = TempSessionDir::new(work_dir.clone());
    let config =
        ShellHostConfig::new(session_id, work_dir).with_integration(ShellIntegration::Enhanced);
    let shell_output = match run_line_interactive_bash(&config, input, output) {
        Ok(output) => output,
        Err(err) => {
            let err = crate::evidence::redact_sensitive_text(&err.to_string()).0;
            eprintln!("interactive demo failed: {err}");
            return 1;
        }
    };

    render_loop_from_events_with_adapter(&shell_output.shell.events, &build_adapter(kind))
}

pub(crate) fn run_adapter_demo(adapter_name: &str) -> i32 {
    let Some(kind) = AdapterKind::parse(adapter_name) else {
        let adapter_name = crate::evidence::redact_sensitive_text(adapter_name).0;
        eprintln!("unknown adapter: {adapter_name}");
        return 2;
    };
    let events = demo_events();
    render_loop_from_events_with_adapter(&events, &build_adapter(kind))
}

fn render_loop_from_events(events: &[ShellEvent]) -> i32 {
    render_loop_from_events_with_adapter(events, &FakeAgentAdapter)
}

struct TempSessionDir {
    path: PathBuf,
}

impl TempSessionDir {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempSessionDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn render_loop_from_events_with_adapter(events: &[ShellEvent], adapter: &impl AgentAdapter) -> i32 {
    let ledger = build_command_blocks(events);
    if !ledger.errors.is_empty() {
        let errors = crate::evidence::redact_sensitive_text(&ledger.errors.join(", ")).0;
        eprintln!("ledger errors: {errors}");
        return 1;
    }

    let Some(block) = ledger.blocks.iter().find(|block| block.exit_code != 0) else {
        println!("No failed command found; no Agent intervention needed");
        return 0;
    };

    let findings = findings_from_blocks(&ledger.blocks);
    let interventions = interventions_from_findings(&findings);
    let user_confirmed = agent_request_confirmed_by_events(events);
    let governed_events = if user_confirmed {
        let Some(request) =
            agent_request_after_confirmation("demo-session", block, &findings, true)
        else {
            eprintln!("agent request was not confirmed");
            return 1;
        };
        let agent_events = match adapter.run(&request) {
            Ok(events) => events,
            Err(err) => {
                let message = crate::evidence::redact_sensitive_text(&err.message).0;
                eprintln!("adapter failed: {message}");
                return 1;
            }
        };
        govern_agent_events(&agent_events, &Policy::default()).events
    } else {
        Vec::new()
    };

    for line in render_transcript(block, &findings, &interventions, &governed_events) {
        let line = crate::evidence::redact_sensitive_text(&line).0;
        println!("{line}");
    }

    if !user_confirmed {
        println!("Enter a slash command or natural-language request to ask for Agent analysis");
    }

    0
}

fn demo_events() -> Vec<ShellEvent> {
    vec![
        ShellEvent::user_input_intercepted("demo-session", "/explain last error"),
        ShellEvent::command_started("demo-session", "cmd-1", "missing-command", "/tmp", 100),
        ShellEvent::command_finished(
            ShellEventKind::CommandFailed,
            "demo-session",
            "cmd-1",
            127,
            140,
            "terminal://demo/cmd-1",
        ),
    ]
}

fn load_hook_engine(cosh_config: &CoshConfig) -> HookEngine {
    let mut hook_engine = HookEngine::new();
    for hook in default_builtin_hooks() {
        hook_engine.register(hook);
    }
    if let Some(hooks_dir) = dirs_for_hook_loading() {
        hook_engine.load_hooks_from_dir(&hooks_dir);
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(project_root) = project_hook_root_from_cwd(&cwd) {
            let trusted = is_trusted_project_root(
                &project_root,
                cosh_config.trusted_project_roots.as_slice(),
            );
            hook_engine.load_project_hooks_from_root(&project_root, trusted);
        }
    }
    hook_engine
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_session_dir_guard_removes_session_directory_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "cosh-shell-temp-session-cleanup-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);

        {
            let _cleanup = TempSessionDir::new(dir.clone());
            fs::create_dir_all(dir.join("output-refs")).expect("create output refs");
            fs::write(dir.join("history"), "echo ok\n").expect("write history");
            fs::write(dir.join("output-refs/cmd-1.txt"), "ok\n").expect("write output ref");
        }

        assert!(!dir.exists(), "temp session dir should be removed on drop");
    }
}
