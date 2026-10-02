//! Behaviour tests for [`ReuseStore`], focused on the two promises the store
//! makes: a human decision survives everything, and an unconfirmed automatic
//! label is still usable.

use std::path::PathBuf;

use super::*;
use crate::reuse::JudgeVerdict;
use crate::reuse::label::{ConfirmState, LabelAction, TrajectoryIdentity, TrajectoryLabel};
use crate::reuse::triage::{TriageMetrics, TriageOutcome};

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("reuse-store-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A store over a plain connection, bypassing the private-directory handling so
/// the tests exercise schema and label behaviour rather than file modes.
fn store_at(path: &std::path::Path) -> ReuseStore {
    ReuseStore::from_connection(Connection::open(path).unwrap()).unwrap()
}

fn store(tag: &str) -> ReuseStore {
    store_at(&tmp_dir(tag).join("reuse.db"))
}

fn identity() -> TrajectoryIdentity {
    TrajectoryIdentity {
        title: Some("看一下版本".to_string()),
        project: "-root-demo".to_string(),
        source: "qoder".to_string(),
        agent_name: "qoder".to_string(),
        started_at: None,
        is_subagent: false,
    }
}

fn outcome(label: TrajectoryLabel, rules: &[&str]) -> TriageOutcome {
    TriageOutcome {
        label,
        reason: format!("auto:{}", label.as_str()),
        metrics: TriageMetrics {
            n_steps: 4,
            n_user_turns: 1,
            n_tool_calls: 2,
            max_agent_len: 321,
        },
        n_findings: rules.len(),
        rules: rules.iter().map(|r| r.to_string()).collect(),
    }
}

fn insert_event_at(store: &ReuseStore, session_id: &str, created_at_ns: i64, reason: &str) {
    let conn = store.lock().unwrap();
    conn.execute(
        "INSERT INTO session_label_events (
            session_id, from_label, to_label, action, reason, decided_by, created_at_ns
         ) VALUES (?1, 'unknown', 'good', 'auto_retriage', ?2, 'system', ?3)",
        params![session_id, reason, created_at_ns],
    )
    .unwrap();
}

fn insert_large_auto_label(store: &ReuseStore, session_id: &str, updated_at_ns: i64) {
    store
        .upsert_auto_label(
            session_id,
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "hash",
            "v1",
        )
        .unwrap();
    let conn = store.lock().unwrap();
    conn.execute(
        "UPDATE session_labels
         SET auto_reason = ?2, updated_at_ns = ?3
         WHERE session_id = ?1",
        params![session_id, "x".repeat(24 * 1024), updated_at_ns],
    )
    .unwrap();
}

#[test]
fn schema_is_created_and_reopening_is_a_no_op() {
    let path = tmp_dir("reopen").join("reuse.db");
    {
        let store = store_at(&path);
        store
            .upsert_auto_label(
                "s1",
                identity(),
                outcome(TrajectoryLabel::Good, &[]),
                "h1",
                "v1",
            )
            .unwrap();
    }
    let reopened = store_at(&path);
    assert!(reopened.get_label("s1").unwrap().is_some());
}

#[test]
fn opening_privately_creates_an_owner_only_database() {
    use std::os::unix::fs::PermissionsExt;

    // The label reasons quote user text, so the file must not be world-readable.
    // Permission repair itself is covered by `private_sqlite`; this only proves
    // the store actually goes through it.
    let state_dir = tmp_dir("private").join(".agentsight-private");
    let store = ReuseStore::open_private(&state_dir).unwrap();
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h1",
            "v1",
        )
        .unwrap();

    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&state_dir), 0o700);
    assert_eq!(mode(&state_dir.join("reuse.db")), 0o600);
}

#[test]
fn a_future_schema_is_refused_rather_than_guessed() {
    let path = tmp_dir("future").join("reuse.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
    }
    let err = ReuseStore::from_connection(Connection::open(&path).unwrap()).unwrap_err();
    assert!(matches!(err, ReuseStoreError::SchemaTooNew { .. }));
}

