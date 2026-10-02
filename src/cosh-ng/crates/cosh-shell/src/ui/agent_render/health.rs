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

const HEALTH_MAX_BODY_ROWS: usize = 12;
const HEALTH_METER_WIDE_WIDTH: usize = 8;
const HEALTH_METER_MEDIUM_WIDTH: usize = 6;
const HEALTH_MAX_VISIBLE_FINDINGS: usize = 3;
const HEALTH_MAX_VISIBLE_PROMPTS: usize = 3;

#[derive(Debug, Clone, Copy)]
pub struct HealthBannerModel<'a> {
    pub report: &'a HealthScanReport,
}

impl<'a> HealthBannerModel<'a> {
    pub fn new(report: &'a HealthScanReport) -> Self {
        Self { report }
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
        if health_uses_startup_row(model.report) {
            return self.health_startup_row_lines(model);
        }
        if self.plain {
            return self.plain_health_banner_lines(model);
        }

        let width = self.health_attention_width();
        let i18n = self.i18n();
        let sections = health_panel_sections(model.report, i18n, width, true);
        let height = sections.body_len().saturating_add(2).min(14) as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_health_banner(model.report, i18n, sections, area, &mut buffer);
        buffer_to_lines(&buffer, area)
    }

    fn health_banner_write_lines(&self, model: HealthBannerModel<'_>) -> Vec<String> {
        if health_uses_startup_row(model.report) {
            return self.health_startup_row_lines(model);
        }
        if self.plain {
            return self.plain_health_banner_lines(model);
        }

        let width = self.health_attention_width();
        let i18n = self.i18n();
        let sections = health_panel_sections(model.report, i18n, width, true);
        let height = sections.body_len().saturating_add(2).min(14) as u16;
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
        let content_width = width.saturating_sub(2).max(20) as usize;
        let i18n = self.i18n();
        let mut lines = vec![format!(
            "{}: {}",
            i18n.t(crate::MessageId::HealthBannerTitle),
            severity_label(model.report.overall_severity, i18n)
        )];
        for line in health_body_lines(model.report, i18n, width, false) {
            lines.extend(wrap_prefixed_line("  ", &line.plain_text(), content_width));
        }
        lines
    }

    pub(crate) fn health_startup_row_lines(&self, model: HealthBannerModel<'_>) -> Vec<String> {
        let width = self.panel_standard_width();
        let content_width = width.saturating_sub(2).max(20) as usize;
        let row = health_startup_row_text(model.report, self.i18n());
        wrap_plain_line(&row, content_width)
    }

    fn health_attention_width(&self) -> u16 {
        self.panel_standard_width()
    }
}

