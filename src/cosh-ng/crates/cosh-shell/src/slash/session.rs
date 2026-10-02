mod command;
mod compact;
mod fresh;
mod panel;
mod state;
#[cfg(test)]
mod tests;

use self::command::{parse_session_command, SessionCommand};
pub(crate) use self::compact::{
    compaction_active, compaction_pending_or_active, note_compaction_recommendation,
    poll_background_compaction, render_agent_queue_full_notice, render_compaction_paused_notice,
    render_control_queue_full_notice,
};
use self::panel::{
    close_session_panel, core_adapter, partition_protected, redraw_session_panel,
    render_current_session_panel, render_not_ready, render_session_error, render_unavailable,
    render_usage, session_all_workspaces_list_lines, session_card_action_from_event,
    session_list_lines, session_management_idle, workspace_scope, SessionCardAction,
};
pub(crate) use self::state::{
    RuntimeSessionPanel, RuntimeSessionPanelPhase, SessionControlState, SessionLaunchRequest,
};
use crate::adapter::{SessionErrorInfo, SessionRecoveryState};
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;
use crate::slash::prompt::write_shell_prompt;

const SESSION_PAGE_SIZE: usize = 20;
// Must stay in sync with cosh-core's MAX_LIST_LIMIT (currently 100). Core
// clamps the requested limit to that range, so a value above it has no effect.
const SESSION_ALL_PAGE_SIZE: usize = 100;
const SESSION_PAGE_LOAD_AHEAD: usize = 4;

pub(crate) fn render_session_command<W: Write>(
    arguments: &str,
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    match parse_session_command(arguments) {
        SessionCommand::OpenPicker => open_session_manager(blocks, adapter, state, output),
        SessionCommand::New => {
            fresh::start_fresh_session(adapter, state, output)?;
            Ok(true)
        }
        SessionCommand::Status => {
            render_session_status(blocks, adapter, state, output)?;
            Ok(true)
        }
        SessionCommand::List { all } => {
            render_session_list(blocks, adapter, state, output, all)?;
            Ok(true)
        }
        SessionCommand::Resume(session_id) => {
            select_session(session_id, blocks, adapter, state, output)?;
            Ok(true)
        }
        SessionCommand::Clear(requested) => {
            begin_explicit_clear(requested, blocks, adapter, state, output)
        }
        SessionCommand::Compact(subcommand) => {
            compact::render_session_compact_command(subcommand, blocks, adapter, state, output)?;
            Ok(true)
        }
        SessionCommand::Usage => {
            render_usage(state, output)?;
            Ok(true)
        }
    }
}

pub(crate) fn render_session_launch<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if !events
        .iter()
        .any(|event| event.kind == ShellEventKind::ShellReady)
    {
        return Ok(());
    }
    let Some(request) = state.control.session_mut().take_pending_launch() else {
        return Ok(());
    };
    match request {
        SessionLaunchRequest::Picker => {
            let _ = open_session_manager(blocks, adapter, state, output)?;
        }
        SessionLaunchRequest::Resume(session_id) => {
            select_session(&session_id, blocks, adapter, state, output)?;
        }
    }
    Ok(())
}

