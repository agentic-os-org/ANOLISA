use super::*;
use crate::diagnostics::health::model::{HealthCheckStatus, HealthCollector, HealthFactValue};
use std::os::unix::fs::PermissionsExt;

fn volume_signal(key: &str, entered: bool, exit: bool, valid: bool) -> VolumeMemberSignal {
    VolumeMemberSignal {
        key: key.to_string(),
        entered,
        exit,
        valid,
    }
}

fn signal(volume: Vec<VolumeMemberSignal>) -> EpisodeSignal {
    EpisodeSignal {
        volume,
        quota: Vec::new(),
        counter: None,
        candidate: None,
    }
}

fn volume_candidate(key: &str, severity: u8) -> CandidateEpisode {
    CandidateEpisode {
        event: false,
        key: key.to_string(),
        problem: "J09:/".into(),
        severity,
        epoch: None,
        counter_window: None,
        quota_branches: None,
    }
}

fn event_candidate(key: &str, severity: u8, epoch: Option<u64>) -> CandidateEpisode {
    CandidateEpisode {
        event: true,
        key: key.to_string(),
        problem: "J11".into(),
        severity,
        epoch,
        counter_window: None,
        quota_branches: None,
    }
}

fn ledger() -> EpisodeLedger {
    EpisodeLedger {
        version: 1,
        ..EpisodeLedger::default()
    }
}

#[test]
fn first_critical_is_immediate_and_same_level_waits_two_hours() {
    let mut state = ledger();
    let candidate = volume_candidate("/|bytes|253:0|1:2", 4);
    assert!(deliverable(&state, &candidate, 1000));
    record_delivery(&mut state, &candidate, 1000);
    assert!(!deliverable(
        &state,
        &candidate,
        1000 + STATE_COOLDOWN_MS - 1
    ));
    assert!(deliverable(&state, &candidate, 1000 + STATE_COOLDOWN_MS));
}

#[test]
fn severity_upgrade_bypasses_state_cooldown_once() {
    let mut state = ledger();
    let warning = volume_candidate("/|bytes|253:0|1:2", 3);
    record_delivery(&mut state, &warning, 1000);
    let critical = volume_candidate("/|bytes|253:0|1:2", 4);
    assert!(deliverable(&state, &critical, 2000));
    record_delivery(&mut state, &critical, 2000);
    assert!(!deliverable(&state, &critical, 3000));
}

#[test]
fn valid_exit_recovers_and_recurrence_waits_ten_minutes_from_delivery() {
    let mut state = ledger();
    let candidate = volume_candidate("/|bytes|253:0|1:2", 4);
    record_delivery(&mut state, &candidate, 1000);
    sync(
        &mut state,
        &signal(vec![volume_signal("/|bytes|253:0|1:2", false, true, true)]),
        5000,
    );
    assert!(state.volume.is_empty());
    assert!(state.recoveries.contains_key("/|bytes|253:0|1:2"));
    assert!(!deliverable(&state, &candidate, 5000));
    assert!(!deliverable(
        &state,
        &candidate,
        1000 + RECURRENCE_GATE_MS - 1
    ));
    assert!(deliverable(&state, &candidate, 1000 + RECURRENCE_GATE_MS));
}

#[test]
fn unknown_sample_never_recovers_and_hysteresis_keeps_episode() {
    let mut state = ledger();
    let candidate = volume_candidate("/|bytes|253:0|1:2", 4);
    record_delivery(&mut state, &candidate, 1000);
    sync(
        &mut state,
        &signal(vec![volume_signal(
            "/|bytes|253:0|1:2",
            false,
            false,
            false,
        )]),
        5000,
    );
    assert!(state.volume.contains_key("/|bytes|253:0|1:2"));
    sync(
        &mut state,
        &signal(vec![volume_signal("/|bytes|253:0|1:2", false, false, true)]),
        6000,
    );
    assert!(state.volume.contains_key("/|bytes|253:0|1:2"));
    assert!(!deliverable(&state, &candidate, 6000));
}

#[test]
fn byte_recovery_does_not_close_inode_member() {
    let mut state = ledger();
    let bytes = volume_candidate("/|bytes|253:0|1:2", 4);
    let inode = volume_candidate("/|inode|253:0|1:2", 4);
    record_delivery(&mut state, &bytes, 1000);
    record_delivery(&mut state, &inode, 1000);
    sync(
        &mut state,
        &signal(vec![
            volume_signal("/|bytes|253:0|1:2", false, true, true),
            volume_signal("/|inode|253:0|1:2", true, false, true),
        ]),
        5000,
    );
    assert!(state.volume.contains_key("/|inode|253:0|1:2"));
    assert!(!state.volume.contains_key("/|bytes|253:0|1:2"));
}

