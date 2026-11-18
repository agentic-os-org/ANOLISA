use super::*;

mod health;

const STARTUP_HEALTH_ROW_WAIT: Duration = Duration::from_millis(100);
const STARTUP_AUTH_HINT_WAIT: Duration = Duration::from_millis(150);

/// A38: precompute startup suggestions before the relay loop starts so the
/// first prompt-boundary observe only renders cheaply under the input_mode
/// lock. The expensive reads (health/auth polls, personalization polls, git
/// context, eligible candidates) run here, before the input thread exists;
/// render_startup_banner consumes the cache. Side effects (claim, ghost input)
/// happen here, ahead of any concurrent input-thread mode write.
pub(crate) fn precompute_startup_suggestions(state: &mut InlineState, cwd: &str) {
    if state.startup_session.compact || state.rendered_startup_banner || !startup_banner_enabled() {
        return;
    }
    // Poll health non-blocking: wait_ready shortens the live scan and its child
    // deadline to that window, so a scan that would finish during the Bash
    // startup overlap is sealed early as a degraded report. The pending
    // deadline and the final closure stay with ShellReady.
    state.startup_health.poll_ready();
    // Poll auth non-blocking: a managed Bash login defers the credential probe
    // until the startup PATH observer fires after spawn, so waiting here would
    // burn the full hint window before the shell exists. An unresolved auth (or
    // not-ready health) is transient: render waits again and prepares against the
    // resolved ShellReady state (e.g. late unconfigured -> hidden), so do not
    // freeze it into the cache.
    state.startup_auth.poll_ready();
    if state.startup_health.report.is_none() || state.startup_auth.resolved.is_none() {
        return;
    }
    let computed = compute_startup_suggestions(state, cwd);
    state.startup_suggestions_cache = Some(computed);
}

pub(crate) fn render_startup_banner<W: Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    shell_label: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.startup_session.compact {
        return super::session::render_initial_question(events, adapter, state, output);
    }
    if state.rendered_startup_banner {
        return Ok(());
    }

    let Some(event) = events
        .iter()
        .find(|event| event.kind == ShellEventKind::ShellReady)
    else {
        return Ok(());
    };

    state.startup_health.wait_ready(STARTUP_HEALTH_ROW_WAIT);
    if !startup_banner_enabled() {
        state.startup_health.rendered = true;
        return Ok(());
    }
    state.rendered_startup_banner = true;
    let cwd = event.cwd.as_deref().unwrap_or("<unknown>");
    let i18n = state.i18n();
    let startup_hook = evaluate_startup_hooks(cwd, i18n);

    write!(output, "\r\x1b[2K")?;
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);

    state.startup_auth.wait_ready(STARTUP_AUTH_HINT_WAIT);
    let mut welcome = super::welcome::welcome_model(state, Some((shell_label, cwd)));
    let recommendation_notice = recommendation_notice_required(state);
    let suggestions = match state.startup_suggestions_cache.take() {
        Some(cached) if cached.cwd == cwd => commit_startup_suggestions(state, cached),
        _ => prepare_startup_suggestions(state, cwd),
    };
    if let Some(markdown) = startup_hook {
        for line in renderer.markdown_text_lines(&markdown) {
            welcome.extra.push(line);
        }
    }
    writeln!(output)?;
    renderer.write_welcome(output, welcome)?;
    writeln!(output)?;
    if recommendation_notice {
        write_recommendation_notice(&renderer, &i18n, output)?;
        writeln!(output)?;
    }
    if let Some(report) = state.startup_health.report.as_ref() {
        health::write_session_health_facts(report, &renderer, output)?;
        state.startup_health.rendered = true;
    }
    write_startup_suggestions(
        &renderer,
        &suggestions.candidates,
        suggestions.mode == StartupSuggestionMode::Interactive,
        output,
    )?;
    output.flush()?;
    record_visible_personal_impressions(state, cwd);
    if recommendation_notice {
        mark_recommendation_notice_shown(state);
    }
    restore_startup_prompt(state, output)?;
    output.flush()
}

pub(crate) fn render_pending_upgrade_notice_at_prompt_boundary<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    // precmd reports ShellReady only for command-less prompts; after a command
    // the same prompt boundary arrives as CommandCompleted or CommandFailed.
    if state.shell_at_prompt
        && events.iter().any(|event| {
            matches!(
                event.kind,
                ShellEventKind::ShellReady
                    | ShellEventKind::CommandCompleted
                    | ShellEventKind::CommandFailed
            )
        })
        && !state.personalization.shell_input_active
    {
        render_pending_upgrade_notice(state, output)?;
    }
    Ok(())
}

