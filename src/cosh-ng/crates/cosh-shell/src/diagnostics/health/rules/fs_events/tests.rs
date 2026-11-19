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

fn mechanism_facts(prefix: &str, age: u64) -> Vec<HealthFact> {
    vec![
        fact(
            &format!("kernel.fs_{prefix}_age_seconds"),
            HealthFactValue::Unsigned(age),
        ),
        fact(
            &format!("kernel.fs_{prefix}_mount"),
            HealthFactValue::String("/data".into()),
        ),
        fact(
            &format!("kernel.fs_{prefix}_device"),
            HealthFactValue::String("sda1".into()),
        ),
        fact(
            &format!("kernel.fs_{prefix}_dev_t"),
            HealthFactValue::String("8:1".into()),
        ),
        fact(
            &format!("kernel.fs_{prefix}_identity"),
            HealthFactValue::String(format!("{prefix}|sda1|uptime-ns:1")),
        ),
        fact(
            &format!("kernel.fs_{prefix}_epoch"),
            HealthFactValue::Unsigned(1700000000),
        ),
        fact("kernel.boot_id", HealthFactValue::String("boot-a".into())),
    ]
}

fn ids(facts: Vec<HealthFact>) -> Vec<(String, HealthSeverity)> {
    fs_event_findings(&facts)
        .into_iter()
        .map(|finding| (finding.id, finding.severity))
        .collect()
}

#[test]
fn forced_ro_and_abort_and_xfs_tiers() {
    assert_eq!(
        ids(mechanism_facts("ext4_forced_ro", 100)),
        vec![("J24".into(), HealthSeverity::Critical)]
    );
    assert_eq!(
        ids(mechanism_facts("ext4_forced_ro", 300)),
        vec![("J24".into(), HealthSeverity::Critical)]
    );
    assert_eq!(
        ids(mechanism_facts("ext4_forced_ro", 301)),
        vec![("J24".into(), HealthSeverity::Warning)]
    );
    assert_eq!(
        ids(mechanism_facts("ext4_forced_ro", 3600)),
        vec![("J24".into(), HealthSeverity::Warning)]
    );
    assert!(ids(mechanism_facts("ext4_forced_ro", 3601)).is_empty());
    assert_eq!(
        ids(mechanism_facts("jbd2_abort", 100)),
        vec![("J26".into(), HealthSeverity::Critical)]
    );
    assert_eq!(
        ids(mechanism_facts("xfs_shutdown", 3500)),
        vec![("J28".into(), HealthSeverity::Warning)]
    );
}

#[test]
fn warning_only_mechanisms_cap_at_300s() {
    assert_eq!(
        ids(mechanism_facts("ext4_error", 300)),
        vec![("J25".into(), HealthSeverity::Warning)]
    );
    assert!(ids(mechanism_facts("ext4_error", 301)).is_empty());
    assert_eq!(
        ids(mechanism_facts("jbd2_flush", 100)),
        vec![("J27".into(), HealthSeverity::Warning)]
    );
    assert!(ids(mechanism_facts("jbd2_flush", 301)).is_empty());
    assert_eq!(
        ids(mechanism_facts("block_io", 100)),
        vec![("J29".into(), HealthSeverity::Warning)]
    );
    assert!(ids(mechanism_facts("block_io", 301)).is_empty());
}

#[test]
fn unbound_or_degraded_chains_stay_silent() {
    let mut facts = mechanism_facts("ext4_forced_ro", 100);
    facts.retain(|fact| fact.key != "kernel.fs_ext4_forced_ro_mount");
    assert!(ids(facts).is_empty());
    let degraded = vec![
        fact(
            "kernel.fs_ext4_forced_ro_degraded_reason",
            HealthFactValue::String("no-error-anchor".into()),
        ),
        fact(
            "kernel.fs_ext4_forced_ro_degraded_age_seconds",
            HealthFactValue::Unsigned(100),
        ),
    ];
    assert!(ids(degraded).is_empty());
}

#[test]
fn multiple_mechanisms_emit_distinct_findings() {
    let mut facts = mechanism_facts("ext4_forced_ro", 100);
    facts.extend(mechanism_facts("block_io", 100));
    let mut found = ids(facts);
    found.sort();
    assert_eq!(
        found,
        vec![
            ("J24".into(), HealthSeverity::Critical),
            ("J29".into(), HealthSeverity::Warning)
        ]
    );
}
