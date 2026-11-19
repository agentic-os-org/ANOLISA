use super::*;
use crate::diagnostics::health::model::{HealthFactCategory, HealthFactSource};

const MIB: u64 = 1024;
const GIB: u64 = 1024 * 1024;

fn critical(j: MemberJudgement) -> bool {
    j.entry == Some(MemberLevel::Critical)
}
fn warning(j: MemberJudgement) -> bool {
    j.entry == Some(MemberLevel::Warning)
}

#[test]
fn critical_requires_both_ratio_and_margin() {
    let total = 20 * GIB;
    assert!(critical(judge_memory(512 * MIB - 1, total, true)));
    let at_margin = judge_memory(512 * MIB, total, true);
    assert!(!critical(at_margin));
    assert!(warning(at_margin));
    let total_1g = GIB;
    assert!(critical(judge_memory(52428, total_1g, true)));
    let at_ratio = judge_memory(52429, total_1g, true);
    assert!(!critical(at_ratio));
    assert!(warning(at_ratio));
}

#[test]
fn warning_dual_boundary_minus_equal_plus() {
    let total = 10 * GIB;
    assert!(warning(judge_memory(GIB - 1, total, true)));
    let at_margin = judge_memory(GIB, total, true);
    assert!(at_margin.entry.is_none());
    assert!(at_margin.exit);
    let below_ratio = judge_memory(total / 10 - 1, total, true);
    assert!(warning(below_ratio));
}

#[test]
fn exit_line_either_ratio_or_margin() {
    let total = 10 * GIB;
    assert!(judge_memory(768 * MIB, total, true).exit);
    assert!(!judge_memory(768 * MIB - 1, total, true).exit);
    let eight_pct = total * 8 / 100;
    assert!(judge_memory(eight_pct + 1, total, true).exit);
    assert!(!judge_memory(0, total, true).exit);
}

#[test]
fn small_machine_high_ratio_stays_silent() {
    let j = judge_memory(100 * MIB, 512 * MIB, true);
    assert!(j.entry.is_none());
    assert!(j.exit);
    assert!(j.valid);
}

#[test]
fn zero_total_or_unqualified_host_is_unknown() {
    assert!(!judge_memory(100, 0, true).valid);
    assert!(!judge_memory(100, 10 * GIB, false).valid);
    assert!(judge_memory(100, 10 * GIB, true).valid);
}

#[test]
fn hysteresis_between_exit_and_entry_keeps_no_critical() {
    let total = 10 * GIB;
    let j = judge_memory(700 * MIB, total, true);
    assert!(!critical(j));
    assert!(!j.exit);
    assert!(j.valid);
}

#[test]
fn only_critical_emits_finding_warning_is_details() {
    let facts = vec![
        HealthFact {
            id: "a".into(),
            key: "memory.available_kib".into(),
            category: HealthFactCategory::Memory,
            value: HealthFactValue::Unsigned(100 * MIB),
            unit: None,
            source: HealthFactSource::ProcMeminfo,
            elapsed_ms: 0,
        },
        HealthFact {
            id: "t".into(),
            key: "memory.total_kib".into(),
            category: HealthFactCategory::Memory,
            value: HealthFactValue::Unsigned(10 * GIB),
            unit: None,
            source: HealthFactSource::ProcMeminfo,
            elapsed_ms: 0,
        },
    ];
    let findings = memory_findings(&facts, true);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J05");
    assert!(memory_findings(&facts, false).is_empty());
}