#[test]
fn round_trip_preserves_metrics_and_rules() {
    let store = store("roundtrip");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Bad, &["failure_then_fabrication", "R1"]),
            "h1",
            "v1",
        )
        .unwrap();
    let got = store.get_label("s1").unwrap().unwrap();
    assert_eq!(got.auto_label, TrajectoryLabel::Bad);
    assert_eq!(got.metrics.n_tool_calls, 2);
    assert_eq!(got.metrics.max_agent_len, 321);
    assert_eq!(got.n_findings, 2);
    assert_eq!(got.auto_rules, vec!["failure_then_fabrication", "R1"]);
    assert_eq!(got.confirm_state, ConfirmState::Unconfirmed);
}

#[test]
fn an_unconfirmed_label_is_returned_as_effective() {
    let store = store("unconfirmed");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Bad, &[]),
            "h1",
            "v1",
        )
        .unwrap();
    let got = store.get_label("s1").unwrap().unwrap();
    assert_eq!(got.effective_label(), TrajectoryLabel::Bad);
    assert!(!got.is_human_backed());
}

#[test]
fn a_decision_survives_a_later_retriage() {
    let store = store("survives");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Bad, &["R1"]),
            "h1",
            "v1",
        )
        .unwrap();
    store
        .apply_decision(
            "s1",
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            Some("报错是预期的".to_string()),
        )
        .unwrap();

    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Useless, &["R1"]),
            "h2",
            "v2",
        )
        .unwrap();

    let got = store.get_label("s1").unwrap().unwrap();
    assert_eq!(got.effective_label(), TrajectoryLabel::Good);
    assert_eq!(got.human_reason.as_deref(), Some("报错是预期的"));
    // The automatic verdict still moved, and is flagged for a second look.
    assert_eq!(got.auto_label, TrajectoryLabel::Useless);
    assert!(got.auto_changed_since_decision);
}

#[test]
fn deciding_on_an_untriaged_session_is_reported_not_invented() {
    let store = store("untriaged");
    let err = store
        .apply_decision("missing", LabelAction::Confirm, "alice", None)
        .unwrap_err();
    assert!(matches!(err, ReuseStoreError::UnknownSession(_)));
    assert!(store.get_label("missing").unwrap().is_none());
}

#[test]
fn events_record_the_whole_history() {
    let store = store("events");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h1",
            "v1",
        )
        .unwrap();
    // An unchanged recompute is not worth an audit row.
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h1",
            "v1",
        )
        .unwrap();
    assert!(store.events("s1").unwrap().is_empty());

    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Bad, &[]),
            "h2",
            "v1",
        )
        .unwrap();
    store
        .apply_decision(
            "s1",
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            None,
        )
        .unwrap();

    let events = store.events("s1").unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].action, "auto_retriage");
    assert_eq!(events[0].decided_by, "system");
    assert_eq!(events[1].action, "override");
    assert_eq!(events[1].decided_by, "alice");
    assert_eq!(events[1].to_label, "good");
}

#[test]
fn listing_filters_on_the_effective_label_not_the_automatic_one() {
    let store = store("filter");
    store
        .upsert_auto_label(
            "bad-kept",
            identity(),
            outcome(TrajectoryLabel::Bad, &[]),
            "h",
            "v1",
        )
        .unwrap();
    store
        .upsert_auto_label(
            "bad-fixed",
            identity(),
            outcome(TrajectoryLabel::Bad, &[]),
            "h",
            "v1",
        )
        .unwrap();
    store
        .apply_decision(
            "bad-fixed",
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            None,
        )
        .unwrap();

    let bad = store
        .list_labels(&LabelFilter {
            effective_labels: vec![TrajectoryLabel::Bad],
            ..LabelFilter::default()
        })
        .unwrap();
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].session_id, "bad-kept");

    let good = store
        .list_labels(&LabelFilter {
            effective_labels: vec![TrajectoryLabel::Good],
            ..LabelFilter::default()
        })
        .unwrap();
    assert_eq!(good.len(), 1);
    assert_eq!(good[0].session_id, "bad-fixed");
}

