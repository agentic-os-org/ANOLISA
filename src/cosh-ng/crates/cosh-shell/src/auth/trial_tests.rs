use super::*;
use crate::agent::run::test_support::test_active_run_with_id;

#[test]
fn ecs_builtin_enable_activates_provider_instead_of_only_saving_preference() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("registry.sh");
    std::fs::write(&script, "#!/bin/sh\nread -r request\nprintf '%s\\n' \"$request\" >> \"$0.requests\"\nprintf '%s\\n' '{\"success\":true,\"data\":{\"active_provider\":\"ecs-trial\",\"model\":\"ecs-model\"}}'\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let core = CoshCoreAdapter::new(script.to_string_lossy(), false);
    let mut state = InlineState::default();
    state.auth.ecs_trial.auto_use_allowed = false;
    state.auth.ecs_trial.cancelled_question = true;
    state.personalization.foreground_model = Some("previous-model".into());
    begin_preference(core, true, &mut state);
    state
        .auth
        .ecs_trial
        .preference_worker
        .take()
        .unwrap()
        .join()
        .unwrap();
    let request: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(dir.path().join("registry.sh.requests"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(request["action"], "activate_trial");
    assert!(request["params"].is_null());
    let adapter = crate::adapter::adapter_for_kind(crate::adapter::AdapterKind::Fake);
    poll(&adapter, &mut state, &mut Vec::new()).unwrap();
    assert!(state.auth.ecs_trial.auto_use_allowed);
    assert!(state.auth.ecs_trial.cancelled_question);
    assert!(state.personalization.foreground_model.is_none());
}

#[test]
fn review5_failed_switch_preserves_previous_session_permission_without_success_notice() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("registry.sh");
    std::fs::write(&script, "#!/bin/sh\nread -r request\nprintf '%s\\n' '{\"success\":false,\"error\":\"cannot activate\"}'\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    for allowed_before in [true, false] {
        let core = CoshCoreAdapter::new(script.to_string_lossy(), false);
        let mut state = InlineState::default();
        state.auth.ecs_trial.auto_use_allowed = allowed_before;
        state.auth.ecs_trial.cancelled_question = true;
        state.personalization.foreground_model = Some("own-model".into());
        begin_preference(core, true, &mut state);
        state
            .auth
            .ecs_trial
            .preference_worker
            .take()
            .unwrap()
            .join()
            .unwrap();
        let adapter = crate::adapter::adapter_for_kind(crate::adapter::AdapterKind::Fake);
        let mut output = Vec::new();
        poll(&adapter, &mut state, &mut output).unwrap();
        assert_eq!(state.auth.ecs_trial.auto_use_allowed, allowed_before);
        assert!(state.auth.ecs_trial.cancelled_question);
        assert_eq!(
            state.personalization.foreground_model.as_deref(),
            Some("own-model")
        );
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("已切换"), "{output}");
        assert!(!output.contains("next question"), "{output}");
    }
}

fn progress(revision: u64, phase: &str) -> EcsTrialProgress {
    EcsTrialProgress {
        run_id: "opaque".into(),
        flow_id: "flow".into(),
        revision,
        phase: phase.into(),
        code: None,
        dispatch_state: "not_dispatched".into(),
        local_terminated: false,
        resume_allowed: false,
    }
}

#[test]
fn waiting_card_uses_crlf_without_terminal_output_processing() {
    let mut state = InlineState::default();
    let (run, _receiver) = test_active_run_with_id("owner");
    state.agent_run.active = Some(run);
    let mut output = Vec::new();
    observe(
        &mut state,
        "owner",
        progress(1, "waiting_activation"),
        &mut output,
    )
    .unwrap();
    assert!(output.contains(&b'\n'));
    for (index, byte) in output.iter().enumerate() {
        if *byte == b'\n' {
            assert!(
                index > 0 && output[index - 1] == b'\r',
                "bare LF at {index}"
            );
        }
    }
}

#[test]
fn trial_notices_use_crlf_without_terminal_output_processing() {
    let mut output = Vec::new();
    notice(&InlineState::default(), &mut output, "提示", "notice").unwrap();
    assert!(output.ends_with(b"\r\n"));
}

