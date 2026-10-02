use super::*;

pub(crate) fn render_startup_banner<W: Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    shell_label: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.rendered_startup_banner || !startup_banner_enabled() {
        return Ok(());
    }

    let Some(event) = events
        .iter()
        .find(|event| event.kind == ShellEventKind::ShellReady)
    else {
        return Ok(());
    };

    state.rendered_startup_banner = true;
    let cwd = event.cwd.as_deref().unwrap_or("<unknown>");
    let i18n = state.i18n();
    let startup_hook = evaluate_startup_hooks(cwd, i18n);

    write!(output, "\r\x1b[2K")?;
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);

    let term_width = ratatui::crossterm::terminal::size()
        .map(|(cols, _)| cols)
        .unwrap_or(80);

    if term_width >= LOGO_MIN_WIDTH {
        writeln!(output)?;
        for (i, line) in LOGO_LINES.iter().enumerate() {
            writeln!(output, "{}{}{}", LOGO_COLORS[i], line, RESET)?;
        }
        writeln!(output)?;
    }

    let mut body = vec![
        i18n.format(
            MessageId::StartupAdapterLine,
            &[
                ("adapter", adapter.name()),
                ("shell", shell_label),
                ("approval", state.approval_mode.label()),
                ("analysis", state.analysis_mode.label()),
            ],
        ),
        i18n.format(MessageId::StartupCwdLine, &[("cwd", cwd)]),
        i18n.t(MessageId::StartupCommandsLine).to_string(),
    ];
    state.startup_auth.wait_ready(STARTUP_AUTH_HINT_WAIT);
    append_startup_auth_hint(state, &mut body);
    let recommendation_notice = append_recommendation_notice(state, &mut body);
    state.startup_health.wait_ready(STARTUP_HEALTH_ROW_WAIT);
    let suggestions = prepare_startup_suggestions(state, cwd);
    if let Some(report) = state.startup_health.report.as_ref() {
        if health_uses_startup_row(report) {
            body.push(renderer.startup_section_separator_line());
            body.extend(renderer.health_startup_row_lines(HealthBannerModel::new(report)));
        }
    }
    if let Some(markdown) = startup_hook.markdown {
        body.push(String::new());
        body.push(startup_hook.summary);
        for line in renderer.markdown_text_lines(&markdown) {
            body.push(line);
        }
    }
    body.push(i18n.t(MessageId::StartupSwitchHint).to_string());
    renderer.write_banner(output, i18n.t(MessageId::StartupTitle), body, None)?;
    writeln!(output)?;
    if let Some(report) = state.startup_health.report.as_ref() {
        if !health_uses_startup_row(report) {
            let mut facts = report.clone();
            facts.try_items.clear();
            renderer.write_health_banner(output, HealthBannerModel::new(&facts))?;
            writeln!(output)?;
        }
        state.startup_health.rendered = true;
    }
    write_startup_suggestion_card(
        state,
        &renderer,
        suggestions.mode,
        &suggestions.candidates,
        output,
    )?;
    output.flush()?;
    if let Some(report) = state.startup_health.report.as_ref() {
        record_visible_startup_health_recommendations(report, &suggestions.candidates);
    }
    record_visible_personal_impressions(state, cwd);
    if recommendation_notice {
        mark_recommendation_notice_shown(state);
    }
    restore_startup_prompt(state, output)?;
    output.flush()
}

pub(crate) fn render_pending_recommendation_notice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.personalization.notice_shown || !recommendation_notice_required(state) {
        return Ok(());
    }
    let i18n = state.i18n();
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    write!(output, "\r\x1b[2K")?;
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: recommendations_notice_title(&i18n),
            body: recommendation_notice_lines(&i18n),
            footer: None,
        },
    )?;
    writeln!(output)?;
    restore_startup_prompt(state, output)?;
    output.flush()?;
    mark_recommendation_notice_shown(state);
    Ok(())
}

pub(super) fn append_startup_auth_hint(state: &mut InlineState, body: &mut Vec<String>) {
    if state.personalization.ai_disabled || !state.startup_auth.ai_unconfigured() {
        return;
    }
    body.push(String::new());
    body.push(state.i18n().t(MessageId::StartupAuthHintLine).to_string());
}

fn append_recommendation_notice(state: &mut InlineState, body: &mut Vec<String>) -> bool {
    if !recommendation_notice_required(state) {
        return false;
    }
    body.push(String::new());
    body.extend(recommendation_notice_lines(&state.i18n()));
    true
}

