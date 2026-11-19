use super::*;

const MIB_BYTES: u64 = 1024 * 1024;

fn fact(key: &str, value: HealthFactValue) -> HealthFact {
    HealthFact {
        id: format!("id-{key}"),
        key: key.to_string(),
        category: super::super::super::model::HealthFactCategory::Kernel,
        value,
        unit: None,
        source: super::super::super::model::HealthFactSource::Derived,
        elapsed_ms: 0,
    }
}

fn combo_facts(current_mib: u64, max_mib: u64, high_mib: u64) -> Vec<HealthFact> {
    vec![
        fact(
            "kernel.current_cgroup_id",
            HealthFactValue::String("1:2".into()),
        ),
        fact(
            "kernel.cgroup_memory_object_id",
            HealthFactValue::String("1:2".into()),
        ),
        fact(
            "kernel.cgroup_psi_object_id",
            HealthFactValue::String("1:2".into()),
        ),
        fact(
            "kernel.current_cgroup_path",
            HealthFactValue::String("/slice".into()),
        ),
        fact(
            "kernel.cgroup_memory_current_bytes",
            HealthFactValue::Unsigned(current_mib * MIB_BYTES),
        ),
        fact(
            "kernel.cgroup_memory_max_bytes",
            HealthFactValue::Unsigned(max_mib * MIB_BYTES),
        ),
        fact(
            "kernel.cgroup_memory_high_bytes",
            HealthFactValue::Unsigned(high_mib * MIB_BYTES),
        ),
        fact(
            "kernel.cgroup_psi_memory_full_avg10",
            HealthFactValue::Float(25.0),
        ),
        fact(
            "kernel.cgroup_psi_memory_full_avg60",
            HealthFactValue::Float(12.0),
        ),
    ]
}

#[test]
fn max_critical_boundary_is_inclusive_ratio_and_headroom() {
    let max = 100 * MIB_BYTES;
    let current = 95 * MIB_BYTES;
    assert_eq!(
        judge_max(current, QuotaLimit::Finite(max)).entry,
        Some(MemberLevel::Critical)
    );
    let max = 2560 * MIB_BYTES;
    let current = max - 128 * MIB_BYTES;
    assert_eq!(
        judge_max(current, QuotaLimit::Finite(max)).entry,
        Some(MemberLevel::Critical)
    );
    let current = max - 128 * MIB_BYTES - 1;
    assert_eq!(
        judge_max(current, QuotaLimit::Finite(max)).entry,
        Some(MemberLevel::Warning)
    );
}

#[test]
fn max_warning_band_and_silent_headroom() {
    let max = 100 * MIB_BYTES;
    assert_eq!(
        judge_max(90 * MIB_BYTES, QuotaLimit::Finite(max)).entry,
        Some(MemberLevel::Warning)
    );
    assert_eq!(
        judge_max(89 * MIB_BYTES, QuotaLimit::Finite(max)).entry,
        None
    );
    let max = 4096 * MIB_BYTES;
    let current = max - 600 * MIB_BYTES;
    assert_eq!(judge_max(current, QuotaLimit::Finite(max)).entry, None);
}

#[test]
fn max_exit_lines_are_inclusive() {
    let max = 100 * MIB_BYTES;
    assert!(judge_max(90 * MIB_BYTES, QuotaLimit::Finite(max)).exit);
    assert!(!judge_max(91 * MIB_BYTES, QuotaLimit::Finite(max)).exit);
    let max = 3072 * MIB_BYTES;
    let current = max - 256 * MIB_BYTES;
    assert!(judge_max(current, QuotaLimit::Finite(max)).exit);
    let current = max - 256 * MIB_BYTES + 1;
    assert!(!judge_max(current, QuotaLimit::Finite(max)).exit);
}

#[test]
fn finite_zero_max_is_critical_without_division_or_ratio_exit() {
    let judgement = judge_max(0, QuotaLimit::Finite(0));
    assert_eq!(judgement.entry, Some(MemberLevel::Critical));
    assert!(!judgement.exit);
    let judgement = judge_max(512 * MIB_BYTES, QuotaLimit::Finite(0));
    assert_eq!(judgement.entry, Some(MemberLevel::Critical));
    assert!(!judgement.exit);
}

#[test]
fn unlimited_max_is_valid_exit() {
    let judgement = judge_max(999 * MIB_BYTES, QuotaLimit::Unlimited);
    assert_eq!(judgement.entry, None);
    assert!(judgement.exit);
    assert!(judgement.valid);
}

#[test]
fn current_above_max_still_critical_with_zero_headroom() {
    let judgement = judge_max(1200 * MIB_BYTES, QuotaLimit::Finite(1024 * MIB_BYTES));
    assert_eq!(judgement.entry, Some(MemberLevel::Critical));
    assert!(!judgement.exit);
}

