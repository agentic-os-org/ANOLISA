use super::*;
use crate::recommendation::personal_model::{
    ActivityContext, ActivityJournal, ActivityOutcome, ActivityPayload, ActivityRecord,
    ActivitySource, AgentRequestBindingKind, AnalyzerSchedulerState, CandidateSource, EntityKind,
    EntityVolatility, RecommendationCache, RecommendationFeedbackState, RecommendationPreferences,
    RecommendationState, RedactionReport, ScopeKind, ToolCategory, UserWorkProfile,
    RECOMMENDATION_SCHEMA_VERSION,
};

fn request(id: &str, session: &str, text: &str, hour: u64) -> ActivityRecord {
    ActivityRecord {
        activity_id: id.to_string(),
        session_scope_id: Some(session.to_string()),
        source_fingerprint: format!("fp-{id}"),
        observed_hour_bucket: hour,
        source: ActivitySource::AgentRequest,
        context: ActivityContext {
            host_id: Some("host-1".to_string()),
            repo_id: Some("repo-1".to_string()),
            repo_name: Some("payment".to_string()),
            cwd_relative: Some("services/api".to_string()),
        },
        payload: ActivityPayload::AgentRequest {
            text: text.to_string(),
            binding: AgentRequestBindingKind::FreeForm,
            context_command_activity_id: None,
            intent_lifecycle_id: format!("intent-{id}"),
            system_recommended_skill: None,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    }
}

fn state(records: Vec<ActivityRecord>) -> RecommendationState {
    RecommendationState {
        schema_version: RECOMMENDATION_SCHEMA_VERSION,
        store_epoch: "epoch-1".to_string(),
        generation: 7,
        updated_hour_bucket: 500,
        preferences: RecommendationPreferences::default(),
        journal: ActivityJournal {
            records,
            history_cursor: None,
            history_baseline_pending: false,
        },
        profile: UserWorkProfile::default(),
        cache: RecommendationCache::default(),
        feedback: Vec::<RecommendationFeedbackState>::new(),
        scheduler: AnalyzerSchedulerState::default(),
    }
}

#[test]
fn generated_terminal_text_must_be_a_single_printable_line() {
    for unsafe_text in [
        "inspect payment-api\nrun kubectl",
        "inspect payment-api\rreplace prompt",
        "inspect payment-api\tthen deploy",
        "inspect payment-api\u{1b}[2J",
        "inspect payment-api\u{7f}",
        "inspect payment-api\u{2028}then deploy",
    ] {
        assert!(validate_text(unsafe_text, "safe prompt").is_err());
        assert!(validate_text("safe summary", unsafe_text).is_err());
    }
    assert!(validate_text("分析 payment-api 异常", "检查 payment-api 最近日志").is_ok());
}

#[test]
fn input_keeps_one_trigger_under_byte_and_record_limits() {
    let mut records = (0..30)
        .map(|index| {
            request(
                &format!("old-{index}"),
                "other",
                &"x".repeat(700),
                400 + index,
            )
        })
        .collect::<Vec<_>>();
    records.push(request("trigger", "session-1", &"任务".repeat(3000), 500));

    let built = build_input(&state(records), "session-1", 500).expect("build input");

    assert!(built.json.len() <= MAX_DYNAMIC_INPUT_BYTES);
    assert!(built.activity_ids.len() <= MAX_ACTIVITY_RECORDS);
    assert!(built.activity_ids.contains(&"trigger".to_string()));
    assert_eq!(
        built.activity_ids.first().map(String::as_str),
        Some("trigger")
    );
}

#[test]
fn full_profile_and_snapshot_capacity_still_keeps_current_scope_trigger() {
    let mut source = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment",
        500,
    )]);
    source.profile.evidence_snapshots = (0..60)
        .map(|index| EvidenceSnapshot {
            snapshot_id: format!("snapshot-{index}"),
            source_kinds: vec![ActivitySource::AgentRequest],
            first_seen_hour_bucket: index,
            last_seen_hour_bucket: index,
            active_day_buckets: vec![(index / 24) as u32],
            context_affinity: ContextAffinity {
                scope_kind: ScopeKind::Repo,
                repo_id: Some("repo-1".to_string()),
                host_id: Some("host-1".to_string()),
            },
            entities: Vec::new(),
            agent_request_count: 1,
            compatible_shell_count: 0,
            submitted_feedback_count: 0,
            intent_occurrence_count: 1,
            last_action_failed: false,
        })
        .collect();
    source.profile.recent_tasks = (0..10)
        .map(|index| RecentTask {
            task_id: format!("task-{index}"),
            summary: format!("task {index}"),
            entities: vec![profile_entity(index)],
            context_affinity: profile_affinity(index),
            last_seen_hour_bucket: 500 - index,
            evidence_snapshot_ids: vec![format!("snapshot-{index}")],
            prompt_text: format!("continue task {index}"),
        })
        .collect();
    source.profile.frequent_patterns = (0..10)
        .map(|index| FrequentPattern {
            pattern_id: format!("pattern-{index}"),
            summary: format!("pattern {index}"),
            stable_entities: vec![profile_entity(index)],
            active_day_buckets: vec![1, 2, 3],
            context_affinity: profile_affinity(index),
            evidence_snapshot_ids: vec![format!("snapshot-{}", index + 10)],
            prompt_text: format!("continue pattern {index}"),
        })
        .collect();

    let built = build_input(&source, "session-1", 500).expect("build full input");
    let envelope: serde_json::Value = serde_json::from_str(&built.json).expect("input JSON");

    assert!(built.json.len() <= MAX_DYNAMIC_INPUT_BYTES);
    assert_eq!(
        envelope["new_activities"][0]["activity_id"].as_str(),
        Some("trigger")
    );
    assert!(!built.json.contains("repo-out-of-scope"));
    assert_eq!(source.profile.evidence_snapshots.len(), 60);
    assert_eq!(
        source.profile.recent_tasks.len() + source.profile.frequent_patterns.len(),
        20
    );
}

