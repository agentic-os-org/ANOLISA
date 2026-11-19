use super::*;

mod evidence_facts;
pub(super) use evidence_facts::format_evidence_fact;

pub(super) fn finding_insight_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    limit: usize,
    content_width: usize,
) -> Vec<HealthBannerLine> {
    if limit == 0 {
        return Vec::new();
    }
    let body_width = content_width.saturating_sub(2).max(1);
    let selected = sorted_findings(report)
        .into_iter()
        .take(limit.min(HEALTH_MAX_VISIBLE_FINDINGS))
        .collect::<Vec<_>>();
    let mut evidence_budget = HEALTH_MAX_FINDING_ROWS.saturating_sub(1 + selected.len());
    let mut lines = Vec::new();
    for finding in selected {
        let insight_id = insight_id_for_finding(finding);
        let mut title = if insight_id == HealthMessageId::HealthInsightGeneric {
            format_health_message(i18n, finding.title_id, &finding.detail_args)
        } else {
            insight_text_for_finding(finding, insight_id, i18n)
        };
        if let Some(service) = finding
            .detail_args
            .get("service")
            .filter(|_| display_width(&title) > body_width)
        {
            title = format!(
                "{}: {service}",
                format_health_message(i18n, finding.title_id, &finding.detail_args)
            );
        }
        let title = truncate_display_width(&title, body_width);
        lines.push(HealthBannerLine {
            spans: vec![
                Span::styled("• ", severity_style(finding.severity)),
                Span::raw(title),
            ],
        });
        if evidence_budget == 0 {
            continue;
        }
        let budget = evidence_budget.min(2);
        let evidence = if finding.title_id == HealthMessageId::HealthFindingRecentOom {
            oom_summary_evidence(report, i18n, body_width, budget)
        } else {
            evidence_text(report, finding, i18n, body_width)
        };
        let rows = wrap_plain_line(&evidence, body_width);
        let count = if evidence.is_empty() || rows.len() > budget {
            0
        } else {
            rows.len()
        };
        lines.extend(rows.into_iter().take(count).map(|line| HealthBannerLine {
            spans: vec![
                Span::raw("  "),
                Span::styled(line, Style::default().fg(Color::Gray)),
            ],
        }));
        evidence_budget -= count;
    }
    lines
}

fn oom_summary_evidence(
    report: &HealthScanReport,
    i18n: crate::I18n,
    width: usize,
    rows: usize,
) -> String {
    let mut pieces = Vec::new();
    if let Some(age) = number_fact(&report.facts, "kernel.oom_latest_age_seconds") {
        pieces.push(format_oom_age(age as u64, i18n));
    }
    let process = string_fact(&report.facts, "kernel.oom_killed_process")
        .or_else(|| string_fact(&report.facts, "kernel.oom_latest_process"));
    let pid = number_fact(&report.facts, "kernel.oom_latest_pid");
    let subject = match (process, pid) {
        (Some(process), Some(pid)) => format!("{} (PID {pid:.0})", middle_ellipsis(process, 16)),
        (Some(process), None) => middle_ellipsis(process, 16),
        (None, Some(pid)) => format!("PID {pid:.0}"),
        (None, None) => String::new(),
    };
    if !subject.is_empty() {
        pieces.push(subject);
    }
    let scope_key = if string_fact(&report.facts, "kernel.oom_latest_scope_label_id").is_some() {
        "kernel.oom_latest_scope_label_id"
    } else {
        "kernel.oom_latest_constraint"
    };
    for key in [scope_key, "kernel.oom_event_count_last_1h"] {
        if let Some(fact) = report.facts.iter().find(|fact| fact.key == key) {
            if let Some(text) = format_evidence_fact(fact, report, i18n) {
                if !pieces.contains(&text) {
                    pieces.push(text);
                }
            }
        }
    }
    let mut text = String::new();
    for piece in pieces {
        let candidate = if text.is_empty() {
            piece
        } else {
            format!("{text} · {piece}")
        };
        if wrap_plain_line(&candidate, width).len() <= rows {
            text = candidate;
        }
    }
    text
}

pub(super) fn top_finding(report: &HealthScanReport) -> Option<&HealthFinding> {
    sorted_findings(report).into_iter().next()
}

