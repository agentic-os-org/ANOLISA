use serde_json::Value;

use crate::hooks::state::{
    HookActionKind, HookFeedback, RuntimeHookDisplayEvent, RuntimeHookFinding,
};
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

pub(super) mod action;

pub(crate) use action::{
    has_pending_hook_action, pending_hook_action_capture, render_hook_action_card_actions,
};

pub(crate) fn render_hooks_command<W: Write>(
    sub: Option<&str>,
    arg: Option<&str>,
    extra: Option<&str>,
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    match (sub, arg, extra) {
        (None, _, _) => {
            let i18n = state.i18n();
            let hooks = state.hooks.engine.registered_hook_infos();
            let shell_lines = hooks_status_body(state, &hooks, &i18n);
            let shell_entries = shell_hook_entries(state, &hooks);

            // Agent Hooks section (only available with CoshCore backend)
            let (agent_view, agent_hook_count) =
                if let AdapterInstance::CoshCore(cosh_core) = adapter {
                    match cosh_core.registry_query("hooks", "list", Value::Null) {
                        Ok(data) => {
                            let count = data.as_array().map(|a| a.len()).unwrap_or(0);
                            let groups = group_agent_hooks(&data);
                            let view = if groups.is_empty() {
                                AgentHooksView::Message("(none)".to_string())
                            } else {
                                AgentHooksView::Groups(groups)
                            };
                            (view, count)
                        }
                        Err(e) => (AgentHooksView::Message(format!("Error: {e}")), 0),
                    }
                } else {
                    (
                        AgentHooksView::Message(
                            i18n.t(MessageId::SlashHooksAgentUnavailable).to_string(),
                        ),
                        0,
                    )
                };

            let total_hook_count = hooks.len() + agent_hook_count;
            let omitted_template = if i18n.language() == crate::config::Language::ZhCn {
                "… 另有 {count} 个 hook 未显示（完整列表见 plain 输出）".to_string()
            } else {
                "… {count} more hook(s) not shown (full list in plain output)".to_string()
            };
            RatatuiInlineRenderer::for_terminal().write_hook_status_panel(
                output,
                HookStatusPanelModel {
                    title: i18n.t(MessageId::SlashHooksRegisteredTitle),
                    shell_label: i18n.t(MessageId::SlashHooksShellSection),
                    shell_lines,
                    shell_entries,
                    agent_label: i18n.t(MessageId::SlashHooksAgentSection),
                    agent: agent_view,
                    omitted_template,
                    footer: hooks_footer(state, total_hook_count, &i18n),
                },
            )
        }
        (Some("history"), None, None) => render_hooks_history(state, output),
        (Some("events"), None, None) => render_hooks_events(state, output),
        (Some("analyze"), Some(hint_id), None)
        | (Some("ignore"), Some(hint_id), None)
        | (Some("details"), Some(hint_id), None) => {
            crate::runtime::hooks::handle_command_hook_hint_action(
                sub.unwrap(),
                hint_id,
                blocks,
                adapter,
                state,
                output,
            )
        }
        (Some("feedback"), Some(action), Some(finding_id)) => {
            render_hooks_feedback(action, finding_id, state, output)
        }
        (Some("clear-feedback"), None, None) => render_clear_hook_feedback(state, output),
        (Some("mute"), Some(target), None) => {
            state.hooks.muted_targets.insert(target.to_string());
            let i18n = state.i18n();
            render_notice_panel(
                output,
                i18n.t(MessageId::SlashHooksTargetMutedTitle),
                vec![i18n.format(MessageId::SlashHooksTargetMutedBody, &[("target", target)])],
                Some(i18n.t(MessageId::SlashHooksTargetMutedFooter)),
            )
        }
        (Some("unmute"), Some(target), None) => {
            let removed = state.hooks.muted_targets.remove(target);
            let i18n = state.i18n();
            let body = if removed {
                vec![i18n.format(
                    MessageId::SlashHooksTargetUnmutedBody,
                    &[("target", target)],
                )]
            } else {
                vec![i18n.format(
                    MessageId::SlashHooksTargetNotMutedBody,
                    &[("target", target)],
                )]
            };
            render_notice_panel(
                output,
                i18n.t(MessageId::SlashHooksTargetUnmutedTitle),
                body,
                None,
            )
        }
        (Some("trust-project"), None, None) => render_project_hook_trust(true, state, output),
        (Some("untrust-project"), None, None) => render_project_hook_trust(false, state, output),
        (Some("clear-project-trust"), None, None) => render_clear_project_hook_trust(state, output),
        (Some("enable"), Some(id), None) => {
            let shell_hooks = state.hooks.engine.registered_hooks();
            let shell_hit = shell_hooks.contains(&id);
            // When the id collides between shell and agent layers, enter
            // interactive disambiguation instead of silently picking one.
            if shell_hit && action::agent_list_contains_id(adapter, id) {
                action::begin_hook_action_confirmation(state, id, HookActionKind::Enable);
                return action::render_hook_action_confirmation(state, output);
            }
            // Non-collision: waterfall routing (shell first, then cosh-core)
            if shell_hit {
                // Shell hook: session-level enable
                state.hooks.disabled.remove(id);
                let i18n = state.i18n();
                render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashHooksEnabledTitle),
                    vec![i18n.format(MessageId::SlashHooksEnabledBody, &[("id", id)])],
                    None,
                )
            } else if let AdapterInstance::CoshCore(cosh_core) = adapter {
                // Agent hook: persistent enable via registry
                let params = serde_json::json!({ "name": id });
                let i18n = state.i18n();
                match cosh_core.registry_query("hooks", "enable", params) {
                    Ok(_) => render_notice_panel(
                        output,
                        i18n.t(MessageId::SlashHooksEnabledTitle),
                        vec![
                            i18n.format(MessageId::SlashHooksActionAgentEnabledBody, &[("id", id)])
                        ],
                        None,
                    ),
                    Err(e) => render_notice_panel(
                        output,
                        i18n.t(MessageId::SlashHooksEnabledTitle),
                        vec![i18n.format(
                            MessageId::SlashHooksActionAgentErrorBody,
                            &[("id", id), ("error", &e)],
                        )],
                        None,
                    ),
                }
            } else {
                // No CoshCore adapter: fallback to session-level enable
                state.hooks.disabled.remove(id);
                let i18n = state.i18n();
                render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashHooksEnabledTitle),
                    vec![i18n.format(MessageId::SlashHooksEnabledBody, &[("id", id)])],
                    None,
                )
            }
        }
        (Some("disable"), Some(id), None) => {
            let shell_hooks = state.hooks.engine.registered_hooks();
            let shell_hit = shell_hooks.contains(&id);
            // When the id collides between shell and agent layers, enter
            // interactive disambiguation instead of silently picking one.
            if shell_hit && action::agent_list_contains_id(adapter, id) {
                action::begin_hook_action_confirmation(state, id, HookActionKind::Disable);
                return action::render_hook_action_confirmation(state, output);
            }
            // Non-collision: waterfall routing (shell first, then cosh-core)
            if shell_hit {
                // Shell hook: session-level disable
                state.hooks.disabled.insert(id.to_string());
                let i18n = state.i18n();
                render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashHooksDisabledTitle),
                    vec![i18n.format(MessageId::SlashHooksDisabledBody, &[("id", id)])],
                    None,
                )
            } else if let AdapterInstance::CoshCore(cosh_core) = adapter {
                // Agent hook: persistent disable via registry
                let params = serde_json::json!({ "name": id });
                let i18n = state.i18n();
                match cosh_core.registry_query("hooks", "disable", params) {
                    Ok(_) => render_notice_panel(
                        output,
                        i18n.t(MessageId::SlashHooksDisabledTitle),
                        vec![i18n
                            .format(MessageId::SlashHooksActionAgentDisabledBody, &[("id", id)])],
                        None,
                    ),
                    Err(e) => render_notice_panel(
                        output,
                        i18n.t(MessageId::SlashHooksDisabledTitle),
                        vec![i18n.format(
                            MessageId::SlashHooksActionAgentErrorBody,
                            &[("id", id), ("error", &e)],
                        )],
                        None,
                    ),
                }
            } else {
                // No CoshCore adapter: fallback to session-level disable
                state.hooks.disabled.insert(id.to_string());
                let i18n = state.i18n();
                render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashHooksDisabledTitle),
                    vec![i18n.format(MessageId::SlashHooksDisabledBody, &[("id", id)])],
                    None,
                )
            }
        }
        _ => {
            let i18n = state.i18n();
            render_notice_panel(
                output,
                i18n.t(MessageId::SlashHooksUsageTitle),
                hooks_usage_body(&i18n),
                None,
            )
        }
    }
}

