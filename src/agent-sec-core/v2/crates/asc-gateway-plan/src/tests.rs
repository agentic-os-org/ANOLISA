use asc_credential_store::ScopeId;
use asc_policy_types::authoring::PolicySource as AuthoredPolicySource;
use asc_policy_types::cedar::CedarPolicySource;
use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::policy::{CredentialBinding, PreparedPolicy};
use asc_policy_types::scope::PreparedScope;
use serde_json::{Value, json};

use crate::{Plan, PlanError, compile_scope};

const GITHUB_CEDAR: &str = include_str!("../policies/github.cedar");
const GITHUB_PLAN: &str = include_str!("../../../fixtures/gateway/github-dev-agent.plan.json");
const DECISIONS: &str = include_str!("../../../fixtures/gateway/decisions.json");
const SCOPE_ID: &str = "00000000-0000-4000-8000-000000000001";

fn github_policy() -> PreparedPolicy {
    let source = CedarPolicySource {
        schema: asc_policy_types::cedar::CedarSchemaReference {
            schema_id: ResourceId::new(asc_policy_cedar::schema::SCHEMA_ID).unwrap(),
            revision: Revision::new(asc_policy_cedar::schema::HTTP_SCHEMA_REVISION).unwrap(),
        },
        text: GITHUB_CEDAR.into(),
    };
    let credential_bindings = asc_policy_cedar::credential_rules(&source)
        .unwrap()
        .into_iter()
        .map(|rule| CredentialBinding {
            rule_id: rule.rule_id,
            pair_id: "github".into(),
        })
        .collect();
    PreparedPolicy {
        policy_id: ResourceId::new("policy:github").unwrap(),
        policy_name: "GitHub".into(),
        revision: Revision::new(7).unwrap(),
        template: AuthoredPolicySource::Cedar(source),
        credential_bindings,
    }
}

fn cedar_mut(policy: &mut PreparedPolicy) -> &mut CedarPolicySource {
    let AuthoredPolicySource::Cedar(source) = &mut policy.template else {
        panic!("expected Cedar test policy");
    };
    source
}

fn scope(policies: &[PreparedPolicy]) -> PreparedScope {
    serde_json::from_value(json!({
        "scopeId": SCOPE_ID, "ownerUid": 1000,
        "selector": {"kind":"process","match":{"processName":"agent"}},
        "policySnapshots": policies, "status":"ACTIVE"
    }))
    .unwrap()
}

fn github_plan() -> Plan {
    compile_scope(&scope(&[github_policy()])).unwrap()
}

fn fixture_value() -> Value {
    // Independent expected endpoint/carrier matrix, in canonical Cedar ID order.
    let matrix = [
        (
            "github.com",
            Some("/*/git-receive-pack"),
            "POST",
            "auto",
            true,
        ),
        (
            "github.com",
            Some("/*/git-receive-pack"),
            "POST",
            "basic",
            true,
        ),
        ("github.com", Some("/*/info/refs"), "GET", "auto", true),
        ("github.com", Some("/*/info/refs"), "GET", "basic", true),
        ("api.github.com", Some("/*"), "ALL", "auto", true),
        ("api.github.com", Some("/*"), "ALL", "basic", true),
        (
            "github.com",
            Some("/*/git-upload-pack"),
            "POST",
            "auto",
            true,
        ),
        (
            "github.com",
            Some("/*/git-upload-pack"),
            "POST",
            "basic",
            true,
        ),
    ];
    let rules: Vec<_> = matrix.into_iter().enumerate().map(|(index, (host, path, method, format, rewrite))| json!({
        "source": {"policyId":"policy:github", "policyRevision":7, "ruleIndex":index},
        "match": {"scheme":"https", "host":host,
            "path":path.map(|value| json!({"kind":"like", "value":value})),
            "methods":if method == "ALL" { vec!["GET","HEAD","POST","PUT","PATCH","DELETE"] } else { vec![method] },
            "credentialId":"github"},
        "header":"Authorization", "format":format, "responseRewrite":rewrite,
        "because":"Credential use allowed by policy"
    })).collect();
    json!({"format":"agent-sec-gateway-plan/v2", "scopeId":SCOPE_ID,
        "policies":[{"id":"policy:github","revision":7}], "credentials":["github"], "rules":rules})
}

