//! Task-owned workspace snapshot inspection and switch confirmation.

mod contract;
mod view;

use std::collections::HashSet;
use std::io::Write;

use serde_json::Value;
use uuid::Uuid;

use crate::config::Language;
use crate::raw_input::RawInputCapture;
use crate::runtime::dispatcher::stable_event_key;
use crate::runtime::prelude::{
    QuestionInputFeedback, QuestionPanelModel, QuestionPanelPresentation, QuestionSelectionMode,
    RatatuiInlineRenderer, ShellEvent, ShellEventKind,
};
use crate::runtime::question_terminal::clear_active_question_panel;
use crate::runtime::state::InlineState;
use crate::slash::panel::render_notice_panel;
use crate::ui::OPTION_DETAIL_SEPARATOR;

use contract::{is_terminal_task_state, task_projection, terminal_task_projection, TaskProjection};
use view::{
    localized, render_snapshot_error, render_usage, safe_field, workspace_label, BoundedLines,
};

const SWITCH_OPTION_COUNT: usize = 2;
const SWITCH_CONFIRM_INDEX: usize = 0;
const SWITCH_CANCEL_INDEX: usize = 1;
const MAX_SNAPSHOT_ROWS: usize = 200;

#[derive(Debug)]
struct PendingSnapshotSwitch {
    panel_id: String,
    task_id: String,
    snapshot_id: String,
    task_state: String,
    workspace: String,
    expected_revision: u64,
    preview_digest: String,
    idempotency_key: String,
    preview_lines: Vec<String>,
    selected: usize,
}

#[derive(Debug, Default)]
pub(crate) struct TaskSnapshotState {
    pending_switch: Option<PendingSnapshotSwitch>,
    handled_events: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotAction {
    Preview,
    Diff,
    Switch,
}

impl SnapshotAction {
    fn gateway_argument(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Diff => "diff",
            Self::Switch => "switch",
        }
    }
}

pub(super) fn render_snapshot_list_command<W: Write>(
    arguments: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    let mut parts = arguments.split_whitespace();
    let _snapshots = parts.next();
    let requested_task_id = parts.next();
    if parts.next().is_some() {
        render_usage(state, output)?;
        return Ok(true);
    }
    let task_id = match requested_task_id.map(str::to_owned) {
        Some(task_id) => task_id,
        None => match super::latest_task_id() {
            Ok(task_id) => task_id,
            Err(error) => {
                render_snapshot_error(state, output, error)?;
                return Ok(true);
            }
        },
    };
    let result = run_snapshot_list(&task_id);
    match result {
        Ok(value) => {
            let projection = match task_projection(&value, &task_id) {
                Ok(projection) => projection,
                Err(error) => {
                    render_snapshot_error(state, output, error)?;
                    return Ok(true);
                }
            };
            let mut lines = vec![format!(
                "{}  {}  revision {}",
                safe_field(&task_id),
                safe_field(&projection.state),
                projection.revision
            )];
            lines.push(format!("Workspace: {}", workspace_label(&value)));
            lines.extend(snapshot_list_lines(&value));
            render_notice_panel(
                output,
                localized(state, "Task snapshots", "Task 快照"),
                lines,
                Some(localized(
                    state,
                    "Preview with /task snapshot preview <task-id> <snapshot-id>.",
                    "使用 /task snapshot preview <task-id> <snapshot-id> 预览。",
                )),
            )?;
        }
        Err(error) => render_snapshot_error(state, output, error)?,
    }
    Ok(true)
}

