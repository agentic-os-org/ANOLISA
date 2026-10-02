use super::*;
use crate::recommendation::personal_model::{
    CandidateEvidenceSummary, CandidateSource, ContextAffinity, EntityEvidenceRef, EntityKind,
    EntityVolatility, ScopeKind, TaskEntity,
};

const NOW: u64 = 10_000;

fn candidate(
    id: &str,
    source: CandidateSource,
    repo_id: Option<&str>,
    host_id: Option<&str>,
    age_hours: u64,
) -> PlannerCandidate {
    PlannerCandidate {
        candidate_id: id.to_string(),
        source,
        task_ref: format!("task-{id}"),
        prompt_text: format!("continue {id}"),
        context_affinity: ContextAffinity {
            scope_kind: if repo_id.is_some() {
                ScopeKind::Repo
            } else {
                ScopeKind::HostFallback
            },
            repo_id: repo_id.map(str::to_string),
            host_id: host_id.map(str::to_string),
        },
        last_seen_hour_bucket: NOW - age_hours,
        evidence: CandidateEvidenceSummary {
            snapshot_ids: vec![format!("snapshot-{id}")],
            agent_request_count: 1,
            compatible_shell_count: 0,
            submitted_feedback_count: 0,
            intent_occurrence_count: 1,
            active_day_buckets: vec![1],
            continuation_evidence: false,
        },
        entities: Vec::new(),
        suppression_key: format!("suppress-{id}"),
        last_action_failed: false,
        consecutive_explicit_dismissals: 0,
        suppressed: false,
    }
}

fn context() -> PlannerContext {
    PlannerContext {
        now_hour_bucket: NOW,
        repo_id: Some("repo-a".to_string()),
        host_id: Some("host-a".to_string()),
    }
}

#[test]
fn pending_or_timed_out_health_never_shows_personal_candidates() {
    let personal = vec![candidate(
        "recent",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    )];

    for health in [HealthResolution::Pending, HealthResolution::TimedOut] {
        let rendered = plan_startup(&context(), health, &personal);
        assert!(rendered.visible_candidates.is_empty());
        assert!(rendered.ghost_candidate.is_none());
    }
}

#[test]
fn repo_mismatch_cannot_fall_back_to_the_same_host() {
    let mismatch = candidate(
        "other-repo",
        CandidateSource::RecentTask,
        Some("repo-b"),
        Some("host-a"),
        1,
    );
    let fallback = candidate(
        "host-fallback",
        CandidateSource::RecentTask,
        None,
        Some("host-a"),
        1,
    );

    let rendered = plan_startup(
        &context(),
        HealthResolution::Resolved(&[]),
        &[mismatch, fallback],
    );

    assert_eq!(
        rendered
            .visible_candidates
            .iter()
            .map(|candidate| candidate.candidate_id.as_str())
            .collect::<Vec<_>>(),
        vec!["host-fallback"]
    );
}

#[test]
fn repo_scope_never_falls_back_to_host_when_repo_identity_is_missing() {
    let mut repo_scoped = candidate(
        "repo-scoped",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    );
    repo_scoped.context_affinity.repo_id = None;

    let rendered = plan_startup(&context(), HealthResolution::Resolved(&[]), &[repo_scoped]);

    assert!(rendered.visible_candidates.is_empty());
}

#[test]
fn dormant_recent_requires_same_repo_and_continuation_evidence() {
    let stale = candidate(
        "stale",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        8 * 24,
    );
    let mut continuation = stale.clone();
    continuation.candidate_id = "continuation".to_string();
    continuation.task_ref = "task-continuation".to_string();
    continuation.evidence.continuation_evidence = true;

    let rendered = plan_startup(
        &context(),
        HealthResolution::Resolved(&[]),
        &[stale, continuation],
    );

    assert_eq!(rendered.visible_candidates.len(), 1);
    assert_eq!(rendered.visible_candidates[0].candidate_id, "continuation");
}

