use asc_policy_cedar::schema::{SCHEMA_ID, SCHEMA_REVISION, SCHEMA_TEXT};
use asc_policy_cedar::{CedarError, pst, validate};
use asc_policy_types::cedar::{CedarPolicySource, CedarSchemaReference};
use asc_policy_types::identifiers::{ResourceId, Revision};
use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityUid, PolicySet, Request, Schema, ValidationMode,
    Validator,
};
use serde_json::json;

fn source(text: &str) -> CedarPolicySource {
    CedarPolicySource {
        schema: CedarSchemaReference {
            schema_id: ResourceId::new(SCHEMA_ID).unwrap(),
            revision: Revision::new(SCHEMA_REVISION).unwrap(),
        },
        text: text.to_owned(),
    }
}

const DELETE_POLICY: &str = r#"
forbid(principal, action == SecCore::Action::"file.write", resource)
when {
    resource.path == "/etc/agent/config.yaml" &&
    context.input.program == "/usr/bin/rm" &&
    context.input.operation == SecCore::WriteOperation::"delete"
};
"#;

const PROCESS_POLICY: &str = r#"
permit(principal, action == SecCore::Action::"file.exec", resource)
when {
    resource.path == "/usr/bin/cc" &&
    context.input.cwd == "/workspace" &&
    context.input.args.containsAll([
        { index: 0, value: "-I" },
        { index: 1, value: "/workspace/include" },
        { index: 2, value: "-I" }
    ])
};
"#;

#[test]
fn file_policy_round_trips_through_official_pst_without_rewriting() {
    let text = format!(
        "// Retain the author's text.\npermit(principal, action, resource);\n{DELETE_POLICY}\n\
         forbid(principal, action == SecCore::Action::\"file.write\", resource)\n\
         when {{ context has input.mode && context.input.mode > 511 }};"
    );
    let input = source(&text);
    let pst = pst(&input).unwrap();
    assert_eq!(input.text, text);
    let policies = PolicySet::from_pst(pst.clone()).unwrap();
    assert_eq!(policies.policies().count(), 3);
    assert_eq!(policies.to_pst().unwrap(), pst);
    assert_eq!(text.parse::<PolicySet>().unwrap().to_pst().unwrap(), pst);
}

#[test]
fn schema_has_no_warnings_and_accepts_action_specific_fields() {
    let (_, warnings) = Schema::from_cedarschema_str(SCHEMA_TEXT).unwrap();
    assert_eq!(warnings.count(), 0);
    for text in [
        r#"permit(principal, action == SecCore::Action::"file.read", resource)
           when { resource.path like "/workspace/*" && context.input.program == "/usr/bin/cat" };"#,
        r#"forbid(principal, action == SecCore::Action::"file.write", resource)
           when { context.input.operation == SecCore::WriteOperation::"setattr" &&
                  context.input has mode && context.input.mode > 511 &&
                  context.input has ownerUid && context.input.ownerUid == 0 &&
                  context.input has ownerGid && context.input.ownerGid == 0 };"#,
        r#"permit(principal, action == SecCore::Action::"file.exec", resource)
           when { resource.path == "/usr/bin/python3" && context.input.cwd == "/workspace" };"#,
        PROCESS_POLICY,
        r#"permit(principal, action == SecCore::Action::"network.use",
                  resource == SecCore::Network::"all");"#,
        r#"forbid(principal, action == SecCore::Action::"file.read", resource) when { resource.path == "principal" };"#,
        r#"forbid(principal, action, resource) when { { Var: "principal" }.Var == "principal" };"#,
        r#"permit(principal, action == SecCore::Action::"file.read", resource) when { false };"#,
    ] {
        validate(&source(text)).unwrap_or_else(|error| panic!("{text}: {error:?}"));
    }
}

#[test]
fn validate_returns_non_blocking_warnings_with_policy_identity() {
    let input = source(
        r#"permit(principal, action == SecCore::Action::"file.read", resource) when { false };"#,
    );
    let warnings = validate(&input).unwrap();
    assert!(warnings.iter().any(|warning| {
        matches!(
            warning,
            cedar_policy::ValidationWarning::ImpossiblePolicy(_)
        ) && warning.policy_id() == &cedar_policy::PolicyId::new("policy0")
    }));
    assert_eq!(pst(&input).unwrap().policies.len(), 1);
}

