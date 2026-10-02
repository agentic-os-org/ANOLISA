use super::policy::{classify_command_intent, command_intent_key, CommandIntent};
use super::prelude::{CommandBlock, CommandOrigin, FindingSeverity, HookFinding};
use crate::types::{BuiltinFactRecord, HookProvenance};

#[derive(Debug, Clone)]
pub(crate) struct AggregatedHookFinding {
    pub(crate) provenance: HookProvenance,
    pub(crate) builtin_facts: Vec<BuiltinFactRecord>,
    pub(crate) primary: HookFinding,
    pub(crate) related: Vec<HookFinding>,
    pub(crate) recommended_skill: Option<String>,
    pub(crate) topic: String,
    pub(crate) entity_key: String,
    pub(crate) effective_severity: FindingSeverity,
    pub(crate) confidence: String,
    pub(crate) suppression_key: String,
}

pub(crate) fn combined_hook_finding(
    mut primary: HookFinding,
    related: &[HookFinding],
) -> HookFinding {
    if related.is_empty() {
        return primary;
    }
    let related_summary = related
        .iter()
        .map(|finding| {
            format!(
                "{} [{}]: {}",
                finding.hook_id,
                severity_label(finding.severity),
                finding.title
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    primary.description = format!(
        "{} Related findings: {related_summary}",
        primary.description
    );
    primary
}

pub(crate) fn recommended_skill_from_findings(
    primary: &HookFinding,
    related: &[HookFinding],
) -> Option<String> {
    primary
        .skill
        .clone()
        .or_else(|| related.iter().find_map(|finding| finding.skill.clone()))
}

pub(crate) fn has_memory_pressure_with_process(aggregate: &AggregatedHookFinding) -> bool {
    let has_pressure = aggregate.primary.hook_id == "memory-pressure"
        || aggregate
            .related
            .iter()
            .any(|finding| finding.hook_id == "memory-pressure");
    let has_process = aggregate.primary.hook_id == "high-memory-process"
        || aggregate
            .related
            .iter()
            .any(|finding| finding.hook_id == "high-memory-process");
    has_pressure && has_process
}

pub(crate) fn apply_memory_pressure_severity_upgrade(aggregate: &mut AggregatedHookFinding) {
    if aggregate.primary.hook_id != "high-memory-process" {
        return;
    }
    let Some(pressure_severity) = aggregate
        .related
        .iter()
        .find(|finding| finding.hook_id == "memory-pressure")
        .map(|finding| finding.severity)
    else {
        return;
    };
    let Some(mem_pct) = process_mem_pct(&aggregate.primary.title) else {
        return;
    };

    let target_severity = if pressure_severity == FindingSeverity::Critical && mem_pct >= 35.0 {
        Some(FindingSeverity::Critical)
    } else if severity_rank(pressure_severity) >= severity_rank(FindingSeverity::Warning)
        && mem_pct >= 20.0
    {
        Some(FindingSeverity::Warning)
    } else {
        None
    };

    if let Some(severity) = target_severity {
        if severity_rank(severity) > severity_rank(aggregate.primary.severity) {
            aggregate.primary.severity = severity;
        }
    }
}

pub(crate) fn is_memory_hook(hook_id: &str) -> bool {
    hook_id == "memory-pressure" || hook_id == "high-memory-process"
}

pub(crate) fn finding_topic(aggregate: &AggregatedHookFinding) -> &str {
    if !aggregate.topic.is_empty() {
        return &aggregate.topic;
    }
    finding_topic_from_findings(&aggregate.primary, &aggregate.related)
}

pub(crate) fn finding_topic_from_findings(
    primary: &HookFinding,
    related: &[HookFinding],
) -> &'static str {
    if is_memory_hook(&primary.hook_id)
        || related
            .iter()
            .any(|finding| is_memory_hook(&finding.hook_id))
    {
        "memory"
    } else {
        "external"
    }
}

pub(crate) fn finding_confidence<'a>(
    block: &CommandBlock,
    aggregate: &'a AggregatedHookFinding,
) -> &'a str {
    if !aggregate.confidence.is_empty() {
        return &aggregate.confidence;
    }
    computed_finding_confidence(block, aggregate)
}