#[test]
fn frequent_requires_three_days_and_thirty_day_recency() {
    let mut eligible = candidate(
        "frequent",
        CandidateSource::FrequentPattern,
        Some("repo-a"),
        Some("host-a"),
        29 * 24,
    );
    eligible.evidence.active_day_buckets = vec![1, 2, 3];
    let mut too_few_days = eligible.clone();
    too_few_days.candidate_id = "two-days".to_string();
    too_few_days.evidence.active_day_buckets = vec![1, 2];
    let mut too_old = eligible.clone();
    too_old.candidate_id = "old".to_string();
    too_old.last_seen_hour_bucket = NOW - 31 * 24;

    let rendered = plan_startup(
        &context(),
        HealthResolution::Resolved(&[]),
        &[too_few_days, too_old, eligible],
    );

    assert_eq!(rendered.visible_candidates.len(), 1);
    assert_eq!(rendered.visible_candidates[0].candidate_id, "frequent");
}

#[test]
fn health_stays_first_deduplicates_entities_and_caps_at_three() {
    let entity = EntityEvidenceRef {
        entity: TaskEntity {
            kind: EntityKind::Service,
            value: "payment-api".to_string(),
            volatility: EntityVolatility::Stable,
        },
        snapshot_ids: vec!["snapshot-health".to_string()],
    };
    let mut health = candidate(
        "health",
        CandidateSource::Health,
        Some("repo-a"),
        Some("host-a"),
        0,
    );
    health.entities.push(entity.clone());
    let second_health = candidate(
        "health-2",
        CandidateSource::Health,
        Some("repo-a"),
        Some("host-a"),
        0,
    );
    let mut duplicate = candidate(
        "duplicate",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    );
    duplicate.entities.push(entity);
    let recent = candidate(
        "recent",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    );
    let frequent = candidate(
        "frequent",
        CandidateSource::FrequentPattern,
        Some("repo-a"),
        Some("host-a"),
        1,
    );
    let mut frequent = frequent;
    frequent.evidence.active_day_buckets = vec![1, 2, 3];

    let rendered = plan_startup(
        &context(),
        HealthResolution::Resolved(&[health, second_health]),
        &[duplicate, recent, frequent],
    );

    assert_eq!(
        rendered
            .visible_candidates
            .iter()
            .map(|candidate| candidate.candidate_id.as_str())
            .collect::<Vec<_>>(),
        vec!["health", "health-2", "recent"]
    );
    assert_eq!(
        rendered
            .ghost_candidate
            .as_ref()
            .map(|candidate| candidate.candidate_id.as_str()),
        Some("health")
    );
}

#[test]
fn omitted_reasons_distinguish_all_render_decisions() {
    let mut low = candidate(
        "low",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    );
    low.evidence.intent_occurrence_count = 0;
    low.evidence.compatible_shell_count = 1;
    let scope = candidate(
        "scope",
        CandidateSource::RecentTask,
        Some("repo-b"),
        Some("host-a"),
        1,
    );
    let mut suppressed = candidate(
        "suppressed",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    );
    suppressed.suppressed = true;
    let first = candidate(
        "first",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    );
    let mut duplicate = first.clone();
    duplicate.candidate_id = "duplicate".to_string();
    let rendered = plan_startup(
        &context(),
        HealthResolution::Resolved(&[]),
        &[low, scope, suppressed, first, duplicate],
    );

    assert_eq!(rendered.omitted_reasons[&OmittedReason::LowConfidence], 1);
    assert_eq!(rendered.omitted_reasons[&OmittedReason::ScopeMismatch], 1);
    assert_eq!(rendered.omitted_reasons[&OmittedReason::Suppressed], 1);
    assert_eq!(rendered.omitted_reasons[&OmittedReason::Duplicate], 1);

    let health = [
        candidate("health-1", CandidateSource::Health, None, None, 0),
        candidate("health-2", CandidateSource::Health, None, None, 0),
        candidate("health-3", CandidateSource::Health, None, None, 0),
    ];
    let personal = [candidate(
        "capacity",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    )];
    let capacity = plan_startup(&context(), HealthResolution::Resolved(&health), &personal);
    assert_eq!(capacity.omitted_reasons[&OmittedReason::Capacity], 1);
}