pub(super) fn render_snapshot_command<W: Write>(
    arguments: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    let mut parts = arguments.split_whitespace();
    let _snapshot = parts.next();
    let action = match parts.next() {
        Some("preview") => SnapshotAction::Preview,
        Some("diff") => SnapshotAction::Diff,
        Some("switch") => SnapshotAction::Switch,
        _ => {
            render_usage(state, output)?;
            return Ok(true);
        }
    };
    let Some(task_id) = parts.next() else {
        render_usage(state, output)?;
        return Ok(true);
    };
    let Some(snapshot_id) = parts.next() else {
        render_usage(state, output)?;
        return Ok(true);
    };
    if parts.next().is_some() {
        render_usage(state, output)?;
        return Ok(true);
    }

    if action == SnapshotAction::Switch {
        return begin_snapshot_switch(task_id, snapshot_id, state, output);
    }
    match run_snapshot_query(action, task_id, snapshot_id) {
        Ok(value) => {
            let projection = match task_projection(&value, task_id) {
                Ok(projection) => projection,
                Err(error) => {
                    render_snapshot_error(state, output, error)?;
                    return Ok(true);
                }
            };
            render_snapshot_query_result(
                action,
                task_id,
                snapshot_id,
                &projection,
                &value,
                state,
                output,
            )?;
        }
        Err(error) => render_snapshot_error(state, output, error)?,
    }
    Ok(true)
}

fn run_snapshot_list(task_id: &str) -> Result<Value, String> {
    let output = super::run_gateway(
        &["task", "--output", "jsonl", "snapshot", "list", task_id],
        None,
    )?;
    super::json_output(&output)
}

fn run_snapshot_query(
    action: SnapshotAction,
    task_id: &str,
    snapshot_id: &str,
) -> Result<Value, String> {
    let arguments = vec![
        "task",
        "--output",
        "jsonl",
        "snapshot",
        action.gateway_argument(),
        task_id,
        snapshot_id,
    ];
    let output = super::run_gateway(&arguments, None)?;
    super::json_output(&output)
}

fn begin_snapshot_switch<W: Write>(
    task_id: &str,
    snapshot_id: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    if state.task_snapshot.pending_switch.is_some() {
        return Ok(false);
    }
    let preview = match run_snapshot_query(SnapshotAction::Preview, task_id, snapshot_id) {
        Ok(value) => value,
        Err(error) => {
            render_snapshot_error(state, output, error)?;
            return Ok(true);
        }
    };
    let projection = match terminal_task_projection(&preview, task_id) {
        Ok(projection) => projection,
        Err(error) => {
            render_snapshot_error(state, output, error)?;
            return Ok(true);
        }
    };
    let Some(preview_digest) = preview
        .get("preview_digest")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        render_snapshot_error(
            state,
            output,
            "Gateway snapshot preview did not contain a preview digest".to_owned(),
        )?;
        return Ok(true);
    };
    state.task_snapshot.pending_switch = Some(PendingSnapshotSwitch {
        panel_id: format!("task-snapshot-switch-{}", Uuid::new_v4()),
        task_id: task_id.to_owned(),
        snapshot_id: snapshot_id.to_owned(),
        task_state: projection.state,
        expected_revision: projection.revision,
        preview_digest,
        idempotency_key: format!("cosh-shell-snapshot-switch-{}", Uuid::new_v4()),
        preview_lines: change_lines(&preview),
        workspace: workspace_label(&preview),
        selected: SWITCH_CANCEL_INDEX,
    });
    render_snapshot_switch_confirmation(state, output)?;
    Ok(false)
}

pub(crate) fn pending_task_snapshot_capture(state: &InlineState) -> Option<RawInputCapture> {
    let pending = state.task_snapshot.pending_switch.as_ref()?;
    Some(RawInputCapture::Question {
        id: pending.panel_id.clone(),
        option_count: SWITCH_OPTION_COUNT,
        selected: pending.selected,
        allow_free_text: false,
        multiple: false,
        secret: false,
    })
}

