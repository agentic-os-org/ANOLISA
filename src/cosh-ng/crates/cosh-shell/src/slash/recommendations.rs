use crate::recommendation::personal_runtime::PersonalRuntime;
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecommendationReadiness {
    Disabled,
    ManualMode,
    AiDisabled,
    CurrentAiUnavailable,
    ReadyWithProfile,
    WaitingForHistory,
}

pub(super) struct RecommendationStatusView {
    pub(super) readiness: RecommendationReadiness,
    pub(super) bash_history: bool,
}

pub(super) fn render_recommendations_command<W: Write>(
    sub: Option<&str>,
    arg: Option<&str>,
    extra: Option<&str>,
    event: &ShellEvent,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    state.personalization.poll_ready();
    if arg.is_some() || extra.is_some() {
        return render_usage(state, output);
    }
    match sub.unwrap_or("status") {
        "status" => render_status(adapter, state, output),
        "privacy" => render_privacy(state, output),
        "on" => set_enabled(true, event, state, output),
        "off" => set_enabled(false, event, state, output),
        "clear" => clear(event, state, output),
        _ => render_usage(state, output),
    }
}

fn set_enabled<W: Write>(
    enabled: bool,
    event: &ShellEvent,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if !enabled {
        crate::agent::intercept::finalize_unresolved_personal_prompt_feedback(event, state);
        state.clear_personal_prompt_ghost();
    }
    let result = prepare_runtime(state, Some(enabled)).and_then(|_| {
        state
            .personalization
            .writer
            .as_mut()
            .ok_or_else(|| "recommendation storage is unavailable".to_string())
            .and_then(|runtime| {
                runtime
                    .set_user_enabled(enabled, now_hour_bucket(), COMMAND_TIMEOUT)
                    .map_err(|error| error.to_string())
            })
    });
    if result.is_ok() {
        if enabled {
            state.personalization.request_analyzer_retry();
        } else {
            cancel_personal_analysis(state);
        }
    }
    render_result(
        state,
        output,
        result.map(|_| {
            if enabled {
                vec![localized(
                    &state.i18n(),
                    "Prompt recommendations are on.",
                    "已开启个性化提示词推荐。",
                )
                .to_string()]
            } else {
                vec![
                    localized(
                        &state.i18n(),
                        "Prompt recommendations are off and local recommendation data was cleared.",
                        "已关闭提示词推荐，并清理本地推荐数据。",
                    )
                    .to_string(),
                    localized(
                        &state.i18n(),
                        "Command failure insights are controlled separately with /mode analysis manual.",
                        "失败命令 Insight 由 /mode analysis manual 单独控制。",
                    )
                    .to_string(),
                ]
            }
        }),
    )
}

fn clear<W: Write>(
    event: &ShellEvent,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    crate::agent::intercept::finalize_unresolved_personal_prompt_feedback(event, state);
    state.clear_personal_prompt_ghost();
    let result = prepare_runtime(state, Some(false)).and_then(|recovered| {
        state
            .personalization
            .writer
            .as_mut()
            .ok_or_else(|| "recommendation storage is unavailable".to_string())
            .and_then(|runtime| {
                runtime
                    .clear(now_hour_bucket(), COMMAND_TIMEOUT)
                    .map_err(|error| error.to_string())
            })?;
        Ok(recovered)
    });
    if result.is_ok() {
        cancel_personal_analysis(state);
    }
    render_result(
        state,
        output,
        result.map(|recovered| {
            if recovered {
                vec![localized(
                    &state.i18n(),
                    "Damaged recommendation data was reset. Recommendations are off; run /recommendations on to enable them.",
                    "已重置损坏的推荐数据。推荐当前关闭，可运行 /recommendations on 开启。",
                )
                .to_string()]
            } else {
                vec![localized(
                    &state.i18n(),
                    "Local recommendation data was cleared. Your on/off setting was kept.",
                    "已清理本地推荐数据，并保留当前开关设置。",
                )
                .to_string()]
            }
        }),
    )
}

