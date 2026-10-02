use super::*;
use crate::recommendation::personal_model::{
    ActivityContext, ActivityJournal, ActivityOutcome, ActivityPayload, ActivityRecord,
    ActivitySource, CachedPromptCandidate, CandidateEvidenceSummary, ContextAffinity,
    EvidenceSnapshot, FrequentPattern, RecentTask, RecommendationState, RedactionReport,
    ShellActivityOrigin, DISCLOSURE_VERSION,
};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, PermissionsExt};

#[test]
fn open_creates_missing_private_parent_directories() {
    let home = test_root("fresh-home");
    std::fs::create_dir(&home).unwrap();
    let root = home.join(".copilot-shell/cosh/recommendations");

    let store = PersonalStore::open(&root).unwrap();
    assert!(store.initialize(1_000).is_ok());
    assert_eq!(mode(&home.join(".copilot-shell")), 0o700);
    assert_eq!(mode(&home.join(".copilot-shell/cosh")), 0o700);
    assert_eq!(mode(&root), 0o700);
    cleanup(&home);
}

#[test]
fn clear_rotates_epoch_and_rejects_stale_commit() {
    let root = test_root("stale");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let version = StateVersion::of(&initial);
    let cleared = store.clear(1_001).unwrap();
    assert_ne!(initial.store_epoch, cleared.store_epoch);
    assert!(cleared.journal.history_baseline_pending);
    assert!(cleared.journal.records.is_empty());
    assert_eq!(
        store.commit(&version, initial, 1_001).unwrap_err(),
        StoreError::StaleState
    );
    cleanup(&root);
}

#[test]
fn clear_preserves_owner_preference_and_notice() {
    let root = test_root("clear-preferences");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let configured = store
        .merge(&StateVersion::of(&initial), 1_001, |state| {
            state.preferences.user_enabled = Some(false);
            state.preferences.notice_version_seen = DISCLOSURE_VERSION;
        })
        .unwrap();

    let cleared = store.clear(1_002).unwrap();

    assert_ne!(cleared.store_epoch, configured.store_epoch);
    assert_eq!(cleared.preferences, configured.preferences);
    assert!(cleared.journal.records.is_empty());
    cleanup(&root);
}

#[test]
fn setting_off_is_atomic_with_epoch_rotation_and_data_clear() {
    let root = test_root("preference-off");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let with_data = store
        .merge(&StateVersion::of(&initial), 1_001, |state| {
            state.journal.records.push(record(1, 1_001, 16));
            state.preferences.notice_version_seen = DISCLOSURE_VERSION;
        })
        .unwrap();
    write_owner_only(root.join(QUARANTINE_FILE), b"old-quarantine");
    write_owner_only(root.join(BACKUP_QUARANTINE_FILE), b"old-backup-quarantine");

    let disabled = store.set_user_enabled(false, 1_002).unwrap();

    assert_ne!(disabled.store_epoch, with_data.store_epoch);
    assert_eq!(disabled.preferences.user_enabled, Some(false));
    assert_eq!(disabled.preferences.notice_version_seen, DISCLOSURE_VERSION);
    assert!(disabled.journal.records.is_empty());
    for name in [BACKUP_FILE, QUARANTINE_FILE, BACKUP_QUARANTINE_FILE] {
        assert!(!root.join(name).exists(), "payload survived off: {name}");
    }
    cleanup(&root);
}

#[test]
fn clear_removes_all_recoverable_old_payloads() {
    let root = test_root("clear-payloads");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    store
        .merge(&StateVersion::of(&initial), 1_001, |state| {
            state.journal.records.push(record(1, 1_001, 16));
        })
        .unwrap();
    write_owner_only(root.join(QUARANTINE_FILE), b"old-quarantine");
    write_owner_only(root.join(BACKUP_QUARANTINE_FILE), b"old-backup-quarantine");

    store.clear(1_002).unwrap();

    for name in [
        BACKUP_FILE,
        TEMP_FILE,
        BACKUP_TEMP_FILE,
        QUARANTINE_FILE,
        BACKUP_QUARANTINE_FILE,
    ] {
        assert!(!root.join(name).exists(), "payload survived clear: {name}");
    }
    write_owner_only(root.join(CURRENT_FILE), b"broken-current");
    assert_eq!(store.load(1_003).unwrap_err(), StoreError::CorruptState);
    cleanup(&root);
}

