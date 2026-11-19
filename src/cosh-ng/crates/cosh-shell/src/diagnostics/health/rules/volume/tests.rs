use super::*;
use crate::diagnostics::health::model::{HealthFactCategory, HealthFactSource};

const GIB_U64: u64 = 1024 * 1024 * 1024;

fn critical(entry: Option<MemberLevel>) -> bool {
    entry == Some(MemberLevel::Critical)
}

fn warning(entry: Option<MemberLevel>) -> bool {
    entry == Some(MemberLevel::Warning)
}

#[test]
fn byte_zero_available_is_critical_without_ratio() {
    let judgement = judge_bytes(950 * 4096, 0);
    assert!(critical(judgement.entry));
    assert!(!judgement.exit);
    let degenerate = judge_bytes(0, 0);
    assert!(critical(degenerate.entry));
    assert!(!degenerate.exit);
}

#[test]
fn byte_critical_needs_both_ratio_and_margin() {
    assert!(critical(judge_bytes(95, 5).entry));
    assert!(critical(judge_bytes(950, 1).entry));
    let at_margin = judge_bytes(95 * GIB_U64, 5 * GIB_U64);
    assert!(!critical(at_margin.entry));
    assert!(warning(at_margin.entry));
    let below_ratio = judge_bytes(94 * GIB_U64, 6 * GIB_U64);
    assert!(!critical(below_ratio.entry));
    assert!(warning(below_ratio.entry));
    let huge_free = judge_bytes(96 * GIB_U64, 100 * GIB_U64);
    assert!(huge_free.entry.is_none());
    assert!(huge_free.exit);
}

#[test]
fn byte_warning_dual_boundary_minus_equal_plus() {
    assert!(warning(judge_bytes(90, 10).entry));
    let at_margin = judge_bytes(180 * GIB_U64, 20 * GIB_U64);
    assert!(at_margin.entry.is_none());
    assert!(at_margin.exit);
    let below_ratio = judge_bytes(89, 11);
    assert!(below_ratio.entry.is_none());
    assert!(below_ratio.exit);
    let just_inside = judge_bytes(180 * GIB_U64, 20 * GIB_U64 - 1);
    assert!(warning(just_inside.entry));
}

#[test]
fn byte_exit_line_minus_equal_plus() {
    assert!(judge_bytes(92, 8).exit);
    assert!(!judge_bytes(93, 7).exit);
    assert!(judge_bytes(99 * GIB_U64, 3 * GIB_U64).exit);
    assert!(!judge_bytes(99 * GIB_U64, 3 * GIB_U64 - 1).exit);
    assert!(judge_bytes(0, GIB_U64).exit);
}

#[test]
fn byte_hysteresis_band_keeps_no_critical_entry_and_no_exit() {
    let judgement = judge_bytes(93, 7);
    assert!(!critical(judgement.entry));
    assert!(warning(judgement.entry));
    assert!(!judgement.exit);
    assert!(judgement.valid);
}

#[test]
fn byte_extreme_values_do_not_overflow() {
    let judgement = judge_bytes(u64::MAX, u64::MAX);
    assert!(judgement.exit);
    assert!(judgement.entry.is_none());
    let judgement = judge_bytes(u64::MAX, 0);
    assert!(critical(judgement.entry));
}

#[test]
fn inode_zero_available_is_critical() {
    let judgement = judge_inodes(10000, 0);
    assert!(critical(judgement.entry));
    assert!(!judgement.exit);
}

#[test]
fn inode_critical_dual_boundary() {
    assert!(critical(judge_inodes(999900, 9999).entry));
    let at_count = judge_inodes(1000000, 10000);
    assert!(!critical(at_count.entry));
    assert!(warning(at_count.entry));
    let below_ratio = judge_inodes(989901, 9999);
    assert!(!critical(below_ratio.entry));
}

#[test]
fn inode_warning_dual_boundary_minus_equal_plus() {
    assert!(warning(judge_inodes(1999980, 99999).entry));
    let at_count = judge_inodes(2000000, 100000);
    assert!(at_count.entry.is_none());
    let below_ratio = judge_inodes(1899981, 99999);
    assert!(below_ratio.entry.is_none());
}

#[test]
fn inode_exit_line_requires_positive_and_either_ratio_or_count() {
    assert!(judge_inodes(10000, 300).exit);
    assert!(!judge_inodes(1000000, 100).exit);
    assert!(judge_inodes(1000000, 20000).exit);
    assert!(!judge_inodes(1000000, 19999).exit);
    assert!(!judge_inodes(10000, 0).exit);
    let hysteresis = judge_inodes(1000000, 19999);
    assert!(!hysteresis.exit);
    assert!(warning(hysteresis.entry));
}

