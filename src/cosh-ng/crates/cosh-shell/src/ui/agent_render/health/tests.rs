use super::*;
use crate::diagnostics::health::model::{HealthCheckResult, HealthCheckStatus};
use crate::diagnostics::health::{HealthFactCategory, HealthFactSource};

fn p6_resource_report(kernel_gap: Option<HealthUnavailableReason>) -> HealthScanReport {
    use crate::diagnostics::health::{HealthCollector, UnavailableCollector};

    let mut report = HealthScanReport::new("p6-resources", 1000);
    for (key, value, category) in [
        ("cpu.load_1m", 0.0, HealthFactCategory::Cpu),
        ("cpu.load_per_core_1m", 0.0, HealthFactCategory::Cpu),
        ("cpu.cores", 8.0, HealthFactCategory::Cpu),
        ("memory.used_ratio", 0.25, HealthFactCategory::Memory),
        ("memory.available_mib", 6144.0, HealthFactCategory::Memory),
        ("memory.total_mib", 8192.0, HealthFactCategory::Memory),
        ("filesystem.root_used_ratio", 0.16, HealthFactCategory::Disk),
        (
            "filesystem.root_available_gib",
            42.0,
            HealthFactCategory::Disk,
        ),
    ] {
        report.facts.push(HealthFact {
            id: key.to_string(),
            key: key.to_string(),
            category,
            value: HealthFactValue::Float(value),
            unit: None,
            source: HealthFactSource::Fixture,
            elapsed_ms: 1,
        });
    }
    for (id, collector, prefix) in [
        ("resource.load", HealthCollector::Cpu, "cpu."),
        ("resource.memory", HealthCollector::Memory, "memory."),
        ("filesystem.root", HealthCollector::Disk, "filesystem."),
    ] {
        report.check_results.push(HealthCheckResult {
            id: id.to_string(),
            collector: Some(collector),
            status: HealthCheckStatus::Ok,
            fact_ids: report
                .facts
                .iter()
                .filter(|fact| fact.key.starts_with(prefix))
                .map(|fact| fact.id.clone())
                .collect(),
            gaps: vec![],
            elapsed_ms: 1,
        });
    }
    if let Some(reason) = kernel_gap {
        report.check_results.push(HealthCheckResult {
            id: "kernel.oom".to_string(),
            collector: Some(HealthCollector::KernelSignal),
            status: HealthCheckStatus::Unavailable,
            fact_ids: vec![],
            gaps: vec![UnavailableCollector {
                collector: HealthCollector::KernelSignal,
                reason,
                severity: HealthSeverity::Unavailable,
                elapsed_ms: 1,
            }],
            elapsed_ms: 1,
        });
    }
    report.recompute_overall_severity();
    report
}

#[test]
fn p6_resource_banner_marks_incomplete_checks_without_technical_noise() {
    for reason in [
        HealthUnavailableReason::PermissionDenied,
        HealthUnavailableReason::ParseError,
    ] {
        let report = p6_resource_report(Some(reason));
        let original = report.clone();
        assert_eq!(report.resource_severity, HealthSeverity::Ok);
        assert_eq!(report.overall_severity, HealthSeverity::Unavailable);
        for (language, expected) in [
            (crate::Language::EnUs, "some checks incomplete"),
            (crate::Language::ZhCn, "部分检查未完成"),
        ] {
            let renderer = RatatuiInlineRenderer::with_width(120).with_language(language);
            let text = renderer
                .health_banner_lines(HealthBannerModel::new(&report))
                .join("\n");
            assert!(text.contains(expected), "{text}");
            let i18n = crate::I18n::new(language);
            assert!(health_startup_row_text(&report, i18n).contains(expected));
            assert!(summary_line(&report, i18n, false)
                .plain_text()
                .contains(expected));
            assert_ne!(health_status_style(&report).fg, Some(Color::Green));
            assert!(!text.contains("permission denied"), "{text}");
            assert!(!text.contains("权限不足"), "{text}");
            assert!(!text.contains("Unavailable:"), "{text}");
            assert!(!text.contains("parse error"), "{text}");
            assert!(!text.contains("解析错误"), "{text}");
            assert!(text.contains("25%"), "{text}");
            assert!(text.contains("16%"), "{text}");
        }
        assert_eq!(report, original);
        assert_eq!(
            crate::diagnostics::health::report_exit_code(&report),
            1,
            "passive rendering must preserve the doctor contract"
        );
    }
}

