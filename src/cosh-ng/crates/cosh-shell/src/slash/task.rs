//! Minimal persistent Task entrypoint backed by the local Gateway CLI.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use uuid::Uuid;

use crate::config::Language;
use crate::runtime::prelude::InlineState;
use crate::slash::panel::render_notice_panel;

mod form;
mod snapshot;
#[cfg(test)]
mod tests;

pub(crate) use form::{pending_task_form_capture, render_task_form_actions, TaskFormState};
pub(crate) use snapshot::{
    pending_task_snapshot_capture, render_task_snapshot_actions, TaskSnapshotState,
};

const TASK_LIST_LIMIT: &str = "20";
const TASK_EVENT_LIMIT: &str = "64";
const MAX_TASK_EVENT_PAGES: usize = 256;
const MAX_RENDERED_RESULT_BYTES: usize = 64 * 1024;
const MAX_RENDERED_TASK_FIELD_BYTES: usize = 1024;
const MAX_TASK_GOAL_BYTES: usize = 256 * 1024;

pub(super) fn render_task_command<W: Write>(
    arguments: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return form::open_task_form(state, String::new(), output).map(|opened| !opened);
    }
    if trimmed == "snapshots" || trimmed.starts_with("snapshots ") {
        return snapshot::render_snapshot_list_command(trimmed, state, output);
    }
    if trimmed == "snapshot" || trimmed.starts_with("snapshot ") {
        return snapshot::render_snapshot_command(trimmed, state, output);
    }
    if !matches!(trimmed, "list" | "show") && !trimmed.starts_with("show ") {
        return form::open_task_form(state, trimmed.to_owned(), output).map(|opened| !opened);
    }

    let result = dispatch_task_query(trimmed);
    let (title, body, footer) = match result {
        Ok(TaskDisplay::List(lines)) => (
            localized(state, "Persistent Tasks", "持久 Tasks"),
            lines,
            Some(localized(
                state,
                "Submit: /task <goal> · Details: /task show [task-id] · Snapshots: /task snapshots [task-id]",
                "提交：/task <目标> · 详情：/task show [task-id] · 快照：/task snapshots [task-id]",
            )),
        ),
        Ok(TaskDisplay::Detail(lines)) => (
            localized(state, "Task result", "Task 结果"),
            lines,
            Some(localized(
                state,
                "Rebuilt from durable events · Snapshots: /task snapshots [task-id]",
                "由持久事件重建 · 快照：/task snapshots [task-id]",
            )),
        ),
        Err(error) => (
            localized(state, "Task unavailable", "Task 暂不可用"),
            vec![safe_text(&error)],
            Some(localized(
                state,
                "Make sure the unified local Gateway service is running.",
                "请确认统一的本机 Gateway 服务正在运行。",
            )),
        ),
    };
    render_notice_panel(output, title, body, footer)?;
    Ok(true)
}

enum TaskDisplay {
    List(Vec<String>),
    Detail(Vec<String>),
}

fn dispatch_task_query(trimmed: &str) -> Result<TaskDisplay, String> {
    if trimmed == "list" {
        return list_tasks().map(TaskDisplay::List);
    }
    if trimmed == "show" {
        let task_id = latest_task_id()?;
        return show_task(&task_id).map(TaskDisplay::Detail);
    }
    if let Some(task_id) = trimmed.strip_prefix("show ").map(str::trim) {
        if task_id.is_empty() || task_id.split_whitespace().count() != 1 {
            return Err("usage: /task show [task-id]".to_owned());
        }
        return show_task(task_id).map(TaskDisplay::Detail);
    }
    Err("usage: /task [list|show [task-id]|<goal>]".to_owned())
}

fn submit_task(
    goal: &str,
    runtime: form::TaskRuntime,
    checkpoint: form::TaskCheckpoint,
    expected_workspace_digest: &str,
) -> Result<String, String> {
    if goal.len() > MAX_TASK_GOAL_BYTES {
        return Err("Task goal exceeds the 256 KiB limit".to_owned());
    }
    let key = format!("cosh-shell-task-{}", Uuid::new_v4());
    let output = run_gateway(
        &[
            "task",
            "--output",
            "jsonl",
            "submit",
            "--idempotency-key",
            &key,
            "--runtime",
            runtime.gateway_argument(),
            "--checkpoint",
            checkpoint.gateway_argument(),
            "--approval-policy",
            "allow-all",
            "--expected-workspace-digest",
            expected_workspace_digest,
        ],
        Some(goal.as_bytes()),
    )?;
    let value = json_output(&output)?;
    value
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Gateway response did not contain a Task ID".to_owned())
}

fn task_capabilities() -> Result<form::TaskCapabilities, String> {
    let output = run_gateway(&["task", "--output", "jsonl", "capabilities"], None)?;
    let value = json_output(&output)?;
    form::parse_task_capabilities(&value)
}

