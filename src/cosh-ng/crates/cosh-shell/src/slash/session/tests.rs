use std::sync::{Arc, Mutex};

use super::*;
use crate::adapter::{SessionHealth, SessionRuntimeState, SessionSummary};
use crate::agent::run::{ActiveAgentRun, AgentRunOrigin};
use crate::evidence::stream::CoshRequestStreamFilter;

const SESSION_ID: &str = "00000000-0000-4000-8000-000000000000";
const SESSION_USAGE_PREFIX: &str = "Usage: /session";
const SESSION_UNAVAILABLE: &str = "Session recovery requires the cosh-core backend.";

#[test]
fn session_all_workspaces_list_lines_groups_by_workspace_and_orders_newest_first() {
    let summaries = vec![
        SessionSummary {
            session_id: "00000000-0000-4000-8000-000000000000".to_string(),
            workspace_scope: "/beta".to_string(),
            created_at_ms: 1,
            updated_at_ms: 20,
            model: Some("mock".to_string()),
            message_count: 2,
            first_prompt: Some("beta newer".to_string()),
            schema_version: Some(1),
            health: SessionHealth::Ready,
        },
        SessionSummary {
            session_id: "11111111-1111-4111-8111-111111111111".to_string(),
            workspace_scope: "/alpha".to_string(),
            created_at_ms: 1,
            updated_at_ms: 30,
            model: Some("mock".to_string()),
            message_count: 2,
            first_prompt: Some("alpha".to_string()),
            schema_version: Some(1),
            health: SessionHealth::Ready,
        },
        SessionSummary {
            session_id: "22222222-2222-4222-8222-222222222222".to_string(),
            workspace_scope: "/beta".to_string(),
            created_at_ms: 1,
            updated_at_ms: 10,
            model: Some("mock".to_string()),
            message_count: 2,
            first_prompt: Some("beta older".to_string()),
            schema_version: Some(1),
            health: SessionHealth::Ready,
        },
    ];

    let lines = session_all_workspaces_list_lines(&summaries, "/beta");

    // Workspaces are sorted alphabetically; current workspace is labelled.
    let alpha_index = lines.iter().position(|line| line == "/alpha").unwrap();
    let beta_index = lines
        .iter()
        .position(|line| line == "/beta (current)")
        .unwrap();
    assert!(alpha_index < beta_index);
    assert!(!lines.iter().any(|line| line == "/beta"));

    // Entries are indented under their workspace.
    assert!(lines
        .iter()
        .any(|line| line.starts_with("  ") && line.contains("alpha")));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("  ") && line.contains("beta newer")));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("  ") && line.contains("beta older")));

    // Within a workspace, newer entries come first.
    let beta_newer_index = lines
        .iter()
        .position(|line| line.contains("beta newer"))
        .unwrap();
    let beta_older_index = lines
        .iter()
        .position(|line| line.contains("beta older"))
        .unwrap();
    assert!(beta_newer_index < beta_older_index);
}

#[test]
fn malformed_session_commands_render_usage_instead_of_selecting() {
    for arguments in [
        "status extra",
        "list extra",
        "--all",
        "resume 00000000-0000-4000-8000-000000000000 extra",
        "clear",
        "compact status extra",
        "-reserved",
    ] {
        let rendered = render_session_arguments(arguments);
        assert!(
            rendered.contains(SESSION_USAGE_PREFIX),
            "{arguments:?} did not render usage: {rendered}"
        );
        assert!(
            !rendered.contains(SESSION_UNAVAILABLE),
            "{arguments:?} entered session recovery: {rendered}"
        );
    }
}

#[test]
fn valid_resume_and_clear_all_keep_session_recovery_routes() {
    for arguments in [SESSION_ID, "resume 00000000-0000-4000-8000-000000000000"] {
        let rendered = render_session_arguments(arguments);
        assert!(
            rendered.contains(SESSION_UNAVAILABLE),
            "{arguments:?} did not enter session recovery: {rendered}"
        );
        assert!(
            !rendered.contains(SESSION_USAGE_PREFIX),
            "{arguments:?} unexpectedly rendered usage: {rendered}"
        );
    }

    let rendered = render_session_arguments("clear --all");
    assert!(rendered.contains(SESSION_UNAVAILABLE), "{rendered}");
    assert!(!rendered.contains(SESSION_USAGE_PREFIX), "{rendered}");
}

