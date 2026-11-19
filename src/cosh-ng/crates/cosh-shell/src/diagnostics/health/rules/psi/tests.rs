use super::*;
use crate::diagnostics::health::model::{
    HealthFactCategory, HealthFactSource, HealthFactValue, HealthScanReport,
};

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

fn host_psi_facts(resource: &str, full_avg10: f64, full_avg60: f64) -> Vec<HealthFact> {
    vec![
        fact(
            &format!("host.pressure_{resource}_full_avg10"),
            HealthFactValue::Float(full_avg10),
        ),
        fact(
            &format!("host.pressure_{resource}_full_avg60"),
            HealthFactValue::Float(full_avg60),
        ),
        fact(
            &format!("host.pressure_{resource}_full_total"),
            HealthFactValue::Unsigned(123456),
        ),
    ]
}

#[test]
fn full_entry_boundary_is_inclusive_dual_window() {
    assert_eq!(judge_full(5.0, 1.0).entry, Some(MemberLevel::Warning));
    assert_eq!(judge_full(4.99, 0.99).entry, None);
    assert_eq!(judge_full(5.0, 0.99).entry, None);
    assert_eq!(judge_full(4.99, 1.0).entry, None);
}

#[test]
fn full_exit_is_strict_on_both_windows() {
    assert!(!judge_full(2.5, 0.4).exit);
    assert!(!judge_full(2.4, 0.5).exit);
    assert!(judge_full(2.4, 0.4).exit);
    assert!(!judge_full(6.0, 2.0).exit);
    assert!(!judge_full(3.0, 0.4).exit);
    assert!(!judge_full(2.4, 0.6).exit);
}

#[test]
fn hysteresis_sequence_keeps_state_between_lines() {
    for (avg10, avg60) in [(6.0, 2.0), (4.0, 2.0), (2.5, 0.4), (2.4, 0.5)] {
        let judgement = judge_full(avg10, avg60);
        assert!(!judgement.exit, "{avg10}/{avg60}");
    }
    assert!(judge_full(2.4, 0.4).exit);
}

#[test]
fn cpu_some_keeps_original_thresholds() {
    assert_eq!(judge_cpu_some(50.0, 20.0).entry, Some(MemberLevel::Warning));
    assert_eq!(judge_cpu_some(49.99, 19.99).entry, None);
    assert!(!judge_cpu_some(20.0, 10.0).exit);
    assert!(judge_cpu_some(19.99, 9.99).exit);
    assert!(!judge_cpu_some(20.0, 9.99).exit);
}

#[test]
fn pressure_level_marks_critical_without_changing_entry() {
    assert_eq!(pressure_level(Some((20.0, 10.0))), Some("critical"));
    assert_eq!(pressure_level(Some((19.99, 10.0))), Some("warning"));
    assert_eq!(pressure_level(Some((5.0, 1.0))), Some("warning"));
    assert_eq!(pressure_level(Some((4.99, 1.0))), None);
    assert_eq!(pressure_level(Some((0.0, 0.0))), None);
    assert_eq!(pressure_level(None), None);
    assert_eq!(judge_full(25.0, 12.0).entry, Some(MemberLevel::Warning));
}

#[test]
fn hundredths_rounds_fixed_point_values() {
    assert_eq!(hundredths(4.99), Some(499));
    assert_eq!(hundredths(0.07), Some(7));
    assert_eq!(hundredths(100.0), Some(10000));
    assert_eq!(hundredths(-0.01), None);
    assert_eq!(hundredths(f64::NAN), None);
}

#[test]
fn host_memory_entry_emits_warning_finding() {
    let facts = host_psi_facts("memory", 6.0, 2.0);
    let findings = pressure_findings(&facts, true);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J20");
    assert_eq!(findings[0].severity, HealthSeverity::Warning);
    assert_eq!(findings[0].detail_args.get("kind").unwrap(), "full");
}

#[test]
fn critical_pressure_values_keep_warning_presentation() {
    let facts = host_psi_facts("memory", 25.0, 12.0);
    let findings = pressure_findings(&facts, true);
    assert_eq!(findings[0].severity, HealthSeverity::Warning);
}

