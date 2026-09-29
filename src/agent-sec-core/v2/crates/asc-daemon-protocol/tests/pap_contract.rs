use std::collections::{BTreeMap, BTreeSet};

use asc_daemon_protocol::method;
use asc_daemon_protocol::{
    CreateBindingParams, CreatePolicyParams, CreateScopeParams, DaemonRequest, DaemonResponse,
    ErrorCode, ListParams, ListResult, RequestId, ResourceParams, RevisionParams,
    UpdateBindingParams, UpdatePolicyParams, UpdateScopeParams, error_code,
};
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::scope::{PreparedScope, ScopeSelector};
use serde_json::{Value, json};

#[test]
fn every_pap_method_has_frozen_input_and_output_types() {
    let fixtures = method_fixtures();
    assert_eq!(fixtures.len(), method::PAP_METHODS.len());
    assert_eq!(
        fixtures
            .iter()
            .map(|fixture| fixture["method"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        method::PAP_METHODS.into_iter().collect::<BTreeSet<_>>()
    );

    for fixture in fixtures {
        let method_name = fixture["method"].as_str().unwrap();
        let params = fixture["params"].clone();
        let canonical = fixture["canonicalParams"].clone();
        let (encoded, result_type) = match method_name {
            method::POLICY_TEMPLATES_CREATE => {
                (round_trip::<CreatePolicyParams>(params), "PreparedPolicy")
            }
            method::POLICY_TEMPLATES_UPDATE => {
                (round_trip::<UpdatePolicyParams>(params), "PreparedPolicy")
            }
            method::POLICY_SCOPES_CREATE => {
                (round_trip::<CreateScopeParams>(params), "PreparedScope")
            }
            method::POLICY_SCOPES_UPDATE => {
                (round_trip::<UpdateScopeParams>(params), "PreparedScope")
            }
            method::POLICY_BINDINGS_CREATE => {
                (round_trip::<CreateBindingParams>(params), "BindingView")
            }
            method::POLICY_BINDINGS_UPDATE => {
                (round_trip::<UpdateBindingParams>(params), "BindingView")
            }
            method::POLICY_TEMPLATES_GET | method::POLICY_TEMPLATES_DELETE => {
                (round_trip::<RevisionParams>(params), "PreparedPolicy")
            }
            method::POLICY_SCOPES_GET | method::POLICY_SCOPES_DELETE => {
                (round_trip::<RevisionParams>(params), "PreparedScope")
            }
            method::POLICY_BINDINGS_GET | method::POLICY_BINDINGS_DELETE => {
                (round_trip::<ResourceParams>(params), "BindingView")
            }
            method::POLICY_TEMPLATES_LIST => (
                round_trip::<ListParams>(params),
                "ListResult<PreparedPolicy>",
            ),
            method::POLICY_SCOPES_LIST => (
                round_trip::<ListParams>(params),
                "ListResult<PreparedScope>",
            ),
            method::POLICY_BINDINGS_LIST => {
                (round_trip::<ListParams>(params), "ListResult<BindingView>")
            }
            unexpected => panic!("unregistered method fixture {unexpected}"),
        };
        assert_eq!(encoded, canonical, "noncanonical params for {method_name}");
        assert_eq!(
            fixture["resultType"], result_type,
            "wrong result for {method_name}"
        );
    }
}

#[test]
fn method_results_reuse_complete_domain_contracts() {
    let binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    let policy = binding.policy.clone();
    let scope = binding.scope.clone();
    let binding = BindingView {
        spec: binding,
        status: (BindingStatus::PendingApply).into(),
    };

    round_trip_value::<PreparedPolicy>(&policy);
    round_trip_value::<PreparedScope>(&scope);
    round_trip_value::<BindingView>(&binding);
    round_trip_value(&ListResult {
        items: vec![policy],
        total: 1,
    });
    round_trip_value(&ListResult {
        items: vec![scope],
        total: 1,
    });
    round_trip_value(&ListResult {
        items: vec![binding],
        total: 1,
    });
}

#[test]
fn authored_params_reject_server_owned_or_legacy_fields() {
    let mut policy = json!({
        "policyName": "protect-important-files",
        "template": {"kind": "prevent_file_deletion", "files": ["/important"]}
    });
    policy["revision"] = json!(1);
    assert!(serde_json::from_value::<CreatePolicyParams>(policy).is_err());

    for selector in [
        json!({"kind": "pid", "pid": 0}),
        json!({"kind": "cgroup_id", "cgroupId": 0}),
        json!({
            "kind": "legacy_execution_domain",
            "executionDomainId": "legacy-domain"
        }),
    ] {
        assert!(
            serde_json::from_value::<CreateScopeParams>(json!({"selector": selector})).is_err()
        );
        assert!(
            serde_json::from_value::<UpdateScopeParams>(json!({
                "scopeId": "existing-scope",
                "selector": selector
            }))
            .is_err()
        );
    }

    let invalid_outbound = CreateScopeParams {
        selector: ScopeSelector::Pid { pid: 0 },
    };
    assert!(serde_json::to_value(invalid_outbound).is_err());
}

#[test]
fn request_and_response_envelopes_are_strict_and_mutually_exclusive() {
    let request: DaemonRequest = serde_json::from_value(json!({
        "method": method::POLICY_TEMPLATES_LIST
    }))
    .unwrap();
    assert_eq!(request.params, json!({}));

    for invalid in [
        json!({"method": "", "params": {}}),
        json!({"method": method::POLICY_TEMPLATES_LIST, "params": null}),
        json!({"method": method::POLICY_TEMPLATES_LIST, "params": {}, "callerUid": 1000}),
    ] {
        assert!(serde_json::from_value::<DaemonRequest>(invalid).is_err());
    }
    for invalid in [
        r#"{"method":"policy.templates.list","params":{"limit":1,"limit":2}}"#,
        r#"{"method":"policy.templates.create","params":{"policyName":"invalid","template":{"kind":"prevent_file_deletion","files":["/a"],"files":["/b"]}}}"#,
    ] {
        assert!(serde_json::from_str::<DaemonRequest>(invalid).is_err());
    }

    let responses: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/daemon-responses.json")).unwrap();
    for fixture in responses {
        let wire = fixture["response"].clone();
        let response: DaemonResponse = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), wire);
    }
    for invalid in [
        json!({"requestId": "request-1"}),
        json!({"requestId": "request-1", "result": {}, "error": {"code": "internal", "message": "failed"}}),
        json!({"requestId": "request-1", "ok": true, "data": {}}),
        json!({"requestId": " ", "result": {}}),
    ] {
        assert!(serde_json::from_value::<DaemonResponse>(invalid).is_err());
    }

    assert_eq!(
        DaemonResponse::success(RequestId::new("request-3").unwrap(), json!({}))
            .request_id()
            .as_str(),
        "request-3"
    );
    for code in [
        error_code::INVALID_REQUEST,
        error_code::INVALID_ARGUMENT,
        error_code::UNKNOWN_METHOD,
        error_code::PERMISSION_DENIED,
        error_code::NOT_FOUND,
        error_code::CONFLICT,
        error_code::RESOURCE_EXHAUSTED,
        error_code::DEADLINE_EXCEEDED,
        error_code::UNAVAILABLE,
        error_code::INTERNAL,
    ] {
        assert_eq!(ErrorCode::new(code).unwrap().as_str(), code);
    }
}