/// Renders a probe result that lands after the banner while the shell idles at
/// an empty prompt, so the user does not have to submit a line to see it.
pub(crate) fn render_pending_upgrade_notice_at_idle_prompt<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    // A draft, an open card, or a running agent owns the screen; wait for the
    // next idle tick or prompt boundary instead of drawing over it.
    if state.personalization.shell_input_active
        || state.agent_run.active.is_some()
        || crate::runtime::controller::pending_card_capture(state).is_some()
    {
        return Ok(());
    }
    render_pending_upgrade_notice(state, output)
}

pub(crate) fn render_pending_upgrade_notice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.startup_session.compact
        || !state.rendered_startup_banner
        || state.startup_upgrade.rendered
    {
        return Ok(());
    }
    state.startup_upgrade.poll_ready();
    let Some(notice) = state.startup_upgrade.notice().cloned() else {
        return Ok(());
    };
    let i18n = state.i18n();
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let body = vec![i18n.format(
        MessageId::StartupUpgradeHintLine,
        &[
            ("current", &notice.current),
            ("latest", &notice.latest),
            ("command", &notice.command),
        ],
    )];
    write!(output, "\r\x1b[2K")?;
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: i18n.t(MessageId::StartupUpgradeNoticeTitle),
            body,
            footer: None,
        },
    )?;
    writeln!(output)?;
    restore_startup_prompt(state, output)?;
    output.flush()?;
    state.startup_upgrade.rendered = true;
    Ok(())
}

pub(crate) fn render_pending_recommendation_notice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.startup_session.compact
        || state.personalization.notice_shown
        || !recommendation_notice_required(state)
    {
        return Ok(());
    }
    let i18n = state.i18n();
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    write!(output, "\r\x1b[2K")?;
    write_recommendation_notice(&renderer, &i18n, output)?;
    writeln!(output)?;
    restore_startup_prompt(state, output)?;
    output.flush()?;
    mark_recommendation_notice_shown(state);
    Ok(())
}

