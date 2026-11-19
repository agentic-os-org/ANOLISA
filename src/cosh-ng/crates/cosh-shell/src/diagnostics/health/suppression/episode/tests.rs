use super::*;
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
