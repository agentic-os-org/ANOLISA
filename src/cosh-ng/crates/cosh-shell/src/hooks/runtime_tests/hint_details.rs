use super::*;

#[test]
fn hook_hint_ignore_records_session_suppression_without_pending_queue() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "ignore",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("ignore hook hint");

    assert!(state
        .hooks
        .ignored_cards
        .contains("memory:system-memory:memory-pressure:free:user_interactive"));
    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.pending_consultation_queue.is_empty());
    assert!(state.hooks.display_events.iter().any(|event| {
        event.action == RuntimeHookDisplayAction::Ignored
            && event.finding_id == "hook-cmd-1-memory-pressure"
    }));
    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("Hook hint ignored"), "{rendered}");
}

#[test]
fn hook_hint_ignore_uses_zh_notice() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "ignore",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("ignore hook hint");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("Hook 提示已忽略"), "{rendered}");
    assert!(
        rendered.contains("本会话已忽略 Hook 提示 'hook-cmd-1-memory-pressure'。"),
        "{rendered}"
    );
}

#[test]
fn hook_hint_details_renders_without_pending_queue() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "details",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("show hook hint details");

    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.pending_consultation_queue.is_empty());
    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("Hook finding details"), "{rendered}");
    assert!(
        rendered.contains("Confidence: medium; policy reason: allowed"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "User-interest reason: diagnostic-intent: explicit diagnostic command with sufficient evidence"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("Topic: memory; entity: system-memory"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Command origin: user_interactive"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "Suppression key: memory:system-memory:memory-pressure:free:user_interactive"
        ),
        "{rendered}"
    );
    assert!(rendered.contains("Output capture: captured"), "{rendered}");
    assert!(!rendered.contains("/tmp/out"), "{rendered}");
    assert!(!rendered.contains("Output ref:"), "{rendered}");
    assert!(
        rendered.contains("Recommended skill: memory-analysis"),
        "{rendered}"
    );
    assert!(rendered.contains("Prompt hint:"), "{rendered}");
}

#[test]
fn hook_hint_details_show_output_ref_in_debug() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState {
        debug: true,
        ..InlineState::default()
    };
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "details",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("show hook hint details");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("Hook finding details"), "{rendered}");
    assert!(rendered.contains("Output capture: captured"), "{rendered}");
    assert!(
        rendered.contains("debug_output_ref: /tmp/out"),
        "{rendered}"
    );
}

#[test]
fn hook_hint_details_uses_zh_labels() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "details",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("show hook hint details");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("Hook 发现详情"), "{rendered}");
    assert!(
        rendered.contains("置信度: medium; 策略原因: allowed"),
        "{rendered}"
    );
    assert!(rendered.contains("输出捕获: captured"), "{rendered}");
    assert!(!rendered.contains("/tmp/out"), "{rendered}");
    assert!(
        rendered.contains("用户关注原因: diagnostic-intent:"),
        "{rendered}"
    );
    assert!(
        rendered.contains("明确的诊断命令且证据充分。"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("explicit diagnostic command with sufficient evidence"),
        "{rendered}"
    );
    assert!(
        rendered.contains("主题: memory; 实体: system-memory"),
        "{rendered}"
    );
    assert!(
        rendered.contains("推荐 skill: memory-analysis"),
        "{rendered}"
    );
    assert!(rendered.contains("分析仍需要确认。"), "{rendered}");
}

#[test]
fn recorded_hook_finding_uses_zh_notice_labels() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    state.hooks.findings[0].display = RuntimeHookDisplay::Hint;
    let mut output = Vec::new();

    render_recorded_hook_findings(&blocks, &mut state, &mut output).expect("render hook finding");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("Hook 发现"), "{rendered}");
    assert!(rendered.contains("命令 Hook 发现"), "{rendered}");
    assert!(!rendered.contains("输出 ID"), "{rendered}");
    assert!(!rendered.contains("terminal-output://"), "{rendered}");
    assert!(
        rendered.contains("Agent 后续分析必须先使用 cosh-shell 的有界证据"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("hook-cmd-1-memory-pressure"),
        "{rendered}"
    );
    assert!(!rendered.contains("details"), "{rendered}");
    assert!(!rendered.contains("Command hook finding"), "{rendered}");
    assert!(!rendered.contains("Output ref:"), "{rendered}");
    assert!(!rendered.contains("/tmp/out"), "{rendered}");
    assert!(
        !rendered.contains("Agent follow-up must inspect"),
        "{rendered}"
    );
}

#[test]
fn hook_hint_details_explains_lookup_intent() {
    let findings = vec![finding("high-memory-process", FindingSeverity::Warning)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("ps -p 1234 -o pid,%mem,rss,comm,args");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "details",
        "hook-cmd-1-high-memory-process",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("show hook hint details");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(
        rendered.contains("Confidence: low; policy reason: allowed"),
        "{rendered}"
    );
    assert!(
        rendered.contains("User-interest reason: lookup-intent:"),
        "{rendered}"
    );
}

#[test]
fn hook_hint_details_explains_wrapper_low_confidence() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("sudo -n free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "details",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("show hook hint details");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(
        rendered.contains("User-interest reason: wrapper-low-confidence:"),
        "{rendered}"
    );
}

#[test]
fn hook_hint_details_explains_user_continued_input() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("top -b -n1 | head -20");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    state.hooks.findings[0].display_reason = "user-continued-input".to_string();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "details",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("show hook hint details");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(
        rendered.contains("User-interest reason: user-continued-input:"),
        "{rendered}"
    );
}

#[test]
fn hook_hint_details_explains_active_run_deferred() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    state.hooks.findings[0].display_reason = "active-agent-run-deferred".to_string();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "details",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("show hook hint details");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(
        rendered.contains("User-interest reason: active-run-deferred:"),
        "{rendered}"
    );
}