pub(crate) fn render_session_card_actions<W: Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let Some(action) = session_card_action_from_event(event) else {
            continue;
        };
        let key = format!(
            "{}:{}",
            stable_event_key("session-card", event_index_base + idx, event),
            event.message.as_deref().unwrap_or_default()
        );
        if !state.control.session_mut().claim_action(key) {
            continue;
        }
        match action {
            SessionCardAction::Focus { id, selected } => {
                let load_request = {
                    let Some(panel) = state
                        .control
                        .session_mut()
                        .pending_panel_mut()
                        .filter(|panel| panel.id == id)
                    else {
                        continue;
                    };
                    if panel.phase != RuntimeSessionPanelPhase::Browse || panel.sessions.is_empty()
                    {
                        continue;
                    }
                    panel.selected_option = selected.min(panel.sessions.len().saturating_sub(1));
                    (panel
                        .selected_option
                        .saturating_add(SESSION_PAGE_LOAD_AHEAD)
                        >= panel.sessions.len())
                    .then(|| {
                        panel
                            .next_cursor
                            .clone()
                            .map(|cursor| (panel.workspace_scope.clone(), cursor, panel.id.clone()))
                    })
                    .flatten()
                };
                let pagination_error = load_request.and_then(|(workspace, cursor, panel_id)| {
                    let core = core_adapter(adapter)?;
                    match core.list_sessions_page(
                        &workspace,
                        SESSION_PAGE_SIZE,
                        Some(&cursor),
                        false,
                    ) {
                        Ok(page) => {
                            let panel = state.control.session_mut().pending_panel_mut().filter(
                                |panel| {
                                    panel.id == panel_id
                                        && panel.next_cursor.as_deref() == Some(cursor.as_str())
                                },
                            )?;
                            for summary in page.sessions {
                                if !panel
                                    .sessions
                                    .iter()
                                    .any(|current| current.session_id == summary.session_id)
                                {
                                    panel.sessions.push(summary);
                                }
                            }
                            panel.next_cursor = page.next_cursor;
                            None
                        }
                        Err(error) => Some(error),
                    }
                });
                if let Some(error) = pagination_error {
                    close_session_panel(state, output)?;
                    render_session_error(state, output, &error)?;
                    write_shell_prompt(state, output)?;
                } else {
                    redraw_session_panel(adapter, state, output)?;
                }
            }
            SessionCardAction::Toggle { id, selected } => {
                let Some(panel) = state
                    .control
                    .session_mut()
                    .pending_panel_mut()
                    .filter(|panel| panel.id == id)
                else {
                    continue;
                };
                if panel.phase != RuntimeSessionPanelPhase::Browse {
                    continue;
                }
                let Some(summary) = panel.sessions.get(selected) else {
                    continue;
                };
                if !panel.selected_for_clear.remove(&summary.session_id) {
                    panel.selected_for_clear.insert(summary.session_id.clone());
                }
                redraw_session_panel(adapter, state, output)?;
            }
            SessionCardAction::Resume { id, selected } => {
                let Some(panel) = state
                    .control
                    .session()
                    .pending_panel()
                    .filter(|panel| panel.id == id)
                    .cloned()
                else {
                    continue;
                };
                let Some(summary) = panel.sessions.get(selected) else {
                    close_session_panel(state, output)?;
                    render_session_error(
                        state,
                        output,
                        &SessionErrorInfo {
                            code: "not_found".to_string(),
                            message: "selected session is no longer listed".to_string(),
                            recoverable: true,
                            hint: Some("Refresh the session list and retry.".to_string()),
                        },
                    )?;
                    write_shell_prompt(state, output)?;
                    continue;
                };
                close_session_panel(state, output)?;
                if !summary.health.can_resume() {
                    render_not_ready(summary, state, output)?;
                    write_shell_prompt(state, output)?;
                    continue;
                }
                select_session_in_scope(
                    &panel.workspace_scope,
                    &summary.session_id,
                    adapter,
                    state,
                    output,
                )?;
                write_shell_prompt(state, output)?;
            }
            SessionCardAction::Delete { id } => {
                let protected = core_adapter(adapter)
                    .map(|adapter| adapter.protected_session_ids())
                    .unwrap_or_default();
                let Some(panel) = state
                    .control
                    .session_mut()
                    .pending_panel_mut()
                    .filter(|panel| panel.id == id)
                else {
                    continue;
                };
                let mut requested = if panel.selected_for_clear.is_empty() {
                    panel
                        .sessions
                        .get(panel.selected_option)
                        .map(|summary| vec![summary.session_id.clone()])
                        .unwrap_or_default()
                } else {
                    panel.selected_for_clear.iter().cloned().collect::<Vec<_>>()
                };
                requested.sort();
                let (clearable, protected_ids) = partition_protected(requested, &protected);
                if clearable.is_empty() {
                    redraw_session_panel(adapter, state, output)?;
                    render_notice_panel(
                        output,
                        state.i18n().t(MessageId::SessionErrorTitle),
                        vec![state.i18n().t(MessageId::SessionProtectedBody).to_string()],
                        None,
                    )?;
                    continue;
                }
                panel.clear_confirmation_ids = clearable;
                panel.protected_clear_ids = protected_ids;
                panel.phase = RuntimeSessionPanelPhase::ConfirmClear;
                redraw_session_panel(adapter, state, output)?;
            }
            SessionCardAction::ConfirmClear { id } => {
                let Some(panel) = state
                    .control
                    .session()
                    .pending_panel()
                    .filter(|panel| panel.id == id)
                    .cloned()
                else {
                    continue;
                };
                close_session_panel(state, output)?;
                clear_sessions(
                    &panel.workspace_scope,
                    &panel.clear_confirmation_ids,
                    panel.protected_clear_ids.len(),
                    adapter,
                    state,
                    output,
                )?;
                write_shell_prompt(state, output)?;
            }
            SessionCardAction::Cancel { id } => {
                let Some(_panel) = state
                    .control
                    .session()
                    .pending_panel()
                    .filter(|panel| panel.id == id)
                else {
                    continue;
                };
                close_session_panel(state, output)?;
                render_notice_panel(
                    output,
                    state.i18n().t(MessageId::SessionCancelledTitle),
                    vec![state.i18n().t(MessageId::SessionCancelledBody).to_string()],
                    None,
                )?;
                write_shell_prompt(state, output)?;
            }
        }
        output.flush()?;
    }
    Ok(())
}