fn hooks_status_body(
    state: &InlineState,
    hooks: &[RegisteredHookInfo],
    i18n: &I18n,
) -> Vec<String> {
    if hooks.is_empty() {
        return vec![i18n.t(MessageId::SlashHooksNoHooksBody).to_string()];
    }

    let total = hooks.len();
    // Aggregate disable counting keeps the baseline session-disable-only
    // semantics: `enabled` means "not session-disabled", orthogonal to the
    // trust dimension reported on the project-trust line.
    let disabled = hooks
        .iter()
        .filter(|hook| state.hooks.disabled.contains(hook.id.as_str()))
        .count();
    let enabled = total.saturating_sub(disabled);
    let builtin = hooks
        .iter()
        .filter(|hook| matches!(hook.source, HookSourceInfo::Builtin))
        .count();
    let user = hooks
        .iter()
        .filter(|hook| matches!(hook.source, HookSourceInfo::ExternalUser))
        .count();
    let project = hooks
        .iter()
        .filter(|hook| matches!(hook.source, HookSourceInfo::ExternalProject))
        .count();
    let trusted = hooks
        .iter()
        .filter(|hook| {
            matches!(hook.source, HookSourceInfo::ExternalProject) && hook.trusted == Some(true)
        })
        .count();
    let untrusted = project.saturating_sub(trusted);

    let total = total.to_string();
    let enabled = enabled.to_string();
    let disabled = disabled.to_string();
    let builtin = builtin.to_string();
    let user = user.to_string();
    let project = project.to_string();
    let trusted = trusted.to_string();
    let untrusted = untrusted.to_string();
    let mut body = vec![
        i18n.format(
            MessageId::SlashHooksStatusCountLine,
            &[
                ("total", &total),
                ("enabled", &enabled),
                ("disabled", &disabled),
            ],
        ),
        i18n.format(
            MessageId::SlashHooksStatusSourcesLine,
            &[
                ("builtin", &builtin),
                ("user", &user),
                ("project", &project),
            ],
        ),
    ];
    if project != "0" {
        body.push(i18n.format(
            MessageId::SlashHooksStatusProjectTrustLine,
            &[("trusted", &trusted), ("untrusted", &untrusted)],
        ));
    }
    body
}

