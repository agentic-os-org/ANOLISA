use super::*;

fn card_event(message: &str, input: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("task-form-test", input);
    event.component = Some("card".to_owned());
    event.message = Some(message.to_owned());
    event
}

fn unavailable_checkpoint_capabilities() -> TaskCapabilities {
    TaskCapabilities {
        workspace: "repo\u{1b}[31m\nworkspace".to_owned(),
        workspace_scope_digest: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
        runtimes: vec![
            RuntimeCapability {
                runtime: TaskRuntime::Core,
                readiness: CapabilityReadiness::Unavailable(
                    "cosh-core is not installed\u{1b}[2J".to_owned(),
                ),
                security: RuntimeSecurityPosture {
                    delegated_local_authority: true,
                    gateway_brokered_effects: false,
                    checkpoint_is_baseline_only: false,
                },
            },
            RuntimeCapability {
                runtime: TaskRuntime::Codex,
                readiness: CapabilityReadiness::Ready,
                security: RuntimeSecurityPosture {
                    delegated_local_authority: true,
                    gateway_brokered_effects: false,
                    checkpoint_is_baseline_only: false,
                },
            },
        ],
        checkpoint: CapabilityReadiness::Unavailable("ws-ckpt daemon is offline".to_owned()),
    }
}

#[test]
fn task_form_defaults_to_core_and_auto_with_prefilled_goal() {
    let mut state = InlineState::default();
    let mut output = Vec::new();
    open_task_form_with_capabilities(
        &mut state,
        "update dependencies".to_owned(),
        TaskCapabilities::ready(),
        &mut output,
    )
    .unwrap();

    let RawInputCapture::TextQuestion { initial_text, .. } =
        pending_task_form_capture(&state).expect("goal capture")
    else {
        panic!("expected text question capture");
    };
    assert_eq!(initial_text, "update dependencies");
    let form = state.task_form.active.as_ref().unwrap();
    assert_eq!(form.runtime, TaskRuntime::Core);
    assert_eq!(form.checkpoint, TaskCheckpoint::Auto);
}

#[test]
fn task_form_advances_and_escape_steps_back() {
    let mut state = InlineState::default();
    let mut output = Vec::new();
    open_task_form_with_capabilities(
        &mut state,
        "run tests".to_owned(),
        TaskCapabilities::ready(),
        &mut output,
    )
    .unwrap();

    render_task_form_actions(
        &[card_event("answer", "run tests")],
        &mut state,
        &mut output,
        10,
    )
    .unwrap();
    let runtime_id = match pending_task_form_capture(&state).unwrap() {
        RawInputCapture::Question { id, selected, .. } => {
            assert_eq!(selected, 0);
            id
        }
        capture => panic!("expected runtime capture, got {capture:?}"),
    };
    render_task_form_actions(
        &[
            card_event("focus", &format!("{runtime_id}:1")),
            card_event("answer", "2"),
        ],
        &mut state,
        &mut output,
        11,
    )
    .unwrap();
    render_task_form_actions(&[card_event("answer", "1")], &mut state, &mut output, 13).unwrap();
    let confirm_id = match pending_task_form_capture(&state).unwrap() {
        RawInputCapture::Question { id, .. } => id,
        capture => panic!("expected confirm capture, got {capture:?}"),
    };
    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains("Goal"));
    assert!(rendered.contains("Launch"));
    assert!(rendered.contains("Authority"));
    assert!(rendered.contains("Approval: allow_all"));

    render_task_form_actions(
        &[card_event("question_cancel", &confirm_id)],
        &mut state,
        &mut output,
        14,
    )
    .unwrap();
    let form = state.task_form.active.as_ref().unwrap();
    assert_eq!(form.phase, TaskFormPhase::Checkpoint);
    assert_eq!(form.runtime, TaskRuntime::Codex);
}