fn open_session_manager<W: Write>(
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    if !session_management_idle(state) {
        render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionErrorTitle),
            vec![state.i18n().t(MessageId::SessionBusyBody).to_string()],
            None,
        )?;
        return Ok(true);
    }
    let Some(core) = core_adapter(adapter) else {
        render_unavailable(state, output)?;
        return Ok(true);
    };
    let workspace = workspace_scope(blocks);
    let list = match core.list_sessions(&workspace, false) {
        Ok(list) => list,
        Err(error) => {
            render_session_error(state, output, &error)?;
            return Ok(true);
        }
    };
    if list.sessions.is_empty() {
        render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionTitle),
            vec![state.i18n().t(MessageId::SessionEmptyBody).to_string()],
            Some(state.i18n().t(MessageId::SessionListFooter)),
        )?;
        return Ok(true);
    }
    let panel_id = state.control.session_mut().new_panel_id();
    state
        .control
        .session_mut()
        .set_pending_panel(RuntimeSessionPanel {
            id: panel_id,
            workspace_scope: workspace,
            sessions: list.sessions,
            next_cursor: list.next_cursor,
            selected_option: 0,
            selected_for_clear: HashSet::new(),
            clear_confirmation_ids: Vec::new(),
            protected_clear_ids: Vec::new(),
            phase: RuntimeSessionPanelPhase::Browse,
        });
    render_current_session_panel(adapter, state, output)?;
    Ok(false)
}

fn render_session_list<W: Write>(
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
    all_workspaces: bool,
) -> std::io::Result<()> {
    let Some(core) = core_adapter(adapter) else {
        return render_unavailable(state, output);
    };
    let limit = if all_workspaces {
        SESSION_ALL_PAGE_SIZE
    } else {
        SESSION_PAGE_SIZE
    };
    let workspace = workspace_scope(blocks);
    let list = match core.list_sessions_page(&workspace, limit, None, all_workspaces) {
        Ok(list) => list,
        Err(error) => return render_session_error(state, output, &error),
    };
    let mut body = if list.sessions.is_empty() {
        vec![state.i18n().t(MessageId::SessionEmptyBody).to_string()]
    } else if all_workspaces {
        session_all_workspaces_list_lines(&list.sessions, &workspace)
    } else {
        list.sessions.iter().flat_map(session_list_lines).collect()
    };
    if list.next_cursor.is_some() {
        body.push("  …".to_string());
    }
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionTitle),
        body,
        Some(state.i18n().t(MessageId::SessionListFooter)),
    )
}

fn render_session_status<W: Write>(
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let shell_id = state
        .shell_session_id
        .as_deref()
        .or_else(|| blocks.last().map(|block| block.session_id.as_str()))
        .unwrap_or("<none>");
    let workspace = workspace_scope(blocks);
    let (active_provider_id, selected_provider_id, recovery_state, error) =
        match core_adapter(adapter) {
            Some(core) => {
                let recovery = core.recovery_snapshot();
                (
                    core.committed_session_id()
                        .unwrap_or_else(|| "<none>".to_string()),
                    recovery
                        .selected_session_id
                        .clone()
                        .unwrap_or_else(|| "<none>".to_string()),
                    recovery.state,
                    recovery.last_error,
                )
            }
            None => (
                adapter
                    .committed_session_id()
                    .unwrap_or_else(|| "<none>".to_string()),
                "<none>".to_string(),
                SessionRecoveryState::None,
                None,
            ),
        };
    let mut body = vec![state
        .i18n()
        .format(MessageId::SessionShellIdLine, &[("id", shell_id)])];
    body.extend(
        state
            .i18n()
            .format(
                MessageId::SessionProviderIdLine,
                &[
                    ("active", &active_provider_id),
                    ("selected", &selected_provider_id),
                ],
            )
            .lines()
            .map(ToOwned::to_owned),
    );
    body.extend([
        state.i18n().format(
            MessageId::SessionWorkspaceLine,
            &[("workspace", &workspace)],
        ),
        state.i18n().format(
            MessageId::SessionRecoveryLine,
            &[("state", recovery_state.label())],
        ),
    ]);
    if let Some(error) = error {
        body.push(state.i18n().format(
            MessageId::SessionErrorLine,
            &[("code", &error.code), ("error", &error.message)],
        ));
    }
    body.push(
        state
            .i18n()
            .t(MessageId::SessionEvidenceNotRestoredBody)
            .to_string(),
    );
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionStatusTitle),
        body,
        None,
    )
}

