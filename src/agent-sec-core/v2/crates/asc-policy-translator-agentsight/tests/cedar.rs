use asc_policy_translator_agentsight::{
    AgentSightBindingPlan, AgentSightTranslator, FILE_PROTECTION_POLICY_MEDIA_TYPE,
};
use asc_policy_types::{binding::PreparedBinding, target::TranslationOutcome};
use serde_json::{Value, json};

fn binding(text: &str) -> PreparedBinding {
    let mut binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    binding.policy.template = serde_json::from_value(json!({"language":"cedar","content":{
        "schema":{"schemaId":"seccore.system","revision":1},"text":text
    }}))
    .unwrap();
    binding
}

fn rule(condition: &str) -> String {
    format!(
        "forbid(principal, action == SecCore::Action::\"file.write\", resource is SecCore::File) when {{ {condition} }};"
    )
}

fn translate(text: &str) -> Value {
    let TranslationOutcome::Translated(plan) =
        AgentSightTranslator.translate(&binding(text)).unwrap()
    else {
        panic!("expected structured policy: {text}");
    };
    let plan: AgentSightBindingPlan = serde_json::from_slice(&plan.content).unwrap();
    assert_eq!(plan.policy.media_type, FILE_PROTECTION_POLICY_MEDIA_TYPE);
    serde_json::from_str(&plan.policy.content).unwrap()
}

#[test]
fn delete_and_write_keep_independent_scopes_and_merge_identical_targets() {
    let text = format!(
        "{}{}{}",
        rule(
            r#"context.input.operation == SecCore::WriteOperation::"delete" && resource.path == "/config""#
        ),
        rule(
            r#"context.input.operation == SecCore::WriteOperation::"write" && resource.path == "/config""#
        ),
        rule(
            r#"context.input.operation == SecCore::WriteOperation::"delete" && resource.path == "/workspace""#
        )
    );
    let policy = translate(&text);
    assert_eq!(policy["version"], 1);
    assert_eq!(policy["targets"].as_array().unwrap().len(), 2);
    assert_eq!(
        policy["targets"][0]["protections"],
        json!({"delete":true,"ordinary_write":true})
    );
    assert_eq!(
        policy["targets"][1]["protections"],
        json!({"delete":true,"ordinary_write":false})
    );
    assert_eq!(translate(&text), policy);
}

#[test]
fn literal_sets_and_unless_preserve_directory_and_entry_exclusions() {
    let text = r#"forbid(principal, action in [SecCore::Action::"file.write"], resource)
        when { [SecCore::WriteOperation::"write", SecCore::WriteOperation::"delete"].contains(context.input.operation)
            && (resource.path == "/workspace" || resource.path like "/workspace/*") }
        unless { resource.path == "/workspace/cache" || resource.path like "/workspace/cache/*"
            || ["/workspace/a", "/workspace/b"].contains(resource.path) };"#;
    let policy = translate(text);
    assert_eq!(policy["targets"].as_array().unwrap().len(), 1);
    let target = &policy["targets"][0];
    assert_eq!(target["kind"], "directory_tree");
    assert_eq!(target["exclusions"].as_array().unwrap().len(), 3);
    assert_eq!(target["exclusions"][0]["kind"], "exact_entry");
    assert_eq!(target["exclusions"][2]["kind"], "directory_subtree");
    assert_eq!(
        target["protections"],
        json!({"delete":true,"ordinary_write":true})
    );
}

#[test]
fn structured_output_matches_the_clients_v1_contract_fixture() {
    let source = r#"
        forbid(principal, action == SecCore::Action::"file.write", resource)
        when { context.input.operation == SecCore::WriteOperation::"write" && resource.path == "/config" };
        forbid(principal, action == SecCore::Action::"file.write", resource)
        when { [SecCore::WriteOperation::"delete", SecCore::WriteOperation::"write"].contains(context.input.operation)
            && (resource.path == "/workspace" || resource.path like "/workspace/*") }
        unless { resource.path == "/workspace/cache" || resource.path like "/workspace/cache/*"
            || resource.path == "/workspace/state" };
    "#;
    let expected: Value = serde_json::from_str(include_str!(
        "../../../fixtures/clients/agentsight/file-protection-v1.json"
    ))
    .unwrap();
    assert_eq!(translate(source), expected);
}