#[test]
fn setting_on_and_notice_seen_persist_in_the_main_state() {
    let root = test_root("preference-on-notice");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();

    let enabled = store.set_user_enabled(true, 1_001).unwrap();
    let noticed = store.mark_notice_seen(DISCLOSURE_VERSION, 1_002).unwrap();

    assert_eq!(enabled.preferences.user_enabled, Some(true));
    assert_eq!(noticed.preferences.user_enabled, Some(true));
    assert_eq!(noticed.preferences.notice_version_seen, DISCLOSURE_VERSION);
    assert_eq!(store.load(1_002).unwrap().unwrap(), noticed);
    assert!(noticed.generation > initial.generation);
    cleanup(&root);
}

#[test]
fn explicit_recovery_quarantines_corrupt_payloads_and_rebuilds_selected_preference() {
    let root = test_root("recover-corrupt");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    write_owner_only(root.join(CURRENT_FILE), b"broken-current");
    write_owner_only(root.join(BACKUP_FILE), b"broken-backup");

    let recovered = store.recover_corrupt_state(false, 1_001).unwrap();

    assert_eq!(recovered.preferences.user_enabled, Some(false));
    assert_eq!(recovered.preferences.notice_version_seen, 0);
    assert!(recovered.journal.records.is_empty());
    assert!(root.join(QUARANTINE_FILE).is_file());
    assert!(root.join(BACKUP_QUARANTINE_FILE).is_file());
    assert_eq!(store.load(1_001).unwrap().unwrap(), recovered);
    cleanup(&root);
}

#[test]
fn explicit_recovery_handles_an_oversized_current_payload() {
    let root = test_root("recover-oversized");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    write_owner_only(
        root.join(CURRENT_FILE),
        &vec![b'x'; MAX_STATE_BYTES + ANALYZER_GUARD_BYTES + 1],
    );
    let _ = std::fs::remove_file(root.join(BACKUP_FILE));

    let recovered = store.recover_corrupt_state(true, 1_001).unwrap();

    assert_eq!(recovered.preferences.user_enabled, Some(true));
    assert!(root.join(QUARANTINE_FILE).is_file());
    assert_eq!(store.load(1_001).unwrap().unwrap(), recovered);
    cleanup(&root);
}

#[test]
fn clear_if_current_rejects_a_concurrent_lease_without_removing_it() {
    let root = test_root("clear-cas");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let initial_version = StateVersion::of(&initial);
    let concurrent = store
        .merge(&initial_version, 1_001, |state| {
            state.scheduler.lease = Some(test_lease(state));
        })
        .unwrap();

    assert_eq!(
        store.clear_if_current(&initial_version, 1_002).unwrap_err(),
        StoreError::StaleState
    );
    assert_eq!(
        store
            .load(1_002)
            .unwrap()
            .unwrap()
            .scheduler
            .lease
            .as_ref()
            .unwrap()
            .lease_nonce,
        "concurrent-lease"
    );

    let cleared = store
        .clear_if_current(&StateVersion::of(&concurrent), 1_002)
        .unwrap();
    assert!(cleared.scheduler.lease.is_none());
    cleanup(&root);
}

#[test]
fn write_and_load_enforce_owner_only_modes() {
    let root = test_root("modes");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&root.join(CURRENT_FILE)), 0o600);
    assert_eq!(mode(&root.join(LOCK_FILE)), 0o600);
    assert_eq!(mode(&root.join(KEY_FILE)), 0o600);
    assert!(store.load(1_000).unwrap().is_some());
    cleanup(&root);
}

