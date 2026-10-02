use std::collections::BTreeMap;

use crate::I18n;

use super::{HealthFinding, HealthMessageId, HealthScanReport, HealthTryItem};

const MAX_VISIBLE_FINDINGS: usize = 3;

pub(crate) fn startup_prompt_suggestions(
    report: &HealthScanReport,
    i18n: I18n,
    limit: usize,
) -> Vec<(String, String)> {
    sorted_try_items(report)
        .into_iter()
        .take(limit)
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
    let visible_finding_rank = sorted_findings(report)
        .into_iter()
        .take(MAX_VISIBLE_FINDINGS)
        .enumerate()
        .map(|(rank, finding)| (finding.id.as_str(), rank))
        .collect::<BTreeMap<_, _>>();
    let mut items = report
        .try_items
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

fn format_health_message(
    i18n: I18n,
    id: HealthMessageId,
    args: &BTreeMap<String, String>,
) -> String {
    let mut text = i18n.t(id.to_i18n()).to_string();
    for (key, value) in args {
        text = text.replace(&format!("{{{key}}}"), value);
    }
    text
}