#[test]
fn previous_profile_keeps_bounded_summary_and_prompt_semantics() {
    let mut source = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment-api",
        500,
    )]);
    source.profile.recent_tasks.push(RecentTask {
        task_id: "task-payment".to_string(),
        summary: "payment-api ".repeat(40),
        entities: vec![profile_entity(0)],
        context_affinity: profile_affinity(0),
        last_seen_hour_bucket: 499,
        evidence_snapshot_ids: vec!["snapshot-payment".to_string()],
        prompt_text: "continue diagnosing payment-api ".repeat(40),
    });

    let built = build_input(&source, "session-1", 500).expect("build input");
    let envelope: serde_json::Value = serde_json::from_str(&built.json).expect("input JSON");
    let task = &envelope["previous_profile"]["recent_tasks"][0];

    assert_eq!(task["id"].as_str(), Some("task-payment"));
    assert!(task["summary"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(task["prompt_text"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(task["summary"].as_str().unwrap().len() <= 256);
    assert!(task["prompt_text"].as_str().unwrap().len() <= 512);
}

#[test]
fn full_snapshot_store_rolls_oldest_snapshot_instead_of_rejecting_merge() {
    let mut source = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment",
        500,
    )]);
    source.profile.evidence_snapshots = (0..60)
        .map(|index| EvidenceSnapshot {
            snapshot_id: format!("snapshot-{index}"),
            source_kinds: vec![ActivitySource::AgentRequest],
            first_seen_hour_bucket: index,
            last_seen_hour_bucket: index,
            active_day_buckets: vec![(index / 24) as u32],
            context_affinity: profile_affinity(0),
            entities: Vec::new(),
            agent_request_count: 1,
            compatible_shell_count: 0,
            submitted_feedback_count: 0,
            intent_occurrence_count: 1,
            last_action_failed: false,
        })
        .collect();
    let built = build_input(&source, "session-1", 500).expect("build input");
    let output = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"inspect payment",
            "entities":[{"kind":"repo","value":"payment","volatility":"stable"}],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue inspecting payment"
        }],
        "frequent_patterns":[]
    }"#;

    let patch = prepare_merge_patch(output, &built, &source, &mut TestIds::default())
        .expect("full store rolls forward");
    apply_merge_patch(&mut source, patch).expect("apply rolling merge");

    assert_eq!(source.profile.evidence_snapshots.len(), 60);
    assert!(!source
        .profile
        .evidence_snapshots
        .iter()
        .any(|snapshot| snapshot.snapshot_id == "snapshot-0"));
    assert!(source
        .profile
        .recent_tasks
        .iter()
        .flat_map(|task| task.evidence_snapshot_ids.iter())
        .all(|id| source
            .profile
            .evidence_snapshots
            .iter()
            .any(|snapshot| &snapshot.snapshot_id == id)));
}