#[test]
fn analyzer_guard_is_the_fixed_length_prefix_of_the_atomically_replaced_state() {
    let root = test_root("guard-header");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();

    let current = root.join(CURRENT_FILE);
    let metadata = std::fs::metadata(&current).unwrap();
    assert!(metadata.len() > ANALYZER_GUARD_BYTES as u64);
    assert_eq!(mode(&current), 0o600);
    assert!(!root.join("analyzer.guard").exists());
    let bytes = std::fs::read(&current).unwrap();
    let prefix = &bytes[..ANALYZER_GUARD_BYTES];
    let prefix_end = prefix
        .iter()
        .rposition(|byte| *byte != b' ')
        .map(|index| index + 1)
        .unwrap();
    let prefix_header: AnalyzerGuardHeader = serde_json::from_slice(&prefix[..prefix_end]).unwrap();
    let header = read_analyzer_guard(&root).unwrap();
    assert_eq!(header, prefix_header);
    assert_eq!(header.store_epoch, initial.store_epoch);
    assert!(header.lease.is_none());

    let cleared = store.clear(1_001).unwrap();
    let header = read_analyzer_guard(&root).unwrap();
    assert_eq!(header.store_epoch, cleared.store_epoch);
    assert!(header.lease.is_none());

    let mut corrupt_body = std::fs::read(&current).unwrap();
    corrupt_body.truncate(ANALYZER_GUARD_BYTES);
    corrupt_body.extend_from_slice(b"not-state-json");
    std::fs::write(&current, corrupt_body).unwrap();
    assert_eq!(read_analyzer_guard(&root).unwrap(), header);
    cleanup(&root);
}

#[test]
fn open_rejects_symlinked_state_file() {
    let root = test_root("symlink");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    let target = root.join("outside");
    std::fs::write(&target, b"outside").unwrap();
    std::fs::remove_file(root.join(CURRENT_FILE)).unwrap();
    symlink(&target, root.join(CURRENT_FILE)).unwrap();
    assert!(matches!(store.load(1_000), Err(StoreError::UnsafePath(_))));
    cleanup(&root);
}

#[test]
fn operation_rejects_directory_that_became_group_accessible() {
    let root = test_root("directory-mode");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o750)).unwrap();

    assert!(matches!(store.load(1_000), Err(StoreError::UnsafePath(_))));
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    cleanup(&root);
}

#[test]
fn journal_prunes_ttl_count_and_serialized_bytes() {
    let mut state = RecommendationState::empty("epoch".to_string(), 1_000);
    state.journal = ActivityJournal {
        records: (0..500)
            .map(|index| record(index, 1_000 - (index % 300) as u64, 6_000))
            .collect(),
        history_cursor: None,
        history_baseline_pending: false,
    };
    prune_state(&mut state, 1_000).unwrap();
    assert!(state.journal.records.len() <= 200);
    assert!(state
        .journal
        .records
        .iter()
        .all(|record| record.observed_hour_bucket + JOURNAL_TTL_HOURS >= 1_000));
    assert!(serde_json::to_vec(&state.journal).unwrap().len() <= MAX_JOURNAL_BYTES);
}

#[test]
fn oversized_state_is_rejected_without_losing_current() {
    let root = test_root("oversized");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let mut next = initial.clone();
    next.profile.recent_tasks = Vec::new();
    next.feedback.push(
        crate::recommendation::personal_model::RecommendationFeedbackState {
            task_ref: "x".repeat(MAX_STATE_BYTES),
            last_impression_hour_bucket: None,
            last_submitted_hour_bucket: None,
            consecutive_explicit_dismissals: 0,
            last_explicit_dismissal_hour_bucket: None,
            consecutive_overrides: 0,
            last_override_hour_bucket: None,
        },
    );
    assert_eq!(
        store
            .commit(&StateVersion::of(&initial), next, 1_001)
            .unwrap_err(),
        StoreError::StateTooLarge
    );
    assert_eq!(store.load(1_001).unwrap().unwrap(), initial);
    cleanup(&root);
}