#[test]
fn only_useless_sessions_leave_the_retrieval_scope() {
    let store = store("excluded");
    store
        .upsert_auto_label(
            "u",
            identity(),
            outcome(TrajectoryLabel::Useless, &[]),
            "h",
            "v1",
        )
        .unwrap();
    store
        .upsert_auto_label(
            "b",
            identity(),
            outcome(TrajectoryLabel::Bad, &[]),
            "h",
            "v1",
        )
        .unwrap();
    store
        .upsert_auto_label(
            "g",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v1",
        )
        .unwrap();
    assert_eq!(store.excluded_sessions().unwrap(), vec!["u".to_string()]);
}

#[test]
fn marking_useless_by_hand_takes_effect_immediately() {
    let store = store("manual-useless");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v1",
        )
        .unwrap();
    assert!(store.excluded_sessions().unwrap().is_empty());
    store
        .apply_decision(
            "s1",
            LabelAction::Override(TrajectoryLabel::Useless),
            "alice",
            None,
        )
        .unwrap();
    assert_eq!(store.excluded_sessions().unwrap(), vec!["s1".to_string()]);
}

#[test]
fn batch_confirm_skips_unknown_ids_instead_of_failing() {
    let store = store("batch");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v1",
        )
        .unwrap();
    store
        .upsert_auto_label(
            "s2",
            identity(),
            outcome(TrajectoryLabel::Bad, &[]),
            "h",
            "v1",
        )
        .unwrap();

    let confirmed = store
        .confirm_batch(
            &["s1".to_string(), "gone".to_string(), "s2".to_string()],
            "alice",
        )
        .unwrap();
    assert_eq!(confirmed, vec!["s1".to_string(), "s2".to_string()]);
    for id in ["s1", "s2"] {
        let got = store.get_label(id).unwrap().unwrap();
        assert_eq!(got.confirm_state, ConfirmState::Confirmed);
        assert!(got.is_human_backed());
    }
}

#[test]
fn confirming_pins_the_label_against_a_later_auto_change() {
    let store = store("pin");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Useless, &[]),
            "h1",
            "v1",
        )
        .unwrap();
    store.confirm_batch(&["s1".to_string()], "alice").unwrap();
    // The trajectory grew a real follow-up, so the rules change their mind.
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h2",
            "v1",
        )
        .unwrap();

    let got = store.get_label("s1").unwrap().unwrap();
    assert_eq!(got.effective_label(), TrajectoryLabel::Useless);
    assert!(got.auto_changed_since_decision);
    // Still excluded, because that is what the human confirmed — surfacing the
    // disagreement is the remedy, not silently re-including it.
    assert_eq!(store.excluded_sessions().unwrap(), vec!["s1".to_string()]);
}

#[test]
fn pending_review_rows_can_be_listed() {
    let store = store("pending");
    store
        .upsert_auto_label(
            "done",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h1",
            "v1",
        )
        .unwrap();
    store.confirm_batch(&["done".to_string()], "alice").unwrap();
    store
        .upsert_auto_label(
            "done",
            identity(),
            outcome(TrajectoryLabel::Bad, &[]),
            "h2",
            "v1",
        )
        .unwrap();
    store
        .upsert_auto_label(
            "fresh",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h1",
            "v1",
        )
        .unwrap();

    let unconfirmed = store
        .list_labels(&LabelFilter {
            confirm_state: Some(ConfirmState::Unconfirmed),
            ..LabelFilter::default()
        })
        .unwrap();
    assert_eq!(unconfirmed.len(), 1);
    assert_eq!(unconfirmed[0].session_id, "fresh");

    let drifted = store
        .list_labels(&LabelFilter {
            only_changed_since_decision: true,
            ..LabelFilter::default()
        })
        .unwrap();
    assert_eq!(drifted.len(), 1);
    assert_eq!(drifted[0].session_id, "done");
}

