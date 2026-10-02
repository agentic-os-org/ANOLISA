use super::*;

pub(crate) fn prune_state(
    state: &mut RecommendationState,
    now_hour_bucket: u64,
) -> Result<(), StoreError> {
    state.journal.records.retain(|record| {
        !expired(
            record.observed_hour_bucket,
            now_hour_bucket,
            JOURNAL_TTL_HOURS,
        )
    });
    state
        .journal
        .records
        .sort_by_key(|record| record.observed_hour_bucket);
    if state.journal.records.len() > MAX_JOURNAL_RECORDS {
        let remove = state.journal.records.len() - MAX_JOURNAL_RECORDS;
        state.journal.records.drain(..remove);
    }
    while serde_json::to_vec(&state.journal)
        .map_err(|_| StoreError::CorruptState)?
        .len()
        > MAX_JOURNAL_BYTES
    {
        if state.journal.records.is_empty() {
            return Err(StoreError::StateTooLarge);
        }
        state.journal.records.remove(0);
    }

    prune_snapshots(state, now_hour_bucket)?;
    reconcile_profile_snapshot_evidence(&mut state.profile, now_hour_bucket);
    state.profile.recent_tasks.retain(|task| {
        !expired(
            task.last_seen_hour_bucket,
            now_hour_bucket,
            RECENT_TTL_HOURS,
        )
    });
    state.profile.frequent_patterns.retain(|pattern| {
        pattern
            .active_day_buckets
            .iter()
            .copied()
            .max()
            .is_some_and(|day| !expired(u64::from(day) * 24, now_hour_bucket, FREQUENT_TTL_HOURS))
    });
    trim_profile_items(state);
    prune_snapshots(state, now_hour_bucket)?;
    rebuild_cache(&mut state.cache, &state.profile, now_hour_bucket);
    prune_cache(state, now_hour_bucket);
    if serialize_state(state)?.len() > MAX_STATE_BYTES {
        return Err(StoreError::StateTooLarge);
    }
    Ok(())
}

fn trim_profile_items(state: &mut RecommendationState) {
    while state.profile.recent_tasks.len() + state.profile.frequent_patterns.len()
        > MAX_PROFILE_ITEMS
    {
        let recent = state
            .profile
            .recent_tasks
            .iter()
            .enumerate()
            .min_by_key(|(_, task)| task.last_seen_hour_bucket)
            .map(|(index, task)| (index, task.last_seen_hour_bucket));
        let frequent = state
            .profile
            .frequent_patterns
            .iter()
            .enumerate()
            .min_by_key(|(_, pattern)| {
                pattern
                    .active_day_buckets
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0)
            })
            .map(|(index, pattern)| {
                (
                    index,
                    u64::from(
                        pattern
                            .active_day_buckets
                            .iter()
                            .copied()
                            .max()
                            .unwrap_or(0),
                    ) * 24,
                )
            });
        if matches!((recent, frequent), (Some((_, recent_hour)), Some((_, frequent_hour))) if recent_hour <= frequent_hour)
            || frequent.is_none()
        {
            if let Some((index, _)) = recent {
                state.profile.recent_tasks.remove(index);
            }
        } else if let Some((index, _)) = frequent {
            state.profile.frequent_patterns.remove(index);
        }
    }
}

fn prune_snapshots(
    state: &mut RecommendationState,
    now_hour_bucket: u64,
) -> Result<(), StoreError> {
    let referenced = referenced_snapshot_ids(state);
    state.profile.evidence_snapshots.retain(|snapshot| {
        referenced.contains(snapshot.snapshot_id.as_str())
            && !expired(
                snapshot.last_seen_hour_bucket,
                now_hour_bucket,
                SNAPSHOT_TTL_HOURS,
            )
            && serde_json::to_vec(snapshot)
                .map(|bytes| bytes.len() <= MAX_SNAPSHOT_BYTES)
                .unwrap_or(false)
    });
    state
        .profile
        .evidence_snapshots
        .sort_by_key(|snapshot| std::cmp::Reverse(snapshot.last_seen_hour_bucket));
    state.profile.evidence_snapshots.truncate(MAX_SNAPSHOTS);
    let retained = state
        .profile
        .evidence_snapshots
        .iter()
        .map(|snapshot| snapshot.snapshot_id.as_str())
        .collect::<HashSet<_>>();
    for task in &mut state.profile.recent_tasks {
        task.evidence_snapshot_ids
            .retain(|id| retained.contains(id.as_str()));
    }
    for pattern in &mut state.profile.frequent_patterns {
        pattern
            .evidence_snapshot_ids
            .retain(|id| retained.contains(id.as_str()));
    }
    state
        .profile
        .recent_tasks
        .retain(|task| !task.evidence_snapshot_ids.is_empty());
    state
        .profile
        .frequent_patterns
        .retain(|pattern| !pattern.evidence_snapshot_ids.is_empty());
    Ok(())
}

fn referenced_snapshot_ids(state: &RecommendationState) -> HashSet<String> {
    state
        .profile
        .recent_tasks
        .iter()
        .flat_map(|task| task.evidence_snapshot_ids.iter().cloned())
        .chain(
            state
                .profile
                .frequent_patterns
                .iter()
                .flat_map(|pattern| pattern.evidence_snapshot_ids.iter().cloned()),
        )
        .collect()
}

fn prune_cache(state: &mut RecommendationState, now_hour_bucket: u64) {
    let recent_ids = state
        .profile
        .recent_tasks
        .iter()
        .map(|task| task.task_id.as_str())
        .collect::<HashSet<_>>();
    let pattern_ids = state
        .profile
        .frequent_patterns
        .iter()
        .map(|pattern| pattern.pattern_id.as_str())
        .collect::<HashSet<_>>();
    state
        .cache
        .candidates
        .retain(|candidate| match candidate.source {
            CandidateSource::RecentTask => {
                recent_ids.contains(candidate.task_ref.as_str())
                    && !expired(
                        candidate.last_seen_hour_bucket,
                        now_hour_bucket,
                        RECENT_TTL_HOURS,
                    )
            }
            CandidateSource::FrequentPattern => {
                pattern_ids.contains(candidate.task_ref.as_str())
                    && !expired(
                        candidate.last_seen_hour_bucket,
                        now_hour_bucket,
                        FREQUENT_TTL_HOURS,
                    )
            }
            CandidateSource::Health => false,
        });
    state
        .cache
        .candidates
        .sort_by_key(|candidate| std::cmp::Reverse(candidate.last_seen_hour_bucket));
    state.cache.candidates.truncate(MAX_CACHE_CANDIDATES);
}

fn expired(seen: u64, now: u64, ttl: u64) -> bool {
    now.saturating_sub(seen) > ttl
}