/// File name of an external hook script. Only the basename is shown so
/// entries stay single-line and do not leak home/project directory layout,
/// mirroring how agent-hook entries identify their source by extension
/// name only.
fn hook_file_label(hook: &RegisteredHookInfo) -> String {
    hook.path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".to_string())
}

/// Build per-hook entries for the shell-hooks section. Order is fixed:
/// builtin hooks first, then user hooks, then project hooks. Within a
/// source the original registration order is preserved. Entry disable
/// state mirrors the aggregate's session-disable dimension only; trust
/// stays orthogonal and is conveyed through the detail suffix, so an
/// untrusted project hook renders enabled with a ", untrusted" suffix.
pub(super) fn shell_hook_entries(
    state: &InlineState,
    hooks: &[RegisteredHookInfo],
) -> Vec<HookEntryView> {
    let mut entries: Vec<(usize, usize, HookEntryView)> = hooks
        .iter()
        .enumerate()
        .map(|(orig_idx, hook)| {
            let source_rank = match hook.source {
                HookSourceInfo::Builtin => 0,
                HookSourceInfo::ExternalUser => 1,
                HookSourceInfo::ExternalProject => 2,
            };
            let detail = match hook.source {
                HookSourceInfo::Builtin => "builtin".to_string(),
                HookSourceInfo::ExternalUser => {
                    format!("user: {}", hook_file_label(hook))
                }
                HookSourceInfo::ExternalProject => {
                    let trusted = hook.trusted == Some(true);
                    if trusted {
                        format!("project: {}", hook_file_label(hook))
                    } else {
                        format!("project: {}, untrusted", hook_file_label(hook))
                    }
                }
            };
            // Session-disable only, matching the aggregate counting so the
            // entry marker and the stats line never disagree.
            let disabled = state.hooks.disabled.contains(hook.id.as_str());
            (
                source_rank,
                orig_idx,
                HookEntryView {
                    name: hook.id.clone(),
                    detail,
                    disabled,
                },
            )
        })
        .collect();
    entries.sort_by_key(|(rank, orig_idx, _)| (*rank, *orig_idx));
    entries.into_iter().map(|(_, _, entry)| entry).collect()
}

