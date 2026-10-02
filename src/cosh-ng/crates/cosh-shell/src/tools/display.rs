use serde_json::Value;

use super::classification::{known_provider_tool, KnownProviderTool};

/// Longest tool name drawn on a status surface.
const MAX_DISPLAY_TOOL_NAME_CHARS: usize = 64;

/// A provider-supplied tool name, made safe to draw on a terminal.
///
/// Mirrors cosh-core's terminal-facing sanitizer because cosh-shell is a
/// standalone crate; keep the filtering and length contract aligned in both.
///
/// Tool names are model output, not validated identifiers: `"[2J"` in the
/// stream JSON arrives here as a real ESC byte, and the status animation writes
/// its label straight to the terminal. Stripping the control characters leaves
/// any escape body as inert text; the length cap keeps one status line from
/// flooding the surface.
pub(crate) fn display_tool_name(tool_name: &str) -> String {
    let safe: String = tool_name
        .chars()
        .filter(|character| {
            !character.is_control() && !matches!(character, '\u{2028}' | '\u{2029}')
        })
        .take(MAX_DISPLAY_TOOL_NAME_CHARS)
        .collect();
    let trimmed = safe.trim();
    if trimmed.is_empty() {
        return "tool".to_string();
    }
    let mut display = trimmed.to_string();
    if tool_name.chars().count() > MAX_DISPLAY_TOOL_NAME_CHARS {
        display.push('…');
    }
    display
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolColor {
    ReadOnly,
    Execute,
    Write,
    Dangerous,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ToolDisplayInfo {
    pub label: String,
    pub color: ToolColor,
    pub preview: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPresentationKind {
    ShellCommand,
    FileRead,
    FileWrite,
    FileEdit,
    FileSearch,
    FileGlob,
    DirectoryList,
    MultiFileRead,
    Lsp,
    WebFetch,
    WebSearch,
    Skill,
    Agent,
    Memory,
    Question,
    ShellEvidence,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolImpact {
    ReadOnly,
    Write,
    Execute,
    Destructive,
    OpenWorld,
    ContextMutation,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPresentationField {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPresentation {
    pub canonical_name: String,
    pub original_name: String,
    pub kind: ToolPresentationKind,
    pub impact: ToolImpact,
    pub target: Option<String>,
    pub secondary: Option<String>,
    pub preview: String,
    pub fields: Vec<ToolPresentationField>,
    pub raw_input_preview: Option<String>,
}

pub fn display_for_tool(name: &str, input_json: &str) -> ToolDisplayInfo {
    let presentation = presentation_for_tool(name, input_json);
    ToolDisplayInfo {
        label: presentation.canonical_name,
        color: color_for_impact(&presentation.impact),
        preview: presentation.preview,
    }
}

pub fn presentation_for_tool(name: &str, input_json: &str) -> ToolPresentation {
    let parsed = serde_json::from_str::<Value>(input_json).ok();

    match known_provider_tool(name) {
        Some(KnownProviderTool::Shell) => presentation_bash(name, &parsed, input_json),
        Some(KnownProviderTool::ReadFile) => presentation_read(name, &parsed, input_json),
        Some(KnownProviderTool::WriteFile) => presentation_write(name, &parsed, input_json),
        Some(KnownProviderTool::Edit) => presentation_edit(name, "Edit", &parsed, input_json),
        Some(KnownProviderTool::NotebookEdit) => {
            presentation_edit(name, "Notebook edit", &parsed, input_json)
        }
        Some(KnownProviderTool::Grep) => presentation_grep(name, &parsed, input_json),
        Some(KnownProviderTool::Glob) => presentation_glob(name, &parsed, input_json),
        Some(KnownProviderTool::ListDirectory) => presentation_ls(name, &parsed, input_json),
        Some(KnownProviderTool::ReadManyFiles) => {
            presentation_many_files(name, &parsed, input_json)
        }
        Some(KnownProviderTool::Lsp) => presentation_lsp(name, &parsed, input_json),
        Some(KnownProviderTool::WebFetch) => presentation_web_fetch(name, &parsed, input_json),
        Some(KnownProviderTool::WebSearch) => presentation_web_search(name, &parsed, input_json),
        Some(KnownProviderTool::Skill) => presentation_skill(name, &parsed, input_json),
        Some(
            tool @ (KnownProviderTool::Agent
            | KnownProviderTool::Workflow
            | KnownProviderTool::SendMessage
            | KnownProviderTool::Task
            | KnownProviderTool::Subagent
            | KnownProviderTool::Delegate),
        ) => presentation_agent(name, agent_label(tool), &parsed, input_json),
        Some(
            tool @ (KnownProviderTool::SaveMemory
            | KnownProviderTool::Todo
            | KnownProviderTool::TodoWrite
            | KnownProviderTool::TaskCreate
            | KnownProviderTool::TaskUpdate
            | KnownProviderTool::TaskList
            | KnownProviderTool::TaskGet
            | KnownProviderTool::TaskStop
            | KnownProviderTool::CronCreate
            | KnownProviderTool::CronDelete
            | KnownProviderTool::CronList
            | KnownProviderTool::ScheduleWakeup),
        ) => {
            let (label, receipt) = memory_tool_receipt(tool);
            presentation_memory(name, label, receipt, &parsed, input_json)
        }
        Some(KnownProviderTool::AskUserQuestion) => {
            presentation_question(name, &parsed, input_json)
        }
        Some(KnownProviderTool::ShellEvidence) => {
            presentation_shell_evidence(name, &parsed, input_json)
        }
        None => presentation_custom(name, &parsed, input_json),
    }
}

fn base_presentation(
    original_name: &str,
    canonical_name: &str,
    kind: ToolPresentationKind,
    impact: ToolImpact,
    target: Option<String>,
    secondary: Option<String>,
    preview: String,
    raw_input_preview: Option<String>,
) -> ToolPresentation {
    ToolPresentation {
        canonical_name: canonical_name.to_string(),
        original_name: original_name.to_string(),
        kind,
        impact,
        target,
        secondary,
        preview,
        fields: Vec::new(),
        raw_input_preview,
    }
}

fn color_for_impact(impact: &ToolImpact) -> ToolColor {
    match impact {
        ToolImpact::ReadOnly | ToolImpact::OpenWorld => ToolColor::ReadOnly,
        ToolImpact::Write | ToolImpact::ContextMutation => ToolColor::Write,
        ToolImpact::Execute => ToolColor::Execute,
        ToolImpact::Destructive => ToolColor::Dangerous,
        ToolImpact::Unknown => ToolColor::Unknown,
    }
}

fn presentation_bash(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let command = parsed
        .as_ref()
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or(input_json);

    let impact = if is_dangerous_command(command) {
        ToolImpact::Destructive
    } else {
        ToolImpact::Execute
    };

    base_presentation(
        original_name,
        "Bash",
        ToolPresentationKind::ShellCommand,
        impact,
        Some(format!("$ {command}")),
        None,
        format!("$ {command}"),
        malformed_raw_preview(parsed, input_json),
    )
}

fn is_dangerous_command(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    tokens
        .iter()
        .any(|t| *t == "sudo" || *t == "rm" || *t == "kill")
}

fn presentation_read(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let file_path = str_field(parsed, "file_path")
        .or_else(|| str_field(parsed, "path"))
        .unwrap_or(input_json);

    let mut preview = file_path.to_string();
    if let Some(offset) = parsed
        .as_ref()
        .and_then(|v| v.get("offset"))
        .and_then(|v| v.as_u64())
    {
        let limit = parsed
            .as_ref()
            .and_then(|v| v.get("limit"))
            .and_then(|v| v.as_u64());
        match limit {
            Some(l) => preview = format!("{file_path} (lines {offset}..+{l})"),
            None => preview = format!("{file_path} (from line {offset})"),
        }
    }

    let target = if file_path.starts_with("terminal-output://") {
        Some("Shell output bookmark".to_string())
    } else {
        Some(file_path.to_string())
    };
    base_presentation(
        original_name,
        "Read",
        ToolPresentationKind::FileRead,
        ToolImpact::ReadOnly,
        target,
        None,
        preview,
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_write(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let file_path = str_field(parsed, "file_path")
        .or_else(|| str_field(parsed, "path"))
        .unwrap_or(input_json);

    base_presentation(
        original_name,
        "Write",
        ToolPresentationKind::FileWrite,
        ToolImpact::Write,
        Some(file_path.to_string()),
        Some("new file".to_string()),
        format!("{file_path} (new file)"),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_edit(
    original_name: &str,
    canonical_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let file_path = str_field(parsed, "file_path")
        .or_else(|| str_field(parsed, "path"))
        .unwrap_or(input_json);
    let old = str_field(parsed, "old_string").unwrap_or("");
    let new = str_field(parsed, "new_string").unwrap_or("");

    let old_short = truncate(old, 30);
    let new_short = truncate(new, 30);
    let diff = if !old_short.is_empty() || !new_short.is_empty() {
        format!(" ({old_short} -> {new_short})")
    } else {
        String::new()
    };

    base_presentation(
        original_name,
        canonical_name,
        ToolPresentationKind::FileEdit,
        ToolImpact::Write,
        Some(file_path.to_string()),
        if diff.is_empty() {
            None
        } else {
            Some(diff.trim().trim_matches(['(', ')']).to_string())
        },
        format!("{file_path}{diff}"),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_grep(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let pattern = str_field(parsed, "pattern")
        .or_else(|| str_field(parsed, "query"))
        .unwrap_or("?");
    let path = str_field(parsed, "path").unwrap_or(input_json);

    base_presentation(
        original_name,
        "Grep",
        ToolPresentationKind::FileSearch,
        ToolImpact::ReadOnly,
        Some(format!("\"{pattern}\" in {path}")),
        None,
        format!("/{pattern}/ in {path}"),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_glob(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let pattern = str_field(parsed, "pattern").unwrap_or(input_json);

    base_presentation(
        original_name,
        "Glob",
        ToolPresentationKind::FileGlob,
        ToolImpact::ReadOnly,
        Some(pattern.to_string()),
        None,
        pattern.to_string(),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_ls(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let path = str_field(parsed, "path")
        .or_else(|| str_field(parsed, "dir_path"))
        .unwrap_or(input_json);

    base_presentation(
        original_name,
        "LS",
        ToolPresentationKind::DirectoryList,
        ToolImpact::ReadOnly,
        Some(path.to_string()),
        None,
        path.to_string(),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_many_files(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let paths = string_array_field(parsed, "paths");
    let (target, preview, secondary, fields) = if paths.is_empty() {
        (
            "multiple files".to_string(),
            compact_json(input_json),
            None,
            Vec::new(),
        )
    } else {
        let visible = paths
            .iter()
            .take(20)
            .map(|path| truncate(path, 80))
            .collect::<Vec<_>>();
        let omitted = paths.len().saturating_sub(visible.len());
        let mut fields = visible
            .iter()
            .map(|path| ToolPresentationField {
                label: "path".to_string(),
                value: path.clone(),
            })
            .collect::<Vec<_>>();
        if omitted > 0 {
            fields.push(ToolPresentationField {
                label: "omitted_paths".to_string(),
                value: omitted.to_string(),
            });
        }
        let preview = if omitted > 0 {
            format!("{}; +{} more", visible.join("; "), omitted)
        } else {
            visible.join("; ")
        };
        (
            format!("{} files", paths.len()),
            preview,
            (omitted > 0).then(|| format!("showing first 20; {omitted} more")),
            fields,
        )
    };

    let mut presentation = base_presentation(
        original_name,
        "Read",
        ToolPresentationKind::MultiFileRead,
        ToolImpact::ReadOnly,
        Some(target),
        secondary,
        preview,
        malformed_raw_preview(parsed, input_json),
    );
    presentation.fields = fields;
    presentation
}

fn presentation_lsp(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let operation = str_field(parsed, "operation").unwrap_or("unknown");
    let file_path = str_field(parsed, "filePath").unwrap_or("?");
    let line = parsed
        .as_ref()
        .and_then(|v| v.get("line"))
        .and_then(|v| v.as_u64())
        .map(|l| l.to_string())
        .unwrap_or_else(|| "?".to_string());

    base_presentation(
        original_name,
        &format!("LSP {operation}"),
        ToolPresentationKind::Lsp,
        ToolImpact::ReadOnly,
        Some(format!("{file_path}:{line}")),
        Some(operation.to_string()),
        format!("{file_path}:{line}"),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_web_fetch(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let url = str_field(parsed, "url").unwrap_or(input_json);

    base_presentation(
        original_name,
        "WebFetch",
        ToolPresentationKind::WebFetch,
        ToolImpact::OpenWorld,
        Some(url.to_string()),
        None,
        url.to_string(),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_web_search(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let query = str_field(parsed, "query").unwrap_or(input_json);

    base_presentation(
        original_name,
        "WebSearch",
        ToolPresentationKind::WebSearch,
        ToolImpact::OpenWorld,
        Some(format!("\"{query}\"")),
        None,
        format!("\"{query}\""),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_skill(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let skill = str_field(parsed, "skill")
        .or_else(|| str_field(parsed, "skill_name"))
        .or_else(|| str_field(parsed, "name"));
    let action = str_field(parsed, "action");
    let target = skill
        .or(action)
        .map(ToString::to_string)
        .unwrap_or_else(|| "skill".to_string());
    let mut presentation = base_presentation(
        original_name,
        "Skill",
        ToolPresentationKind::Skill,
        ToolImpact::ContextMutation,
        Some(target.clone()),
        None,
        target,
        malformed_raw_preview(parsed, input_json),
    );
    if let Some(action) = action {
        presentation.fields.push(ToolPresentationField {
            label: "action".to_string(),
            value: action.to_string(),
        });
    }
    presentation
}

fn presentation_agent(
    original_name: &str,
    canonical_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let agent = str_field(parsed, "prompt")
        .or_else(|| str_field(parsed, "task"))
        .or_else(|| str_field(parsed, "description"))
        .or_else(|| str_field(parsed, "agent"))
        .or_else(|| str_field(parsed, "subagent"))
        .or_else(|| str_field(parsed, "name"))
        .unwrap_or("agent");
    base_presentation(
        original_name,
        canonical_name,
        ToolPresentationKind::Agent,
        ToolImpact::ContextMutation,
        Some(truncate(agent, 80)),
        None,
        truncate(agent, 80),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_memory(
    original_name: &str,
    canonical_name: &str,
    receipt: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let target = str_field(parsed, "task_id")
        .or_else(|| str_field(parsed, "todo_id"))
        .or_else(|| str_field(parsed, "cron_id"))
        .or_else(|| str_field(parsed, "schedule_id"))
        .or_else(|| str_field(parsed, "title"))
        .or_else(|| str_field(parsed, "name"))
        .or_else(|| str_field(parsed, "fact"))
        .or_else(|| str_field(parsed, "time"))
        .unwrap_or("context");
    base_presentation(
        original_name,
        canonical_name,
        ToolPresentationKind::Memory,
        ToolImpact::ContextMutation,
        Some(truncate(target, 80)),
        Some(receipt.to_string()),
        truncate(target, 80),
        malformed_raw_preview(parsed, input_json),
    )
}

fn agent_label(tool: KnownProviderTool) -> &'static str {
    match tool {
        KnownProviderTool::Task => "Task",
        KnownProviderTool::Subagent => "Subagent",
        KnownProviderTool::Delegate => "Delegate",
        _ => "Agent",
    }
}

fn memory_tool_receipt(tool: KnownProviderTool) -> (&'static str, &'static str) {
    match tool {
        KnownProviderTool::SaveMemory => ("Memory", "saved"),
        KnownProviderTool::Todo | KnownProviderTool::TodoWrite => ("Todo", "updated"),
        KnownProviderTool::TaskCreate => ("Task", "created"),
        KnownProviderTool::TaskUpdate => ("Task", "updated"),
        KnownProviderTool::TaskList => ("Task", "listed"),
        KnownProviderTool::TaskGet => ("Task", "loaded"),
        KnownProviderTool::TaskStop => ("Task", "stopped"),
        KnownProviderTool::CronCreate => ("Cron", "created"),
        KnownProviderTool::CronDelete => ("Cron", "deleted"),
        KnownProviderTool::CronList => ("Cron", "listed"),
        KnownProviderTool::ScheduleWakeup => ("Wakeup", "scheduled"),
        _ => unreachable!("only memory identities reach receipt rendering"),
    }
}

fn presentation_question(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let question = str_field(parsed, "question").unwrap_or("question");
    base_presentation(
        original_name,
        "Question",
        ToolPresentationKind::Question,
        ToolImpact::Unknown,
        Some(question.to_string()),
        None,
        question.to_string(),
        malformed_raw_preview(parsed, input_json),
    )
}

fn presentation_shell_evidence(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let action = str_field(parsed, "action").unwrap_or("evidence");
    let command = str_field(parsed, "command").filter(|value| *value != "<none>");
    let command_label = str_field(parsed, "command_label").filter(|value| *value != "<none>");
    let preview = match action {
        "list_commands" => "command history",
        "read_output" => "shell output excerpt",
        "already_delivered" => "already delivered shell evidence",
        _ => "shell evidence",
    }
    .to_string();
    let mut presentation = base_presentation(
        original_name,
        "Evidence",
        ToolPresentationKind::ShellEvidence,
        ToolImpact::ReadOnly,
        command_label
            .map(ToString::to_string)
            .or_else(|| {
                command.map(|command| {
                    if command.starts_with('$') {
                        command.to_string()
                    } else {
                        format!("$ {command}")
                    }
                })
            })
            .or(Some(preview.clone())),
        None,
        preview,
        malformed_raw_preview(parsed, input_json),
    );
    presentation.fields.push(ToolPresentationField {
        label: "action".to_string(),
        value: action.to_string(),
    });
    for key in [
        "command",
        "output_id",
        "command_label",
        "command_count",
        "has_more",
        "direction",
        "lines",
        "status",
        "reason",
        "duplicate_provider_request",
    ] {
        if let Some(value) = str_field(parsed, key) {
            if value == "<none>" {
                continue;
            }
            presentation.fields.push(ToolPresentationField {
                label: key.to_string(),
                value: value.to_string(),
            });
        }
    }
    presentation
}

fn presentation_custom(
    original_name: &str,
    parsed: &Option<Value>,
    input_json: &str,
) -> ToolPresentation {
    let mut fields = Vec::new();
    if let Some((server, tool)) = mcp_server_tool(original_name) {
        fields.push(ToolPresentationField {
            label: "server".to_string(),
            value: server.to_string(),
        });
        fields.push(ToolPresentationField {
            label: "tool".to_string(),
            value: tool.to_string(),
        });
    }
    for key in [
        "path",
        "file_path",
        "command",
        "query",
        "url",
        "name",
        "title",
        "action",
        "operation",
    ] {
        if fields.len() >= 3 {
            break;
        }
        if let Some(value) = str_field(parsed, key) {
            fields.push(ToolPresentationField {
                label: key.to_string(),
                value: truncate(value, 80),
            });
        }
    }
    let preview = if fields.is_empty() {
        match parsed {
            Some(Value::Object(_)) | Some(Value::Array(_)) => "input: structured payload",
            Some(_) | None => "input: opaque payload",
        }
        .to_string()
    } else {
        fields
            .iter()
            .map(|field| format!("{}: {}", field.label, field.value))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let mut presentation = base_presentation(
        original_name,
        original_name,
        ToolPresentationKind::Custom,
        ToolImpact::Unknown,
        Some(preview.clone()),
        None,
        preview,
        Some(compact_json(input_json)),
    );
    presentation.fields = fields;
    presentation
}

fn str_field<'a>(parsed: &'a Option<Value>, key: &str) -> Option<&'a str> {
    parsed.as_ref()?.get(key)?.as_str()
}

fn string_array_field(parsed: &Option<Value>, key: &str) -> Vec<String> {
    parsed
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.lines().next().unwrap_or(s);
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        format!("{}...", trimmed.chars().take(max).collect::<String>())
    }
}

fn compact_json(input: &str) -> String {
    match serde_json::from_str::<Value>(input) {
        Ok(v) => {
            let s = v.to_string();
            truncate(&s, 120)
        }
        Err(_) => truncate(input, 120),
    }
}

fn malformed_raw_preview(parsed: &Option<Value>, input_json: &str) -> Option<String> {
    if parsed.is_none() {
        Some(truncate(input_json, 120))
    } else {
        None
    }
}

fn mcp_server_tool(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        None
    } else {
        Some((server, tool))
    }
}

#[cfg(test)]
mod tests;
