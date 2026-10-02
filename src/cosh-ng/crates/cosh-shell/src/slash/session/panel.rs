use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::adapter::{SessionErrorInfo, SessionSummary};
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

use super::RuntimeSessionPanelPhase;

const SESSION_VIEWPORT_SIZE: usize = 8;
const SESSION_PREVIEW_CHARS: usize = 72;

pub(super) fn render_current_session_panel<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(panel) = state.control.session().pending_panel() else {
        return Ok(());
    };
    if state.control.session().active_panel_id() == Some(panel.id.as_str()) {
        return Ok(());
    }
    let (title, body, footer) =
        match panel.phase {
            RuntimeSessionPanelPhase::Browse => {
                let active = core_adapter(adapter).and_then(|core| core.committed_session_id());
                let selected = core_adapter(adapter)
                    .and_then(|core| core.recovery_snapshot().selected_session_id);
                let (start, end) = session_viewport(
                    panel.sessions.len(),
                    panel.selected_option,
                    SESSION_VIEWPORT_SIZE,
                );
                let mut body = Vec::new();
                if start > 0 {
                    body.push(format!("  … +{start}"));
                }
                body.extend(panel.sessions[start..end].iter().enumerate().map(
                    |(offset, summary)| {
                        let index = start + offset;
                        session_picker_line(
                            summary,
                            panel.selected_option == index,
                            panel.selected_for_clear.contains(&summary.session_id),
                            active.as_deref() == Some(summary.session_id.as_str())
                                || selected.as_deref() == Some(summary.session_id.as_str()),
                        )
                    },
                ));
                if end < panel.sessions.len() {
                    body.push(format!("  … +{}", panel.sessions.len() - end));
                }
                // Counters lead the footer so they stay on the first visual
                // line when the key-hint text wraps on narrow terminals.
                let mut footer = format!(
                    "{}/{}",
                    panel.selected_option.saturating_add(1),
                    panel.sessions.len()
                );
                if !panel.selected_for_clear.is_empty() {
                    footer.push_str(" · ");
                    footer.push_str(&state.i18n().format(
                        MessageId::SessionPickerMarkedCount,
                        &[("count", &panel.selected_for_clear.len().to_string())],
                    ));
                }
                footer.push_str(" · ");
                let footer_message = if panel.selected_for_clear.is_empty() {
                    MessageId::SessionPickerFooter
                } else {
                    MessageId::SessionPickerMarkedFooter
                };
                footer.push_str(state.i18n().t(footer_message));
                (
                    state.i18n().t(MessageId::SessionTitle).to_string(),
                    body,
                    footer,
                )
            }
            RuntimeSessionPanelPhase::ConfirmClear => {
                let count = panel.clear_confirmation_ids.len().to_string();
                let mut body = vec![state.i18n().format(
                    MessageId::SessionClearConfirmCountLine,
                    &[("count", &count)],
                )];
                body.extend(
                    panel
                        .clear_confirmation_ids
                        .iter()
                        .take(SESSION_VIEWPORT_SIZE)
                        .map(|session_id| format!("  {session_id}")),
                );
                if panel.clear_confirmation_ids.len() > SESSION_VIEWPORT_SIZE {
                    body.push(format!(
                        "  … +{}",
                        panel.clear_confirmation_ids.len() - SESSION_VIEWPORT_SIZE
                    ));
                }
                if !panel.protected_clear_ids.is_empty() {
                    body.push(state.i18n().t(MessageId::SessionProtectedBody).to_string());
                    body.extend(
                        panel
                            .protected_clear_ids
                            .iter()
                            .take(SESSION_VIEWPORT_SIZE)
                            .map(|session_id| format!("  protected: {session_id}")),
                    );
                    if panel.protected_clear_ids.len() > SESSION_VIEWPORT_SIZE {
                        body.push(format!(
                            "  … +{} protected",
                            panel.protected_clear_ids.len() - SESSION_VIEWPORT_SIZE
                        ));
                    }
                }
                (
                    state
                        .i18n()
                        .t(MessageId::SessionClearConfirmTitle)
                        .to_string(),
                    body,
                    state
                        .i18n()
                        .t(MessageId::SessionClearConfirmFooter)
                        .to_string(),
                )
            }
        };
    let panel_id = panel.id.clone();
    render_notice_panel(output, &title, body.clone(), Some(&footer))?;
    state
        .control
        .session_mut()
        .set_active_panel(panel_id, session_notice_height(&body, Some(&footer)));
    Ok(())
}

pub(super) fn redraw_session_panel<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_session_panel(state, output)?;
    render_current_session_panel(adapter, state, output)
}

pub(super) fn close_session_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_session_panel(state, output)?;
    state.control.session_mut().clear_pending_panel();
    Ok(())
}