fn hooks_usage_body(i18n: &I18n) -> Vec<String> {
    [
        MessageId::SlashHooksUsageListLine,
        MessageId::SlashHooksUsageHistoryLine,
        MessageId::SlashHooksUsageEventsLine,
        MessageId::SlashHooksUsageAnalyzeLine,
        MessageId::SlashHooksUsageIgnoreLine,
        MessageId::SlashHooksUsageDetailsLine,
        MessageId::SlashHooksUsageFeedbackLine,
        MessageId::SlashHooksUsageClearFeedbackLine,
        MessageId::SlashHooksUsageMuteLine,
        MessageId::SlashHooksUsageUnmuteLine,
        MessageId::SlashHooksUsageTrustProjectLine,
        MessageId::SlashHooksUsageUntrustProjectLine,
        MessageId::SlashHooksUsageClearProjectTrustLine,
        MessageId::SlashHooksUsageEnableLine,
        MessageId::SlashHooksUsageDisableLine,
    ]
    .into_iter()
    .map(|id| i18n.t(id).to_string())
    .collect()
}

fn render_project_hook_trust<W: Write>(
    trusted: bool,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let project_roots = registered_project_hook_roots(state);
    let updated = state.hooks.engine.set_project_hooks_trusted(trusted);
    let i18n = state.i18n();
    let count = updated.to_string();
    let (title, updated_body) = if trusted {
        (
            i18n.t(MessageId::SlashHooksProjectTrustedTitle),
            i18n.format(
                MessageId::SlashHooksProjectTrustedBody,
                &[("count", &count)],
            ),
        )
    } else {
        (
            i18n.t(MessageId::SlashHooksProjectUntrustedTitle),
            i18n.format(
                MessageId::SlashHooksProjectUntrustedBody,
                &[("count", &count)],
            ),
        )
    };
    let body = if updated == 0 {
        vec![i18n
            .t(MessageId::SlashHooksProjectTrustNoHooksBody)
            .to_string()]
    } else {
        vec![updated_body]
    };
    let footer = if updated == 0 {
        i18n.t(MessageId::SlashHooksProjectTrustNoChangeFooter)
            .to_string()
    } else {
        persist_project_hook_trust(trusted, &project_roots, &i18n)
    };
    render_notice_panel(output, title, body, Some(&footer))
}