fn profile_affinity(index: u64) -> ContextAffinity {
    ContextAffinity {
        scope_kind: ScopeKind::Repo,
        repo_id: Some(if index.is_multiple_of(2) {
            "repo-1".to_string()
        } else {
            "repo-out-of-scope".to_string()
        }),
        host_id: Some("host-1".to_string()),
    }
}

fn profile_entity(index: u64) -> TaskEntity {
    TaskEntity {
        kind: EntityKind::Repo,
        value: if index.is_multiple_of(2) {
            "payment".to_string()
        } else {
            "repo-out-of-scope".to_string()
        },
        volatility: EntityVolatility::Stable,
    }
}

#[test]
fn strict_output_rejects_unknown_fields_and_discard_overlap() {
    let source = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment",
        500,
    )]);
    let built = build_input(&source, "session-1", 500).expect("build input");
    let unknown =
        r#"{"discarded_activities":[],"recent_tasks":[],"frequent_patterns":[],"extra":1}"#;
    assert!(matches!(
        prepare_merge_patch(unknown, &built, &source, &mut TestIds::default()),
        Err(AnalyzerError::InvalidJson)
    ));

    let overlap = r#"{
        "discarded_activities":[{"activity_id":"trigger","reason":"no_recommendation_value"}],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"inspect payment",
            "entities":[],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"inspect payment"
        }],
        "frequent_patterns":[]
    }"#;
    assert!(matches!(
        prepare_merge_patch(overlap, &built, &source, &mut TestIds::default()),
        Err(AnalyzerError::DiscardEvidenceOverlap)
    ));
}

#[test]
fn strict_output_rejects_missing_arrays_unknown_enums_and_markdown_fences() {
    let source = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment",
        500,
    )]);
    let built = build_input(&source, "session-1", 500).expect("build input");

    for invalid in [
        r#"{"discarded_activities":[],"recent_tasks":[]}"#,
        r#"{"discarded_activities":[{"activity_id":"trigger","reason":"maybe_later"}],"recent_tasks":[],"frequent_patterns":[]}"#,
        "```json\n{\"discarded_activities\":[],\"recent_tasks\":[],\"frequent_patterns\":[]}\n```",
    ] {
        assert_eq!(
            prepare_merge_patch(invalid, &built, &source, &mut TestIds::default())
                .expect_err("strict schema must reject malformed output"),
            AnalyzerError::InvalidJson
        );
    }
}

#[test]
fn prompt_and_provider_budgets_are_byte_exact() {
    let input = BuiltAnalyzerInput {
        json: "i".repeat(MAX_DYNAMIC_INPUT_BYTES),
        activity_ids: vec![],
        base_epoch: String::new(),
        base_profile_generation: 0,
        trusted_now_hour_bucket: 0,
    };

    assert_eq!(
        validate_provider_budget(&"p".repeat(MAX_FIXED_PROMPT_BYTES), &input),
        Ok(())
    );
    assert_eq!(
        validate_provider_budget(&"p".repeat(MAX_FIXED_PROMPT_BYTES + 1), &input),
        Err(AnalyzerError::PromptTooLarge)
    );
    assert!(build_fixed_prompt(&"x".repeat(MAX_FIXED_PROMPT_BYTES)).is_err());
}

#[test]
fn entity_values_must_be_grounded_in_evidence() {
    let source = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment",
        500,
    )]);
    let built = build_input(&source, "session-1", 500).expect("build input");
    let hallucinated = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"inspect payment",
            "entities":[{"kind":"repo","value":"unseen-repo","volatility":"stable"}],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"inspect payment"
        }],
        "frequent_patterns":[]
    }"#;

    assert!(matches!(
        prepare_merge_patch(hallucinated, &built, &source, &mut TestIds::default()),
        Err(AnalyzerError::UngroundedEntity)
    ));
}

#[test]
fn identifier_like_prompt_references_must_be_grounded_without_entities() {
    let source = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment-api",
        500,
    )]);
    let built = build_input(&source, "session-1", 500).expect("build input");
    let hallucinated = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"inspect unseen-service",
            "entities":[],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue diagnosing unseen-service"
        }],
        "frequent_patterns":[]
    }"#;

    assert!(matches!(
        prepare_merge_patch(hallucinated, &built, &source, &mut TestIds::default()),
        Err(AnalyzerError::UngroundedEntity)
    ));

    let grounded = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"inspect payment-api",
            "entities":[{"kind":"service","value":"payment-api","volatility":"stable"}],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue diagnosing payment-api failures"
        }],
        "frequent_patterns":[]
    }"#;
    prepare_merge_patch(grounded, &built, &source, &mut TestIds::default())
        .expect("grounded identifier and ordinary verbs are allowed");
}

