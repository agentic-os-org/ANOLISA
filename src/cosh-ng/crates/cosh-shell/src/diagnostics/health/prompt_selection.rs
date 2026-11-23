use std::collections::{BTreeMap, HashSet};

use crate::I18n;

use super::plan::advice_for_prompt;
use super::{HealthFinding, HealthMessageId, HealthScanReport, HealthSeverity, HealthTryItem};

const MAX_VISIBLE_FINDINGS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdviceSurface {
    Bash,
    Cosh,
}

pub(crate) fn startup_prompt_suggestions(
    report: &HealthScanReport,
    i18n: I18n,
    limit: usize,
) -> Vec<(String, String)> {
    select_try_items(report, &report.try_items, AdviceSurface::Cosh, limit)
        .into_iter()
        .map(|item| {
            let prompt_id = item.prompt_id.unwrap_or(item.label_id);
            let prompt_args = if item.prompt_id.is_some() {
                &item.prompt_args
            } else {
                &item.label_args
            };
            (
                item.id.clone(),
                format_health_message(i18n, prompt_id, prompt_args),
            )
        })
        .collect()
}

pub(crate) fn sorted_findings(report: &HealthScanReport) -> Vec<&HealthFinding> {
    let mut findings = report.findings.iter().collect::<Vec<_>>();
    findings.sort_by(|left, right| {
        right
            .severity
            .precedence()
            .cmp(&left.severity.precedence())
            .then_with(|| finding_display_rank(left).cmp(&finding_display_rank(right)))
            .then_with(|| left.id.cmp(&right.id))
    });
    findings
}

pub(crate) fn sorted_try_items(report: &HealthScanReport) -> Vec<&HealthTryItem> {
    ranked_candidates(report, &report.try_items)
}

pub(crate) fn select_try_items<'a>(
    report: &HealthScanReport,
    candidates: &'a [HealthTryItem],
    surface: AdviceSurface,
    limit: usize,
) -> Vec<&'a HealthTryItem> {
    let mut selected = Vec::new();
    if limit == 0 {
        return selected;
    }
    let mut categories = HashSet::new();
    for item in ranked_candidates(report, candidates) {
        if surface == AdviceSurface::Bash
            && !item
                .prompt_id
                .and_then(advice_for_prompt)
                .is_some_and(|advice| advice.bash_surface)
        {
            continue;
        }
        let critical = report
            .findings
            .iter()
            .find(|finding| finding.id == item.finding_id)
            .is_some_and(|finding| finding.severity == HealthSeverity::Critical);
        let category = item.id.split(':').next().unwrap_or(item.id.as_str());
        if !critical && !categories.insert(category.to_string()) {
            continue;
        }
        selected.push(item);
        if selected.len() == limit {
            break;
        }
    }
    selected
}

fn ranked_candidates<'a>(
    report: &HealthScanReport,
    candidates: &'a [HealthTryItem],
) -> Vec<&'a HealthTryItem> {
    let visible_finding_rank = sorted_findings(report)
        .into_iter()
        .take(MAX_VISIBLE_FINDINGS)
        .enumerate()
        .map(|(rank, finding)| (finding.id.as_str(), rank))
        .collect::<BTreeMap<_, _>>();
    let mut items = candidates
        .iter()
        .filter(|item| {
            report.findings.is_empty()
                || visible_finding_rank.contains_key(item.finding_id.as_str())
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| {
        let finding_rank = visible_finding_rank
            .get(item.finding_id.as_str())
            .copied()
            .unwrap_or(usize::MAX);
        (finding_rank, std::cmp::Reverse(item.score), item.id.clone())
    });
    items
}

fn finding_display_rank(finding: &HealthFinding) -> u8 {
    match finding.title_id {
        HealthMessageId::HealthFindingRecentOom => 0,
        HealthMessageId::HealthFindingCpuLoadHigh => 1,
        HealthMessageId::HealthFindingMemoryAvailableLow => 2,
        HealthMessageId::HealthFindingSwapPressure => 3,
        HealthMessageId::HealthFindingDiskHigh => 4,
        HealthMessageId::HealthFindingServiceFailed
        | HealthMessageId::HealthFindingServiceInactive => 5,
        HealthMessageId::HealthFindingCoreCollectorUnavailable
        | HealthMessageId::HealthFindingPlatformUnsupported => 6,
        HealthMessageId::HealthFindingKernelPanic => 7,
        _ => 8,
    }
}