#[test]
fn rule_stats_separate_overrides_from_confirmations() {
    let store = store("stats");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Bad, &["R1", "shared"]),
            "h",
            "v1",
        )
        .unwrap();
    store
        .upsert_auto_label(
            "s2",
            identity(),
            outcome(TrajectoryLabel::Bad, &["R2", "shared"]),
            "h",
            "v1",
        )
        .unwrap();
    store
        .upsert_auto_label(
            "s3",
            identity(),
            outcome(TrajectoryLabel::Bad, &["R1"]),
            "h",
            "v1",
        )
        .unwrap();

    store
        .apply_decision(
            "s1",
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            None,
        )
        .unwrap();
    store
        .apply_decision(
            "s3",
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            None,
        )
        .unwrap();
    store
        .apply_decision("s2", LabelAction::Confirm, "alice", None)
        .unwrap();

    let stats = store.rule_override_stats().unwrap();
    let by_rule = |rule: &str| stats.iter().find(|s| s.rule == rule).unwrap().clone();
    // Most-overturned first, so the worst rule is what a reader sees.
    assert_eq!(stats[0].rule, "R1");
    assert_eq!(by_rule("R1").overridden, 2);
    assert_eq!(by_rule("R1").confirmed, 0);
    assert_eq!(by_rule("R2").overridden, 0);
    assert_eq!(by_rule("R2").confirmed, 1);
    assert_eq!(by_rule("shared").overridden, 1);
    assert_eq!(by_rule("shared").confirmed, 1);
}

#[test]
fn unconfirmed_rows_contribute_nothing_to_rule_stats() {
    let store = store("stats-unconfirmed");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Bad, &["R1"]),
            "h",
            "v1",
        )
        .unwrap();
    assert!(store.rule_override_stats().unwrap().is_empty());
}

#[test]
fn a_corrupt_label_token_is_reported_not_silently_defaulted() {
    let store = store("corrupt");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v1",
        )
        .unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE session_labels SET auto_label = 'nonsense' WHERE session_id = 's1'",
            [],
        )
        .unwrap();
    }
    let err = store.get_label("s1").unwrap_err();
    assert!(matches!(err, ReuseStoreError::Corrupt { .. }));
}

// ─── The model judge's lane ──────────────────────────────────────────────────

fn verdict(label: TrajectoryLabel, cited: Vec<usize>, downgraded: bool) -> JudgeVerdict {
    JudgeVerdict {
        label,
        reason: "模型理由".to_string(),
        cited_steps: cited,
        downgraded,
    }
}

#[test]
fn a_judgement_outranks_the_rules_without_erasing_them() {
    // Both verdicts have to survive: comparing them is how rule misfires get
    // measured, and overwriting the rules' own would take that with it.
    let store = store("judge-over-rules");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Unknown, &["r1"]),
            "h1",
            "v1",
        )
        .unwrap();

    let label = store
        .record_judgement("s1", &verdict(TrajectoryLabel::Bad, vec![4], false))
        .unwrap();
    assert_eq!(label.effective_label(), TrajectoryLabel::Bad);
    assert_eq!(
        label.auto_label,
        TrajectoryLabel::Unknown,
        "rules preserved"
    );
    assert_eq!(label.llm_cited_steps, vec![4]);
}

#[test]
fn a_person_outranks_the_model() {
    let store = store("human-over-judge");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Unknown, &["r1"]),
            "h1",
            "v1",
        )
        .unwrap();
    store
        .record_judgement("s1", &verdict(TrajectoryLabel::Bad, vec![4], false))
        .unwrap();

    let label = store
        .apply_decision(
            "s1",
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            None,
        )
        .unwrap();
    assert_eq!(label.effective_label(), TrajectoryLabel::Good);
    assert_eq!(
        label.llm_label,
        Some(TrajectoryLabel::Bad),
        "model preserved"
    );
}

