use super::super::*;

pub(in crate::ui::agent_render::health) fn pressure_evidence_text(
    finding: &HealthFinding,
    i18n: crate::I18n,
) -> Option<String> {
    let args = super::pressure_args(finding, i18n);
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

pub(in crate::ui::agent_render::health) fn quota_evidence_text(
    finding: &HealthFinding,
    i18n: crate::I18n,
) -> Option<String> {
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

pub(in crate::ui::agent_render::health) fn format_evidence_fact(
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
        "kernel.oom_event_count_last_1h" => oom_count_text(
            fact,
            report,
            i18n,
            crate::MessageId::HealthEvidenceOomOneHourCount,
            "1h",
        ),
        "kernel.oom_event_count_last_24h" => oom_count_text(
            fact,
            report,
            i18n,
            crate::MessageId::HealthEvidenceOomTwentyFourHourCount,
            "24h",
        ),
        _ => None,
    }
}

fn oom_count_text(
    fact: &HealthFact,
    report: &HealthScanReport,
    i18n: crate::I18n,
    id: crate::MessageId,
    window: &str,
) -> Option<String> {
    let count = format!("{:.0}", number_from_fact(fact)?);
    let complete = report.fact_metadata.get(&fact.id).is_some_and(|metadata| {
        matches!(
            metadata.coverage,
            crate::diagnostics::health::model::HealthFactCoverage::ReadWindow {
                complete: true,
                ..
            }
        )
    });
    if complete {
        return Some(i18n.format(id, &[("count", &count)]));
    }
    Some(if is_zh(i18n) {
        format!("{window} OOM 已读范围至少 {count} 次")
    } else {
        format!("{window} OOM: at least {count} in the read window")
    })
}
