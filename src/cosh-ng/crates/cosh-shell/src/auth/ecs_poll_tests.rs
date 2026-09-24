use super::*;
use crate::adapter::FakeAgentAdapter;
use crate::runtime::prelude::AuthProviderInfo;
use std::collections::HashMap;

fn state(id: &str) -> InlineState {
    let mut state = InlineState::default();
    state.auth.state = Some(RuntimeAuthState {
        id: id.into(),
        request_id: id.into(),
        phase: AuthPhase::AliyunEcsChallenge {
            instance_id: "i-test".into(),
            console_url: "https://example.invalid/authorize".into(),
        },
        providers: vec![AuthProviderInfo {
            id: "aliyun".into(),
            label: "Aliyun".into(),
            description: None,
            description_zh_cn: None,
            builtin_base_url: None,
            fields: Vec::new(),
        }],
        selected_provider: 0,
        current_field: 0,
        collected_values: HashMap::from([
            ("auth_source".into(), "ecs_ram_role".into()),
            ("provider_id".into(), "aliyun".into()),
        ]),
        field_input: String::new(),
        field_error: None,
        field_capture_revision: 0,
        existing_providers: Vec::new(),
        editing_provider_name: None,
        default_provider_id: true,
        from_sysom_shortcut: false,
        error_message: None,
        backend: AuthBackend::CoreRegistry,
        sysom: SysomMenu::default(),
    });
    state.auth.ecs = Some(EcsFlow::new(
        state.auth.state.as_ref().unwrap(),
        Instant::now(),
    ));
    state
}

#[test]
fn slash_auth_installs_cancellable_empty_capture_before_menu_prepare() {
    let (dir, core) = probe_fixture(Value::Null);
    // Both replies are immediate: this asserts ordering, not a timing threshold.
    std::fs::write(
        dir.path().join("registry.sh"),
        r#"#!/bin/sh
read -r request
printf '%s\n' "$request" >> "$0.calls"
request_id=${request#*\"request_id\":\"}
request_id=${request_id%%\"*}
case "$request" in
    *'"action":"state"'*)
        data='{"templates":[{"id":"aliyun","label":"Aliyun","fields":[]}],"saved_providers":[]}' ;;
    *'"action":"prepare"'*) data='{"mode":"manual"}' ;;
    *) exit 1 ;;
esac
printf '{"type":"registry_response","request_id":"%s","success":true,"data":%s}\n' "$request_id" "$data"
"#,
    )
    .unwrap();
    let adapter = AdapterInstance::CoshCore(core);
    let mut state = InlineState::default();
    let mut output = Vec::new();

    runtime::trigger_auth_from_slash(&adapter, &mut state, &mut output).unwrap();

    let calls = registry_calls(&dir);
    let actions: Vec<_> = calls.iter().map(|call| call["action"].as_str()).collect();
    assert_eq!(
        actions,
        vec![Some("state")],
        "the slash handler may read config, but must defer prepare until poll"
    );
    let capture = runtime::pending_auth_capture(&state).expect("initial auth capture");
    let crate::runtime::prelude::RawInputCapture::Question {
        id,
        option_count: 0,
        allow_free_text: false,
        multiple: false,
        secret: false,
        ..
    } = capture
    else {
        panic!("menu prepare must have a non-editable capture with no options");
    };
    assert!(state.auth.state.is_some());
    assert_eq!(
        state.questions.active_panel_id.as_deref(),
        Some(id.as_str())
    );
    assert!(state.questions.active_panel_height > 0);
    assert!(
        !output.is_empty(),
        "the initial panel must already be rendered"
    );

    runtime::cancel_auth_panel(&mut state, &mut output).unwrap();
    assert!(state.auth.state.is_none());
    assert!(state.auth.ecs.is_none());
    poll(&adapter, &mut state, &mut output).unwrap();
    assert!(runtime::pending_auth_capture(&state).is_none());
    assert_eq!(registry_calls(&dir), calls, "cancel must not start prepare");
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("Auth cancelled"));
}

fn menu_state(id: &str) -> InlineState {
    let mut state = state(id);
    let auth = state.auth.state.as_mut().unwrap();
    auth.phase = AuthPhase::PreparingMenu;
    auth.collected_values.clear();
    state.auth.ecs = Some(EcsFlow::new(auth, Instant::now()));
    state
}

#[test]
fn menu_prepare_results_return_to_user_choice_without_verifying_or_saving() {
    for (data, rejected, expected_phase, cached) in [
        (
            serde_json::json!({"mode": "manual"}),
            false,
            AuthPhase::SelectingProvider,
            true,
        ),
        (
            serde_json::json!({"mode": "ecs_ram_role", "instance_id": "i-menu", "console_url": "https://example.invalid/authorize"}),
            false,
            AuthPhase::ManagingProviders,
            true,
        ),
        (Value::Null, false, AuthPhase::SelectingProvider, false),
        (
            serde_json::json!({"error_code": "metadata_access_denied"}),
            true,
            AuthPhase::SelectingProvider,
            false,
        ),
    ] {
        let (dir, core) = probe_fixture(data);
        if rejected {
            let path = dir.path().join("registry.sh");
            let script = std::fs::read_to_string(&path).unwrap();
            std::fs::write(
                path,
                script.replace("\"success\":true", "\"success\":false"),
            )
            .unwrap();
        }
        let mut state = menu_state("menu-result");
        state.auth.ecs.as_mut().unwrap().operation = Some(Operation::Probe(
            core.start_ecs_probe("prepare").unwrap(),
            true,
        ));
        let adapter = AdapterInstance::CoshCore(core);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut output = Vec::new();
        while state.auth.ecs.is_some() {
            assert!(Instant::now() < deadline);
            poll(&adapter, &mut state, &mut output).unwrap();
            thread::yield_now();
        }
        let auth = state.auth.state.as_ref().unwrap();
        assert_eq!(auth.phase, expected_phase);
        assert_eq!(auth.sysom.prefetched().is_some(), cached);
        assert!(auth.collected_values.is_empty());
        assert!(!state.auth.completed_ids.contains("menu-result"));
        poll(&adapter, &mut state, &mut output).unwrap();
        let calls = registry_calls(&dir);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["action"], "prepare");
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("Select your AI provider:"));
    }
}