#[test]
fn event_identity_delivers_once_and_aging_never_replays() {
    let mut state = ledger();
    let candidate = event_candidate("kernel-boottime-v1|uptime-ns:1|pid:9", 4, Some(100));
    assert!(deliverable(&state, &candidate, 1000));
    record_delivery(&mut state, &candidate, 1000);
    assert!(!deliverable(&state, &candidate, 1000 + EVENT_MERGE_MS + 1));
    assert!(!deliverable(
        &state,
        &candidate,
        1000 + STATE_COOLDOWN_MS + 1
    ));
}

#[test]
fn event_channel_merges_within_ten_minutes_without_sliding() {
    let mut state = ledger();
    let first = event_candidate("kernel-boottime-v1|uptime-ns:1|pid:9", 3, Some(100));
    record_delivery(&mut state, &first, 1000);
    let second = event_candidate("kernel-boottime-v1|uptime-ns:2|pid:10", 3, Some(200));
    assert!(!deliverable(&state, &second, 1000 + EVENT_MERGE_MS - 1));
    assert!(deliverable(&state, &second, 1000 + EVENT_MERGE_MS));
    record_delivery(&mut state, &second, 1000 + EVENT_MERGE_MS);
    let third = event_candidate("kernel-boottime-v1|uptime-ns:3|pid:11", 3, Some(300));
    assert!(!deliverable(
        &state,
        &third,
        1000 + EVENT_MERGE_MS + 9 * 60 * 1000
    ));
}

#[test]
fn event_warning_to_critical_upgrades_inside_merge_window() {
    let mut state = ledger();
    let warning = event_candidate("kernel-boottime-v1|uptime-ns:1|pid:9", 3, Some(100));
    record_delivery(&mut state, &warning, 1000);
    let critical = event_candidate("kernel-boottime-v1|uptime-ns:2|pid:10", 4, Some(200));
    assert!(deliverable(&state, &critical, 2000));
    record_delivery(&mut state, &critical, 2000);
    let another_critical = event_candidate("kernel-boottime-v1|uptime-ns:3|pid:11", 4, Some(300));
    assert!(!deliverable(&state, &another_critical, 3000));
}

#[test]
fn counter_first_nonzero_sample_is_historical_only() {
    let mut state = ledger();
    let sample = CounterSample {
        boot_id: "boot-a".into(),
        cgroup_id: "1:2".into(),
        source: "memory.events.local".into(),
        count: 3,
        sampled_epoch: 1000,
    };
    let sync_signal = EpisodeSignal {
        volume: Vec::new(),
        quota: Vec::new(),
        counter: Some(sample.clone()),
        candidate: None,
    };
    assert!(sync(&mut state, &sync_signal, 1000).is_none());
    assert!(peek_increment(&state, &sample).is_none());
}

#[test]
fn counter_true_increment_rolls_baseline_and_reports_window() {
    let mut state = ledger();
    let first = CounterSample {
        boot_id: "boot-a".into(),
        cgroup_id: "1:2".into(),
        source: "memory.events.local".into(),
        count: 1,
        sampled_epoch: 1000,
    };
    let second = CounterSample {
        count: 3,
        sampled_epoch: 1200,
        ..first.clone()
    };
    sync(
        &mut state,
        &EpisodeSignal {
            volume: Vec::new(),
            quota: Vec::new(),
            counter: Some(first),
            candidate: None,
        },
        1000,
    );
    assert_eq!(peek_increment(&state, &second), Some((1000, 1200, 2)));
    let increment = sync(
        &mut state,
        &EpisodeSignal {
            volume: Vec::new(),
            quota: Vec::new(),
            counter: Some(second.clone()),
            candidate: None,
        },
        1200,
    );
    assert_eq!(increment, Some((1000, 1200, 2)));
    assert!(peek_increment(&state, &second).is_none());
}