fn clear_active_session_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let height = state.control.session().active_panel_height();
    if height == 0 {
        state.control.session_mut().clear_active_panel_id();
        return Ok(());
    }
    write!(output, "\x1b[{height}A")?;
    for row in 0..height {
        write!(output, "\r\x1b[2K")?;
        if row + 1 < height {
            write!(output, "\x1b[1B")?;
        }
    }
    if height > 1 {
        write!(output, "\x1b[{}A", height - 1)?;
    }
    write!(output, "\r")?;
    state.control.session_mut().clear_active_panel();
    Ok(())
}

fn session_notice_height(body: &[String], footer: Option<&str>) -> usize {
    let renderer = RatatuiInlineRenderer::for_terminal();
    let mut lines = body
        .iter()
        .flat_map(|line| renderer.markdown_text_lines(line))
        .collect::<Vec<_>>();
    if let Some(footer) = footer {
        lines.extend(renderer.markdown_text_lines(footer));
    }
    lines.len().max(1) + 2
}

/// Renders `/session list --all` output grouped by workspace_scope.
///
/// Workspaces are sorted alphabetically; sessions within each workspace are
/// sorted newest-first, consistent with the default `/session list` order.
/// The group for `current_workspace` is labelled with ` (current)` so users can
/// locate resumable sessions without scanning individual health indicators.
pub(super) fn session_all_workspaces_list_lines(
    summaries: &[SessionSummary],
    current_workspace: &str,
) -> Vec<String> {
    if summaries.is_empty() {
        return Vec::new();
    }

    let mut groups: BTreeMap<&str, Vec<&SessionSummary>> = BTreeMap::new();
    for summary in summaries {
        groups
            .entry(summary.workspace_scope.as_str())
            .or_default()
            .push(summary);
    }

    let mut lines = Vec::new();
    for (workspace, group) in &groups {
        if workspace == &current_workspace {
            lines.push(format!("{workspace} (current)"));
        } else {
            lines.push(workspace.to_string());
        }
        let mut group = group.clone();
        group.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
        });
        for summary in group {
            for line in session_list_lines(summary) {
                lines.push(format!("  {line}"));
            }
        }
    }
    lines
}

/// Renders one `/session list` entry: the full canonical session ID leads so
/// it can be copied verbatim into `/session resume <id>` or
/// `/session clear <id>`, followed by an indented prompt preview when the
/// session has one.
pub(super) fn session_list_lines(summary: &SessionSummary) -> Vec<String> {
    let model = summary.model.as_deref().unwrap_or("-");
    let mut lines = vec![format!(
        "{} · {} · {} · {} msg · {}",
        summary.session_id,
        summary.health.label(),
        model,
        summary.message_count,
        relative_time(summary.updated_at_ms),
    )];
    if let Some(prompt) = summary
        .first_prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        lines.push(format!(
            "  {}",
            bounded_prompt_preview(prompt, SESSION_PREVIEW_CHARS)
        ));
    }
    lines
}

/// Renders one interactive picker row. The short ID prefix is visual
/// disambiguation only — resume and clear always operate on the full
/// canonical UUID carried by the panel state, never on the prefix.
pub(super) fn session_picker_line(
    summary: &SessionSummary,
    focused: bool,
    marked_for_clear: bool,
    protected: bool,
) -> String {
    let cursor = if focused { ">" } else { " " };
    let marked = if marked_for_clear { "[x]" } else { "[ ]" };
    let protected = if protected { " protected" } else { "" };
    let prompt = summary
        .first_prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
        .unwrap_or(&summary.session_id);
    let prompt = bounded_prompt_preview(prompt, SESSION_PREVIEW_CHARS);
    let model = summary.model.as_deref().unwrap_or("-");
    format!(
        "{cursor} {marked} {} · {prompt} · {} · {} msg · {} · {}{protected}",
        short_session_id(&summary.session_id),
        relative_time(summary.updated_at_ms),
        summary.message_count,
        model,
        summary.health.label()
    )
}

