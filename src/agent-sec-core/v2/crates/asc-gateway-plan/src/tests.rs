use asc_credential_store::ScopeId;
use asc_policy_types::authoring::{PolicySource as AuthoredPolicySource, PolicyTemplate, Resource};
use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::scope::PreparedScope;
use serde_json::{Value, json};

use crate::{Plan, PlanError, compile_scope, github_credential_spec};

const GITHUB_POLICY: &str = include_str!("../policies/github.json");
const GITHUB_PLAN: &str = include_str!("../../../fixtures/gateway/github-dev-agent.plan.json");
const DECISIONS: &str = include_str!("../../../fixtures/gateway/decisions.json");
const SCOPE_ID: &str = "00000000-0000-4000-8000-000000000001";

fn github_policy() -> PreparedPolicy {
    PreparedPolicy {
        policy_id: ResourceId::new("policy:github").unwrap(),
        policy_name: "GitHub".into(),
        revision: Revision::new(7).unwrap(),
        template: AuthoredPolicySource::SecCore(serde_json::from_str(GITHUB_POLICY).unwrap()),
    }
}

fn native_template(source: &AuthoredPolicySource) -> &PolicyTemplate {
    let AuthoredPolicySource::SecCore(template) = source else {
        panic!("expected native test policy");
    };
    template
}

fn native_template_mut(source: &mut AuthoredPolicySource) -> &mut PolicyTemplate {
    let AuthoredPolicySource::SecCore(template) = source else {
        panic!("expected native test policy");
    };
    template
}

fn scope(policies: &[PreparedPolicy]) -> PreparedScope {
    serde_json::from_value(json!({
        "scopeId": SCOPE_ID,
        "ownerUid": 1000,
        "selector": {"kind":"process","match":{"processName":"agent"}},
        "policySnapshots": policies,
        "status":"ACTIVE"
    }))
    .unwrap()
}

fn github_plan() -> Plan {
    compile_scope(&scope(&[github_policy()])).unwrap()
}

fn fixture_value() -> Value {
    serde_json::from_str(GITHUB_PLAN).unwrap()
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
fn native_github_policy_compiles_to_the_shared_gateway_fixture() {
    let compiled = github_plan();
    assert_eq!(
        serde_json::from_slice::<Value>(&compiled.to_json()).unwrap(),
        fixture_value()
    );
    assert_eq!(decode_plan(GITHUB_PLAN.as_bytes()).unwrap(), compiled);
    let decisions: Value = serde_json::from_str(DECISIONS).unwrap();
    assert_eq!(decisions["plan"], "github-dev-agent.plan.json");
    assert_eq!(decisions["listeners"][0]["scopeId"], SCOPE_ID);
    let spec = github_credential_spec();
    for entry in decisions["secrets"]["entries"].as_array().unwrap() {
        assert_eq!(entry["credential"], spec.id.as_str());
        assert_eq!(
            entry["carriers"],
            serde_json::to_value(&spec.carriers).unwrap()
        );
    }
}

#[test]
fn compilation_aggregates_snapshots_canonically_and_preserves_each_rule_source() {
    let first = github_policy();
    let mut second = first.clone();
    second.policy_id = ResourceId::new("policy:other").unwrap();
    second.revision = Revision::new(9).unwrap();
    let second_template = native_template_mut(&mut second.template);
    second_template.rules.truncate(1);
    second_template.rules[0].because = None;
    let Resource::HttpRequest {
        credential_id,
        methods,
        ..
    } = &mut second_template.rules[0].target
    else {
        panic!("expected HTTP target");
    };
    *credential_id = "other-token".into();
    methods.reverse();
    let file = PreparedPolicy {
        policy_id: ResourceId::new("policy:file").unwrap(),
        policy_name: "Files".into(),
        revision: Revision::new(1).unwrap(),
        template: AuthoredPolicySource::SecCore(
            serde_json::from_value(json!({"specVersion":"0.1","rules":[{
                "effect":"block","category":"file","action":"read",
                "target":{"type":"file","path":"/secret"}
            }]}))
            .unwrap(),
        ),
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
            },
        ]
    );
    assert_eq!(
        plan.credentials
            .iter()
            .map(asc_credential_store::CredentialId::as_str)
            .collect::<Vec<_>>(),
        ["github", "other-token"]
    );
    assert_eq!(
        plan.rules.last().unwrap().source,
        crate::RuleSource {
            policy_id: second.policy_id,
            policy_revision: second.revision,
            rule_index: 0,
        }
    );
    assert_eq!(
        plan.rules.last().unwrap().because,
        "Credential use allowed by policy"
    );
    let mut projected = plan.clone();
    projected.rules.remove(0);
    projected.validate().unwrap();
    assert_eq!(decode_plan(&projected.to_json()).unwrap(), projected);
    assert_eq!(projected.rules, plan.rules[1..]);
    projected.rules.clear();
    projected.credentials.clear();
    projected.validate().unwrap();
    for (index, rule) in plan
        .rules
        .iter()
        .enumerate()
        .take(native_template(&first.template).rules.len())
    {
        assert_eq!(
            rule.source,
            crate::RuleSource {
                policy_id: first.policy_id.clone(),
                policy_revision: first.revision,
                rule_index: index,
            }
        );
    }
}