fn write_recommendation_notice<W: Write>(
    renderer: &RatatuiInlineRenderer,
    i18n: &I18n,
    output: &mut W,
) -> std::io::Result<()> {
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: recommendations_notice_title(i18n),
            body: recommendation_notice_lines(i18n),
            footer: None,
        },
    )
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
            "推荐已开启：当前 AI 参考近期 Shell 与 Agent 记录；尝试识别并脱敏常见凭证，不能保证覆盖所有敏感信息。".to_string(),
            "用 /recommendations off 关闭，/recommendations privacy 查看详情。"
                .to_string(),
        ]
    } else {
        vec![
            "Prompt recommendations are on; the current AI uses recent Shell and Agent activity."
                .to_string(),
            "We attempt to identify and redact common credentials, but cannot guarantee removal of all sensitive information. Use /recommendations off to disable or /recommendations privacy for details."
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
    if state.startup_session.compact
        || !state.rendered_startup_banner
        || state.startup_health.rendered
    {
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
    writeln!(output)?;
    health::write_session_health_facts(&report, &renderer, output)?;
    write_startup_suggestions(
        &renderer,
        &suggestions.candidates,
        suggestions.mode == StartupSuggestionMode::Interactive,
        output,
    )?;
    output.flush()?;
    record_visible_personal_impressions(state, &cwd);
    restore_startup_prompt(state, output)?;
    output.flush()
}

pub(crate) struct PreparedStartupSuggestions {
    pub(crate) mode: StartupSuggestionMode,
    pub(crate) candidates: Vec<PlannerCandidate>,
}

pub(crate) struct ComputedStartup {
    pub(crate) cwd: String,
    pub(crate) mode: StartupSuggestionMode,
    pub(crate) candidates: Vec<PlannerCandidate>,
    pub(crate) profile_generation: u64,
}

/// A38: expensive reads and candidate planning without committing the
/// cwd-sensitive side effects (health claim, ghost input, prompt bindings).
/// Those are committed at render against the actual ShellReady cwd, so a
/// precompute whose cwd differs from the shell's first real cwd cannot leak
/// wrong-directory candidates or suppress the fallback's health claim.
fn compute_startup_suggestions(state: &mut InlineState, cwd: &str) -> ComputedStartup {
    let Some(report) = state.startup_health.report.clone() else {
        state.personalization.startup_suppressed = true;
        return ComputedStartup {
            cwd: cwd.to_string(),
            mode: StartupSuggestionMode::Hidden,
            candidates: Vec::new(),
            profile_generation: 0,
        };
    };
    state.startup_auth.poll_ready();
    let mode = if state.startup_auth.ai_unconfigured() {
        StartupSuggestionMode::Hidden
    } else {
        startup_suggestion_mode(
            std::env::var("COSH_SHELL_ISOLATED").is_ok(),
            std::env::var("TERM").ok().as_deref(),
            &report,
        )
    };
    if mode == StartupSuggestionMode::Hidden {
        state.personalization.startup_suppressed = true;
        return ComputedStartup {
            cwd: cwd.to_string(),
            mode,
            candidates: Vec::new(),
            profile_generation: 0,
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
    let health =
        health::prepare_session(&report, state.language, !state.personalization.ai_disabled);
    let mut health_projection = report.clone();
    health_projection.try_items = health::eligible_candidates(&health, &report);
    let planned = plan_startup_for_render(
        state.i18n(),
        Some(&health_projection),
        &planner_context,
        &personal,
    );
    let mut visible_all = match mode {
        StartupSuggestionMode::ReadOnly => planned
            .visible_candidates
            .iter()
            .filter(|candidate| candidate.source == CandidateSource::Health)
            .cloned()
            .collect(),
        StartupSuggestionMode::Interactive => planned.visible_candidates.clone(),
        StartupSuggestionMode::Hidden => Vec::new(),
    };
    let session_questions = crate::ui::guidance::session_health_questions(&report, state.language);
    for candidate in &mut visible_all {
        if candidate.source == CandidateSource::Health {
            if let Some(question) = session_questions
                .iter()
                .find(|question| question.id == candidate.candidate_id)
            {
                candidate.prompt_text = question.question.clone();
            }
        }
    }
    ComputedStartup {
        cwd: cwd.to_string(),
        mode,
        candidates: visible_all,
        profile_generation,
    }
}

/// Commits the cwd-sensitive startup side effects (health claim, prompt
/// bindings, ghost input) for the actual ShellReady cwd. Kept separate from
/// compute so the precomputed cache is reused only when its cwd matches.
fn commit_startup_suggestions(
    state: &mut InlineState,
    computed: ComputedStartup,
) -> PreparedStartupSuggestions {
    let ComputedStartup {
        mode,
        candidates,
        profile_generation,
        ..
    } = computed;
    let mut visible_all = candidates;
    let report = state.startup_health.report.clone();
    let health = report
        .as_ref()
        .map(|report| {
            health::prepare_session(report, state.language, !state.personalization.ai_disabled)
        })
        .unwrap_or_else(health::PreparedHealth::empty);
    let displayed_health_ids = visible_all
        .iter()
        .filter(|candidate| candidate.source == CandidateSource::Health)
        .map(|candidate| candidate.candidate_id.clone())
        .collect::<Vec<_>>();
    let claimed = match report.as_ref() {
        Some(report) => health::claim_displayed_health(&health, report, &displayed_health_ids),
        None => Vec::new(),
    };
    if !displayed_health_ids.is_empty() && claimed.len() < displayed_health_ids.len() {
        tracing::warn!(
            requested = displayed_health_ids.len(),
            claimed = claimed.len(),
            "startup health suggestion claim dropped candidates"
        );
    }
    visible_all.retain(|candidate| {
        candidate.source != CandidateSource::Health || claimed.contains(&candidate.candidate_id)
    });
    if mode == StartupSuggestionMode::ReadOnly {
        return PreparedStartupSuggestions {
            mode,
            candidates: visible_all,
        };
    }
    let visible_personal = visible_all
        .iter()
        .filter(|candidate| candidate.source != CandidateSource::Health)
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
        state.suggestion_snapshot_generation = state.suggestion_snapshot_generation.wrapping_add(1);
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
            generation: state.suggestion_snapshot_generation,
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

pub(super) fn prepare_startup_suggestions(
    state: &mut InlineState,
    cwd: &str,
) -> PreparedStartupSuggestions {
    let computed = compute_startup_suggestions(state, cwd);
    commit_startup_suggestions(state, computed)
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

pub(super) fn write_startup_suggestions<W: Write>(
    renderer: &RatatuiInlineRenderer,
    suggestions: &[PlannerCandidate],
    interactive: bool,
    output: &mut W,
) -> std::io::Result<()> {
    if suggestions.is_empty() {
        return Ok(());
    }
    let items = suggestions
        .iter()
        .map(|candidate| crate::ui::guidance::SuggestionItem {
            id: candidate.candidate_id.clone(),
            question: candidate.prompt_text.clone(),
        })
        .collect::<Vec<_>>();
    renderer.write_suggestion_list(
        output,
        crate::ui::guidance::SuggestionListModel {
            items: &items,
            active: 0,
            interactive,
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
#[path = "recommendations_tests.rs"]
mod tests;