fn render_clear_project_hook_trust<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let updated = state.hooks.engine.set_project_hooks_trusted(false);
    let i18n = state.i18n();
    let footer = match clear_project_trust_store() {
        Ok(()) => i18n
            .t(MessageId::SlashHooksProjectTrustClearedFooter)
            .to_string(),
        Err(err) => {
            let err = err.to_string();
            i18n.format(
                MessageId::SlashHooksProjectTrustClearFailedFooter,
                &[("error", &err)],
            )
        }
    };
    let count = updated.to_string();
    render_notice_panel(
        output,
        i18n.t(MessageId::SlashHooksProjectTrustClearedTitle),
        vec![i18n.format(
            MessageId::SlashHooksProjectTrustClearedBody,
            &[("count", &count)],
        )],
        Some(&footer),
    )
}

fn registered_project_hook_roots(state: &InlineState) -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    for hook in state.hooks.engine.registered_hook_infos() {
        if hook.source != HookSourceInfo::ExternalProject {
            continue;
        }
        let Some(root) = hook.project_root else {
            continue;
        };
        if !roots.iter().any(|existing| existing == &root) {
            roots.push(root);
        }
    }
    roots
}

fn persist_project_hook_trust(trusted: bool, roots: &[std::path::PathBuf], i18n: &I18n) -> String {
    let mut failures = Vec::new();
    for root in roots {
        let result = if trusted {
            trust_project_root(root)
        } else {
            untrust_project_root(root)
        };
        if let Err(err) = result {
            failures.push(format!("{}: {err}", root.display()));
        }
    }
    if failures.is_empty() {
        if trusted {
            i18n.t(MessageId::SlashHooksProjectTrustPersistedFooter)
                .to_string()
        } else {
            i18n.t(MessageId::SlashHooksProjectTrustRemovedFooter)
                .to_string()
        }
    } else {
        let failures = failures.join("; ");
        i18n.format(
            MessageId::SlashHooksProjectTrustPersistenceFailedFooter,
            &[("failures", &failures)],
        )
    }
}

fn hooks_footer(state: &InlineState, hook_count: usize, i18n: &I18n) -> String {
    let count = hook_count.to_string();
    if state.hooks.muted_targets.is_empty() {
        return i18n.format(MessageId::SlashHooksFooterCount, &[("count", &count)]);
    }
    let mut muted = state
        .hooks
        .muted_targets
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    muted.sort();
    let targets = muted.join(", ");
    i18n.format(
        MessageId::SlashHooksFooterMutedTargets,
        &[("count", &count), ("targets", &targets)],
    )
}

fn render_hooks_feedback<W: Write>(
    action: &str,
    finding_id: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(feedback) = parse_hook_feedback(action) else {
        let i18n = state.i18n();
        return render_notice_panel(
            output,
            i18n.t(MessageId::SlashHooksUsageTitle),
            vec![i18n.t(MessageId::SlashHooksFeedbackUsageBody).to_string()],
            None,
        );
    };
    let Some(hint) = state
        .hooks
        .findings
        .iter()
        .find(|hint| hint.id == finding_id)
    else {
        let i18n = state.i18n();
        return render_notice_panel(
            output,
            i18n.t(MessageId::SlashHooksFeedbackTitle),
            vec![i18n.format(
                MessageId::SlashHooksFeedbackFindingNotFoundBody,
                &[("finding_id", finding_id)],
            )],
            Some(i18n.t(MessageId::SlashHooksFeedbackFindingNotFoundFooter)),
        );
    };
    let suppression_key = hint.suppression_key.clone();
    let hook_id = hint
        .hook_finding
        .as_ref()
        .map(|finding| finding.hook_id.clone())
        .unwrap_or_else(|| "unknown".to_string());

    if feedback == HookFeedback::Useful {
        state.hooks.ignored_cards.remove(&suppression_key);
    }
    state
        .hooks
        .feedback
        .insert(suppression_key.clone(), feedback);
    let i18n = state.i18n();
    let footer = match record_hook_feedback_preference(hook_feedback_preference(hint, feedback)) {
        Ok(()) => i18n
            .t(MessageId::SlashHooksFeedbackPersistedFooter)
            .to_string(),
        Err(err) => {
            let err = err.to_string();
            i18n.format(
                MessageId::SlashHooksFeedbackPersistenceFailedFooter,
                &[("error", &err)],
            )
        }
    };

    render_notice_panel(
        output,
        i18n.t(MessageId::SlashHooksFeedbackRecordedTitle),
        vec![
            i18n.format(
                MessageId::SlashHooksFeedbackRecordedBody,
                &[("feedback", feedback.label()), ("finding_id", finding_id)],
            ),
            i18n.format(
                MessageId::SlashHooksFeedbackHookLine,
                &[("hook_id", &hook_id)],
            ),
            i18n.format(
                MessageId::SlashHooksFeedbackPolicyKeyLine,
                &[("key", &suppression_key)],
            ),
        ],
        Some(&footer),
    )
}