#[test]
fn cedar_sources_are_not_compiled_as_gateway_policies() {
    let cedar = PreparedPolicy {
        policy_id: ResourceId::new("policy:cedar").unwrap(),
        policy_name: "Cedar".into(),
        revision: Revision::new(1).unwrap(),
        template: serde_json::from_value(json!({
            "language": "cedar",
            "content": {
                "schema": {"schemaId": "seccore.system", "revision": 1},
                "text": "permit(principal, action, resource);"
            }
        }))
        .unwrap(),
    };
    assert_eq!(
        compile_scope(&scope(&[cedar, github_policy()])).unwrap(),
        github_plan()
    );
}

#[test]
fn invalid_or_mixed_policy_never_compiles_a_partial_plan() {
    for edit in [
        (|template: &mut PolicyTemplate| {
            template.rules[1].effect = asc_policy_types::authoring::Effect::Block;
        }) as fn(&mut PolicyTemplate),
        |template: &mut PolicyTemplate| {
            template.rules[1].action = asc_policy_types::authoring::Action::Read;
        },
        |template: &mut PolicyTemplate| {
            template.rules[1].target = Resource::File {
                path: "/secret".into(),
            };
        },
    ] {
        let mut policy = github_policy();
        edit(native_template_mut(&mut policy.template));
        assert!(matches!(
            compile_scope(&scope(&[policy])),
            Err(PlanError::Policy(_))
        ));
    }
    let mut policy = github_policy();
    let template = native_template_mut(&mut policy.template);
    template.rules[1] = serde_json::from_value(json!({
        "effect":"block","category":"file","action":"read","target":{"type":"file","path":"/secret"}
    }))
    .unwrap();
    assert!(matches!(
        compile_scope(&scope(&[policy.clone()])),
        Err(PlanError::Policy(_))
    ));
    let template = native_template_mut(&mut policy.template);
    template.rules = vec![template.rules[1].clone()];
    assert!(matches!(
        compile_scope(&scope(&[policy])),
        Err(PlanError::Invalid {
            field: "policies",
            ..
        })
    ));
    let mut selected = scope(&[github_policy()]);
    selected.selector = asc_policy_types::scope::ScopeSelector::Pid { pid: 4242 };
    assert!(matches!(
        compile_scope(&selected),
        Err(PlanError::Policy(_))
    ));
}