#[test]
fn task_answer_is_reserved_from_question_consumer_and_ctrl_c_aborts() {
    let mut state = InlineState::default();
    let mut output = Vec::new();
    open_task_form_with_capabilities(
        &mut state,
        String::new(),
        TaskCapabilities::ready(),
        &mut output,
    )
    .unwrap();
    let answer = card_event("answer", "inspect logs");
    let expected_key = stable_event_key("question-answer", 20, &answer);
    render_task_form_actions(&[answer], &mut state, &mut output, 20).unwrap();
    assert!(state.questions.handled_answers.contains(&expected_key));

    let runtime_id = match pending_task_form_capture(&state).unwrap() {
        RawInputCapture::Question { id, .. } => id,
        capture => panic!("expected runtime capture, got {capture:?}"),
    };
    render_task_form_actions(
        &[card_event("question_abort", &runtime_id)],
        &mut state,
        &mut output,
        21,
    )
    .unwrap();
    assert!(state.task_form.active.is_none());
    assert!(state.trigger_pty_prompt);
}

#[test]
fn task_capabilities_parse_workspace_and_readiness() {
    let capabilities = parse_task_capabilities(&serde_json::json!({
        "event": "task_capabilities",
        "launch_schema_version": 1,
        "default_workspace": {
            "scope_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "display_name": "cosh-ng"
        },
        "runtimes": [
            {
                "runtime": "core",
                "readiness": {"status": "ready"},
                "security": {
                    "delegated_local_authority": true,
                    "gateway_brokered_effects": false,
                    "checkpoint_is_baseline_only": false
                }
            },
            {
                "runtime": "codex",
                "readiness": {"status": "unavailable", "reason": "adapter missing"},
                "security": {
                    "delegated_local_authority": true,
                    "gateway_brokered_effects": false,
                    "checkpoint_is_baseline_only": false
                }
            }
        ],
        "checkpoint": {"status": "unavailable", "reason": "daemon offline"},
        "default_approval": "allow_all"
    }))
    .unwrap();

    assert_eq!(capabilities.workspace, "cosh-ng");
    assert!(capabilities
        .runtime_readiness(TaskRuntime::Core)
        .unwrap()
        .is_ready());
    assert_eq!(
        capabilities
            .runtime_readiness(TaskRuntime::Codex)
            .unwrap()
            .reason(),
        Some("adapter missing")
    );
    assert_eq!(capabilities.checkpoint_options().len(), 2);
    assert!(!capabilities
        .checkpoint_options()
        .contains(&TaskCheckpoint::On));
}

#[test]
fn unavailable_choices_are_omitted_and_auto_discloses_downgrade() {
    let mut state = InlineState::default();
    let mut output = Vec::new();
    assert!(open_task_form_with_capabilities(
        &mut state,
        "run tests".to_owned(),
        unavailable_checkpoint_capabilities(),
        &mut output,
    )
    .unwrap());
    assert_eq!(
        state.task_form.active.as_ref().unwrap().runtime,
        TaskRuntime::Codex
    );

    advance_form(&mut state, "run tests", &mut output).unwrap();
    let RawInputCapture::Question {
        option_count,
        selected,
        ..
    } = pending_task_form_capture(&state).unwrap()
    else {
        panic!("expected Runtime capture");
    };
    assert_eq!(option_count, 1);
    assert_eq!(selected, 0);
    assert!(String::from_utf8_lossy(&output).contains("cosh-core is not installed[2J"));

    advance_form(&mut state, "1", &mut output).unwrap();
    let RawInputCapture::Question { option_count, .. } = pending_task_form_capture(&state).unwrap()
    else {
        panic!("expected Checkpoint capture");
    };
    assert_eq!(option_count, 2, "On must not be selectable");
    advance_form(&mut state, "1", &mut output).unwrap();
    let confirmation = question_text(
        state.task_form.active.as_ref().unwrap(),
        crate::config::Language::EnUs,
    );
    assert!(confirmation.contains("Gateway records an explicit downgrade"));
    assert!(confirmation.contains("Workspace: repo[31m workspace"));
    assert!(confirmation.contains("delegated local service-user authority"));
    assert!(confirmation.contains("Runtime-native effects use the service user's authority"));
    assert!(confirmation.contains("barrier before each approved Runtime effect"));
    assert!(
        confirmation.contains("do not protect network, credentials, cloud resources"),
        "{confirmation}"
    );
    assert!(!confirmation.contains('\u{1b}'));
}