#[test]
fn resume_without_id_keeps_picker_contract() {
    let rendered = render_session_arguments("resume");
    assert!(rendered.contains(SESSION_UNAVAILABLE), "{rendered}");
    assert!(!rendered.contains(SESSION_USAGE_PREFIX), "{rendered}");
}

#[test]
fn direct_resume_refuses_to_select_while_agent_run_is_active() {
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let mut state = InlineState::default();
    state.agent_run.active = Some(test_active_run());
    let mut output = Vec::new();

    select_session_in_scope(
        "/tmp",
        "00000000-0000-4000-8000-000000000000",
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("render busy notice");

    let rendered = String::from_utf8(output).expect("UTF-8 notice");
    assert!(
        rendered.contains("Finish the active Agent run"),
        "{rendered}"
    );
    assert_eq!(
        match adapter {
            AdapterInstance::CoshCore(ref core) => core.recovery_snapshot().state,
            _ => unreachable!("test adapter is cosh-core"),
        },
        SessionRecoveryState::None
    );
}

#[test]
fn picker_panel_shows_short_ids_marked_count_and_key_semantics() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let panel_id = state.control.session_mut().new_panel_id();
    let mut selected_for_clear = HashSet::new();
    selected_for_clear.insert(SESSION_ID.to_string());
    state
        .control
        .session_mut()
        .set_pending_panel(RuntimeSessionPanel {
            id: panel_id,
            workspace_scope: "/tmp".to_string(),
            sessions: vec![SessionSummary {
                session_id: SESSION_ID.to_string(),
                workspace_scope: "/tmp".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
                model: Some("mock".to_string()),
                message_count: 2,
                first_prompt: Some("first prompt".to_string()),
                schema_version: Some(1),
                health: SessionHealth::Ready,
            }],
            next_cursor: None,
            selected_option: 0,
            selected_for_clear,
            clear_confirmation_ids: Vec::new(),
            protected_clear_ids: Vec::new(),
            phase: RuntimeSessionPanelPhase::Browse,
        });
    let mut output = Vec::new();

    assert!(matches!(
        crate::runtime::controller::pending_card_capture(&state),
        Some(crate::raw_input::RawInputCapture::Session {
            marked_for_clear,
            ..
        }) if marked_for_clear == vec![true]
    ));
    render_current_session_panel(&adapter, &mut state, &mut output).expect("render picker panel");

    // Strip per-line borders before joining so wrapped footer text stays width-agnostic.
    let rendered = String::from_utf8(output).expect("UTF-8 picker panel");
    let flat = rendered
        .lines()
        .map(|line| line.trim_matches(|ch: char| ch == '│' || ch.is_whitespace()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(flat.contains("[x] 00000000… · first prompt"), "{rendered}");
    assert!(flat.contains("1/1 · 1 marked"), "{rendered}");
    assert!(flat.contains("Enter review clear"), "{rendered}");
    assert!(flat.contains("Space toggle clear mark"), "{rendered}");
    assert!(flat.contains("d review clear"), "{rendered}");
    assert!(!flat.contains("Space mark for clear"), "{rendered}");
}

#[test]
fn session_new_detaches_active_provider_session_and_clears_recovery() {
    let adapter = active_core_adapter();
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let mut output = Vec::new();

    render_session_command("new", &[], &adapter, &mut state, &mut output)
        .expect("render fresh session");
    let rendered = String::from_utf8(output).expect("UTF-8 fresh notice");

    assert!(rendered.contains("Fresh session"), "{rendered}");
    assert!(
        rendered.contains(&format!("Detached from provider session {SESSION_ID}")),
        "{rendered}"
    );
    assert!(
        rendered.contains("starts a fresh conversation"),
        "{rendered}"
    );
    assert!(
        rendered.contains("cwd, history, and settings are unchanged"),
        "{rendered}"
    );

    let AdapterInstance::CoshCore(core) = &adapter else {
        unreachable!("test adapter is cosh-core");
    };
    assert_eq!(core.recovery_snapshot().state, SessionRecoveryState::None);
    assert_eq!(core.committed_session_id(), None);
}

#[test]
fn session_new_is_idempotent_when_no_session_is_attached() {
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let mut output = Vec::new();

    render_session_command("new", &[], &adapter, &mut state, &mut output)
        .expect("render fresh session");
    let rendered = String::from_utf8(output).expect("UTF-8 fresh notice");

    assert!(rendered.contains("Fresh session"), "{rendered}");
    assert!(
        rendered.contains("No provider session was attached"),
        "{rendered}"
    );
    assert!(
        rendered.contains("starts a fresh conversation"),
        "{rendered}"
    );
}

#[test]
fn session_new_refuses_while_agent_run_is_active_without_detaching() {
    let adapter = active_core_adapter();
    let mut state = InlineState::default();
    state.agent_run.active = Some(test_active_run());
    let mut output = Vec::new();

    render_session_command("new", &[], &adapter, &mut state, &mut output)
        .expect("render busy notice");
    let rendered = String::from_utf8(output).expect("UTF-8 notice");

    assert!(
        rendered.contains("Finish the active Agent run"),
        "{rendered}"
    );
    let AdapterInstance::CoshCore(core) = &adapter else {
        unreachable!("test adapter is cosh-core");
    };
    assert_eq!(core.committed_session_id().as_deref(), Some(SESSION_ID));
    assert_eq!(core.recovery_snapshot().state, SessionRecoveryState::Active);
}

fn active_core_adapter() -> AdapterInstance {
    AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        allow_model_call: false,
        session: Arc::new(Mutex::new(SessionRuntimeState::with_active(
            SESSION_ID, "/tmp",
        ))),
        ..CoshCoreAdapter::default()
    })
}

