use crate::agent::run::ActiveAgentRun;
use crate::runtime::prelude::*;
use crate::tools::display::{presentation_for_tool, ToolPresentationKind};

#[derive(Debug, Clone)]
struct PendingTool {
    key: String,
    name: String,
    kind: ToolPresentationKind,
}

#[cfg(test)]
pub(super) fn pending_tool_status_detail<'a>(
    language: Language,
    events: impl Iterator<Item = &'a GovernedEvent>,
) -> Option<String> {
    pending_tool_status_detail_with_completed(language, events, std::iter::empty())
}

pub(super) fn pending_tool_status_detail_for_run(active_run: &ActiveAgentRun) -> Option<String> {
    pending_tool_status_detail_with_completed(
        active_run.language,
        active_run.governed_events.iter(),
        active_run
            .host_completed_tool_ids
            .iter()
            .map(String::as_str),
    )
}

pub(super) fn pending_tool_status_detail_with_completed<'a, 'b>(
    language: Language,
    events: impl Iterator<Item = &'a GovernedEvent>,
    completed_tool_ids: impl Iterator<Item = &'b str>,
) -> Option<String> {
    let mut pending = Vec::new();
    for event in events {
        match &event.event {
            AgentEvent::ToolCall {
                tool_id,
                name,
                input,
                ..
            } => {
                let presentation = presentation_for_tool(name, input);
                if matches!(presentation.kind, ToolPresentationKind::Question) {
                    continue;
                }
                upsert_pending_tool(
                    &mut pending,
                    PendingTool {
                        key: tool_id.clone().unwrap_or_else(|| name.to_string()),
                        name: name.to_string(),
                        kind: presentation.kind,
                    },
                );
            }
            AgentEvent::ToolPermissionRequest {
                request_id,
                tool_name,
                tool_input,
                tool_use_id,
                ..
            } => {
                let presentation = presentation_for_tool(tool_name, &tool_input.to_string());
                if matches!(presentation.kind, ToolPresentationKind::Question) {
                    continue;
                }
                upsert_pending_tool(
                    &mut pending,
                    PendingTool {
                        key: control_tool_key(tool_use_id, request_id),
                        name: tool_name.to_string(),
                        kind: presentation.kind,
                    },
                );
            }
            AgentEvent::ToolCompleted { tool_id, .. } => {
                remove_pending_tool(&mut pending, tool_id);
            }
            AgentEvent::AgentCompleted { .. }
            | AgentEvent::AgentFailed { .. }
            | AgentEvent::AgentCancelled { .. } => pending.clear(),
            _ => {}
        }
    }
    for completed_tool_id in completed_tool_ids {
        remove_pending_tool(&mut pending, completed_tool_id);
    }
    pending_tool_summary(language, &pending)
}

fn control_tool_key(tool_use_id: &str, request_id: &str) -> String {
    if tool_use_id.trim().is_empty() {
        request_id.to_string()
    } else {
        tool_use_id.to_string()
    }
}

fn upsert_pending_tool(pending: &mut Vec<PendingTool>, tool: PendingTool) {
    if let Some(existing) = pending.iter_mut().find(|existing| existing.key == tool.key) {
        *existing = tool;
    } else {
        pending.push(tool);
    }
}

fn remove_pending_tool(pending: &mut Vec<PendingTool>, tool_id: &str) {
    if let Some(index) = pending.iter().position(|tool| tool.key == tool_id) {
        pending.remove(index);
        return;
    }
    if let Some(index) = pending
        .iter()
        .position(|tool| tool.name.eq_ignore_ascii_case(tool_id))
    {
        pending.remove(index);
    }
}

fn pending_tool_summary(language: Language, pending: &[PendingTool]) -> Option<String> {
    let mut buckets = Vec::<(ToolPresentationKind, usize)>::new();
    for tool in pending {
        if let Some((_, count)) = buckets.iter_mut().find(|(kind, _)| *kind == tool.kind) {
            *count += 1;
        } else {
            buckets.push((tool.kind, 1));
        }
    }
    if buckets.is_empty() {
        return None;
    }
    Some(
        buckets
            .into_iter()
            .map(|(kind, count)| pending_tool_bucket_label(language, kind, count))
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

fn pending_tool_bucket_label(
    language: Language,
    kind: ToolPresentationKind,
    count: usize,
) -> String {
    if matches!(language, Language::ZhCn) {
        let unit = match kind {
            ToolPresentationKind::FileRead | ToolPresentationKind::MultiFileRead => "个文件",
            _ => "项",
        };
        let action = match kind {
            ToolPresentationKind::FileRead | ToolPresentationKind::MultiFileRead => "正在读取",
            ToolPresentationKind::FileWrite => "正在写入",
            ToolPresentationKind::FileEdit => "正在编辑",
            ToolPresentationKind::FileSearch | ToolPresentationKind::Lsp => "正在搜索",
            ToolPresentationKind::FileGlob => "正在查找文件",
            ToolPresentationKind::DirectoryList => "正在列目录",
            ToolPresentationKind::ShellCommand => "正在执行 Shell",
            ToolPresentationKind::WebFetch => "正在读取网页",
            ToolPresentationKind::WebSearch => "正在网页搜索",
            ToolPresentationKind::Skill => "正在加载技能",
            ToolPresentationKind::Agent => "正在调用 Agent",
            ToolPresentationKind::Memory => "正在更新记忆",
            ToolPresentationKind::ShellEvidence => "Shell 证据",
            ToolPresentationKind::Question => "正在提问",
            ToolPresentationKind::Custom => "自定义工具",
        };
        return format!("{action} {count} {unit}");
    }

    let plural = if count == 1 { "" } else { "s" };
    let action = match kind {
        ToolPresentationKind::FileRead | ToolPresentationKind::MultiFileRead => "reading file",
        ToolPresentationKind::FileWrite => "writing file",
        ToolPresentationKind::FileEdit => "editing file",
        ToolPresentationKind::FileSearch | ToolPresentationKind::Lsp => "searching item",
        ToolPresentationKind::FileGlob => "finding file",
        ToolPresentationKind::DirectoryList => "listing directory",
        ToolPresentationKind::ShellCommand => "running shell command",
        ToolPresentationKind::WebFetch => "fetching page",
        ToolPresentationKind::WebSearch => "searching web",
        ToolPresentationKind::Skill => "loading skill",
        ToolPresentationKind::Agent => "running agent",
        ToolPresentationKind::Memory => "updating memory",
        ToolPresentationKind::ShellEvidence => "handling shell evidence",
        ToolPresentationKind::Question => "asking question",
        ToolPresentationKind::Custom => "running custom tool",
    };
    format!("{action}{plural}: {count}")
}

pub(super) fn shell_evidence_status_message(language: Language, _action: &str) -> String {
    if matches!(language, Language::ZhCn) {
        "正在处理 Shell 证据 1 项".to_string()
    } else {
        "handling shell evidence: 1".to_string()
    }
}