fn recommendation_notice_required(state: &mut InlineState) -> bool {
    if state.personalization.notice_shown
        || state.analysis_mode == crate::runtime::state::AnalysisMode::Manual
        || state.personalization.ai_disabled
    {
        return false;
    }
    // While no auth is configured the recommendation pipeline cannot run;
    // stay quiet without consuming the first-time disclosure.
    state.startup_auth.poll_ready();
    if state.startup_auth.ai_unconfigured() {
        return false;
    }
    state.personalization.poll_ready();
    let Some(writer) = state.personalization.writer.as_ref() else {
        return false;
    };
    writer.poll_status().is_some_and(|status| status.enabled)
        && writer
            .poll_snapshot()
            .is_some_and(|snapshot| snapshot.preferences.notice_version_seen < DISCLOSURE_VERSION)
}

fn mark_recommendation_notice_shown(state: &mut InlineState) {
    state.personalization.notice_shown = true;
    if let Some(writer) = state.personalization.writer.as_mut() {
        let _ = writer.mark_notice_seen(
            DISCLOSURE_VERSION,
            now_hour_bucket(),
            Duration::from_millis(100),
        );
    }
}

fn recommendation_notice_lines(i18n: &I18n) -> Vec<String> {
    if i18n.language() == Language::ZhCn {
        vec![
            "提示词推荐已开启，将由当前 AI 参考近期 Shell 与 Agent 使用记录生成建议。".to_string(),
            "凭证会脱敏；用 /recommendations off 关闭，/recommendations privacy 查看详情。"
                .to_string(),
        ]
    } else {
        vec![
            "Prompt recommendations are on; the current AI uses recent Shell and Agent activity."
                .to_string(),
            "Credentials are sanitized. Use /recommendations off to disable or /recommendations privacy for details."
                .to_string(),
        ]
    }
}

fn recommendations_notice_title(i18n: &I18n) -> &'static str {
    if i18n.language() == Language::ZhCn {
        "提示词推荐"
    } else {
        "Prompt recommendations"
    }
}

pub(crate) fn render_startup_health_banner<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if !state.rendered_startup_banner || state.startup_health.rendered {
        return Ok(());
    }

    state.startup_health.poll_ready();
    let Some(report) = state.startup_health.report.clone() else {
        return Ok(());
    };

    state.startup_health.rendered = true;
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(str::to_string))
        .unwrap_or_else(|| ".".to_string());
    let suggestions = prepare_startup_suggestions(state, &cwd);
    write!(output, "\r\x1b[2K")?;
    // Late all-green reports still surface the compact startup row here;
    // `write_health_banner` renders the row shape for `ok` reports and the
    // attention panel otherwise, so both stay visible after the banner.
    let mut facts = report.clone();
    facts.try_items.clear();
    renderer.write_health_banner(output, HealthBannerModel::new(&facts))?;
    writeln!(output)?;
    write_startup_suggestion_card(
        state,
        &renderer,
        suggestions.mode,
        &suggestions.candidates,
        output,
    )?;
    output.flush()?;
    record_visible_startup_health_recommendations(&report, &suggestions.candidates);
    record_visible_personal_impressions(state, &cwd);
    restore_startup_prompt(state, output)?;
    output.flush()
}

struct PreparedStartupSuggestions {
    mode: StartupSuggestionMode,
    candidates: Vec<PlannerCandidate>,
}