/// First eight characters of the session ID plus an ellipsis when truncated.
/// Character-based so a malformed non-ASCII ID cannot split a code point.
fn short_session_id(session_id: &str) -> String {
    let prefix: String = session_id.chars().take(8).collect();
    if session_id.chars().count() > 8 {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn session_viewport(total: usize, selected: usize, capacity: usize) -> (usize, usize) {
    if total == 0 || capacity == 0 {
        return (0, 0);
    }
    let capacity = capacity.min(total);
    let selected = selected.min(total - 1);
    let start = selected
        .saturating_sub(capacity / 2)
        .min(total.saturating_sub(capacity));
    (start, start + capacity)
}

fn bounded_prompt_preview(prompt: &str, max_chars: usize) -> String {
    // Strip control characters so persisted metadata cannot inject terminal
    // control bytes (BEL, BS, C1 CSI) into rendered picker rows.
    let compact = prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let keep = max_chars.saturating_sub(1);
    let mut bounded = compact.chars().take(keep).collect::<String>();
    bounded.push('…');
    bounded
}

fn relative_time(updated_at_ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(updated_at_ms);
    let seconds = now.saturating_sub(updated_at_ms) / 1_000;
    match seconds {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

pub(super) fn render_not_ready<W: Write>(
    summary: &SessionSummary,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionErrorTitle),
        vec![state.i18n().format(
            MessageId::SessionNotReadyBody,
            &[
                ("id", &summary.session_id),
                ("health", summary.health.label()),
            ],
        )],
        None,
    )
}

pub(super) fn render_session_error<W: Write>(
    state: &InlineState,
    output: &mut W,
    error: &SessionErrorInfo,
) -> std::io::Result<()> {
    let mut body = vec![state.i18n().format(
        MessageId::SessionErrorLine,
        &[("code", &error.code), ("error", &error.message)],
    )];
    if let Some(hint) = error.hint.as_deref() {
        body.push(hint.to_string());
    }
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionErrorTitle),
        body,
        None,
    )
}

pub(super) fn render_unavailable<W: Write>(
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionErrorTitle),
        vec![state
            .i18n()
            .t(MessageId::SessionUnavailableBody)
            .to_string()],
        None,
    )
}

pub(super) fn render_usage<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionTitle),
        vec![state.i18n().t(MessageId::SessionUsageBody).to_string()],
        None,
    )
}

/// Gate for session mutations (`clear`, `resume`, `delete`, starting a new
/// compaction): idle only when no interaction is pending *and* no part of the
/// compaction lifecycle — running compactor, finished-but-unrendered
/// completion, or recommended automatic attempt — could be invalidated by the
/// mutation.
pub(super) fn session_management_idle(state: &InlineState) -> bool {
    session_interaction_idle(state) && !crate::slash::session::compaction_pending_or_active(state)
}

/// Interaction-only idle check shared by [`session_management_idle`] and the
/// automatic compaction starter.
///
/// Deliberately excludes compaction state: the auto-start path must not be
/// blocked by its *own* pending recommendation, so it layers its own
/// active/pending-completion checks on top of this.
pub(super) fn session_interaction_idle(state: &InlineState) -> bool {
    state.agent_run.active.is_none()
        && !state
            .approvals
            .requests
            .iter()
            .any(|request| request.status == ApprovalRequestStatus::Pending)
        && state.questions.pending_id.is_none()
        && state.auth.state.is_none()
        && state.control.pending_mode_panel().is_none()
        && state.control.pending_config_panel().is_none()
        && state.control.pending_config_language_panel().is_none()
        && state.control.session().pending_panel().is_none()
}

pub(super) fn workspace_scope(blocks: &[CommandBlock]) -> String {
    let candidate = blocks.last().map(|block| {
        if block.end_cwd.is_empty() {
            block.cwd.clone()
        } else {
            block.end_cwd.clone()
        }
    });
    let path = candidate
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::canonicalize(&path)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub(super) fn core_adapter(adapter: &AdapterInstance) -> Option<&crate::adapter::CoshCoreAdapter> {
    match adapter {
        AdapterInstance::CoshCore(core) => Some(core),
        _ => None,
    }
}

pub(super) fn partition_protected(
    requested: Vec<String>,
    protected: &[String],
) -> (Vec<String>, Vec<String>) {
    requested
        .into_iter()
        .partition(|session_id| !protected.contains(session_id))
}

pub(super) enum SessionCardAction {
    Focus { id: String, selected: usize },
    Toggle { id: String, selected: usize },
    Resume { id: String, selected: usize },
    Delete { id: String },
    ConfirmClear { id: String },
    Cancel { id: String },
}

pub(super) fn session_card_action_from_event(event: &ShellEvent) -> Option<SessionCardAction> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("card")
    {
        return None;
    }
    match event.message.as_deref()? {
        "session_focus" => {
            let (id, selected) = split_session_value(event.input.as_deref()?)?;
            Some(SessionCardAction::Focus { id, selected })
        }
        "session_toggle" => {
            let (id, selected) = split_session_value(event.input.as_deref()?)?;
            Some(SessionCardAction::Toggle { id, selected })
        }
        "session_resume" => {
            let (id, selected) = split_session_value(event.input.as_deref()?)?;
            Some(SessionCardAction::Resume { id, selected })
        }
        "session_delete" => Some(SessionCardAction::Delete {
            id: event.input.as_deref()?.to_string(),
        }),
        "session_clear_confirm" => Some(SessionCardAction::ConfirmClear {
            id: event.input.as_deref()?.to_string(),
        }),
        "session_cancel" => Some(SessionCardAction::Cancel {
            id: event.input.as_deref()?.to_string(),
        }),
        _ => None,
    }
}