#[test]
fn already_activated_request_never_displays_activation_success() {
    let mut state = InlineState::default();
    let (run, _receiver) = test_active_run_with_id("owner");
    state.agent_run.active = Some(run);
    let mut output = Vec::new();
    observe(&mut state, "owner", progress(1, "preparing"), &mut output).unwrap();
    observe(&mut state, "owner", progress(1, "ready"), &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(!output.contains("Activated; continuing"), "{output}");
}

#[test]
fn failed_progress_clears_the_card_without_duplicating_the_terminal_error() {
    let mut state = InlineState {
        language: crate::config::Language::EnUs,
        ..InlineState::default()
    };
    let (run, _receiver) = test_active_run_with_id("owner");
    state.agent_run.active = Some(run);
    let mut output = Vec::new();
    observe(&mut state, "owner", progress(1, "preparing"), &mut output).unwrap();
    let mut failure = progress(1, "failed");
    failure.code = Some("Sysom.CreditsExhausted".into());
    failure.local_terminated = true;
    observe(&mut state, "owner", failure, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(
        !output.contains("Available quota is insufficient."),
        "{output}"
    );
    assert_eq!(
        state.auth.ecs_trial.flow.as_ref().unwrap().phase,
        TrialPhase::Failed
    );
}

#[test]
fn disconnected_preference_worker_releases_ui_with_session_only_failure() {
    let mut state = InlineState::default();
    state.auth.ecs_trial.auto_use_allowed = false;
    let (sender, receiver) = mpsc::channel();
    drop(sender);
    state.auth.ecs_trial.preference_job = Some(receiver);
    let adapter = crate::adapter::adapter_for_kind(crate::adapter::AdapterKind::Fake);
    let mut output = Vec::new();
    poll(&adapter, &mut state, &mut output).unwrap();
    assert!(state.auth.ecs_trial.preference_job.is_none());
    assert!(!state.auth.ecs_trial.auto_use_allowed);
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("this session only"));
}

#[test]
fn unknown_settings_survive_agent_terminal_and_never_resume() {
    let mut state = InlineState::default();
    let mut flow = TrialFlow::new(progress(1, "waiting_activation"));
    flow.settings();
    let mut paused = progress(2, "paused");
    paused.local_terminated = true;
    paused.dispatch_state = "unknown".into();
    assert!(flow.observe(paused));
    state.auth.ecs_trial.owner = Some("owner".into());
    state.auth.ecs_trial.flow = Some(flow);
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter::new("/not-used", false));
    poll(&adapter, &mut state, &mut Vec::new()).unwrap();
    let flow = state.auth.ecs_trial.flow.as_ref().unwrap();
    assert_eq!(flow.phase, TrialPhase::Settings);
    assert!(!flow.can_resume());
    assert!(matches!(
        pending_capture(&state),
        Some(RawInputCapture::TrialWaiting {
            settings: Some(_),
            ..
        })
    ));
}

#[test]
fn local_pause_ack_disarms_cleanup_timer_while_user_reads_settings() {
    let mut state = InlineState::default();
    let (run, _receiver) = test_active_run_with_id("owner");
    state.agent_run.active = Some(run);
    let mut output = Vec::new();
    observe(
        &mut state,
        "owner",
        progress(1, "waiting_activation"),
        &mut output,
    )
    .unwrap();
    state.auth.ecs_trial.flow.as_mut().unwrap().settings();
    state.auth.ecs_trial.deadline = Some(Instant::now() + Duration::from_secs(2));
    let mut paused = progress(2, "paused");
    paused.local_terminated = true;
    paused.resume_allowed = true;
    observe(&mut state, "owner", paused, &mut output).unwrap();
    assert!(state.auth.ecs_trial.deadline.is_none());
}

#[test]
fn cancel_ack_keeps_cleanup_timer_until_result_and_actual_run_release() {
    let mut state = InlineState::default();
    let (run, _receiver) = test_active_run_with_id("owner");
    state.agent_run.active = Some(run);
    let mut output = Vec::new();
    observe(
        &mut state,
        "owner",
        progress(1, "waiting_activation"),
        &mut output,
    )
    .unwrap();
    let flow = state.auth.ecs_trial.flow.as_mut().unwrap();
    flow.phase = TrialPhase::Cancelling;
    flow.progress.revision = 2;
    state.auth.ecs_trial.deadline = Some(Instant::now() + Duration::from_secs(2));
    let mut terminated = progress(2, "terminated");
    terminated.local_terminated = true;
    observe(&mut state, "owner", terminated, &mut output).unwrap();
    assert_eq!(
        state.auth.ecs_trial.flow.as_ref().unwrap().phase,
        TrialPhase::Closed
    );
    assert!(state.auth.ecs_trial.deadline.is_some());
}