#[test]
fn menu_prepare_timeout_reaps_before_returning_to_the_menu() {
    let (dir, core) = probe_fixture(Value::Null);
    std::fs::write(dir.path().join("registry.sh"), "#!/bin/sh\nread -r request\nprintf '%s\\n' \"$request\" >> \"$0.calls\"\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nexec sleep 60\n").unwrap();
    let mut state = menu_state("menu-timeout");
    state.auth.ecs.as_mut().unwrap().operation = Some(Operation::Probe(
        core.start_ecs_probe("prepare").unwrap(),
        true,
    ));
    let pid = probe_pid(&dir);
    state.auth.ecs.as_mut().unwrap().operation_deadline = Some(Instant::now());
    let adapter = AdapterInstance::CoshCore(core);
    let deadline = Instant::now() + Duration::from_secs(2);
    while state.auth.ecs.is_some() {
        assert!(Instant::now() < deadline);
        assert_eq!(
            state.auth.state.as_ref().unwrap().phase,
            AuthPhase::PreparingMenu
        );
        poll(&adapter, &mut state, &mut Vec::new()).unwrap();
        thread::yield_now();
    }
    assert_probe_reaped(pid);
    assert_eq!(
        state.auth.state.as_ref().unwrap().phase,
        AuthPhase::SelectingProvider
    );
    assert_eq!(registry_calls(&dir).len(), 1);
    assert!(!state.auth.completed_ids.contains("menu-timeout"));
}

#[cfg(target_os = "linux")]
#[test]
fn menu_prepare_completed_reply_cannot_reopen_after_cancel_or_exit() {
    for shell_exited in [false, true] {
        let (dir, core) = probe_fixture(serde_json::json!({"mode": "manual"}));
        let mut state = menu_state("menu-cancel");
        state.auth.ecs.as_mut().unwrap().operation = Some(Operation::Probe(
            core.start_ecs_probe("prepare").unwrap(),
            true,
        ));
        await_unconsumed_ready_probe(&dir);
        let adapter = AdapterInstance::CoshCore(core);
        let mut output = Vec::new();
        if shell_exited {
            state.shell_exited = true;
        } else {
            runtime::cancel_auth_panel(&mut state, &mut output).unwrap();
        }
        poll(&adapter, &mut state, &mut output).unwrap();
        assert!(state.auth.state.is_none());
        assert!(state.auth.ecs.is_none());
        output.clear();
        poll(&adapter, &mut state, &mut output).unwrap();
        assert!(output.is_empty());
        assert_eq!(registry_calls(&dir).len(), 1);
    }
}

#[test]
fn cancellation_keeps_the_submitted_capture_until_cleanup_finishes() {
    for menu in [true, false] {
        let (dir, core) = probe_fixture(Value::Null);
        let mut state = if menu {
            menu_state("capture-cancel")
        } else {
            state("capture-cancel")
        };
        state.auth.ecs.as_mut().unwrap().operation = Some(Operation::Probe(
            core.start_ecs_probe("prepare").unwrap(),
            true,
        ));
        let pid = probe_pid(&dir);
        let mut output = Vec::new();
        render_current_auth_panel(&mut state, &mut output).unwrap();
        let capture = runtime::pending_auth_capture(&state).unwrap();
        runtime::cancel_auth_panel(&mut state, &mut output).unwrap();
        assert_eq!(runtime::pending_auth_capture(&state), Some(capture.clone()));
        state.auth.ecs.as_mut().unwrap().cleanup_reported = true;
        redraw(&mut state, &mut output).unwrap();
        assert_eq!(runtime::pending_auth_capture(&state), Some(capture));
        let adapter = AdapterInstance::CoshCore(core);
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.auth.ecs.is_some() {
            assert!(Instant::now() < deadline);
            poll(&adapter, &mut state, &mut output).unwrap();
            thread::yield_now();
        }
        assert!(runtime::pending_auth_capture(&state).is_none());
        assert_probe_reaped(pid);
    }
}

#[test]
fn cancellation_overrides_cleanup_destination_without_resetting_deadline() {
    let mut state = menu_state("cleanup-priority");
    let flow = state.auth.ecs.as_mut().unwrap();
    let now = Instant::now();
    flow.stop(Stage::Failed, now);
    let deadline = flow.operation_deadline;
    flow.stop(Stage::Cancelling, now + Duration::from_secs(1));
    assert_eq!(flow.stop_stage, Stage::Cancelling);
    assert_eq!(flow.operation_deadline, deadline);
    flow.stop(Stage::TimedOut, now + Duration::from_secs(2));
    assert_eq!(flow.stop_stage, Stage::Cancelling);
    assert_eq!(flow.operation_deadline, deadline);
}

