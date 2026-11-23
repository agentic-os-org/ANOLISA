use super::*;

fn fact(key: &str, value: HealthFactValue) -> HealthFact {
    HealthFact {
        id: format!("id-{key}"),
        key: key.to_string(),
        category: super::super::super::model::HealthFactCategory::Host,
        value,
        unit: None,
        source: super::super::super::model::HealthFactSource::Derived,
        elapsed_ms: 0,
    }
}

fn facts(count: u64, max: u64) -> Vec<HealthFact> {
    vec![
        fact("host.conntrack_count", HealthFactValue::Unsigned(count)),
        fact("host.conntrack_max", HealthFactValue::Unsigned(max)),
        fact(
            "host.conntrack_netns_id",
            HealthFactValue::String("4:5".into()),
        ),
    ]
}

#[test]
fn count_at_or_above_max_is_critical() {
    assert_eq!(
        judge_conntrack(1000, 1000).entry,
        Some(MemberLevel::Critical)
    );
    assert_eq!(
        judge_conntrack(1200, 1000).entry,
        Some(MemberLevel::Critical)
    );
}

#[test]
fn critical_boundary_98_percent_and_1024_headroom() {
    let max = 1_000_000u64;
    assert_eq!(
        judge_conntrack(max - 1024, max).entry,
        Some(MemberLevel::Critical)
    );
    assert_eq!(
        judge_conntrack(max - 1025, max).entry,
        Some(MemberLevel::Warning)
    );
    assert_eq!(
        judge_conntrack(49_000, 50_000).entry,
        Some(MemberLevel::Critical)
    );
    assert_eq!(
        judge_conntrack(48_999, 50_000).entry,
        Some(MemberLevel::Warning)
    );
}

#[test]
fn ordinary_90_percent_is_warning_only() {
    assert_eq!(
        judge_conntrack(900_000, 1_000_000).entry,
        Some(MemberLevel::Warning)
    );
    assert_eq!(judge_conntrack(899_999, 1_000_000).entry, None);
    assert!(conntrack_findings(&facts(900_000, 1_000_000)).is_empty());
}

#[test]
fn exit_lines_are_inclusive_with_positive_headroom() {
    assert!(judge_conntrack(900_000, 1_000_000).exit);
    assert!(!judge_conntrack(999_000, 1_000_000).exit);
    let max = 1_000_000u64;
    assert!(judge_conntrack(max - 2048, max).exit);
    assert!(!judge_conntrack(max - 2047, max).exit);
    assert!(!judge_conntrack(1000, 1000).exit);
}

#[test]
fn proven_zero_max_exits_without_reporting_full_or_healthy() {
    let judgement = judge_conntrack(0, 0);
    assert_eq!(judgement.entry, None);
    assert!(judgement.exit);
    assert!(judgement.valid);
    assert!(conntrack_findings(&facts(0, 0)).is_empty());
    assert!(conntrack_findings(&facts(500, 0)).is_empty());
}

#[test]
fn critical_fact_emits_single_warning_finding() {
    let findings = conntrack_findings(&facts(999_000, 1_000_000));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J23");
    assert_eq!(findings[0].severity, HealthSeverity::Warning);
    assert_eq!(findings[0].detail_args.get("count").unwrap(), "999000");
    assert_eq!(findings[0].detail_args.get("max").unwrap(), "1000000");
}

#[test]
fn missing_scalars_stay_unknown() {
    let judgement = conntrack_judgement(&facts(100, 1000));
    assert!(judgement.valid);
    let partial = vec![fact("host.conntrack_count", HealthFactValue::Unsigned(100))];
    assert!(!conntrack_judgement(&partial).valid);
    assert!(conntrack_findings(&partial).is_empty());
}

#[test]
fn fixed_small_ratios_use_exact_integers() {
    assert_eq!(judge_conntrack(900, 1000).entry, Some(MemberLevel::Warning));
    assert_eq!(
        judge_conntrack(980, 1000).entry,
        Some(MemberLevel::Critical)
    );
    assert_eq!(
        judge_conntrack(999_000, 1_000_000).entry,
        Some(MemberLevel::Critical)
    );
}