#[test]
fn task_capabilities_require_complete_security_posture() {
    let error = parse_task_capabilities(&serde_json::json!({
        "event": "task_capabilities",
        "launch_schema_version": 1,
        "default_workspace": {
            "scope_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "display_name": "cosh-ng"
        },
        "runtimes": [
            {
                "runtime": "core",
                "readiness": {"status": "ready"},
                "security": {
                    "delegated_local_authority": true,
                    "gateway_brokered_effects": false
                }
            },
            {
                "runtime": "codex",
                "readiness": {"status": "ready"},
                "security": {
                    "delegated_local_authority": true,
                    "gateway_brokered_effects": false,
                    "checkpoint_is_baseline_only": false
                }
            }
        ],
        "checkpoint": {"status": "ready"},
        "default_approval": "allow_all"
    }))
    .unwrap_err();

    assert!(error.contains("checkpoint_is_baseline_only"));
}

#[test]
fn task_goal_preview_removes_terminal_controls_and_line_breaks() {
    let preview = safe_goal_preview("before\u{1b}[31m\nafter\tvalue\r\u{7}");
    assert_eq!(preview, "before[31m after value");
    assert!(!preview.chars().any(char::is_control));
}

#[test]
fn task_goal_preview_is_bounded_at_a_utf8_boundary() {
    let preview = safe_goal_preview(&"任务".repeat(MAX_RENDERED_GOAL_BYTES));
    assert!(preview.ends_with('…'));
    assert!(preview.len() <= MAX_RENDERED_GOAL_BYTES + '…'.len_utf8());
    assert!(preview.is_char_boundary(preview.len()));
}

#[test]
fn core_security_summary_discloses_runtime_authority_and_checkpoint_barrier() {
    let posture = RuntimeSecurityPosture {
        delegated_local_authority: true,
        gateway_brokered_effects: false,
        checkpoint_is_baseline_only: false,
    };
    let english = security_summary(posture, true);
    assert!(english.contains("delegated local service-user authority"));
    assert!(english.contains("after Gateway approval"));
    assert!(english.contains("barrier before each approved Runtime effect"));

    let chinese = security_summary(posture, false);
    assert!(chinese.contains("委托本地服务用户权限"));
    assert!(chinese.contains("经 Gateway 审批后"));
    assert!(chinese.contains("操作前增加屏障快照"));
    assert!(!chinese.contains("Authority:"));
}

#[test]
fn task_form_titles_and_keys_are_localized_for_all_four_steps() {
    let mut state = InlineState::default();
    let mut output = Vec::new();
    open_task_form_with_capabilities(
        &mut state,
        "ship release".to_owned(),
        TaskCapabilities::ready(),
        &mut output,
    )
    .unwrap();
    let form = state.task_form.active.as_mut().unwrap();

    for (phase, english_heading, chinese_heading) in [
        (
            TaskFormPhase::Goal,
            "Step 1 of 4 · Goal",
            "第 1/4 步 · 目标",
        ),
        (
            TaskFormPhase::Runtime,
            "Step 2 of 4 · Runtime",
            "第 2/4 步 · Runtime",
        ),
        (
            TaskFormPhase::Checkpoint,
            "Step 3 of 4 · Checkpoint",
            "第 3/4 步 · Checkpoint",
        ),
        (
            TaskFormPhase::Confirm,
            "Step 4 of 4 · Review",
            "第 4/4 步 · 检查",
        ),
    ] {
        form.phase = phase;
        let english = question_text(form, crate::config::Language::EnUs);
        let chinese = question_text(form, crate::config::Language::ZhCn);
        assert!(english.contains(english_heading), "{english}");
        assert!(chinese.contains(chinese_heading), "{chinese}");
        assert!(!english.contains("Task keys"), "{english}");
        assert!(!chinese.contains("Task 快捷键"), "{chinese}");
        let english_presentation = render::task_presentation(phase, crate::config::Language::EnUs);
        let chinese_presentation = render::task_presentation(phase, crate::config::Language::ZhCn);
        assert_eq!(english_presentation.title, Some("Persistent Task"));
        assert_eq!(chinese_presentation.title, Some("持久 Task"));
        assert!(english_presentation
            .instruction
            .is_some_and(|text| text.contains("Ctrl+C cancel")));
        assert!(chinese_presentation
            .instruction
            .is_some_and(|text| text.contains("Ctrl+C 取消")));
    }

    form.phase = TaskFormPhase::Confirm;
    assert!(
        render::task_presentation(TaskFormPhase::Confirm, crate::config::Language::EnUs)
            .instruction
            .is_some_and(|text| text.contains("Enter activate selected action"))
    );
    assert!(
        render::task_presentation(TaskFormPhase::Confirm, crate::config::Language::ZhCn)
            .instruction
            .is_some_and(|text| text.contains("Enter 执行所选操作"))
    );
}