fn split_session_value(value: &str) -> Option<(String, usize)> {
    let (id, selected) = value.rsplit_once(':')?;
    Some((id.to_string(), selected.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::SessionHealth;

    fn ready_summary() -> SessionSummary {
        SessionSummary {
            session_id: "00000000-0000-4000-8000-000000000000".to_string(),
            workspace_scope: "/tmp".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            model: Some("mock".to_string()),
            message_count: 3,
            first_prompt: Some("remember this".to_string()),
            schema_version: Some(1),
            health: SessionHealth::Corrupt,
        }
    }

    #[test]
    fn picker_lines_surface_short_id_health_and_clear_mark() {
        let line = session_picker_line(&ready_summary(), true, true, false);
        assert!(
            line.starts_with("> [x] 00000000… · remember this"),
            "{line}"
        );
        assert!(line.contains("3 msg"));
        assert!(line.contains("corrupt"));
        assert!(
            !line.contains("00000000-0000-4000-8000-000000000000"),
            "picker rows stay compact: {line}"
        );
    }

    #[test]
    fn list_lines_keep_full_id_when_first_prompt_is_present() {
        let lines = session_list_lines(&ready_summary());
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with("00000000-0000-4000-8000-000000000000 · corrupt · mock · 3 msg"),
            "{lines:?}"
        );
        assert!(lines[1].contains("remember this"), "{lines:?}");
    }

    #[test]
    fn list_lines_sanitize_prompt_preview() {
        let mut summary = ready_summary();
        summary.first_prompt = Some("line one\nline\u{7} two".to_string());
        let lines = session_list_lines(&summary);
        assert_eq!(lines[1], "  line one line two", "{lines:?}");
    }

    #[test]
    fn empty_prompt_falls_back_to_session_id_without_losing_ids() {
        let mut summary = ready_summary();
        summary.first_prompt = Some("   ".to_string());

        let lines = session_list_lines(&summary);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("00000000-0000-4000-8000-000000000000"),
            "{lines:?}"
        );

        let line = session_picker_line(&summary, false, false, true);
        assert!(line.contains("00000000…"), "{line}");
        assert!(
            line.contains("· 00000000-0000-4000-8000-000000000000 ·"),
            "fallback preview keeps the full ID: {line}"
        );
        assert!(line.ends_with("protected"), "{line}");
    }

    #[test]
    fn short_session_id_is_utf8_safe_and_skips_ellipsis_when_short() {
        assert_eq!(short_session_id("abcd"), "abcd");
        assert_eq!(short_session_id("0123456789"), "01234567…");
        assert_eq!(
            short_session_id("会话标识符测试超长编号"),
            "会话标识符测试超…"
        );
    }

    #[test]
    fn session_viewport_tracks_selection_without_rendering_every_entry() {
        assert_eq!(session_viewport(30, 0, 8), (0, 8));
        assert_eq!(session_viewport(30, 15, 8), (11, 19));
        assert_eq!(session_viewport(30, 29, 8), (22, 30));
        assert_eq!(session_viewport(3, 2, 8), (0, 3));
    }

    #[test]
    fn session_preview_is_single_line_utf8_safe_and_bounded() {
        let preview = bounded_prompt_preview(
            "第一行\n第二行\twith a deliberately long suffix that must not fill the terminal",
            24,
        );
        assert!(!preview.contains('\n'));
        assert!(!preview.contains('\t'));
        assert!(preview.chars().count() <= 24);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn session_preview_strips_terminal_control_bytes() {
        let preview = bounded_prompt_preview("safe\u{7}\u{8}\u{9b}31mtext", 24);
        assert_eq!(preview, "safe31mtext");
    }

    #[test]
    fn clear_partition_protects_active_and_selected_ids() {
        let (clearable, protected) = partition_protected(
            vec![
                "old".to_string(),
                "active".to_string(),
                "selected".to_string(),
            ],
            &["active".to_string(), "selected".to_string()],
        );
        assert_eq!(clearable, vec!["old"]);
        assert_eq!(protected, vec!["active", "selected"]);
    }

    #[test]
    fn session_management_waits_for_pending_questions() {
        let mut state = InlineState::default();
        assert!(session_management_idle(&state));

        state.questions.pending_id = Some("question-1".to_string());
        assert!(!session_management_idle(&state));
    }
}