#[test]
fn unsupported_conditions_reject_the_whole_policy_including_later_statements() {
    let supported = rule(
        r#"context.input.operation == SecCore::WriteOperation::"delete" && resource.path == "/safe""#,
    );
    for condition in [
        r#"context.input.operation == SecCore::WriteOperation::"delete" && context.input.program == "/usr/bin/rm" && resource.path == "/a""#,
        r#"context.input.operation == SecCore::WriteOperation::"rename" && resource.path == "/a""#,
        r#"context.input.operation == SecCore::WriteOperation::"create" && resource.path == "/a""#,
        r#"context.input.operation == SecCore::WriteOperation::"setattr" && context.input has mode && context.input.mode == 0 && resource.path == "/a""#,
        r#"context.input.operation == SecCore::WriteOperation::"write" && resource.path like "/workspace/*""#,
        r#"context.input.operation == SecCore::WriteOperation::"write" && resource.path like "/workspace/*.log""#,
        r#"context.input.operation == SecCore::WriteOperation::"write" && resource.path == "relative""#,
        r#"context.input.operation == SecCore::WriteOperation::"write" && resource.path == "/a/../b""#,
        r#"context.input.operation != SecCore::WriteOperation::"write" && resource.path == "/a""#,
        r#"resource.path == "/a""#,
    ] {
        let outcome = AgentSightTranslator
            .translate(&binding(&format!("{supported}{}", rule(condition))))
            .unwrap();
        let TranslationOutcome::Rejected(error) = outcome else {
            panic!("must reject {condition}");
        };
        assert!(error.code.starts_with("CEDAR_RULE_1_"), "{}", error.code);
    }
    for text in [
        rule(r#"false && context.input.program == "/usr/bin/rm""#),
        rule("[].contains(context.input.program)"),
        supported.replace("forbid", "permit"),
        supported.replace("resource is SecCore::File)", "resource == SecCore::File::\"a\")"),
        "forbid(principal, action == SecCore::Action::\"file.read\", resource) when { resource.path == \"/a\" };".into(),
    ] {
        assert!(matches!(AgentSightTranslator.translate(&binding(&text)).unwrap(), TranslationOutcome::Rejected(_)));
    }
}

#[test]
fn runtime_only_rules_reject_the_whole_agentsight_binding() {
    let base = rule(
        r#"context.input.operation == SecCore::WriteOperation::"write" && resource.path == "/config""#,
    );
    for runtime_rule in [
        base.replace("forbid(principal,", r#"forbid(principal == SecCore::Subject::"worker","#),
        base.replace("when {", r#"when { principal == SecCore::Subject::"worker" &&"#),
        format!(r#"@id("protected") {base}"#),
        r#"@advice("{\"verdict\":\"escalate\"}") permit(principal, action == SecCore::Action::"process.spawn", resource);"#.into(),
    ] {
        let combined = format!("{base}\n{runtime_rule}");
        let input = binding(&combined);
        let asc_policy_types::authoring::PolicySource::Cedar(source) = &input.policy.template else {
            unreachable!()
        };
        asc_policy_cedar::validate(source).unwrap();
        assert!(matches!(AgentSightTranslator.translate(&input).unwrap(), TranslationOutcome::Rejected(_)), "{runtime_rule}");
    }
}

#[test]
fn conflicts_limits_empty_output_and_unknown_schema_fail_without_a_plan() {
    let base = r#"context.input.operation == SecCore::WriteOperation::"write" && (resource.path == "/a" || resource.path like "/a/*")"#;
    for text in [
        format!(
            "{}{}",
            rule(base),
            rule(&format!("{base} && resource.path != \"/a/cache\""))
        ),
        rule(&format!("{base} && resource.path != \"/a\"")),
        rule(&format!("{base} && false")),
        rule(&format!(
            r#"context.input.operation == SecCore::WriteOperation::"write" && [{}].contains(resource.path)"#,
            (0..65)
                .map(|i| format!("\"/file{i}\""))
                .collect::<Vec<_>>()
                .join(",")
        )),
    ] {
        assert!(
            matches!(
                AgentSightTranslator.translate(&binding(&text)).unwrap(),
                TranslationOutcome::Rejected(_)
            ),
            "{text}"
        );
    }
    let mut input = binding(&rule(base));
    let asc_policy_types::authoring::PolicySource::Cedar(source) = &mut input.policy.template
    else {
        unreachable!()
    };
    source.schema.revision = asc_policy_types::identifiers::Revision::new(2).unwrap();
    assert!(matches!(
        AgentSightTranslator.translate(&input).unwrap(),
        TranslationOutcome::Rejected(_)
    ));
}

#[test]
fn boolean_lowering_matches_cedar_for_supported_request_conditions() {
    let source = r#"forbid(principal, action == SecCore::Action::"file.write", resource)
        when {
            (context.input.operation == SecCore::WriteOperation::"delete" && (resource.path == "/tree" || resource.path like "/tree/*") && resource.path != "/tree/cache")
            || (context.input.operation == SecCore::WriteOperation::"write" && ["/tree/cache", "/config"].contains(resource.path))
        };
        forbid(principal, action == SecCore::Action::"file.write", resource)
        when { context.input.operation == SecCore::WriteOperation::"write" && (resource.path == "/other" || resource.path like "/other/*") }
        unless { resource.path == "/other/cache" || resource.path like "/other/cache/*" };"#;
    let lowered = translate(source);
    // Baseline permit isolates forbid matching from Cedar's default-deny rule.
    let policies: cedar_policy::PolicySet = format!("permit(principal, action, resource);{source}")
        .parse()
        .unwrap();
    let authorizer = cedar_policy::Authorizer::new();
    for path in [
        "/tree",
        "/tree/new",
        "/tree/cache",
        "/treehouse",
        "/config",
        "/other",
        "/other/cache",
        "/other/cache/new",
        "/other/new",
        "/outside",
    ] {
        let entities = cedar_policy::Entities::from_json_value(
            json!([{
                "uid":{"type":"SecCore::File","id":"request"}, "attrs":{"path":path},"parents":[]
            }]),
            None,
        )
        .unwrap();
        for operation in ["delete", "write", "create", "rename", "setattr"] {
            let context = cedar_policy::Context::from_json_value(json!({"input":{
                "program":"/bin/test", "operation":{"__entity":{"type":"SecCore::WriteOperation","id":operation}}
            }}), None).unwrap();
            let request = cedar_policy::Request::new(
                "SecCore::Subject::\"test\"".parse().unwrap(),
                "SecCore::Action::\"file.write\"".parse().unwrap(),
                "SecCore::File::\"request\"".parse().unwrap(),
                context,
                None,
            )
            .unwrap();
            let denied = authorizer
                .is_authorized(&request, &policies, &entities)
                .decision()
                == cedar_policy::Decision::Deny;
            let blocked = lowered["targets"].as_array().unwrap().iter().any(|target| {
                let active = match operation {
                    "delete" => target["protections"]["delete"].as_bool().unwrap(),
                    "write" => target["protections"]["ordinary_write"].as_bool().unwrap(),
                    _ => false,
                };
                let matches = |root: &str, tree: bool| {
                    path == root || tree && path.starts_with(&format!("{root}/"))
                };
                active
                    && matches(
                        target["path"].as_str().unwrap(),
                        target["kind"] == "directory_tree",
                    )
                    && !target["exclusions"].as_array().unwrap().iter().any(|ex| {
                        matches(
                            ex["path"].as_str().unwrap(),
                            ex["kind"] == "directory_subtree",
                        )
                    })
            });
            assert_eq!(blocked, denied, "{operation} {path}");
        }
    }
}
