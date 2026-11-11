use std::{
    collections::BTreeMap,
    io::{self, Write},
};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border::ROUNDED,
    text::{Line, Span, Text},
    widgets::{block::Padding, Block, Paragraph, Widget, Wrap},
};

use super::health_labels::collector_label;
use crate::diagnostics::health::{
    sorted_findings, sorted_try_items, HealthFact, HealthFactValue, HealthFinding, HealthMessageId,
    HealthScanReport, HealthSeverity, HealthUnavailableReason,
};

use super::{
    buffer_to_lines, buffer_to_styled_lines, display_width, wrap_plain_line, RatatuiInlineRenderer,
};

mod facts;
mod findings;
mod layout;
mod metrics;
#[cfg(test)]
mod tests;

use facts::*;
use findings::*;
use layout::*;
use metrics::*;

const HEALTH_MAX_BODY_ROWS: usize = 16;
const HEALTH_METER_WIDE_WIDTH: usize = 8;
const HEALTH_METER_MEDIUM_WIDTH: usize = 6;
const HEALTH_MAX_VISIBLE_FINDINGS: usize = 3;
const HEALTH_MAX_FINDING_ROWS: usize = 6;
const HEALTH_MAX_VISIBLE_PROMPTS: usize = 3;

#[derive(Debug, Clone, Copy)]
pub struct HealthBannerModel<'a> {
    pub report: &'a HealthScanReport,
    show_guide: bool,
    show_login_entry: bool,
}

impl<'a> HealthBannerModel<'a> {
    pub fn new(report: &'a HealthScanReport) -> Self {
        Self {
            report,
            show_guide: false,
            show_login_entry: false,
        }
    }

    pub(crate) fn with_login_entry(mut self, show: bool) -> Self {
        self.show_login_entry = show;
        self
    }

    pub(crate) fn with_guide(mut self) -> Self {
        self.show_guide = true;
        self
    }
}