pub(crate) fn render_task_snapshot_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    if state.task_snapshot.pending_switch.is_none() {
        return Ok(());
    }
    for (index, event) in events.iter().enumerate() {
        if event.kind != ShellEventKind::UserInputIntercepted
            || event.component.as_deref() != Some("card")
        {
            continue;
        }
        let event_index = event_index_base + index;
        let key = stable_event_key("task-snapshot", event_index, event);
        if !state.task_snapshot.handled_events.insert(key) {
            continue;
        }
        match event.message.as_deref() {
            Some("focus") => {
                let Some((id, selected)) = parse_id_value(event) else {
                    continue;
                };
                if !event_targets_pending_switch(state, &id) {
                    continue;
                }
                if let Some(pending) = state.task_snapshot.pending_switch.as_mut() {
                    pending.selected = selected.min(SWITCH_OPTION_COUNT - 1);
                }
                redraw_snapshot_switch_confirmation(state, output)?;
            }
            Some("answer") => {
                reserve_question_answer_event(state, event_index, event);
                submit_snapshot_switch(state, output)?;
            }
            Some("question_submit_empty") => {
                let Some(id) = event.input.as_deref().map(str::trim) else {
                    continue;
                };
                if event_targets_pending_switch(state, id) {
                    reserve_question_answer_event(state, event_index, event);
                    submit_snapshot_switch(state, output)?;
                }
            }
            Some("question_cancel") | Some("question_abort") => {
                let Some(id) = event.input.as_deref().map(str::trim) else {
                    continue;
                };
                if event_targets_pending_switch(state, id) {
                    cancel_snapshot_switch(state, output)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn submit_snapshot_switch<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state
        .task_snapshot
        .pending_switch
        .as_ref()
        .is_some_and(|pending| pending.selected != SWITCH_CONFIRM_INDEX)
    {
        return cancel_snapshot_switch(state, output);
    }
    let Some(pending) = state.task_snapshot.pending_switch.take() else {
        return Ok(());
    };
    clear_active_question_panel(state, output)?;
    if !is_terminal_task_state(&pending.task_state) {
        render_snapshot_error(
            state,
            output,
            format!(
                "Task {} is {}; switching snapshots requires a terminal Task",
                safe_field(&pending.task_id),
                safe_field(&pending.task_state)
            ),
        )?;
        state.trigger_pty_prompt = true;
        return Ok(());
    }
    render_notice_panel(
        output,
        localized(state, "Switching Task snapshot…", "正在切换 Task 快照…"),
        vec![localized(
            state,
            "Gateway is revalidating Task state, ownership, revision, and preview digest.",
            "Gateway 正在重新校验 Task 状态、归属、revision 与预览摘要。",
        )
        .to_owned()],
        None,
    )?;
    output.flush()?;
    let revision = pending.expected_revision.to_string();
    let result = super::run_gateway(
        &[
            "task",
            "--output",
            "jsonl",
            "snapshot",
            "switch",
            &pending.task_id,
            &pending.snapshot_id,
            "--preview-digest",
            &pending.preview_digest,
            "--idempotency-key",
            &pending.idempotency_key,
            "--expected-revision",
            &revision,
        ],
        None,
    )
    .and_then(|output| super::json_output(&output));
    match result {
        Ok(value) => {
            let destination = value
                .get("snapshot_id")
                .or_else(|| value.get("to"))
                .and_then(Value::as_str)
                .unwrap_or(&pending.snapshot_id);
            let recovery_snapshot = value.get("recovery_snapshot_id").and_then(Value::as_str);
            let mut lines = vec![format!("Snapshot: {}", safe_field(destination))];
            if let Some(recovery_snapshot) = recovery_snapshot {
                lines.push(format!(
                    "Recovery snapshot: {}",
                    safe_field(recovery_snapshot)
                ));
            }
            render_notice_panel(
                output,
                localized(state, "Task snapshot switched", "Task 快照已切换"),
                lines,
                Some(localized(
                    state,
                    "The workspace now reflects the selected snapshot.",
                    "工作区现已切换到所选快照。",
                )),
            )?;
        }
        Err(error) => render_snapshot_error(state, output, error)?,
    }
    state.trigger_pty_prompt = true;
    Ok(())
}

fn cancel_snapshot_switch<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_question_panel(state, output)?;
    state.task_snapshot.pending_switch = None;
    render_notice_panel(
        output,
        localized(state, "Snapshot switch cancelled", "已取消快照切换"),
        vec![localized(
            state,
            "The workspace was not changed.",
            "工作区未发生改变。",
        )
        .to_owned()],
        None,
    )?;
    state.trigger_pty_prompt = true;
    Ok(())
}

fn render_snapshot_switch_confirmation<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(pending) = state.task_snapshot.pending_switch.as_ref() else {
        return Ok(());
    };
    let english = state.language == Language::EnUs;
    let mut question = if english {
        vec![
            "Switch workspace to this Task snapshot?".to_owned(),
            format!("Task: {}", safe_field(&pending.task_id)),
            format!(
                "State: {} · revision {}",
                safe_field(&pending.task_state),
                pending.expected_revision
            ),
            format!("Snapshot: {}", safe_field(&pending.snapshot_id)),
            format!("Workspace: {}", safe_field(&pending.workspace)),
            "This replaces workspace files. It does not restore credentials, services, network, cloud, or other external effects."
                .to_owned(),
            "COSH must have been launched outside this managed workspace; cd in its embedded shell is not sufficient."
                .to_owned(),
        ]
    } else {
        vec![
            "将工作区切换到此 Task 快照吗？".to_owned(),
            format!("Task：{}", safe_field(&pending.task_id)),
            format!(
                "状态：{} · revision {}",
                safe_field(&pending.task_state),
                pending.expected_revision
            ),
            format!("快照：{}", safe_field(&pending.snapshot_id)),
            format!("工作区：{}", safe_field(&pending.workspace)),
            "此操作会替换工作区文件，但不会恢复凭据、服务、网络、云资源或其他外部影响。".to_owned(),
            "COSH 必须从此托管工作区外启动；仅在内嵌 shell 中执行 cd 不足以解除占用。".to_owned(),
        ]
    };
    question.extend(pending.preview_lines.iter().take(8).cloned());
    let question = question.join("\n");
    let options = if english {
        vec![
            format!(
                "Switch snapshot{OPTION_DETAIL_SEPARATOR}Apply this exact preview after Gateway revalidation."
            ),
            format!("Cancel{OPTION_DETAIL_SEPARATOR}Keep the workspace unchanged."),
        ]
    } else {
        vec![
            format!("切换快照{OPTION_DETAIL_SEPARATOR}Gateway 重新校验后应用此预览。"),
            format!("取消{OPTION_DETAIL_SEPARATOR}保持工作区不变。"),
        ]
    };
    let model = QuestionPanelModel {
        id: &pending.panel_id,
        question: &question,
        options: &options,
        selected_option: pending.selected,
        selected_options: &[],
        custom_answer: "",
        allow_free_text: false,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: QuestionInputFeedback::Disabled,
    };
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let cursor_row = renderer
        .active_question_cursor_placement(&model)
        .map(|placement| placement.row);
    let width = renderer.panel_standard_width();
    let presentation = if english {
        QuestionPanelPresentation::new(
            "Task snapshot",
            "Snapshot keys · ",
            "Enter activate selected action · Esc cancel · Ctrl+C cancel",
        )
    } else {
        QuestionPanelPresentation::new(
            "Task 快照",
            "快照快捷键 · ",
            "Enter 执行所选操作 · Esc 取消 · Ctrl+C 取消",
        )
    };
    let height = renderer.write_question_panel_with_presentation(output, model, presentation)?;
    state.questions.active_panel_id = Some(pending.panel_id.clone());
    state.questions.active_panel_height = height;
    state.questions.active_panel_cursor_row = cursor_row;
    state.questions.active_panel_width = Some(width);
    Ok(())
}

fn redraw_snapshot_switch_confirmation<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_question_panel(state, output)?;
    render_snapshot_switch_confirmation(state, output)
}