#[test]
fn cache_prefers_newest_trusted_recent_tasks_before_capacity_cutoff() {
    let recent = (0..11)
        .map(|index| RecentTask {
            task_id: format!("task-{index}"),
            summary: format!("task {index}"),
            entities: Vec::new(),
            context_affinity: profile_affinity(0),
            last_seen_hour_bucket: if index == 0 { 900 } else { 400 + index },
            evidence_snapshot_ids: Vec::new(),
            prompt_text: format!("continue task {index}"),
        })
        .collect::<Vec<_>>();

    let cache = build_cache(&recent, &[], &[], 1, 500, &mut TestIds::default());
    let refs = cache
        .candidates
        .iter()
        .map(|candidate| candidate.task_ref.as_str())
        .collect::<Vec<_>>();

    assert_eq!(refs.first().copied(), Some("task-10"));
    assert!(refs.contains(&"task-1"));
    assert!(!refs.contains(&"task-0"));
}

#[test]
fn cache_reserves_room_for_distinct_frequent_patterns() {
    let recent = (0..10)
        .map(|index| RecentTask {
            task_id: format!("task-{index}"),
            summary: format!("task {index}"),
            entities: Vec::new(),
            context_affinity: profile_affinity(0),
            last_seen_hour_bucket: 400 + index,
            evidence_snapshot_ids: Vec::new(),
            prompt_text: format!("continue task {index}"),
        })
        .collect::<Vec<_>>();
    let frequent = vec![
        FrequentPattern {
            pattern_id: "duplicate-pattern".to_string(),
            summary: "duplicate".to_string(),
            stable_entities: Vec::new(),
            active_day_buckets: vec![1, 2, 3],
            context_affinity: profile_affinity(0),
            evidence_snapshot_ids: Vec::new(),
            prompt_text: "continue task 9".to_string(),
        },
        FrequentPattern {
            pattern_id: "distinct-pattern".to_string(),
            summary: "weekly incidents".to_string(),
            stable_entities: Vec::new(),
            active_day_buckets: vec![1, 2, 3],
            context_affinity: profile_affinity(0),
            evidence_snapshot_ids: Vec::new(),
            prompt_text: "review weekly payment incidents".to_string(),
        },
    ];

    let cache = build_cache(&recent, &frequent, &[], 1, 500, &mut TestIds::default());

    assert_eq!(cache.candidates.len(), 10);
    assert!(cache
        .candidates
        .iter()
        .any(|candidate| candidate.task_ref == "distinct-pattern"));
    assert!(!cache
        .candidates
        .iter()
        .any(|candidate| candidate.task_ref == "duplicate-pattern"));

    let frequent_only = (0..10)
        .map(|index| FrequentPattern {
            pattern_id: format!("pattern-{index}"),
            summary: format!("pattern {index}"),
            stable_entities: Vec::new(),
            active_day_buckets: vec![1, 2, 3],
            context_affinity: profile_affinity(0),
            evidence_snapshot_ids: Vec::new(),
            prompt_text: format!("review pattern {index}"),
        })
        .collect::<Vec<_>>();
    let frequent_cache = build_cache(&[], &frequent_only, &[], 1, 500, &mut TestIds::default());
    assert_eq!(frequent_cache.candidates.len(), 10);
}

