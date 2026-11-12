use super::*;

pub(super) fn render_health_banner(
    report: &HealthScanReport,
    i18n: crate::I18n,
    sections: HealthPanelSections,
    area: Rect,
    buffer: &mut Buffer,
) {
    let severity = health_display_severity(report);
    let border_color = if severity == HealthSeverity::Ok {
        Color::Gray
    } else {
        severity_color(severity)
    };
    let show_status = health_status_fits_title(report, i18n, area.width);
    let mut title = vec![Span::styled(
        format!(
            "─ {}{}",
            i18n.t(crate::MessageId::HealthBannerTitle),
            if show_status { ": " } else { " " }
        ),
        Style::default()
            .fg(severity_color(severity))
            .add_modifier(Modifier::BOLD),
    )];
    if show_status {
        title.push(Span::styled(
            format!("{} ", health_status_label(report, i18n)),
            health_status_style(report).add_modifier(Modifier::BOLD),
        ));
    }
    let block = Block::bordered()
        .title(Line::from(title))
        .border_set(ROUNDED)
        .padding(Padding::horizontal(1))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    block.render(area, buffer);

    render_health_panel_sections(sections, inner, buffer);
}

fn health_status_fits_title(report: &HealthScanReport, i18n: crate::I18n, width: u16) -> bool {
    let title = format!(
        "─ {}: {} ",
        i18n.t(crate::MessageId::HealthBannerTitle),
        health_status_label(report, i18n)
    );
    (has_resource_facts(report) || !report.findings.is_empty())
        && display_width(&title) <= width.saturating_sub(2) as usize
}

pub(super) fn health_body_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    width: u16,
    allow_meter: bool,
) -> Vec<HealthBannerLine> {
    let content_width = width.saturating_sub(4).max(1) as usize;
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

pub(super) fn health_panel_sections(
    report: &HealthScanReport,
    i18n: crate::I18n,
    width: u16,
    allow_meter: bool,
) -> HealthPanelSections {
    let content_width = width.saturating_sub(4).max(1) as usize;

    if report.findings.is_empty() {
        let mut main = Vec::new();
        if !health_status_fits_title(report, i18n, width) {
            main.push(summary_line(report, i18n, allow_meter));
        }
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
            guide: Vec::new(),
        };
    }

    let mut main = Vec::new();
    if !health_status_fits_title(report, i18n, width) {
        main.push(summary_line(report, i18n, allow_meter));
    }
    main.extend(metric_band_lines(report, i18n, content_width, allow_meter));
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
        guide: Vec::new(),
    }
}

pub(super) fn trim_panel_sections(
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

pub(super) fn prompt_block_count(lines: &[HealthBannerLine]) -> usize {
    lines
        .iter()
        .filter(|line| line.plain_text().trim_start().starts_with('›'))
        .count()
}

pub(super) fn pop_last_prompt_block(lines: &mut Vec<HealthBannerLine>) -> bool {
    let Some(start) = lines
        .iter()
        .rposition(|line| line.plain_text().trim_start().starts_with('›'))
    else {
        return false;
    };
    lines.truncate(start);
    true
}

pub(super) fn render_health_panel_sections(
    sections: HealthPanelSections,
    area: Rect,
    buffer: &mut Buffer,
) {
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
        .wrap(Wrap { trim: false })
        .render(*chunk, buffer);
    }
}

pub(super) fn summary_line(
    report: &HealthScanReport,
    i18n: crate::I18n,
    allow_meter: bool,
) -> HealthBannerLine {
    let severity = health_display_severity(report);
    let mut spans = vec![Span::styled(
        health_status_label(report, i18n),
        health_status_style(report).add_modifier(Modifier::BOLD),
    )];
    if allow_meter && meter_visible_for_severity(severity) {
        if let Some(score) = report.health_score {
            let meter = MeterModel::health_score(score).render(HEALTH_METER_MEDIUM_WIDTH);
            spans.push(Span::raw(format!("  {score}/100 {meter}")));
        }
    }
    if report.elapsed_ms > 0 && !has_resource_facts(report) {
        spans.push(Span::raw(format!("  {}ms", report.elapsed_ms)));
    }
    HealthBannerLine { spans }
}

pub(super) fn metric_band_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    content_width: usize,
    allow_meter: bool,
) -> Vec<HealthBannerLine> {
    let mut pressure = Vec::new();
    let mut resources = Vec::new();
    for mut cell in metric_cells(report, i18n, false) {
        let label = match cell.kind {
            HealthMetricKind::Load => {
                pressure.push(cell);
                continue;
            }
            HealthMetricKind::CpuUsed => crate::MessageId::HealthMetricCpu,
            HealthMetricKind::MemoryUsed => crate::MessageId::HealthMetricMemory,
            HealthMetricKind::SwapUsed => crate::MessageId::HealthMetricSwap,
            HealthMetricKind::DiskUsed => crate::MessageId::HealthMetricDisk,
        };
        if cell.kind == HealthMetricKind::DiskUsed {
            cell.label = cell
                .label
                .strip_prefix(i18n.t(label))
                .unwrap_or(&cell.label)
                .trim_start()
                .to_string();
        } else if cell.direction != MetricDirection::Available {
            cell.label = i18n.t(crate::MessageId::HealthMetricUsed).to_string();
        }
        resources.extend(metric_lane_lines(
            i18n.t(label),
            vec![cell],
            report.overall_severity,
            content_width,
            allow_meter,
            true,
        ));
    }
    let mut lines = metric_lane_lines(
        i18n.t(crate::MessageId::HealthMetricPressure),
        pressure,
        report.overall_severity,
        content_width,
        allow_meter,
        true,
    );
    lines.extend(resources);
    lines
}

pub(super) fn metric_lane_lines(
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
    if allow_second_line
        && meter_width.is_none()
        && cells.iter().any(|cell| cell.quantity.is_some())
    {
        for cell in &cells {
            let mut primary = cell.clone();
            if display_width(&cell.render()) > body_width {
                primary.quantity = None;
            }
            lines.push(metric_lane_line_from_cells(
                label,
                std::slice::from_ref(&primary),
                None,
                body_width,
            ));
            if primary.quantity.is_none() {
                if let Some(quantity) = &cell.quantity {
                    for text in wrap_plain_line(quantity, body_width.max(1)) {
                        lines.push(HealthBannerLine {
                            spans: vec![Span::styled(
                                format!("{}{text}", " ".repeat(label_width)),
                                Style::default().fg(Color::Reset),
                            )],
                        });
                    }
                }
            }
            if lines.len() >= HEALTH_MAX_BODY_ROWS {
                lines.truncate(HEALTH_MAX_BODY_ROWS);
                return lines;
            }
        }
        return lines;
    }
    let max_rows = if allow_second_line && content_width >= 76 {
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

pub(super) fn fitting_metric_prefix_len(
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

pub(super) fn metric_lane_line_from_cells(
    label: &str,
    cells: &[HealthMetricCell],
    meter_width: Option<usize>,
    body_width: usize,
) -> HealthBannerLine {
    let text = join_metric_cells(cells, meter_width);
    let mut spans = vec![Span::styled(
        format!("{label}  "),
        Style::default()
            .fg(Color::Reset)
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

pub(super) fn section_line(title: &str, content_width: usize) -> HealthBannerLine {
    let tail = "─".repeat(content_width.saturating_sub(display_width(title) + 1));
    HealthBannerLine {
        spans: vec![
            Span::styled(
                title.to_string(),
                Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {tail}"), Style::default().fg(Color::Gray)),
        ],
    }
}