fn render_submission_result<W: Write>(
    result: Result<String, String>,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let (title, body, footer) = match result {
        Ok(task_id) => (
            localized(state, "Persistent Task submitted", "持久 Task 已提交"),
            vec![
                format!("Task: {}", safe_task_field(&task_id)),
                localized(
                    state,
                    "The Task continues under the local Gateway after this SSH session exits.",
                    "退出当前 SSH 后，Task 仍由本机 Gateway 托管。",
                )
                .to_owned(),
            ],
            Some(localized(
                state,
                "Use /task show <task-id> for durable progress and results.",
                "使用 /task show <task-id> 查看持久进度与结果。",
            )),
        ),
        Err(error) => (
            localized(state, "Persistent Task not submitted", "持久 Task 未提交"),
            vec![safe_task_field(&error)],
            Some(localized(
                state,
                "The Task was not submitted. Check the local Gateway and try again.",
                "Task 未提交。请检查本机 Gateway 后重试。",
            )),
        ),
    };
    render_notice_panel(output, title, body, footer)
}

fn render_submission_progress<W: Write>(
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    render_notice_panel(
        output,
        localized(state, "Submitting persistent Task…", "正在提交持久 Task…"),
        vec![localized(
            state,
            "Recording the launch policy with the local Gateway.",
            "正在向本机 Gateway 记录启动策略。",
        )
        .to_owned()],
        None,
    )
}

fn list_tasks() -> Result<Vec<String>, String> {
    let output = run_gateway(
        &[
            "task",
            "--output",
            "jsonl",
            "list",
            "--limit",
            TASK_LIST_LIMIT,
        ],
        None,
    )?;
    let value = json_output(&output)?;
    let tasks = value
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| "Gateway response did not contain a Task list".to_owned())?;
    if tasks.is_empty() {
        return Ok(vec!["No persistent Tasks yet.".to_owned()]);
    }
    Ok(tasks
        .iter()
        .filter_map(|task| {
            let launch = task.get("launch");
            let launch_summary = launch.and_then(|launch| {
                let runtime = launch.get("runtime").and_then(Value::as_str)?;
                let checkpoint = launch.get("checkpoint").and_then(Value::as_str)?;
                Some(format!("  {runtime}/{checkpoint}"))
            });
            Some(format!(
                "{}  {}{}  revision {}",
                task.get("task_id")?.as_str()?,
                task.get("state")?.as_str()?,
                launch_summary.as_deref().unwrap_or_default(),
                task.get("revision")?.as_u64()?
            ))
        })
        .collect())
}