#[test]
fn near_limit_state_commits_with_current_backup_and_temp_present() {
    let root = test_root("near-limit-atomic-write");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let mut next = initial.clone();
    next.feedback.push(
        crate::recommendation::personal_model::RecommendationFeedbackState {
            task_ref: String::new(),
            last_impression_hour_bucket: None,
            last_submitted_hour_bucket: None,
            consecutive_explicit_dismissals: 0,
            last_explicit_dismissal_hour_bucket: None,
            consecutive_overrides: 0,
            last_override_hour_bucket: None,
        },
    );
    next.generation = 1;
    next.updated_hour_bucket = 1_001;
    let base_len = serde_json::to_vec(&next).unwrap().len();
    next.feedback[0].task_ref = "x".repeat(MAX_STATE_BYTES - base_len);
    assert_eq!(serde_json::to_vec(&next).unwrap().len(), MAX_STATE_BYTES);

    let first = store
        .commit(&StateVersion::of(&initial), next, 1_001)
        .unwrap();
    let second = store
        .commit(&StateVersion::of(&first), first.clone(), 1_002)
        .unwrap();
    let third = store.commit(&StateVersion::of(&second), second, 1_003);

    assert!(third.is_ok());
    cleanup(&root);
}

#[test]
fn clear_keeps_master_key_but_rotates_epoch_key() {
    let root = test_root("epoch-key");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let master_before = std::fs::read(root.join(KEY_FILE)).unwrap();
    let epoch_key_before = store.epoch_key(&initial.store_epoch).unwrap();

    let cleared = store.clear(1_001).unwrap();

    assert_eq!(std::fs::read(root.join(KEY_FILE)).unwrap(), master_before);
    assert_ne!(
        store.epoch_key(&cleared.store_epoch).unwrap(),
        epoch_key_before
    );
    cleanup(&root);
}

#[test]
fn merge_owns_epoch_and_generation_fields() {
    let root = test_root("merge-version");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let committed = store
        .merge(&StateVersion::of(&initial), 1_001, |state| {
            state.generation = 99;
        })
        .unwrap();
    assert_eq!(committed.generation, initial.generation + 1);

    assert_eq!(
        store
            .merge(&StateVersion::of(&committed), 1_002, |state| {
                state.store_epoch = "replacement".to_string();
            })
            .unwrap_err(),
        StoreError::StaleState
    );
    assert_eq!(store.load(1_002).unwrap().unwrap(), committed);
    cleanup(&root);
}

#[test]
fn corrupt_current_falls_back_to_backup() {
    let root = test_root("backup");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let committed = store
        .merge(&StateVersion::of(&initial), 1_001, |state| {
            state.journal.history_baseline_pending = false;
        })
        .unwrap();
    assert_ne!(initial, committed);
    std::fs::write(root.join(CURRENT_FILE), b"not-json").unwrap();

    assert_eq!(store.load(1_001).unwrap().unwrap(), initial);
    cleanup(&root);
}

#[test]
fn lock_contention_is_nonblocking() {
    let root = test_root("lock");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(LOCK_FILE))
        .unwrap();
    assert_eq!(
        unsafe { nix::libc::flock(lock.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB,) },
        0
    );

    assert_eq!(store.load(1_000).unwrap_err(), StoreError::LockBusy);
    cleanup(&root);
}