pub(crate) fn computed_finding_confidence(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
) -> &'static str {
    if aggregate
        .primary
        .description
        .contains("Confidence is lower")
        || is_low_confidence_command_intent(&block.command)
    {
        "low"
    } else if aggregate.related.is_empty() {
        "medium"
    } else {
        "high"
    }
}

fn is_low_confidence_command_intent(command: &str) -> bool {
    matches!(
        classify_command_intent(command),
        CommandIntent::Lookup
            | CommandIntent::Pipeline
            | CommandIntent::Script
            | CommandIntent::Wrapper
            | CommandIntent::Interactive
    )
}

#[cfg(test)]
pub(crate) fn suppression_key(block: &CommandBlock, aggregate: &AggregatedHookFinding) -> String {
    if !aggregate.suppression_key.is_empty() {
        return aggregate.suppression_key.clone();
    }
    computed_suppression_key_with_origin(block, aggregate, CommandOrigin::UserInteractive)
}

pub(crate) fn computed_suppression_key(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
) -> String {
    computed_suppression_key_with_origin(block, aggregate, CommandOrigin::UserInteractive)
}

pub(crate) fn computed_suppression_key_with_origin(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
    origin: CommandOrigin,
) -> String {
    let origin = command_origin_label(origin);
    format!(
        "{}:{}:{}:{}:{}",
        finding_topic(aggregate),
        entity_key(block, aggregate),
        aggregate.primary.hook_id,
        command_intent_key(&block.command),
        origin
    )
}

pub(crate) fn command_origin_label(origin: CommandOrigin) -> &'static str {
    match origin {
        CommandOrigin::UserInteractive => "user_interactive",
        CommandOrigin::UserSendToShell => "user_send_to_shell",
        CommandOrigin::UserAnalysisAction => "user_analysis_action",
        CommandOrigin::AgentHandoff => "agent_handoff",
        CommandOrigin::ProviderTool => "provider_tool",
        CommandOrigin::ShellInternal => "shell_internal",
        CommandOrigin::Unknown => "unknown",
    }
}

pub(crate) fn entity_key(block: &CommandBlock, aggregate: &AggregatedHookFinding) -> String {
    if !aggregate.entity_key.is_empty() {
        return aggregate.entity_key.clone();
    }
    computed_entity_key(block, aggregate)
}

pub(crate) fn computed_entity_key(
    block: &CommandBlock,
    aggregate: &AggregatedHookFinding,
) -> String {
    match aggregate.primary.hook_id.as_str() {
        "memory-pressure" => "system-memory".to_string(),
        "high-memory-process" => process_entity_key(&aggregate.primary.title),
        _ => command_intent_key(&block.command).to_string(),
    }
}

fn process_entity_key(title: &str) -> String {
    if let Some(pid) = extract_pid_from_process_title(title) {
        return format!("process:pid:{pid}");
    }
    format!("process:title:{}", title.trim())
}

fn process_mem_pct(title: &str) -> Option<f64> {
    let before_marker = title.rsplit_once("% MEM")?.0;
    let pct = before_marker.split_whitespace().last()?;
    pct.parse().ok()
}

fn extract_pid_from_process_title(title: &str) -> Option<&str> {
    let marker = "(PID ";
    let start = title.find(marker)? + marker.len();
    let rest = &title[start..];
    let end = rest.find(')')?;
    let pid = rest[..end].trim();
    if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) {
        Some(pid)
    } else {
        None
    }
}

pub(crate) fn memory_hook_preference(hook_id: &str) -> u8 {
    match hook_id {
        "memory-pressure" => 1,
        _ => 0,
    }
}

pub(crate) fn severity_rank(severity: FindingSeverity) -> u8 {
    match severity {
        FindingSeverity::Info => 0,
        FindingSeverity::Warning => 1,
        FindingSeverity::Critical => 2,
    }
}

pub(crate) fn severity_label(severity: FindingSeverity) -> &'static str {
    match severity {
        FindingSeverity::Info => "info",
        FindingSeverity::Warning => "warning",
        FindingSeverity::Critical => "critical",
    }
}
