use super::*;
use crate::diagnostics::health::model::{HealthFactCategory, HealthFactSource};

#[test]
fn unqualified_service_is_details_only() {
    let j = judge_service(
        "inactive",
        "dead",
        HealthServiceExpectedState::Active,
        false,
    );
    assert!(!j.valid);
    assert!(j.entry.is_none());
}

#[test]
fn expected_active_deviations_and_recovery() {
    let dead = judge_service("inactive", "dead", HealthServiceExpectedState::Active, true);
    assert_eq!(dead.entry, Some(MemberLevel::Warning));
    assert!(!dead.exit);
    let failed = judge_service("failed", "failed", HealthServiceExpectedState::Active, true);
    assert_eq!(failed.entry, Some(MemberLevel::Warning));
    let running = judge_service(
        "active",
        "running",
        HealthServiceExpectedState::Active,
        true,
    );
    assert!(running.entry.is_none());
    assert!(running.exit);
    let exited = judge_service("active", "exited", HealthServiceExpectedState::Active, true);
    assert!(exited.entry.is_none());
    assert!(!exited.exit);
}

#[test]
fn expected_inactive_deviations_and_details() {
    let running = judge_service(
        "active",
        "running",
        HealthServiceExpectedState::Inactive,
        true,
    );
    assert_eq!(running.entry, Some(MemberLevel::Warning));
    let dead = judge_service(
        "inactive",
        "dead",
        HealthServiceExpectedState::Inactive,
        true,
    );
    assert!(dead.entry.is_none());
    assert!(dead.exit);
    let failed = judge_service(
        "failed",
        "failed",
        HealthServiceExpectedState::Inactive,
        true,
    );
    assert!(failed.entry.is_none());
    let exited = judge_service(
        "active",
        "exited",
        HealthServiceExpectedState::Inactive,
        true,
    );
    assert!(exited.entry.is_none());
}

fn facts(name: &str, active: &str, sub: &str, qualified: bool) -> Vec<HealthFact> {
    let prefix = format!("service.{name}.");
    vec![
        HealthFact {
            id: "q".into(),
            key: format!("{prefix}persistent_qualified"),
            category: HealthFactCategory::Service,
            value: HealthFactValue::Bool(qualified),
            unit: None,
            source: HealthFactSource::Systemctl,
            elapsed_ms: 0,
        },
        HealthFact {
            id: "a".into(),
            key: format!("{prefix}active_state"),
            category: HealthFactCategory::Service,
            value: HealthFactValue::String(active.into()),
            unit: None,
            source: HealthFactSource::Systemctl,
            elapsed_ms: 0,
        },
        HealthFact {
            id: "s".into(),
            key: format!("{prefix}sub_state"),
            category: HealthFactCategory::Service,
            value: HealthFactValue::String(sub.into()),
            unit: None,
            source: HealthFactSource::Systemctl,
            elapsed_ms: 0,
        },
    ]
}

#[test]
fn findings_only_for_qualified_deviation() {
    let mut config = HealthConfig::default();
    config.services.push(crate::config::HealthServiceConfig {
        name: "nginx.service".into(),
        expected: HealthServiceExpectedState::Active,
        activation: "persistent".into(),
    });
    let findings = service_findings(&facts("nginx.service", "failed", "failed", true), &config);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J15:nginx.service");
    assert!(
        service_findings(&facts("nginx.service", "failed", "failed", false), &config).is_empty()
    );
    assert!(
        service_findings(&facts("nginx.service", "active", "running", true), &config).is_empty()
    );
}