pub(crate) fn exposure_evidence(report: &HealthScanReport, item: &HealthTryItem) -> String {
    let mut parts = vec![exposure_problem(report, item)];
    let Some(finding) = report
        .findings
        .iter()
        .find(|finding| finding.id == item.finding_id)
    else {
        return parts.join("|");
    };
    if let Some(observed) = finding.detail_args.get("observed") {
        parts.push(format!("observed={observed}"));
    }
    if matches!(finding.id.split(':').next(), Some("J11" | "J12" | "J13")) {
        for key in [
            "kernel.oom_attributed_event_identity",
            "kernel.oom_attributed_scope",
            "kernel.oom_attributed_task_cgroup",
            "kernel.oom_attributed_oom_cgroup",
            "kernel.oom_latest_event_identity",
            "kernel.boot_id",
            "kernel.oom_latest_pid",
            "kernel.oom_killed_process",
            "kernel.oom_latest_constraint",
            "kernel.oom_latest_task_cgroup",
            "kernel.oom_latest_oom_cgroup",
        ] {
            if let Some(fact) = report.facts.iter().find(|fact| fact.key == key) {
                parts.push(format!("{key}={:?}", fact.value));
            }
        }
    }
    parts.join("|")
}

pub(crate) fn exposure_problem(report: &HealthScanReport, item: &HealthTryItem) -> String {
    let category = item.id.split(':').next().unwrap_or(&item.id);
    let object = report
        .findings
        .iter()
        .find(|finding| finding.id == item.finding_id)
        .and_then(|finding| {
            ["mount", "service", "path", "cgroup"]
                .iter()
                .find_map(|key| finding.detail_args.get(*key))
        });
    object.map_or_else(
        || category.to_string(),
        |object| format!("{category}:{object}"),
    )
}