fn select_session<W: Write>(
    session_id: &str,
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    select_session_in_scope(&workspace_scope(blocks), session_id, adapter, state, output)
}

fn select_session_in_scope<W: Write>(
    workspace: &str,
    session_id: &str,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if !session_management_idle(state) {
        return render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionErrorTitle),
            vec![state.i18n().t(MessageId::SessionBusyBody).to_string()],
            None,
        );
    }
    let Some(core) = core_adapter(adapter) else {
        return render_unavailable(state, output);
    };
    match core.select_session(workspace, session_id) {
        Ok(summary) => render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionSelectedTitle),
            vec![state.i18n().format(
                MessageId::SessionSelectedBody,
                &[("id", &summary.session_id)],
            )],
            Some(state.i18n().t(MessageId::SessionEvidenceNotRestoredBody)),
        ),
        Err(error) => render_session_error(state, output, &error),
    }
}

fn begin_explicit_clear<W: Write>(
    requested: Vec<String>,
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    if !session_management_idle(state) {
        render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionErrorTitle),
            vec![state.i18n().t(MessageId::SessionBusyBody).to_string()],
            None,
        )?;
        return Ok(true);
    }
    let Some(core) = core_adapter(adapter) else {
        render_unavailable(state, output)?;
        return Ok(true);
    };
    let workspace = workspace_scope(blocks);
    let (requested, mut protected_ids) = if requested.as_slice() == ["--all"] {
        match core.prepare_clear_all(&workspace) {
            Ok(plan) => (plan.session_ids, plan.protected_session_ids),
            Err(error) => {
                render_session_error(state, output, &error)?;
                return Ok(true);
            }
        }
    } else if requested.iter().any(|value| value == "--all") {
        render_usage(state, output)?;
        return Ok(true);
    } else {
        (requested, Vec::new())
    };
    if requested.is_empty() {
        if !protected_ids.is_empty() {
            let count = protected_ids.len().to_string();
            render_notice_panel(
                output,
                state.i18n().t(MessageId::SessionErrorTitle),
                vec![
                    state.i18n().t(MessageId::SessionProtectedBody).to_string(),
                    state
                        .i18n()
                        .format(MessageId::SessionSkippedBody, &[("count", &count)]),
                ],
                None,
            )?;
            return Ok(true);
        }
        render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionTitle),
            vec![state.i18n().t(MessageId::SessionEmptyBody).to_string()],
            None,
        )?;
        return Ok(true);
    }
    let protected = core.protected_session_ids();
    let (clearable, newly_protected) = partition_protected(requested, &protected);
    protected_ids.extend(newly_protected);
    protected_ids.sort();
    protected_ids.dedup();
    if clearable.is_empty() {
        render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionErrorTitle),
            vec![state.i18n().t(MessageId::SessionProtectedBody).to_string()],
            None,
        )?;
        return Ok(true);
    }
    let panel_id = state.control.session_mut().new_panel_id();
    state
        .control
        .session_mut()
        .set_pending_panel(RuntimeSessionPanel {
            id: panel_id,
            workspace_scope: workspace,
            sessions: Vec::new(),
            next_cursor: None,
            selected_option: 0,
            selected_for_clear: HashSet::new(),
            clear_confirmation_ids: clearable,
            protected_clear_ids: protected_ids,
            phase: RuntimeSessionPanelPhase::ConfirmClear,
        });
    render_current_session_panel(adapter, state, output)?;
    Ok(false)
}

fn clear_sessions<W: Write>(
    workspace: &str,
    session_ids: &[String],
    shell_protected_count: usize,
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(core) = core_adapter(adapter) else {
        return render_unavailable(state, output);
    };
    match core.clear_sessions(workspace, session_ids) {
        Ok(result) => {
            let skipped = result.skipped.len() + shell_protected_count;
            let mut body = vec![state.i18n().format(
                MessageId::SessionClearedBody,
                &[("count", &result.deleted.len().to_string())],
            )];
            if skipped > 0 {
                body.push(state.i18n().format(
                    MessageId::SessionSkippedBody,
                    &[("count", &skipped.to_string())],
                ));
            }
            if let Some(interruption) = result.interruption {
                body.push(state.i18n().format(
                    MessageId::SessionClearInterruptedBody,
                    &[
                        ("code", &interruption.error.code),
                        (
                            "unknown",
                            &interruption.unknown_session_ids.len().to_string(),
                        ),
                        (
                            "unattempted",
                            &interruption.unattempted_session_ids.len().to_string(),
                        ),
                    ],
                ));
            }
            render_notice_panel(
                output,
                state.i18n().t(MessageId::SessionClearedTitle),
                body,
                None,
            )
        }
        Err(error) => render_session_error(state, output, &error),
    }
}