#[test]
fn a_judgement_may_not_overturn_a_person() {
    let store = store("judge-under-human");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h1",
            "v1",
        )
        .unwrap();
    store
        .apply_decision("s1", LabelAction::Confirm, "alice", None)
        .unwrap();

    let label = store
        .record_judgement("s1", &verdict(TrajectoryLabel::Bad, vec![9], false))
        .unwrap();
    assert_eq!(
        label.effective_label(),
        TrajectoryLabel::Good,
        "the human decision stands"
    );
    assert_eq!(
        label.llm_label,
        Some(TrajectoryLabel::Bad),
        "still recorded"
    );
}

#[test]
fn a_downgraded_judgement_records_that_it_was_downgraded() {
    let store = store("judge-downgraded");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h1",
            "v1",
        )
        .unwrap();
    let label = store
        .record_judgement("s1", &verdict(TrajectoryLabel::Unknown, vec![], true))
        .unwrap();
    assert!(label.llm_downgraded);
    assert!(label.llm_cited_steps.is_empty());
}

#[test]
fn a_judgement_survives_reopening_the_database() {
    let path = tmp_dir("judge-reopen").join("reuse.db");
    {
        let store = store_at(&path);
        store
            .upsert_auto_label(
                "s1",
                identity(),
                outcome(TrajectoryLabel::Unknown, &["r1"]),
                "h1",
                "v1",
            )
            .unwrap();
        store
            .record_judgement("s1", &verdict(TrajectoryLabel::Bad, vec![2, 3], false))
            .unwrap();
    }
    let store = store_at(&path);
    let label = store.get_label("s1").unwrap().unwrap();
    assert_eq!(label.llm_label, Some(TrajectoryLabel::Bad));
    assert_eq!(label.llm_cited_steps, vec![2, 3]);
    assert_eq!(label.effective_label(), TrajectoryLabel::Bad);
}

#[test]
fn judging_an_untriaged_trajectory_is_refused() {
    let store = store("judge-missing");
    let error = store
        .record_judgement("ghost", &verdict(TrajectoryLabel::Bad, vec![1], false))
        .unwrap_err();
    assert!(matches!(error, ReuseStoreError::UnknownSession(_)));
}

#[test]
fn a_judgement_is_audited() {
    let store = store("judge-audit");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Unknown, &["r1"]),
            "h1",
            "v1",
        )
        .unwrap();
    store
        .record_judgement("s1", &verdict(TrajectoryLabel::Bad, vec![1], false))
        .unwrap();
    let events = store.events("s1").unwrap();
    let judged = events
        .iter()
        .find(|e| e.action == LabelEventKind::LlmJudge.as_str())
        .expect("the judgement must leave a trail");
    assert_eq!(judged.to_label, "bad");
    assert_eq!(judged.decided_by, "model");
}

#[test]
fn a_judged_bad_leaves_retrieval_alone_but_a_judged_useless_would_not() {
    // `excluded_sessions` reads the effective label, so a model verdict has to
    // reach it. `bad` stays retrievable — it is the counterexample source.
    let store = store("judge-retrieval");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Unknown, &["r1"]),
            "h1",
            "v1",
        )
        .unwrap();
    store
        .record_judgement("s1", &verdict(TrajectoryLabel::Bad, vec![1], false))
        .unwrap();
    assert!(store.excluded_sessions().unwrap().is_empty());
}

// ─── Display identity ────────────────────────────────────────────────────────

#[test]
fn identity_round_trips() {
    let store = store("identity-roundtrip");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v",
        )
        .unwrap();
    let label = store.get_label("s1").unwrap().unwrap();
    assert_eq!(label.identity.title.as_deref(), Some("看一下版本"));
    assert_eq!(label.identity.source, "qoder");
    assert!(!label.identity.is_subagent);
}