#[test]
fn counter_reset_or_boot_change_rebuilds_baseline_without_event() {
    let mut state = ledger();
    let base = CounterSample {
        boot_id: "boot-a".into(),
        cgroup_id: "1:2".into(),
        source: "memory.events.local".into(),
        count: 5,
        sampled_epoch: 1000,
    };
    sync(
        &mut state,
        &EpisodeSignal {
            volume: Vec::new(),
            quota: Vec::new(),
            counter: Some(base.clone()),
            candidate: None,
        },
        1000,
    );
    for changed in [
        CounterSample {
            count: 2,
            ..base.clone()
        },
        CounterSample {
            boot_id: "boot-b".into(),
            ..base.clone()
        },
        CounterSample {
            cgroup_id: "9:9".into(),
            ..base.clone()
        },
        CounterSample {
            source: "memory.events".into(),
            ..base.clone()
        },
    ] {
        assert!(peek_increment(&state, &changed).is_none());
        assert!(sync(
            &mut state,
            &EpisodeSignal {
                volume: Vec::new(),
                quota: Vec::new(),
                counter: Some(changed),
                candidate: None,
            },
            2000,
        )
        .is_none());
    }
}

#[test]
fn log_event_inside_delivered_counter_window_only_supplements() {
    let mut state = ledger();
    let counter = CandidateEpisode {
        event: true,
        key: "cgroup-counter-v1|counter:boot-a:1:2:1000:1200:2".into(),
        problem: "J11".into(),
        severity: 4,
        epoch: Some(1200),
        counter_window: Some((1000, 1200)),
        quota_branches: None,
    };
    record_delivery(&mut state, &counter, 1000);
    let late_log = event_candidate("kernel-boottime-v1|uptime-ns:1100|pid:9", 4, Some(1100));
    assert!(!deliverable(&state, &late_log, 2000));
    let outside = event_candidate("kernel-boottime-v1|uptime-ns:1300|pid:10", 4, Some(1300));
    assert!(!deliverable(&state, &outside, 2000));
    assert!(deliverable(&state, &outside, 1000 + EVENT_MERGE_MS));
}

#[test]
fn counter_candidate_covered_by_delivered_log_is_suppressed() {
    let mut state = ledger();
    let log = event_candidate("kernel-boottime-v1|uptime-ns:1100|pid:9", 4, Some(1100));
    record_delivery(&mut state, &log, 1000);
    let counter = CandidateEpisode {
        event: true,
        key: "cgroup-counter-v1|counter:boot-a:1:2:1000:1200:2".into(),
        problem: "J11".into(),
        severity: 4,
        epoch: Some(1200),
        counter_window: Some((1000, 1200)),
        quota_branches: None,
    };
    assert!(!deliverable(&state, &counter, 2000));
}

#[test]
fn ledger_roundtrip_and_corrupt_file_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join(FILE);
    let mut state = ledger();
    record_delivery(&mut state, &volume_candidate("/|bytes|253:0|1:2", 4), 1000);
    write(&path, &state).unwrap();
    let loaded = load_or_default(&path).unwrap();
    assert_eq!(loaded, state);
    std::fs::write(&path, b"{corrupt").unwrap();
    assert!(load_or_default(&path).is_none());
    let missing = root.join("missing.json");
    assert_eq!(
        load_or_default(&missing).unwrap(),
        EpisodeLedger {
            version: 1,
            ..EpisodeLedger::default()
        }
    );
}

fn quota_candidate(max_entry: bool, high_entry: bool) -> CandidateEpisode {
    CandidateEpisode {
        event: false,
        key: "cgroup|1:2|quota".into(),
        problem: "J19".into(),
        severity: 3,
        epoch: None,
        counter_window: None,
        quota_branches: Some((max_entry, high_entry)),
    }
}

fn branch(entry: bool, exit: bool, valid: bool) -> QuotaBranchSignal {
    QuotaBranchSignal { entry, exit, valid }
}

fn quota_signal(
    max: QuotaBranchSignal,
    high: QuotaBranchSignal,
    psi_valid: bool,
    psi_recovery: bool,
) -> EpisodeSignal {
    EpisodeSignal {
        volume: Vec::new(),
        quota: vec![QuotaComboSignal {
            key: "cgroup|1:2|quota".into(),
            max,
            high,
            psi_valid,
            psi_recovery,
        }],
        counter: None,
        candidate: None,
    }
}