#[test]
fn daemon_error_messages_are_bounded_at_construction_and_decode() {
    let response = DaemonResponse::<Value>::error(
        RequestId::new("request-bounded-error").unwrap(),
        error_code::INVALID_REQUEST,
        &"x".repeat(512),
    );
    let DaemonResponse::Error(response) = response else {
        panic!("the error constructor must return an error response");
    };
    assert_eq!(response.error.message().len(), 256);
    assert!(response.error.message().ends_with("..."));

    let oversized_wire = json!({
        "requestId": "request-oversized-error",
        "error": {
            "code": "invalid_request",
            "message": "x".repeat(257)
        }
    });
    assert!(serde_json::from_value::<DaemonResponse>(oversized_wire).is_err());
}

#[test]
fn identifiers_revisions_and_pagination_are_bounded() {
    for invalid in [
        json!({"id": "invalid/id", "revision": 1}),
        json!({"id": "policy-1", "revision": 0}),
        json!({"id": "policy-1", "revision": -1}),
    ] {
        assert!(serde_json::from_value::<RevisionParams>(invalid).is_err());
    }
    assert_eq!(
        serde_json::from_value::<ListParams>(json!({})).unwrap(),
        ListParams::default()
    );
    for invalid in [
        json!({"limit": 0}),
        json!({"limit": 1001}),
        json!({"limit": 1, "offset": -1}),
    ] {
        assert!(serde_json::from_value::<ListParams>(invalid).is_err());
    }
}