fn prepare_startup_suggestions(state: &mut InlineState, cwd: &str) -> PreparedStartupSuggestions {
    let Some(report) = state.startup_health.report.clone() else {
        state.personalization.startup_suppressed = true;
        return PreparedStartupSuggestions {
            mode: StartupSuggestionMode::Hidden,
            candidates: Vec::new(),
        };
    };
    let mode = startup_suggestion_mode(
        std::env::var("COSH_SHELL_ISOLATED").is_ok(),
        std::env::var("TERM").ok().as_deref(),
        &report,
    );
    if mode == StartupSuggestionMode::Hidden {
        state.personalization.startup_suppressed = true;
        return PreparedStartupSuggestions {
            mode,
            candidates: Vec::new(),
        };
    }
    state.personalization.poll_ready();
    let personal_enabled = mode == StartupSuggestionMode::Interactive
        && state.analysis_mode != crate::runtime::state::AnalysisMode::Manual
        && !state.personalization.ai_disabled;
    let now = now_hour_bucket();
    let (personal, context, profile_generation) = if personal_enabled {
        match state.personalization.writer.as_mut() {
            Some(writer) => (
                writer.poll_planner_candidates(now).unwrap_or_default(),
                current_personal_context(writer, cwd),
                writer
                    .poll_snapshot()
                    .map(|snapshot| snapshot.cache.profile_generation)
                    .unwrap_or_default(),
            ),
            None => (Vec::new(), None, 0),
        }
    } else {
        state.personalization.startup_suppressed = true;
        (Vec::new(), None, 0)
    };
    let planner_context = PlannerContext {
        now_hour_bucket: now,
        repo_id: context.as_ref().and_then(|value| value.repo_id.clone()),
        host_id: context.as_ref().and_then(|value| value.host_id.clone()),
    };
    let planned = plan_startup_for_render(state.i18n(), Some(&report), &planner_context, &personal);
    let visible_all = match mode {
        StartupSuggestionMode::ReadOnly => planned
            .visible_candidates
            .iter()
            .filter(|candidate| candidate.source == CandidateSource::Health)
            .cloned()
            .collect(),
        StartupSuggestionMode::Interactive => planned.visible_candidates.clone(),
        StartupSuggestionMode::Hidden => Vec::new(),
    };
    if mode == StartupSuggestionMode::ReadOnly {
        return PreparedStartupSuggestions {
            mode,
            candidates: visible_all,
        };
    }
    let visible_personal = visible_personal_candidates(&planned)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    state.pending_prompt_suggestion_bindings.clear();
    for candidate in visible_all
        .iter()
        .filter(|candidate| candidate.source == CandidateSource::Health)
    {
        state.pending_prompt_suggestion_bindings.insert(
            candidate.candidate_id.clone(),
            PendingInputGhostBinding::Health(AgentContextBinding::StartupHealthFollowUp),
        );
    }
    for candidate in visible_personal {
        let intent_lifecycle_id = random_hex(16).unwrap_or_else(|_| candidate.candidate_id.clone());
        let binding = FrozenPromptBinding {
            candidate_id: candidate.candidate_id.clone(),
            task_ref: candidate.task_ref.clone(),
            original_prompt: candidate.prompt_text.clone(),
            source: candidate.source,
            suppression_key: candidate.suppression_key.clone(),
            profile_generation,
            intent_lifecycle_id,
        };
        state.pending_prompt_suggestion_bindings.insert(
            candidate.candidate_id.clone(),
            PendingInputGhostBinding::Personal(binding),
        );
    }
    if !visible_all.is_empty() {
        let candidates = visible_all
            .iter()
            .map(|candidate| PromptGhostCandidate {
                text: candidate.prompt_text.clone(),
                suggestion_id: candidate.candidate_id.clone(),
            })
            .collect::<Vec<_>>();
        let first_text = candidates[0].text.clone();
        let first_id = candidates[0].suggestion_id.clone();
        state.pending_input_ghost = Some(first_text);
        state.pending_input_ghost_route = PromptGhostRoute::AgentSelection {
            candidates,
            active: 0,
        };
        state.pending_input_ghost_binding = state
            .pending_prompt_suggestion_bindings
            .get(&first_id)
            .cloned();
    }
    PreparedStartupSuggestions {
        mode,
        candidates: visible_all,
    }
}