#[test]
fn a_shared_repo_entity_does_not_merge_distinct_entity_signatures() {
    fn entity(kind: EntityKind, value: &str) -> EntityEvidenceRef {
        EntityEvidenceRef {
            entity: TaskEntity {
                kind,
                value: value.to_string(),
                volatility: EntityVolatility::Stable,
            },
            snapshot_ids: vec![format!("snapshot-{value}")],
        }
    }

    let mut health = candidate(
        "health",
        CandidateSource::Health,
        Some("repo-a"),
        Some("host-a"),
        0,
    );
    health.entities = vec![
        entity(EntityKind::Repo, "repo-a"),
        entity(EntityKind::Service, "payments"),
    ];
    let mut recent = candidate(
        "recent",
        CandidateSource::RecentTask,
        Some("repo-a"),
        Some("host-a"),
        1,
    );
    recent.entities = vec![
        entity(EntityKind::Repo, "repo-a"),
        entity(EntityKind::Service, "orders"),
    ];

    let rendered = plan_startup(&context(), HealthResolution::Resolved(&[health]), &[recent]);

    assert_eq!(rendered.visible_candidates.len(), 2);
}

#[test]
fn offline_profile_context_matrix_has_no_cross_scope_leakage_and_abstains_when_required() {
    let mut eligible_cases = 0usize;
    let mut top1_hits = 0usize;
    let mut top3_hits = 0usize;
    let mut abstain_cases = 0usize;

    for index in 0..64 {
        let repo_id = format!("repo-{index}");
        let context = PlannerContext {
            now_hour_bucket: NOW,
            repo_id: Some(repo_id.clone()),
            host_id: Some("host-a".to_string()),
        };
        let mut relevant = candidate(
            &format!("relevant-{index}"),
            CandidateSource::RecentTask,
            Some(&repo_id),
            Some("host-a"),
            1,
        );
        let spooky = candidate(
            &format!("spooky-{index}"),
            CandidateSource::RecentTask,
            Some("other-repo"),
            Some("host-a"),
            1,
        );

        let (personal, expected_id) = if index < 32 {
            eligible_cases += 1;
            (vec![spooky, relevant], Some(format!("relevant-{index}")))
        } else if index < 48 {
            abstain_cases += 1;
            (vec![spooky], None)
        } else {
            abstain_cases += 1;
            relevant.evidence.intent_occurrence_count = 0;
            relevant.evidence.compatible_shell_count = 1;
            (vec![spooky, relevant], None)
        };

        let rendered = plan_startup(&context, HealthResolution::Resolved(&[]), &personal);
        assert!(rendered.visible_candidates.iter().all(|candidate| {
            candidate.context_affinity.repo_id.as_deref() == Some(repo_id.as_str())
        }));
        assert!(!rendered
            .visible_candidates
            .iter()
            .any(|candidate| candidate.candidate_id.starts_with("spooky-")));

        if let Some(expected_id) = expected_id {
            top1_hits += usize::from(
                rendered
                    .visible_candidates
                    .first()
                    .is_some_and(|candidate| candidate.candidate_id == expected_id),
            );
            top3_hits += usize::from(
                rendered
                    .visible_candidates
                    .iter()
                    .any(|candidate| candidate.candidate_id == expected_id),
            );
        } else {
            assert!(rendered.visible_candidates.is_empty(), "case {index}");
        }
    }

    assert_eq!(eligible_cases, 32);
    assert_eq!(top1_hits * 100 / eligible_cases, 100);
    assert_eq!(top3_hits * 100 / eligible_cases, 100);
    assert_eq!(abstain_cases, 32);
}
