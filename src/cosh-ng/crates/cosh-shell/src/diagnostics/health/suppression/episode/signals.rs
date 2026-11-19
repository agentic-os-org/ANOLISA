//! Stateless per-scan episode signals derived from report facts; the ledger
//! applies them under the guide lock so delivery decisions stay linearizable.

use super::super::super::model::{
    HealthCheckStatus, HealthCollector, HealthFactValue, HealthScanReport,
};
use super::super::super::rules::memory::memory_judgement;
use super::super::super::rules::volume::{evaluate_volumes, MemberLevel};
use super::super::super::rules::{conntrack, psi, quota, ro, service};
use super::{
    volume_member_key, CounterSample, EpisodeSignal, QuotaBranchSignal, QuotaComboSignal,
    VolumeMemberSignal,
};

pub(super) fn string_fact(report: &HealthScanReport, key: &str) -> Option<String> {
    report.facts.iter().find_map(|fact| {
        (fact.key == key).then(|| match &fact.value {
            HealthFactValue::String(value) => value.clone(),
            _ => String::new(),
        })
    })
}

pub(super) fn unsigned_fact(report: &HealthScanReport, key: &str) -> Option<u64> {
    report.facts.iter().find_map(|fact| {
        (fact.key == key).then(|| match &fact.value {
            HealthFactValue::Unsigned(value) => Some(*value),
            HealthFactValue::Integer(value) => u64::try_from(*value).ok(),
            _ => None,
        })?
    })
}

pub(crate) fn signals_from_report(report: &HealthScanReport) -> EpisodeSignal {
    let mut volume = Vec::new();
    for judgement in evaluate_volumes(&report.facts) {
        let device = judgement.device.clone().unwrap_or_default();
        let fsid = judgement.fsid.clone().unwrap_or_default();
        for (member, signal) in [("bytes", judgement.bytes), ("inode", judgement.inodes)] {
            volume.push(VolumeMemberSignal {
                key: volume_member_key(&judgement.mount, member, &device, &fsid),
                entered: signal.entry == Some(MemberLevel::Critical),
                exit: signal.exit,
                valid: signal.valid,
            });
        }
        let ro = ro::ro_judgement(&report.facts, &judgement.mount);
        if ro.valid {
            volume.push(VolumeMemberSignal {
                key: volume_member_key(&judgement.mount, "ro", &device, &fsid),
                entered: ro.entry == Some(MemberLevel::Warning),
                exit: ro.exit,
                valid: ro.valid,
            });
        }
    }
    let counter = counter_sample(report);
    let host_qualified = report.check_results.iter().any(|check| {
        check.collector == Some(HealthCollector::Host) && check.status == HealthCheckStatus::Ok
    });
    let memory = memory_judgement(&report.facts, host_qualified);
    if memory.valid {
        volume.push(VolumeMemberSignal {
            key: "host|memory".to_string(),
            entered: memory.entry == Some(MemberLevel::Critical),
            exit: memory.exit,
            valid: memory.valid,
        });
    }
    for fact in &report.facts {
        let Some(name) = fact
            .key
            .strip_prefix("service.")
            .and_then(|suffix| suffix.strip_suffix(".persistent_qualified"))
        else {
            continue;
        };
        let expected = match string_fact(report, &format!("service.{name}.expected")).as_deref() {
            Some("inactive") => crate::config::HealthServiceExpectedState::Inactive,
            _ => crate::config::HealthServiceExpectedState::Active,
        };
        let signal = service::service_judgement(&report.facts, name, expected);
        if signal.valid {
            volume.push(VolumeMemberSignal {
                key: format!("service|{name}"),
                entered: signal.entry == Some(MemberLevel::Warning),
                exit: signal.exit,
                valid: signal.valid,
            });
        }
    }
    for reading in psi::psi_readings(&report.facts, host_qualified) {
        volume.push(VolumeMemberSignal {
            key: reading.key,
            entered: reading.judgement.entry == Some(MemberLevel::Warning),
            exit: reading.judgement.exit,
            valid: reading.judgement.valid,
        });
    }
    if host_qualified && string_fact(report, "host.conntrack_netns_id").is_some() {
        let conntrack = conntrack::conntrack_judgement(&report.facts);
        if conntrack.valid {
            volume.push(VolumeMemberSignal {
                key: "host|conntrack".to_string(),
                entered: conntrack.entry == Some(MemberLevel::Critical),
                exit: conntrack.exit,
                valid: conntrack.valid,
            });
        }
    }
    let mut quota = Vec::new();
    let judgement = quota::quota_judgement(&report.facts);
    if let Some(id) = judgement
        .cgroup_id
        .as_ref()
        .filter(|_| string_fact(report, "kernel.cgroup_memory_object_id").is_some())
    {
        quota.push(QuotaComboSignal {
            key: format!("cgroup|{id}|quota"),
            max: QuotaBranchSignal {
                entry: judgement.max.entry == Some(MemberLevel::Critical),
                exit: judgement.max.exit,
                valid: judgement.max.valid,
            },
            high: QuotaBranchSignal {
                entry: judgement.high.entry.is_some(),
                exit: judgement.high.exit,
                valid: judgement.high.valid,
            },
            psi_valid: judgement.psi_full.is_some(),
            psi_recovery: quota::psi_combination_recovery(judgement.psi_full),
        });
    }
    EpisodeSignal {
        volume,
        quota,
        counter,
        candidate: None,
    }
}

pub(crate) fn counter_sample(report: &HealthScanReport) -> Option<CounterSample> {
    match (
        string_fact(report, "kernel.boot_id"),
        string_fact(report, "kernel.current_cgroup_id"),
        string_fact(report, "kernel.cgroup_oom_kill_source"),
        unsigned_fact(report, "kernel.cgroup_oom_kill_count"),
        unsigned_fact(report, "kernel.cgroup_oom_kill_sampled_epoch"),
    ) {
        (Some(boot_id), Some(cgroup_id), Some(source), Some(count), Some(sampled_epoch))
            if !boot_id.is_empty() && !cgroup_id.is_empty() =>
        {
            Some(CounterSample {
                boot_id,
                cgroup_id,
                source,
                count,
                sampled_epoch,
            })
        }
        _ => None,
    }
}
