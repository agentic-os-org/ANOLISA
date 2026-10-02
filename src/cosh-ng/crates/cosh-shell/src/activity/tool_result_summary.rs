use crate::runtime::prelude::*;
use crate::tools::display::{ToolPresentation, ToolPresentationKind};

use super::tool_invocation::{ToolResultAccumulator, ToolResultPresentation};

pub(super) fn result_for_status(
    presentation: &ToolPresentation,
    output: &ToolResultAccumulator,
    status: &str,
    language: Language,
) -> ToolResultPresentation {
    let i18n = I18n::new(language);
    let duplicate_shell_evidence = matches!(presentation.kind, ToolPresentationKind::ShellEvidence)
        && shell_evidence_duplicate_provider_request(presentation);
    let failed = matches!(status, "error" | "failed" | "interrupted");
    let headline = if duplicate_shell_evidence {
        i18n.t(MessageId::ToolCardShellEvidenceDuplicateResult)
            .to_string()
    } else if failed && matches!(presentation.kind, ToolPresentationKind::ShellEvidence) {
        i18n.t(MessageId::ToolCardShellEvidenceFailedResult)
            .to_string()
    } else if failed {
        output
            .first_error_line
            .clone()
            .unwrap_or_else(|| status.to_string())
    } else {
        tool_result_summary(language, presentation, output)
            .and_then(|summary| summary.headline)
            .unwrap_or_else(|| result_success_headline(&i18n, presentation, output, status))
    };
    let mut metrics = Vec::new();
    if matches!(presentation.kind, ToolPresentationKind::ShellEvidence) {
        metrics.extend(shell_evidence_result_metrics(language, presentation));
        return ToolResultPresentation {
            headline,
            metrics,
            action: None,
        };
    } else if !failed {
        if let Some(summary) = tool_result_summary(language, presentation, output) {
            metrics.extend(summary.metrics);
        }
    }
    if metrics.is_empty() && output.stdout_lines > 0 {
        metrics.push(i18n.format(
            MessageId::ToolCardStdoutMetric,
            &[("count", &output.stdout_lines.to_string())],
        ));
    }
    if output.stderr_lines > 0 {
        metrics.push(i18n.format(
            MessageId::ToolCardStderrMetric,
            &[("count", &output.stderr_lines.to_string())],
        ));
    }
    if output.truncated {
        metrics.push(i18n.t(MessageId::ToolCardTruncatedMetric).to_string());
    }
    ToolResultPresentation {
        headline,
        metrics,
        action: None,
    }
}

fn result_success_headline(
    i18n: &I18n,
    presentation: &ToolPresentation,
    output: &ToolResultAccumulator,
    status: &str,
) -> String {
    match presentation.kind {
        ToolPresentationKind::FileWrite => change_summary_headline(
            i18n.t(MessageId::ToolCardWriteCompletedResult),
            presentation,
            i18n.language(),
        ),
        ToolPresentationKind::FileEdit => change_summary_headline(
            i18n.t(MessageId::ToolCardEditCompletedResult),
            presentation,
            i18n.language(),
        ),
        ToolPresentationKind::Skill => i18n.t(MessageId::ToolCardSkillAvailableResult).to_string(),
        ToolPresentationKind::Memory => memory_receipt_headline(i18n.language(), presentation),
        ToolPresentationKind::ShellEvidence => shell_evidence_result_headline(i18n, presentation),
        ToolPresentationKind::ShellCommand
            if output.stdout_lines > 0 || output.stderr_lines > 0 =>
        {
            i18n.t(MessageId::ToolCardOutputCapturedResult).to_string()
        }
        ToolPresentationKind::Custom => output
            .first_stdout_line
            .clone()
            .unwrap_or_else(|| status.to_string()),
        _ if output.stdout_lines > 0 => i18n.format(
            MessageId::ToolCardLinesReturnedResult,
            &[("count", &output.stdout_lines.to_string())],
        ),
        _ => status.to_string(),
    }
}

