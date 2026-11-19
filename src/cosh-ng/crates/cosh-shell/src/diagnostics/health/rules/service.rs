//! v3.7 R11 service expectation: only persistent-online-qualified units produce
//! notices; activation-type or unproven lifecycles stay details-only.

use std::collections::BTreeMap;

use super::super::model::{
    HealthFact, HealthFactValue, HealthFinding, HealthFindingCategory, HealthMessageId,
    HealthSeverity,
};
use super::evidence_ids;
use super::volume::{MemberJudgement, MemberLevel};
use crate::config::{HealthConfig, HealthServiceExpectedState};

fn string_value(facts: &[HealthFact], key: &str) -> Option<String> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn bool_value(facts: &[HealthFact], key: &str) -> Option<bool> {
    match &super::fact(facts, key)?.value {
        HealthFactValue::Bool(value) => Some(*value),
        _ => None,
    }
}

pub(crate) fn judge_service(
    active_state: &str,
    sub_state: &str,
    expected: HealthServiceExpectedState,
    qualified: bool,
) -> MemberJudgement {
    if !qualified {
        return MemberJudgement {
            entry: None,
            exit: false,
            valid: false,
        };
    }
    let stable_active = active_state == "active" && sub_state == "running";
    let stable_inactive = active_state == "inactive" && sub_state == "dead";
    let failed = active_state == "failed" && sub_state == "failed";
    let entry = match expected {
        HealthServiceExpectedState::Active => {
            if stable_inactive || failed {
                Some(MemberLevel::Warning)
            } else {
                None
            }
        }
        HealthServiceExpectedState::Inactive => {
            if stable_active {
                Some(MemberLevel::Warning)
            } else {
                None
            }
        }
    };
    let exit = match expected {
        HealthServiceExpectedState::Active => stable_active,
        HealthServiceExpectedState::Inactive => stable_inactive,
    };
    MemberJudgement {
        entry,
        exit,
        valid: true,
    }
}

pub(crate) fn service_active(facts: &[HealthFact]) -> bool {
    facts
        .iter()
        .any(|fact| fact.key.ends_with(".persistent_qualified"))
}

pub(crate) fn service_judgement(
    facts: &[HealthFact],
    name: &str,
    expected: HealthServiceExpectedState,
) -> MemberJudgement {
    let prefix = format!("service.{name}.");
    let qualified = bool_value(facts, &format!("{prefix}persistent_qualified")) == Some(true);
    judge_service(
        &string_value(facts, &format!("{prefix}active_state")).unwrap_or_default(),
        &string_value(facts, &format!("{prefix}sub_state")).unwrap_or_default(),
        expected,
        qualified,
    )
}

pub(crate) fn service_findings(facts: &[HealthFact], config: &HealthConfig) -> Vec<HealthFinding> {
    let mut findings = Vec::new();
    for service in &config.services {
        let prefix = format!("service.{}.", service.name);
        if bool_value(facts, &format!("{prefix}persistent_qualified")) != Some(true) {
            continue;
        }
        let judgement = service_judgement(facts, &service.name, service.expected);
        if judgement.entry != Some(MemberLevel::Warning) {
            continue;
        }
        let rule = match service.expected {
            HealthServiceExpectedState::Active => "J15",
            HealthServiceExpectedState::Inactive => "J16",
        };
        let title = match service.expected {
            HealthServiceExpectedState::Active => HealthMessageId::HealthFindingServiceFailed,
            HealthServiceExpectedState::Inactive => HealthMessageId::HealthFindingServiceInactive,
        };
        let mut result = HealthFinding {
            id: format!("{rule}:{}", service.name),
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::RootCause,
            title_id: title,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: evidence_ids(
                facts,
                &[
                    &format!("{prefix}active_state"),
                    &format!("{prefix}sub_state"),
                    &format!("{prefix}result"),
                    &format!("{prefix}persistent_qualified"),
                    &format!("{prefix}state_age_us"),
                ],
            ),
            suggested_try_ids: Vec::new(),
        };
        result
            .detail_args
            .insert("service".into(), service.name.clone());
        result.detail_args.insert(
            "observed".into(),
            format!(
                "{}/{}",
                string_value(facts, &format!("{prefix}active_state")).unwrap_or_default(),
                string_value(facts, &format!("{prefix}sub_state")).unwrap_or_default()
            ),
        );
        result.detail_args.insert(
            "expected".into(),
            match service.expected {
                HealthServiceExpectedState::Active => "active".to_string(),
                HealthServiceExpectedState::Inactive => "inactive".to_string(),
            },
        );
        findings.push(result);
    }
    findings
}

#[cfg(test)]
mod tests;