pub(crate) fn health_uses_startup_row(report: &HealthScanReport) -> bool {
    report.overall_severity == HealthSeverity::Ok
        && report.findings.is_empty()
        && report.unavailable.is_empty()
        && report.try_items.is_empty()
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

fn render_health_banner(
    report: &HealthScanReport,
    i18n: crate::I18n,
    sections: HealthPanelSections,
    area: Rect,
    buffer: &mut Buffer,
) {
    let severity = report.overall_severity;
    let block = Block::bordered()
        .title(Line::from(Span::styled(
            format!(
                "─ {}: {} ",
                i18n.t(crate::MessageId::HealthBannerTitle),
                severity_label(severity, i18n)
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .border_set(ROUNDED)
        .padding(Padding::horizontal(1))
        .border_style(Style::default().fg(severity_color(severity)));
    let inner = block.inner(area);
    block.render(area, buffer);

    render_health_panel_sections(sections, inner, buffer);
}

fn health_body_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    width: u16,
    allow_meter: bool,
) -> Vec<HealthBannerLine> {
    let content_width = width.saturating_sub(4).max(20) as usize;
    let mut lines = Vec::new();

    if !report.findings.is_empty() {
        lines.extend(metric_band_lines(report, i18n, content_width, allow_meter));
        lines.push(section_line(
            i18n.t(crate::MessageId::HealthBannerFindingsSection),
            content_width,
        ));
        lines.extend(finding_insight_lines(
            report,
            i18n,
            HEALTH_MAX_VISIBLE_FINDINGS,
            content_width,
        ));

        let try_limit = HEALTH_MAX_BODY_ROWS
            .saturating_sub(lines.len() + 2)
            .min(max_visible_prompt_count(content_width));
        if try_limit > 0 && !report.try_items.is_empty() {
            lines.push(section_line(
                i18n.t(crate::MessageId::HealthBannerSuggestedPromptSection),
                content_width,
            ));
            lines.extend(try_lines(report, i18n, try_limit, content_width));
        }

        if lines.len() < HEALTH_MAX_BODY_ROWS {
            lines.extend(unavailable_lines(
                report,
                i18n,
                HEALTH_MAX_BODY_ROWS.saturating_sub(lines.len()).min(1),
            ));
        }
    } else {
        lines.push(summary_line(report, i18n, allow_meter));
        lines.extend(metric_band_lines(report, i18n, content_width, allow_meter));
        lines.extend(unavailable_lines(
            report,
            i18n,
            HEALTH_MAX_BODY_ROWS.saturating_sub(lines.len()).min(2),
        ));
        let try_limit = HEALTH_MAX_BODY_ROWS
            .saturating_sub(lines.len() + 1)
            .min(max_visible_prompt_count(content_width));
        lines.extend(try_lines(report, i18n, try_limit, content_width));
    }

    lines.truncate(HEALTH_MAX_BODY_ROWS);
    lines
}

fn health_panel_sections(
    report: &HealthScanReport,
    i18n: crate::I18n,
    width: u16,
    allow_meter: bool,
) -> HealthPanelSections {
    let content_width = width.saturating_sub(2).max(20) as usize;

    if report.findings.is_empty() {
        let mut main = vec![summary_line(report, i18n, allow_meter)];
        main.extend(metric_band_lines(report, i18n, content_width, allow_meter));
        main.extend(unavailable_lines(
            report,
            i18n,
            HEALTH_MAX_BODY_ROWS.saturating_sub(main.len()).min(2),
        ));
        let try_limit = HEALTH_MAX_BODY_ROWS
            .saturating_sub(main.len() + 1)
            .min(max_visible_prompt_count(content_width));
        main.extend(try_lines(report, i18n, try_limit, content_width));
        main.truncate(HEALTH_MAX_BODY_ROWS);
        return HealthPanelSections {
            main,
            findings: Vec::new(),
            prompts: Vec::new(),
            unavailable: Vec::new(),
        };
    }

    let mut main = metric_band_lines(report, i18n, content_width, allow_meter);
    let mut findings = Vec::new();
    findings.push(section_line(
        i18n.t(crate::MessageId::HealthBannerFindingsSection),
        content_width,
    ));
    findings.extend(finding_insight_lines(
        report,
        i18n,
        HEALTH_MAX_VISIBLE_FINDINGS,
        content_width,
    ));

    let mut prompts = Vec::new();
    if !report.try_items.is_empty() {
        prompts.push(section_line(
            i18n.t(crate::MessageId::HealthBannerSuggestedPromptSection),
            content_width,
        ));
        prompts.extend(try_lines(
            report,
            i18n,
            max_visible_prompt_count(content_width),
            content_width,
        ));
    }

    let mut unavailable = Vec::new();
    unavailable.extend(unavailable_lines(report, i18n, 1));

    trim_panel_sections(&mut main, &mut findings, &mut prompts, &mut unavailable);

    HealthPanelSections {
        main,
        findings,
        prompts,
        unavailable,
    }
}

fn trim_panel_sections(
    main: &mut Vec<HealthBannerLine>,
    findings: &mut Vec<HealthBannerLine>,
    prompts: &mut Vec<HealthBannerLine>,
    unavailable: &mut Vec<HealthBannerLine>,
) {
    while main.len() + findings.len() + prompts.len() + unavailable.len() > HEALTH_MAX_BODY_ROWS {
        if unavailable.pop().is_some() {
            continue;
        }
        if prompt_block_count(prompts) > 1 && pop_last_prompt_block(prompts) {
            continue;
        }
        if !prompts.is_empty() {
            prompts.clear();
            continue;
        }
        if findings.len() > 2 && findings.pop().is_some() {
            continue;
        }
        if main.len() > 1 && main.pop().is_some() {
            continue;
        }
        break;
    }
    if !prompts.is_empty() && prompt_block_count(prompts) == 0 {
        prompts.clear();
    }
}

fn prompt_block_count(lines: &[HealthBannerLine]) -> usize {
    lines
        .iter()
        .filter(|line| line.plain_text().trim_start().starts_with('›'))
        .count()
}

fn pop_last_prompt_block(lines: &mut Vec<HealthBannerLine>) -> bool {
    let Some(start) = lines
        .iter()
        .rposition(|line| line.plain_text().trim_start().starts_with('›'))
    else {
        return false;
    };
    lines.truncate(start);
    true
}

fn render_health_panel_sections(sections: HealthPanelSections, area: Rect, buffer: &mut Buffer) {
    let groups = sections.into_groups();
    if groups.is_empty() {
        return;
    }
    let constraints = groups
        .iter()
        .map(|group| Constraint::Length(group.len() as u16))
        .collect::<Vec<_>>();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    for (group, chunk) in groups.into_iter().zip(chunks.iter()) {
        Paragraph::new(Text::from(
            group
                .into_iter()
                .map(|line| line.into_line())
                .collect::<Vec<_>>(),
        ))
        .wrap(Wrap { trim: true })
        .render(*chunk, buffer);
    }
}

fn summary_line(
    report: &HealthScanReport,
    i18n: crate::I18n,
    allow_meter: bool,
) -> HealthBannerLine {
    let severity = report.overall_severity;
    let mut spans = vec![Span::styled(
        severity_label(severity, i18n),
        severity_style(severity).add_modifier(Modifier::BOLD),
    )];
    if allow_meter && meter_visible_for_severity(severity) {
        if let Some(score) = report.health_score {
            let meter = MeterModel::health_score(score).render(HEALTH_METER_MEDIUM_WIDTH);
            spans.push(Span::raw(format!("  {score}/100 {meter}")));
        }
    }
    if report.elapsed_ms > 0 {
        spans.push(Span::raw(format!("  {}ms", report.elapsed_ms)));
    }
    HealthBannerLine { spans }
}

fn metric_band_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    content_width: usize,
    allow_meter: bool,
) -> Vec<HealthBannerLine> {
    let mut pressure = Vec::new();
    let mut levels = Vec::new();
    for cell in metric_cells(report, i18n) {
        match cell.direction {
            MetricDirection::LoadPerCore => pressure.push(cell),
            MetricDirection::Available | MetricDirection::Used => levels.push(cell),
        }
    }

    let mut lines = Vec::new();
    lines.extend(metric_lane_lines(
        i18n.t(crate::MessageId::HealthMetricPressure),
        pressure,
        report.overall_severity,
        content_width,
        allow_meter,
        false,
    ));
    lines.extend(metric_lane_lines(
        i18n.t(crate::MessageId::HealthMetricLevels),
        levels,
        report.overall_severity,
        content_width,
        allow_meter,
        true,
    ));
    lines
}

fn metric_lane_lines(
    label: &str,
    mut cells: Vec<HealthMetricCell>,
    severity: HealthSeverity,
    content_width: usize,
    allow_meter: bool,
    allow_second_line: bool,
) -> Vec<HealthBannerLine> {
    let label_width = display_width(label) + 2;
    let body_width = content_width.saturating_sub(label_width);
    let meter_width = metric_meter_width(content_width, severity, allow_meter);
    let mut lines = Vec::new();
    let max_rows = if allow_second_line && content_width >= 78 {
        2
    } else {
        1
    };
    for _ in 0..max_rows {
        if cells.is_empty() {
            break;
        }
        let take = fitting_metric_prefix_len(&cells, meter_width, body_width);
        let row_cells = cells.drain(..take).collect::<Vec<_>>();
        lines.push(metric_lane_line_from_cells(
            label,
            &row_cells,
            meter_width,
            body_width,
        ));
    }
    lines
}

fn fitting_metric_prefix_len(
    cells: &[HealthMetricCell],
    meter_width: Option<usize>,
    body_width: usize,
) -> usize {
    for len in (1..=cells.len()).rev() {
        if display_width(&join_metric_cells(&cells[..len], meter_width)) <= body_width {
            return len;
        }
    }
    1
}

fn metric_lane_line_from_cells(
    label: &str,
    cells: &[HealthMetricCell],
    meter_width: Option<usize>,
    body_width: usize,
) -> HealthBannerLine {
    let text = join_metric_cells(cells, meter_width);
    let mut spans = vec![Span::styled(
        format!("{label}  "),
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )];
    if display_width(&text) <= body_width {
        spans.extend(metric_cell_spans(cells, meter_width));
    } else {
        spans.push(Span::styled(
            truncate_display_width(&text, body_width),
            metric_cell_value_style(cells.first()),
        ));
    }
    HealthBannerLine { spans }
}

fn section_line(title: &str, content_width: usize) -> HealthBannerLine {
    let prefix = "─ ";
    let title_text = title.to_string();
    let _ = content_width;
    HealthBannerLine {
        spans: vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(
                title_text,
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
        ],
    }
}

fn health_startup_row_text(report: &HealthScanReport, i18n: crate::I18n) -> String {
    let mut cells = vec![format!(
        "{}: {}",
        i18n.t(crate::MessageId::HealthStartupLabel),
        severity_label(report.overall_severity, i18n)
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

fn load_metric_cell(
    report: &HealthScanReport,
    i18n: crate::I18n,
    load_key: &str,
    ratio_key: &str,
    compact_label: bool,
) -> Option<HealthMetricCell> {
    let load = number_fact(&report.facts, load_key)?;
    let ratio = number_fact(&report.facts, ratio_key)?;
    let cores = number_fact(&report.facts, "cpu.cores")
        .map(|value| value.round() as u64)
        .filter(|value| *value > 0)?;
    let label = if compact_label {
        i18n.t(crate::MessageId::HealthMetricLoad1mShort)
    } else {
        i18n.t(crate::MessageId::HealthMetricCpuLoadPerCore)
    };
    let load = format!("{load:.1}");
    let cores = cores.to_string();
    let ratio = format!("{ratio:.1}");
    let value = i18n.format(
        crate::MessageId::HealthMetricLoadValue,
        &[("load", &load), ("cores", &cores), ("ratio", &ratio)],
    );
    Some(HealthMetricCell::new(
        label,
        value,
        MetricDirection::LoadPerCore,
        HealthMetricKind::Load,
    ))
}

fn should_show_swap_metric(report: &HealthScanReport) -> bool {
    report.findings.iter().any(|finding| {
        matches!(
            finding.title_id,
            HealthMessageId::HealthFindingSwapPressure
                | HealthMessageId::HealthFindingMemoryAvailableLow
                | HealthMessageId::HealthFindingRecentOom
        )
    })
}

fn dynamic_load_5m_cell(report: &HealthScanReport, i18n: crate::I18n) -> Option<HealthMetricCell> {
    let has_high_load = report
        .findings
        .iter()
        .any(|finding| finding.title_id == HealthMessageId::HealthFindingCpuLoadHigh);
    if !has_high_load {
        return None;
    }
    let mut cell = load_metric_cell(report, i18n, "cpu.load_5m", "cpu.load_per_core_5m", true)?;
    cell.label = i18n
        .t(crate::MessageId::HealthMetricLoad5mShort)
        .to_string();
    Some(cell)
}

fn metric_cells(report: &HealthScanReport, i18n: crate::I18n) -> Vec<HealthMetricCell> {
    let mut cells = Vec::new();
    if let Some(cell) = load_metric_cell(
        report,
        i18n,
        "cpu.load_1m",
        "cpu.load_per_core_1m",
        !health_uses_startup_row(report),
    ) {
        cells.push(cell);
    }
    if let Some(cell) = dynamic_load_5m_cell(report, i18n) {
        cells.push(cell);
    }
    if let Some(value) = number_fact(&report.facts, "cpu.utilization_ratio") {
        cells.push(metric_cell_with_ratio(
            i18n.t(crate::MessageId::HealthMetricCpuUsed),
            value,
            MetricDirection::Used,
            HealthMetricKind::CpuUsed,
        ));
    }
    if let Some(value) = number_fact(&report.facts, "memory.used_ratio") {
        cells.push(metric_cell_with_ratio(
            i18n.t(crate::MessageId::HealthMetricMemoryUsed),
            value,
            MetricDirection::Used,
            HealthMetricKind::MemoryUsed,
        ));
    }
    if let Some((mount, value)) = root_disk_metric(report).or_else(|| riskiest_disk_metric(report))
    {
        let label = i18n.format(
            crate::MessageId::HealthMetricDiskMountUsed,
            &[("mount", &middle_ellipsis(mount, 26))],
        );
        cells.push(metric_cell_with_ratio(
            &label,
            value,
            MetricDirection::Used,
            HealthMetricKind::DiskUsed,
        ));
    }
    if should_show_riskiest_disk_metric(report) {
        if let Some((mount, value)) = riskiest_disk_metric(report) {
            if Some(mount) != root_disk_metric(report).map(|(root, _)| root) {
                let label = i18n.format(
                    crate::MessageId::HealthMetricDiskMountUsed,
                    &[("mount", &middle_ellipsis(mount, 26))],
                );
                cells.push(metric_cell_with_ratio(
                    &label,
                    value,
                    MetricDirection::Used,
                    HealthMetricKind::DiskUsed,
                ));
            }
        }
    }
    if let Some(value) = number_fact(&report.facts, "memory.swap_used_ratio") {
        if value > 0.0 || should_show_swap_metric(report) {
            cells.push(metric_cell_with_ratio(
                i18n.t(crate::MessageId::HealthMetricSwapUsed),
                value,
                MetricDirection::Used,
                HealthMetricKind::SwapUsed,
            ));
        }
    }
    cells
}

fn should_show_riskiest_disk_metric(report: &HealthScanReport) -> bool {
    report
        .findings
        .iter()
        .any(|finding| finding.title_id == HealthMessageId::HealthFindingDiskHigh)
}

fn root_disk_metric(report: &HealthScanReport) -> Option<(&str, f64)> {
    number_fact(&report.facts, "filesystem.root_used_ratio").map(|value| ("/", value))
}

fn riskiest_disk_metric(report: &HealthScanReport) -> Option<(&str, f64)> {
    let value = number_fact(&report.facts, "filesystem.max_used_ratio")?;
    let mount = string_fact(&report.facts, "filesystem.riskiest_mount").unwrap_or("?");
    Some((mount, value))
}

fn finding_insight_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    limit: usize,
    content_width: usize,
) -> Vec<HealthBannerLine> {
    if limit == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();

    for finding in sorted_findings(report).into_iter().take(limit) {
        let insight_id = insight_id_for_finding(finding);
        let mut insight = insight_text_for_finding(finding, insight_id, i18n);
        if insight_id == HealthMessageId::HealthInsightGeneric {
            let title = format_health_message(i18n, finding.title_id, &finding.detail_args);
            let separator = if is_zh(i18n) { "；" } else { "; " };
            insight = format!("{title}{separator}{insight}");
        }
        let evidence = evidence_text(report, finding, i18n, content_width);
        let with_evidence = if evidence.is_empty() {
            insight.clone()
        } else {
            append_evidence_sentence(&insight, &evidence, i18n)
        };
        let wrapped_with_evidence = wrap_plain_line(&with_evidence, content_width);
        let wrapped = if !evidence.is_empty() && wrapped_with_evidence.len() > 2 {
            let mut split = wrap_plain_line(&insight, content_width)
                .into_iter()
                .take(1)
                .collect::<Vec<_>>();
            split.extend(
                wrap_plain_line(&evidence, content_width)
                    .into_iter()
                    .take(1),
            );
            split
        } else {
            wrapped_with_evidence
        };
        for line in wrapped.into_iter().take(2) {
            lines.push(HealthBannerLine {
                spans: vec![Span::raw(line)],
            });
        }
    }
    lines
}

fn top_finding(report: &HealthScanReport) -> Option<&HealthFinding> {
    sorted_findings(report).into_iter().next()
}

fn insight_id_for_finding(finding: &HealthFinding) -> HealthMessageId {
    match finding.title_id {
        HealthMessageId::HealthFindingMemoryAvailableLow => {
            HealthMessageId::HealthInsightMemoryAvailableLow
        }
        HealthMessageId::HealthFindingDiskHigh => HealthMessageId::HealthInsightDiskHigh,
        HealthMessageId::HealthFindingRecentOom => HealthMessageId::HealthInsightRecentOom,
        HealthMessageId::HealthFindingCpuLoadHigh => HealthMessageId::HealthInsightCpuLoadHigh,
        HealthMessageId::HealthFindingSwapPressure => HealthMessageId::HealthInsightSwapPressure,
        HealthMessageId::HealthFindingServiceFailed
        | HealthMessageId::HealthFindingServiceInactive => {
            HealthMessageId::HealthInsightServiceState
        }
        _ => HealthMessageId::HealthInsightGeneric,
    }
}

fn insight_text_for_finding(
    finding: &HealthFinding,
    insight_id: HealthMessageId,
    i18n: crate::I18n,
) -> String {
    if matches!(
        finding.title_id,
        HealthMessageId::HealthFindingServiceFailed | HealthMessageId::HealthFindingServiceInactive
    ) {
        if has_service_detail_args(finding) {
            return format_health_message(i18n, insight_id, &finding.detail_args);
        }
        return format_health_message(i18n, finding.title_id, &finding.detail_args);
    }
    format_health_message(i18n, insight_id, &finding.detail_args)
}

fn append_evidence_sentence(insight: &str, evidence: &str, i18n: crate::I18n) -> String {
    let needs_separator = !matches!(
        insight.chars().next_back(),
        Some('.' | '!' | '?' | '。' | '！' | '？')
    );
    let separator = if is_zh(i18n) {
        if needs_separator {
            "。"
        } else {
            ""
        }
    } else if needs_separator {
        "."
    } else {
        ""
    };
    format!("{insight}{separator} {evidence}")
}

fn has_service_detail_args(finding: &HealthFinding) -> bool {
    finding.detail_args.contains_key("service")
        && finding.detail_args.contains_key("observed")
        && finding.detail_args.contains_key("expected")
}

fn evidence_text(
    report: &HealthScanReport,
    finding: &HealthFinding,
    i18n: crate::I18n,
    content_width: usize,
) -> String {
    if finding.title_id == HealthMessageId::HealthFindingRecentOom {
        if let Some(text) = oom_evidence_text(report, i18n) {
            return text;
        }
    }
    if finding.title_id == HealthMessageId::HealthFindingMemoryAvailableLow {
        if let Some(text) = memory_available_evidence_text(report, i18n) {
            return text;
        }
    }
    if finding.title_id == HealthMessageId::HealthFindingSwapPressure {
        if let Some(text) = swap_evidence_text(report, i18n) {
            return text;
        }
    }
    if finding.title_id == HealthMessageId::HealthFindingDiskHigh {
        if let Some(text) = disk_evidence_text(report, i18n) {
            return text;
        }
    }

    let mut pieces = Vec::new();
    for fact_id in &finding.evidence_fact_ids {
        let Some(fact) = report.facts.iter().find(|item| &item.id == fact_id) else {
            continue;
        };
        let Some(piece) = format_evidence_fact(fact, report, i18n) else {
            continue;
        };
        let next = join_text_cells(&pieces_with_candidate(&pieces, &piece));
        if !pieces.is_empty() && display_width(&next) > content_width {
            break;
        }
        pieces.push(piece);
    }
    join_text_cells(&pieces)
}

fn pieces_with_candidate(pieces: &[String], candidate: &str) -> Vec<String> {
    let mut values = pieces.to_vec();
    values.push(candidate.to_string());
    values
}

fn format_evidence_fact(
    fact: &HealthFact,
    report: &HealthScanReport,
    i18n: crate::I18n,
) -> Option<String> {
    match fact.key.as_str() {
        "cpu.load_per_core_1m" | "cpu.load_per_core_5m" => number_from_fact(fact).map(|value| {
            format!(
                "{} {:.1}{}",
                load_evidence_label(fact.key.as_str(), i18n),
                value,
                i18n.t(crate::MessageId::HealthMetricCpuPerCoreUnit)
            )
        }),
        "memory.available_ratio" => number_from_fact(fact).map(|value| {
            format!(
                "{} {}",
                i18n.t(crate::MessageId::HealthMetricMemoryAvailable),
                percent(value)
            )
        }),
        "memory.available_mib" => number_from_fact(fact).map(|value| {
            format!(
                "{} {:.0} MiB",
                i18n.t(crate::MessageId::HealthMetricMemoryAvailable),
                value
            )
        }),
        "memory.swap_used_ratio" => number_from_fact(fact).map(|value| {
            format!(
                "{} {}",
                i18n.t(crate::MessageId::HealthMetricSwapUsed),
                percent(value)
            )
        }),
        "memory.swap_used_mib" => {
            number_from_fact(fact).map(|value| format!("Swap {:.0} MiB", value))
        }
        "filesystem.max_used_ratio" => number_from_fact(fact).map(|value| {
            let mount = string_fact(&report.facts, "filesystem.riskiest_mount").unwrap_or("?");
            let label = i18n.format(
                crate::MessageId::HealthMetricDiskMountUsed,
                &[("mount", &middle_ellipsis(mount, 26))],
            );
            format!("{label} {}", percent(value))
        }),
        "filesystem.available_gib" => number_from_fact(fact).map(|value| {
            let gib = format!("{value:.1}");
            i18n.format(
                crate::MessageId::HealthEvidenceDiskAvailable,
                &[("gib", &gib)],
            )
        }),
        "filesystem.riskiest_mount" => string_from_fact(fact).map(|value| {
            let value = middle_ellipsis(value, 26);
            i18n.format(crate::MessageId::HealthEvidenceMount, &[("mount", &value)])
        }),
        "kernel.oom_latest_age_seconds" => {
            number_from_fact(fact).map(|value| format_oom_age(value as u64, i18n))
        }
        "kernel.oom_killed_process" | "kernel.oom_latest_process" => {
            string_from_fact(fact).map(|value| {
                i18n.format(
                    crate::MessageId::HealthEvidenceOomKilledProcess,
                    &[("process", value)],
                )
            })
        }
        "kernel.oom_latest_pid" => number_from_fact(fact).map(|value| format!("PID {:.0}", value)),
        "kernel.oom_latest_scope_label_id" => {
            string_from_fact(fact).map(|value| oom_scope_label_from_id(value, i18n))
        }
        "kernel.oom_latest_constraint" => {
            string_from_fact(fact).map(|value| oom_scope_label_from_constraint(value, i18n))
        }
        "kernel.oom_latest_task_cgroup" | "kernel.oom_latest_oom_cgroup" => string_from_fact(fact)
            .map(|value| {
                let cgroup = middle_ellipsis(value, 28);
                i18n.format(
                    crate::MessageId::HealthEvidenceOomCgroup,
                    &[("cgroup", &cgroup)],
                )
            }),
        "kernel.oom_event_count_last_1h" => number_from_fact(fact).map(|value| {
            let count = format!("{value:.0}");
            i18n.format(
                crate::MessageId::HealthEvidenceOomOneHourCount,
                &[("count", &count)],
            )
        }),
        "kernel.oom_event_count_last_24h" => number_from_fact(fact).map(|value| {
            let count = format!("{value:.0}");
            i18n.format(
                crate::MessageId::HealthEvidenceOomTwentyFourHourCount,
                &[("count", &count)],
            )
        }),
        _ => None,
    }
}

fn memory_available_evidence_text(report: &HealthScanReport, i18n: crate::I18n) -> Option<String> {
    let ratio = number_fact(&report.facts, "memory.available_ratio");
    let mib = number_fact(&report.facts, "memory.available_mib");
    match (mib, ratio) {
        (Some(mib), Some(ratio)) => {
            let label = i18n.t(crate::MessageId::HealthMetricMemoryAvailable);
            Some(format!("{label} {:.0} MiB ({})", mib, percent(ratio)))
        }
        (Some(mib), None) => {
            let label = i18n.t(crate::MessageId::HealthMetricMemoryAvailable);
            Some(format!("{label} {:.0} MiB", mib))
        }
        (None, Some(ratio)) => {
            let label = i18n.t(crate::MessageId::HealthMetricMemoryAvailable);
            Some(format!("{label} {}", percent(ratio)))
        }
        (None, None) => None,
    }
}

fn swap_evidence_text(report: &HealthScanReport, i18n: crate::I18n) -> Option<String> {
    let ratio = number_fact(&report.facts, "memory.swap_used_ratio");
    let mib = number_fact(&report.facts, "memory.swap_used_mib");
    let label = i18n.t(crate::MessageId::HealthMetricSwapUsed);
    match (mib, ratio) {
        (Some(mib), Some(ratio)) => Some(format!("{label} {:.0} MiB ({})", mib, percent(ratio))),
        (Some(mib), None) => Some(format!("{label} {:.0} MiB", mib)),
        (None, Some(ratio)) => Some(format!("{label} {}", percent(ratio))),
        (None, None) => None,
    }
}

fn disk_evidence_text(report: &HealthScanReport, i18n: crate::I18n) -> Option<String> {
    let ratio = number_fact(&report.facts, "filesystem.max_used_ratio")?;
    let mount = string_fact(&report.facts, "filesystem.riskiest_mount").unwrap_or("/");
    let label = i18n.format(
        crate::MessageId::HealthMetricDiskMountUsed,
        &[("mount", &middle_ellipsis(mount, 26))],
    );
    let available = number_fact(&report.facts, "filesystem.available_gib");
    if let Some(available) = available {
        if is_zh(i18n) {
            Some(format!(
                "{label} {}，可用 {:.1} GiB",
                percent(ratio),
                available
            ))
        } else {
            Some(format!(
                "{label} {}; {:.1} GiB available",
                percent(ratio),
                available
            ))
        }
    } else {
        Some(format!("{label} {}", percent(ratio)))
    }
}

fn oom_evidence_text(report: &HealthScanReport, i18n: crate::I18n) -> Option<String> {
    let age = number_fact(&report.facts, "kernel.oom_latest_age_seconds")
        .map(|value| format_age_seconds(value as u64));
    let process = string_fact(&report.facts, "kernel.oom_killed_process")
        .or_else(|| string_fact(&report.facts, "kernel.oom_latest_process"));
    let pid = number_fact(&report.facts, "kernel.oom_latest_pid");
    let scope = string_fact(&report.facts, "kernel.oom_latest_scope_label_id")
        .map(|value| oom_scope_label_from_id(value, i18n))
        .or_else(|| {
            string_fact(&report.facts, "kernel.oom_latest_constraint")
                .map(|value| oom_scope_label_from_constraint(value, i18n))
        });
    let cgroup = string_fact(&report.facts, "kernel.oom_latest_oom_cgroup")
        .or_else(|| string_fact(&report.facts, "kernel.oom_latest_task_cgroup"));

    if age.is_none() && process.is_none() && pid.is_none() && scope.is_none() {
        return None;
    }

    let mut subject = process.unwrap_or("unknown").to_string();
    if let Some(pid) = pid {
        subject = format!("{subject}(PID {:.0})", pid);
    }
    if is_zh(i18n) {
        let victim = if let Some(age) = age {
            i18n.format(
                crate::MessageId::HealthEvidenceOomVictimKilledAgo,
                &[("age", &age), ("subject", &subject)],
            )
        } else {
            i18n.format(
                crate::MessageId::HealthEvidenceOomVictimKilled,
                &[("subject", &subject)],
            )
        };
        return Some(format_oom_evidence_sentence_zh(
            &victim,
            scope.as_deref(),
            cgroup,
        ));
    }

    let victim = if let Some(age) = age {
        i18n.format(
            crate::MessageId::HealthEvidenceOomVictimKilledAgo,
            &[("subject", &subject), ("age", &age)],
        )
    } else {
        i18n.format(
            crate::MessageId::HealthEvidenceOomVictimKilled,
            &[("subject", &subject)],
        )
    };
    Some(format_oom_evidence_sentence_en(
        &victim,
        scope.as_deref(),
        cgroup,
    ))
}

fn format_oom_evidence_sentence_zh(
    victim: &str,
    scope: Option<&str>,
    cgroup: Option<&str>,
) -> String {
    let mut text = String::new();
    if let Some(scope) = scope {
        text.push_str("范围：");
        text.push_str(scope);
    }
    if !text.is_empty() {
        text.push('，');
    }
    text.push_str(victim);
    if let Some(cgroup) = cgroup {
        text.push_str("，cgroup：");
        text.push_str(&middle_ellipsis(cgroup, 28));
    }
    text
}

fn format_oom_evidence_sentence_en(
    victim: &str,
    scope: Option<&str>,
    cgroup: Option<&str>,
) -> String {
    let mut text = String::new();
    if let Some(scope) = scope {
        text.push_str("scope: ");
        text.push_str(scope);
    }
    if !text.is_empty() {
        text.push_str("; ");
    }
    text.push_str(victim);
    if let Some(cgroup) = cgroup {
        text.push_str("; cgroup: ");
        text.push_str(&middle_ellipsis(cgroup, 28));
    }
    text
}

fn unavailable_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    limit: usize,
) -> Vec<HealthBannerLine> {
    report
        .unavailable
        .iter()
        .take(limit)
        .map(|item| HealthBannerLine {
            spans: vec![
                Span::styled(
                    format!(
                        "{}: ",
                        i18n.t(crate::MessageId::HealthBannerUnavailableLabel)
                    ),
                    severity_style(item.severity),
                ),
                Span::raw(format!(
                    "{} {}",
                    collector_label(item.collector, i18n),
                    unavailable_reason_label(item.reason, i18n)
                )),
            ],
        })
        .collect()
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
    for intro in wrap_plain_line(
        i18n.t(crate::MessageId::HealthBannerSuggestedPromptIntro),
        content_width,
    )
    .into_iter()
    .take(2)
    {
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
}

impl HealthPanelSections {
    fn body_len(&self) -> usize {
        self.main.len() + self.findings.len() + self.prompts.len() + self.unavailable.len()
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
        groups
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricDirection {
    LoadPerCore,
    Available,
    Used,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthMetricKind {
    Load,
    CpuUsed,
    MemoryUsed,
    SwapUsed,
    DiskUsed,
}

#[derive(Debug, Clone)]
struct HealthMetricCell {
    label: String,
    value: String,
    direction: MetricDirection,
    kind: HealthMetricKind,
    ratio: Option<f64>,
}

impl HealthMetricCell {
    fn new(label: &str, value: String, direction: MetricDirection, kind: HealthMetricKind) -> Self {
        Self {
            label: label.to_string(),
            value,
            direction,
            kind,
            ratio: None,
        }
    }

    fn with_ratio(mut self, ratio: f64) -> Self {
        self.ratio = Some(ratio);
        self
    }

    fn render(&self) -> String {
        self.render_with_meter(None)
    }

    fn render_with_meter(&self, meter_width: Option<usize>) -> String {
        let text = format!("{} {}", self.label, self.value);
        if let Some(meter) = self.meter(meter_width) {
            format!("{text} {meter}")
        } else {
            text
        }
    }

    fn meter(&self, meter_width: Option<usize>) -> Option<String> {
        if self.direction == MetricDirection::LoadPerCore {
            return None;
        }
        let ratio = self.ratio?;
        let width = meter_width?;
        Some(MeterModel::ratio(ratio).render(width))
    }

    fn value_style(&self) -> Style {
        let Some(ratio) = self.ratio else {
            return Style::default().fg(Color::Cyan);
        };
        let color = match self.kind {
            HealthMetricKind::DiskUsed if ratio >= 0.95 => Color::Red,
            HealthMetricKind::DiskUsed if ratio >= 0.90 => Color::Yellow,
            HealthMetricKind::SwapUsed if ratio >= 0.90 => Color::Red,
            HealthMetricKind::SwapUsed if ratio >= 0.50 => Color::Yellow,
            HealthMetricKind::MemoryUsed if ratio >= 0.95 => Color::Red,
            HealthMetricKind::MemoryUsed if ratio >= 0.85 => Color::Yellow,
            HealthMetricKind::CpuUsed if ratio >= 0.90 => Color::Red,
            HealthMetricKind::CpuUsed if ratio >= 0.75 => Color::Yellow,
            _ => Color::Cyan,
        };
        Style::default().fg(color)
    }
}

fn metric_cell_with_ratio(
    label: &str,
    value: f64,
    direction: MetricDirection,
    kind: HealthMetricKind,
) -> HealthMetricCell {
    HealthMetricCell::new(label, percent(value), direction, kind).with_ratio(value)
}

fn metric_cell_spans(cells: &[HealthMetricCell], meter_width: Option<usize>) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (idx, cell) in cells.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{} ", cell.label),
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::styled(cell.value.clone(), cell.value_style()));
        if let Some(meter) = cell.meter(meter_width) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(meter, cell.value_style()));
        }
    }
    spans
}

fn metric_cell_value_style(cell: Option<&HealthMetricCell>) -> Style {
    cell.map(HealthMetricCell::value_style)
        .unwrap_or_else(|| Style::default().fg(Color::Cyan))
}

fn middle_ellipsis(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let keep = max_width.saturating_sub(3);
    let head_width = keep / 2;
    let tail_width = keep.saturating_sub(head_width);
    let mut head = String::new();
    for ch in text.chars() {
        if display_width(&format!("{head}{ch}")) > head_width {
            break;
        }
        head.push(ch);
    }
    let mut tail = String::new();
    for ch in text.chars().rev() {
        let next = format!("{ch}{tail}");
        if display_width(&next) > tail_width {
            break;
        }
        tail = next;
    }
    format!("{head}...{tail}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeterOrientation {
    HigherIsBetter,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct MeterModel {
    value_ratio: f64,
    orientation: MeterOrientation,
}

impl MeterModel {
    fn health_score(score: u8) -> Self {
        Self {
            value_ratio: f64::from(score) / 100.0,
            orientation: MeterOrientation::HigherIsBetter,
        }
    }

    fn ratio(value_ratio: f64) -> Self {
        Self {
            value_ratio,
            orientation: MeterOrientation::HigherIsBetter,
        }
    }

    fn render(&self, width: usize) -> String {
        let ratio = match self.orientation {
            MeterOrientation::HigherIsBetter => self.value_ratio,
        };
        let filled = (ratio.clamp(0.0, 1.0) * width as f64).ceil() as usize;
        let filled = filled.min(width);
        format!("▕{}{}▏", "█".repeat(filled), "░".repeat(width - filled))
    }
}

fn join_metric_cells(cells: &[HealthMetricCell], meter_width: Option<usize>) -> String {
    cells
        .iter()
        .map(|cell| cell.render_with_meter(meter_width))
        .collect::<Vec<_>>()
        .join("  ")
}

fn metric_meter_width(
    content_width: usize,
    severity: HealthSeverity,
    allow_meter: bool,
) -> Option<usize> {
    if !allow_meter || !meter_visible_for_severity(severity) {
        return None;
    }
    if content_width >= 118 {
        Some(HEALTH_METER_WIDE_WIDTH)
    } else if content_width >= 78 {
        Some(HEALTH_METER_MEDIUM_WIDTH)
    } else {
        None
    }
}

fn join_text_cells(cells: &[String]) -> String {
    cells.join("  ")
}

fn percent(value: f64) -> String {
    format!("{:.0}%", (value * 100.0).clamp(0.0, 999.0))
}

fn truncate_display_width(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let mut value = text.to_string();
    while display_width(&value) > max_width.saturating_sub(3) {
        if value.pop().is_none() {
            break;
        }
    }
    value.push_str("...");
    value
}

fn meter_visible_for_severity(severity: HealthSeverity) -> bool {
    matches!(
        severity,
        HealthSeverity::Degraded | HealthSeverity::Warning | HealthSeverity::Critical
    )
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
        HealthSeverity::Ok => Color::Green,
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

fn format_health_message(
    i18n: crate::I18n,
    id: HealthMessageId,
    args: &BTreeMap<String, String>,
) -> String {
    let mut text = i18n.t(id.to_i18n()).to_string();
    for (key, value) in args {
        text = text.replace(&format!("{{{key}}}"), value);
    }
    text
}

fn number_fact(facts: &[HealthFact], key: &str) -> Option<f64> {
    facts
        .iter()
        .find(|fact| fact.key == key)
        .and_then(|fact| match &fact.value {
            HealthFactValue::Integer(value) => Some(*value as f64),
            HealthFactValue::Unsigned(value) => Some(*value as f64),
            HealthFactValue::Float(value) => Some(*value),
            HealthFactValue::String(_) | HealthFactValue::Bool(_) => None,
        })
}

fn string_fact<'a>(facts: &'a [HealthFact], key: &str) -> Option<&'a str> {
    facts
        .iter()
        .find(|fact| fact.key == key)
        .and_then(|fact| match &fact.value {
            HealthFactValue::String(value) => Some(value.as_str()),
            HealthFactValue::Integer(_)
            | HealthFactValue::Unsigned(_)
            | HealthFactValue::Float(_)
            | HealthFactValue::Bool(_) => None,
        })
}

fn number_from_fact(fact: &HealthFact) -> Option<f64> {
    match &fact.value {
        HealthFactValue::Integer(value) => Some(*value as f64),
        HealthFactValue::Unsigned(value) => Some(*value as f64),
        HealthFactValue::Float(value) => Some(*value),
        HealthFactValue::String(_) | HealthFactValue::Bool(_) => None,
    }
}

fn string_from_fact(fact: &HealthFact) -> Option<&str> {
    match &fact.value {
        HealthFactValue::String(value) => Some(value.as_str()),
        HealthFactValue::Integer(_)
        | HealthFactValue::Unsigned(_)
        | HealthFactValue::Float(_)
        | HealthFactValue::Bool(_) => None,
    }
}

fn format_age_seconds(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    }
}

fn format_oom_age(seconds: u64, i18n: crate::I18n) -> String {
    let age = format_age_seconds(seconds);
    i18n.format(crate::MessageId::HealthEvidenceOomAge, &[("age", &age)])
}

fn load_evidence_label(key: &str, i18n: crate::I18n) -> String {
    let label_id = match key {
        "cpu.load_per_core_5m" => crate::MessageId::HealthMetricLoad5mShort,
        _ => crate::MessageId::HealthMetricLoad1mShort,
    };
    let compact = i18n.t(label_id);
    let load = i18n.t(crate::MessageId::HealthMetricPressure);
    if is_zh(i18n) {
        format!("{compact}{load}")
    } else {
        format!("{load} {compact}")
    }
}

fn oom_scope_label_from_constraint(raw: &str, i18n: crate::I18n) -> String {
    oom_scope_label_from_id(oom_scope_label_id_from_constraint(raw), i18n)
}

fn oom_scope_label_id_from_constraint(raw: &str) -> &'static str {
    match raw {
        "CONSTRAINT_MEMCG" => "memcg",
        "CONSTRAINT_NONE" => "host",
        "CONSTRAINT_CPUSET" => "cpuset",
        "CONSTRAINT_MEMORY_POLICY" => "memory_policy",
        _ => "unknown",
    }
}

fn oom_scope_label_from_id(id: &str, i18n: crate::I18n) -> String {
    let message_id = match id {
        "memcg" => crate::MessageId::HealthOomScopeMemcg,
        "host" => crate::MessageId::HealthOomScopeHost,
        "cpuset" => crate::MessageId::HealthOomScopeCpuset,
        "memory_policy" => crate::MessageId::HealthOomScopeMemoryPolicy,
        _ => crate::MessageId::HealthOomScopeUnknown,
    };
    i18n.t(message_id).to_string()
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