#[derive(Debug, Clone)]
struct ToolResultSummary {
    headline: Option<String>,
    metrics: Vec<String>,
}

fn tool_result_summary(
    language: Language,
    presentation: &ToolPresentation,
    output: &ToolResultAccumulator,
) -> Option<ToolResultSummary> {
    match presentation.kind {
        ToolPresentationKind::FileSearch => file_search_result_summary(language, output),
        ToolPresentationKind::FileGlob => count_result_summary(
            language,
            output,
            "files",
            "file",
            "files",
            "个文件",
            &["file_count", "files", "count", "total"],
            &["files", "matches", "results"],
        ),
        ToolPresentationKind::DirectoryList => directory_list_result_summary(language, output),
        ToolPresentationKind::Lsp => count_result_summary(
            language,
            output,
            "locations",
            "location",
            "locations",
            "个位置",
            &["location_count", "locations", "count", "total"],
            &["locations", "definitions", "references", "results"],
        ),
        ToolPresentationKind::WebSearch => web_search_result_summary(language, output),
        ToolPresentationKind::WebFetch => web_fetch_result_summary(language, output),
        ToolPresentationKind::Skill => skill_result_summary(language, presentation, output),
        _ => None,
    }
}

fn skill_result_summary(
    language: Language,
    presentation: &ToolPresentation,
    output: &ToolResultAccumulator,
) -> Option<ToolResultSummary> {
    if presentation_field(presentation, "action") != Some("list") {
        return None;
    }
    let text = output.output_sample.trim();
    let empty = text.is_empty() || text.eq_ignore_ascii_case("no skills found.");
    let count = if empty { 0 } else { non_empty_line_count(text) };
    let headline = match (language, count) {
        (Language::ZhCn, 0) => "未找到技能".to_string(),
        (Language::ZhCn, count) => format!("找到 {count} 个技能"),
        (Language::EnUs, 0) => "No skills found.".to_string(),
        (Language::EnUs, 1) => "1 skill found".to_string(),
        (Language::EnUs, count) => format!("{count} skills found"),
    };
    let metric = match language {
        Language::ZhCn => format!("技能: {count}"),
        Language::EnUs => format!("skills: {count}"),
    };
    Some(ToolResultSummary {
        headline: Some(headline),
        metrics: vec![metric],
    })
}

fn file_search_result_summary(
    language: Language,
    output: &ToolResultAccumulator,
) -> Option<ToolResultSummary> {
    let text = output.output_sample.trim();
    if text.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(text).ok();
    let match_count = parsed
        .as_ref()
        .and_then(|value| numeric_field(value, &["match_count", "matches", "total_matches"]))
        .or_else(|| parsed.as_ref().and_then(json_array_len))
        .or_else(|| Some(non_empty_line_count(text)));
    let file_count = parsed
        .as_ref()
        .and_then(|value| numeric_field(value, &["file_count", "files", "total_files"]))
        .or_else(|| parsed.as_ref().and_then(json_file_count))
        .or_else(|| text_file_count(text));
    let matches = match_count?;
    let headline = match language {
        Language::ZhCn => match file_count {
            Some(files) => format!("命中 {matches} 处，涉及 {files} 个文件"),
            None => format!("命中 {matches} 处"),
        },
        Language::EnUs => match file_count {
            Some(files) => format!("{matches} matches in {files} files"),
            None => format!("{matches} matches"),
        },
    };
    let mut metrics = Vec::new();
    metrics.push(match language {
        Language::ZhCn => format!("匹配: {matches}"),
        Language::EnUs => format!("matches: {matches}"),
    });
    if let Some(files) = file_count {
        metrics.push(match language {
            Language::ZhCn => format!("文件: {files}"),
            Language::EnUs => format!("files: {files}"),
        });
    }
    Some(ToolResultSummary {
        headline: Some(headline),
        metrics,
    })
}