#[cfg(target_os = "linux")]
#[test]
fn timeout_cleanup_then_cancel_or_exit_never_returns_to_auth() {
    for menu in [true, false] {
        for shell_exited in [false, true] {
            let (dir, core) = probe_fixture(serde_json::json!({"mode": "manual"}));
            let mut state = if menu {
                menu_state("timeout-cancel")
            } else {
                state("timeout-cancel")
            };
            state.auth.ecs.as_mut().unwrap().operation = Some(Operation::Probe(
                core.start_ecs_probe("prepare").unwrap(),
                true,
            ));
            await_unconsumed_ready_probe(&dir);
            state
                .auth
                .ecs
                .as_mut()
                .unwrap()
                .stop(Stage::Failed, Instant::now());
            let adapter = AdapterInstance::CoshCore(core);
            let mut output = Vec::new();
            if shell_exited {
                state.shell_exited = true;
            } else {
                runtime::cancel_auth_panel(&mut state, &mut output).unwrap();
            }
            poll(&adapter, &mut state, &mut output).unwrap();
            assert!(
                state.auth.state.is_none(),
                "cancel/exit must override the pending timeout result"
            );
            assert!(state.auth.ecs.is_none());
            assert!(!String::from_utf8(output)
                .unwrap()
                .contains("Select your AI provider:"));
            assert_eq!(registry_calls(&dir).len(), 1);
        }
    }
}

#[test]
fn orphaned_idle_flow_is_removed_without_an_auth_panel() {
    let mut state = state("old");
    state.auth.state = None;
    poll(
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut Vec::new(),
    )
    .unwrap();
    assert!(state.auth.ecs.is_none());
}