fn render_snapshot_query_result<W: Write>(
    action: SnapshotAction,
    task_id: &str,
    snapshot_id: &str,
    projection: &TaskProjection,
    value: &Value,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let mut lines = vec![
        format!("Task: {}", safe_field(task_id)),
        format!(
            "State: {} · revision {}",
            safe_field(&projection.state),
            projection.revision
        ),
        format!("Snapshot: {}", safe_field(snapshot_id)),
        format!("Workspace: {}", workspace_label(value)),
    ];
    lines.extend(change_lines(value));
    let title = match action {
        SnapshotAction::Preview => localized(state, "Snapshot switch preview", "快照切换预览"),
        SnapshotAction::Diff => localized(state, "Task snapshot diff", "Task 快照差异"),
        SnapshotAction::Switch => unreachable!(),
    };
    render_notice_panel(output, title, lines, None)
}

fn snapshot_list_lines(value: &Value) -> Vec<String> {
    let Some(snapshots) = value.get("snapshots").and_then(Value::as_array) else {
        return vec!["No Task-owned snapshots are available.".to_owned()];
    };
    if snapshots.is_empty() {
        return vec!["No Task-owned snapshots are available.".to_owned()];
    }
    let mut rendered = BoundedLines::default();
    for snapshot in snapshots.iter().take(MAX_SNAPSHOT_ROWS) {
        let id = snapshot
            .get("snapshot_id")
            .or_else(|| snapshot.get("checkpoint_id"))
            .or_else(|| snapshot.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let kind = snapshot
            .get("kind")
            .or_else(|| snapshot.get("source"))
            .and_then(Value::as_str);
        let state = snapshot.get("state").and_then(Value::as_str);
        let run_id = snapshot.get("run_id").and_then(Value::as_str);
        let approval_id = snapshot.get("approval_id").and_then(Value::as_str);
        let mut line = safe_field(id);
        for field in [kind, state, run_id, approval_id].into_iter().flatten() {
            line.push_str("  ");
            line.push_str(&safe_field(field));
        }
        rendered.push(line);
    }
    if snapshots.len() > MAX_SNAPSHOT_ROWS || rendered.truncated {
        rendered
            .lines
            .push("… additional snapshots omitted".to_owned());
    }
    rendered.lines
}

fn change_lines(value: &Value) -> Vec<String> {
    let changes = value.get("changes").and_then(Value::as_array).or_else(|| {
        value
            .get("preview")
            .and_then(|preview| preview.get("changes"))
            .and_then(Value::as_array)
    });
    let Some(changes) = changes else {
        return vec!["No workspace file changes were reported.".to_owned()];
    };
    if changes.is_empty() {
        return vec!["No workspace file changes.".to_owned()];
    }
    let mut rendered = BoundedLines::default();
    for change in changes.iter().take(MAX_SNAPSHOT_ROWS) {
        let path = change
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let kind = change
            .get("change_type")
            .or_else(|| change.get("change"))
            .or_else(|| change.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("changed");
        let mut line = format!("{}  {}", safe_field(kind), safe_field(path));
        if let Some(detail) = change.get("detail").and_then(Value::as_str) {
            line.push_str("  ");
            line.push_str(&safe_field(detail));
        }
        rendered.push(line);
    }
    if changes.len() > MAX_SNAPSHOT_ROWS || rendered.truncated {
        rendered
            .lines
            .push("… additional changes omitted".to_owned());
    }
    if let Some(omitted) = value.get("changes_omitted").and_then(Value::as_u64) {
        if omitted > 0 {
            rendered.lines.push(format!(
                "… Gateway omitted {omitted} additional changes; the preview digest covers the full diff."
            ));
        }
    }
    rendered.lines
}

fn parse_id_value(event: &ShellEvent) -> Option<(String, usize)> {
    let (id, selected) = event.input.as_deref()?.rsplit_once(':')?;
    Some((id.to_owned(), selected.parse().ok()?))
}

fn event_targets_pending_switch(state: &InlineState, id: &str) -> bool {
    state
        .task_snapshot
        .pending_switch
        .as_ref()
        .is_some_and(|pending| pending.panel_id == id)
}

fn reserve_question_answer_event(state: &mut InlineState, index: usize, event: &ShellEvent) {
    state
        .questions
        .handled_answers
        .insert(stable_event_key("question-answer", index, event));
}

#[cfg(test)]
mod tests;
