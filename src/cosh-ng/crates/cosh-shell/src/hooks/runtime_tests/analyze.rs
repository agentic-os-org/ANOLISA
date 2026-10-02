use super::*;

#[test]
fn hook_hint_analyze_starts_agent_without_pending_queue() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let block = block_with_command("free -m");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "analyze",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("analyze hook hint");

    assert!(state.hooks.pending_consultation.is_none());
    assert!(state.hooks.pending_consultation_queue.is_empty());
    let request = &state.agent_run.active.as_ref().expect("active run").request;
    assert_eq!(
        request
            .hook_finding
            .as_ref()
            .map(|finding| finding.hook_id.as_str()),
        Some("memory-pressure")
    );
    assert!(request.recommended_skill.is_none());
    assert_eq!(request.mode, AgentMode::RecommendOnly);
    assert!(request.user_confirmed);
    assert!(state.hooks.display_events.iter().any(|event| {
        event.action == RuntimeHookDisplayAction::Analyzed
            && event.finding_id == "hook-cmd-1-memory-pressure"
    }));
    let prompt = request.user_input.as_deref().unwrap_or("");
    assert!(prompt.contains("confidence=medium"), "{prompt}");
    assert!(prompt.contains("policy_reason=allowed"), "{prompt}");
    assert!(
        prompt.contains("output_id=terminal-output://session/cmd-1"),
        "{prompt}"
    );
    assert!(!prompt.contains("output_ref=/tmp/out"), "{prompt}");
    assert!(
        prompt.contains("Do not execute follow-up commands automatically"),
        "{prompt}"
    );
    assert!(prompt.contains("command governance/approval"), "{prompt}");
}

#[test]
fn hook_hint_analyze_preserves_related_finding_context() {
    let aggregate = aggregate_hook_findings(vec![
        finding("memory-pressure", FindingSeverity::Critical),
        finding("high-memory-process", FindingSeverity::Warning),
    ])
    .into_iter()
    .find(|finding| finding.primary.hook_id == "memory-pressure")
    .expect("memory-pressure aggregate");
    let block = block_with_command("top -b -n1 | head -20");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "analyze",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("analyze composite hook hint");

    let request = &state.agent_run.active.as_ref().expect("active run").request;
    assert_eq!(
        request
            .hook_finding
            .as_ref()
            .map(|finding| finding.hook_id.as_str()),
        Some("memory-pressure")
    );
    assert!(
        request
            .hook_finding
            .as_ref()
            .map(|finding| finding.description.contains("Related findings:"))
            .unwrap_or(false),
        "{:?}",
        request.hook_finding
    );
    assert!(
        request
            .context_hints
            .iter()
            .any(|hint| hint.contains("related_hook_ids=high-memory-process")),
        "{:?}",
        request.context_hints
    );
    assert!(
        request
            .context_hints
            .iter()
            .any(|hint| hint.contains("related_findings=1")),
        "{:?}",
        request.context_hints
    );
}