#[test]
fn access_copy_partial_activation_preserves_applied_provider_without_replay() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("registry.sh");
    std::fs::write(&script, r#"#!/bin/sh
read -r request
printf '%s\n' '{"success":false,"data":{"provider_updated":true,"error_code":"io_error"},"error":"session persistence failed"}'
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let core = CoshCoreAdapter::new(script.to_string_lossy(), false);
    let mut state = InlineState::default();
    state.auth.ecs_trial.auto_use_allowed = false;
    state.auth.ecs_trial.disable_save_failed = true;
    state.auth.ecs_trial.cancelled_question = true;
    state.personalization.foreground_model = Some("own-model".into());
    begin_preference(core, true, &mut state);
    state
        .auth
        .ecs_trial
        .preference_worker
        .take()
        .unwrap()
        .join()
        .unwrap();
    let adapter = crate::adapter::adapter_for_kind(crate::adapter::AdapterKind::Fake);
    let mut output = Vec::new();
    poll(&adapter, &mut state, &mut output).unwrap();
    assert!(state.auth.ecs_trial.auto_use_allowed);
    assert!(!state.auth.ecs_trial.disable_save_failed);
    assert!(state.auth.ecs_trial.cancelled_question);
    assert!(state.personalization.foreground_model.is_none());
    assert!(state.agent_run.active.is_none());
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("session could not be saved"), "{text}");
    assert!(!text.contains("Unable to save; retry"), "{text}");
}

#[test]
fn access_copy_multiline_quota_notice_keeps_crlf_in_raw_terminal() {
    let state = InlineState::default();
    let mut output = Vec::new();
    notice(
        &state,
        &mut output,
        "额度不足。\nhttps://console.aliyun.com/alinux",
        "Quota insufficient.\nhttps://console.aliyun.com/alinux",
    )
    .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert_eq!(
        text,
        "Quota insufficient.\r\nhttps://console.aliyun.com/alinux\r\n"
    );
}

#[test]
fn kongche_idle_waiting_reports_connection_failure_instead_of_user_cancellation() {
    let mut state = InlineState {
        language: crate::config::Language::EnUs,
        ..InlineState::default()
    };
    state.auth.ecs_trial.owner = Some("owner".into());
    state.auth.ecs_trial.flow = Some(TrialFlow::new(progress(1, "waiting_activation")));
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter::new("/not-used", false));
    let mut output = Vec::new();
    poll(&adapter, &mut state, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains("Cancelled"), "{text}");
    assert!(text.contains("connection"), "{text}");
    assert!(state.auth.ecs_trial.flow.is_none());
}

#[test]
fn kongche_only_waiting_capture_allows_opening_settings() {
    for phase in [
        TrialPhase::Waiting,
        TrialPhase::Pausing,
        TrialPhase::Cancelling,
    ] {
        let mut state = InlineState::default();
        let mut flow = TrialFlow::new(progress(1, "waiting_activation"));
        flow.phase = phase;
        state.auth.ecs_trial.flow = Some(flow);
        assert!(
            matches!(pending_capture(&state), Some(RawInputCapture::TrialWaiting { allow_settings, .. }) if allow_settings == (phase == TrialPhase::Waiting))
        );
    }
}

#[test]
fn kongche_idle_after_failed_progress_keeps_the_specific_failure() {
    let mut state = InlineState {
        language: crate::config::Language::EnUs,
        ..InlineState::default()
    };
    let mut failure = progress(1, "failed");
    failure.code = Some("Sysom.CreditsExhausted".into());
    let mut flow = TrialFlow::new(failure);
    flow.phase = TrialPhase::Failed;
    state.auth.ecs_trial.flow = Some(flow);
    state.auth.ecs_trial.owner = Some("owner".into());
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter::new("/not-used", false));
    let mut output = Vec::new();
    poll(&adapter, &mut state, &mut output).unwrap();
    poll(&adapter, &mut state, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert_eq!(
        text.matches("Available quota is insufficient.").count(),
        1,
        "{text}"
    );
    assert!(!text.contains("Cancelled"), "{text}");
}