#[test]
fn declared_business_terms_must_be_grounded_without_rejecting_action_words() {
    for (evidence, summary, prompt, kind, entity) in [
        (
            "inspect payment database",
            "inspect inventory database",
            "continue investigating inventory database",
            "service",
            "inventory",
        ),
        (
            "排查支付服务异常",
            "排查订单服务异常",
            "继续排查订单服务异常",
            "service",
            "订单",
        ),
    ] {
        let source = state(vec![request("trigger", "session-1", evidence, 500)]);
        let built = build_input(&source, "session-1", 500).expect("build input");
        let output = serde_json::json!({
            "discarded_activities": [],
            "recent_tasks": [{
                "prior_task_id": null,
                "summary": summary,
                "entities": [{"kind": kind, "value": entity, "volatility": "stable"}],
                "evidence_activity_ids": ["trigger"],
                "prior_snapshot_ids": [],
                "prompt_text": prompt
            }],
            "frequent_patterns": []
        })
        .to_string();

        assert_eq!(
            prepare_merge_patch(&output, &built, &source, &mut TestIds::default())
                .expect_err("ungrounded business term"),
            AnalyzerError::UngroundedEntity
        );
    }

    let source = state(vec![request(
        "trigger",
        "session-1",
        "排查支付服务异常",
        500,
    )]);
    let built = build_input(&source, "session-1", 500).expect("build input");
    let grounded = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"继续排查支付服务异常",
            "entities":[{"kind":"service","value":"支付","volatility":"stable"}],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"继续检查并处理支付问题"
        }],
        "frequent_patterns":[]
    }"#;
    prepare_merge_patch(grounded, &built, &source, &mut TestIds::default())
        .expect("declared Chinese entity is grounded without parsing action words");

    let generic = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"继续排查问题",
            "entities":[],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"继续检查并处理问题"
        }],
        "frequent_patterns":[]
    }"#;
    prepare_merge_patch(generic, &built, &source, &mut TestIds::default())
        .expect("generic action words do not require invented entities");

    let ordinary_action_words = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"check logs for payment",
            "entities":[{"kind":"service","value":"支付","volatility":"stable"}],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"查看支付服务日志并定位根因"
        }],
        "frequent_patterns":[]
    }"#;
    prepare_merge_patch(
        ordinary_action_words,
        &built,
        &source,
        &mut TestIds::default(),
    )
    .expect("ordinary generated action words do not need verbatim evidence");
}

#[test]
fn cache_prompt_dedup_is_scoped_by_context() {
    let repo_a = ContextAffinity {
        scope_kind: ScopeKind::Repo,
        repo_id: Some("repo-a".to_string()),
        host_id: Some("host-1".to_string()),
    };
    let repo_b = ContextAffinity {
        scope_kind: ScopeKind::Repo,
        repo_id: Some("repo-b".to_string()),
        host_id: Some("host-1".to_string()),
    };
    let recent = (0..7)
        .map(|index| RecentTask {
            task_id: format!("task-{index}"),
            summary: format!("task {index}"),
            entities: Vec::new(),
            context_affinity: repo_a.clone(),
            last_seen_hour_bucket: 400 + index,
            evidence_snapshot_ids: Vec::new(),
            prompt_text: if index == 6 {
                "run tests".to_string()
            } else {
                format!("continue task {index}")
            },
        })
        .collect::<Vec<_>>();
    let frequent = [FrequentPattern {
        pattern_id: "repo-b-tests".to_string(),
        summary: "repo b tests".to_string(),
        stable_entities: Vec::new(),
        active_day_buckets: vec![1, 2, 3],
        context_affinity: repo_b,
        evidence_snapshot_ids: Vec::new(),
        prompt_text: "run tests".to_string(),
    }];

    let cache = build_cache(&recent, &frequent, &[], 1, 500, &mut TestIds::default());

    assert!(cache
        .candidates
        .iter()
        .any(|candidate| candidate.task_ref == "repo-b-tests"));
}

#[test]
fn frequent_pattern_time_scope_and_counts_are_recomputed_locally() {
    let mut source = state(vec![
        request("day-1", "session-1", "inspect payment", 24),
        request("day-2", "session-1", "inspect payment", 48),
        request("trigger", "session-1", "inspect payment", 72),
    ]);
    source.updated_hour_bucket = 0;
    let built = build_input(&source, "session-1", 500).expect("build input");
    let output = r#"{
        "discarded_activities":[],
        "recent_tasks":[],
        "frequent_patterns":[{
            "prior_pattern_id":null,
            "summary":"inspect payment",
            "stable_entities":[{"kind":"repo","value":"payment","volatility":"stable"}],
            "evidence_activity_ids":["day-1","day-2","trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue inspecting payment"
        }]
    }"#;
    let patch = prepare_merge_patch(output, &built, &source, &mut TestIds::default())
        .expect("prepare patch");

    apply_merge_patch(&mut source, patch).expect("apply patch");

    let pattern = &source.profile.frequent_patterns[0];
    let snapshot = &source.profile.evidence_snapshots[0];
    assert_eq!(pattern.active_day_buckets, [1, 2, 3]);
    assert_eq!(pattern.context_affinity.scope_kind, ScopeKind::Repo);
    assert_eq!(snapshot.agent_request_count, 3);
    assert_eq!(snapshot.last_seen_hour_bucket, 72);
    assert_eq!(pattern.stable_entities[0].kind, EntityKind::Repo);
    assert_eq!(
        pattern.stable_entities[0].volatility,
        EntityVolatility::Stable
    );
}