#[test]
fn quota_combination_delivers_once_then_waits_two_hours() {
    let mut state = ledger();
    let candidate = quota_candidate(true, true);
    assert!(deliverable(&state, &candidate, 1000));
    record_delivery(&mut state, &candidate, 1000);
    assert!(!deliverable(
        &state,
        &candidate,
        1000 + STATE_COOLDOWN_MS - 1
    ));
    assert!(deliverable(&state, &candidate, 1000 + STATE_COOLDOWN_MS));
    let episode = state.quota.get("cgroup|1:2|quota").unwrap();
    assert!(episode.max_entered && episode.high_entered);
}

#[test]
fn quota_redelivery_preserves_branch_history() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, false), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(true, false, true),
            true,
            false,
        ),
        2000,
    );
    let episode = state.quota.get("cgroup|1:2|quota").unwrap();
    assert!(episode.max_exited);
    assert!(episode.high_entered && !episode.high_exited);
    record_delivery(
        &mut state,
        &quota_candidate(false, true),
        1000 + STATE_COOLDOWN_MS,
    );
    let episode = state.quota.get("cgroup|1:2|quota").unwrap();
    assert!(episode.max_entered && episode.max_exited);
    assert!(episode.high_entered);
}

#[test]
fn quota_closes_only_when_every_entered_branch_exits_known() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, true), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(true, false, true),
            true,
            false,
        ),
        2000,
    );
    assert!(state.quota.contains_key("cgroup|1:2|quota"));
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(false, true, true),
            true,
            false,
        ),
        3000,
    );
    assert!(!state.quota.contains_key("cgroup|1:2|quota"));
    let recovery = state.recoveries.get("cgroup|1:2|quota").unwrap();
    assert_eq!(recovery.last_delivered_ms, 1000);
    assert!(!deliverable(
        &state,
        &quota_candidate(true, true),
        1000 + RECURRENCE_GATE_MS - 1
    ));
    assert!(deliverable(
        &state,
        &quota_candidate(true, true),
        1000 + RECURRENCE_GATE_MS
    ));
}

#[test]
fn unknown_active_branch_never_counts_as_exited() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, true), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(false, false, false),
            true,
            false,
        ),
        2000,
    );
    assert!(state.quota.contains_key("cgroup|1:2|quota"));
}

#[test]
fn max_only_episode_keeps_while_never_entered_high_is_unknown() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, false), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(false, false, false),
            true,
            false,
        ),
        2000,
    );
    assert!(state.quota.contains_key("cgroup|1:2|quota"));
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(false, true, true),
            true,
            false,
        ),
        3000,
    );
    assert!(!state.quota.contains_key("cgroup|1:2|quota"));
}

#[test]
fn max_only_with_known_inactive_high_closes_on_max_exit() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, false), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(false, true, true),
            true,
            false,
        ),
        2000,
    );
    assert!(!state.quota.contains_key("cgroup|1:2|quota"));
}

#[test]
fn psi_dual_low_recovery_closes_even_with_active_branches() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, true), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(true, false, true),
            branch(true, false, true),
            true,
            true,
        ),
        2000,
    );
    assert!(!state.quota.contains_key("cgroup|1:2|quota"));
}

#[test]
fn unknown_psi_does_not_recover() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, true), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(true, false, true),
            branch(true, false, true),
            false,
            true,
        ),
        2000,
    );
    assert!(state.quota.contains_key("cgroup|1:2|quota"));
}

#[test]
fn exited_max_reenters_only_on_fresh_critical_entry() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, false), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(false, true, true),
            true,
            false,
        ),
        2000,
    );
    assert!(!state.quota.contains_key("cgroup|1:2|quota"));
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, true), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(false, true, true),
            branch(true, false, true),
            true,
            false,
        ),
        2000,
    );
    sync(
        &mut state,
        &quota_signal(
            branch(false, false, true),
            branch(true, false, true),
            true,
            false,
        ),
        3000,
    );
    let episode = state.quota.get("cgroup|1:2|quota").unwrap();
    assert!(episode.max_exited);
    sync(
        &mut state,
        &quota_signal(
            branch(true, false, true),
            branch(true, false, true),
            true,
            false,
        ),
        4000,
    );
    let episode = state.quota.get("cgroup|1:2|quota").unwrap();
    assert!(episode.max_entered && !episode.max_exited);
}

#[test]
fn zero_high_current_zero_exits_only_high_branch() {
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, true), 1000);
    sync(
        &mut state,
        &quota_signal(
            branch(true, false, true),
            branch(false, true, true),
            true,
            false,
        ),
        2000,
    );
    let episode = state.quota.get("cgroup|1:2|quota").unwrap();
    assert!(episode.high_exited);
    assert!(!episode.max_exited);
    assert!(state.quota.contains_key("cgroup|1:2|quota"));
}