fn hook_feedback_preference(
    hint: &RuntimeHookFinding,
    feedback: HookFeedback,
) -> HookFeedbackPreference {
    HookFeedbackPreference {
        suppression_key: hint.suppression_key.clone(),
        label: feedback.label().to_string(),
        topic: hint.topic.clone(),
        entity_key: hint.entity_key.clone(),
        severity: hook_severity_label(hint.effective_severity).to_string(),
        command_intent: command_intent_from_suppression_key(&hint.suppression_key).to_string(),
        action: feedback.label().to_string(),
        recorded_at_ms: current_epoch_ms(),
        window_ms: 10 * 60 * 1000,
    }
}

fn command_intent_from_suppression_key(suppression_key: &str) -> &str {
    let mut parts = suppression_key.rsplit(':');
    let last = parts.next().unwrap_or("unknown");
    if is_command_origin_label(last) {
        parts.next().unwrap_or("unknown")
    } else {
        last
    }
}

fn is_command_origin_label(value: &str) -> bool {
    matches!(
        value,
        "user_interactive"
            | "user_send_to_shell"
            | "user_analysis_action"
            | "agent_handoff"
            | "provider_tool"
            | "shell_internal"
            | "unknown"
    )
}

fn current_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn render_clear_hook_feedback<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let cleared = state.hooks.feedback.len();
    state.hooks.feedback.clear();
    state.hooks.ignored_cards.clear();
    let i18n = state.i18n();
    let footer = match clear_hook_feedback_store() {
        Ok(()) => i18n
            .t(MessageId::SlashHooksFeedbackClearedFooter)
            .to_string(),
        Err(err) => {
            let err = err.to_string();
            i18n.format(
                MessageId::SlashHooksFeedbackClearFailedFooter,
                &[("error", &err)],
            )
        }
    };
    let count = cleared.to_string();
    render_notice_panel(
        output,
        i18n.t(MessageId::SlashHooksFeedbackClearedTitle),
        vec![i18n.format(
            MessageId::SlashHooksFeedbackClearedBody,
            &[("count", &count)],
        )],
        Some(&footer),
    )
}

fn parse_hook_feedback(action: &str) -> Option<HookFeedback> {
    match action {
        "noisy" => Some(HookFeedback::Noisy),
        "useful" => Some(HookFeedback::Useful),
        _ => None,
    }
}

fn render_hooks_history<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    let i18n = state.i18n();
    let body = if state.hooks.findings.is_empty() {
        vec![i18n.t(MessageId::SlashHooksHistoryEmptyBody).to_string()]
    } else {
        state
            .hooks
            .findings
            .iter()
            .rev()
            .take(10)
            .map(hook_history_line)
            .collect()
    };
    render_notice_panel(
        output,
        i18n.t(MessageId::SlashHooksHistoryTitle),
        body,
        Some(i18n.t(MessageId::SlashHooksHistoryFooter)),
    )
}