#[test]
fn confirm_cancel_action_cancels_without_submitting() {
    let mut state = InlineState::default();
    let mut output = Vec::new();
    open_task_form_with_capabilities(
        &mut state,
        "run tests".to_owned(),
        TaskCapabilities::ready(),
        &mut output,
    )
    .unwrap();
    advance_form(&mut state, "run tests", &mut output).unwrap();
    advance_form(&mut state, "1", &mut output).unwrap();
    advance_form(&mut state, "1", &mut output).unwrap();
    state.task_form.active.as_mut().unwrap().selected = CONFIRM_CANCEL_INDEX;

    advance_form(&mut state, "2", &mut output).unwrap();

    assert!(state.task_form.active.is_none());
    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains("Task creation cancelled"), "{rendered}");
    assert!(
        !rendered.contains("Submitting persistent Task"),
        "{rendered}"
    );
}

#[test]
fn narrow_review_panel_keeps_both_actions_visible() {
    let mut state = InlineState::default();
    let mut output = Vec::new();
    open_task_form_with_capabilities(
        &mut state,
        "run the complete release validation".to_owned(),
        TaskCapabilities::ready(),
        &mut output,
    )
    .unwrap();
    let form = state.task_form.active.as_mut().unwrap();
    form.phase = TaskFormPhase::Confirm;
    let question = question_text(form, crate::config::Language::EnUs);
    let options = render::confirm_options(crate::config::Language::EnUs);
    let model = crate::runtime::prelude::QuestionPanelModel {
        id: "narrow-task-review",
        question: &question,
        options: &options,
        selected_option: 0,
        selected_options: &[],
        custom_answer: "",
        allow_free_text: false,
        selection_mode: crate::runtime::prelude::QuestionSelectionMode::Single,
        input_feedback: QuestionInputFeedback::Disabled,
    };
    let lines = crate::runtime::prelude::RatatuiInlineRenderer::with_width(40)
        .question_panel_lines_with_presentation(
            model,
            render::task_presentation(TaskFormPhase::Confirm, crate::config::Language::EnUs),
        );
    let rendered = lines.join("\n");

    assert!(lines.iter().any(|line| line.contains("Submit persistent")));
    assert!(lines.iter().any(|line| line.contains("Cancel")));
    assert!(lines.iter().all(|line| line.chars().count() <= 40));
    assert!(rendered.contains("Persistent Task"), "{rendered}");
    assert!(rendered.contains("Task keys"), "{rendered}");
    assert!(rendered.contains("activate selected"), "{rendered}");
    assert!(!rendered.contains("Agent question"), "{rendered}");
    assert!(!rendered.contains("Enter send"), "{rendered}");
}

#[test]
fn unavailable_form_notice_is_localized_safe_and_bounded() {
    for (language, title, summary, separator) in [
        (
            crate::config::Language::EnUs,
            "Task unavailable",
            "No Task Runtime is currently ready.",
            ": ",
        ),
        (
            crate::config::Language::ZhCn,
            "Task 暂不可用",
            "当前没有已就绪的 Task Runtime。",
            "：",
        ),
    ] {
        let mut capabilities = TaskCapabilities::ready();
        let unsafe_reason = format!("missing\u{1b}[2J\n{}", "任务".repeat(800));
        for capability in &mut capabilities.runtimes {
            capability.readiness = CapabilityReadiness::Unavailable(unsafe_reason.clone());
        }
        let mut state = InlineState {
            language,
            ..InlineState::default()
        };
        let mut output = Vec::new();

        assert!(!open_task_form_with_capabilities(
            &mut state,
            String::new(),
            capabilities,
            &mut output,
        )
        .unwrap());

        let rendered = String::from_utf8_lossy(&output);
        assert!(rendered.contains(title), "{rendered}");
        assert!(rendered.contains(summary), "{rendered}");
        assert!(rendered.contains(&format!("Core (cosh-core){separator}missing[2J")));
        assert!(!rendered.contains('\u{1b}'), "{rendered}");
        assert_eq!(rendered.matches('…').count(), 2, "{rendered}");
    }
}
