use super::*;

pub(super) fn load_metric_cell(
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

pub(super) fn should_show_swap_metric(report: &HealthScanReport) -> bool {
    report.findings.iter().any(|finding| {
        matches!(
            finding.title_id,
            HealthMessageId::HealthFindingSwapPressure
                | HealthMessageId::HealthFindingMemoryAvailableLow
                | HealthMessageId::HealthFindingRecentOom
        )
    })
}

pub(super) fn dynamic_load_5m_cell(
    report: &HealthScanReport,
    i18n: crate::I18n,
) -> Option<HealthMetricCell> {
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

pub(super) fn metric_cells(report: &HealthScanReport, i18n: crate::I18n) -> Vec<HealthMetricCell> {
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
        let mut cell = metric_cell_with_ratio(
            i18n.t(crate::MessageId::HealthMetricMemoryUsed),
            value,
            MetricDirection::Used,
            HealthMetricKind::MemoryUsed,
        );
        if let Some(available) = number_fact(&report.facts, "memory.available_mib") {
            let gib = format!("{:.1}", available / 1024.0);
            let mut quantity = i18n.format(
                crate::MessageId::HealthEvidenceDiskAvailable,
                &[("gib", &gib)],
            );
            if let Some(total) = number_fact(&report.facts, "memory.total_mib") {
                quantity.push_str(&format!(" / {:.1} GiB", total / 1024.0));
            }
            cell.quantity = Some(quantity);
        }
        cells.push(cell);
    } else if let Some(available) = number_fact(&report.facts, "memory.available_mib") {
        let gib = format!("{:.1}", available / 1024.0);
        cells.push(HealthMetricCell::new(
            "",
            i18n.format(
                crate::MessageId::HealthEvidenceDiskAvailable,
                &[("gib", &gib)],
            ),
            MetricDirection::Available,
            HealthMetricKind::MemoryUsed,
        ));
    }
    if let Some((mount, value)) = root_disk_metric(report).or_else(|| riskiest_disk_metric(report))
    {
        let label = i18n.format(
            crate::MessageId::HealthMetricDiskMountUsed,
            &[("mount", &middle_ellipsis(mount, 26))],
        );
        let mut cell = metric_cell_with_ratio(
            &label,
            value,
            MetricDirection::Used,
            HealthMetricKind::DiskUsed,
        );
        let available = if root_disk_metric(report).is_some() {
            number_fact(&report.facts, "filesystem.root_available_gib")
        } else {
            number_fact(&report.facts, "filesystem.available_gib")
        };
        if let Some(available) = available {
            let gib = format!("{available:.1}");
            cell.quantity = Some(i18n.format(
                crate::MessageId::HealthEvidenceDiskAvailable,
                &[("gib", &gib)],
            ));
        }
        cells.push(cell);
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

pub(super) fn should_show_riskiest_disk_metric(report: &HealthScanReport) -> bool {
    report
        .findings
        .iter()
        .any(|finding| finding.title_id == HealthMessageId::HealthFindingDiskHigh)
}

pub(super) fn root_disk_metric(report: &HealthScanReport) -> Option<(&str, f64)> {
    number_fact(&report.facts, "filesystem.root_used_ratio").map(|value| ("/", value))
}

pub(super) fn riskiest_disk_metric(report: &HealthScanReport) -> Option<(&str, f64)> {
    let value = number_fact(&report.facts, "filesystem.max_used_ratio")?;
    let mount = string_fact(&report.facts, "filesystem.riskiest_mount").unwrap_or("?");
    Some((mount, value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetricDirection {
    LoadPerCore,
    Available,
    Used,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HealthMetricKind {
    Load,
    CpuUsed,
    MemoryUsed,
    SwapUsed,
    DiskUsed,
}

#[derive(Debug, Clone)]
pub(super) struct HealthMetricCell {
    pub(super) label: String,
    pub(super) value: String,
    pub(super) direction: MetricDirection,
    pub(super) kind: HealthMetricKind,
    pub(super) ratio: Option<f64>,
    pub(super) quantity: Option<String>,
}

impl HealthMetricCell {
    pub(super) fn new(
        label: &str,
        value: String,
        direction: MetricDirection,
        kind: HealthMetricKind,
    ) -> Self {
        Self {
            label: label.to_string(),
            value,
            direction,
            kind,
            ratio: None,
            quantity: None,
        }
    }

    pub(super) fn with_ratio(mut self, ratio: f64) -> Self {
        self.ratio = Some(ratio);
        self
    }

    pub(super) fn render(&self) -> String {
        self.render_with_meter(None)
    }

    pub(super) fn render_with_meter(&self, meter_width: Option<usize>) -> String {
        let mut text = if self.label.is_empty() {
            self.value.clone()
        } else {
            format!("{} {}", self.label, self.value)
        };
        if let Some(quantity) = &self.quantity {
            text.push_str(&format!(" ({quantity})"));
        }
        if let Some(meter) = self.meter(meter_width) {
            format!("{text} {meter}")
        } else {
            text
        }
    }

    pub(super) fn meter(&self, meter_width: Option<usize>) -> Option<String> {
        if self.direction == MetricDirection::LoadPerCore {
            return None;
        }
        let ratio = self.ratio?;
        let width = meter_width?;
        Some(MeterModel::ratio(ratio).render(width))
    }

    pub(super) fn value_style(&self) -> Style {
        let Some(ratio) = self.ratio else {
            return Style::default().fg(Color::Reset);
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
            _ => Color::Reset,
        };
        Style::default().fg(color)
    }
}

pub(super) fn metric_cell_with_ratio(
    label: &str,
    value: f64,
    direction: MetricDirection,
    kind: HealthMetricKind,
) -> HealthMetricCell {
    HealthMetricCell::new(label, percent(value), direction, kind).with_ratio(value)
}

pub(super) fn metric_cell_spans(
    cells: &[HealthMetricCell],
    meter_width: Option<usize>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (idx, cell) in cells.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        if !cell.label.is_empty() {
            spans.push(Span::styled(
                format!("{} ", cell.label),
                Style::default().fg(Color::Reset),
            ));
        }
        spans.push(Span::styled(cell.value.clone(), cell.value_style()));
        if let Some(quantity) = &cell.quantity {
            spans.push(Span::styled(format!(" ({quantity})"), cell.value_style()));
        }
        if let Some(meter) = cell.meter(meter_width) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(meter, cell.value_style()));
        }
    }
    spans
}

pub(super) fn metric_cell_value_style(cell: Option<&HealthMetricCell>) -> Style {
    cell.map(HealthMetricCell::value_style)
        .unwrap_or_else(|| Style::default().fg(Color::Reset))
}

pub(super) fn middle_ellipsis(text: &str, max_width: usize) -> String {
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
pub(super) enum MeterOrientation {
    HigherIsBetter,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MeterModel {
    pub(super) value_ratio: f64,
    pub(super) orientation: MeterOrientation,
}

impl MeterModel {
    pub(super) fn health_score(score: u8) -> Self {
        Self {
            value_ratio: f64::from(score) / 100.0,
            orientation: MeterOrientation::HigherIsBetter,
        }
    }

    pub(super) fn ratio(value_ratio: f64) -> Self {
        Self {
            value_ratio,
            orientation: MeterOrientation::HigherIsBetter,
        }
    }

    pub(super) fn render(&self, width: usize) -> String {
        let ratio = match self.orientation {
            MeterOrientation::HigherIsBetter => self.value_ratio,
        };
        let filled = (ratio.clamp(0.0, 1.0) * width as f64).ceil() as usize;
        let filled = filled.min(width);
        format!("▕{}{}▏", "█".repeat(filled), "░".repeat(width - filled))
    }
}

pub(super) fn join_metric_cells(cells: &[HealthMetricCell], meter_width: Option<usize>) -> String {
    cells
        .iter()
        .map(|cell| cell.render_with_meter(meter_width))
        .collect::<Vec<_>>()
        .join("  ")
}

pub(super) fn metric_meter_width(
    content_width: usize,
    _severity: HealthSeverity,
    allow_meter: bool,
) -> Option<usize> {
    if !allow_meter {
        return None;
    }
    if content_width >= 116 {
        Some(HEALTH_METER_WIDE_WIDTH)
    } else if content_width >= 76 {
        Some(HEALTH_METER_MEDIUM_WIDTH)
    } else {
        None
    }
}

pub(super) fn join_text_cells(cells: &[String]) -> String {
    cells.join("  ")
}

pub(super) fn percent(value: f64) -> String {
    format!("{:.0}%", (value * 100.0).clamp(0.0, 999.0))
}

pub(super) fn truncate_display_width(text: &str, max_width: usize) -> String {
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

pub(super) fn meter_visible_for_severity(severity: HealthSeverity) -> bool {
    matches!(
        severity,
        HealthSeverity::Degraded | HealthSeverity::Warning | HealthSeverity::Critical
    )
}