#[test]
fn complete_crud_scenario_freezes_every_registered_method_and_domain_result() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/pap-crud-e2e.json")).unwrap();
    assert_eq!(fixture["schemaVersion"], 1);

    let dynamic_values = fixture["dynamicValues"].as_object().unwrap();
    assert_eq!(
        dynamic_values
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        ["binding_id", "policy_id", "request_id", "scope_id"]
            .into_iter()
            .collect()
    );

    let objects = fixture["objects"].as_object().unwrap();
    let variables = BTreeMap::from([
        (
            "request_id".to_owned(),
            json!("00000000-0000-4000-8000-000000000001"),
        ),
        (
            "policy_id".to_owned(),
            json!("00000000-0000-4000-8000-000000000002"),
        ),
        (
            "scope_id".to_owned(),
            json!("00000000-0000-4000-8000-000000000003"),
        ),
        (
            "binding_id".to_owned(),
            json!("00000000-0000-4000-8000-000000000004"),
        ),
    ]);
    let steps = fixture["steps"].as_array().unwrap();
    assert_eq!(steps.len(), method::PAP_METHODS.len());
    assert_eq!(
        steps
            .iter()
            .map(|step| step["request"]["method"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        method::PAP_METHODS.into_iter().collect::<BTreeSet<_>>()
    );

    for step in steps {
        assert!(step["name"].as_str().is_some_and(|name| !name.is_empty()));
        let request_value = expand_fixture(&step["request"], objects, &variables, 0);
        let request: DaemonRequest = serde_json::from_value(request_value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), request_value);
        decode_params(&request.method, &request.params);

        for capture in step["captures"].as_array().unwrap() {
            assert!(dynamic_values.contains_key(capture["name"].as_str().unwrap()));
            assert_eq!(capture["format"], "uuid");
            assert!(capture["pointer"].as_str().unwrap().starts_with("/result/"));
        }

        let response_value = expand_fixture(&step["expectedResponse"], objects, &variables, 0);
        let response: DaemonResponse = serde_json::from_value(response_value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&response).unwrap(), response_value);
        let DaemonResponse::Success(response) = response else {
            panic!("CRUD fixture steps must freeze successful responses");
        };
        decode_result(&request.method, &response.result);
    }
}