pub(super) fn insight_id_for_finding(finding: &HealthFinding) -> HealthMessageId {
    match finding.title_id {
        HealthMessageId::HealthFindingMemoryAvailableLow => {
            HealthMessageId::HealthInsightMemoryAvailableLow
        }
        HealthMessageId::HealthFindingDiskHigh => HealthMessageId::HealthInsightDiskHigh,
        HealthMessageId::HealthFindingMountReadonly => HealthMessageId::HealthInsightMountReadonly,
        HealthMessageId::HealthFindingRecentOom => HealthMessageId::HealthInsightRecentOom,
        HealthMessageId::HealthFindingCpuLoadHigh => HealthMessageId::HealthInsightCpuLoadHigh,
        HealthMessageId::HealthFindingSwapPressure => HealthMessageId::HealthInsightSwapPressure,
        HealthMessageId::HealthFindingServiceFailed
        | HealthMessageId::HealthFindingServiceInactive => {
            HealthMessageId::HealthInsightServiceState
        }
        HealthMessageId::HealthFindingCgroupMemoryPressure => {
            HealthMessageId::HealthInsightCgroupMemoryPressure
        }
        HealthMessageId::HealthFindingPressureHigh => HealthMessageId::HealthInsightPressureHigh,
        _ => HealthMessageId::HealthInsightGeneric,
    }
}

pub(super) fn insight_text_for_finding(
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
    if finding.title_id == HealthMessageId::HealthFindingPressureHigh {
        return format_health_message(i18n, insight_id, &pressure_args(finding, i18n));
    }
    format_health_message(i18n, insight_id, &finding.detail_args)
}

fn pressure_args(finding: &HealthFinding, i18n: crate::I18n) -> BTreeMap<String, String> {
    let mut args = finding.detail_args.clone();
    if let Some(resource) = finding.detail_args.get("resource") {
        let label = match resource.as_str() {
            "memory" => i18n.t(crate::MessageId::HealthMetricMemory),
            "io" => i18n.t(crate::MessageId::HealthMetricDisk),
            "cpu" => i18n.t(crate::MessageId::HealthMetricCpu),
            _ => resource.as_str(),
        };
        args.insert("resource".into(), label.to_string());
    }
    args
}

