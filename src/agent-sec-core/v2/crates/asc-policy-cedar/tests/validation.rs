use asc_policy_cedar::http::compile as credential_rules;
use asc_policy_cedar::schema::{SCHEMA_ID, SCHEMA_REVISION, SCHEMA_TEXT};
use asc_policy_cedar::{CedarError, PolicyDomain, project_pst, pst, validate};
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
fn retired_agent_type_is_rejected_before_cedar_can_treat_it_as_always_false() {
    for condition in [
        "principal has agent_type",
        r#"principal has "agent_type" && principal["agent_type"] == "cosh""#,
        r#"principal.agent_type == "cosh""#,
    ] {
        let input = source(&format!(
            r#"forbid(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource)
                   when {{ {condition} }};"#
        ));
        assert!(matches!(
            validate(&input),
            Err(CedarError::LegacyAgentType { statement_id }) if statement_id == "policy0"
        ));
    }
    let supported = source(
        r#"// Legacy agent_type may be mentioned in a comment.
            forbid(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource)
            when { principal has agentType && principal.agentType == "agent_type" };"#,
    );
    assert!(validate(&supported).is_ok());
}

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
    unknown_schema.schema.revision = Revision::new(SCHEMA_REVISION + 1).unwrap();
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
    input.schema.revision = Revision::new(SCHEMA_REVISION + 1).unwrap();
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
fn accepts_principal_logic_in_headers_and_nested_conditions() {
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
        validate(&source(text)).unwrap_or_else(|error| panic!("{text}: {error:?}"));
    }
}

