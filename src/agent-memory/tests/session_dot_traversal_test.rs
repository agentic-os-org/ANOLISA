// A "." session id used to anchor the session tree on the shared base
// itself, so end(Discard) recursively deleted every sibling session
// (see the paired issue for the original repro). This integration test
// pins the property that was actually lost — sibling sessions survive —
// through both refusal layers, including an id smuggled past the
// validator via the derived Deserialize.
use agent_memory::session::{EndAction, SessionBase, SessionId, SessionLogService};
use tempfile::tempdir;

#[test]
fn dot_session_id_cannot_anchor_on_the_base_and_discard_siblings() {
    let tmp = tempdir().unwrap();
    let base_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&base_dir).unwrap();
    for sid in ["ses_a", "ses_b"] {
        SessionLogService::start(
            &base_dir,
            SessionId::from_string(sid).unwrap(),
            "alice",
            Some("agent"),
            "user-alice",
            None,
        )
        .unwrap()
        .end(EndAction::Keep)
        .unwrap();
    }

    // Layer 1: the id cannot even be constructed.
    assert!(SessionId::from_string(".").is_err());

    // Layer 2: an id that bypassed the validator (SessionId derives
    // Deserialize) is still refused before anything is created or removed.
    let smuggled: SessionId = serde_json::from_str("\".\"").unwrap();
    let base = SessionBase::open_creating(&base_dir).unwrap();
    assert!(
        SessionLogService::start_in(base, smuggled, "alice", Some("agent"), "user-alice", None)
            .is_err()
    );

    assert!(base_dir.join("ses_a").is_dir());
    assert!(base_dir.join("ses_b").is_dir());
}