#[test]
fn quota_episode_roundtrips_through_ledger_file() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join(FILE);
    let mut state = ledger();
    record_delivery(&mut state, &quota_candidate(true, false), 1000);
    write(&path, &state).unwrap();
    assert_eq!(load_or_default(&path).unwrap(), state);
}

fn psi_report(avg10: f64, avg60: f64) -> HealthScanReport {
    let mut report = HealthScanReport::new("psi-sequence", 0);
    for (key, value) in [
        (
            "host.pressure_memory_full_avg10",
            HealthFactValue::Float(avg10),
        ),
        (
            "host.pressure_memory_full_avg60",
            HealthFactValue::Float(avg60),
        ),
    ] {
        report
            .facts
            .push(crate::diagnostics::health::model::HealthFact {
                id: format!("id-{key}"),
                key: key.to_string(),
                category: crate::diagnostics::health::model::HealthFactCategory::Memory,
                value,
                unit: None,
                source: crate::diagnostics::health::model::HealthFactSource::Derived,
                elapsed_ms: 0,
            });
    }
    report
        .check_results
        .push(crate::diagnostics::health::model::HealthCheckResult {
            id: "host.uptime".into(),
            collector: Some(HealthCollector::Host),
            status: HealthCheckStatus::Ok,
            fact_ids: Vec::new(),
            gaps: Vec::new(),
            elapsed_ms: 0,
        });
    report
}

fn psi_candidate() -> CandidateEpisode {
    CandidateEpisode {
        event: false,
        key: "host|psi_memory".into(),
        problem: "J20".into(),
        severity: 3,
        epoch: None,
        counter_window: None,
        quota_branches: None,
    }
}

#[test]
fn psi_hysteresis_sequence_keeps_episode_until_strict_dual_low() {
    let mut state = ledger();
    let entry = signals_from_report(&psi_report(6.0, 2.0));
    assert!(entry
        .volume
        .iter()
        .any(|member| member.key == "host|psi_memory" && member.entered));
    assert!(deliverable(&state, &psi_candidate(), 1000));
    record_delivery(&mut state, &psi_candidate(), 1000);
    for (now, avg10, avg60) in [(2000, 4.0, 2.0), (3000, 2.5, 0.4), (4000, 2.4, 0.5)] {
        let signal = signals_from_report(&psi_report(avg10, avg60));
        let member = signal
            .volume
            .iter()
            .find(|member| member.key == "host|psi_memory")
            .unwrap();
        assert!(!member.entered, "{avg10}/{avg60}");
        assert!(!member.exit, "{avg10}/{avg60}");
        sync(&mut state, &signal, now);
        assert!(state.volume.contains_key("host|psi_memory"));
    }
    let exit = signals_from_report(&psi_report(2.4, 0.4));
    sync(&mut state, &exit, 5000);
    assert!(!state.volume.contains_key("host|psi_memory"));
    assert!(state.recoveries.contains_key("host|psi_memory"));
    let reentry = signals_from_report(&psi_report(5.0, 1.0));
    assert!(reentry
        .volume
        .iter()
        .any(|member| member.key == "host|psi_memory" && member.entered));
    assert!(!deliverable(&state, &psi_candidate(), 6000));
    assert!(deliverable(
        &state,
        &psi_candidate(),
        1000 + RECURRENCE_GATE_MS
    ));
}

#[test]
fn psi_reentry_uses_original_episode_cooldown() {
    let mut state = ledger();
    record_delivery(&mut state, &psi_candidate(), 1000);
    sync(
        &mut state,
        &signals_from_report(&psi_report(4.0, 2.0)),
        2000,
    );
    assert!(state.volume.contains_key("host|psi_memory"));
    assert!(!deliverable(&state, &psi_candidate(), 3000));
    assert!(!deliverable(
        &state,
        &psi_candidate(),
        1000 + STATE_COOLDOWN_MS - 1
    ));
    assert!(deliverable(
        &state,
        &psi_candidate(),
        1000 + STATE_COOLDOWN_MS
    ));
    record_delivery(&mut state, &psi_candidate(), 1000 + STATE_COOLDOWN_MS);
    assert!(!deliverable(
        &state,
        &psi_candidate(),
        1000 + 2 * STATE_COOLDOWN_MS - 1
    ));
}

