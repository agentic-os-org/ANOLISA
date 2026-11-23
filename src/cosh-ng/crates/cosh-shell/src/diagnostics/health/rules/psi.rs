//! v3.7 R08 PSI: dual-window entry/exit with hysteresis for host and current
//! cgroup domains. Presentation stays Warning even at critical pressure values.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFinding, HealthFindingCategory, HealthMessageId, HealthSeverity,
};
use super::volume::{MemberJudgement, MemberLevel};
use super::{evidence_ids, number};

/// PSI averages are kernel fixed-point percentages; compare in hundredths so
/// boundary equality never depends on binary floating point rounding.
pub(crate) fn hundredths(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0).then(|| (value * 100.0).round() as u64)
}

pub(crate) fn judge_full(avg10: f64, avg60: f64) -> MemberJudgement {
    match (hundredths(avg10), hundredths(avg60)) {
        (Some(avg10), Some(avg60)) => MemberJudgement {
            entry: (avg10 >= 500 && avg60 >= 100).then_some(MemberLevel::Warning),
            exit: avg10 < 250 && avg60 < 50,
            valid: true,
        },
        _ => MemberJudgement::UNKNOWN,
    }
}

pub(crate) fn judge_cpu_some(avg10: f64, avg60: f64) -> MemberJudgement {
    match (hundredths(avg10), hundredths(avg60)) {
        (Some(avg10), Some(avg60)) => MemberJudgement {
            entry: (avg10 >= 5000 && avg60 >= 2000).then_some(MemberLevel::Warning),
            exit: avg10 < 2000 && avg60 < 1000,
            valid: true,
        },
        _ => MemberJudgement::UNKNOWN,
    }
}

pub(crate) fn pressure_level(full: Option<(f64, f64)>) -> Option<&'static str> {
    let (avg10, avg60) = full?;
    let avg10 = hundredths(avg10)?;
    let avg60 = hundredths(avg60)?;
    if avg10 >= 2000 && avg60 >= 1000 {
        Some("critical")
    } else if avg10 >= 500 && avg60 >= 100 {
        Some("warning")
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PsiReading {
    pub key: String,
    pub base_id: &'static str,
    pub resource: &'static str,
    pub domain: &'static str,
    pub kind: &'static str,
    pub prefix: String,
    pub judgement: MemberJudgement,
    pub cgroup_path: Option<String>,
}

fn judge_from_facts(facts: &[HealthFact], prefix: &str, kind: &str) -> MemberJudgement {
    match (
        number(facts, &format!("{prefix}_{kind}_avg10")),
        number(facts, &format!("{prefix}_{kind}_avg60")),
    ) {
        (Some(avg10), Some(avg60)) => {
            if kind == "some" {
                judge_cpu_some(avg10, avg60)
            } else {
                judge_full(avg10, avg60)
            }
        }
        _ => MemberJudgement::UNKNOWN,
    }
}

fn string_fact(facts: &[HealthFact], key: &str) -> Option<String> {
    match &super::fact(facts, key)?.value {
        super::super::model::HealthFactValue::String(value) => Some(value.clone()),
        _ => None,
    }
}

pub(crate) fn psi_readings(facts: &[HealthFact], host_qualified: bool) -> Vec<PsiReading> {
    let mut readings = Vec::new();
    let resources = [("memory", "J20"), ("io", "J21"), ("cpu", "J22")];
    if host_qualified {
        for (resource, base_id) in resources {
            let kind = if resource == "cpu" { "some" } else { "full" };
            let prefix = format!("host.pressure_{resource}");
            let judgement = judge_from_facts(facts, &prefix, kind);
            if judgement.valid {
                readings.push(PsiReading {
                    key: format!("host|psi_{resource}"),
                    base_id,
                    resource,
                    domain: "host",
                    kind,
                    prefix,
                    judgement,
                    cgroup_path: None,
                });
            }
        }
    }
    let cgroup_id = string_fact(facts, "kernel.current_cgroup_id").filter(|id| !id.is_empty());
    let psi_id = string_fact(facts, "kernel.cgroup_psi_object_id");
    if let (Some(id), Some(psi_id)) = (cgroup_id, psi_id) {
        if id == psi_id {
            let cgroup_path = string_fact(facts, "kernel.current_cgroup_path");
            for (resource, base_id) in resources {
                let kind = if resource == "cpu" { "some" } else { "full" };
                let prefix = format!("kernel.cgroup_psi_{resource}");
                let judgement = judge_from_facts(facts, &prefix, kind);
                if judgement.valid {
                    readings.push(PsiReading {
                        key: format!("cgroup|{id}|psi_{resource}"),
                        base_id,
                        resource,
                        domain: "cgroup",
                        kind,
                        prefix,
                        judgement,
                        cgroup_path: cgroup_path.clone(),
                    });
                }
            }
        }
    }
    readings
}

pub(crate) fn pressure_findings(facts: &[HealthFact], host_qualified: bool) -> Vec<HealthFinding> {
    let mut findings = Vec::new();
    for reading in psi_readings(facts, host_qualified) {
        if reading.judgement.entry != Some(MemberLevel::Warning) {
            continue;
        }
        let avg10_key = format!("{}_{}_avg10", reading.prefix, reading.kind);
        let avg60_key = format!("{}_{}_avg60", reading.prefix, reading.kind);
        let mut evidence_keys = vec![
            avg10_key.clone(),
            avg60_key.clone(),
            format!("{}_{}_total", reading.prefix, reading.kind),
        ];
        if reading.domain == "cgroup" {
            evidence_keys.push("kernel.current_cgroup_path".to_string());
            evidence_keys.push("kernel.cgroup_psi_object_id".to_string());
        } else {
            evidence_keys.push(format!("{}_level", reading.prefix));
        }
        let evidence_keys: Vec<&str> = evidence_keys.iter().map(String::as_str).collect();
        let mut result = HealthFinding {
            id: if reading.domain == "host" {
                reading.base_id.to_string()
            } else {
                format!("{}:cgroup", reading.base_id)
            },
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::Anomaly,
            title_id: HealthMessageId::HealthFindingPressureHigh,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: evidence_ids(facts, &evidence_keys),
            suggested_try_ids: Vec::new(),
        };
        result
            .detail_args
            .insert("resource".into(), reading.resource.into());
        result
            .detail_args
            .insert("domain".into(), reading.domain.into());
        result
            .detail_args
            .insert("kind".into(), reading.kind.into());
        if let Some(avg10) = number(facts, &avg10_key) {
            result
                .detail_args
                .insert("avg10".into(), format!("{avg10:.2}"));
        }
        if let Some(avg60) = number(facts, &avg60_key) {
            result
                .detail_args
                .insert("avg60".into(), format!("{avg60:.2}"));
        }
        if let Some(path) = reading.cgroup_path {
            result.detail_args.insert("cgroup".into(), path);
        }
        findings.push(result);
    }
    findings
}

#[cfg(test)]
mod tests;