#[test]
fn validate_and_pst_enforce_the_same_admission_contract() {
    let mut unknown_schema = source(DELETE_POLICY);
    unknown_schema.schema.revision = Revision::new(u32::MAX).unwrap();
    for input in [
        source(DELETE_POLICY),
        source(""),
        source("forbid("),
        source("// No policy statements."),
        source(r#"@id("label") permit(principal, action, resource);"#),
        source(
            r#"permit(principal, action, resource)
               when { principal == SecCore::Subject::"worker" };"#,
        ),
        source("permit(principal, action, resource) when { context.typo };"),
        source(
            r#"permit(principal, action == SecCore::Action::"file.exec",
                      resource == SecCore::Network::"all");"#,
        ),
        unknown_schema,
    ] {
        let validation = validate(&input)
            .map(|_| ())
            .map_err(|error| std::mem::discriminant(&error));
        let syntax_tree = pst(&input)
            .map(|_| ())
            .map_err(|error| std::mem::discriminant(&error));
        assert_eq!(validation, syntax_tree, "{}", input.text);
    }
}

#[test]
fn unknown_schema_versions_never_fall_back_to_the_builtin() {
    let mut input = source(DELETE_POLICY);
    input.schema.schema_id = ResourceId::new("another.schema").unwrap();
    assert!(matches!(
        validate(&input),
        Err(CedarError::UnsupportedSchema { .. })
    ));
    input.schema.schema_id = ResourceId::new(SCHEMA_ID).unwrap();
    input.schema.revision = Revision::new(u32::MAX).unwrap();
    assert!(matches!(
        validate(&input),
        Err(CedarError::UnsupportedSchema { .. })
    ));
}

#[test]
fn rejects_invalid_sources_and_empty_policy_sets() {
    for text in ["", " \n\t", "forbid(principal, action, resource);\0"] {
        assert!(matches!(
            validate(&source(text)),
            Err(CedarError::Source(_))
        ));
    }
    assert!(matches!(
        validate(&source("forbid(")),
        Err(CedarError::Parse(_))
    ));
    assert!(matches!(
        validate(&source("// There is no statement.")),
        Err(CedarError::Unsupported { .. })
    ));
}

#[test]
fn rejects_principal_logic_in_headers_and_nested_conditions() {
    for text in [
        r#"permit(principal == SecCore::Subject::"worker", action, resource);"#,
        "permit(principal is SecCore::Subject, action, resource);",
        r#"permit(principal, action, resource)
           when { principal == SecCore::Subject::"worker" };"#,
        r#"permit(principal, action, resource)
           unless { !(principal == SecCore::Subject::"worker") };"#,
        r#"permit(principal, action, resource)
           when { [principal].contains(SecCore::Subject::"worker") };"#,
        r#"permit(principal, action, resource)
           when { { nested: { subject: principal } }.nested.subject == SecCore::Subject::"worker" };"#,
        r#"permit(principal, action, resource)
           when { if true then principal == SecCore::Subject::"worker" else false };"#,
        r#"permit(principal, action, resource)
           when { false && principal == SecCore::Subject::"worker" };"#,
        r#"permit(principal, action, resource)
           when { true || principal == SecCore::Subject::"worker" };"#,
        r#"permit(principal, action, resource)
           when { if false then principal == SecCore::Subject::"worker" else true };"#,
        "permit(principal, action, resource)
           when { { subject: principal } has subject };",
    ] {
        assert!(
            matches!(validate(&source(text)), Err(CedarError::Unsupported { .. })),
            "{text}"
        );
    }
}

