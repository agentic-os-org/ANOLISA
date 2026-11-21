//! Hermes lifecycle event and reason mapping.

use super::*;

#[test]
fn lifecycle_distinguishes_hermes_turn_end_from_session_finalization() {
    let context = NativeContext {
        target: "hermes".into(),
        instance_id: "instance".into(),
        cwd: "/work".into(),
    };
    let mut native =
        json!({"hook_event_name":"on_session_start","session_id":"session","cwd":"/work"});
    assert!(Hermes.normalize(&context, "session.start", &native).is_ok());
    native["hook_event_name"] = json!("on_session_end");
    native["turn_id"] = json!("turn");
    native["reason"] = json!("interrupted_by_user");
    assert!(Hermes.normalize(&context, "turn.stop", &native).is_ok());
    assert!(Hermes.normalize(&context, "session.end", &native).is_err());
    native["hook_event_name"] = json!("on_session_finalize");
    native["reason"] = json!("shutdown");
    assert_eq!(
        Hermes.normalize(&context, "session.end", &native).unwrap()["native"]["reason"],
        "shutdown"
    );
}