#[test]
fn inode_sentinel_and_inverted_counts_are_unknown() {
    assert!(!judge_inodes(0, 0).valid);
    assert!(!judge_inodes(100, 101).valid);
    assert!(judge_inodes(u64::MAX, u64::MAX).valid);
}

fn fact(key: &str, value: HealthFactValue) -> HealthFact {
    HealthFact {
        id: format!("id-{key}"),
        key: key.to_string(),
        category: HealthFactCategory::Disk,
        value,
        unit: None,
        source: HealthFactSource::Statfs,
        elapsed_ms: 0,
    }
}

fn volume_facts(
    mount: &str,
    proven: bool,
    used: u64,
    bavail: u64,
    files: u64,
    favail: u64,
) -> Vec<HealthFact> {
    let prefix = format!("filesystem.mount.{mount}.");
    let mut facts = vec![
        fact(
            &format!("{prefix}volume_proven"),
            HealthFactValue::Bool(proven),
        ),
        fact(
            &format!("{prefix}volume_inode_applicable"),
            HealthFactValue::Bool(true),
        ),
        fact(
            &format!("{prefix}volume_fs_type"),
            HealthFactValue::String("ext4".into()),
        ),
        fact(
            &format!("{prefix}volume_device"),
            HealthFactValue::String("253:0".into()),
        ),
    ];
    if proven {
        facts.extend([
            fact(
                &format!("{prefix}volume_used_bytes"),
                HealthFactValue::Unsigned(used),
            ),
            fact(
                &format!("{prefix}volume_bavail_bytes"),
                HealthFactValue::Unsigned(bavail),
            ),
            fact(
                &format!("{prefix}volume_files"),
                HealthFactValue::Unsigned(files),
            ),
            fact(
                &format!("{prefix}volume_favail"),
                HealthFactValue::Unsigned(favail),
            ),
        ]);
    }
    facts
}

#[test]
fn evaluate_reads_proven_volume_and_merges_members() {
    let facts = volume_facts("/", true, 99 * GIB_U64, 0, 10000, 0);
    let judgements = evaluate_volumes(&facts);
    assert_eq!(judgements.len(), 1);
    assert!(critical(judgements[0].bytes.entry));
    assert!(critical(judgements[0].inodes.entry));
    let findings = volume_findings(&facts);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J09");
    assert_eq!(
        findings[0].detail_args.get("members").unwrap(),
        "bytes+inode"
    );
}

#[test]
fn inode_only_critical_uses_separate_rule_id() {
    let facts = volume_facts("/data", true, GIB_U64, 50 * GIB_U64, 10000, 0);
    let findings = volume_findings(&facts);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J17:/data");
    assert_eq!(findings[0].detail_args.get("members").unwrap(), "inode");
}

#[test]
fn warning_and_unknown_mounts_produce_no_findings() {
    let warning = volume_facts("/", true, 91 * GIB_U64, 5 * GIB_U64, 100000, 50000);
    assert!(volume_findings(&warning).is_empty());
    let unknown = volume_facts("/", false, 0, 0, 0, 0);
    assert!(volume_findings(&unknown).is_empty());
    assert!(!evaluate_volumes(&unknown)[0].bytes.valid);
}

#[test]
fn non_ext4_inode_member_is_not_applicable() {
    let mut facts = volume_facts("/", true, 99 * GIB_U64, 0, 0, 0);
    for item in &mut facts {
        if item.key.ends_with("volume_inode_applicable") {
            item.value = HealthFactValue::Bool(false);
        }
        if item.key.ends_with("volume_fs_type") {
            item.value = HealthFactValue::String("xfs".into());
        }
    }
    facts
        .retain(|item| !item.key.ends_with("volume_files") && !item.key.ends_with("volume_favail"));
    let judgements = evaluate_volumes(&facts);
    assert!(critical(judgements[0].bytes.entry));
    assert!(judgements[0].inodes.valid);
    assert!(judgements[0].inodes.entry.is_none());
    assert!(!judgements[0].inodes.exit);
}

#[test]
fn volume_active_detects_any_proven_flag() {
    assert!(!volume_active(&[]));
    assert!(volume_active(&volume_facts("/", false, 0, 0, 0, 0)));
}