#[test]
fn high_branch_entry_and_ninety_percent_exit() {
    let high = 100 * MIB_BYTES;
    assert_eq!(
        judge_high(high, QuotaLimit::Finite(high)).entry,
        Some(MemberLevel::Warning)
    );
    assert!(judge_high(90 * MIB_BYTES, QuotaLimit::Finite(high)).exit);
    let judgement = judge_high(91 * MIB_BYTES, QuotaLimit::Finite(high));
    assert_eq!(judgement.entry, None);
    assert!(!judgement.exit);
}

#[test]
fn zero_high_only_participates_with_positive_current() {
    let judgement = judge_high(1, QuotaLimit::Finite(0));
    assert_eq!(judgement.entry, Some(MemberLevel::Warning));
    assert!(!judgement.exit);
    let judgement = judge_high(0, QuotaLimit::Finite(0));
    assert_eq!(judgement.entry, None);
    assert!(judgement.exit);
}

#[test]
fn unlimited_high_is_valid_exit() {
    let judgement = judge_high(999 * MIB_BYTES, QuotaLimit::Unlimited);
    assert_eq!(judgement.entry, None);
    assert!(judgement.exit);
}

#[test]
fn combination_requires_critical_quota_or_high_entry_with_dual_high_psi() {
    let facts = combo_facts(96, 100, 200);
    let judgement = quota_judgement(&facts);
    assert!(combo_entry(&judgement));
    let findings = quota_findings(&facts);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J19");
    assert_eq!(findings[0].severity, HealthSeverity::Warning);
    assert_eq!(findings[0].detail_args.get("branches").unwrap(), "max");

    let facts = combo_facts(91, 100, 200);
    assert!(!combo_entry(&quota_judgement(&facts)));
    assert!(quota_findings(&facts).is_empty());

    let facts = combo_facts(60, 100, 50);
    let judgement = quota_judgement(&facts);
    assert!(combo_entry(&judgement));
    assert_eq!(
        quota_findings(&facts)[0]
            .detail_args
            .get("branches")
            .unwrap(),
        "high"
    );

    let facts = combo_facts(96, 100, 50);
    assert_eq!(
        quota_findings(&facts)[0]
            .detail_args
            .get("branches")
            .unwrap(),
        "max+high"
    );
}

#[test]
fn combination_psi_boundary_is_inclusive() {
    let mut facts = combo_facts(96, 100, 200);
    facts[7] = fact(
        "kernel.cgroup_psi_memory_full_avg10",
        HealthFactValue::Float(20.0),
    );
    facts[8] = fact(
        "kernel.cgroup_psi_memory_full_avg60",
        HealthFactValue::Float(10.0),
    );
    assert!(combo_entry(&quota_judgement(&facts)));
    facts[7] = fact(
        "kernel.cgroup_psi_memory_full_avg10",
        HealthFactValue::Float(19.99),
    );
    assert!(!combo_entry(&quota_judgement(&facts)));
}

#[test]
fn psi_recovery_is_strict_dual_low() {
    assert!(psi_combination_recovery(Some((4.99, 0.99))));
    assert!(!psi_combination_recovery(Some((5.0, 0.99))));
    assert!(!psi_combination_recovery(Some((4.99, 1.0))));
    assert!(!psi_combination_recovery(None));
}

#[test]
fn quota_alone_stays_diagnostics_only() {
    let mut facts = combo_facts(96, 100, 200);
    facts.retain(|fact| !fact.key.starts_with("kernel.cgroup_psi_"));
    assert!(quota_findings(&facts).is_empty());
}

#[test]
fn ancestor_quota_with_leaf_psi_never_combines() {
    let mut facts = combo_facts(96, 100, 200);
    facts[1] = fact(
        "kernel.cgroup_memory_object_id",
        HealthFactValue::String("9:9".into()),
    );
    assert!(!quota_judgement(&facts).same_domain);
    assert!(quota_findings(&facts).is_empty());
}

#[test]
fn missing_full_psi_blocks_combination() {
    let mut facts = combo_facts(96, 100, 200);
    facts.retain(|fact| fact.key != "kernel.cgroup_psi_memory_full_avg60");
    assert!(quota_findings(&facts).is_empty());
}

#[test]
fn zero_max_combination_reports_zero_quota_evidence() {
    let mut facts = combo_facts(96, 100, 200);
    facts[5] = fact(
        "kernel.cgroup_memory_max_bytes",
        HealthFactValue::Unsigned(0),
    );
    let findings = quota_findings(&facts);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].detail_args.get("max_zero").unwrap(), "true");
    assert!(!findings[0].detail_args.contains_key("percent"));
}

#[test]
fn unknown_quota_fields_stay_unknown() {
    let mut facts = combo_facts(96, 100, 200);
    facts.retain(|fact| fact.key != "kernel.cgroup_memory_current_bytes");
    let judgement = quota_judgement(&facts);
    assert!(!judgement.max.valid);
    assert!(!judgement.high.valid);
    assert!(quota_findings(&facts).is_empty());
}