#[test]
fn pruning_enforces_profile_snapshot_and_cache_limits() {
    let mut state = RecommendationState::empty("epoch".to_string(), 10_000);
    state.profile.evidence_snapshots = (0..70)
        .map(|index| snapshot(index, 10_000 - index))
        .collect();
    state.profile.evidence_snapshots[69].last_seen_hour_bucket = 10_000 - SNAPSHOT_TTL_HOURS - 1;
    state.profile.recent_tasks = (0..25)
        .map(|index| recent_task(index, 10_000 - index, vec![format!("snapshot-{index}")]))
        .collect();
    state.profile.recent_tasks.push(recent_task(
        99,
        10_000 - RECENT_TTL_HOURS - 1,
        vec!["snapshot-69".to_string()],
    ));
    state.profile.recent_tasks[0].evidence_snapshot_ids =
        (0..70).map(|index| format!("snapshot-{index}")).collect();
    state.cache.candidates = (0..15)
        .map(|index| cache_candidate(index, 10_000 - index, "task-0"))
        .collect();
    state
        .cache
        .candidates
        .push(cache_candidate(99, 10_000 - RECENT_TTL_HOURS - 1, "task-0"));
    let mut health = cache_candidate(100, 10_000, "task-0");
    health.source = CandidateSource::Health;
    state.cache.candidates.push(health);

    prune_state(&mut state, 10_000).unwrap();

    assert!(state.profile.recent_tasks.len() <= MAX_PROFILE_ITEMS);
    assert!(state.profile.evidence_snapshots.len() <= MAX_SNAPSHOTS);
    assert!(state
        .profile
        .evidence_snapshots
        .iter()
        .all(|snapshot| { snapshot.last_seen_hour_bucket + SNAPSHOT_TTL_HOURS >= 10_000 }));
    assert!(state.cache.candidates.len() <= MAX_CACHE_CANDIDATES);
    assert!(state
        .cache
        .candidates
        .iter()
        .all(|candidate| candidate.source == CandidateSource::RecentTask));
    assert!(state
        .profile
        .recent_tasks
        .iter()
        .all(|task| task.last_seen_hour_bucket + RECENT_TTL_HOURS >= 10_000));
}

#[test]
fn pruning_recomputes_cached_evidence_from_retained_snapshots() {
    let now = 10_000;
    let retained_hour = now - 1;
    let expired_hour = now - SNAPSHOT_TTL_HOURS - 1;
    let mut state = RecommendationState::empty("epoch".to_string(), now);
    state.profile.evidence_snapshots = vec![snapshot(0, retained_hour), snapshot(1, expired_hour)];
    state.profile.recent_tasks = vec![recent_task(
        0,
        retained_hour,
        vec!["snapshot-0".to_string(), "snapshot-1".to_string()],
    )];
    let mut candidate = cache_candidate(0, retained_hour, "task-0");
    candidate.evidence.snapshot_ids = vec!["snapshot-0".to_string(), "snapshot-1".to_string()];
    candidate.evidence.agent_request_count = 2;
    candidate.evidence.intent_occurrence_count = 2;
    candidate.evidence.active_day_buckets =
        vec![(expired_hour / 24) as u32, (retained_hour / 24) as u32];
    state.cache.candidates = vec![candidate];

    prune_state(&mut state, now).unwrap();

    let candidate = &state.cache.candidates[0];
    assert_eq!(candidate.evidence.snapshot_ids, ["snapshot-0"]);
    assert_eq!(candidate.evidence.agent_request_count, 1);
    assert_eq!(candidate.evidence.intent_occurrence_count, 1);
    assert_eq!(
        candidate.evidence.active_day_buckets,
        [(retained_hour / 24) as u32]
    );
}

#[test]
fn snapshot_capacity_pruning_recomputes_frequent_eligibility() {
    let now = 10_000;
    let mut state = RecommendationState::empty("epoch".to_string(), now);
    state.profile.evidence_snapshots = (0..=MAX_SNAPSHOTS)
        .map(|index| snapshot(index as u64, now - index as u64))
        .collect();
    state.profile.recent_tasks = vec![recent_task(
        0,
        now,
        (0..=MAX_SNAPSHOTS)
            .map(|index| format!("snapshot-{index}"))
            .collect(),
    )];
    let removed_id = format!("snapshot-{MAX_SNAPSHOTS}");
    state.profile.frequent_patterns = vec![FrequentPattern {
        pattern_id: "pattern-0".to_string(),
        summary: "pattern".to_string(),
        stable_entities: Vec::new(),
        active_day_buckets: vec![414, 415, 416],
        context_affinity: ContextAffinity::default(),
        evidence_snapshot_ids: vec![
            "snapshot-0".to_string(),
            "snapshot-1".to_string(),
            removed_id,
        ],
        prompt_text: "continue pattern".to_string(),
    }];
    let mut candidate = cache_candidate(0, now, "pattern-0");
    candidate.source = CandidateSource::FrequentPattern;
    candidate.evidence.snapshot_ids = state.profile.frequent_patterns[0]
        .evidence_snapshot_ids
        .clone();
    state.cache.candidates = vec![candidate];

    prune_state(&mut state, now).unwrap();

    assert!(state.profile.frequent_patterns.is_empty());
    assert!(state
        .cache
        .candidates
        .iter()
        .all(|candidate| candidate.source != CandidateSource::FrequentPattern));
}