fn latest_task_id() -> Result<String, String> {
    let output = run_gateway(&["task", "--output", "jsonl", "list", "--limit", "1"], None)?;
    let value = json_output(&output)?;
    value
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|tasks| tasks.first())
        .and_then(|task| task.get("task_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "No persistent Task is available".to_owned())
}

fn show_task(task_id: &str) -> Result<Vec<String>, String> {
    let projection = run_gateway(&["task", "--output", "jsonl", "get", task_id], None)?;
    let projection = json_output(&projection)?;
    let mut lines = vec![format!(
        "{}  {}  revision {}",
        projection
            .get("task_id")
            .and_then(Value::as_str)
            .unwrap_or(task_id),
        projection
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        projection
            .get("revision")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    )];
    if let Some(launch) = projection.get("launch") {
        let runtime = launch
            .get("runtime")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let checkpoint = launch
            .get("checkpoint")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let approval = launch
            .get("approval")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        lines.push(format!(
            "Runtime {runtime}  checkpoint {checkpoint}  approval {approval}"
        ));
    }
    if let Some(baseline) = projection.get("baseline") {
        let state = baseline
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let mut summary = format!("Baseline {state}");
        if let Some(reason) = baseline.get("reason").and_then(Value::as_str) {
            summary.push_str(": ");
            summary.push_str(&safe_text(reason));
        }
        lines.push(summary);
    }
    let mut cursor = 0_u64;
    let mut transcript = TaskTranscript::default();
    let mut truncated = false;
    for page_index in 0..MAX_TASK_EVENT_PAGES {
        let cursor_text = cursor.to_string();
        let events = run_gateway(
            &[
                "task",
                "--output",
                "jsonl",
                "events",
                task_id,
                "--after",
                &cursor_text,
                "--limit",
                TASK_EVENT_LIMIT,
            ],
            None,
        )?;
        let events = json_output(&events)?;
        if let Some(items) = events.get("events").and_then(Value::as_array) {
            for item in items {
                let Some(event) = item.get("event") else {
                    continue;
                };
                transcript.record(event);
            }
        }
        let has_more = events.get("has_more").and_then(Value::as_bool) == Some(true);
        let next = events
            .get("next_revision")
            .and_then(Value::as_u64)
            .unwrap_or(cursor);
        if !has_more {
            break;
        }
        if next <= cursor {
            truncated = true;
            break;
        }
        if page_index + 1 == MAX_TASK_EVENT_PAGES {
            truncated = true;
            break;
        }
        cursor = next;
    }
    let (transcript_lines, transcript_truncated) = transcript.finish();
    lines.extend(transcript_lines);
    if truncated || transcript_truncated {
        lines.insert(
            1,
            "Some output was omitted; the durable event stream remains available.".to_owned(),
        );
    }
    Ok(lines)
}

#[derive(Default)]
struct TaskTranscript {
    lines: Vec<String>,
    pending: String,
    pending_run: Option<String>,
    rendered_bytes: usize,
    truncated: bool,
}

impl TaskTranscript {
    fn record(&mut self, event: &Value) {
        let event_name = event.get("event").and_then(Value::as_str).unwrap_or("");
        let update = event.get("update");
        let is_progress = event_name == "runtime_event_recorded"
            && update
                .and_then(|value| value.get("update"))
                .and_then(Value::as_str)
                == Some("progress");
        if is_progress {
            if let Some(summary) = update
                .and_then(|value| value.get("summary"))
                .and_then(Value::as_str)
            {
                let run_id = event.get("run_id").and_then(Value::as_str);
                if !self.pending.is_empty() && self.pending_run.as_deref() != run_id {
                    self.flush_progress();
                }
                self.pending_run = run_id.map(str::to_owned);
                self.append_progress(summary);
            }
            return;
        }

        self.flush_progress();
        if matches!(
            event_name,
            "task_succeeded" | "task_failed" | "task_cancelled" | "run_suspended"
        ) {
            self.lines.push(event_name.replace('_', " "));
        }
    }

    fn append_progress(&mut self, summary: &str) {
        let summary = safe_text(summary);
        let remaining = MAX_RENDERED_RESULT_BYTES.saturating_sub(self.rendered_bytes);
        if remaining == 0 {
            self.truncated |= !summary.is_empty();
            return;
        }
        let mut boundary = summary.len().min(remaining);
        while !summary.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        self.pending.push_str(&summary[..boundary]);
        self.rendered_bytes = self.rendered_bytes.saturating_add(boundary);
        self.truncated |= boundary < summary.len();
    }

    fn flush_progress(&mut self) {
        let paragraph = self
            .pending
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !paragraph.is_empty() {
            self.lines.push(paragraph);
        }
        self.pending.clear();
        self.pending_run = None;
    }

    fn finish(mut self) -> (Vec<String>, bool) {
        self.flush_progress();
        (self.lines, self.truncated)
    }
}

fn run_gateway(arguments: &[&str], stdin: Option<&[u8]>) -> Result<Output, String> {
    let program = gateway_executable()?;
    let mut command = Command::new(program);
    if arguments.first() == Some(&"task") {
        command.arg("task");
        if let Some(socket) = configured_gateway_socket()? {
            command.arg("--socket").arg(socket);
        }
        command.args(&arguments[1..]);
    } else {
        command.args(arguments);
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start cosh-gateway: {error}"))?;
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .ok_or_else(|| "Gateway stdin was unavailable".to_owned())?
            .write_all(input)
            .map_err(|error| format!("failed to submit Task goal: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for cosh-gateway: {error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        let message = String::from_utf8_lossy(&output.stderr);
        Err(if message.trim().is_empty() {
            format!("cosh-gateway exited with {}", output.status)
        } else {
            message.trim().to_owned()
        })
    }
}

fn configured_gateway_socket() -> Result<Option<PathBuf>, String> {
    if let Some(configured) = std::env::var_os("COSH_GATEWAY_SOCKET") {
        let configured = PathBuf::from(configured);
        if !configured.is_absolute() {
            return Err("COSH_GATEWAY_SOCKET must be absolute".to_owned());
        }
        return Ok(Some(configured));
    }
    let Some(user) = std::env::var_os("USER") else {
        return Ok(None);
    };
    let user = user
        .to_str()
        .filter(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
        .ok_or_else(|| "USER cannot name a Gateway runtime directory".to_owned())?;
    let packaged = PathBuf::from(format!("/run/cosh-gateway-{user}/gateway.sock"));
    Ok(packaged.exists().then_some(packaged))
}

fn gateway_executable() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("COSH_GATEWAY_EXECUTABLE") {
        let configured = PathBuf::from(configured);
        if !configured.is_absolute() {
            return Err("COSH_GATEWAY_EXECUTABLE must be absolute".to_owned());
        }
        return Ok(configured);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            let sibling = parent.join("cosh-gateway");
            if is_executable_file(&sibling) {
                return Ok(sibling);
            }
        }
    }
    Ok(PathBuf::from("cosh-gateway"))
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn json_output(output: &Output) -> Result<Value, String> {
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| "Gateway returned non-UTF-8 output".to_owned())?;
    text.lines()
        .rev()
        .find_map(|line| serde_json::from_str(line).ok())
        .ok_or_else(|| "Gateway returned no JSON result".to_owned())
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>()
        .replace(['\n', '\t'], " ")
}

fn safe_task_field(value: &str) -> String {
    let value = safe_text(value);
    if value.len() <= MAX_RENDERED_TASK_FIELD_BYTES {
        return value;
    }
    let mut boundary = MAX_RENDERED_TASK_FIELD_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    format!("{}…", &value[..boundary])
}

fn localized<'a>(state: &InlineState, english: &'a str, chinese: &'a str) -> &'a str {
    match state.language {
        Language::EnUs => english,
        Language::ZhCn => chinese,
    }
}