#[test]
fn set_identity_backfills_without_touching_the_verdict() {
    // The fast path uses this for rows labelled before the identity columns
    // existed: the verdict and the human decision must be left exactly as they
    // were, only the display fields filled in.
    let store = store("identity-backfill");
    store
        .upsert_auto_label(
            "s1",
            TrajectoryIdentity::default(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v",
        )
        .unwrap();
    store
        .apply_decision("s1", LabelAction::Confirm, "alice", None)
        .unwrap();

    store.set_identity("s1", &identity()).unwrap();

    let label = store.get_label("s1").unwrap().unwrap();
    assert_eq!(label.identity.title.as_deref(), Some("看一下版本"));
    assert_eq!(
        label.confirm_state,
        ConfirmState::Confirmed,
        "decision intact"
    );
    assert_eq!(label.effective_label(), TrajectoryLabel::Good);
}

#[test]
fn set_identity_on_a_missing_row_is_refused() {
    let store = store("identity-missing");
    let error = store.set_identity("ghost", &identity()).unwrap_err();
    assert!(matches!(error, ReuseStoreError::UnknownSession(_)));
}

#[test]
fn a_blank_title_is_stored_as_absent() {
    assert_eq!(TrajectoryIdentity::title_from_message(Some("   ")), None);
    assert_eq!(TrajectoryIdentity::title_from_message(None), None);
    assert_eq!(
        TrajectoryIdentity::title_from_message(Some("  hello  ")).as_deref(),
        Some("hello")
    );
}

#[test]
fn sessions_with_labels_mirrors_the_rust_precedence() {
    // The trajectory filter's SQL derives the effective label; it must agree
    // with `SessionLabel::effective_label` (human > model > rules). A row
    // where the two disagree would silently vanish from or leak into the
    // label-filtered history an agent reads.
    let store = store("label-precedence");
    // Rules said unknown, model said good, human said bad -> effective bad.
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Unknown, &[]),
            "h1",
            "v1",
        )
        .unwrap();
    store
        .record_judgement("s1", &verdict(TrajectoryLabel::Good, vec![2], false))
        .unwrap();
    store
        .apply_decision(
            "s1",
            LabelAction::Override(TrajectoryLabel::Bad),
            "alice",
            None,
        )
        .unwrap();

    // Rules said useless, nobody else spoke -> effective useless.
    store
        .upsert_auto_label(
            "s2",
            identity(),
            outcome(TrajectoryLabel::Useless, &[]),
            "h2",
            "v1",
        )
        .unwrap();

    // Rules said good, model said bad (no human) -> effective bad.
    store
        .upsert_auto_label(
            "s3",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h3",
            "v1",
        )
        .unwrap();
    store
        .record_judgement("s3", &verdict(TrajectoryLabel::Bad, vec![2], false))
        .unwrap();

    let bad = store.sessions_with_labels(&[TrajectoryLabel::Bad]).unwrap();
    assert_eq!(bad, vec!["s1".to_string(), "s3".to_string()]);
    let useless = store
        .sessions_with_labels(&[TrajectoryLabel::Useless])
        .unwrap();
    assert_eq!(useless, vec!["s2".to_string()]);
    let good = store
        .sessions_with_labels(&[TrajectoryLabel::Good])
        .unwrap();
    assert!(good.is_empty(), "nothing effective-good remains: {good:?}");
}

#[test]
fn sessions_with_labels_empty_input_is_empty() {
    let store = store("label-empty");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v",
        )
        .unwrap();
    assert!(store.sessions_with_labels(&[]).unwrap().is_empty());
}

// ─── Retention and capacity maintenance ──────────────────────────────────────