#[test]
fn invalid_request_fixture_covers_every_registered_crud_method() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/pap-invalid-requests.json")).unwrap();
    assert_eq!(fixture["schemaVersion"], 1);
    assert_eq!(
        fixture["generatedValues"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        ["path_4097", "policy_name_257", "resource_id_129"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        fixture["dynamicValues"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        ["existing_policy_id"].into_iter().collect()
    );

    let cases = fixture["cases"].as_array().unwrap();
    let names = cases
        .iter()
        .map(|case| case["name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names.len(),
        cases.len(),
        "error fixture names must be unique"
    );
    assert_eq!(
        cases
            .iter()
            .map(|case| case["request"]["method"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        method::PAP_METHODS.into_iter().collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert!(case["request"]["params"].is_object());
        let code = case["expectedError"]["code"].as_str().unwrap();
        ErrorCode::new(code).unwrap();
        let message = case["expectedError"]["message"].as_str().unwrap();
        assert!(!message.is_empty(), "{} has an empty message", case["name"]);
        assert!(
            message.len() <= 256,
            "{} has an unbounded expected message",
            case["name"]
        );
    }
}

fn method_fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/pap-methods.json")).unwrap()
}

fn decode_params(method_name: &str, params: &Value) {
    match method_name {
        method::POLICY_TEMPLATES_CREATE => assert_canonical_value::<CreatePolicyParams>(params),
        method::POLICY_TEMPLATES_UPDATE => assert_canonical_value::<UpdatePolicyParams>(params),
        method::POLICY_SCOPES_CREATE => assert_canonical_value::<CreateScopeParams>(params),
        method::POLICY_SCOPES_UPDATE => assert_canonical_value::<UpdateScopeParams>(params),
        method::POLICY_BINDINGS_CREATE => assert_canonical_value::<CreateBindingParams>(params),
        method::POLICY_BINDINGS_UPDATE => assert_canonical_value::<UpdateBindingParams>(params),
        method::POLICY_TEMPLATES_GET
        | method::POLICY_TEMPLATES_DELETE
        | method::POLICY_SCOPES_GET
        | method::POLICY_SCOPES_DELETE => assert_canonical_value::<RevisionParams>(params),
        method::POLICY_BINDINGS_GET | method::POLICY_BINDINGS_DELETE => {
            assert_canonical_value::<ResourceParams>(params);
        }
        method::POLICY_TEMPLATES_LIST
        | method::POLICY_SCOPES_LIST
        | method::POLICY_BINDINGS_LIST => assert_canonical_value::<ListParams>(params),
        unexpected => panic!("unregistered CRUD fixture method {unexpected}"),
    }
}

fn decode_result(method_name: &str, result: &Value) {
    match method_name {
        method::POLICY_TEMPLATES_CREATE
        | method::POLICY_TEMPLATES_UPDATE
        | method::POLICY_TEMPLATES_GET
        | method::POLICY_TEMPLATES_DELETE => assert_canonical_value::<PreparedPolicy>(result),
        method::POLICY_SCOPES_CREATE
        | method::POLICY_SCOPES_UPDATE
        | method::POLICY_SCOPES_GET
        | method::POLICY_SCOPES_DELETE => assert_canonical_value::<PreparedScope>(result),
        method::POLICY_BINDINGS_CREATE
        | method::POLICY_BINDINGS_UPDATE
        | method::POLICY_BINDINGS_GET
        | method::POLICY_BINDINGS_DELETE => assert_canonical_value::<BindingView>(result),
        method::POLICY_TEMPLATES_LIST => {
            assert_canonical_value::<ListResult<PreparedPolicy>>(result);
        }
        method::POLICY_SCOPES_LIST => {
            assert_canonical_value::<ListResult<PreparedScope>>(result);
        }
        method::POLICY_BINDINGS_LIST => {
            assert_canonical_value::<ListResult<BindingView>>(result);
        }
        unexpected => panic!("unregistered CRUD fixture method {unexpected}"),
    }
}

fn assert_canonical_value<T>(value: &Value)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<T>(value.clone()).unwrap()).unwrap(),
        *value
    );
}

fn expand_fixture(
    value: &Value,
    objects: &serde_json::Map<String, Value>,
    variables: &BTreeMap<String, Value>,
    depth: u8,
) -> Value {
    assert!(depth < 32, "fixture reference cycle");
    match value {
        Value::Object(fields) if fields.len() == 1 && fields.contains_key("$ref") => {
            let reference = fields["$ref"].as_str().unwrap();
            expand_fixture(
                objects
                    .get(reference)
                    .unwrap_or_else(|| panic!("unknown fixture object {reference}")),
                objects,
                variables,
                depth + 1,
            )
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        expand_fixture(value, objects, variables, depth + 1),
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|value| expand_fixture(value, objects, variables, depth + 1))
                .collect(),
        ),
        Value::String(candidate) if candidate.starts_with("${") && candidate.ends_with('}') => {
            let name = &candidate[2..candidate.len() - 1];
            variables
                .get(name)
                .unwrap_or_else(|| panic!("unknown fixture variable {name}"))
                .clone()
        }
        _ => value.clone(),
    }
}

fn round_trip<T>(value: Value) -> Value
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    serde_json::to_value(serde_json::from_value::<T>(value).unwrap()).unwrap()
}

fn round_trip_value<T>(value: &T)
where
    T: serde::de::DeserializeOwned + serde::Serialize + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(value).unwrap();
    assert_eq!(serde_json::from_value::<T>(encoded).unwrap(), *value);
}