fn directory_list_result_summary(
    language: Language,
    output: &ToolResultAccumulator,
) -> Option<ToolResultSummary> {
    let text = output.output_sample.trim();
    if text.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(text).ok();
    let count = parsed
        .as_ref()
        .and_then(|value| numeric_field(value, &["entry_count", "entries", "count", "total"]))
        .or_else(|| parsed.as_ref().and_then(json_array_len))
        .unwrap_or_else(|| non_empty_line_count(text));
    let mut metrics = Vec::new();
    if let Some((files, dirs)) = parsed.as_ref().and_then(json_file_dir_counts) {
        if files > 0 {
            metrics.push(match language {
                Language::ZhCn => format!("文件: {files}"),
                Language::EnUs => format!("files: {files}"),
            });
        }
        if dirs > 0 {
            metrics.push(match language {
                Language::ZhCn => format!("目录: {dirs}"),
                Language::EnUs => format!("dirs: {dirs}"),
            });
        }
    }
    Some(ToolResultSummary {
        headline: Some(count_phrase(language, count, "entry", "entries", "项")),
        metrics,
    })
}

fn web_search_result_summary(
    language: Language,
    output: &ToolResultAccumulator,
) -> Option<ToolResultSummary> {
    let text = output.output_sample.trim();
    if text.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(text).ok();
    let count = parsed
        .as_ref()
        .and_then(|value| numeric_field(value, &["result_count", "results", "count", "total"]))
        .or_else(|| parsed.as_ref().and_then(json_array_len))
        .unwrap_or_else(|| non_empty_line_count(text));
    let mut metrics = Vec::new();
    if let Some(title) = parsed.as_ref().and_then(first_result_title) {
        metrics.push(match language {
            Language::ZhCn => format!(
                "首条: {}",
                super::runtime::truncate_activity_preview(title, 80)
            ),
            Language::EnUs => format!(
                "top: {}",
                super::runtime::truncate_activity_preview(title, 80)
            ),
        });
    }
    Some(ToolResultSummary {
        headline: Some(count_phrase(language, count, "result", "results", "条结果")),
        metrics,
    })
}

fn web_fetch_result_summary(
    language: Language,
    output: &ToolResultAccumulator,
) -> Option<ToolResultSummary> {
    let text = output.output_sample.trim();
    if text.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(text).ok();
    let mut metrics = Vec::new();
    if let Some(status) = parsed.as_ref().and_then(|value| {
        numeric_field(value, &["status", "status_code"]).map(|status| status.to_string())
    }) {
        metrics.push(match language {
            Language::ZhCn => format!("状态: {status}"),
            Language::EnUs => format!("status: {status}"),
        });
    }
    if let Some(title) = parsed
        .as_ref()
        .and_then(|value| string_field(value, &["title"]))
    {
        metrics.push(match language {
            Language::ZhCn => format!(
                "标题: {}",
                super::runtime::truncate_activity_preview(title, 80)
            ),
            Language::EnUs => format!(
                "title: {}",
                super::runtime::truncate_activity_preview(title, 80)
            ),
        });
    }
    let content_chars = parsed
        .as_ref()
        .and_then(|value| string_field(value, &["content", "text", "markdown", "body"]))
        .map(|content| content.chars().count())
        .unwrap_or_else(|| text.chars().count());
    metrics.push(match language {
        Language::ZhCn => format!("正文: {content_chars} 字符"),
        Language::EnUs => format!("content: {content_chars} chars"),
    });
    Some(ToolResultSummary {
        headline: Some(match language {
            Language::ZhCn => "网页已读取".to_string(),
            Language::EnUs => "page fetched".to_string(),
        }),
        metrics,
    })
}