pub(super) fn append_evidence_sentence(insight: &str, evidence: &str, i18n: crate::I18n) -> String {
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

pub(super) fn has_service_detail_args(finding: &HealthFinding) -> bool {
    finding.detail_args.contains_key("service")
        && finding.detail_args.contains_key("observed")
        && finding.detail_args.contains_key("expected")
}

pub(super) fn evidence_text(
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
        if let Some(text) = disk_evidence_text(finding, report, i18n) {
            return text;
        }
    }
    if finding.title_id == HealthMessageId::HealthFindingMountReadonly {
        if let Some(text) = disk_evidence_text(finding, report, i18n) {
            return text;
        }
    }
    if finding.title_id == HealthMessageId::HealthFindingPressureHigh {
        if let Some(text) = pressure_evidence_text(finding, i18n) {
            return text;
        }
    }
    if finding.title_id == HealthMessageId::HealthFindingCgroupMemoryPressure {
        if let Some(text) = quota_evidence_text(finding, i18n) {
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

pub(super) fn pieces_with_candidate(pieces: &[String], candidate: &str) -> Vec<String> {
    let mut values = pieces.to_vec();
    values.push(candidate.to_string());
    values
}

fn pressure_evidence_text(finding: &HealthFinding, i18n: crate::I18n) -> Option<String> {
    let args = pressure_args(finding, i18n);
    Some(i18n.format(
        crate::MessageId::HealthEvidencePressureAvg,
        &[
            ("resource", args.get("resource")?),
            ("kind", args.get("kind")?),
            ("avg10", args.get("avg10")?),
            ("avg60", args.get("avg60")?),
        ],
    ))
}

fn quota_evidence_text(finding: &HealthFinding, i18n: crate::I18n) -> Option<String> {
    let mut pieces = Vec::new();
    if finding
        .detail_args
        .get("max_zero")
        .is_some_and(|value| value == "true")
    {
        pieces.push(
            i18n.t(crate::MessageId::HealthEvidenceCgroupQuotaZero)
                .to_string(),
        );
    } else if let (Some(percent), Some(remaining)) = (
        finding.detail_args.get("percent"),
        finding.detail_args.get("remaining_mib"),
    ) {
        pieces.push(i18n.format(
            crate::MessageId::HealthEvidenceCgroupQuota,
            &[("percent", percent), ("remaining_mib", remaining)],
        ));
    }
    if let (Some(avg10), Some(avg60)) = (
        finding.detail_args.get("avg10"),
        finding.detail_args.get("avg60"),
    ) {
        pieces.push(i18n.format(
            crate::MessageId::HealthEvidencePressureAvg,
            &[
                ("resource", i18n.t(crate::MessageId::HealthMetricMemory)),
                ("kind", "full"),
                ("avg10", avg10),
                ("avg60", avg60),
            ],
        ));
    }
    (!pieces.is_empty()).then(|| pieces.join(" · "))
}

pub(super) fn memory_available_evidence_text(
    report: &HealthScanReport,
    i18n: crate::I18n,
) -> Option<String> {
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

pub(super) fn swap_evidence_text(report: &HealthScanReport, i18n: crate::I18n) -> Option<String> {
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

pub(super) fn disk_evidence_text(
    finding: &HealthFinding,
    report: &HealthScanReport,
    i18n: crate::I18n,
) -> Option<String> {
    if let Some(members) = finding.detail_args.get("members") {
        let mount = finding
            .detail_args
            .get("mount")
            .map(String::as_str)
            .unwrap_or("/");
        if members == "readonly" {
            return Some(i18n.format(
                crate::MessageId::HealthEvidenceMountReadonly,
                &[("mount", &middle_ellipsis(mount, 26))],
            ));
        }
        let label = i18n.format(
            crate::MessageId::HealthMetricDiskMountUsed,
            &[("mount", &middle_ellipsis(mount, 26))],
        );
        let mut parts = vec![label];
        if let Some(gib) = finding.detail_args.get("available_gib") {
            parts.push(i18n.format(
                crate::MessageId::HealthEvidenceVolumeAvailable,
                &[("gib", gib)],
            ));
        }
        if members.contains("inode") {
            if let Some(count) = finding.detail_args.get("inode_free") {
                parts.push(i18n.format(
                    crate::MessageId::HealthEvidenceInodeFree,
                    &[("count", count)],
                ));
            }
        }
        let separator = if is_zh(i18n) { "，" } else { "; " };
        return Some(parts.join(separator));
    }
    let scoped = finding.evidence_fact_ids.iter().find_map(|id| {
        let fact = report.facts.iter().find(|fact| {
            &fact.id == id
                && fact.key.starts_with("filesystem.")
                && fact.key.ends_with("used_ratio")
        })?;
        let metadata = report.fact_metadata.get(&fact.id)?;
        let crate::diagnostics::health::model::HealthFactScope::Path(path) = &metadata.scope else {
            return None;
        };
        Some((path.as_str(), number_from_fact(fact)?))
    });
    let (mount, ratio, available) = if let Some((mount, ratio)) = scoped {
        let available = report.facts.iter().find_map(|fact| {
            if !fact.key.starts_with("filesystem.") || !fact.key.ends_with("available_gib") { return None; }
            let metadata = report.fact_metadata.get(&fact.id)?;
            if !matches!(&metadata.scope, crate::diagnostics::health::model::HealthFactScope::Path(path) if path == mount) { return None; }
            number_from_fact(fact)
        });
        (mount, ratio, available)
    } else {
        (
            string_fact(&report.facts, "filesystem.riskiest_mount").unwrap_or("/"),
            number_fact(&report.facts, "filesystem.max_used_ratio")?,
            number_fact(&report.facts, "filesystem.available_gib"),
        )
    };
    let label = i18n.format(
        crate::MessageId::HealthMetricDiskMountUsed,
        &[("mount", &middle_ellipsis(mount, 26))],
    );
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

pub(super) fn oom_evidence_text(report: &HealthScanReport, i18n: crate::I18n) -> Option<String> {
    let attributed_scope = string_fact(&report.facts, "kernel.oom_attributed_scope");
    if attributed_scope.is_none()
        && number_fact(&report.facts, "kernel.cgroup_oom_kill_increment")
            .is_some_and(|value| value > 0.0)
        && string_fact(&report.facts, "kernel.current_cgroup_path").is_some()
    {
        if let (Some(start), Some(end), Some(delta)) = (
            number_fact(&report.facts, "kernel.cgroup_oom_kill_window_start_epoch"),
            number_fact(&report.facts, "kernel.cgroup_oom_kill_window_end_epoch"),
            number_fact(&report.facts, "kernel.cgroup_oom_kill_increment"),
        ) {
            if end >= start {
                let age = format_age_seconds((end - start) as u64);
                return Some(i18n.format(
                    crate::MessageId::HealthEvidenceOomCounterWindow,
                    &[("age", &age), ("count", &format!("{delta:.0}"))],
                ));
            }
        }
    }
    let attributed = attributed_scope.is_some();
    let age = number_fact(
        &report.facts,
        if attributed {
            "kernel.oom_attributed_age_seconds"
        } else {
            "kernel.oom_latest_age_seconds"
        },
    )
    .map(|value| format_age_seconds(value as u64));
    let process = if attributed {
        string_fact(&report.facts, "kernel.oom_attributed_process")
    } else {
        string_fact(&report.facts, "kernel.oom_killed_process")
            .or_else(|| string_fact(&report.facts, "kernel.oom_latest_process"))
    };
    let pid = number_fact(
        &report.facts,
        if attributed {
            "kernel.oom_attributed_pid"
        } else {
            "kernel.oom_latest_pid"
        },
    );
    let scope = match attributed_scope {
        Some("current-cgroup") => Some(
            i18n.t(crate::MessageId::HealthOomScopeCurrentCgroup)
                .to_string(),
        ),
        Some("host") => Some(oom_scope_label_from_id("host", i18n)),
        _ => string_fact(&report.facts, "kernel.oom_latest_scope_label_id")
            .map(|value| oom_scope_label_from_id(value, i18n))
            .or_else(|| {
                string_fact(&report.facts, "kernel.oom_latest_constraint")
                    .map(|value| oom_scope_label_from_constraint(value, i18n))
            }),
    };
    let cgroup = if attributed {
        string_fact(&report.facts, "kernel.oom_attributed_task_cgroup")
    } else {
        string_fact(&report.facts, "kernel.oom_latest_oom_cgroup")
            .or_else(|| string_fact(&report.facts, "kernel.oom_latest_task_cgroup"))
    };

    if age.is_none() && process.is_none() && pid.is_none() && scope.is_none() {
        return None;
    }

    let mut subject = process.unwrap_or("unknown").to_string();
    if let Some(pid) = pid {
        subject = format!("{subject}(PID {:.0})", pid);
    }
    let text = if is_zh(i18n) {
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
        format_oom_evidence_sentence_zh(&victim, scope.as_deref(), cgroup)
    } else {
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
        format_oom_evidence_sentence_en(&victim, scope.as_deref(), cgroup)
    };
    let count = if attributed {
        None
    } else {
        report
            .facts
            .iter()
            .find(|fact| fact.key == "kernel.oom_event_count_last_1h")
            .or_else(|| {
                report
                    .facts
                    .iter()
                    .find(|fact| fact.key == "kernel.oom_event_count_last_24h")
            })
            .filter(|fact| number_from_fact(fact).is_some_and(|count| count > 0.0))
            .and_then(|fact| format_evidence_fact(fact, report, i18n))
    };
    Some(if let Some(count) = count {
        format!("{text}; {count}")
    } else {
        text
    })
}

pub(super) fn format_oom_evidence_sentence_zh(
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

pub(super) fn format_oom_evidence_sentence_en(
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

pub(super) fn unavailable_lines(
    report: &HealthScanReport,
    i18n: crate::I18n,
    limit: usize,
) -> Vec<HealthBannerLine> {
    let labelled = if report.check_results.is_empty() {
        report
            .unavailable
            .iter()
            .map(|item| (item, collector_label(item.collector, i18n)))
            .collect::<Vec<_>>()
    } else {
        report
            .check_results
            .iter()
            .flat_map(|check| {
                check.gaps.iter().map(move |item| {
                    let label = if check.compatibility_id() == "swap" {
                        i18n.t(crate::MessageId::HealthMetricSwap)
                    } else {
                        collector_label(item.collector, i18n)
                    };
                    (item, label)
                })
            })
            .collect::<Vec<_>>()
    };
    labelled
        .into_iter()
        .filter(|(item, _)| {
            item.collector != crate::diagnostics::health::HealthCollector::KernelSignal
                || !has_resource_facts(report)
        })
        .take(limit)
        .map(|(item, label)| HealthBannerLine {
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
                    label,
                    unavailable_reason_label(item.reason, i18n)
                )),
            ],
        })
        .collect()
}

#[cfg(test)]
#[path = "findings/tests.rs"]
mod tests;