#[test]
fn frequent_ttl_uses_days_recomputed_from_retained_snapshots() {
    let now = 10_000;
    let mut state = RecommendationState::empty("epoch".to_string(), now);
    state.profile.evidence_snapshots = vec![
        snapshot(0, now),
        snapshot(1, now - 24),
        snapshot(2, now - 48),
    ];
    state.profile.frequent_patterns = vec![FrequentPattern {
        pattern_id: "pattern-0".to_string(),
        summary: "pattern".to_string(),
        stable_entities: Vec::new(),
        active_day_buckets: vec![1, 2, 3],
        context_affinity: ContextAffinity::default(),
        evidence_snapshot_ids: vec![
            "snapshot-0".to_string(),
            "snapshot-1".to_string(),
            "snapshot-2".to_string(),
        ],
        prompt_text: "continue pattern".to_string(),
    }];

    prune_state(&mut state, now).unwrap();

    assert_eq!(state.profile.frequent_patterns.len(), 1);
    assert_eq!(
        state.profile.frequent_patterns[0].active_day_buckets,
        [
            (now / 24 - 2) as u32,
            (now / 24 - 1) as u32,
            (now / 24) as u32
        ]
    );
}

#[test]
fn cache_rebuild_backfills_eligible_profile_items_and_keeps_survivor_id() {
    let now = 10_000;
    let expired_hour = now - SNAPSHOT_TTL_HOURS - 1;
    let mut state = RecommendationState::empty("epoch".to_string(), now);
    state.profile.evidence_snapshots = (0..8)
        .map(|index| snapshot(index, now - index))
        .chain(std::iter::once(snapshot(99, expired_hour)))
        .collect();
    state.profile.recent_tasks = (0..8)
        .map(|index| recent_task(index, now - index, vec![format!("snapshot-{index}")]))
        .chain(std::iter::once(recent_task(
            99,
            expired_hour,
            vec!["snapshot-99".to_string()],
        )))
        .collect();
    let mut survivor = cache_candidate(7, now, "task-0");
    survivor.candidate_id = "candidate-stable".to_string();
    let stale = cache_candidate(99, expired_hour, "task-99");
    state.cache.candidates = vec![survivor, stale];

    prune_state(&mut state, now).unwrap();

    assert_eq!(state.cache.candidates.len(), 8);
    assert!(state
        .cache
        .candidates
        .iter()
        .any(|candidate| candidate.task_ref == "task-0"
            && candidate.candidate_id == "candidate-stable"));
    assert!(state
        .cache
        .candidates
        .iter()
        .any(|candidate| candidate.task_ref == "task-7"));
    assert!(!state
        .cache
        .candidates
        .iter()
        .any(|candidate| candidate.task_ref == "task-99"));
}

#[test]
fn load_removes_transient_payloads_and_retains_quarantine() {
    let root = test_root("transient");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    write_owner_only(root.join(TEMP_FILE), b"partial");
    write_owner_only(root.join(BACKUP_TEMP_FILE), b"partial-backup");
    write_owner_only(root.join(QUARANTINE_FILE), b"old");

    assert!(store.load(1_000).unwrap().is_some());
    assert!(!root.join(TEMP_FILE).exists());
    assert!(!root.join(BACKUP_TEMP_FILE).exists());
    assert!(root.join(QUARANTINE_FILE).exists());
    cleanup(&root);
}

