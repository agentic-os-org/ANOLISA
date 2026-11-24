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
fn cwd_entry_must_match_the_launch_at_both_protocol_boundaries() {
    use asc_daemon_protocol::authorization::ExecutionSpec;

    let mut request = os_request();
    let cwd = request.context.launch.cwd.clone();
    request.context.requested_permissions.execution.cwd_entry = Some(cwd.clone());
    request.validate().unwrap();
    let mut spec: ExecutionSpec = serde_json::from_value(json!({
        "version": 1,
        "launch": {"program": "true", "resolved_program": "/usr/bin/true", "args": [],
                   "cwd": cwd, "env": {}},
        "permissions": request.context.requested_permissions,
        "path_facts": []
    }))
    .unwrap();
    spec.validate().unwrap();

    request.context.launch.cwd = "/other".into();
    spec.launch.cwd = "/other".into();
    for error in [
        request.validate().unwrap_err(),
        spec.validate().unwrap_err(),
    ] {
        assert_eq!(error.path, "cwd_entry");
        assert!(error.message.contains(&cwd), "{error}");
    }
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

#[test]
fn backend_plans_preserve_linux_wire_compatibility_and_reject_mixed_backends() {
    use asc_daemon_protocol::authorization::{
        BackendSelection, ExecutionPlan, ExecutionSpec, LandlockMode, LinuxSandboxOptions,
    };
    let spec = json!({
        "version":1,
        "launch":{"program":"echo","resolved_program":"/bin/echo","args":[],"cwd":"/","env":{}},
        "permissions":os_request().context.requested_permissions,
        "path_facts":[]
    });
    for mode in [
        LandlockMode::Off,
        LandlockMode::Auto,
        LandlockMode::Required,
    ] {
        let mut wire = spec.clone();
        if !mode.is_off() {
            wire["landlock"] = json!(mode);
        }
        let plan: ExecutionPlan = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            plan.backend,
            BackendSelection::LinuxSandbox(LinuxSandboxOptions { landlock: mode })
        );
        assert_eq!(serde_json::to_value(&plan.spec).unwrap(), spec);
        assert_eq!(serde_json::to_value(&plan).unwrap(), wire);
        if !mode.is_off() {
            assert!(serde_json::from_value::<ExecutionSpec>(wire).is_err());
        }
    }
    let docker = json!({"backend":{"kind":"docker"}, "spec":spec});
    let plan: ExecutionPlan = serde_json::from_value(docker.clone()).unwrap();
    assert_eq!(plan.backend, BackendSelection::Docker {});
    assert_eq!(serde_json::to_value(plan).unwrap(), docker);
    let mut extra = docker.clone();
    extra["landlock"] = json!("required");
    let mut mixed = docker.clone();
    mixed["backend"]["landlock"] = json!("required");
    let mut unknown = docker.clone();
    unknown["backend"]["kind"] = json!("unknown");
    let mut legacy = spec.clone();
    legacy["backend"] = json!({"kind":"docker"});
    let mut nested = docker;
    nested["spec"]["landlock"] = json!("required");
    for invalid in [extra, mixed, unknown, legacy, nested] {
        assert!(
            serde_json::from_value::<ExecutionPlan>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn execution_receipts_keep_backend_evidence_strict_and_flat() {
    use asc_daemon_protocol::authorization::{BackendEvidence, ExecutionResult, LandlockStatus};
    let mut result = ExecutionResult::default();
    let mut wire = serde_json::to_value(&result).unwrap();
    assert!(wire.get("landlock").is_none());
    result.evidence = BackendEvidence::LinuxSandbox {
        landlock: LandlockStatus::Enforced { abi: 3 },
    };
    wire["landlock"] = json!({"state":"enforced","abi":3});
    assert_eq!(serde_json::to_value(&result).unwrap(), wire);
    assert_eq!(
        serde_json::from_value::<ExecutionResult>(wire.clone()).unwrap(),
        result
    );
    wire["landlock"] = json!({"state":"unknown"});
    assert!(serde_json::from_value::<ExecutionResult>(wire.clone()).is_err());
    wire.as_object_mut().unwrap().remove("landlock");
    wire["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ExecutionResult>(wire).is_err());
}

#[test]
fn linux_installation_evidence_is_valid_only_for_a_compatible_backend() {
    use asc_daemon_protocol::authorization::{BackendEvidence, BackendSelection, LandlockStatus};

    let enforced = BackendEvidence::LinuxSandbox {
        landlock: LandlockStatus::Enforced { abi: 3 },
    };
    let docker = Some(BackendSelection::Docker {});
    assert!(BackendEvidence::None.matches_installation(docker, true));
    assert!(!enforced.matches_installation(docker, true));
    assert!(enforced.matches_installation(None, true));
    assert!(!enforced.matches_installation(None, false));
    let invalid = BackendEvidence::LinuxSandbox {
        landlock: LandlockStatus::Skipped {
            reason: String::new(),
        },
    };
    assert!(!invalid.matches_installation(None, true));
}