#[test]
fn p6_resource_banner_shows_available_quantities_for_normal_and_partial_scans() {
    for gap in [None, Some(HealthUnavailableReason::PermissionDenied)] {
        let report = p6_resource_report(gap);
        let renderer = RatatuiInlineRenderer::with_width(120).with_language(crate::Language::EnUs);
        let text = renderer
            .health_banner_lines(HealthBannerModel::new(&report))
            .join("\n");
        assert!(text.contains("6.0 GiB"), "{text}");
        assert!(text.contains("8.0 GiB"), "{text}");
        assert!(text.contains("42.0 GiB"), "{text}");
        assert!(text.contains("avail"), "{text}");
    }
}

#[test]
fn p6_resource_banner_uses_short_meters_for_normal_and_partial_scans() {
    for gap in [None, Some(HealthUnavailableReason::PermissionDenied)] {
        let report = p6_resource_report(gap);
        let renderer = RatatuiInlineRenderer::with_width(120).with_language(crate::Language::EnUs);
        let text = renderer
            .health_banner_lines(HealthBannerModel::new(&report))
            .join("\n");
        let meters = text.matches('▕').count();
        assert!(meters >= 2, "memory and disk need resource meters: {text}");
        for meter in text.split('▕').skip(1) {
            let body = meter.split('▏').next().expect("meter body");
            assert!(body.chars().count() <= 8, "{text}");
        }
    }
}

#[test]
fn p6_resource_banner_keeps_quantities_without_meters_at_narrow_width() {
    let report = p6_resource_report(Some(HealthUnavailableReason::PermissionDenied));
    for renderer in [
        RatatuiInlineRenderer::with_width(40),
        RatatuiInlineRenderer::plain_with_width(40),
    ] {
        let text = renderer
            .with_language(crate::Language::EnUs)
            .health_banner_lines(HealthBannerModel::new(&report))
            .join("\n");
        let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for quantity in ["25%", "16%", "6.0 GiB", "8.0 GiB", "42.0 GiB"] {
            assert!(flattened.contains(quantity), "missing {quantity}: {text}");
        }
        assert!(!text.contains('▕'), "{text}");
        assert!(text.lines().all(|line| display_width(line) <= 40), "{text}");
    }
}

#[test]
fn health_banner_scopes_ok_to_checked_items_when_other_checks_are_disabled() {
    let mut report = HealthScanReport::new("partial-coverage", 1000);
    report.facts.push(HealthFact {
        id: "root-used".to_string(),
        key: "filesystem.root_used_ratio".to_string(),
        category: HealthFactCategory::Disk,
        value: HealthFactValue::Float(0.42),
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 0,
    });
    for (id, status, fact_ids) in [
        (
            "filesystem.mounts",
            HealthCheckStatus::Ok,
            vec!["root-used".to_string()],
        ),
        ("resource.memory", HealthCheckStatus::Disabled, vec![]),
        ("kernel.oom", HealthCheckStatus::Disabled, vec![]),
    ] {
        report.check_results.push(HealthCheckResult {
            id: id.to_string(),
            collector: None,
            status,
            fact_ids,
            gaps: vec![],
            elapsed_ms: 0,
        });
    }
    let original = report.clone();
    for (language, expected) in [
        (crate::Language::EnUs, "no anomalies in checked items"),
        (crate::Language::ZhCn, "已检查项未见异常"),
    ] {
        let i18n = crate::I18n::new(language);
        let row = health_startup_row_text(&report, i18n);
        let summary = summary_line(&report, i18n, false).plain_text();
        assert!(row.contains(expected), "{row}");
        assert!(summary.contains(expected), "{summary}");
        assert!(unavailable_lines(&report, i18n, 2).is_empty());
    }
    assert_eq!(report, original);

    report.facts.clear();
    report.check_results[0].status = HealthCheckStatus::Disabled;
    let i18n = crate::I18n::new(crate::Language::EnUs);
    assert!(health_startup_row_text(&report, i18n).contains("not checked"));
    assert!(summary_line(&report, i18n, false)
        .plain_text()
        .contains("not checked"));
    report.overall_severity = HealthSeverity::Warning;
    assert_eq!(
        summary_line(&report, i18n, false).plain_text(),
        severity_label(HealthSeverity::Warning, i18n)
    );
}