#[test]
fn merge_preserves_concurrently_appended_activity() {
    let mut current = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment",
        500,
    )]);
    let built = build_input(&current, "session-1", 500).expect("build input");
    let output = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"inspect payment",
            "entities":[{"kind":"repo","value":"payment","volatility":"stable"}],
            "evidence_activity_ids":["trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue inspecting payment"
        }],
        "frequent_patterns":[]
    }"#;
    let patch = prepare_merge_patch(output, &built, &current, &mut TestIds::default())
        .expect("prepare patch");
    current
        .journal
        .records
        .push(request("concurrent", "session-1", "new work", 501));
    current.generation += 1;

    apply_merge_patch(&mut current, patch).expect("apply patch");

    assert!(
        current
            .journal
            .records
            .iter()
            .any(|record| record.activity_id == "concurrent"
                && record.summarized_generation.is_none())
    );
    assert_eq!(current.profile.recent_tasks.len(), 1);
    assert_eq!(
        current.cache.candidates[0].source,
        CandidateSource::RecentTask
    );
}

#[test]
fn stale_profile_rejection_is_atomic() {
    let mut current = state(vec![request(
        "trigger",
        "session-1",
        "inspect payment",
        500,
    )]);
    let built = build_input(&current, "session-1", 500).expect("build input");
    let output = r#"{
        "discarded_activities":[{"activity_id":"trigger","reason":"no_recommendation_value"}],
        "recent_tasks":[],
        "frequent_patterns":[]
    }"#;
    let patch = prepare_merge_patch(output, &built, &current, &mut TestIds::default())
        .expect("prepare patch");
    current.profile.summary_generation += 1;
    let before = current.clone();

    assert_eq!(
        apply_merge_patch(&mut current, patch),
        Err(AnalyzerError::StaleProfile)
    );
    assert_eq!(current, before);
}

#[test]
fn unlinked_agent_run_cannot_establish_profile_item() {
    let source = state(vec![
        request("trigger", "session-1", "inspect payment", 48),
        agent_run("run-only", "missing-request", 72),
    ]);
    let built = build_input(&source, "session-1", 500).expect("build input");
    let output = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"invented from run metadata",
            "entities":[],
            "evidence_activity_ids":["run-only"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue invented task"
        }],
        "frequent_patterns":[]
    }"#;

    assert!(prepare_merge_patch(output, &built, &source, &mut TestIds::default()).is_err());
}

#[test]
fn linked_agent_run_cannot_refresh_beyond_its_request() {
    let source = state(vec![
        request("trigger", "session-1", "inspect payment", 48),
        agent_run("linked-run", "trigger", 72),
    ]);
    let built = build_input(&source, "session-1", 500).expect("build input");
    let output = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"inspect payment",
            "entities":[{"kind":"repo","value":"payment","volatility":"stable"}],
            "evidence_activity_ids":["trigger","linked-run"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue inspection"
        }],
        "frequent_patterns":[]
    }"#;
    let patch = prepare_merge_patch(output, &built, &source, &mut TestIds::default())
        .expect("linked run may enrich its request");
    let mut next = source;

    apply_merge_patch(&mut next, patch).expect("apply linked run patch");

    assert_eq!(next.profile.recent_tasks[0].last_seen_hour_bucket, 48);
    assert_eq!(next.profile.evidence_snapshots[0].agent_request_count, 1);
    assert!(!next.profile.evidence_snapshots[0].last_action_failed);
}

#[test]
fn frequent_pattern_rejects_ephemeral_entities() {
    let source = state(vec![
        request("day-1", "session-1", "inspect payment", 24),
        request("day-2", "session-1", "inspect payment", 48),
        request("trigger", "session-1", "inspect payment", 72),
    ]);
    let built = build_input(&source, "session-1", 500).expect("build input");
    let output = r#"{
        "discarded_activities":[],
        "recent_tasks":[],
        "frequent_patterns":[{
            "prior_pattern_id":null,
            "summary":"inspect payment",
            "stable_entities":[{"kind":"repo","value":"payment","volatility":"ephemeral"}],
            "evidence_activity_ids":["day-1","day-2","trigger"],
            "prior_snapshot_ids":[],
            "prompt_text":"inspect payment again"
        }]
    }"#;

    assert!(matches!(
        prepare_merge_patch(output, &built, &source, &mut TestIds::default()),
        Err(AnalyzerError::InvalidFrequentEvidence)
    ));
}