pub(crate) fn exposure_event(
    report: &HealthScanReport,
    item: &HealthTryItem,
) -> Option<(&'static str, String)> {
    let base = item.finding_id.split(':').next();
    if !matches!(
        base,
        Some("J11" | "J12" | "J13" | "J24" | "J25" | "J26" | "J27" | "J28" | "J29" | "J30" | "J31")
    ) {
        return Some(("observation-v1", exposure_evidence(report, item)));
    }
    let fact = |key: &str| {
        report
            .facts
            .iter()
            .find(|fact| fact.key == key)
            .map(|fact| &fact.value)
    };
    if matches!(base, Some("J24" | "J25" | "J26" | "J27" | "J28" | "J29")) {
        let prefix = match base {
            Some("J24") => "ext4_forced_ro",
            Some("J25") => "ext4_error",
            Some("J26") => "jbd2_abort",
            Some("J27") => "jbd2_flush",
            Some("J28") => "xfs_shutdown",
            _ => "block_io",
        };
        let Some(super::HealthFactValue::String(identity)) =
            fact(&format!("kernel.fs_{prefix}_identity"))
        else {
            return None;
        };
        let Some(super::HealthFactValue::String(boot)) = fact("kernel.boot_id") else {
            return None;
        };
        if boot.is_empty() {
            return None;
        }
        return Some(("fs-event-v1", format!("{identity}|boot:{boot}")));
    }
    if matches!(base, Some("J30" | "J31")) {
        let (kind, identity_key) = if base == Some("J30") {
            ("hung-task", "kernel.hung_task_event_identity")
        } else {
            ("soft-lockup", "kernel.soft_lockup_event_identity")
        };
        let Some(super::HealthFactValue::String(identity)) = fact(identity_key) else {
            return None;
        };
        let Some(super::HealthFactValue::String(boot)) = fact("kernel.boot_id") else {
            return None;
        };
        if boot.is_empty() {
            return None;
        }
        return Some(("host-event-v1", format!("{kind}|{identity}|boot:{boot}")));
    }
    if fact("kernel.current_cgroup_version").is_some()
        && fact("kernel.oom_attributed_event_identity").is_none()
        && fact("kernel.cgroup_oom_kill_increment").is_some()
    {
        let text = |key: &str| match fact(key) {
            Some(super::HealthFactValue::String(value)) => Some(value.clone()),
            _ => None,
        };
        let number = |key: &str| match fact(key) {
            Some(super::HealthFactValue::Unsigned(value)) => Some(*value),
            _ => None,
        };
        if let (Some(boot), Some(cgroup_id), Some(start), Some(end), Some(delta)) = (
            text("kernel.boot_id"),
            text("kernel.current_cgroup_id"),
            number("kernel.cgroup_oom_kill_window_start_epoch"),
            number("kernel.cgroup_oom_kill_window_end_epoch"),
            number("kernel.cgroup_oom_kill_increment"),
        ) {
            return Some((
                "cgroup-counter-v1",
                format!("counter:{boot}:{cgroup_id}:{start}:{end}:{delta}"),
            ));
        }
        return None;
    }
    let identity_key = if fact("kernel.oom_attributed_event_identity").is_some() {
        "kernel.oom_attributed_event_identity"
    } else {
        "kernel.oom_latest_event_identity"
    };
    let pid_key = if fact("kernel.oom_attributed_pid").is_some() {
        "kernel.oom_attributed_pid"
    } else {
        "kernel.oom_latest_pid"
    };
    let Some(super::HealthFactValue::String(identity)) = fact(identity_key) else {
        return None;
    };
    let Some(super::HealthFactValue::Unsigned(pid)) = fact(pid_key) else {
        return None;
    };
    if *pid == 0 {
        return None;
    }
    let event = format!("{identity}|pid:{pid}");
    if identity.starts_with("epoch:") {
        return Some(("kernel-epoch-v1", event));
    }
    if !identity.starts_with("uptime-ns:") {
        return None;
    }
    let Some(super::HealthFactValue::String(boot)) = fact("kernel.boot_id") else {
        return None;
    };
    if boot.is_empty() {
        return None;
    }
    Some(("kernel-boottime-v1", format!("{event}|boot:{boot}")))
}

pub(crate) fn legacy_exposure_problems(item: &HealthTryItem) -> Vec<String> {
    let category = item.id.split(':').next().unwrap_or(&item.id);
    let suffix = item
        .finding_id
        .split_once(':')
        .map(|(_, object)| format!(":{object}"))
        .unwrap_or_default();
    std::iter::once(item.id.clone())
        .chain(
            super::plan::RULE_ADVICE
                .iter()
                .filter(|(_, kinds)| {
                    kinds
                        .iter()
                        .any(|kind| super::plan::advice_definition(*kind).legacy_id == category)
                })
                .map(|(rule, _)| format!("{category}:{rule}{suffix}"))
                .filter(|problem| problem != &item.id),
        )
        .collect()
}

pub(crate) fn format_health_message(
    i18n: I18n,
    id: HealthMessageId,
    args: &BTreeMap<String, String>,
) -> String {
    let mut text = i18n.t(id.to_i18n()).to_string();
    for (key, value) in args {
        text = text.replace(&format!("{{{key}}}"), value);
    }
    if id == HealthMessageId::HealthPromptInspectDiskUsage {
        if let Some(mount) = args.get("mount").filter(|mount| !mount.is_empty()) {
            return format!("{mount}: {text}");
        }
    }
    text
}

