//! Bounded, terminal-safe rendering helpers for Task snapshots.

use std::io::Write;

use serde_json::Value;

use crate::runtime::state::InlineState;
use crate::slash::panel::render_notice_panel;

const MAX_SNAPSHOT_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct BoundedLines {
    pub(super) lines: Vec<String>,
    bytes: usize,
    pub(super) truncated: bool,
}

impl BoundedLines {
    pub(super) fn push(&mut self, line: String) {
        let remaining = MAX_SNAPSHOT_OUTPUT_BYTES.saturating_sub(self.bytes);
        if remaining == 0 {
            self.truncated = true;
            return;
        }
        let mut boundary = line.len().min(remaining);
        while !line.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        self.lines.push(line[..boundary].to_owned());
        self.bytes = self.bytes.saturating_add(boundary);
        self.truncated |= boundary < line.len();
    }
}

pub(super) fn render_usage<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    render_notice_panel(
        output,
        localized(state, "Task snapshots", "Task 快照"),
        vec![
            "/task snapshots [task-id]".to_owned(),
            "/task snapshot preview <task-id> <snapshot-id>".to_owned(),
            "/task snapshot diff <task-id> <snapshot-id>".to_owned(),
            "/task snapshot switch <task-id> <snapshot-id>".to_owned(),
        ],
        None,
    )
}

pub(super) fn render_snapshot_error<W: Write>(
    state: &InlineState,
    output: &mut W,
    error: String,
) -> std::io::Result<()> {
    let lower = error.to_ascii_lowercase();
    let safe_error = safe_field(&error);
    let cwd_occupied = lower.contains("cwd")
        && (lower.contains("occupied")
            || lower.contains("inside workspace")
            || lower.contains("working directory"));
    if cwd_occupied {
        return render_notice_panel(
            output,
            localized(state, "Snapshot switch blocked", "快照切换被阻止"),
            vec![
                safe_error,
                localized(
                    state,
                    "A process has its cwd inside the workspace. cd in the embedded shell does not move cosh-shell itself. Exit this COSH session, stop other occupants, restart COSH from outside the workspace, preview again, and use a fresh switch attempt. Preview and diff remain safe.",
                    "有进程的 cwd 位于工作区内。在内嵌 shell 中 cd 不会移动 cosh-shell 自身。请退出整个 COSH 会话并停止其他占用进程，从工作区外重新启动 COSH，再次预览后发起全新的切换。预览和差异查看仍可安全使用。",
                )
                .to_owned(),
            ],
            None,
        );
    }
    render_notice_panel(
        output,
        localized(state, "Task snapshots unavailable", "Task 快照暂不可用"),
        vec![safe_error],
        Some(localized(
            state,
            "Check the local Gateway and snapshot provider, then retry.",
            "请检查本机 Gateway 与快照 Provider 后重试。",
        )),
    )
}

pub(super) fn safe_field(value: &str) -> String {
    super::super::safe_task_field(value)
}

pub(super) fn workspace_label(value: &Value) -> String {
    value
        .get("workspace")
        .and_then(|workspace| workspace.get("display_name"))
        .and_then(Value::as_str)
        .map(safe_field)
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(super) fn localized<'a>(state: &InlineState, english: &'a str, chinese: &'a str) -> &'a str {
    super::super::localized(state, english, chinese)
}