#[test]
fn compiler_enforces_aggregate_gateway_limits() {
    let mut policy = github_policy();
    let template = native_template_mut(&mut policy.template);
    template.rules = vec![template.rules[0].clone(); 256];
    assert_eq!(
        compile_scope(&scope(&[policy.clone()]))
            .unwrap()
            .rules
            .len(),
        256
    );
    let template = native_template_mut(&mut policy.template);
    template.rules.push(template.rules[0].clone());
    assert!(matches!(
        compile_scope(&scope(&[policy])),
        Err(PlanError::Invalid { field: "rules", .. })
    ));
    let mut policies = Vec::new();
    for index in 0..17 {
        let mut policy = github_policy();
        policy.policy_id = ResourceId::new(format!("policy:{index}")).unwrap();
        for (rule_index, rule) in native_template_mut(&mut policy.template)
            .rules
            .iter_mut()
            .enumerate()
        {
            let Resource::HttpRequest { credential_id, .. } = &mut rule.target else {
                panic!("expected HTTP target");
            };
            *credential_id = format!("c-{index}-{rule_index}");
        }
        policies.push(policy);
    }
    assert!(matches!(
        compile_scope(&scope(&policies)),
        Err(PlanError::Invalid {
            field: "credentials",
            ..
        })
    ));
}

#[test]
fn plan_uses_exact_snapshots_not_policy_names_or_description() {
    let expected = github_plan();
    let mut policy = github_policy();
    policy.policy_name = "synthetic-private-context".into();
    native_template_mut(&mut policy.template).description =
        Some("synthetic-private-context".into());
    assert_eq!(compile_scope(&scope(&[policy.clone()])).unwrap(), expected);
    policy.revision = policy.revision.checked_next().unwrap();
    assert_ne!(compile_scope(&scope(&[policy])).unwrap(), expected);
}

#[test]
fn unknown_fields_and_formats_are_rejected() {
    for edit in [
        (|plan: &mut Value| plan["extra"] = true.into()) as fn(&mut Value),
        |plan: &mut Value| plan["format"] = "agent-sec-gateway-plan/v2".into(),
        |plan: &mut Value| plan["rules"][0]["match"]["methods"] = json!(["OPTIONS"]),
        |plan: &mut Value| plan["rules"][0]["match"]["scheme"] = "http".into(),
    ] {
        assert!(matches!(mutated(edit), Err(PlanDecodeError::Decode(_))));
    }
    let duplicated = GITHUB_PLAN.replacen(
        "\"scopeId\":",
        "\"scopeId\": \"other-scope\", \"scopeId\":",
        1,
    );
    assert_ne!(duplicated, GITHUB_PLAN);
    assert!(matches!(
        decode_plan(duplicated.as_bytes()),
        Err(PlanDecodeError::Decode(_))
    ));
}

#[test]
fn rule_constraints_are_enforced() {
    for host in ["*.github.com", "GitHub.com", "localhost", "a..b", "-a.com"] {
        assert!(
            matches!(
                mutated(|plan| plan["rules"][0]["match"]["host"] = host.into()),
                Err(PlanDecodeError::Validation(PlanError::InvalidRule {
                    field: "match.host",
                    ..
                }))
            ),
            "{host}"
        );
    }
    for path in ["repos", "/a/../b", "/a//b", "/a%2fb", "/a/", "/a*b"] {
        assert!(
            matches!(
                mutated(|plan| plan["rules"][0]["match"]["path"] = path.into()),
                Err(PlanDecodeError::Validation(PlanError::InvalidRule {
                    field: "match.path",
                    ..
                }))
            ),
            "{path}"
        );
    }
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
    assert!(matches!(
        mutated(|p| p["credentials"] = json!(["a", "a"])),
        Err(PlanDecodeError::Validation(PlanError::Invalid {
            field: "credentials",
            ..
        }))
    ));
    assert!(matches!(
        mutated(|p| p["policies"] = json!([])),
        Err(PlanDecodeError::Validation(PlanError::Invalid {
            field: "policies",
            ..
        }))
    ));
    assert!(matches!(
        mutated(|p| p["scopeId"] = "Dev".into()),
        Err(PlanDecodeError::Decode(_))
    ));
    assert!(matches!(
        mutated(|p| p["policies"][0]["revision"] = 0.into()),
        Err(PlanDecodeError::Decode(_))
    ));
}