impl RatatuiInlineRenderer {
    pub fn write_health_banner<W: Write>(
        &self,
        output: &mut W,
        model: HealthBannerModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.health_banner_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn health_banner_lines(&self, model: HealthBannerModel<'_>) -> Vec<String> {
        if !model.show_guide && health_uses_startup_row(model.report) {
            return self.health_startup_row_lines(model);
        }
        if self.plain {
            return self.plain_health_banner_lines(model);
        }

        let width = self.health_attention_width();
        let i18n = self.i18n();
        let mut sections = health_panel_sections(model.report, i18n, width, true);
        if model.show_guide {
            sections.guide = health_guide_lines(
                i18n,
                width.saturating_sub(4) as usize,
                model.show_login_entry,
            );
        }
        let height = sections.body_len().saturating_add(2) as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_health_banner(model.report, i18n, sections, area, &mut buffer);
        buffer_to_lines(&buffer, area)
    }

    fn health_banner_write_lines(&self, model: HealthBannerModel<'_>) -> Vec<String> {
        if !model.show_guide && health_uses_startup_row(model.report) {
            return self.health_startup_row_lines(model);
        }
        if self.plain {
            return self.plain_health_banner_lines(model);
        }

        let width = self.health_attention_width();
        let i18n = self.i18n();
        let mut sections = health_panel_sections(model.report, i18n, width, true);
        if model.show_guide {
            sections.guide = health_guide_lines(
                i18n,
                width.saturating_sub(4) as usize,
                model.show_login_entry,
            );
        }
        let height = sections.body_len().saturating_add(2) as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_health_banner(model.report, i18n, sections, area, &mut buffer);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_health_banner_lines(&self, model: HealthBannerModel<'_>) -> Vec<String> {
        let width = self.panel_standard_width();
        let content_width = width.saturating_sub(2).max(1) as usize;
        let i18n = self.i18n();
        let mut lines = wrap_plain_line(
            &format!(
                "{}: {}",
                i18n.t(crate::MessageId::HealthBannerTitle),
                health_status_label(model.report, i18n)
            ),
            width.max(1) as usize,
        );
        for line in health_body_lines(model.report, i18n, width, false) {
            lines.extend(wrap_prefixed_line("  ", &line.plain_text(), content_width));
        }
        if model.show_guide {
            for line in health_guide_lines(i18n, content_width, model.show_login_entry) {
                lines.push(format!("  {}", line.plain_text()));
            }
        }
        plain_health_buffer(lines, width)
    }

    pub(crate) fn health_startup_row_lines(&self, model: HealthBannerModel<'_>) -> Vec<String> {
        let width = self.panel_standard_width();
        let content_width = width.saturating_sub(2).max(1) as usize;
        let row = health_startup_row_text(model.report, self.i18n());
        plain_health_buffer(wrap_plain_line(&row, content_width), width)
    }

    fn health_attention_width(&self) -> u16 {
        self.panel_standard_width()
    }
}

fn health_guide_lines(i18n: crate::I18n, width: usize, show_entry: bool) -> Vec<HealthBannerLine> {
    let (entry, guide) = if is_zh(i18n) {
        ("输入 cosh 开始 AI 问答", "输入 cosh guide 查看用法")
    } else {
        ("Type cosh to ask AI", "Type cosh guide for help")
    };
    let combined = format!("{entry} · {guide}");
    let phrases = if !show_entry {
        vec![guide.to_string()]
    } else if display_width(&combined) <= width {
        vec![combined]
    } else {
        vec![entry.to_string(), guide.to_string()]
    };
    phrases
        .into_iter()
        .flat_map(|phrase| wrap_plain_line(&phrase, width.max(1)))
        .map(|line| {
            let mut spans = Vec::new();
            let mut rest = line.as_str();
            while let Some(pos) = rest.find("cosh") {
                spans.push(Span::styled(
                    rest[..pos].to_string(),
                    Style::default().fg(Color::Gray),
                ));
                rest = &rest[pos..];
                let command = if rest.starts_with("cosh guide") {
                    "cosh guide"
                } else {
                    "cosh"
                };
                spans.push(Span::styled(
                    command,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                rest = &rest[command.len()..];
            }
            spans.push(Span::styled(
                rest.to_string(),
                Style::default().fg(Color::Gray),
            ));
            HealthBannerLine { spans }
        })
        .collect()
}

fn plain_health_buffer(lines: Vec<String>, width: u16) -> Vec<String> {
    let area = Rect::new(
        0,
        0,
        width.max(1),
        lines.len().min(u16::MAX as usize) as u16,
    );
    let mut buffer = Buffer::empty(area);
    let text = Text::from(lines.into_iter().map(Line::raw).collect::<Vec<_>>());
    Paragraph::new(text).render(area, &mut buffer);
    buffer_to_lines(&buffer, area)
}

pub(crate) fn health_uses_startup_row(report: &HealthScanReport) -> bool {
    report.overall_severity == HealthSeverity::Ok
        && report.findings.is_empty()
        && report.unavailable.is_empty()
        && report.try_items.is_empty()
        && !report.facts.iter().any(|fact| {
            matches!(
                fact.key.as_str(),
                "memory.available_mib"
                    | "filesystem.root_available_gib"
                    | "filesystem.available_gib"
            )
        })
}

fn has_resource_facts(report: &HealthScanReport) -> bool {
    report.facts.iter().any(|fact| {
        matches!(
            fact.category,
            crate::diagnostics::health::HealthFactCategory::Cpu
                | crate::diagnostics::health::HealthFactCategory::Memory
                | crate::diagnostics::health::HealthFactCategory::Disk
        )
    })
}

pub(crate) fn primary_health_prompt_suggestion(
    report: &HealthScanReport,
    i18n: crate::I18n,
) -> Option<String> {
    let item = sorted_try_items(report).into_iter().next()?;
    let prompt_id = item.prompt_id.unwrap_or(item.label_id);
    let prompt_args = if item.prompt_id.is_some() {
        &item.prompt_args
    } else {
        &item.label_args
    };
    Some(format_health_message(i18n, prompt_id, prompt_args))
}

fn health_startup_row_text(report: &HealthScanReport, i18n: crate::I18n) -> String {
    let status = health_status_label(report, i18n);
    let mut cells = vec![format!(
        "{}: {}",
        i18n.t(crate::MessageId::HealthStartupLabel),
        status
    )];
    if report.elapsed_ms > 0 {
        cells.push(format!("{}ms", report.elapsed_ms));
    }
    cells.extend(
        metric_cells(report, i18n)
            .into_iter()
            .map(|cell| cell.render()),
    );
    cells.join(" · ")
}

fn health_collection_gap_only(report: &HealthScanReport) -> bool {
    report.resource_severity == HealthSeverity::Ok
        && has_resource_facts(report)
        && !report.unavailable.is_empty()
        && report.findings.iter().all(|finding| {
            finding.category == crate::diagnostics::health::HealthFindingCategory::CollectionGap
        })
}

fn health_display_severity(report: &HealthScanReport) -> HealthSeverity {
    if health_collection_gap_only(report) {
        HealthSeverity::Ok
    } else {
        report.overall_severity
    }
}

fn health_status_style(report: &HealthScanReport) -> Style {
    let severity = health_display_severity(report);
    if severity == HealthSeverity::Ok && !report.facts.is_empty() {
        Style::default().fg(Color::Green)
    } else {
        severity_style(severity)
    }
}

fn health_status_label(report: &HealthScanReport, i18n: crate::I18n) -> String {
    let collection_gap_only = health_collection_gap_only(report);
    if report.overall_severity != HealthSeverity::Ok && !collection_gap_only {
        return severity_label(report.overall_severity, i18n);
    }
    if report.facts.is_empty() {
        if is_zh(i18n) {
            "尚未取得检查事实".to_string()
        } else {
            "not checked".to_string()
        }
    } else if collection_gap_only
        || report
            .check_results
            .iter()
            .any(|check| check.status != crate::diagnostics::health::model::HealthCheckStatus::Ok)
    {
        if is_zh(i18n) {
            "已检查项未见异常".to_string()
        } else {
            "no anomalies in checked items".to_string()
        }
    } else {
        severity_label(report.overall_severity, i18n)
    }
}

fn try_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    limit: usize,
    content_width: usize,
) -> Vec<HealthBannerLine> {
    let mut lines = Vec::new();
    if limit == 0 || report.try_items.is_empty() {
        return lines;
    }
    let intro = if content_width < 40 && !is_zh(i18n) {
        "You can type these prompts:"
    } else {
        i18n.t(crate::MessageId::HealthBannerSuggestedPromptIntro)
    };
    for intro in wrap_plain_line(intro, content_width).into_iter().take(2) {
        lines.push(HealthBannerLine {
            spans: vec![Span::styled(intro, Style::default().fg(Color::Gray))],
        });
    }
    for item in sorted_try_items(report).into_iter().take(limit) {
        let prompt_id = item.prompt_id.unwrap_or(item.label_id);
        let prompt_args = if item.prompt_id.is_some() {
            &item.prompt_args
        } else {
            &item.label_args
        };
        let body = format_health_message(i18n, prompt_id, prompt_args);
        lines.extend(wrap_prompt_line(&body, content_width));
    }
    lines
}

fn max_visible_prompt_count(content_width: usize) -> usize {
    if content_width < 60 {
        1
    } else {
        HEALTH_MAX_VISIBLE_PROMPTS
    }
}

#[derive(Debug, Clone)]
struct HealthBannerLine {
    spans: Vec<Span<'static>>,
}

impl HealthBannerLine {
    fn plain_text(&self) -> String {
        self.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn into_line(self) -> Line<'static> {
        Line::from(self.spans)
    }
}

#[derive(Debug, Clone)]
struct HealthPanelSections {
    main: Vec<HealthBannerLine>,
    findings: Vec<HealthBannerLine>,
    prompts: Vec<HealthBannerLine>,
    unavailable: Vec<HealthBannerLine>,
    guide: Vec<HealthBannerLine>,
}

impl HealthPanelSections {
    fn body_len(&self) -> usize {
        self.main.len()
            + self.findings.len()
            + self.prompts.len()
            + self.unavailable.len()
            + self.guide.len()
    }

    fn into_groups(self) -> Vec<Vec<HealthBannerLine>> {
        let mut groups = Vec::new();
        if !self.main.is_empty() {
            groups.push(self.main);
        }
        if !self.findings.is_empty() {
            groups.push(self.findings);
        }
        if !self.prompts.is_empty() {
            groups.push(self.prompts);
        }
        if !self.unavailable.is_empty() {
            groups.push(self.unavailable);
        }
        if !self.guide.is_empty() {
            groups.push(self.guide);
        }
        groups
    }
}

fn severity_label(severity: HealthSeverity, i18n: crate::I18n) -> String {
    let id = match severity {
        HealthSeverity::Ok => crate::MessageId::HealthSeverityOk,
        HealthSeverity::Warning => crate::MessageId::HealthSeverityWarning,
        HealthSeverity::Critical => crate::MessageId::HealthSeverityCritical,
        HealthSeverity::Degraded => crate::MessageId::HealthSeverityDegraded,
        HealthSeverity::Unavailable => crate::MessageId::HealthSeverityUnavailable,
    };
    i18n.t(id).to_string()
}

fn severity_style(severity: HealthSeverity) -> Style {
    Style::default().fg(severity_color(severity))
}

fn severity_color(severity: HealthSeverity) -> Color {
    match severity {
        HealthSeverity::Ok => Color::Reset,
        HealthSeverity::Warning => Color::Yellow,
        HealthSeverity::Critical => Color::Red,
        HealthSeverity::Degraded => Color::Cyan,
        HealthSeverity::Unavailable => Color::DarkGray,
    }
}

fn is_zh(i18n: crate::I18n) -> bool {
    i18n.language() == crate::Language::ZhCn
}

fn unavailable_reason_label(reason: HealthUnavailableReason, i18n: crate::I18n) -> &'static str {
    let id = match reason {
        HealthUnavailableReason::Unsupported => crate::MessageId::HealthUnavailableUnsupported,
        HealthUnavailableReason::MissingContext => crate::MessageId::HealthBannerUnavailableLabel,
        HealthUnavailableReason::PermissionDenied => {
            crate::MessageId::HealthUnavailablePermissionDenied
        }
        HealthUnavailableReason::CommandMissing => {
            crate::MessageId::HealthUnavailableCommandMissing
        }
        HealthUnavailableReason::Timeout => crate::MessageId::HealthUnavailableTimeout,
        HealthUnavailableReason::ParseError => crate::MessageId::HealthUnavailableParseError,
    };
    i18n.t(id)
}

fn wrap_prefixed_line(prefix: &str, text: &str, width: usize) -> Vec<String> {
    let continuation = " ".repeat(display_width(prefix));
    let content_width = width.saturating_sub(display_width(prefix)).max(1);
    wrap_plain_line(text, content_width)
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                format!("{prefix}{line}")
            } else {
                format!("{continuation}{line}")
            }
        })
        .collect()
}

fn wrap_prompt_line(text: &str, width: usize) -> Vec<HealthBannerLine> {
    let prefix = "› ";
    let continuation = "  ";
    let body_width = width.saturating_sub(display_width(prefix)).max(1);
    wrap_plain_line(text, body_width)
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                HealthBannerLine {
                    spans: vec![
                        Span::styled(
                            prefix.to_string(),
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(line),
                    ],
                }
            } else {
                HealthBannerLine {
                    spans: vec![
                        Span::styled(
                            continuation.to_string(),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(line, Style::default().fg(Color::DarkGray)),
                    ],
                }
            }
        })
        .collect()
}
