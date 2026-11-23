use super::*;
use crate::diagnostics::health::model::{HealthFactCategory, HealthFactSource};

#[test]
fn configured_deviation_is_warning_ordinary_ro_is_details() {
    let deviation = judge_ro(Some(true), true, true);
    assert_eq!(deviation.entry, Some(MemberLevel::Warning));
    assert!(!deviation.exit);
    assert!(deviation.valid);
    let ordinary = judge_ro(Some(true), false, true);
    assert!(ordinary.entry.is_none());
    assert!(!ordinary.exit);
    assert!(ordinary.valid);
}

#[test]
fn same_object_rw_exits_unknown_does_not() {
    let recovered = judge_ro(Some(false), true, true);
    assert!(recovered.exit);
    assert!(recovered.entry.is_none());
    let unknown = judge_ro(None, true, true);
    assert!(!unknown.valid);
    assert!(!unknown.exit);
    let unproven = judge_ro(Some(true), true, false);
    assert!(!unproven.valid);
}

fn facts(readonly: bool, expected: bool, proven: bool) -> Vec<HealthFact> {
    vec![
        HealthFact {
            id: "p".into(),
            key: "filesystem.mount./data.volume_proven".into(),
            category: HealthFactCategory::Disk,
            value: HealthFactValue::Bool(proven),
            unit: None,
            source: HealthFactSource::Statfs,
            elapsed_ms: 0,
        },
        HealthFact {
            id: "r".into(),
            key: "filesystem.mount./data.volume_readonly".into(),
            category: HealthFactCategory::Disk,
            value: HealthFactValue::Bool(readonly),
            unit: None,
            source: HealthFactSource::Statfs,
            elapsed_ms: 0,
        },
        HealthFact {
            id: "e".into(),
            key: "filesystem.mount./data.volume_expected_writable".into(),
            category: HealthFactCategory::Disk,
            value: HealthFactValue::Bool(expected),
            unit: None,
            source: HealthFactSource::Statfs,
            elapsed_ms: 0,
        },
    ]
}

#[test]
fn only_configured_deviation_emits_j18() {
    let findings = ro_findings(&facts(true, true, true));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J18:/data");
    assert_eq!(findings[0].severity, HealthSeverity::Warning);
    assert!(ro_findings(&facts(true, false, true)).is_empty());
    assert!(ro_findings(&facts(false, true, true)).is_empty());
    assert!(ro_findings(&facts(true, true, false)).is_empty());
}