#[test]
fn record_metadata_cannot_ground_an_entity() {
    let source = state(vec![request(
        "metadata-only-value",
        "session-1",
        "inspect payment",
        48,
    )]);
    let built = build_input(&source, "session-1", 500).expect("build input");
    let output = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"metadata injection",
            "entities":[{"kind":"repo","value":"metadata-only-value","volatility":"stable"}],
            "evidence_activity_ids":["metadata-only-value"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue metadata injection"
        }],
        "frequent_patterns":[]
    }"#;

    assert!(matches!(
        prepare_merge_patch(output, &built, &source, &mut TestIds::default()),
        Err(AnalyzerError::UngroundedEntity)
    ));
}

#[test]
fn future_and_unverified_times_do_not_refresh_or_complete_patterns() {
    let mut source = state(vec![
        request("day-1", "session-1", "inspect payment", 24),
        request("trigger", "session-1", "inspect payment", 48),
        request("future", "session-1", "inspect payment", 240),
        unverified_history("history", 72),
    ]);
    source.updated_hour_bucket = 100;
    let built = build_input(&source, "session-1", 100).expect("build input");
    let recent = r#"{
        "discarded_activities":[],
        "recent_tasks":[{
            "prior_task_id":null,
            "summary":"inspect payment",
            "entities":[{"kind":"repo","value":"payment","volatility":"stable"}],
            "evidence_activity_ids":["trigger","future"],
            "prior_snapshot_ids":[],
            "prompt_text":"continue inspection"
        }],
        "frequent_patterns":[]
    }"#;
    let patch = prepare_merge_patch(recent, &built, &source, &mut TestIds::default())
        .expect("prepare bounded recency patch");
    let mut recent_state = source.clone();
    apply_merge_patch(&mut recent_state, patch).expect("apply bounded recency patch");
    assert_eq!(
        recent_state.profile.recent_tasks[0].last_seen_hour_bucket,
        48
    );
    assert_eq!(
        recent_state.profile.evidence_snapshots[0].intent_occurrence_count,
        1
    );

    let frequent = r#"{
        "discarded_activities":[],
        "recent_tasks":[],
        "frequent_patterns":[{
            "prior_pattern_id":null,
            "summary":"inspect payment",
            "stable_entities":[{"kind":"repo","value":"payment","volatility":"stable"}],
            "evidence_activity_ids":["day-1","trigger","future","history"],
            "prior_snapshot_ids":[],
            "prompt_text":"inspect payment again"
        }]
    }"#;
    assert!(matches!(
        prepare_merge_patch(frequent, &built, &source, &mut TestIds::default()),
        Err(AnalyzerError::InvalidFrequentEvidence)
    ));
}

fn agent_run(id: &str, request_activity_id: &str, hour: u64) -> ActivityRecord {
    ActivityRecord {
        activity_id: id.to_string(),
        session_scope_id: Some("session-1".to_string()),
        source_fingerprint: format!("fp-{id}"),
        observed_hour_bucket: hour,
        source: ActivitySource::AgentRun,
        context: ActivityContext::default(),
        payload: ActivityPayload::AgentRun {
            request_activity_id: request_activity_id.to_string(),
            tool_categories: vec![ToolCategory::Shell],
            outcome: ActivityOutcome::Failure,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    }
}

fn unverified_history(id: &str, execution_hour_bucket: u64) -> ActivityRecord {
    ActivityRecord {
        activity_id: id.to_string(),
        session_scope_id: None,
        source_fingerprint: format!("fp-{id}"),
        observed_hour_bucket: execution_hour_bucket,
        source: ActivitySource::BashHistory,
        context: ActivityContext::default(),
        payload: ActivityPayload::BashHistoryCommand {
            command: "inspect payment".to_string(),
            origin_unverified: true,
            execution_hour_bucket: Some(execution_hour_bucket),
            time_unverified: true,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    }
}

#[derive(Default)]
struct TestIds(u64);

impl LocalIdSource for TestIds {
    fn next_id(&mut self, prefix: &str) -> String {
        self.0 += 1;
        format!("{prefix}-{}", self.0)
    }
}