fn count_result_summary(
    language: Language,
    output: &ToolResultAccumulator,
    metric_label: &str,
    singular: &str,
    plural: &str,
    zh_suffix: &str,
    number_fields: &[&str],
    array_fields: &[&str],
) -> Option<ToolResultSummary> {
    let text = output.output_sample.trim();
    if text.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(text).ok();
    let count = parsed
        .as_ref()
        .and_then(|value| numeric_field(value, number_fields))
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(|value| array_field_len(value, array_fields))
        })
        .or_else(|| parsed.as_ref().and_then(json_array_len))
        .unwrap_or_else(|| non_empty_line_count(text));
    Some(ToolResultSummary {
        headline: Some(count_phrase(language, count, singular, plural, zh_suffix)),
        metrics: vec![match language {
            Language::ZhCn => format!("{}: {count}", localized_count_metric_label(metric_label)),
            Language::EnUs => format!("{metric_label}: {count}"),
        }],
    })
}

fn localized_count_metric_label(metric_label: &str) -> &str {
    match metric_label {
        "files" => "文件",
        "locations" => "位置",
        _ => metric_label,
    }
}

fn memory_receipt_headline(language: Language, presentation: &ToolPresentation) -> String {
    let receipt = presentation.secondary.as_deref().unwrap_or("updated");
    match language {
        Language::ZhCn => format!(
            "{}{}",
            localized_memory_name(&presentation.canonical_name),
            localized_memory_receipt(receipt)
        ),
        Language::EnUs => format!("{} {receipt}", presentation.canonical_name.to_lowercase()),
    }
}

fn localized_memory_name(canonical_name: &str) -> &str {
    match canonical_name {
        "Memory" => "记忆",
        "Task" => "任务",
        "Wakeup" => "唤醒",
        _ => canonical_name,
    }
}

fn localized_memory_receipt(receipt: &str) -> &str {
    match receipt {
        "created" => "已创建",
        "updated" => "已更新",
        "listed" => "已列出",
        "deleted" => "已删除",
        "stopped" => "已停止",
        "scheduled" => "已安排",
        _ => "已更新",
    }
}

fn count_phrase(
    language: Language,
    count: usize,
    singular: &str,
    plural: &str,
    zh_suffix: &str,
) -> String {
    match language {
        Language::ZhCn => format!("{count}{zh_suffix}"),
        Language::EnUs if count == 1 => format!("1 {singular}"),
        Language::EnUs => format!("{count} {plural}"),
    }
}

fn numeric_field(value: &serde_json::Value, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|field| {
            field
                .as_u64()
                .map(|number| number as usize)
                .or_else(|| field.as_str()?.parse::<usize>().ok())
        })
    })
}

fn string_field<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|field| field.as_str()))
}

fn array_field_len(value: &serde_json::Value, keys: &[&str]) -> Option<usize> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|field| field.as_array()))
        .map(Vec::len)
}

fn json_array_len(value: &serde_json::Value) -> Option<usize> {
    value
        .as_array()
        .map(Vec::len)
        .or_else(|| array_field_len(value, &["items", "entries", "results"]))
}

fn json_file_count(value: &serde_json::Value) -> Option<usize> {
    if let Some(count) = array_field_len(value, &["files"]) {
        return Some(count);
    }
    let items = value
        .as_array()
        .or_else(|| value.get("matches").and_then(|field| field.as_array()))
        .or_else(|| value.get("results").and_then(|field| field.as_array()))?;
    let files = items
        .iter()
        .filter_map(|item| string_field(item, &["path", "file", "file_path", "filename", "uri"]))
        .collect::<std::collections::HashSet<_>>();
    (!files.is_empty()).then_some(files.len())
}

fn json_file_dir_counts(value: &serde_json::Value) -> Option<(usize, usize)> {
    let items = value
        .as_array()
        .or_else(|| value.get("entries").and_then(|field| field.as_array()))
        .or_else(|| value.get("items").and_then(|field| field.as_array()))?;
    let mut files = 0;
    let mut dirs = 0;
    for item in items {
        match string_field(item, &["type", "kind"]).unwrap_or_default() {
            "file" => files += 1,
            "dir" | "directory" => dirs += 1,
            _ => {}
        }
    }
    (files > 0 || dirs > 0).then_some((files, dirs))
}

