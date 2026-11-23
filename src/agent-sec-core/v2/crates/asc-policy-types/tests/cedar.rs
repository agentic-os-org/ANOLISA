use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::policy::PreparedPolicy;
use serde_json::{Value, json};

fn cedar_source() -> Value {
    json!({
        "language": "cedar",
        "content": {
            "schema": {"schemaId": "seccore.system", "revision": 1},
            "text": "// Keep original source for diagnostics.\nforbid(principal, action, resource);"
        }
    })
}

#[test]
fn native_source_reuses_the_existing_snapshot_fixture() {
    let objects: Value = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/objects.json"
    ))
    .unwrap();
    let snapshot_json = objects["policy.prevent-file-deletion"].clone();
    let snapshot: PreparedPolicy = serde_json::from_value(snapshot_json.clone()).unwrap();
    snapshot.validate().unwrap();
    let source_json = json!({"language": "seccore", "content": snapshot_json["template"]});
    let source: PolicySource = serde_json::from_value(source_json.clone()).unwrap();
    source.validate().unwrap();
    assert_eq!(source, snapshot.template);
    assert_eq!(serde_json::to_value(source).unwrap(), source_json);

    assert_eq!(serde_json::to_value(snapshot).unwrap(), snapshot_json);
}

#[test]
fn cedar_source_preserves_text_and_rejects_injected_ast() {
    let source_json = cedar_source();
    let source: PolicySource = serde_json::from_value(source_json.clone()).unwrap();
    source.validate().unwrap();
    assert_eq!(serde_json::to_value(source).unwrap(), source_json);

    let mut injected = source_json;
    injected["content"]["ast"] = json!({"staticPolicies": {}});
    assert!(serde_json::from_value::<PolicySource>(injected).is_err());
}

#[test]
fn sources_reject_unknown_languages_fields_and_unpinned_schemas() {
    let mut bad_language = cedar_source();
    bad_language["language"] = json!("unknown");
    let mut extra_field = cedar_source();
    extra_field["principal"] = json!("agent-1");
    let mut zero_revision = cedar_source();
    zero_revision["content"]["schema"]["revision"] = json!(0);
    let mut invalid_schema_id = cedar_source();
    invalid_schema_id["content"]["schema"]["schemaId"] = json!("../schema");
    for input in [bad_language, extra_field, zero_revision, invalid_schema_id] {
        assert!(serde_json::from_value::<PolicySource>(input).is_err());
    }
    for text in ["", " \n\t", "forbid(principal, action, resource);\0"] {
        let mut input = cedar_source();
        input["content"]["text"] = json!(text);
        let source: PolicySource = serde_json::from_value(input).unwrap();
        assert_eq!(source.validate().unwrap_err().path, "content.text");
    }
}

#[test]
fn policy_snapshot_source_compatibility_is_strict_and_preserves_native_wire_shape() {
    let mut snapshot: Value = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/objects.json"
    ))
    .unwrap();
    let native = snapshot["policy.prevent-file-deletion"].take();
    let mut wrapped = native.clone();
    wrapped["template"] = json!({"language":"seccore", "content": native["template"]});
    let parsed: PreparedPolicy = serde_json::from_value(wrapped).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), native);

    let mut cedar = native;
    cedar["template"] = cedar_source();
    let parsed: PreparedPolicy = serde_json::from_value(cedar.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), cedar);
    let prefix = r#"{"policyId":"p","policyName":"test","revision":1,"template":"#;
    for source in [
        r#"{"language":"cedar","language":"cedar","content":{"schema":{"schemaId":"seccore.system","revision":1},"text":"first"}}"#,
        r#"{"language":"cedar","content":{"schema":{"schemaId":"seccore.system","revision":1},"text":"first","text":"second"}}"#,
        r#"{"language":"cedar","content":{"schema":{"schemaId":"seccore.system","revision":1,"revision":2},"text":"first"}}"#,
        r#"{"language":"unknown","specVersion":"0.1","rules":[]}"#,
        r#"{"specVersion":"0.1","rules":[],"rules":[]}"#,
    ] {
        assert!(
            serde_json::from_str::<PreparedPolicy>(&format!("{prefix}{source}}}")).is_err(),
            "{source}"
        );
    }
}