#[test]
fn rejects_templates_and_annotations_without_an_execution_contract() {
    for text in [
        "permit(principal == ?principal, action, resource);",
        "permit(principal, action, resource == ?resource);",
        r#"@advice("require_confirmation") permit(principal, action, resource);"#,
        r#"@id("label") permit(principal, action, resource);"#,
    ] {
        assert!(matches!(
            validate(&source(text)),
            Err(CedarError::Unsupported { .. })
        ));
    }
}

#[test]
fn rejects_unknown_fields_wrong_types_and_unmodeled_capabilities() {
    for text in [
        r#"forbid(principal, action == SecCore::Action::"file.read", resource)
           when { resource.pth == "/etc/passwd" };"#,
        r#"forbid(principal, action == SecCore::Action::"file.read", resource)
           when { resource.path == 42 };"#,
        r#"forbid(principal, action == SecCore::Action::"file.write", resource)
           when { context.input.operation == "delete" };"#,
        r#"forbid(principal, action == SecCore::Action::"file.write", resource)
           when { context.input.operation == SecCore::WriteOperation::"unlink" };"#,
        r#"forbid(principal, action == SecCore::Action::"file.read", resource)
           when { context.input.mode == 0 };"#,
        r#"forbid(principal, action == SecCore::Action::"file.write", resource)
           when { context.input.ownerUid == 0 };"#,
        r#"forbid(principal, action == SecCore::Action::"file.exec", resource)
           when { context.input.argsPrefix == ["-c"] };"#,
        r#"forbid(principal, action == SecCore::Action::"file.exec", resource)
           when { context.input.args.contains("-c") };"#,
        r#"forbid(principal, action == SecCore::Action::"file.exec", resource)
           when { context.input.args.contains({ index: "0", value: "-c" }) };"#,
        r#"permit(principal, action == SecCore::Action::"file.read", resource)
           when { context.input.cwd == "/workspace" };"#,
        r#"permit(principal, action == SecCore::Action::"file.exec",
                  resource == SecCore::Network::"all");"#,
        r#"forbid(principal, action == SecCore::Action::"file.write",
                  resource == SecCore::Network::"all");"#,
        r#"permit(principal, action == SecCore::Action::"network.use",
                  resource == SecCore::Network::"example.com");"#,
        r#"permit(principal, action == SecCore::Action::"network.use", resource)
           when { resource.port == 443 };"#,
        r#"permit(principal, action == SecCore::Action::"file.read", resource)
           when { context.pii.verdict == "pass" };"#,
        "forbid(principal, action, resource) when { context.previous == true };",
        r#"forbid(principal, action == SecCore::Action::"tool.invoke", resource);"#,
    ] {
        let Err(CedarError::Validation { errors, warnings }) = validate(&source(text)) else {
            panic!("expected a schema validation failure: {text}");
        };
        assert!(!errors.is_empty() || !warnings.is_empty());
    }
}

#[test]
fn rejects_the_whole_set_when_one_statement_is_invalid() {
    let text =
        format!("{DELETE_POLICY}\npermit(principal, action, resource) when {{ context.typo }};");
    assert!(matches!(
        validate(&source(&text)),
        Err(CedarError::Validation { .. })
    ));
}

#[test]
fn process_prefix_preserves_positions_repeated_tokens_and_trailing_arguments() {
    let schema = asc_policy_cedar::schema::load_schema(&source(PROCESS_POLICY).schema).unwrap();
    let pst = pst(&source(PROCESS_POLICY)).unwrap();
    let policies = PolicySet::from_pst(pst).unwrap();
    let entities = Entities::from_json_value(
        json!([{
            "uid": { "type": "SecCore::File", "id": "compiler" },
            "attrs": { "path": "/usr/bin/cc" },
            "parents": []
        }]),
        Some(&schema),
    )
    .unwrap();
    let action: EntityUid = r#"SecCore::Action::"file.exec""#.parse().unwrap();
    let cases: &[(&[&str], Decision)] = &[
        (&["-I", "/workspace/include", "-I"], Decision::Allow),
        (
            &["-I", "/workspace/include", "-I", "/usr/include", "main.c"],
            Decision::Allow,
        ),
        (&["/workspace/include", "-I", "-I"], Decision::Deny),
        (&["-I", "/workspace/include", "-D"], Decision::Deny),
        (&["-I", "/workspace/include"], Decision::Deny),
        (&[], Decision::Deny),
    ];
    for (args, expected) in cases {
        let positioned: Vec<_> = args
            .iter()
            .enumerate()
            .map(|(index, value)| json!({ "index": index, "value": value }))
            .collect();
        let context = Context::from_json_value(
            json!({ "input": { "cwd": "/workspace", "arg_count": args.len(), "args": positioned } }),
            Some((&schema, &action)),
        )
        .unwrap();
        let request = Request::new(
            r#"SecCore::Subject::"worker""#.parse().unwrap(),
            action.clone(),
            r#"SecCore::File::"compiler""#.parse().unwrap(),
            context,
            Some(&schema),
        )
        .unwrap();
        let response = Authorizer::new().is_authorized(&request, &policies, &entities);
        assert_eq!(response.decision(), *expected, "{args:?}");
        assert_eq!(response.diagnostics().errors().count(), 0, "{args:?}");
    }
}