#[test]
fn health_insights_are_short_observations_without_unmeasured_predictions() {
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        let i18n = crate::I18n::new(language);
        for id in [
            crate::MessageId::HealthInsightMemoryAvailableLow,
            crate::MessageId::HealthInsightDiskHigh,
            crate::MessageId::HealthInsightSwapPressure,
        ] {
            assert!(display_width(i18n.t(id)) <= 58, "{}", i18n.t(id));
        }
    }
    assert_eq!(severity_color(HealthSeverity::Ok), Color::Reset);
}

#[test]
fn resource_collection_gaps_do_not_color_a_normal_banner_as_an_alarm() {
    let report = p6_resource_report(Some(HealthUnavailableReason::PermissionDenied));
    let area = Rect::new(0, 0, 100, 8);
    let mut buffer = Buffer::empty(area);
    let i18n = crate::I18n::new(crate::Language::EnUs);
    let sections = health_panel_sections(&report, i18n, 100, true);
    render_health_banner(&report, i18n, sections, area, &mut buffer);
    assert_eq!(buffer[(0, 0)].fg, Color::Gray);
    assert_eq!(report.overall_severity, HealthSeverity::Unavailable);
    assert_eq!(crate::diagnostics::health::report_exit_code(&report), 1);
}

#[test]
fn resource_categories_stay_on_separate_rows() {
    let report = p6_resource_report(None);
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        for width in [40, 80, 100, 120] {
            for plain in [false, true] {
                let renderer = if plain {
                    RatatuiInlineRenderer::plain_with_width(width)
                } else {
                    RatatuiInlineRenderer::with_width(width)
                }
                .with_language(language);
                let lines = renderer.health_banner_lines(HealthBannerModel::new(&report));
                assert!(
                    lines
                        .iter()
                        .all(|line| !(line.contains("25%") && line.contains("16%"))),
                    "{width}: {lines:?}"
                );
                assert!(
                    lines
                        .iter()
                        .all(|line| display_width(line) <= width as usize),
                    "{lines:?}"
                );
                let text = lines.join("\n");
                for value in ["25%", "16%", "6.0", "42.0"] {
                    assert!(text.contains(value), "missing {value}: {text}");
                }
            }
        }
    }
    assert_health_card_status_appears_once_at_compact_and_wide_widths();
    assert_plain_health_card_status_appears_once_at_compact_and_wide_widths();
}

#[test]
fn combined_entry_colors_both_commands_and_splits_between_actions() {
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        let i18n = crate::I18n::new(language);
        let wide = health_guide_lines(i18n, 76, true);
        assert_eq!(wide.len(), 1);
        let commands = wide[0]
            .spans
            .iter()
            .filter(|span| span.content.starts_with("cosh"))
            .collect::<Vec<_>>();
        assert_eq!(commands.len(), 2);
        for command in commands {
            assert_eq!(command.style.fg, Some(Color::Cyan));
            assert!(command.style.add_modifier.contains(Modifier::BOLD));
        }
        assert_eq!(health_guide_lines(i18n, 36, true).len(), 2);
        let guide_only = health_guide_lines(i18n, 76, false);
        assert!(!guide_only[0].plain_text().contains("AI"));
    }
}

#[test]
fn healthy_card_colors_only_observed_status_green() {
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        for gap in [None, Some(HealthUnavailableReason::PermissionDenied)] {
            let report = p6_resource_report(gap);
            let i18n = crate::I18n::new(language);
            let sections = health_panel_sections(&report, i18n, 80, true);
            let area = Rect::new(0, 0, 80, sections.body_len() as u16 + 2);
            let mut buffer = Buffer::empty(area);
            render_health_banner(&report, i18n, sections, area, &mut buffer);
            let green = buffer
                .content
                .iter()
                .filter(|cell| cell.fg == Color::Green)
                .map(|cell| cell.symbol())
                .collect::<String>();
            if gap.is_some() {
                assert!(green.trim().is_empty(), "{green}");
            } else {
                assert_eq!(green.trim(), health_status_label(&report, i18n));
            }
            assert_eq!(buffer[(0, 0)].fg, Color::Gray);
            assert!(buffer
                .content
                .iter()
                .filter(|cell| cell.symbol() == "█")
                .all(|cell| cell.fg == Color::Reset));
        }
    }
    let mut report = HealthScanReport::new("not-checked", 500);
    for severity in [
        HealthSeverity::Ok,
        HealthSeverity::Warning,
        HealthSeverity::Critical,
    ] {
        report.overall_severity = severity;
        let i18n = crate::I18n::new(crate::Language::EnUs);
        let sections = health_panel_sections(&report, i18n, 80, true);
        let area = Rect::new(0, 0, 80, sections.body_len() as u16 + 2);
        let mut buffer = Buffer::empty(area);
        render_health_banner(&report, i18n, sections, area, &mut buffer);
        assert!(!buffer.content.iter().any(|cell| cell.fg == Color::Green));
        if severity != HealthSeverity::Ok {
            assert_eq!(buffer[(0, 0)].fg, severity_color(severity));
        }
    }
    assert_health_card_keeps_status_for_findings_with_and_without_resource_facts();
}