fn prepare_runtime(
    state: &mut InlineState,
    recovery_preference: Option<bool>,
) -> Result<bool, String> {
    state.personalization.poll_ready();
    if state.personalization.writer.is_some() {
        return Ok(false);
    }
    if let Some(receiver) = state.personalization.writer_pending.take() {
        match receiver.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(writer) => {
                state.personalization.writer = Some(writer);
                return Ok(false);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                state.personalization.writer_pending = Some(receiver);
                return Err("recommendation storage is still starting; try again".to_string());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }
    let preference =
        recovery_preference.ok_or_else(|| "recommendation storage is unavailable".to_string())?;
    let root = state
        .personalization
        .store_root
        .clone()
        .ok_or_else(|| "recommendation storage path is unavailable".to_string())?;
    let runtime = PersonalRuntime::recover_with_preference(
        state.personalization.configured_enabled,
        state.personalization.environment_override,
        root,
        preference,
        now_hour_bucket(),
    )
    .map_err(|error| error.to_string())?;
    state.personalization.writer = Some(runtime.spawn_writer().map_err(|error| error.to_string())?);
    Ok(true)
}

fn cancel_personal_analysis(state: &mut InlineState) {
    if let Some(cancellation) = state.personalization.analyzer_cancellation.as_ref() {
        cancellation.cancel_current();
    }
    state.personalization.analyzer_started = false;
    state.clear_personal_prompt_ghost();
}

fn render_status<W: Write>(
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let readiness = recommendation_readiness(adapter, state);
    let view = RecommendationStatusView {
        readiness,
        bash_history: state.personalization.bash_history,
    };
    render_notice_panel(
        output,
        recommendations_title(&state.i18n()),
        render_status_lines(&state.i18n(), &view),
        None,
    )
}

fn recommendation_readiness(
    adapter: &AdapterInstance,
    state: &InlineState,
) -> RecommendationReadiness {
    let Some(runtime) = state.personalization.writer.as_ref() else {
        return RecommendationReadiness::Disabled;
    };
    let Some(status) = runtime.poll_status() else {
        return RecommendationReadiness::WaitingForHistory;
    };
    if !status.enabled {
        return RecommendationReadiness::Disabled;
    }
    if state.analysis_mode == AnalysisMode::Manual {
        return RecommendationReadiness::ManualMode;
    }
    if state.personalization.ai_disabled {
        return RecommendationReadiness::AiDisabled;
    }
    let AdapterInstance::CoshCore(core) = adapter else {
        return RecommendationReadiness::CurrentAiUnavailable;
    };
    if !matches!(runtime.current_ai_configured(core), Ok(true)) {
        return RecommendationReadiness::CurrentAiUnavailable;
    }
    if status.profile_generation > 0 || status.cached_candidates > 0 {
        RecommendationReadiness::ReadyWithProfile
    } else {
        RecommendationReadiness::WaitingForHistory
    }
}

pub(super) fn render_status_lines(i18n: &I18n, status: &RecommendationStatusView) -> Vec<String> {
    let state = match status.readiness {
        RecommendationReadiness::Disabled => localized(
            i18n,
            "Off. Run /recommendations on to enable it.",
            "已关闭。运行 /recommendations on 可重新开启。",
        ),
        RecommendationReadiness::ManualMode => localized(
            i18n,
            "On, but Manual mode does not generate personalized prompts.",
            "已开启，但 Manual 模式下不会生成个性化提示词。",
        ),
        RecommendationReadiness::AiDisabled => localized(
            i18n,
            "On, but AI is currently disabled; no background analysis is sent.",
            "已开启，但当前 AI 已关闭，不会发起后台分析。",
        ),
        RecommendationReadiness::CurrentAiUnavailable => localized(
            i18n,
            "On, but the current AI service or model is not ready.",
            "已开启，但当前 AI 服务或模型尚未就绪。",
        ),
        RecommendationReadiness::ReadyWithProfile => localized(
            i18n,
            "On. Recommendations use your recent Shell and Agent activity.",
            "已开启，将结合近期 Shell 与 Agent 使用情况提供建议。",
        ),
        RecommendationReadiness::WaitingForHistory => localized(
            i18n,
            "On. More recent activity is needed before personalized prompts can be generated.",
            "已开启，积累更多近期使用记录后会生成个性化提示词。",
        ),
    };
    let history = if status.bash_history {
        localized(i18n, "Bash history: included", "Bash history：已纳入")
    } else {
        localized(i18n, "Bash history: not included", "Bash history：未纳入")
    };
    vec![state.to_string(), history.to_string()]
}

fn render_privacy<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    render_notice_panel(
        output,
        recommendations_title(&state.i18n()),
        privacy_lines(&state.i18n()),
        None,
    )
}

pub(super) fn privacy_lines(i18n: &I18n) -> Vec<String> {
    if i18n.language() == Language::ZhCn {
        vec![
            "采集：脱敏后的 Shell 命令、Agent 请求与结果、推荐反馈及可选的 Bash history。"
                .to_string(),
            "保护：密码、Token 等凭证会脱敏；项目、服务、Pod 等推荐所需语义可能保留。".to_string(),
            "分析：有限的近期活动会发送给当前 AI 服务生成画像；后台不会操作 Shell。".to_string(),
            "本地保留：活动 7 天、近期画像 14 天、长期模式 90 天，并受容量限制。".to_string(),
            "/recommendations clear 仅清除本地数据；AI 服务侧保留遵循其政策。".to_string(),
        ]
    } else {
        vec![
            "Collected: sanitized Shell commands, Agent activity, feedback, and optional Bash history.".to_string(),
            "Protected: credentials are sanitized; useful project, service, and Pod context may remain.".to_string(),
            "Analysis: bounded recent activity may be sent to the current AI; it cannot operate the Shell.".to_string(),
            "Local retention: activity 7 days, recent profiles 14 days, long-term patterns 90 days, within capacity limits.".to_string(),
            "/recommendations clear removes local data only; AI-side retention follows its policy.".to_string(),
        ]
    }
}

fn render_usage<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    render_notice_panel(
        output,
        recommendations_title(&state.i18n()),
        vec!["/recommendations [on|off|status|privacy|clear]".to_string()],
        None,
    )
}

fn render_result<W: Write>(
    state: &InlineState,
    output: &mut W,
    result: Result<Vec<String>, impl std::fmt::Display>,
) -> std::io::Result<()> {
    let body = match result {
        Ok(lines) => lines,
        Err(error) => vec![localized_owned(
            &state.i18n(),
            format!("Recommendation operation failed: {error}"),
            format!("推荐操作失败：{error}"),
        )],
    };
    render_notice_panel(output, recommendations_title(&state.i18n()), body, None)
}

fn recommendations_title(i18n: &I18n) -> &'static str {
    localized(i18n, "Prompt recommendations", "提示词推荐")
}

fn localized<'a>(i18n: &I18n, en: &'a str, zh: &'a str) -> &'a str {
    if i18n.language() == Language::ZhCn {
        zh
    } else {
        en
    }
}

fn localized_owned(i18n: &I18n, en: String, zh: String) -> String {
    if i18n.language() == Language::ZhCn {
        zh
    } else {
        en
    }
}

fn now_hour_bucket() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 3600
}