#[test]
fn stale_flow_cleanup_does_not_cancel_a_new_auth_owner() {
    let old = state("old");
    let mut current = state("new");
    current.auth.ecs = old.auth.ecs;
    poll(
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut current,
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(
        current.auth.state.as_ref().map(|auth| auth.id.as_str()),
        Some("new")
    );
    assert!(!current.auth.completed_ids.contains("new"));
}

#[test]
fn waiting_panel_is_redrawn_when_recorded_width_changes() {
    let mut state = state("resize");
    let flow = state.auth.ecs.as_mut().unwrap();
    flow.stage = Stage::Waiting;
    flow.next_check = Instant::now() + Duration::from_secs(60);
    state.questions.active_panel_id = Some("resize".into());
    state.questions.active_panel_height = 4;
    state.questions.active_panel_width = Some(1);
    let mut output = Vec::new();
    poll(
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut output,
    )
    .unwrap();
    assert!(String::from_utf8(output.clone())
        .unwrap()
        .contains("Waiting for ECS RAM Role"));
    assert_ne!(state.questions.active_panel_width, Some(1));
    output.clear();
    poll(
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut output,
    )
    .unwrap();
    assert!(
        output.is_empty(),
        "unchanged width must not repaint the QR panel"
    );
}

#[test]
fn reconfirmed_identity_leaves_the_ecs_field_error_stage() {
    let mut state = state("identity-retry");
    let flow = state.auth.ecs.as_mut().unwrap();
    flow.stage = Stage::Editing;
    flow.error = Some("invalid provider name".into());
    poll(
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut Vec::new(),
    )
    .unwrap();
    let flow = state.auth.ecs.as_ref().unwrap();
    assert_ne!(
        flow.stage,
        Stage::Editing,
        "returning to the ECS phase must restart verification"
    );
}

#[test]
fn confirmed_save_resolves_an_earlier_unknown_outcome() {
    let mut state = state("late-save");
    let worker = thread::spawn(|| Ok(()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let flow = state.auth.ecs.as_mut().unwrap();
    flow.stage = Stage::Unknown;
    flow.operation = Some(Operation::Configure(Some(worker)));
    poll(
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut Vec::new(),
    )
    .unwrap();
    assert!(
        state.auth.state.is_none(),
        "an authoritative save reply must resolve uncertainty"
    );
    assert!(state.auth.completed_ids.contains("late-save"));
}

#[test]
fn completed_save_is_consumed_before_the_observer_timeout() {
    let mut state = state("save");
    let worker = thread::spawn(|| Ok(()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let flow = state.auth.ecs.as_mut().unwrap();
    flow.stage = Stage::Submitting;
    flow.operation = Some(Operation::Configure(Some(worker)));
    flow.operation_deadline = Some(Instant::now() - Duration::from_millis(1));
    poll(
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut Vec::new(),
    )
    .unwrap();
    assert!(state.auth.state.is_none());
    assert!(state.auth.completed_ids.contains("save"));
}

fn completed_configuration(state: &mut InlineState, result: Result<(), AuthConfigureFailure>) {
    let worker = thread::spawn(move || result);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let flow = state.auth.ecs.as_mut().unwrap();
    flow.stage = Stage::Submitting;
    flow.operation = Some(Operation::Configure(Some(worker)));
}

#[test]
fn shell_exit_consumes_successful_configuration_without_cancelling() {
    for stage in [Stage::Submitting, Stage::Unknown] {
        let mut state = state("closed-save");
        completed_configuration(&mut state, Ok(()));
        state.auth.ecs.as_mut().unwrap().stage = stage;
        state.shell_exited = true;
        let mut output = Vec::new();
        poll(
            &AdapterInstance::Fake(FakeAgentAdapter),
            &mut state,
            &mut output,
        )
        .unwrap();
        assert!(state.auth.state.is_none());
        assert!(state.auth.completed_ids.contains("closed-save"));
        assert!(!String::from_utf8(output)
            .unwrap()
            .contains("Auth cancelled"));
    }
}

#[test]
fn shell_exit_keeps_pending_configuration_owned_until_completion() {
    let mut state = state("pending-save");
    let (send, receive) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        Ok(())
    });
    let flow = state.auth.ecs.as_mut().unwrap();
    flow.stage = Stage::Submitting;
    flow.operation = Some(Operation::Configure(Some(worker)));
    flow.operation_deadline = Some(Instant::now() + Duration::from_secs(12));
    state.shell_exited = true;
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll(&adapter, &mut state, &mut output).unwrap();
    let stage = state.auth.ecs.as_ref().unwrap().stage;
    let owned = matches!(
        state.auth.ecs.as_ref().unwrap().operation,
        Some(Operation::Configure(_))
    );
    send.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while state.auth.ecs.is_some() {
        assert!(Instant::now() < deadline);
        poll(&adapter, &mut state, &mut output).unwrap();
        thread::yield_now();
    }
    assert!(owned);
    assert_eq!(stage, Stage::Submitting);
    assert!(!String::from_utf8(output)
        .unwrap()
        .contains("Auth cancelled"));
}

#[test]
fn shell_exit_preserves_unknown_save_result_without_scheduling() {
    let mut state = state("unknown-save");
    completed_configuration(
        &mut state,
        Err(AuthConfigureFailure {
            message: "Save result is unknown".into(),
            code: None,
        }),
    );
    state.shell_exited = true;
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll(&adapter, &mut state, &mut output).unwrap();
    poll(&adapter, &mut state, &mut output).unwrap();
    let flow = state
        .auth
        .ecs
        .as_ref()
        .expect("unknown save must not be cancelled");
    assert_eq!(flow.stage, Stage::Unknown);
    assert!(flow.operation.is_none());
    assert!(!String::from_utf8(output)
        .unwrap()
        .contains("Auth cancelled"));
}

fn probe_fixture(data: Value) -> (tempfile::TempDir, crate::adapter::CoshCoreAdapter) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("registry.sh");
    std::fs::write(&script, format!(
        r##"#!/bin/sh
read -r request
request_id=${{request#*\"request_id\":\"}}
request_id=${{request_id%%\"*}}
printf '%s\n' "$request" >> "$0.calls"
printf '%s\n' "$$" > "$0.pid"
case "$request" in
    *'"action":"configure"'*)
        if [ -f "$0.configure-error" ]; then
            read -r code < "$0.configure-error"
            [ "$code" = transport ] && exit 0
            printf '{{"type":"registry_response","request_id":"%s","success":false,"error":"fixture save rejected","data":{{"error_code":"%s"}}}}\n' "$request_id" "$code"
            exit 0
        fi
        ;;
esac
printf '{{"type":"registry_response","request_id":"%s","success":true,"data":{data}}}\n' "$request_id"
exec sleep 60
"##
    )).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let core = crate::adapter::CoshCoreAdapter::new(script.to_string_lossy(), false);
    (dir, core)
}

fn attach_probe(state: &mut InlineState, core: &crate::adapter::CoshCoreAdapter) {
    state.auth.ecs.as_mut().unwrap().operation = Some(Operation::Probe(
        core.start_ecs_probe("verify").unwrap(),
        false,
    ));
}

fn finish_probe(state: &mut InlineState) -> Vec<u8> {
    finish_operation(&AdapterInstance::Fake(FakeAgentAdapter), state)
}

fn finish_operation(adapter: &AdapterInstance, state: &mut InlineState) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut output = Vec::new();
    while state.auth.ecs.as_ref().unwrap().operation.is_some() {
        assert!(Instant::now() < deadline);
        poll(adapter, state, &mut output).unwrap();
        thread::yield_now();
    }
    output
}

fn registry_calls(dir: &tempfile::TempDir) -> Vec<Value> {
    std::fs::read_to_string(dir.path().join("registry.sh.calls"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn probe_pid(dir: &tempfile::TempDir) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(pid) = std::fs::read_to_string(dir.path().join("registry.sh.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "probe fixture did not start");
        thread::yield_now();
    }
}

fn assert_probe_reaped(pid: i32) {
    assert_eq!(unsafe { nix::libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(nix::libc::ESRCH)
    );
    assert_eq!(
        unsafe { nix::libc::waitpid(pid, std::ptr::null_mut(), nix::libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(nix::libc::ECHILD)
    );
}

#[cfg(target_os = "linux")]
fn await_unconsumed_ready_probe(dir: &tempfile::TempDir) {
    let pid = probe_pid(dir);
    let deadline = Instant::now() + Duration::from_secs(2);
    // The fixture sleeps after Ready; only the probe's response cleanup can reap it.
    while unsafe { nix::libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline, "Ready was not read and reaped");
        thread::yield_now();
    }
    assert_probe_reaped(pid);
    // try_finish consumes the reply. Observe worker exit instead, as in protocol tests,
    // so the very next poll races an already-completed Ready against cancel/timeout.
    while std::fs::read_dir("/proc/self/task").unwrap().any(|entry| {
        std::fs::read_to_string(entry.unwrap().path().join("comm"))
            .is_ok_and(|name| name.starts_with("cosh-auth-ecs"))
    }) {
        assert!(Instant::now() < deadline, "probe worker did not terminate");
        thread::yield_now();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn expired_deadlines_discard_completed_ready_without_configuration() {
    for stage in [Stage::Checking, Stage::Waiting] {
        for (total_expired, operation_expired, expected) in [
            (true, false, Stage::TimedOut),
            (false, true, Stage::Failed),
            (true, true, Stage::TimedOut),
        ] {
            let (dir, core) = probe_fixture(serde_json::json!({"status": "ready"}));
            let mut state = state("expired-ready");
            state.auth.ecs.as_mut().unwrap().stage = stage;
            attach_probe(&mut state, &core);
            await_unconsumed_ready_probe(&dir);
            let now = Instant::now();
            let flow = state.auth.ecs.as_mut().unwrap();
            flow.deadline = Some(if total_expired {
                now - Duration::from_millis(1)
            } else {
                now + WAIT_LIMIT
            });
            flow.operation_deadline = Some(if operation_expired {
                now - Duration::from_millis(1)
            } else {
                now + OPERATION_LIMIT
            });
            let adapter = AdapterInstance::CoshCore(core);
            let mut output = Vec::new();
            poll(&adapter, &mut state, &mut output).unwrap();
            let flow = state.auth.ecs.as_ref().unwrap();
            assert_eq!(flow.stage, expected);
            assert!(
                flow.operation.is_none(),
                "expired Ready must be consumed, not saved"
            );
            poll(&adapter, &mut state, &mut output).unwrap();
            assert_eq!(state.auth.ecs.as_ref().unwrap().stage, expected);
            assert!(state.auth.ecs.as_ref().unwrap().operation.is_none());
            assert!(state.auth.state.is_some());
            assert!(!state.auth.completed_ids.contains("expired-ready"));
            assert!(!String::from_utf8(output)
                .unwrap()
                .contains("Auth configured"));
            let calls = registry_calls(&dir);
            assert_eq!(calls.len(), 1, "expired Ready must never configure");
            assert_eq!(calls[0]["action"], "verify");
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn cancellation_wins_over_ready_already_completed_in_the_same_poll() {
    let (dir, core) = probe_fixture(serde_json::json!({"status": "ready"}));
    let mut state = state("cancel-ready");
    attach_probe(&mut state, &core);
    await_unconsumed_ready_probe(&dir);
    let adapter = AdapterInstance::CoshCore(core);
    let mut output = Vec::new();
    runtime::cancel_auth_panel(&mut state, &mut output).unwrap();
    assert_eq!(state.auth.ecs.as_ref().unwrap().stage, Stage::Cancelling);
    poll(&adapter, &mut state, &mut output).unwrap();
    assert!(state.auth.ecs.is_none());
    assert!(state.auth.state.is_none());
    assert!(state.auth.completed_ids.contains("cancel-ready"));
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Auth cancelled"));
    assert!(!output.contains("Auth configured"));
    let calls = registry_calls(&dir);
    assert_eq!(
        calls.len(),
        1,
        "cancellation must discard Ready without saving"
    );
    assert_eq!(calls[0]["action"], "verify");
}

#[test]
fn unavailable_configure_preserves_identity_and_deadline_and_reprobes_fast_and_waiting() {
    for waited in [false, true] {
        let (dir, core) = probe_fixture(serde_json::json!({
            "status": "not_ready", "reason": "role_missing"
        }));
        std::fs::write(
            dir.path().join("registry.sh.configure-error"),
            "credential_source_unavailable\n",
        )
        .unwrap();
        let mut state = state("unavailable-reprobe");
        let auth = state.auth.state.as_mut().unwrap();
        auth.request_id = "original-request".into();
        auth.collected_values
            .insert("provider_id".into(), "ecs-custom".into());
        auth.editing_provider_name = waited.then(|| "ecs-existing".into());
        let identity = auth.clone();
        let original_deadline = Some(Instant::now() + Duration::from_secs(60));
        state.auth.ecs.as_mut().unwrap().deadline = original_deadline;
        if waited {
            attach_probe(&mut state, &core);
            finish_probe(&mut state);
            assert_eq!(state.auth.ecs.as_ref().unwrap().stage, Stage::Waiting);
        } else {
            assert_eq!(state.auth.ecs.as_ref().unwrap().stage, Stage::Checking);
        }
        let adapter = AdapterInstance::CoshCore(core);
        let mut output = Vec::new();
        start_configuration(&adapter, &mut state, &mut output).unwrap();
        output.extend(finish_operation(&adapter, &mut state));
        let flow = state.auth.ecs.as_ref().unwrap();
        assert_eq!(flow.stage, Stage::Checking);
        assert_eq!(flow.deadline, original_deadline);
        assert!(flow.next_check > Instant::now());
        assert!(!question(&state).2);
        let calls_before_reprobe = registry_calls(&dir);
        let configure = calls_before_reprobe.last().unwrap();
        assert_eq!(configure["action"], "configure");
        assert_eq!(configure["params"]["provider_type"], "aliyun");
        assert_eq!(
            configure["params"]["provider_id"],
            if waited { "ecs-existing" } else { "ecs-custom" }
        );
        assert_eq!(
            configure["params"]["values"],
            serde_json::json!(identity.collected_values)
        );
        poll(&adapter, &mut state, &mut output).unwrap();
        assert!(state.auth.ecs.as_ref().unwrap().operation.is_none());
        assert_eq!(registry_calls(&dir), calls_before_reprobe);
        state.auth.ecs.as_mut().unwrap().next_check = Instant::now() - INTERVAL;
        poll(&adapter, &mut state, &mut output).unwrap();
        assert!(matches!(
            state.auth.ecs.as_ref().unwrap().operation,
            Some(Operation::Probe(_, false))
        ));
        output.extend(finish_operation(&adapter, &mut state));
        let flow = state.auth.ecs.as_ref().unwrap();
        assert_eq!(flow.stage, Stage::Waiting);
        assert_eq!(flow.deadline, original_deadline);
        assert_eq!(flow.id, identity.id);
        let auth = state.auth.state.as_ref().unwrap();
        assert_eq!(auth.id, identity.id);
        assert_eq!(auth.request_id, identity.request_id);
        assert_eq!(auth.collected_values, identity.collected_values);
        assert_eq!(auth.editing_provider_name, identity.editing_provider_name);
        assert_eq!(auth.phase, identity.phase);
        assert!(!state.auth.completed_ids.contains(&identity.id));
        assert!(!String::from_utf8(output)
            .unwrap()
            .contains("Auth configured"));
        let calls = registry_calls(&dir);
        let actions: Vec<_> = calls
            .iter()
            .map(|call| call["action"].as_str().unwrap())
            .collect();
        assert_eq!(
            actions,
            if waited {
                vec!["verify", "configure", "verify"]
            } else {
                vec!["configure", "verify"]
            }
        );
        assert_eq!(
            calls.last().unwrap()["params"]["auth_source"],
            "ecs_ram_role"
        );
    }
}

#[test]
fn rejected_and_transport_failed_saves_never_report_success_or_resubmit() {
    for (code, expected_stage, expected_message) in [
        ("persistence_failed", Stage::Failed, "fixture save rejected"),
        ("transport", Stage::Unknown, "no response received (EOF)"),
    ] {
        let (dir, core) = probe_fixture(serde_json::json!({"status": "ready"}));
        std::fs::write(
            dir.path().join("registry.sh.configure-error"),
            format!("{code}\n"),
        )
        .unwrap();
        let mut state = state("failed-save");
        let adapter = AdapterInstance::CoshCore(core);
        let mut output = Vec::new();
        start_configuration(&adapter, &mut state, &mut output).unwrap();
        output.extend(finish_operation(&adapter, &mut state));
        for overdue in [INTERVAL, INTERVAL * 2] {
            let flow = state.auth.ecs.as_mut().unwrap();
            assert_eq!(flow.stage, expected_stage);
            assert_eq!(flow.error.as_deref(), Some(expected_message));
            assert!(flow.operation.is_none());
            flow.next_check = Instant::now() - overdue;
            flow.deadline = Some(Instant::now() - overdue);
            flow.operation_deadline = Some(Instant::now() - overdue);
            poll(&adapter, &mut state, &mut output).unwrap();
            let flow = state.auth.ecs.as_ref().unwrap();
            assert_eq!(flow.stage, expected_stage);
            assert!(
                flow.operation.is_none(),
                "terminal save must not start any worker"
            );
            assert_eq!(
                registry_calls(&dir).len(),
                1,
                "configure must remain single-shot"
            );
        }
        assert_eq!(state.auth.state.as_ref().unwrap().id, "failed-save");
        assert!(!state.auth.completed_ids.contains("failed-save"));
        assert_eq!(registry_calls(&dir)[0]["action"], "configure");
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(expected_message));
        assert!(!output.contains("Auth configured"));
        assert!(!output.contains("credentials saved"));
        if expected_stage == Stage::Unknown {
            assert_eq!(question(&state).1, vec!["Return to provider management"]);
        }
    }
}

#[test]
fn cancelled_and_reaped_probe_stays_idle_despite_overdue_checks() {
    let (dir, core) = probe_fixture(Value::Null);
    std::fs::write(
        dir.path().join("registry.sh"),
        "#!/bin/sh\nread -r request\nprintf '%s\\n' \"$request\" >> \"$0.calls\"\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nexec sleep 60\n",
    )
    .unwrap();
    let mut state = state("cancel-running");
    attach_probe(&mut state, &core);
    let pid = probe_pid(&dir);
    let flow = state.auth.ecs.as_mut().unwrap();
    // Leave the check overdue by two intervals; cancellation must retire the timer.
    flow.next_check = Instant::now() - INTERVAL * 2;
    flow.deadline = Some(Instant::now() - INTERVAL * 2);
    let adapter = AdapterInstance::CoshCore(core);
    let mut output = Vec::new();
    runtime::cancel_auth_panel(&mut state, &mut output).unwrap();
    assert_eq!(state.auth.ecs.as_ref().unwrap().stage, Stage::Cancelling);
    let limit = Instant::now() + Duration::from_secs(2);
    while state.auth.ecs.is_some() {
        assert!(Instant::now() < limit, "cancelled probe was not reaped");
        poll(&adapter, &mut state, &mut output).unwrap();
        thread::yield_now();
    }
    assert_probe_reaped(pid);
    assert!(state.auth.state.is_none());
    assert!(state.auth.completed_ids.contains("cancel-running"));
    let calls = registry_calls(&dir);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["action"], "verify");
    let notice = String::from_utf8(output.clone()).unwrap();
    assert!(notice.contains("Auth cancelled"));
    assert!(!notice.contains("Auth configured"));
    output.clear();
    for _ in 0..2 {
        poll(&adapter, &mut state, &mut output).unwrap();
        thread::yield_now();
        assert!(
            state.auth.ecs.is_none(),
            "cancelled flow must never respawn"
        );
        assert!(state.auth.state.is_none());
        assert_eq!(
            registry_calls(&dir),
            calls,
            "cancelled flow must not probe or save again"
        );
        assert!(
            output.is_empty(),
            "cancelled flow must not publish later results"
        );
    }
}

#[test]
fn ecs_refresh_message_is_appended_and_bilingual() {
    use crate::config::Language;
    // Tail ownership moved to the appended #3413 health-confinement segment, so
    // this keeps the ECS messages pinned to their own segment position; the
    // current tail is asserted in the i18n discriminant test.
    let id = MessageId::AuthEcsRefreshing;
    assert_eq!(id as usize, MessageId::AuthEcsCancelHint as usize + 1);
    assert_eq!(
        I18n::new(Language::EnUs).t(id),
        "Waiting for ECS credentials to refresh. Configuration will continue automatically."
    );
    assert_eq!(
        I18n::new(Language::ZhCn).t(id),
        "正在等待 ECS 凭据刷新，刷新后将自动继续配置。"
    );
}

#[test]
fn expired_credentials_wait_for_refresh_without_authorization_qr() {
    let (_dir, core) = probe_fixture(serde_json::json!({
        "status": "not_ready", "reason": "credentials_expired"
    }));
    let mut state = state("expired");
    attach_probe(&mut state, &core);
    finish_probe(&mut state);
    let (text, options, qr) = question(&state);
    assert!(
        !qr,
        "expired credentials must not ask for RAM Role authorization"
    );
    assert!(options.is_empty());
    assert!(
        text.contains("Waiting for ECS credentials to refresh"),
        "{text}"
    );
    state.language = crate::config::Language::ZhCn;
    assert!(question(&state).0.contains("等待 ECS 凭据刷新"));
    let flow = state.auth.ecs.as_ref().unwrap();
    assert!(flow.next_check > Instant::now());
    assert!(flow.next_check <= Instant::now() + INTERVAL);
}

#[test]
fn missing_role_keeps_authorization_wait_and_qr() {
    let (_dir, core) = probe_fixture(serde_json::json!({
        "status": "not_ready", "reason": "role_missing"
    }));
    let mut state = state("missing-role");
    attach_probe(&mut state, &core);
    finish_probe(&mut state);
    assert_eq!(state.auth.ecs.as_ref().unwrap().stage, Stage::Waiting);
    assert!(question(&state).2);
}

#[test]
fn unavailable_configure_rechecks_before_claiming_authorization_is_missing() {
    let mut state = state("unavailable");
    completed_configuration(
        &mut state,
        Err(AuthConfigureFailure {
            message: "ECS credentials unavailable".into(),
            code: Some("credential_source_unavailable".into()),
        }),
    );
    // Inspect the transition before the next scheduled probe.
    state.auth.ecs.as_mut().unwrap().next_check = Instant::now() + INTERVAL;
    poll(
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(state.auth.ecs.as_ref().unwrap().stage, Stage::Checking);
    assert!(!question(&state).2);
}

#[test]
fn shell_exit_discards_ready_probe_without_starting_configuration() {
    let (dir, core) = probe_fixture(serde_json::json!({"status": "ready"}));
    let mut state = state("closed-probe");
    attach_probe(&mut state, &core);
    let deadline = Instant::now() + Duration::from_secs(2);
    // The probe only reaps this sleeping fixture after reading its Ready reply.
    loop {
        assert!(Instant::now() < deadline);
        if let Some(pid) = std::fs::read_to_string(dir.path().join("registry.sh.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<i32>().ok())
        {
            if unsafe { nix::libc::kill(pid, 0) } == -1 {
                break;
            }
        }
        thread::yield_now();
    }
    state.shell_exited = true;
    let adapter = AdapterInstance::CoshCore(core);
    while state.auth.ecs.is_some() {
        assert!(Instant::now() < deadline);
        poll(&adapter, &mut state, &mut Vec::new()).unwrap();
        thread::yield_now();
    }
    assert!(state.auth.state.is_none());
    let calls = std::fs::read_to_string(dir.path().join("registry.sh.calls")).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "shutdown must not schedule configure"
    );
}

#[test]
fn active_run_ready_clears_auth_panel_before_sending_response() {
    for stage in [Stage::Checking, Stage::Waiting] {
        for send_succeeds in [true, false] {
            let (dir, core) = probe_fixture(serde_json::json!({"status": "ready"}));
            let mut state = state("active-ready");
            state.auth.state.as_mut().unwrap().backend = AuthBackend::ActiveRun;
            state.auth.ecs.as_mut().unwrap().stage = stage;
            let (mut active, _approval_rx) =
                crate::agent::run::test_support::test_active_run_with_id("active-owner");
            let (auth_tx, auth_rx) = std::sync::mpsc::channel();
            active.handle.auth_sender = Some(auth_tx);
            state.agent_run.active = Some(active);
            let auth_rx = send_succeeds.then_some(auth_rx);
            let mut output = Vec::new();
            render_current_auth_panel(&mut state, &mut output).unwrap();
            let height = state.questions.active_panel_height;
            assert!(height > 0);
            output.clear();
            attach_probe(&mut state, &core);
            let adapter = AdapterInstance::CoshCore(core);
            let deadline = Instant::now() + Duration::from_secs(2);
            while state.auth.state.is_some() {
                assert!(Instant::now() < deadline);
                poll(&adapter, &mut state, &mut output).unwrap();
                thread::yield_now();
            }
            assert!(state.auth.ecs.is_none());
            assert_eq!(state.questions.active_panel_height, 0);
            assert!(state.questions.active_panel_id.is_none());
            assert!(state.questions.active_panel_width.is_none());
            assert!(state.auth.completed_ids.contains("active-ready"));
            let output = String::from_utf8(output).unwrap();
            assert!(output.starts_with(&format!("\x1b[{height}A")), "{output}");
            assert!(!output.contains("credentials saved"));
            if let Some(auth_rx) = auth_rx {
                let response = auth_rx.try_recv().unwrap();
                assert_eq!(response.request_id, "active-ready");
                assert_eq!(response.provider_id, "aliyun");
                assert_eq!(response.values["auth_source"], "ecs_ram_role");
                assert!(auth_rx.try_recv().is_err());
            } else {
                assert!(output.contains("Auth failed"), "{output}");
            }
            let calls = registry_calls(&dir);
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0]["action"], "verify");
        }
    }
}

#[test]
fn changing_wait_reason_repaints_the_existing_panel() {
    for (reason, refreshing, message) in [
        (
            "credentials_expired",
            true,
            "Waiting for ECS credentials to refresh",
        ),
        (
            "role_missing",
            false,
            "Waiting for ECS RAM Role authorization",
        ),
    ] {
        let (_dir, core) = probe_fixture(serde_json::json!({
            "status": "not_ready", "reason": reason
        }));
        let mut state = state("changed-reason");
        let flow = state.auth.ecs.as_mut().unwrap();
        flow.stage = Stage::Waiting;
        flow.waiting_for_refresh = !refreshing;
        attach_probe(&mut state, &core);
        let output = String::from_utf8(finish_probe(&mut state)).unwrap();
        assert!(
            output.contains(message),
            "reason changes must repaint: {output}"
        );
        assert_eq!(question(&state).2, !refreshing);
    }
}

#[test]
fn refresh_wait_uses_the_same_schedule_and_deadline_without_authorization_claims() {
    let (_dir, core) = probe_fixture(serde_json::json!({
        "status": "not_ready", "reason": "credentials_expired"
    }));
    let mut state = state("refresh-schedule");
    let original_deadline = state.auth.ecs.as_ref().unwrap().deadline;
    attach_probe(&mut state, &core);
    finish_probe(&mut state);
    assert_eq!(state.auth.ecs.as_ref().unwrap().deadline, original_deadline);
    assert!(original_deadline.unwrap() <= Instant::now() + WAIT_LIMIT);
    let adapter = AdapterInstance::CoshCore(core);
    // The future next_check prevents an early probe.
    poll(&adapter, &mut state, &mut Vec::new()).unwrap();
    assert!(state.auth.ecs.as_ref().unwrap().operation.is_none());
    state.auth.ecs.as_mut().unwrap().next_check = Instant::now();
    poll(&adapter, &mut state, &mut Vec::new()).unwrap();
    assert!(matches!(
        state.auth.ecs.as_ref().unwrap().operation,
        Some(Operation::Probe(..))
    ));
    state.auth.ecs.as_mut().unwrap().deadline = Some(Instant::now());
    let limit = Instant::now() + Duration::from_secs(2);
    while state.auth.ecs.as_ref().unwrap().stage != Stage::TimedOut {
        assert!(Instant::now() < limit);
        poll(&adapter, &mut state, &mut Vec::new()).unwrap();
        thread::yield_now();
    }
    assert!(state.auth.ecs.as_ref().unwrap().operation.is_none());
    assert!(!question(&state).2);
    assert!(!question(&state).0.contains("authorization"));
    state.language = crate::config::Language::ZhCn;
    assert!(!question(&state).0.contains("授权"));
}

#[test]
fn shell_exit_does_not_schedule_new_prepare_verify_or_configure() {
    for stage in [Stage::Preparing, Stage::Checking, Stage::Waiting] {
        let (dir, core) = probe_fixture(serde_json::json!({"status": "ready"}));
        let adapter = AdapterInstance::CoshCore(core);
        let mut state = state("closed-idle");
        state.auth.ecs.as_mut().unwrap().stage = stage;
        state.shell_exited = true;
        start_configuration(&adapter, &mut state, &mut Vec::new()).unwrap();
        assert!(state.auth.ecs.as_ref().unwrap().operation.is_none());
        poll(&adapter, &mut state, &mut Vec::new()).unwrap();
        assert!(state.auth.ecs.is_none());
        assert!(!dir.path().join("registry.sh.calls").exists());
    }
}

#[test]
fn shell_exit_cancels_and_reaps_an_inflight_probe() {
    let (dir, core) = probe_fixture(Value::Null);
    std::fs::write(
        dir.path().join("registry.sh"),
        "#!/bin/sh\nread -r request\nprintf '%s\\n' \"$$\" > \"$0.pid\"\nexec sleep 60\n",
    )
    .unwrap();
    let mut state = state("closed-running-probe");
    attach_probe(&mut state, &core);
    let deadline = Instant::now() + Duration::from_secs(2);
    let pid = loop {
        assert!(Instant::now() < deadline);
        if let Some(pid) = std::fs::read_to_string(dir.path().join("registry.sh.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<i32>().ok())
        {
            break pid;
        }
        thread::yield_now();
    };
    state.shell_exited = true;
    let adapter = AdapterInstance::CoshCore(core);
    while state.auth.ecs.is_some() {
        assert!(Instant::now() < deadline);
        poll(&adapter, &mut state, &mut Vec::new()).unwrap();
        thread::yield_now();
    }
    assert!(state.auth.state.is_none());
    assert_eq!(unsafe { nix::libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(nix::libc::ESRCH)
    );
    assert_eq!(
        unsafe { nix::libc::waitpid(pid, std::ptr::null_mut(), nix::libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(nix::libc::ECHILD)
    );
}