#[test]
fn valid_zero_is_silent_observation() {
    let facts = host_psi_facts("memory", 0.0, 0.0);
    let readings = psi_readings(&facts, true);
    assert!(readings.iter().any(|reading| reading.judgement.valid));
    assert!(pressure_findings(&facts, true).is_empty());
}

#[test]
fn missing_full_stays_unknown_even_with_high_some() {
    let mut facts = host_psi_facts("memory", 6.0, 2.0);
    facts.retain(|fact| !fact.key.contains("_full_"));
    facts.push(fact(
        "host.pressure_memory_some_avg10",
        HealthFactValue::Float(40.0),
    ));
    facts.push(fact(
        "host.pressure_memory_some_avg60",
        HealthFactValue::Float(30.0),
    ));
    assert!(psi_readings(&facts, true).is_empty());
    assert!(pressure_findings(&facts, true).is_empty());
}

#[test]
fn unqualified_host_suppresses_host_readings() {
    let facts = host_psi_facts("memory", 6.0, 2.0);
    assert!(psi_readings(&facts, false).is_empty());
}

#[test]
fn cpu_uses_some_not_full() {
    let mut facts = vec![
        fact("host.pressure_cpu_some_avg10", HealthFactValue::Float(60.0)),
        fact("host.pressure_cpu_some_avg60", HealthFactValue::Float(25.0)),
        fact("host.pressure_cpu_full_avg10", HealthFactValue::Float(99.0)),
        fact("host.pressure_cpu_full_avg60", HealthFactValue::Float(99.0)),
    ];
    let findings = pressure_findings(&facts, true);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J22");
    assert_eq!(findings[0].detail_args.get("kind").unwrap(), "some");
    facts.retain(|fact| !fact.key.contains("_some_"));
    assert!(pressure_findings(&facts, true).is_empty());
}

#[test]
fn cgroup_readings_require_matching_object_identity() {
    let mut facts = vec![
        fact(
            "kernel.current_cgroup_id",
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
            "kernel.cgroup_psi_memory_full_avg10",
            HealthFactValue::Float(6.0),
        ),
        fact(
            "kernel.cgroup_psi_memory_full_avg60",
            HealthFactValue::Float(2.0),
        ),
    ];
    let findings = pressure_findings(&facts, false);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J20:cgroup");
    facts[1] = fact(
        "kernel.cgroup_psi_object_id",
        HealthFactValue::String("9:9".into()),
    );
    assert!(pressure_findings(&facts, false).is_empty());
}

#[test]
fn io_and_cpu_findings_use_distinct_ids() {
    let mut facts = host_psi_facts("io", 6.0, 2.0);
    facts.extend([
        fact("host.pressure_cpu_some_avg10", HealthFactValue::Float(60.0)),
        fact("host.pressure_cpu_some_avg60", HealthFactValue::Float(25.0)),
    ]);
    let ids: Vec<String> = pressure_findings(&facts, true)
        .into_iter()
        .map(|finding| finding.id)
        .collect();
    assert_eq!(ids, vec!["J21", "J22"]);
}

#[test]
fn readings_feed_episode_signals_with_hysteresis_exit() {
    let facts = host_psi_facts("memory", 2.4, 0.4);
    let readings = psi_readings(&facts, true);
    let memory = readings
        .iter()
        .find(|reading| reading.resource == "memory")
        .unwrap();
    assert!(memory.judgement.valid);
    assert!(memory.judgement.exit);
    assert_eq!(memory.judgement.entry, None);
    let mut report = HealthScanReport::new("psi-test", 0);
    report.facts = facts;
    report
        .check_results
        .push(crate::diagnostics::health::model::HealthCheckResult {
            id: "host.uptime".into(),
            collector: Some(crate::diagnostics::health::model::HealthCollector::Host),
            status: crate::diagnostics::health::model::HealthCheckStatus::Ok,
            fact_ids: Vec::new(),
            gaps: Vec::new(),
            elapsed_ms: 0,
        });
    let signal = crate::diagnostics::health::suppression::episode::signals_from_report(&report);
    assert!(signal
        .volume
        .iter()
        .any(|member| member.key == "host|psi_memory" && member.exit && member.valid));
}