fn render_session_arguments(arguments: &str) -> String {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let mut output = Vec::new();

    render_session_command(arguments, &[], &adapter, &mut state, &mut output)
        .expect("render session command");
    String::from_utf8(output).expect("UTF-8 session panel")
}

fn test_active_run() -> ActiveAgentRun {
    let request = AgentRequest {
        id: "active-request".to_string(),
        session_id: "shell-session".to_string(),
        command_block: CommandBlock {
            id: "command".to_string(),
            session_id: "shell-session".to_string(),
            command: "echo active".to_string(),
            origin: Default::default(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("active".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    let handle = AdapterInstance::Fake(FakeAgentAdapter)
        .start_cancellable(request.clone(), CoshApprovalMode::Recommend);
    let renderer = RatatuiInlineRenderer::for_terminal();
    ActiveAgentRun {
        request,
        origin: AgentRunOrigin::Standard,
        handle,
        provider_name: "fake",
        language: Language::EnUs,
        renderer: renderer.clone(),
        status_animation: renderer.status_animation(),
        markdown_stream: renderer.stream_markdown_agent(),
        governed_events: Vec::new(),
        deferred_events: Vec::new(),
        held_events: Vec::new(),
        cosh_request_filter: CoshRequestStreamFilter::default(),
        pending_cosh_requests: Vec::new(),
        pending_cosh_request_audits: Vec::new(),
        rendered_governed_event_count: 0,
        selectable_after_event_index: None,
        started_at: std::time::Instant::now(),
        last_activity_at: std::time::Instant::now(),
        last_heartbeat_at: std::time::Instant::now(),
        current_phase: String::new(),
        current_message: String::new(),
        has_visible_text_delta: false,
        completed: false,
        host_completed_tool_ids: Vec::new(),
        pending_hook_notifications: Vec::new(),
    }
}