fn render_hooks_events<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    let i18n = state.i18n();
    let body = if state.hooks.display_events.is_empty() {
        vec![i18n.t(MessageId::SlashHooksEventsEmptyBody).to_string()]
    } else {
        state
            .hooks
            .display_events
            .iter()
            .rev()
            .take(10)
            .map(hook_event_line)
            .collect()
    };
    render_notice_panel(
        output,
        i18n.t(MessageId::SlashHooksEventsTitle),
        body,
        Some(i18n.t(MessageId::SlashHooksEventsFooter)),
    )
}

fn hook_event_line(event: &RuntimeHookDisplayEvent) -> String {
    format!(
        "action={} id={} hook={} display={} reason={} topic={} entity={} confidence={} ended_at_ms={} block={} suppression_key={}",
        event.action.label(),
        event.finding_id,
        event.hook_id,
        event.display.label(),
        event.display_reason,
        event.topic,
        event.entity_key,
        event.confidence,
        event.ended_at_ms,
        event.command_block_id,
        event.suppression_key
    )
}

fn hook_history_line(hint: &RuntimeHookFinding) -> String {
    let hook_id = hint
        .hook_finding
        .as_ref()
        .map(|finding| finding.hook_id.as_str())
        .unwrap_or("unknown");
    format!(
        "id={} {} [{}] display={} reason={} topic={} entity={} block={} command={}",
        hint.id,
        hook_id,
        hook_severity_label(hint.effective_severity),
        hint.display.label(),
        hint.display_reason,
        hint.topic,
        hint.entity_key,
        hint.command_block_id,
        hint.command.trim()
    )
}

fn hook_severity_label(severity: FindingSeverity) -> &'static str {
    match severity {
        FindingSeverity::Info => "info",
        FindingSeverity::Warning => "warning",
        FindingSeverity::Critical => "critical",
    }
}

/// Fixed display order for agent hook events; unknown events keep their
/// first-seen order after the known ones.
///
/// Currently consumed only by `group_agent_hooks`. If another surface ever
/// needs the same event ordering (e.g. another panel or CLI output),
/// extract this constant and `agent_hook_event_rank` into a shared hooks
/// domain module instead of duplicating the order.
const AGENT_HOOK_EVENT_ORDER: [&str; 12] = [
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "UserPromptSubmit",
    "BeforeModel",
    "AfterModel",
    "Stop",
    "SessionStart",
    "SessionEnd",
    "PreCompact",
    "Notification",
    "BeforeToolSelection",
];

fn agent_hook_event_rank(event: &str) -> usize {
    AGENT_HOOK_EVENT_ORDER
        .iter()
        .position(|known| *known == event)
        .unwrap_or(AGENT_HOOK_EVENT_ORDER.len())
}

/// Groups the flat `(name, event)` records returned by cosh-core by event
/// type so a hook registered for several events no longer scatters across
/// the panel (#1713). Events follow a fixed lifecycle order; hooks keep the
/// registry order inside each group.
pub(super) fn group_agent_hooks(data: &Value) -> Vec<HookEventGroup> {
    let Some(arr) = data.as_array() else {
        return Vec::new();
    };

    let mut groups: Vec<HookEventGroup> = Vec::new();
    for hook in arr {
        let Some(name) = hook.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let event = hook
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let extension = hook
            .get("extension")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let entry = HookEntryView {
            name: name.to_string(),
            detail: format!("ext: {extension}"),
            disabled: hook
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
        match groups.iter_mut().find(|group| group.event == event) {
            Some(group) => group.hooks.push(entry),
            None => groups.push(HookEventGroup {
                event,
                hooks: vec![entry],
            }),
        }
    }
    // Stable sort: known events by lifecycle rank, unknown events keep their
    // first-seen order after all known ones.
    groups.sort_by_key(|group| agent_hook_event_rank(&group.event));
    groups
}