#[test]
fn rejects_templates_and_annotations_without_an_execution_contract() {
    for text in [
        "permit(principal == ?principal, action, resource);",
        "permit(principal, action, resource == ?resource);",
        r#"@advice("require_confirmation") permit(principal, action, resource);"#,
        r#"@unknown("label") permit(principal, action, resource);"#,
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

fn http_source(annotations: &str, condition: &str) -> CedarPolicySource {
    let mut input = source(&format!(
        "{annotations}\npermit(principal, action == SecCore::Action::\"network.send\", resource is SecCore::HttpEndpoint)\nwhen {{ {condition} }};"
    ));
    input.schema.revision = Revision::new(SCHEMA_REVISION).unwrap();
    input
}

const CREDENTIAL_ANNOTATIONS: &str = r#"
@modify("inject_credential")
@real_token_file("/secure/real")
@fake_token_file("/secure/fake")
@inject_header("Authorization")
@inject_format("auto")
@response_rewrite("true")
"#;
const HTTP_CONDITION: &str =
    r#"resource.host == "api.example.com" && context.input.scheme == "https""#;

#[test]
fn credential_rules_share_admission_and_do_not_read_token_files() {
    use asc_policy_cedar::http::{CredentialPath, CredentialRule};
    use asc_policy_types::authoring::HttpMethod;
    let input = http_source(CREDENTIAL_ANNOTATIONS, HTTP_CONDITION);
    assert!(validate(&input).is_ok());
    assert!(pst(&input).is_ok());
    assert_eq!(
        credential_rules(&input).unwrap(),
        vec![CredentialRule {
            rule_id: "policy0".into(),
            real_token_file: "/secure/real".into(),
            fake_token_file: "/secure/fake".into(),
            header: "Authorization".into(),
            format: "auto".into(),
            response_rewrite: true,
            host: "api.example.com".into(),
            methods: vec![
                HttpMethod::Get,
                HttpMethod::Head,
                HttpMethod::Post,
                HttpMethod::Put,
                HttpMethod::Patch,
                HttpMethod::Delete
            ],
            path: None,
        }]
    );
    let disabled = http_source(
        &CREDENTIAL_ANNOTATIONS.replace(
            r#"@response_rewrite("true")"#,
            r#"@response_rewrite("false")"#,
        ),
        HTTP_CONDITION,
    );
    let mut expected = credential_rules(&input).unwrap();
    expected[0].response_rewrite = false;
    assert_eq!(credential_rules(&disabled).unwrap(), expected);
    for (expression, expected) in [
        (
            r#"context.input.path == "/api/a*b""#,
            CredentialPath {
                kind: "eq".into(),
                value: "/api/a*b".into(),
            },
        ),
        (
            r#"context.input.path like "/a*b/*""#,
            CredentialPath {
                kind: "like".into(),
                value: "/a*b/*".into(),
            },
        ),
        (
            r#"context.input.path like "/literal\*/*""#,
            CredentialPath {
                kind: "like".into(),
                value: r"/literal\*/*".into(),
            },
        ),
    ] {
        let input = http_source(
            CREDENTIAL_ANNOTATIONS,
            &format!("{HTTP_CONDITION} && {expression}"),
        );
        assert_eq!(credential_rules(&input).unwrap()[0].path, Some(expected));
    }
    assert!(credential_rules(&source(DELETE_POLICY)).unwrap().is_empty());
}

#[test]
fn method_constraints_and_custom_authentication_formats_lower_exactly() {
    use asc_policy_types::authoring::HttpMethod;
    for (condition, methods) in [
        (r#"context.input.method == "POST""#, vec![HttpMethod::Post]),
        (
            r#"["DELETE", "GET", "PATCH"].contains(context.input.method)"#,
            vec![HttpMethod::Get, HttpMethod::Patch, HttpMethod::Delete],
        ),
    ] {
        for format in [
            "auto",
            "basic",
            "Bearer {fake}",
            "{fake}",
            "prefix-{fake}-suffix",
        ] {
            let annotations = CREDENTIAL_ANNOTATIONS
                .replace("\"auto\"", &serde_json::to_string(format).unwrap())
                .replace("Authorization", "X-Api-Key");
            let input = http_source(&annotations, &format!("{HTTP_CONDITION} && {condition}"));
            let rules = credential_rules(&input).unwrap();
            assert_eq!(rules[0].methods, methods);
            assert_eq!(rules[0].header, "X-Api-Key");
            assert_eq!(rules[0].format, format);
        }
    }
}

#[test]
fn http_lowering_checks_annotation_contracts_after_common_validation() {
    let mut annotations = Vec::new();
    for name in [
        "modify",
        "real_token_file",
        "fake_token_file",
        "inject_header",
        "inject_format",
        "response_rewrite",
    ] {
        annotations.push(
            CREDENTIAL_ANNOTATIONS
                .lines()
                .filter(|line| !line.starts_with(&format!("@{name}(")))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    for path in [
        "relative",
        "~/real",
        "/$HOME/real",
        "/a/./b",
        "/a/../b",
        "/a//b",
        "/a/",
        "/",
        "/a\0b",
        "/a\nb",
        "/secure/fake",
    ] {
        annotations.push(
            CREDENTIAL_ANNOTATIONS
                .replace("\"/secure/real\"", &serde_json::to_string(path).unwrap()),
        );
    }
    for path in ["relative", "/a//b", "/$TOKEN/fake", "/a\0b"] {
        annotations.push(
            CREDENTIAL_ANNOTATIONS
                .replace("\"/secure/fake\"", &serde_json::to_string(path).unwrap()),
        );
    }
    for header in [
        "Host",
        "host",
        "Content-Length",
        "Transfer-Encoding",
        "Connection",
        "TE",
        "Trailer",
        "Upgrade",
        "Proxy-Connection",
        "Keep-Alive",
        "Expect",
        "bad header",
        "X-Key\r\nHost",
        "",
    ] {
        annotations.push(
            CREDENTIAL_ANNOTATIONS
                .replace("\"Authorization\"", &serde_json::to_string(header).unwrap()),
        );
    }
    for format in [
        "",
        "Bearer",
        "{real}",
        "{fake}{fake}",
        "{fake}{other}",
        "{fake}\n",
        "{fake}\0",
    ] {
        annotations.push(
            CREDENTIAL_ANNOTATIONS.replace("\"auto\"", &serde_json::to_string(format).unwrap()),
        );
    }
    annotations.push(CREDENTIAL_ANNOTATIONS.replace("inject_credential", "other"));
    for rewrite in [
        "buffered",
        "none",
        "streaming",
        "True",
        "False",
        "1",
        "",
        " true ",
    ] {
        annotations.push(CREDENTIAL_ANNOTATIONS.replace(
            r#"@response_rewrite("true")"#,
            &format!(
                "@response_rewrite({})",
                serde_json::to_string(rewrite).unwrap()
            ),
        ));
    }
    for annotations in annotations {
        let input = http_source(&annotations.replace(r"\u0000", r"\u{0}"), HTTP_CONDITION);
        validate(&input).unwrap();
        assert!(credential_rules(&input).is_err(), "{annotations}");
    }
}

#[test]
fn credentials_reject_overlapping_rules_principals_and_every_unconsumed_condition() {
    let valid = http_source(CREDENTIAL_ANNOTATIONS, HTTP_CONDITION);
    let variants = [
        valid.text.replace("permit(", "forbid("),
        valid
            .text
            .replace("principal,", r#"principal == SecCore::Subject::"worker","#),
        valid
            .text
            .replace("principal,", "principal is SecCore::Subject,"),
        valid.text.replace("network.send", "network.use"),
        valid
            .text
            .replace("resource is SecCore::HttpEndpoint", "resource"),
        valid.text.replace("when", "unless"),
        format!(r#"@advice("{{\"verdict\":\"warn\"}}") {}"#, valid.text),
        format!(
            "{}\nforbid(principal, action == SecCore::Action::\"network.send\", resource);",
            valid.text
        ),
        format!("{}\npermit(principal, action, resource);", valid.text),
        format!("{}\nforbid(principal, action, resource);", valid.text),
        valid.text.replace("api.example.com", "Api.example.com"),
        valid.text.replace("api.example.com", "*.example.com"),
        valid.text.replace("api.example.com", "127.0.0.1"),
        valid.text.replace("https", "http"),
    ];
    for text in variants {
        let mut input = valid.clone();
        input.text = text;
        assert!(
            pst(&input).map_or(true, |syntax| asc_policy_cedar::http::lower(
                &syntax,
                &input.schema
            )
            .is_err()),
            "{}",
            input.text
        );
    }
    for extra in [
        "true",
        "false",
        "principal has agentType",
        r#"principal == SecCore::Subject::"worker""#,
        r#"context.input.path != "/secret""#,
        r#"context.input.path like "relative*""#,
        r#"context.input.path == "/a%2fb""#,
        r#"context.input.path == "/a%5Cb""#,
        r#"context.input.path == "/a%2eb""#,
        r#"context.input.path == "/a/../b""#,
        r#"context.input.path == "/a?x""#,
        r#"context.input.path == "/a#x""#,
        r#"context.input.path == "/a%xx""#,
        r#"context.input.path == "/a//b""#,
        r#"context.input.method == "OPTIONS""#,
        "[].contains(context.input.method)",
        r#"["GET", "GET"].contains(context.input.method)"#,
        "[context.input.method].contains(context.input.method)",
        r#"(context.input.method == "GET" || context.input.method == "POST")"#,
        r#"!(context.input.method == "POST")"#,
        r#"if true then context.input.method == "GET" else false"#,
        r#"resource.host == "api.example.com""#,
        r#"context.input.scheme == "https""#,
        r#"{r: resource}.r.host == "api.example.com""#,
        r#"context.input.method == "GET" && context.input.method == "POST""#,
        r#"context.input.path == "/a" && context.input.path like "/*""#,
    ] {
        let input = http_source(
            CREDENTIAL_ANNOTATIONS,
            &format!("{HTTP_CONDITION} && {extra}"),
        );
        assert!(
            pst(&input).map_or(true, |syntax| asc_policy_cedar::http::lower(
                &syntax,
                &input.schema
            )
            .is_err()),
            "{extra}"
        );
    }
    assert!(
        credential_rules(&http_source(
            CREDENTIAL_ANNOTATIONS,
            r#"context.input.scheme == "https""#
        ))
        .is_err()
    );
    assert_eq!(
        credential_rules(&http_source(
            CREDENTIAL_ANNOTATIONS,
            r#"resource.host == "api.example.com""#
        ))
        .unwrap(),
        credential_rules(&valid).unwrap(),
    );
}

#[test]
fn mixed_credentials_and_file_forbids_preserve_both_complete_views() {
    let http = http_source(CREDENTIAL_ANNOTATIONS, HTTP_CONDITION);
    let expected = credential_rules(&http).unwrap();
    assert!(project_pst(&pst(&http).unwrap(), PolicyDomain::File).is_none());
    for text in [
        format!("{}\n{DELETE_POLICY}", http.text),
        format!("{DELETE_POLICY}\n{}", http.text),
    ] {
        let mut mixed = http.clone();
        mixed.text = text.clone();
        validate(&mixed).unwrap();
        let complete = pst(&mixed).unwrap();
        let original_syntax = complete.clone();
        assert_eq!(complete.policies.len(), 2);
        let rules = credential_rules(&mixed).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].host, expected[0].host);
        let files = project_pst(&complete, PolicyDomain::File).unwrap();
        let network = project_pst(&complete, PolicyDomain::Network).unwrap();
        assert_eq!(files.policies.len(), 1);
        assert_eq!(network.policies.len(), 1);
        assert_eq!(
            asc_policy_cedar::http::lower(&network, &mixed.schema).unwrap(),
            rules
        );
        for view in [&files, &network] {
            for (id, statement) in &view.policies {
                assert_eq!(complete.policies.get(id), Some(statement));
            }
        }
        assert_eq!(
            complete, original_syntax,
            "both views borrow the same unchanged PST"
        );
        let files = PolicySet::from_pst(files).unwrap();
        let original: PolicySet = mixed.text.parse().unwrap();
        let http = original
            .policies()
            .find(|policy| policy.annotation("modify").is_some())
            .unwrap();
        assert_eq!(rules[0].rule_id, http.id().as_ref());
        let file = files.policies().next().unwrap();
        assert_eq!(file, original.policy(file.id()).unwrap());
        assert_eq!(mixed.text, text);
    }
}

#[test]
fn network_views_include_permissions_and_restrictions_without_credentials() {
    for text in [
        r#"permit(principal, action == SecCore::Action::"network.use", resource == SecCore::Network::"all");"#,
        r#"forbid(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
           when { resource.host == "api.example.com" };"#,
    ] {
        let mut input = source(text);
        input.schema.revision = Revision::new(SCHEMA_REVISION).unwrap();
        let syntax = pst(&input).unwrap();
        let view = project_pst(&syntax, PolicyDomain::Network).unwrap();
        assert_eq!(view, syntax);
        assert!(project_pst(&syntax, PolicyDomain::File).is_none());
        assert!(credential_rules(&input).unwrap().is_empty());
        assert!(asc_policy_cedar::http::lower(&view, &input.schema).is_err());
    }
}

#[test]
fn ambiguous_actions_preserve_restrictions_in_both_domain_views() {
    for text in [
        "forbid(principal, action, resource);",
        r#"forbid(principal, action in [SecCore::Action::"file.read", SecCore::Action::"network.use"], resource);"#,
    ] {
        let input = source(text);
        let complete = pst(&input).unwrap();
        for domain in [PolicyDomain::File, PolicyDomain::Network] {
            assert_eq!(project_pst(&complete, domain).unwrap(), complete);
        }
    }
}

#[test]
fn manual_file_bindings_keep_subject_specific_http_guards_in_the_scope_network_view() {
    for endpoint in [
        "resource is SecCore::HttpEndpoint",
        r#"resource == SecCore::HttpEndpoint::"remote""#,
        r#"resource in SecCore::HttpEndpoint::"remote""#,
        r#"resource is SecCore::HttpEndpoint in SecCore::HttpEndpoint::"remote""#,
    ] {
        let guard = format!(
            r#"forbid(principal, action, {endpoint})
                when {{ principal has agentType && principal.agentType == "cosh" }};"#
        );
        let syntax = pst(&source(&format!("{DELETE_POLICY}\n{guard}"))).unwrap();
        let (file, network) = asc_policy_cedar::manual_binding_views(&syntax, false);
        assert_eq!(file.unwrap().policies.len(), 1, "{endpoint}");
        assert!(network.is_none(), "{endpoint}");
        let scope_network = project_pst(&syntax, PolicyDomain::Network).unwrap();
        assert_eq!(scope_network.policies.len(), 1);
        assert_eq!(
            asc_policy_cedar::manual_binding_views(&syntax, true).1,
            Some(scope_network)
        );
    }
    // A bare HTTP restriction is itself manually assigned even without a grant.
    let bare = "forbid(principal, action, resource is SecCore::HttpEndpoint);";
    let syntax = pst(&source(&format!("{DELETE_POLICY}\n{bare}"))).unwrap();
    let (file, network) = asc_policy_cedar::manual_binding_views(&syntax, false);
    assert_eq!(file.unwrap().policies.len(), 1);
    assert_eq!(network.unwrap().policies.len(), 1);
}

#[test]
fn http_annotations_cannot_escape_through_file_or_request_authorization_views() {
    let input = source(&format!("{CREDENTIAL_ANNOTATIONS}\n{DELETE_POLICY}"));
    assert!(project_pst(&pst(&input).unwrap(), PolicyDomain::Network).is_none());
    assert!(credential_rules(&input).is_err());
    let mut input = http_source(CREDENTIAL_ANNOTATIONS, HTTP_CONDITION);
    input.text = input
        .text
        .replace("principal,", "principal is SecCore::Subject,");
    validate(&input).unwrap();
    assert!(!asc_policy_cedar::request_authorization_eligible(&input).unwrap());
}

#[test]
fn rule_identity_is_stable_across_sdk_and_text_order_and_rejects_collisions() {
    let a = http_source(
        &format!("@id(\"a\")\n{CREDENTIAL_ANNOTATIONS}"),
        HTTP_CONDITION,
    );
    let b = http_source(
        &format!(
            "@id(\"b\")\n{}",
            CREDENTIAL_ANNOTATIONS
                .replace("auto", "basic")
                .replace("/secure/real", "/secure/other-real")
        ),
        HTTP_CONDITION,
    );
    let mut input = a.clone();
    input.text = format!("{}\n{}", b.text, a.text);
    let expected = credential_rules(&input).unwrap();
    input.text = format!("{}\n{}", a.text, b.text);
    assert_eq!(credential_rules(&input).unwrap(), expected);
    assert_eq!(expected[0].rule_id, "a");
    assert_eq!(expected[0].format, "auto");
    assert_eq!(expected[1].rule_id, "b");
    assert_eq!(expected[1].real_token_file, "/secure/other-real");
    input.text = format!("{}\n{}", a.text, a.text);
    assert!(credential_rules(&input).is_err());
    let anonymous = http_source(CREDENTIAL_ANNOTATIONS, HTTP_CONDITION);
    input.text = format!(
        "{}\n{}",
        a.text.replace("@id(\"a\")", "@id(\"policy1\")"),
        anonymous.text
    );
    assert!(credential_rules(&input).is_err());
}

#[test]
fn credential_ast_limits_reject_the_whole_set_not_a_partial_lowering() {
    let base = http_source(CREDENTIAL_ANNOTATIONS, HTTP_CONDITION);
    let mut input = base.clone();
    input.text = base.text.repeat(256);
    assert_eq!(credential_rules(&input).unwrap().len(), 256);
    input.text.push_str(&base.text);
    assert!(credential_rules(&input).is_err());
    let large = http_source(
        CREDENTIAL_ANNOTATIONS,
        &format!(
            r#"{HTTP_CONDITION} && ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].contains(context.input.method)"#
        ),
    );
    input.text = large.text.repeat(256);
    assert!(matches!(
        credential_rules(&input),
        Err(CedarError::Unsupported {
            reason: "credential AST exceeds 16 levels or 4096 nodes",
            ..
        })
    ));
    let mut condition = HTTP_CONDITION.to_owned();
    for _ in 0..17 {
        condition = format!(r#"({condition}) && context.input.method == "GET""#);
    }
    assert!(matches!(
        credential_rules(&http_source(CREDENTIAL_ANNOTATIONS, &condition)),
        Err(CedarError::Unsupported { .. })
    ));
    for condition in [
        format!("{}{}{}", "(".repeat(65), HTTP_CONDITION, ")".repeat(65)),
        format!("{}true", "!".repeat(65)),
        format!("{}true", "!".repeat(1000)),
        format!(
            "{}true{}",
            "if true then ".repeat(65),
            " else false".repeat(65)
        ),
        "a".repeat(256 * 1024 + 1),
    ] {
        let input = http_source(CREDENTIAL_ANNOTATIONS, &condition);
        for result in [
            validate(&input).map(|_| ()),
            pst(&input).map(|_| ()),
            credential_rules(&input).map(|_| ()),
        ] {
            assert!(matches!(result, Err(CedarError::Unsupported { .. })));
        }
    }
    input = base.clone();
    input.text = format!("// {}\n{}", "(".repeat(1000), base.text);
    assert_eq!(
        credential_rules(&input).unwrap(),
        credential_rules(&base).unwrap()
    );
}

#[test]
fn shared_source_budget_accepts_boundary_inputs_and_rejects_overflow() {
    let paths: Vec<_> = (0..242).map(|index| format!("/p{index}")).collect();
    let text = format!(
        r#"permit(principal, action == SecCore::Action::"file.read", resource)
when {{ {}.contains(resource.path) }};"#,
        json!(paths),
    );
    {
        let mut input = source(&text);
        input.text.push_str(&" ".repeat(256 * 1024 - text.len()));
        assert!(validate(&input).is_ok());
        assert!(pst(&input).is_ok());
        assert!(credential_rules(&input).unwrap().is_empty());
        let overflow = text.replace("].contains", ",\"/overflow\"].contains");
        for text in [format!("{} ", input.text), overflow] {
            input.text = text;
            for result in [
                validate(&input).map(|_| ()),
                pst(&input).map(|_| ()),
                credential_rules(&input).map(|_| ()),
            ] {
                assert!(matches!(
                    result,
                    Err(CedarError::Unsupported { reason, .. }) if reason.contains("source exceeds")
                ));
            }
        }
    }
}

#[test]
fn canonical_schema_includes_http_and_rejects_retired_revisions() {
    assert_eq!(
        asc_policy_cedar::schema::embedded_reference()
            .revision
            .get(),
        3
    );
    let mut input = http_source(CREDENTIAL_ANNOTATIONS, HTTP_CONDITION);
    assert!(credential_rules(&input).is_ok());
    for revision in [1, 2] {
        input.schema.revision = Revision::new(revision).unwrap();
        assert!(matches!(
            credential_rules(&input),
            Err(CedarError::UnsupportedSchema { .. })
        ));
    }
    let action: EntityUid = r#"SecCore::Action::"network.send""#.parse().unwrap();
    let schema =
        asc_policy_cedar::schema::load_schema(&asc_policy_cedar::schema::embedded_reference())
            .unwrap();
    let context = Context::from_json_value(
        json!({"input":{"scheme":"https","method":"GET","path":"/a/b"}}),
        Some((&schema, &action)),
    )
    .unwrap();
    let request = Request::new(
        r#"SecCore::Subject::"worker""#.parse().unwrap(),
        action.clone(),
        r#"SecCore::HttpEndpoint::"endpoint""#.parse().unwrap(),
        context,
        Some(&schema),
    );
    assert!(request.is_ok());
    assert!(
        Context::from_json_value(
            json!({"input":{"scheme":"https","method":"GET"}}),
            Some((&schema, &action))
        )
        .is_err()
    );
}

#[test]
fn shared_gateway_path_matrix_agrees_with_sdk_authorization() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/gateway/decisions.json")).unwrap();
    for case in cases["pathCases"].as_array().unwrap() {
        let kind = case["constraint"]["kind"].as_str().unwrap();
        let value = case["constraint"]["value"].as_str().unwrap();
        let literal = if kind == "like" {
            format!("\"{}\"", value.replace('"', "\\\""))
        } else {
            serde_json::to_string(value).unwrap()
        };
        let operator = if kind == "like" { "like" } else { "==" };
        let source = http_source(
            CREDENTIAL_ANNOTATIONS,
            &format!("{HTTP_CONDITION} && context.input.path {operator} {literal}"),
        );
        let policies = PolicySet::from_pst(pst(&source).unwrap()).unwrap();
        let schema = asc_policy_cedar::schema::load_schema(&source.schema).unwrap();
        let action: EntityUid = r#"SecCore::Action::"network.send""#.parse().unwrap();
        let entities = Entities::from_json_value(
            json!([{
                "uid": {"type": "SecCore::HttpEndpoint", "id": "endpoint"},
                "attrs": {"host": "api.example.com"}, "parents": []
            }]),
            Some(&schema),
        )
        .unwrap();
        let context = Context::from_json_value(
            json!({"input": {
                "scheme": "https", "method": "GET", "path": case["path"]
            }}),
            Some((&schema, &action)),
        )
        .unwrap();
        let request = Request::new(
            r#"SecCore::Subject::"worker""#.parse().unwrap(),
            action,
            r#"SecCore::HttpEndpoint::"endpoint""#.parse().unwrap(),
            context,
            Some(&schema),
        )
        .unwrap();
        let response = Authorizer::new().is_authorized(&request, &policies, &entities);
        assert_eq!(response.diagnostics().errors().count(), 0, "{case}");
        assert_eq!(
            response.decision() == Decision::Allow,
            case["matches"].as_bool().unwrap(),
            "{case}"
        );
    }
}

#[test]
fn v3_registers_only_credential_annotations_and_preserves_subjects_for_runtime() {
    let fixture = include_str!("../../../fixtures/policy/qoder-v3.cedar");
    let mut input = CedarPolicySource {
        schema: asc_policy_cedar::schema::embedded_reference(),
        text: format!(
            "{fixture}\nforbid(principal, action, resource is SecCore::HttpEndpoint) when {{ resource.host == \"api.github.com\" }};"
        ),
    };
    let rules = credential_rules(&input).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].rule_id, "qoder-github");
    assert_eq!(rules[0].real_token_file, "/secure/github-real");
    assert!(asc_policy_cedar::request_authorization_eligible(&input).unwrap());
    let syntax = pst(&input).unwrap();
    assert_eq!(
        syntax,
        input.text.parse::<PolicySet>().unwrap().to_pst().unwrap()
    );
    // A process selector is still present, and the unannotated network forbid still applies.
    let network = project_pst(&syntax, PolicyDomain::Network).unwrap();
    assert_eq!(network.policies.len(), 2);
    assert!(asc_policy_cedar::http::lower(&network, &input.schema).is_err());
    input.text = fixture.replace(r#"action == SecCore::Action::"network.send""#, "action");
    assert!(credential_rules(&input).is_err());
    assert!(asc_policy_cedar::request_authorization_eligible(&input).is_err());
}

#[test]
fn credential_registration_accepts_a_subject_alternative_without_authorizing_its_residual() {
    let fixture = include_str!("../../../fixtures/policy/qoder-v3.cedar");
    let mut source = CedarPolicySource {
        schema: asc_policy_cedar::schema::embedded_reference(),
        text: fixture.replace(
            "principal.agentType == \"qoder\" &&",
            "(principal.agentType == \"qoder\" || principal.agentType == \"cosh\") &&",
        ),
    };
    assert_eq!(credential_rules(&source).unwrap().len(), 1);
    let network = project_pst(&pst(&source).unwrap(), PolicyDomain::Network).unwrap();
    let statement = network.policies.values().next().unwrap();
    assert!(asc_policy_cedar::deployment::split_subject(statement).is_err());
    assert!(asc_policy_cedar::http::lower(&network, &source.schema).is_err());
    source.text=fixture.replace("resource.host == \"api.github.com\"", "(resource.host == \"api.github.com\" || (principal has agentType && principal.agentType == \"cosh\"))");
    assert!(credential_rules(&source).is_err());
}

#[test]
fn migration_preserves_request_effects_and_process_selectors() {
    use asc_policy_cedar::{
        deployment::statements, migration::upgrade_text, request_authorization_eligible,
    };

    let text = r#"forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
        when { resource.path == "/secret" };"#;
    let mut file = source(text);
    assert!(!request_authorization_eligible(&file).unwrap());
    file.text = upgrade_text(&file, 3, true).unwrap().unwrap();
    assert!(request_authorization_eligible(&file).unwrap());
    assert!(file.text.contains("principal is SecCore::Subject"));
    assert!(upgrade_text(&source(text), 2, true).unwrap().is_none());
    assert!(upgrade_text(&source(text), 3, false).unwrap().is_none());

    let mut old = source(
        r#"forbid(principal, action == SecCore::Action::"file.read", resource)
        when { principal has "agent_type" && principal["agent_type"] == "qoder" }
        when { resource.path == "/agent_type" };"#,
    );
    old.text = upgrade_text(&old, 1, true).unwrap().unwrap();
    assert!(old.text.contains("/agent_type"));
    let rules = statements(&old).unwrap();
    let target = rules[0].subject.as_ref().unwrap().as_ref().unwrap();
    assert_eq!(target.agent_type, "qoder");
    assert_eq!(target.owner_uid, None);
    assert!(!request_authorization_eligible(&old).unwrap());
}