#[derive(Debug, thiserror::Error)]
enum PlanDecodeError {
    #[error(transparent)]
    Decode(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] PlanError),
}

fn decode_plan(bytes: &[u8]) -> Result<Plan, PlanDecodeError> {
    let plan: Plan = serde_json::from_slice(bytes)?;
    plan.validate()?;
    Ok(plan)
}

fn mutated(edit: impl FnOnce(&mut Value)) -> Result<Plan, PlanDecodeError> {
    let mut value = fixture_value();
    edit(&mut value);
    decode_plan(&serde_json::to_vec(&value).unwrap())
}

#[test]
fn cedar_github_policy_compiles_all_eight_endpoint_carrier_rules() {
    let compiled = github_plan();
    assert_eq!(
        serde_json::from_slice::<Value>(&compiled.to_json()).unwrap(),
        fixture_value()
    );
    assert_eq!(decode_plan(&compiled.to_json()).unwrap(), compiled);
    let policy = github_policy();
    let AuthoredPolicySource::Cedar(source) = policy.template else {
        panic!("expected Cedar");
    };
    assert_eq!(source.text, GITHUB_CEDAR);
    // The shared addon fixture may order rules differently; its v2 wire contract must still decode.
    decode_plan(GITHUB_PLAN.as_bytes()).unwrap();
    let decisions: Value = serde_json::from_str(DECISIONS).unwrap();
    assert_eq!(decisions["plan"], "github-dev-agent.plan.json");
    assert_eq!(decisions["listeners"][0]["scopeId"], SCOPE_ID);
    let encoded = String::from_utf8(compiled.to_json()).unwrap();
    for forbidden in [
        "/secure/",
        "realTokenFile",
        "fakeTokenFile",
        "policyName",
        "credentialBindings",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn compilation_aggregates_snapshots_canonically_and_preserves_each_rule_source() {
    let first = github_policy();
    let mut second = first.clone();
    second.policy_id = ResourceId::new("policy:other").unwrap();
    second.revision = Revision::new(9).unwrap();
    for binding in &mut second.credential_bindings {
        binding.pair_id = "other-token".into();
    }
    second.credential_bindings.reverse();
    let file = PreparedPolicy {
        policy_id: ResourceId::new("policy:file").unwrap(), policy_name: "Files".into(), revision: Revision::new(1).unwrap(),
        template: serde_json::from_value(json!({"language":"cedar","content":{"schema":{"schemaId":"seccore.system","revision":1},"text":"forbid(principal, action == SecCore::Action::\"file.read\", resource) when { resource.path == \"/secret\" };"}})).unwrap(),
        credential_bindings: vec![],
    };
    let plan = compile_scope(&scope(&[second.clone(), file.clone(), first.clone()])).unwrap();
    assert_eq!(
        plan,
        compile_scope(&scope(&[file, first.clone(), second.clone()])).unwrap()
    );
    assert_eq!(plan.scope_id, ScopeId::new(SCOPE_ID).unwrap());
    assert_eq!(
        plan.policies,
        vec![
            crate::PolicySource {
                id: first.policy_id.clone(),
                revision: first.revision
            },
            crate::PolicySource {
                id: second.policy_id.clone(),
                revision: second.revision
            }
        ]
    );
    assert_eq!(
        plan.credentials
            .iter()
            .map(asc_credential_store::CredentialId::as_str)
            .collect::<Vec<_>>(),
        ["github", "other-token"]
    );
    for (index, rule) in plan.rules.iter().take(8).enumerate() {
        assert_eq!(
            rule.source,
            crate::RuleSource {
                policy_id: first.policy_id.clone(),
                policy_revision: first.revision,
                rule_index: index
            }
        );
    }
    assert_eq!(
        plan.rules.last().unwrap().source,
        crate::RuleSource {
            policy_id: second.policy_id,
            policy_revision: second.revision,
            rule_index: 7
        }
    );
    let mut projected = plan.clone();
    projected.rules.remove(0);
    projected.validate().unwrap();
    assert_eq!(decode_plan(&projected.to_json()).unwrap(), projected);
    assert_eq!(projected.rules, plan.rules[1..]);
    projected.rules.clear();
    projected.credentials.clear();
    projected.validate().unwrap();
}

#[test]
fn independent_noncredential_sources_are_skipped_but_cedar_still_validates() {
    let mut file = github_policy();
    file.policy_id = ResourceId::new("policy:file").unwrap();
    file.credential_bindings.clear();
    file.template = AuthoredPolicySource::SecCore(serde_json::from_value(json!({"specVersion":"0.1","rules":[{
        "effect":"block","category":"file","action":"read","target":{"type":"file","path":"/secret"}
    }]})).unwrap());
    assert_eq!(
        compile_scope(&scope(&[file.clone(), github_policy()])).unwrap(),
        github_plan()
    );
    assert!(matches!(
        compile_scope(&scope(&[file])),
        Err(PlanError::Invalid {
            field: "policies",
            ..
        })
    ));
    let mut cedar = github_policy();
    cedar.policy_id = ResourceId::new("policy:invalid-cedar").unwrap();
    cedar.credential_bindings.clear();
    cedar_mut(&mut cedar).text =
        "permit(principal, action, resource) when { context.typo };".into();
    assert!(matches!(
        compile_scope(&scope(&[cedar, github_policy()])),
        Err(PlanError::Cedar(_))
    ));
}

#[test]
fn invalid_or_mixed_policy_never_compiles_a_partial_plan() {
    for (from, to) in [
        ("permit(", "forbid("),
        ("network.send", "file.read"),
        (
            "resource is SecCore::HttpEndpoint",
            "resource is SecCore::File",
        ),
        (
            "context.input.method == \"POST\"",
            "context.input.method == \"OPTIONS\"",
        ),
    ] {
        let mut policy = github_policy();
        cedar_mut(&mut policy).text = cedar_mut(&mut policy).text.replacen(from, to, 1);
        assert!(compile_scope(&scope(&[policy])).is_err(), "{from} -> {to}");
    }
    let mut policy = github_policy();
    cedar_mut(&mut policy)
        .text
        .push_str("\nforbid(principal, action == SecCore::Action::\"file.read\", resource);");
    assert!(matches!(
        compile_scope(&scope(&[policy])),
        Err(PlanError::Cedar(_))
    ));
    let mut selected = scope(&[github_policy()]);
    selected.selector = asc_policy_types::scope::ScopeSelector::Pid { pid: 4242 };
    assert!(matches!(
        compile_scope(&selected),
        Err(PlanError::Policy(_))
    ));
}

#[test]
fn bindings_are_complete_unique_and_joined_by_rule_id_not_position() {
    let mut policy = github_policy();
    for (index, binding) in policy.credential_bindings.iter_mut().enumerate() {
        binding.pair_id = format!("pair-{index}");
    }
    let expected = compile_scope(&scope(std::slice::from_ref(&policy))).unwrap();
    policy.credential_bindings.reverse();
    assert_eq!(
        compile_scope(&scope(std::slice::from_ref(&policy))).unwrap(),
        expected
    );
    for (index, rule) in expected.rules.iter().enumerate() {
        assert_eq!(rule.matcher.credential_id.as_str(), format!("pair-{index}"));
    }
    for edit in [
        (|p: &mut PreparedPolicy| {
            p.credential_bindings.pop();
        }) as fn(&mut PreparedPolicy),
        |p: &mut PreparedPolicy| {
            p.credential_bindings.push(CredentialBinding {
                rule_id: "extra".into(),
                pair_id: "extra".into(),
            });
        },
        |p: &mut PreparedPolicy| {
            p.credential_bindings[0].rule_id = "unknown".into();
        },
        |p: &mut PreparedPolicy| {
            p.credential_bindings[0].rule_id = p.credential_bindings[1].rule_id.clone();
        },
        |p: &mut PreparedPolicy| {
            p.credential_bindings[0].pair_id = "Bad/Pair".into();
        },
    ] {
        let mut policy = github_policy();
        edit(&mut policy);
        assert!(compile_scope(&scope(&[policy])).is_err());
    }
    let mut file = github_policy();
    cedar_mut(&mut file).text = "permit(principal, action, resource);".into();
    assert!(compile_scope(&scope(&[file])).is_err());
}

fn repeated_policy(count: usize, policy_index: usize, unique_pairs: bool) -> PreparedPolicy {
    let mut policy = github_policy();
    policy.policy_id = ResourceId::new(format!("policy:{policy_index}")).unwrap();
    let annotations = "@modify(\"inject_credential\") @real_token_file(\"/secure/real\") @fake_token_file(\"/secure/fake\") @inject_header(\"Authorization\") @inject_format(\"auto\") @response_rewrite(\"true\")";
    cedar_mut(&mut policy).text = format!("{annotations} permit(principal, action == SecCore::Action::\"network.send\", resource is SecCore::HttpEndpoint) when {{ resource.host == \"api.example.com\" && context.input.scheme == \"https\" }};").repeat(count);
    let rules = asc_policy_cedar::credential_rules(cedar_mut(&mut policy)).unwrap();
    policy.credential_bindings = rules
        .into_iter()
        .enumerate()
        .map(|(i, rule)| CredentialBinding {
            rule_id: rule.rule_id,
            pair_id: if unique_pairs {
                format!("c-{policy_index}-{i}")
            } else {
                "github".into()
            },
        })
        .collect();
    policy
}

#[test]
fn compiler_enforces_aggregate_gateway_limits() {
    let policy = repeated_policy(256, 0, false);
    assert_eq!(
        compile_scope(&scope(std::slice::from_ref(&policy)))
            .unwrap()
            .rules
            .len(),
        256
    );
    let other = repeated_policy(1, 1, false);
    assert!(matches!(
        compile_scope(&scope(&[policy, other])),
        Err(PlanError::Invalid { field: "rules", .. })
    ));
    let policies: Vec<_> = (0..17)
        .map(|index| repeated_policy(4, index, true))
        .collect();
    assert!(matches!(
        compile_scope(&scope(&policies)),
        Err(PlanError::Invalid {
            field: "credentials",
            ..
        })
    ));
    let policies: Vec<_> = (0..33)
        .map(|index| repeated_policy(1, index, false))
        .collect();
    assert!(compile_scope(&scope(&policies)).is_err());
}

#[test]
fn plan_uses_exact_snapshots_not_policy_names_or_description() {
    let expected = github_plan();
    let mut policy = github_policy();
    policy.policy_name = "synthetic-private-context".into();
    cedar_mut(&mut policy)
        .text
        .insert_str(0, "// synthetic-private-context\n");
    assert_eq!(
        compile_scope(&scope(std::slice::from_ref(&policy))).unwrap(),
        expected
    );
    policy.revision = policy.revision.checked_next().unwrap();
    assert_ne!(compile_scope(&scope(&[policy])).unwrap(), expected);
}

#[test]
fn unknown_fields_and_formats_are_rejected() {
    for edit in [
        (|p: &mut Value| p["extra"] = true.into()) as fn(&mut Value),
        |p: &mut Value| p["format"] = "agent-sec-gateway-plan/v3".into(),
        |p: &mut Value| p["rules"][0]["match"]["methods"] = json!(["OPTIONS"]),
        |p: &mut Value| p["rules"][0]["match"]["scheme"] = "http".into(),
        |p: &mut Value| p["rules"][0]["match"]["path"] = "/**".into(),
        |p: &mut Value| p["rules"][0]["responseRewrite"] = "buffered".into(),
        |p: &mut Value| p["rules"][0]["responseRewrite"] = "none".into(),
        |p: &mut Value| p["rules"][0]["responseRewrite"] = "true".into(),
        |p: &mut Value| p["rules"][0]["responseRewrite"] = Value::Null,
        |p: &mut Value| {
            p["rules"][0].as_object_mut().unwrap().remove("header");
        },
        |p: &mut Value| {
            p["rules"][0].as_object_mut().unwrap().remove("format");
        },
        |p: &mut Value| {
            p["rules"][0]
                .as_object_mut()
                .unwrap()
                .remove("responseRewrite");
        },
    ] {
        assert!(matches!(mutated(edit), Err(PlanDecodeError::Decode(_))));
    }
    assert!(matches!(
        mutated(|p| p["format"] = "agent-sec-gateway-plan/v1".into()),
        Err(PlanDecodeError::Validation(_))
    ));
    let encoded = fixture_value().to_string();
    let duplicated = encoded.replacen(
        "\"scopeId\":",
        "\"scopeId\":\"other-scope\",\"scopeId\":",
        1,
    );
    assert_ne!(duplicated, encoded);
    assert!(matches!(
        decode_plan(duplicated.as_bytes()),
        Err(PlanDecodeError::Decode(_))
    ));
}

#[test]
fn rule_constraints_are_enforced() {
    for host in [
        "*.github.com",
        "GitHub.com",
        "localhost",
        "a..b",
        "-a.com",
        "127.0.0.1",
    ] {
        assert!(
            matches!(
                mutated(|p| p["rules"][0]["match"]["host"] = host.into()),
                Err(PlanDecodeError::Validation(PlanError::InvalidRule {
                    field: "match.host",
                    ..
                }))
            ),
            "{host}"
        );
    }
    for path in [
        json!({"kind":"eq","value":"repos"}),
        json!({"kind":"glob","value":"/*"}),
        json!({"kind":"like","value":"/a\nb"}),
        json!({"kind":"like","value":"/a\\"}),
        json!({"kind":"like","value":"/a\\x"}),
    ] {
        assert!(matches!(
            mutated(|p| p["rules"][0]["match"]["path"] = path),
            Err(PlanDecodeError::Validation(PlanError::InvalidRule {
                field: "match.path",
                ..
            }))
        ));
    }
    for path in [
        Value::Null,
        json!({"kind":"eq","value":"/a*b"}),
        json!({"kind":"like","value":"/a*b/**"}),
    ] {
        mutated(|p| p["rules"][0]["match"]["path"] = path).unwrap();
    }
    let disabled = mutated(|p| p["rules"][0]["responseRewrite"] = false.into()).unwrap();
    assert!(!disabled.rules[0].response_rewrite);
    assert_eq!(decode_plan(&disabled.to_json()).unwrap(), disabled);
    for (field, edit) in [
        (
            "match.methods",
            (|p: &mut Value| p["rules"][0]["match"]["methods"] = json!(["POST", "GET"]))
                as fn(&mut Value),
        ),
        ("match.methods", |p: &mut Value| {
            p["rules"][0]["match"]["methods"] = json!([]);
        }),
        ("match.methods", |p: &mut Value| {
            p["rules"][0]["match"]["methods"] = json!(["GET", "GET"]);
        }),
        ("match.credentialId", |p: &mut Value| {
            p["rules"][0]["match"]["credentialId"] = "other".into();
        }),
        ("header/format", |p: &mut Value| {
            p["rules"][0]["header"] = "Host".into();
        }),
        ("header/format", |p: &mut Value| {
            p["rules"][0]["format"] = "{fake}{fake}".into();
        }),
        ("because", |p: &mut Value| {
            p["rules"][0]["because"] = "line\nbreak".into();
        }),
        ("source", |p: &mut Value| {
            p["rules"][0]["source"]["policyRevision"] = 99.into();
        }),
        ("source", |p: &mut Value| {
            p["rules"][0]["source"]["ruleIndex"] = 1024.into();
        }),
        ("source", |p: &mut Value| {
            p["rules"][0]["source"] = p["rules"][1]["source"].clone();
        }),
    ] {
        match mutated(edit) {
            Err(PlanDecodeError::Validation(PlanError::InvalidRule { field: actual, .. })) => {
                assert_eq!(actual, field);
            }
            other => panic!("expected {field} error, got {other:?}"),
        }
    }
}

#[test]
fn plan_level_constraints_are_enforced() {
    for (field, edit) in [
        (
            "credentials",
            (|p: &mut Value| p["credentials"] = json!(["a", "a"])) as fn(&mut Value),
        ),
        ("policies", |p: &mut Value| p["policies"] = json!([])),
        ("policies", |p: &mut Value| {
            let policy = p["policies"][0].clone();
            p["policies"] = json!([policy.clone(), policy]);
        }),
    ] {
        assert!(
            matches!(mutated(edit), Err(PlanDecodeError::Validation(PlanError::Invalid { field:actual, .. })) if actual == field)
        );
    }
    assert!(matches!(
        mutated(|p| p["scopeId"] = "Dev".into()),
        Err(PlanDecodeError::Decode(_))
    ));
    assert!(matches!(
        mutated(|p| p["policies"][0]["revision"] = 0.into()),
        Err(PlanDecodeError::Decode(_))
    ));
}
