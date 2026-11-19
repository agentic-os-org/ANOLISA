use super::*;
use crate::diagnostics::health::model::{HealthFactCategory, HealthFactSource, HealthFactValue};

fn fact(key: &str, value: HealthFactValue) -> HealthFact {
    HealthFact {
        id: format!("id-{key}"),
        key: key.to_string(),
        category: HealthFactCategory::Kernel,
        value,
        unit: None,
        source: HealthFactSource::Derived,
        elapsed_ms: 0,
    }
}

fn hung_facts(age: u64) -> Vec<HealthFact> {
    vec![
        fact(
            "kernel.hung_task_latest_age_seconds",
            HealthFactValue::Unsigned(age),
        ),
        fact(
            "kernel.hung_task_event_identity",
            HealthFactValue::String("uptime-ns:1".into()),
        ),
        fact(
            "kernel.hung_task_process",
            HealthFactValue::String("bash".into()),
        ),
        fact("kernel.hung_task_pid", HealthFactValue::Unsigned(1234)),
        fact(
            "kernel.hung_task_blocked_seconds",
            HealthFactValue::Unsigned(120),
        ),
        fact("kernel.boot_id", HealthFactValue::String("boot-a".into())),
    ]
}

#[test]
fn hung_task_recent_window_and_identity_required() {
    let findings = host_event_findings(&hung_facts(100));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J30");
    assert_eq!(findings[0].severity, HealthSeverity::Warning);
    assert_eq!(findings[0].detail_args.get("process").unwrap(), "bash");
    assert!(host_event_findings(&hung_facts(300)).len() == 1);
    assert!(host_event_findings(&hung_facts(301)).is_empty());
    let mut facts = hung_facts(100);
    facts.retain(|fact| fact.key != "kernel.hung_task_event_identity");
    assert!(host_event_findings(&facts).is_empty());
}

#[test]
fn soft_lockup_recent_window() {
    let facts = vec![
        fact(
            "kernel.soft_lockup_latest_age_seconds",
            HealthFactValue::Unsigned(100),
        ),
        fact(
            "kernel.soft_lockup_event_identity",
            HealthFactValue::String("uptime-ns:2".into()),
        ),
        fact("kernel.soft_lockup_cpu", HealthFactValue::Unsigned(3)),
        fact(
            "kernel.soft_lockup_stuck_seconds",
            HealthFactValue::Unsigned(22),
        ),
        fact("kernel.boot_id", HealthFactValue::String("boot-a".into())),
    ];
    let findings = host_event_findings(&facts);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J31");
    assert_eq!(findings[0].detail_args.get("cpu").unwrap(), "3");
}

#[test]
fn d_only_host_events_never_emit_findings() {
    let facts = vec![
        fact(
            "kernel.hardware_error_count_1h",
            HealthFactValue::Unsigned(3),
        ),
        fact(
            "kernel.hardware_error_category",
            HealthFactValue::String("corrected".into()),
        ),
        fact(
            "kernel.segfault_latest_age_seconds",
            HealthFactValue::Unsigned(100),
        ),
        fact(
            "kernel.segfault_process",
            HealthFactValue::String("myapp".into()),
        ),
        fact(
            "kernel.netdev_timeout_latest_age_seconds",
            HealthFactValue::Unsigned(100),
        ),
        fact(
            "kernel.netdev_timeout_interface",
            HealthFactValue::String("eth0".into()),
        ),
    ];
    assert!(host_event_findings(&facts).is_empty());
}