#[test]
fn psi_unknown_reading_never_recovers_episode() {
    let mut state = ledger();
    record_delivery(&mut state, &psi_candidate(), 1000);
    let mut report = psi_report(0.0, 0.0);
    report.facts.retain(|fact| !fact.key.contains("_full_"));
    let signal = signals_from_report(&report);
    assert!(!signal
        .volume
        .iter()
        .any(|member| member.key == "host|psi_memory"));
    sync(&mut state, &signal, 2000);
    assert!(state.volume.contains_key("host|psi_memory"));
}

fn conntrack_report(count: u64, max: u64) -> HealthScanReport {
    let mut report = HealthScanReport::new("conntrack-test", 0);
    for (key, value) in [
        ("host.conntrack_count", HealthFactValue::Unsigned(count)),
        ("host.conntrack_max", HealthFactValue::Unsigned(max)),
        (
            "host.conntrack_netns_id",
            HealthFactValue::String("4:5".into()),
        ),
    ] {
        report
            .facts
            .push(crate::diagnostics::health::model::HealthFact {
                id: format!("id-{key}"),
                key: key.to_string(),
                category: crate::diagnostics::health::model::HealthFactCategory::Host,
                value,
                unit: None,
                source: crate::diagnostics::health::model::HealthFactSource::Derived,
                elapsed_ms: 0,
            });
    }
    report
        .check_results
        .push(crate::diagnostics::health::model::HealthCheckResult {
            id: "host.uptime".into(),
            collector: Some(HealthCollector::Host),
            status: HealthCheckStatus::Ok,
            fact_ids: Vec::new(),
            gaps: Vec::new(),
            elapsed_ms: 0,
        });
    report
}

fn conntrack_candidate() -> CandidateEpisode {
    CandidateEpisode {
        event: false,
        key: "host|conntrack".into(),
        problem: "J23".into(),
        severity: 3,
        epoch: None,
        counter_window: None,
        quota_branches: None,
    }
}

#[test]
fn conntrack_only_critical_enters_and_zero_max_closes_member() {
    let warning = signals_from_report(&conntrack_report(900_000, 1_000_000));
    let member = warning
        .volume
        .iter()
        .find(|member| member.key == "host|conntrack")
        .unwrap();
    assert!(!member.entered);
    let critical = signals_from_report(&conntrack_report(999_000, 1_000_000));
    let member = critical
        .volume
        .iter()
        .find(|member| member.key == "host|conntrack")
        .unwrap();
    assert!(member.entered && member.valid);
    let mut state = ledger();
    assert!(deliverable(&state, &conntrack_candidate(), 1000));
    record_delivery(&mut state, &conntrack_candidate(), 1000);
    assert!(!deliverable(&state, &conntrack_candidate(), 2000));
    let removed = signals_from_report(&conntrack_report(0, 0));
    let member = removed
        .volume
        .iter()
        .find(|member| member.key == "host|conntrack")
        .unwrap();
    assert!(member.exit && !member.entered);
    sync(&mut state, &removed, 3000);
    assert!(!state.volume.contains_key("host|conntrack"));
    assert!(state.recoveries.contains_key("host|conntrack"));
}

#[test]
fn conntrack_recovery_line_closes_and_recurrence_gate_applies() {
    let mut state = ledger();
    record_delivery(&mut state, &conntrack_candidate(), 1000);
    let recovered = signals_from_report(&conntrack_report(800_000, 1_000_000));
    sync(&mut state, &recovered, 2000);
    assert!(!state.volume.contains_key("host|conntrack"));
    assert!(!deliverable(
        &state,
        &conntrack_candidate(),
        1000 + RECURRENCE_GATE_MS - 1
    ));
    assert!(deliverable(
        &state,
        &conntrack_candidate(),
        1000 + RECURRENCE_GATE_MS
    ));
}

#[test]
fn conntrack_unknown_scalars_do_not_recover() {
    let mut state = ledger();
    record_delivery(&mut state, &conntrack_candidate(), 1000);
    let mut report = conntrack_report(0, 0);
    report.facts.retain(|fact| fact.key != "host.conntrack_max");
    let signal = signals_from_report(&report);
    assert!(!signal
        .volume
        .iter()
        .any(|member| member.key == "host|conntrack"));
    sync(&mut state, &signal, 2000);
    assert!(state.volume.contains_key("host|conntrack"));
}