#[test]
fn hook_hint_analyze_uses_related_history_facts() {
    let aggregate =
        aggregate_hook_findings(vec![finding("memory-pressure", FindingSeverity::Critical)])
            .remove(0);
    let mut setup = block_with_command_at("echo setup context", 1_000);
    setup.id = "setup".to_string();
    setup.cwd = "/repo".to_string();
    setup.end_cwd = "/repo".to_string();
    let mut previous_failed = block_with_command_at("grep --bad-option", 2_000);
    previous_failed.id = "previous-failed".to_string();
    previous_failed.status = CommandStatus::Failed;
    previous_failed.exit_code = 2;
    let mut block = block_with_command_at("free -m", 3_000);
    block.id = "target".to_string();
    block.cwd = "/repo".to_string();
    block.end_cwd = "/repo".to_string();
    let blocks = vec![setup.clone(), previous_failed.clone(), block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "analyze",
        "hook-target-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("analyze hook hint with related history");

    let request = &state.agent_run.active.as_ref().expect("active run").request;
    let ids = request
        .context_blocks
        .iter()
        .map(|block| block.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["setup", "previous-failed"]);
    assert!(request
        .context_blocks
        .iter()
        .all(|block| block.id != "target"));
}

#[test]
fn hook_hint_analyze_injects_target_excerpt_without_history_output_body() {
    let aggregate =
        aggregate_hook_findings(vec![finding("memory-pressure", FindingSeverity::Critical)])
            .remove(0);
    let dir = std::env::temp_dir().join(format!("cosh-shell-hook-evidence-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let setup_output = dir.join("setup.txt");
    let target_output = dir.join("target.txt");
    std::fs::write(&setup_output, "UNRELATED_HISTORY_OUTPUT_BODY\n").expect("write setup output");
    std::fs::write(
        &target_output,
        "MemTotal: 32768 MiB\nMemAvailable: 1400 MiB\nTARGET_HOOK_OUTPUT_BODY\n",
    )
    .expect("write target output");
    let mut setup = block_with_command_at("echo setup context", 1_000);
    setup.id = "setup".to_string();
    setup.cwd = "/repo".to_string();
    setup.end_cwd = "/repo".to_string();
    setup.output.terminal_output_ref = Some(setup_output.display().to_string());
    setup.output.terminal_output_bytes = 30;
    let mut block = block_with_command_at("free -m", 2_000);
    block.id = "target".to_string();
    block.cwd = "/repo".to_string();
    block.end_cwd = "/repo".to_string();
    block.output.terminal_output_ref = Some(target_output.display().to_string());
    block.output.terminal_output_bytes = 68;
    let blocks = vec![setup.clone(), block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "analyze",
        "hook-target-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("analyze hook hint with target excerpt");

    let request = &state.agent_run.active.as_ref().expect("active run").request;
    let prompt = prompt_from_request(request);
    assert!(prompt.contains("ShellEvidenceExcerpt"), "{prompt}");
    assert!(
        prompt.contains("output_id: terminal-output://session/target"),
        "{prompt}"
    );
    assert!(prompt.contains("TARGET_HOOK_OUTPUT_BODY"), "{prompt}");
    assert!(
        !prompt.contains("UNRELATED_HISTORY_OUTPUT_BODY"),
        "{prompt}"
    );
    assert!(!prompt.contains(setup_output.to_str().unwrap()), "{prompt}");
    assert!(
        !prompt.contains(target_output.to_str().unwrap()),
        "{prompt}"
    );
}

#[test]
fn hook_hint_analyze_maps_related_hook_ids_to_command_ids() {
    let pressure_aggregate =
        aggregate_hook_findings(vec![finding("memory-pressure", FindingSeverity::Warning)])
            .remove(0);
    let process_aggregate =
        aggregate_hook_findings(vec![process_finding("java (PID 1234) uses 31.2% MEM")]).remove(0);
    let mut pressure_block = block_with_command_at("free -m", 1_000);
    pressure_block.id = "pressure".to_string();
    let mut process_block = block_with_command_at("ps aux --sort=-%mem | head", 2_000);
    process_block.id = "process".to_string();
    let blocks = vec![pressure_block.clone(), process_block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&pressure_block, pressure_aggregate, &mut state);
    record_aggregated_hook_finding(&process_block, process_aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "analyze",
        "hook-process-high-memory-process",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("analyze hook hint with related hook ids");

    let request = &state.agent_run.active.as_ref().expect("active run").request;
    assert!(
        request
            .context_hints
            .iter()
            .any(|hint| hint.contains("related_hook_ids=memory-pressure")),
        "{:?}",
        request.context_hints
    );
    let ids = request
        .context_blocks
        .iter()
        .map(|block| block.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["pressure"]);
}

#[test]
fn hook_hint_analyze_marks_missing_output_ref() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregate = aggregate_hook_findings(findings).remove(0);
    let mut block = block_with_command("free -m");
    block.output.terminal_output_ref = None;
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "analyze",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("analyze hook hint with missing output ref");

    let request = &state.agent_run.active.as_ref().expect("active run").request;
    let prompt = request.user_input.as_deref().unwrap_or("");
    assert!(prompt.contains("output_id=<missing>"), "{prompt}");
    assert!(!prompt.contains("output_ref=<missing>"), "{prompt}");
    assert!(
        request
            .context_hints
            .iter()
            .any(|hint| hint.contains("output_id=<missing>")),
        "{:?}",
        request.context_hints
    );
}

#[test]
fn low_confidence_hook_analyze_prompts_readonly_verification() {
    let mut finding = finding("memory-pressure", FindingSeverity::Critical);
    finding
        .description
        .push_str(" Confidence is lower because output lacks avail Mem.");
    let aggregate = aggregate_hook_findings(vec![finding]).remove(0);
    let block = block_with_command("top -b -n1 | head -20");
    let blocks = vec![block.clone()];
    let mut state = InlineState::default();
    record_aggregated_hook_finding(&block, aggregate, &mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();

    handle_command_hook_hint_action(
        "analyze",
        "hook-cmd-1-memory-pressure",
        &blocks,
        &adapter,
        &mut state,
        &mut output,
    )
    .expect("analyze low confidence hook hint");

    let request = &state.agent_run.active.as_ref().expect("active run").request;
    let prompt = request.user_input.as_deref().unwrap_or("");
    assert!(prompt.contains("low confidence"), "{prompt}");
    assert!(prompt.contains("read-only commands"), "{prompt}");
    assert!(
        prompt.contains("before giving a root-cause conclusion"),
        "{prompt}"
    );
}