#[test]
fn maintenance_deletes_only_expired_events_by_age() {
    let store = store("maintenance-retention");
    store
        .upsert_auto_label(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v",
        )
        .unwrap();
    insert_event_at(&store, "s1", 1, "expired");
    insert_event_at(&store, "s1", now_ns(), "current");

    let report = store.maintain(1, 0).unwrap();

    assert_eq!(report.expired_events, 1);
    assert_eq!(report.size.status, MaintenanceStatus::Disabled);
    let events = store.events("s1").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].reason.as_deref(), Some("current"));
    assert!(store.get_label("s1").unwrap().is_some());
}

#[test]
fn size_maintenance_deletes_events_before_labels() {
    let store = store("maintenance-event-size");
    store
        .upsert_auto_label(
            "label-must-remain",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v",
        )
        .unwrap();
    let payload = "x".repeat(24 * 1024);
    for index in 0..96 {
        insert_event_at(&store, "label-must-remain", index, &payload);
    }
    assert!(store.size_snapshot().unwrap().physical_bytes > 1024 * 1024);

    let report = store.maintain(0, 1).unwrap();

    assert!(report.size.deleted_rows > 0);
    assert_eq!(report.size.status, MaintenanceStatus::TargetReached);
    assert!(store.events("label-must-remain").unwrap().len() < 96);
    assert!(store.get_label("label-must-remain").unwrap().is_some());
}

#[test]
fn size_maintenance_removes_oldest_automatic_labels_but_protects_decisions() {
    let store = store("maintenance-label-size");
    for index in 0..90 {
        insert_large_auto_label(&store, &format!("auto-{index:03}"), index);
    }
    insert_large_auto_label(&store, "confirmed", 91);
    store
        .apply_decision("confirmed", LabelAction::Confirm, "alice", None)
        .unwrap();
    insert_large_auto_label(&store, "overridden", 92);
    store
        .apply_decision(
            "overridden",
            LabelAction::Override(TrajectoryLabel::Bad),
            "bob",
            Some("reviewed".to_string()),
        )
        .unwrap();
    assert!(store.size_snapshot().unwrap().physical_bytes > 1024 * 1024);

    let report = store.maintain(0, 1).unwrap();

    assert!(
        report.size.deleted_rows > 2,
        "labels should be capacity-pruned"
    );
    assert!(store.get_label("auto-000").unwrap().is_none());
    assert!(store.get_label("auto-089").unwrap().is_some());
    let confirmed = store.get_label("confirmed").unwrap().unwrap();
    assert_eq!(confirmed.confirm_state, ConfirmState::Confirmed);
    assert_eq!(confirmed.decided_by.as_deref(), Some("alice"));
    let overridden = store.get_label("overridden").unwrap().unwrap();
    assert_eq!(overridden.confirm_state, ConfirmState::Overridden);
    assert_eq!(overridden.human_label, Some(TrajectoryLabel::Bad));
    assert_eq!(overridden.decided_by.as_deref(), Some("bob"));
}

#[test]
fn retention_checkpoint_busy_stops_before_size_pruning() {
    let state_dir = tmp_dir("maintenance-busy").join("private");
    let path = state_dir.join(ReuseStore::DB_NAME);
    let store = ReuseStore::open_private(&state_dir).unwrap();
    store
        .upsert_auto_label(
            "retained",
            identity(),
            outcome(TrajectoryLabel::Good, &[]),
            "h",
            "v",
        )
        .unwrap();
    let payload = "x".repeat(24 * 1024);
    for index in 0..64 {
        insert_event_at(&store, "retained", index, &payload);
    }
    assert_eq!(store.checkpoint().unwrap(), CheckpointOutcome::Completed);

    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT COUNT(*) FROM session_label_events;")
        .unwrap();

    let report = store.maintain(1, 1).unwrap();

    assert_eq!(report.expired_events, 64);
    assert_eq!(report.size.status, MaintenanceStatus::CheckpointBusy);
    assert_eq!(report.size.rounds, 0);
    assert_eq!(report.size.deleted_rows, 0);
    assert!(store.get_label("retained").unwrap().is_some());
}