#[cfg(test)]
library_tests! {
mod tests {
    use super::*;
    use crate::diagnostics::health::builder::HealthReportBuilder;
    use crate::diagnostics::health::model::{
        HealthFactValue, HealthFindingCategory, HealthTryKind,
    };

    #[test]
    fn review_same_oom_event_keeps_identity_across_sampling_second_boundary() {
        use crate::diagnostics::health::model::{HealthFactCategory, HealthFactSource};
        let item = item(
            "T03:J11",
            HealthMessageId::HealthPromptCheckRecentOom,
            "J11",
            100,
        );
        let make = |started, elapsed, age| {
            let mut builder = HealthReportBuilder::for_started_at(started);
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_age_seconds",
                HealthFactValue::Unsigned(age),
                None,
                HealthFactSource::Fixture,
                elapsed,
            );
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_event_epoch",
                HealthFactValue::Unsigned(990),
                None,
                HealthFactSource::Fixture,
                elapsed,
            );
            let mut report = builder.finish(started + elapsed);
            report.findings = vec![finding(
                "J11",
                HealthSeverity::Critical,
                HealthMessageId::HealthFindingRecentOom,
            )];
            report
        };
        let first = make(1_000_900, 200, 10);
        let second = make(1_001_100, 0, 11);
        assert_eq!(
            exposure_evidence(&first, &item),
            exposure_evidence(&second, &item),
            "same event epoch 990, sampled after start on first scan"
        );
    }

    #[test]
    fn distinct_oom_victims_in_the_same_second_have_distinct_identities() {
        use crate::diagnostics::health::model::{HealthFactCategory, HealthFactSource};
        let item = item(
            "T03:J11",
            HealthMessageId::HealthPromptCheckRecentOom,
            "J11",
            100,
        );
        let make = |pid| {
            let mut builder = HealthReportBuilder::for_started_at(1000);
            for (key, value) in [
                ("kernel.oom_latest_event_epoch", 990),
                ("kernel.oom_latest_pid", pid),
            ] {
                builder.add_fact(
                    HealthFactCategory::Kernel,
                    key,
                    HealthFactValue::Unsigned(value),
                    None,
                    HealthFactSource::Fixture,
                    0,
                );
            }
            let mut report = builder.finish(1000);
            report.findings = vec![finding(
                "J11",
                HealthSeverity::Critical,
                HealthMessageId::HealthFindingRecentOom,
            )];
            report
        };
        assert_ne!(
            exposure_evidence(&make(10), &item),
            exposure_evidence(&make(11), &item)
        );
    }

    #[test]
    fn oom_aging_changes_neither_help_scope_nor_event_fingerprint() {
        let first_item = item(
            "T03:J11",
            HealthMessageId::HealthPromptCheckRecentOom,
            "J11",
            100,
        );
        let second_item = item(
            "T03:J12",
            HealthMessageId::HealthPromptCheckRecentOom,
            "J12",
            90,
        );
        let first = report_with(
            vec![finding(
                "J11",
                HealthSeverity::Critical,
                HealthMessageId::HealthFindingRecentOom,
            )],
            vec![],
        );
        let second = report_with(
            vec![finding(
                "J12",
                HealthSeverity::Warning,
                HealthMessageId::HealthFindingRecentOom,
            )],
            vec![],
        );
        assert_eq!(
            exposure_problem(&first, &first_item),
            exposure_problem(&second, &second_item)
        );
        assert_eq!(
            exposure_evidence(&first, &first_item),
            exposure_evidence(&second, &second_item)
        );
        assert!(legacy_exposure_problems(&second_item).contains(&"T03:J11".to_string()));
    }

    fn finding(id: &str, severity: HealthSeverity, title: HealthMessageId) -> HealthFinding {
        HealthFinding {
            id: id.to_string(),
            severity,
            category: HealthFindingCategory::Anomaly,
            title_id: title,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: vec!["fact".to_string()],
            suggested_try_ids: Vec::new(),
        }
    }

    fn item(id: &str, prompt: HealthMessageId, finding_id: &str, score: i32) -> HealthTryItem {
        HealthTryItem {
            id: id.to_string(),
            label_id: prompt,
            label_args: BTreeMap::new(),
            prompt_id: Some(prompt),
            prompt_args: BTreeMap::new(),
            kind: HealthTryKind::AskAgent,
            command: None,
            reason_id: prompt,
            reason_args: BTreeMap::new(),
            score,
            finding_id: finding_id.to_string(),
        }
    }

    fn report_with(findings: Vec<HealthFinding>, items: Vec<HealthTryItem>) -> HealthScanReport {
        let mut report = HealthReportBuilder::for_started_at(1).finish(1);
        report.findings = findings;
        report.try_items = items;
        report
    }

    #[test]
    fn bash_selection_reaches_eligible_candidate_behind_non_bash_items() {
        let report = report_with(
            vec![
                finding(
                    "J15:a",
                    HealthSeverity::Critical,
                    HealthMessageId::HealthFindingServiceFailed,
                ),
                finding(
                    "J15:b",
                    HealthSeverity::Critical,
                    HealthMessageId::HealthFindingServiceFailed,
                ),
                finding(
                    "J06",
                    HealthSeverity::Warning,
                    HealthMessageId::HealthFindingMemoryAvailableLow,
                ),
            ],
            vec![
                item(
                    "T05:J15:a",
                    HealthMessageId::HealthPromptInspectServiceStatus,
                    "J15:a",
                    135,
                ),
                item(
                    "T05:J15:b",
                    HealthMessageId::HealthPromptInspectServiceStatus,
                    "J15:b",
                    130,
                ),
                item(
                    "T01:J06",
                    HealthMessageId::HealthPromptAnalyzeMemoryPressure,
                    "J06",
                    95,
                ),
            ],
        );
        let bash = select_try_items(&report, &report.try_items, AdviceSurface::Bash, 1);
        assert_eq!(bash.len(), 1);
        assert_eq!(bash[0].id, "T01:J06");
        let cosh = select_try_items(&report, &report.try_items, AdviceSurface::Cosh, 3);
        assert_eq!(
            cosh.len(),
            3,
            "critical service items are not category-deduped"
        );
    }

    #[test]
    fn selection_dedups_non_critical_categories_and_caps() {
        let report = report_with(
            vec![
                finding(
                    "J06",
                    HealthSeverity::Warning,
                    HealthMessageId::HealthFindingMemoryAvailableLow,
                ),
                finding(
                    "J06b",
                    HealthSeverity::Warning,
                    HealthMessageId::HealthFindingMemoryAvailableLow,
                ),
                finding(
                    "J08",
                    HealthSeverity::Warning,
                    HealthMessageId::HealthFindingSwapPressure,
                ),
            ],
            vec![
                item(
                    "T01:J06",
                    HealthMessageId::HealthPromptAnalyzeMemoryPressure,
                    "J06",
                    95,
                ),
                item(
                    "T01:J06b",
                    HealthMessageId::HealthPromptAnalyzeMemoryPressure,
                    "J06b",
                    90,
                ),
                item(
                    "T02:J08",
                    HealthMessageId::HealthPromptCheckSwapPressure,
                    "J08",
                    85,
                ),
            ],
        );
        let selected = select_try_items(&report, &report.try_items, AdviceSurface::Cosh, 3);
        assert_eq!(selected.len(), 2, "same non-critical category collapses");
        assert_eq!(selected[0].id, "T01:J06");
        assert_eq!(selected[1].id, "T02:J08");
        let capped = select_try_items(&report, &report.try_items, AdviceSurface::Cosh, 1);
        assert_eq!(capped.len(), 1);
        assert!(select_try_items(&report, &report.try_items, AdviceSurface::Cosh, 0).is_empty());
    }

    #[test]
    fn bash_surface_excludes_unavailable_checks_prompt() {
        let report = report_with(
            vec![finding(
                "J02",
                HealthSeverity::Degraded,
                HealthMessageId::HealthFindingCoreCollectorUnavailable,
            )],
            vec![item(
                "T08:J02",
                HealthMessageId::HealthPromptReviewUnavailableChecks,
                "J02",
                70,
            )],
        );
        assert!(select_try_items(&report, &report.try_items, AdviceSurface::Bash, 1).is_empty());
        assert_eq!(
            select_try_items(&report, &report.try_items, AdviceSurface::Cosh, 1).len(),
            1
        );
    }
}
}