fn first_result_title(value: &serde_json::Value) -> Option<&str> {
    let item = value
        .as_array()
        .and_then(|items| items.first())
        .or_else(|| {
            value
                .get("results")
                .and_then(|field| field.as_array())
                .and_then(|items| items.first())
        })?;
    string_field(item, &["title", "name"])
}

fn non_empty_line_count(text: &str) -> usize {
    text.lines().filter(|line| !line.trim().is_empty()).count()
}

fn text_file_count(text: &str) -> Option<usize> {
    let files = text
        .lines()
        .filter_map(|line| line.split_once(':').map(|(file, _)| file.trim()))
        .filter(|file| !file.is_empty())
        .collect::<std::collections::HashSet<_>>();
    (!files.is_empty()).then_some(files.len())
}

fn change_summary_headline(
    base: &str,
    presentation: &ToolPresentation,
    language: Language,
) -> String {
    presentation
        .secondary
        .as_deref()
        .filter(|summary| !summary.trim().is_empty())
        .map(|summary| format!("{base}: {}", localized_change_summary(language, summary)))
        .unwrap_or_else(|| base.to_string())
}

fn localized_change_summary(language: Language, summary: &str) -> String {
    match (language, summary) {
        (Language::ZhCn, "new file") => "新文件".to_string(),
        _ => summary.to_string(),
    }
}

fn shell_evidence_result_headline(i18n: &I18n, presentation: &ToolPresentation) -> String {
    if shell_evidence_status(presentation) == Some("already_delivered") {
        return i18n
            .t(MessageId::ToolCardShellEvidenceAlreadyDeliveredResult)
            .to_string();
    }
    match shell_evidence_action(presentation) {
        Some("read_output") => i18n
            .t(MessageId::ToolCardShellEvidenceReadResult)
            .to_string(),
        Some("list_commands") => i18n
            .t(MessageId::ToolCardShellEvidenceListResult)
            .to_string(),
        _ => i18n
            .t(MessageId::ToolCardShellEvidenceDeliveredResult)
            .to_string(),
    }
}

fn shell_evidence_action(presentation: &ToolPresentation) -> Option<&str> {
    presentation_field(presentation, "action")
}

fn shell_evidence_status(presentation: &ToolPresentation) -> Option<&str> {
    presentation_field(presentation, "status")
}

fn shell_evidence_duplicate_provider_request(presentation: &ToolPresentation) -> bool {
    presentation_field(presentation, "duplicate_provider_request") == Some("true")
}

fn shell_evidence_result_metrics(
    language: Language,
    presentation: &ToolPresentation,
) -> Vec<String> {
    let mut metrics = Vec::new();
    match shell_evidence_action(presentation) {
        Some("list_commands") => {
            if let Some(count) = presentation_field(presentation, "command_count") {
                metrics.push(match language {
                    Language::ZhCn => format!("命令 {count} 条"),
                    Language::EnUs if count == "1" => "1 command".to_string(),
                    Language::EnUs => format!("{count} commands"),
                });
            }
            if presentation_field(presentation, "has_more") == Some("true") {
                metrics.push(match language {
                    Language::ZhCn => "还有更多历史".to_string(),
                    Language::EnUs => "more history available".to_string(),
                });
            }
        }
        Some("read_output") => {
            if let (Some(direction), Some(lines)) = (
                presentation_field(presentation, "direction"),
                presentation_field(presentation, "lines"),
            ) {
                metrics.push(match language {
                    Language::ZhCn => format!("{direction} {lines} 行"),
                    Language::EnUs => format!("{direction} {lines} lines"),
                });
            }
        }
        _ => {}
    }
    if let Some(reason) = presentation_field(presentation, "reason") {
        metrics.push(match language {
            Language::ZhCn => format!("原因: {reason}"),
            Language::EnUs => format!("reason: {reason}"),
        });
    }
    metrics
}

fn presentation_field<'a>(presentation: &'a ToolPresentation, label: &str) -> Option<&'a str> {
    presentation
        .fields
        .iter()
        .find(|field| field.label == label)
        .map(|field| field.value.as_str())
}
