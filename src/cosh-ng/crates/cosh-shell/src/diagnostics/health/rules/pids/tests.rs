use super::*;

#[test]
fn snapshot_critical_excludes_proven_probe_bound() {
    let max = 100u64;
    assert_eq!(
        judge_pids(104, PidsLimit::Finite(max), 4).entry,
        Some(MemberLevel::Critical)
    );
    assert_eq!(
        judge_pids(103, PidsLimit::Finite(max), 4).entry,
        Some(MemberLevel::Warning)
    );
    assert_eq!(
        judge_pids(100, PidsLimit::Finite(max), 4).entry,
        Some(MemberLevel::Warning)
    );
    assert_eq!(judge_pids(89, PidsLimit::Finite(max), 4).entry, None);
}

#[test]
fn warning_band_90_percent_and_32_headroom() {
    assert_eq!(
        judge_pids(90, PidsLimit::Finite(100), 4).entry,
        Some(MemberLevel::Warning)
    );
    assert_eq!(judge_pids(89, PidsLimit::Finite(100), 4).entry, None);
    let max = 1000u64;
    assert_eq!(
        judge_pids(max - 32, PidsLimit::Finite(max), 4).entry,
        Some(MemberLevel::Warning)
    );
    assert_eq!(judge_pids(max - 33, PidsLimit::Finite(max), 4).entry, None);
    assert_eq!(judge_pids(900, PidsLimit::Finite(max), 4).entry, None);
}

#[test]
fn probe_only_occupancy_is_not_critical() {
    assert_eq!(
        judge_pids(4, PidsLimit::Finite(4), 4).entry,
        Some(MemberLevel::Warning)
    );
    assert_eq!(judge_pids(3, PidsLimit::Finite(4), 4).entry, None);
    assert_eq!(
        judge_pids(8, PidsLimit::Finite(4), 4).entry,
        Some(MemberLevel::Critical)
    );
}

#[test]
fn finite_zero_is_prohibition_fact_without_ratio() {
    let judgement = judge_pids(0, PidsLimit::Finite(0), 4);
    assert_eq!(judgement.entry, Some(MemberLevel::Critical));
    assert!(!judgement.exit);
    let judgement = judge_pids(50, PidsLimit::Finite(0), 4);
    assert_eq!(judgement.entry, Some(MemberLevel::Critical));
    assert!(!judgement.exit);
}

#[test]
fn unlimited_is_valid_exit() {
    let judgement = judge_pids(999, PidsLimit::Unlimited, 4);
    assert_eq!(judgement.entry, None);
    assert!(judgement.exit);
}

#[test]
fn recovery_uses_raw_conservative_headroom() {
    let max = 100u64;
    assert!(judge_pids(90, PidsLimit::Finite(max), 4).exit);
    assert!(!judge_pids(95, PidsLimit::Finite(max), 4).exit);
    assert!(judge_pids(max - 8, PidsLimit::Finite(max), 4).exit);
    assert!(!judge_pids(max - 7, PidsLimit::Finite(max), 4).exit);
    assert!(!judge_pids(100, PidsLimit::Finite(max), 4).exit);
    assert!(!judge_pids(0, PidsLimit::Finite(0), 4).exit);
}

#[test]
fn p_max_is_not_one_and_bound_is_documented() {
    const { assert!(P_MAX_HEALTH_PATH_TIDS > 1) };
    assert_eq!(
        judge_pids(5, PidsLimit::Finite(4), 1).entry,
        Some(MemberLevel::Critical)
    );
    assert_eq!(
        judge_pids(5, PidsLimit::Finite(4), P_MAX_HEALTH_PATH_TIDS).entry,
        Some(MemberLevel::Warning)
    );
}

#[test]
fn missing_facts_stay_unknown() {
    assert!(!pids_judgement(&[]).valid);
    assert_eq!(pids_level(&[]), None);
}