#[test]
fn load_reclaims_quarantine_when_payload_total_exceeds_limit() {
    let root = test_root("quarantine-limit");
    let store = PersonalStore::open(&root).unwrap();
    let expected = store.initialize(1_000).unwrap();
    let quarantine = vec![b'x'; MAX_TOTAL_BYTES as usize / 2];
    write_owner_only(root.join(QUARANTINE_FILE), &quarantine);
    write_owner_only(root.join(BACKUP_QUARANTINE_FILE), &quarantine);

    let loaded = store.load(1_001).unwrap().unwrap();

    assert_eq!(loaded.store_epoch, expected.store_epoch);
    assert!(!root.join(QUARANTINE_FILE).exists());
    assert!(!root.join(BACKUP_QUARANTINE_FILE).exists());
    cleanup(&root);
}

fn record(index: usize, hour: u64, bytes: usize) -> ActivityRecord {
    ActivityRecord {
        activity_id: format!("activity-{index}"),
        session_scope_id: Some("scope".to_string()),
        source_fingerprint: format!("fingerprint-{index}"),
        observed_hour_bucket: hour,
        source: ActivitySource::ShellCommand,
        context: ActivityContext::default(),
        payload: ActivityPayload::ShellCommand {
            command: "x".repeat(bytes),
            origin: ShellActivityOrigin::Interactive,
            parent_request_activity_id: None,
            outcome: ActivityOutcome::Success,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    }
}

fn test_lease(state: &RecommendationState) -> crate::recommendation::personal_model::AnalyzerLease {
    crate::recommendation::personal_model::AnalyzerLease {
        owner_session_id: "concurrent-session".to_string(),
        lease_nonce: "concurrent-lease".to_string(),
        owner_pid: 10,
        owner_start_identity: "owner-start".to_string(),
        core_leader_pid: Some(20),
        core_leader_start_identity: Some("leader-start".to_string()),
        core_process_group_id: Some(20),
        base_epoch: state.store_epoch.clone(),
        base_generation: state.generation,
        expires_unix_secs: 2_000,
    }
}

fn snapshot(index: u64, hour: u64) -> EvidenceSnapshot {
    EvidenceSnapshot {
        snapshot_id: format!("snapshot-{index}"),
        source_kinds: vec![ActivitySource::AgentRequest],
        first_seen_hour_bucket: hour,
        last_seen_hour_bucket: hour,
        active_day_buckets: vec![(hour / 24) as u32],
        context_affinity: ContextAffinity::default(),
        entities: Vec::new(),
        agent_request_count: 1,
        compatible_shell_count: 0,
        submitted_feedback_count: 0,
        intent_occurrence_count: 1,
        last_action_failed: false,
    }
}

fn recent_task(index: u64, hour: u64, snapshot_ids: Vec<String>) -> RecentTask {
    RecentTask {
        task_id: format!("task-{index}"),
        summary: format!("task {index}"),
        entities: Vec::new(),
        context_affinity: ContextAffinity::default(),
        last_seen_hour_bucket: hour,
        evidence_snapshot_ids: snapshot_ids,
        prompt_text: format!("continue task {index}"),
    }
}

fn cache_candidate(index: u64, hour: u64, task_ref: &str) -> CachedPromptCandidate {
    CachedPromptCandidate {
        candidate_id: format!("candidate-{index}"),
        source: CandidateSource::RecentTask,
        task_ref: task_ref.to_string(),
        prompt_text: "continue".to_string(),
        context_affinity: ContextAffinity::default(),
        last_seen_hour_bucket: hour,
        last_action_failed: false,
        evidence: CandidateEvidenceSummary::default(),
        entities: Vec::new(),
    }
}

fn write_owner_only(path: std::path::PathBuf, bytes: &[u8]) {
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn test_root(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "cosh-personal-store-{label}-{}",
        crate::recommendation::personal_crypto::random_hex(12).unwrap()
    ))
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn cleanup(root: &std::path::Path) {
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
}
