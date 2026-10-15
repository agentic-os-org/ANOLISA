//! Literal and Codex-alias ownership for captured session child lookup.

use super::tests::{sample_record, tmp_db};
use super::*;

const UUID: &str = "00000000-0000-0000-0000-000000000001";

fn child(store: &TrajectoryStore, parent: &str, source: &str, stem: &str) -> String {
    let mut record = sample_record();
    record.session_id = format!("{parent}:subagent:{stem}");
    record.source = source.into();
    record.is_subagent = true;
    record.atif_json = format!("{{\"session_id\":\"{}\"}}", record.session_id);
    store.upsert_trajectory(&record).unwrap();
    record.atif_json
}

#[test]
fn captured_children_resolve_only_literal_codex_rollout_parents() {
    let store = TrajectoryStore::new_with_path(&tmp_db("captured-child-alias")).unwrap();
    child(&store, &format!("opt:{UUID}"), "codex", "opt");
    child(
        &store,
        &format!("rollout-other-{UUID}"),
        "qoder",
        "other-source",
    );
    assert!(store
        .get_captured_session_subagents(UUID)
        .unwrap()
        .is_empty());
    let rollout = format!("rollout-2026-10-15T00-00-00-{UUID}");
    let expected = child(&store, &rollout, "codex", "direct");
    child(
        &store,
        &format!("{rollout}:subagent:direct"),
        "codex",
        "nested",
    );
    assert_eq!(
        store.get_captured_session_subagents(UUID).unwrap(),
        vec![expected]
    );
    // Exact public lookup remains literal; the new fallback is capture-only.
    assert!(store.get_subagent_atif_jsons(UUID).unwrap().is_empty());
    assert!(store
        .get_captured_session_subagents("%_")
        .unwrap()
        .is_empty());
}

#[test]
fn captured_children_prefer_exact_ids_and_refuse_ambiguous_aliases() {
    let store = TrajectoryStore::new_with_path(&tmp_db("captured-child-precedence")).unwrap();
    child(&store, &format!("rollout-first-{UUID}"), "codex", "first");
    child(&store, &format!("rollout-second-{UUID}"), "codex", "second");
    assert!(store
        .get_captured_session_subagents(UUID)
        .unwrap()
        .is_empty());
    let exact = child(&store, UUID, "qoder", "exact");
    assert_eq!(
        store.get_captured_session_subagents(UUID).unwrap(),
        vec![exact]
    );
}