pub(crate) fn record_visible_personal_impressions(state: &mut InlineState, cwd: &str) {
    let bindings = state
        .pending_prompt_suggestion_bindings
        .values()
        .filter_map(|binding| match binding {
            PendingInputGhostBinding::Personal(binding) => Some(binding.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if bindings.is_empty() {
        return;
    }
    let now = now_hour_bucket();
    let Some(writer) = state.personalization.writer.as_mut() else {
        return;
    };
    let Some(context) = current_personal_context(writer, cwd) else {
        return;
    };
    for binding in bindings {
        let identity = format!(
            "{}\0{}\0impression",
            binding.candidate_id, binding.intent_lifecycle_id
        );
        let feedback = FeedbackEvent {
            candidate_id: binding.candidate_id.clone(),
            candidate_source: binding.source,
            task_ref: binding.task_ref,
            profile_generation: binding.profile_generation,
            intent_lifecycle_id: binding.intent_lifecycle_id,
            action: FeedbackAction::Impression,
            edit_bucket: None,
        };
        if let Ok(Some(record)) =
            writer.feedback_record(feedback, now, context.clone(), identity.as_bytes())
        {
            let _ = writer.try_enqueue_identified_deferred(record);
        }
    }
}

fn current_personal_context(
    writer: &crate::recommendation::personal_runtime::PersonalRuntimeWriter,
    cwd: &str,
) -> Option<crate::recommendation::personal_model::ActivityContext> {
    let cwd = Path::new(cwd);
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_default();
    let repo = discover_repo_context(cwd);
    writer.build_context(
        &host,
        cwd,
        repo.as_ref().map(|value| value.root.as_path()),
        repo.as_ref()
            .and_then(|value| value.normalized_identity.as_deref()),
        &home,
    )
}

pub(super) fn write_startup_suggestion_card<W: Write>(
    state: &InlineState,
    renderer: &RatatuiInlineRenderer,
    mode: StartupSuggestionMode,
    suggestions: &[PlannerCandidate],
    output: &mut W,
) -> std::io::Result<()> {
    if suggestions.is_empty() {
        return Ok(());
    }
    let body = suggestions
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let source = match (state.language, candidate.source) {
                (Language::ZhCn, CandidateSource::Health) => "异常排查",
                (Language::ZhCn, _) => "个性化",
                (_, CandidateSource::Health) => "Health",
                (_, _) => "Personal",
            };
            format!("{}. [{source}] {}", index + 1, candidate.prompt_text)
        })
        .collect();
    let footer = match (mode, state.language, suggestions.len() > 1) {
        (StartupSuggestionMode::Interactive, Language::ZhCn, true) => {
            Some("Shift+Tab 切换 · Tab 填入 · Enter 直接提问")
        }
        (StartupSuggestionMode::Interactive, Language::ZhCn, false) => {
            Some("Tab 填入 · Enter 直接提问")
        }
        (StartupSuggestionMode::Interactive, _, true) => {
            Some("Shift+Tab cycle · Tab insert · Enter ask")
        }
        (StartupSuggestionMode::Interactive, _, false) => Some("Tab insert · Enter ask"),
        _ => None,
    };
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: if state.language == Language::ZhCn {
                "可以试试"
            } else {
                "Suggested prompts"
            },
            body,
            footer,
        },
    )?;
    writeln!(output)
}

pub(crate) fn plan_startup_for_render(
    i18n: crate::I18n,
    report: Option<&HealthScanReport>,
    context: &PlannerContext,
    personal: &[PlannerCandidate],
) -> crate::recommendation::personal_planner::RenderedStartupSuggestions {
    let Some(report) = report else {
        return plan_startup(context, HealthResolution::TimedOut, personal);
    };
    let health = crate::diagnostics::health::startup_prompt_suggestions(report, i18n, 3)
        .into_iter()
        .map(|(id, prompt_text)| health_planner_candidate(id, prompt_text))
        .collect::<Vec<_>>();
    plan_startup(context, HealthResolution::Resolved(&health), personal)
}

pub(crate) fn visible_personal_candidates(
    rendered: &crate::recommendation::personal_planner::RenderedStartupSuggestions,
) -> Vec<&PlannerCandidate> {
    rendered
        .visible_candidates
        .iter()
        .filter(|candidate| candidate.source != CandidateSource::Health)
        .collect()
}

fn record_visible_startup_health_recommendations(
    report: &HealthScanReport,
    suggestions: &[PlannerCandidate],
) {
    let visible_ids = suggestions
        .iter()
        .filter(|candidate| candidate.source == CandidateSource::Health)
        .map(|candidate| candidate.candidate_id.clone())
        .collect::<HashSet<_>>();
    if visible_ids.is_empty() {
        return;
    }
    let mut visible_report = report.clone();
    visible_report
        .try_items
        .retain(|item| visible_ids.contains(&item.id));
    record_startup_health_recommendations(&visible_report);
}

fn health_planner_candidate(candidate_id: String, prompt_text: String) -> PlannerCandidate {
    PlannerCandidate {
        candidate_id: candidate_id.clone(),
        source: CandidateSource::Health,
        task_ref: candidate_id.clone(),
        prompt_text,
        context_affinity: ContextAffinity {
            scope_kind: ScopeKind::HostFallback,
            repo_id: None,
            host_id: None,
        },
        last_seen_hour_bucket: now_hour_bucket(),
        evidence: CandidateEvidenceSummary::default(),
        entities: Vec::new(),
        suppression_key: candidate_id,
        last_action_failed: false,
        consecutive_explicit_dismissals: 0,
        suppressed: false,
    }
}

fn now_hour_bucket() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 3600)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_disclosure_is_two_short_lines_with_controls() {
        let lines = recommendation_notice_lines(&I18n::new(Language::ZhCn));

        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.chars().count() <= 70));
        let text = lines.join("\n");
        assert!(text.contains("/recommendations off"));
        assert!(text.contains("/recommendations privacy"));
    }
}
