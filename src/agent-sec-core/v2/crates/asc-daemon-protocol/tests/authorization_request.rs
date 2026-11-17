use asc_daemon_protocol::authorization::{
    AuthorizationRequest, EntityReference, OsAuthorizationRequest, Validate,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolContext {
    arguments: Vec<String>,
}

#[test]
fn another_action_reuses_parc_without_os_fields() {
    let request = AuthorizationRequest {
        request_id: "tool-request".into(),
        principal: EntityReference::new("Tools::User", "alice"),
        action: EntityReference::new("Tools::Action", "invoke"),
        resource: EntityReference::new("Tools::Tool", "search"),
        context: ToolContext {
            arguments: vec!["query".into()],
        },
    };
    request.validate_envelope().unwrap();
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 5);
    assert!(value["context"].get("launch").is_none());
    let decoded: AuthorizationRequest<ToolContext> = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded, request);
    // A reusable envelope does not claim that the OS service implements this action.
    assert!(serde_json::from_value::<OsAuthorizationRequest>(value).is_err());
}

fn os_request() -> OsAuthorizationRequest {
    OsAuthorizationRequest::new("request".into(), serde_json::from_value(json!({
        "execution_id": "execution", "agent": {"agent_type": "cosh"},
        "launch": {"program": "true", "resolved_program": "/usr/bin/true", "args": [], "cwd": "/work"},
        "requested_permissions": {"version": 1, "filesystem": {"default": "deny", "rules": []},
            "execution": {"deny_paths": []}, "network": {"mode": "off"}}
    })).unwrap())
}

#[test]
fn os_adapter_rejects_unknown_actions_subjects_resources_and_fields() {
    let request = os_request();
    request.validate().unwrap();
    for field in ["principal", "action", "resource"] {
        let mut value = serde_json::to_value(&request).unwrap();
        value[field]["id"] = json!("different");
        let changed: OsAuthorizationRequest = serde_json::from_value(value).unwrap();
        assert_ne!(changed.digest().unwrap(), request.digest().unwrap());
        assert!(changed.validate().is_err(), "{field}");
    }
    let mut changed = request.clone();
    changed.context.launch.args.push("changed".into());
    assert_ne!(changed.digest().unwrap(), request.digest().unwrap());
    let mut unknown = serde_json::to_value(&request).unwrap();
    unknown["type"] = json!("os");
    let error = serde_json::from_value::<OsAuthorizationRequest>(unknown).unwrap_err();
    assert!(
        error.to_string().contains("unknown field `type`"),
        "{error}"
    );
    let old = json!({
        "type": "os", "request_id": request.request_id,
        "execution_id": request.context.execution_id, "principal": request.context.agent,
        "launch": request.context.launch,
        "requested_permissions": request.context.requested_permissions
    });
    assert!(serde_json::from_value::<OsAuthorizationRequest>(old).is_err());
}