#[test]
fn narrow_resource_rows_keep_one_label_and_complete_quantities() {
    let report = p6_resource_report(None);
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        let lines = metric_band_lines(&report, crate::I18n::new(language), 36, false);
        let text = lines
            .iter()
            .map(HealthBannerLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n");
        let label = crate::I18n::new(language).t(crate::MessageId::HealthMetricMemory);
        assert_eq!(text.matches(label).count(), 1, "{text}");
        assert!(text.lines().any(|line| line.contains("6.0 GiB")), "{text}");
        assert!(text.lines().any(|line| line.contains("8.0 GiB")), "{text}");
        assert!(text.lines().all(|line| display_width(line) <= 36), "{text}");
        let quantity = lines
            .iter()
            .find(|line| line.plain_text().contains("6.0 GiB"))
            .unwrap();
        assert!(quantity
            .spans
            .iter()
            .filter(|span| span.content.contains("GiB"))
            .all(|span| span.style.fg != Some(Color::Red)));
    }
}

#[test]
fn partial_memory_keeps_available_capacity_without_inventing_used_percentage() {
    let mut report = p6_resource_report(None);
    report.facts.retain(|fact| fact.key != "memory.used_ratio");
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        let text = metric_band_lines(&report, crate::I18n::new(language), 76, true)
            .iter()
            .map(HealthBannerLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("6.0 GiB"), "{text}");
        assert!(!text.contains("25%"), "{text}");
        let memory = metric_cells(&report, crate::I18n::new(language), false)
            .into_iter()
            .find(|cell| cell.render().contains("6.0 GiB"))
            .unwrap();
        assert!(memory.ratio.is_none());
        assert!(!memory.render().contains("used") && !memory.render().contains("已用"));
    }
}

#[test]
fn guide_banner_with_only_load_metric_does_not_repeat_the_load_label() {
    let mut report = p6_resource_report(None);
    report.facts.retain(|fact| fact.key.starts_with("cpu."));
    report
        .check_results
        .retain(|check| check.id == "resource.load");
    report.recompute_overall_severity();
    assert!(health_uses_startup_row(&report));

    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        let renderer = RatatuiInlineRenderer::with_width(120).with_language(language);
        let startup = renderer
            .health_banner_lines(HealthBannerModel::new(&report))
            .join("\n");
        assert!(startup.contains("0.0 / 8"), "{startup}");
        for width in [40, 80, 120] {
            let guided = RatatuiInlineRenderer::with_width(width)
                .with_language(language)
                .health_banner_lines(HealthBannerModel::new(&report).with_guide())
                .join("\n");
            assert!(!guided.contains("负载  1分钟负载"), "{guided}");
            assert!(!guided.contains("Load  Load"), "{guided}");
            assert!(guided.contains("0.0 / 8"), "{guided}");
        }
    }
}

fn assert_health_card_status_appears_once_at_compact_and_wide_widths() {
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        for gap in [None, Some(HealthUnavailableReason::PermissionDenied)] {
            let report = p6_resource_report(gap);
            let i18n = crate::I18n::new(language);
            let status = health_status_label(&report, i18n);
            for width in [40, 80, 120] {
                let lines = RatatuiInlineRenderer::with_width(width)
                    .with_language(language)
                    .health_banner_lines(HealthBannerModel::new(&report));
                let text = lines.join("\n");
                assert_eq!(text.matches(&status).count(), 1, "{width}: {text}");
                assert!(
                    lines[0].contains(i18n.t(crate::MessageId::HealthBannerTitle)),
                    "{text}"
                );
                assert!(
                    lines
                        .iter()
                        .all(|line| display_width(line) <= width as usize),
                    "{text}"
                );
                let status_prefix = status.chars().take(5).collect::<String>();
                assert!(
                    !lines[0].contains(&status_prefix) || lines[0].contains(&status),
                    "truncated status in title: {text}"
                );
            }
        }
    }
}

