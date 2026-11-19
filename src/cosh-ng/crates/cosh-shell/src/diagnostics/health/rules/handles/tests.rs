use super::*;

#[test]
fn zero_remaining_is_critical_without_ratio() {
    let judgement = judge_handles(1000, 0, 1000);
    assert_eq!(judgement.level, Some("critical"));
    assert!(!judgement.exit);
    let judgement = judge_handles(1200, 0, 1000);
    assert_eq!(judgement.level, Some("critical"));
}

#[test]
fn critical_boundary_95_percent_and_4096_headroom() {
    let max = 100_000u64;
    let allocated = 96_000;
    assert_eq!(judge_handles(allocated, 0, max).level, Some("critical"));
    let max = 1_000_000u64;
    assert_eq!(judge_handles(max - 4096, 0, max).level, Some("critical"));
    assert_eq!(judge_handles(max - 4097, 0, max).level, Some("warning"));
}

#[test]
fn warning_band_at_80_percent_and_silent_below() {
    assert_eq!(judge_handles(80_000, 0, 100_000).level, Some("warning"));
    assert_eq!(judge_handles(79_999, 0, 100_000).level, None);
}

#[test]
fn unused_handles_reduce_in_use_count() {
    assert_eq!(judge_handles(95_000, 20_000, 100_000).level, None);
    assert_eq!(
        judge_handles(110_000, 10_000, 100_000).level,
        Some("critical")
    );
}

#[test]
fn exit_requires_positive_remaining_and_line() {
    assert!(judge_handles(90_000, 0, 100_000).exit);
    assert!(!judge_handles(95_000, 0, 100_000).exit);
    let max = 1_000_000u64;
    assert!(judge_handles(max - 8192, 0, max).exit);
    assert!(!judge_handles(max - 8191, 0, max).exit);
    assert!(!judge_handles(1000, 0, 1000).exit);
}

#[test]
fn zero_max_is_separate_d_fact_without_division() {
    let judgement = judge_handles(500, 0, 0);
    assert!(judgement.zero_max);
    assert!(judgement.valid);
    assert_eq!(judgement.level, None);
    assert!(!judgement.exit);
}

#[test]
fn huge_limit_is_not_truncated() {
    let max = u64::MAX / 2;
    let judgement = judge_handles(max, 0, max);
    assert_eq!(judgement.level, Some("critical"));
    assert!(!judgement.exit);
}
