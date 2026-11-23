use asc_daemon_protocol::authorization::{ApprovalTarget, Decision, FileRight};
use serde_json::json;

#[test]
fn agent_classification_accepts_optional_type_without_identity() {
    use asc_daemon_protocol::authorization::{AgentPrincipal, Validate};
    let subject: AgentPrincipal = serde_json::from_value(json!({})).unwrap();
    subject.validate().unwrap();
    assert_eq!(subject.agent_type, None);
    assert_eq!(serde_json::to_value(&subject).unwrap(), json!({}));
    for agent_type in [json!(""), json!(123)] {
        let parsed = serde_json::from_value::<AgentPrincipal>(json!({
            "agent_type":agent_type
        }));
        assert!(parsed.is_err() || parsed.unwrap().validate().is_err());
    }
    let typed: AgentPrincipal = serde_json::from_value(json!({"agent_type":"codex"})).unwrap();
    typed.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        json!({"agent_type":"codex"})
    );
}

#[test]
fn approval_provenance_round_trips_and_older_decisions_default_to_no_items() {
    let mut wire = json!({
        "decision_id":"d", "request_id":"r", "execution_id":"e", "request_digest":"digest",
        "policy_version":"p", "expires_at":100,
        "principal":{"agent_type":"cosh"},
        "launch":{"program":"python3", "resolved_program":"/usr/bin/python3", "args":["export.py"], "cwd":"/work"},
        "verdict":"STEP_UP", "reason":"review", "warnings":[], "effective_permissions":null
    });
    let legacy: Decision = serde_json::from_value(wire.clone()).unwrap();
    assert!(legacy.approval_requirements.is_empty());
    assert!(
        serde_json::to_value(legacy)
            .unwrap()
            .get("approval_requirements")
            .is_none()
    );

    wire["approval_requirements"] = json!([
        {"target":{"kind":"filesystem", "path":"/work", "right":"write", "excluded_paths":["/work/input"]}, "message":"Shared reports"},
        {"target":{"kind":"execution", "original_request":true}},
        {"target":{"kind":"network"}, "message":"Host network"}
    ]);
    let decision: Decision = serde_json::from_value(wire).unwrap();
    assert_eq!(
        decision.approval_requirements[0].target,
        ApprovalTarget::Filesystem {
            path: "/work".into(),
            right: FileRight::Write,
            excluded_paths: vec!["/work/input".into()],
        }
    );
    assert_eq!(
        serde_json::from_value::<Decision>(serde_json::to_value(&decision).unwrap()).unwrap(),
        decision
    );
}