fn assert_plain_health_card_status_appears_once_at_compact_and_wide_widths() {
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        for gap in [None, Some(HealthUnavailableReason::PermissionDenied)] {
            let report = p6_resource_report(gap);
            let status = health_status_label(&report, crate::I18n::new(language));
            for width in [40, 80, 120] {
                let lines = RatatuiInlineRenderer::plain_with_width(width)
                    .with_language(language)
                    .health_banner_lines(HealthBannerModel::new(&report));
                let text = lines.join("\n");
                let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
                assert_eq!(flattened.matches(&status).count(), 1, "{width}: {text}");
                assert!(
                    lines
                        .iter()
                        .all(|line| display_width(line) <= width as usize),
                    "{text}"
                );
            }
        }
    }
}

library_tests! {
fn matrix_abnormal_report() -> HealthScanReport {
    let mut report = p6_resource_report(None);
    report.findings.push(HealthFinding {
        id: "service.failed:sshd".to_string(),
        severity: HealthSeverity::Critical,
        category: crate::diagnostics::health::HealthFindingCategory::RootCause,
        title_id: HealthMessageId::HealthFindingServiceFailed,
        detail_id: None,
        detail_args: [
            ("service".to_string(), "sshd".to_string()),
            ("observed".to_string(), "failed".to_string()),
            ("expected".to_string(), "active".to_string()),
        ]
        .into(),
        evidence_fact_ids: vec![],
        suggested_try_ids: vec![],
    });
    report.recompute_overall_severity();
    report
}

fn matrix_long_event_report() -> HealthScanReport {
    let mut report = p6_resource_report(None);
    for (key, value) in [
        (
            "kernel.oom_latest_age_seconds",
            HealthFactValue::Float(180.0),
        ),
        (
            "kernel.oom_killed_process",
            HealthFactValue::String("python3".to_string()),
        ),
        ("kernel.oom_latest_pid", HealthFactValue::Float(49991.0)),
        (
            "kernel.oom_latest_constraint",
            HealthFactValue::String("CONSTRAINT_MEMCG".to_string()),
        ),
        (
            "kernel.oom_event_count_last_1h",
            HealthFactValue::Float(12.0),
        ),
    ] {
        report.facts.push(HealthFact {
            id: key.to_string(),
            key: key.to_string(),
            category: HealthFactCategory::Kernel,
            value,
            unit: None,
            source: HealthFactSource::Fixture,
            elapsed_ms: 1,
        });
    }
    report.findings.push(HealthFinding {
        id: "kernel.oom".to_string(),
        severity: HealthSeverity::Critical,
        category: crate::diagnostics::health::HealthFindingCategory::Anomaly,
        title_id: HealthMessageId::HealthFindingRecentOom,
        detail_id: None,
        detail_args: BTreeMap::new(),
        evidence_fact_ids: vec![
            "kernel.oom_latest_age_seconds".to_string(),
            "kernel.oom_killed_process".to_string(),
            "kernel.oom_latest_pid".to_string(),
            "kernel.oom_latest_constraint".to_string(),
            "kernel.oom_event_count_last_1h".to_string(),
        ],
        suggested_try_ids: vec![],
    });
    report.recompute_overall_severity();
    report
}

#[test]
fn health_acceptance_matrix_keeps_one_complete_status_across_terminal_modes() {
    let scenarios = [
        ("normal", p6_resource_report(None)),
        (
            "partial",
            p6_resource_report(Some(HealthUnavailableReason::PermissionDenied)),
        ),
        ("abnormal", matrix_abnormal_report()),
        ("long-event", matrix_long_event_report()),
    ];
    for (name, report) in &scenarios {
        for language in [crate::Language::EnUs, crate::Language::ZhCn] {
            let i18n = crate::I18n::new(language);
            let status = health_status_label(report, i18n);
            let status_prefix = status.chars().take(5).collect::<String>();
            for width in [40u16, 80, 120] {
                for (mode, styled) in [("ansi", true), ("no-color", false)] {
                    for guide in [false, true] {
                        let mut renderer = RatatuiInlineRenderer::with_width(width);
                        renderer.styled = styled;
                        let mut model = HealthBannerModel::new(report);
                        if guide {
                            model = model.with_guide();
                        }
                        let mut output = Vec::new();
                        renderer
                            .with_language(language)
                            .write_health_banner(&mut output, model)
                            .unwrap();
                        let raw = String::from_utf8(output).unwrap();
                        let text = crate::ui::wrap::strip_ansi_escape(&raw);
                        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
                        let cell = format!("{name}/{language:?}/{width}px/{mode}/guide={guide}");

                        assert_eq!(
                            flat.matches(&status).count(),
                            1,
                            "{cell}: status {status:?} must appear exactly once:\n{text}"
                        );
                        let lines = text.lines().collect::<Vec<_>>();
                        assert!(
                            !lines[0].contains(&status_prefix) || lines[0].contains(&status),
                            "{cell}: title truncates the status:\n{text}"
                        );
                        for line in &lines {
                            assert!(
                                display_width(line) <= width as usize,
                                "{cell}: line wider than {width}:\n{text}"
                            );
                        }
                        assert_eq!(
                            raw.contains('\u{1b}'),
                            styled,
                            "{cell}: ANSI presence must follow the style mode:\n{raw:?}"
                        );
                        for quantity in ["25%", "16%", "6.0 GiB", "42.0 GiB"] {
                            assert!(
                                flat.contains(quantity),
                                "{cell}: resource data {quantity} dropped:\n{text}"
                            );
                        }
                        assert_eq!(
                            flat.contains("cosh guide"),
                            guide,
                            "{cell}: guide lines must follow the Bash/Cosh entry:\n{text}"
                        );
                        match *name {
                            "abnormal" => assert!(
                                flat.contains("sshd"),
                                "{cell}: finding content dropped:\n{text}"
                            ),
                            "long-event" => {
                                assert!(
                                    flat.contains("python3") && flat.contains("49991"),
                                    "{cell}: long event evidence dropped:\n{text}"
                                );
                                if width == 40 {
                                    let first = lines
                                        .iter()
                                        .position(|line| line.contains("python3"))
                                        .expect("event subject row");
                                    let continuation = lines
                                        .iter()
                                        .position(|line| line.contains("cgroup"))
                                        .expect("event continuation row");
                                    assert!(
                                        continuation > first,
                                        "{cell}: long event must wrap onto following rows:\n{text}"
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}
}

fn assert_health_card_keeps_status_for_findings_with_and_without_resource_facts() {
    let mut report = HealthScanReport::new("service-only", 1000);
    report.overall_severity = HealthSeverity::Warning;
    report.findings.push(HealthFinding {
        id: "service.failed:sshd".to_string(),
        severity: HealthSeverity::Warning,
        category: crate::diagnostics::health::HealthFindingCategory::RootCause,
        title_id: HealthMessageId::HealthFindingServiceFailed,
        detail_id: None,
        detail_args: [("service".to_string(), "sshd".to_string())].into(),
        evidence_fact_ids: vec![],
        suggested_try_ids: vec![],
    });
    let mut partial = p6_resource_report(Some(HealthUnavailableReason::PermissionDenied));
    let mut gap = report.findings[0].clone();
    gap.category = crate::diagnostics::health::HealthFindingCategory::CollectionGap;
    gap.title_id = HealthMessageId::HealthFindingCoreCollectorUnavailable;
    partial.findings.push(gap);
    for report in [report, partial] {
        for language in [crate::Language::ZhCn, crate::Language::EnUs] {
            let i18n = crate::I18n::new(language);
            for width in [40, 80] {
                let sections = health_panel_sections(&report, i18n, width, true);
                let area = Rect::new(0, 0, width, sections.body_len() as u16 + 2);
                let mut buffer = Buffer::empty(area);
                render_health_banner(&report, i18n, sections, area, &mut buffer);
                let text = buffer_to_lines(&buffer, area).join("\n");
                assert!(text.contains(&health_status_label(&report, i18n)), "{text}");
            }
        }
    }
}