#[test]
fn requests_require_the_resource_and_input_shape_of_their_action() {
    let schema = asc_policy_cedar::schema::load_schema(&source(DELETE_POLICY).schema).unwrap();
    let cases = [
        (
            "file.read",
            "File",
            json!({ "input": { "program": "/usr/bin/cat" } }),
        ),
        (
            "file.write",
            "File",
            json!({ "input": {
                "program": "/usr/bin/chmod",
                "operation": { "__entity": { "type": "SecCore::WriteOperation", "id": "setattr" } },
                "mode": 420, "ownerUid": 1000, "ownerGid": 1000
            } }),
        ),
        (
            "file.exec",
            "File",
            json!({ "input": { "cwd": "/workspace", "arg_count": 0, "args": [] } }),
        ),
        ("network.use", "Network", json!({})),
    ];
    for (name, resource_type, input) in cases {
        let action: EntityUid = format!(r#"SecCore::Action::"{name}""#).parse().unwrap();
        let context = Context::from_json_value(input, Some((&schema, &action))).unwrap();
        let resource: EntityUid = format!(r#"SecCore::{resource_type}::"all""#)
            .parse()
            .unwrap();
        let principal: EntityUid = r#"SecCore::Subject::"worker""#.parse().unwrap();
        Request::new(
            principal.clone(),
            action.clone(),
            resource.clone(),
            context.clone(),
            Some(&schema),
        )
        .unwrap();
        assert!(
            Request::new(
                principal.clone(),
                action.clone(),
                principal.clone(),
                context,
                Some(&schema),
            )
            .is_err(),
            "{name}: subject cannot serve as the resource"
        );
        if name != "network.use" {
            assert!(
                Request::new(principal, action, resource, Context::empty(), Some(&schema),)
                    .is_err(),
                "{name}: required input cannot be omitted"
            );
        }
    }
}

#[test]
fn a_hook_schema_can_be_added_without_changing_system_action_contexts() {
    // Test-only extension: no Hook actions or detection facts ship in the schema.
    let extension = r#"
namespace AgentHook {
    entity ToolResource;
    action "tool.invoke" appliesTo {
        principal: SecCore::Subject,
        resource: ToolResource,
        context: { input: { tool_name: String } }
    };
}
"#;
    let hook_policy = r#"
permit(principal, action == AgentHook::Action::"tool.invoke", resource)
when { context.input.tool_name == "read_file" };
"#;
    assert!(matches!(
        validate(&source(hook_policy)),
        Err(CedarError::Validation { .. })
    ));
    let (schema, warnings) =
        Schema::from_cedarschema_str(&format!("{SCHEMA_TEXT}\n{extension}")).unwrap();
    assert_eq!(warnings.count(), 0);
    let policies: PolicySet = format!("{DELETE_POLICY}\n{PROCESS_POLICY}\n{hook_policy}")
        .parse()
        .unwrap();
    let validation = Validator::new(schema).validate(&policies, ValidationMode::Strict);
    assert!(validation.validation_passed(), "{validation:?}");
}
