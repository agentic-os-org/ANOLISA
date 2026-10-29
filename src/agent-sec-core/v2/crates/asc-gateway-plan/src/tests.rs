use asc_credential_store::{AgentName, Carrier, CredentialId};

use crate::template::parse_template;
use crate::{Plan, PlanError, Preset, ResponseRewrite, preset_plan};

const GITHUB_PLAN: &str = include_str!("../../../fixtures/gateway/github-dev-agent.plan.json");
const DECISIONS: &str = include_str!("../../../fixtures/gateway/decisions.json");

fn agent(name: &str) -> AgentName {
    AgentName::new(name).unwrap()
}

fn github_plan() -> Plan {
    preset_plan(&agent("dev-agent"), &[Preset::Github]).unwrap()
}

fn fixture_value() -> serde_json::Value {
    serde_json::from_str(GITHUB_PLAN).unwrap()
}

fn mutated(edit: impl FnOnce(&mut serde_json::Value)) -> Result<Plan, PlanError> {
    let mut value = fixture_value();
    edit(&mut value);
    Plan::from_json(&serde_json::to_vec(&value).unwrap())
}

#[test]
fn github_preset_expands_to_the_shared_fixture() {
    let encoded: serde_json::Value = serde_json::from_slice(&github_plan().to_json()).unwrap();
    assert_eq!(encoded, fixture_value());
    assert_eq!(
        Plan::from_json(GITHUB_PLAN.as_bytes()).unwrap(),
        github_plan()
    );
}

#[test]
fn decision_fixture_uses_the_preset_plan_and_its_credential() {
    let decisions: serde_json::Value = serde_json::from_str(DECISIONS).unwrap();
    assert_eq!(decisions["plan"], "github-dev-agent.plan.json");
    let spec = Preset::Github.credential_spec();
    for entry in decisions["secrets"]["entries"].as_array().unwrap() {
        assert_eq!(entry["credential"], spec.id.as_str());
        assert_eq!(
            entry["carriers"],
            serde_json::json!(["header_token", "basic_password"])
        );
    }
}

#[test]
fn preset_expansion_is_canonical() {
    let twice = preset_plan(&agent("dev-agent"), &[Preset::Github, Preset::Github]).unwrap();
    assert_eq!(twice, github_plan());
    assert!(matches!(
        preset_plan(&agent("dev-agent"), &[]),
        Err(PlanError::Invalid {
            field: "presets",
            ..
        })
    ));
    assert_eq!("github".parse::<Preset>().unwrap(), Preset::Github);
    assert!("bailian".parse::<Preset>().is_err());
}

#[test]
fn github_rules_stream_only_upload_pack() {
    let plan = github_plan();
    let streamed: Vec<&str> = plan
        .rules
        .iter()
        .filter(|rule| rule.response_rewrite == ResponseRewrite::Passthrough)
        .map(|rule| rule.matcher.path.as_str())
        .collect();
    assert_eq!(streamed, ["/**/git-upload-pack"]);
}

#[test]
fn github_credential_spec_matches_the_carriers_the_gateway_inspects() {
    let spec = Preset::Github.credential_spec();
    assert_eq!(spec.id, CredentialId::new("github-token").unwrap());
    assert_eq!(
        spec.carriers,
        [Carrier::HeaderToken, Carrier::BasicPassword]
    );
}

#[test]
fn plans_never_carry_credential_values() {
    let text = String::from_utf8(github_plan().to_json()).unwrap();
    assert!(!text.contains("ghp_") && !text.contains("fake") && !text.contains("real"));
}

#[test]
fn unknown_fields_and_formats_are_rejected() {
    assert!(matches!(
        mutated(|plan| plan["extra"] = true.into()),
        Err(PlanError::Decode(_))
    ));
    assert!(matches!(
        mutated(|plan| plan["format"] = "agent-sec-gateway-plan/v2".into()),
        Err(PlanError::Decode(_))
    ));
    assert!(matches!(
        mutated(|plan| plan["credentialRequestDefault"] = "allow".into()),
        Err(PlanError::Decode(_))
    ));
    assert!(matches!(
        mutated(|plan| plan["rules"][0]["match"]["method"]["in"] = serde_json::json!(["OPTIONS"])),
        Err(PlanError::Decode(_))
    ));
    assert!(matches!(
        mutated(|plan| plan["rules"][0]["match"]["scheme"] = "http".into()),
        Err(PlanError::Decode(_))
    ));
    let duplicated = GITHUB_PLAN.replacen(
        "\"requestDefault\": \"allow\"",
        "\"requestDefault\": \"block\", \"requestDefault\": \"allow\"",
        1,
    );
    assert_ne!(duplicated, GITHUB_PLAN);
    assert!(matches!(
        Plan::from_json(duplicated.as_bytes()),
        Err(PlanError::Decode(_))
    ));
}

#[test]
fn rule_constraints_are_enforced() {
    let rule_error = |edit: fn(&mut serde_json::Value)| match mutated(edit) {
        Err(PlanError::InvalidRule { field, .. }) => field,
        other => panic!("expected a rule error, got {other:?}"),
    };
    assert_eq!(rule_error(|p| p["rules"][1]["index"] = 5.into()), "index");
    for host in ["*.github.com", "GitHub.com", "localhost", "a..b", "-a.com"] {
        let mut value = fixture_value();
        value["rules"][0]["match"]["host"] = host.into();
        assert!(
            matches!(
                Plan::from_json(&serde_json::to_vec(&value).unwrap()),
                Err(PlanError::InvalidRule {
                    field: "match.host",
                    ..
                })
            ),
            "{host}"
        );
    }
    for path in ["repos", "/a/../b", "/a//b", "/a%2fb", "/a/", "/a*b"] {
        let mut value = fixture_value();
        value["rules"][0]["match"]["path"] = path.into();
        assert!(
            matches!(
                Plan::from_json(&serde_json::to_vec(&value).unwrap()),
                Err(PlanError::InvalidRule {
                    field: "match.path",
                    ..
                })
            ),
            "{path}"
        );
    }
    assert_eq!(
        rule_error(|p| p["rules"][0]["match"]["method"]["in"] = serde_json::json!(["POST", "GET"])),
        "match.method"
    );
    assert_eq!(
        rule_error(|p| p["rules"][0]["match"]["credential"]["in"] = serde_json::json!(["other"])),
        "match.credential"
    );
    assert_eq!(
        rule_error(|p| p["rules"][0]["because"] = "line\nbreak".into()),
        "because"
    );
}

#[test]
fn plan_level_constraints_are_enforced() {
    assert!(matches!(
        mutated(|p| p["credentials"] = serde_json::json!([{"id": "a"}, {"id": "a"}])),
        Err(PlanError::Invalid {
            field: "credentials",
            ..
        })
    ));
    assert!(matches!(
        mutated(|p| p["source"]["presets"] = serde_json::json!([])),
        Err(PlanError::Invalid {
            field: "source.presets",
            ..
        })
    ));
    assert!(matches!(
        mutated(|p| p["subject"]["name"] = "Dev".into()),
        Err(PlanError::Decode(_))
    ));
}

/// Template with unknown fields is rejected by `deny_unknown_fields`.
#[test]
fn template_unknown_fields_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "test-token",
            "carriers": ["header_token"],
            "fakePrefixes": ["test_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "extra": true,
        "rules": [{
            "host": "api.example.com",
            "path": "/**",
            "method": ["GET"],
            "responseRewrite": "buffered",
            "because": "test"
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::Decode(_))
    ));
}

/// Template exceeding size limit is rejected.
#[test]
fn template_oversized_rejected() {
    // 32 KiB + 1 byte
    let oversized = " ".repeat(32_769);
    assert!(matches!(
        parse_template(&oversized, "test"),
        Err(PlanError::Invalid {
            field: "template",
            reason: "exceeds maximum size",
        })
    ));
}

/// Template with invalid name is rejected.
#[test]
fn template_invalid_name_rejected() {
    let make = |name: &str| {
        format!(
            r#"{{
                "name": "{name}",
                "version": 1,
                "credential": {{
                    "id": "t",
                    "carriers": ["header_token"],
                    "fakePrefixes": ["t_"]
                }},
                "credentialRequestDefault": "block",
                "requestDefault": "allow",
                "rules": [{{
                    "host": "a.b",
                    "path": "/**",
                    "method": ["GET"],
                    "responseRewrite": "buffered",
                    "because": "test"
                }}]
            }}"#
        )
    };

    // Uppercase
    assert!(parse_template(&make("GitHub"), "test").is_err());
    // Empty
    assert!(parse_template(&make(""), "test").is_err());
    // Starts with dash
    assert!(parse_template(&make("-bad"), "test").is_err());
}

/// Template with version 0 is rejected.
#[test]
fn template_version_zero_rejected() {
    let source = r#"{
        "name": "test",
        "version": 0,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "a.b",
            "path": "/**",
            "method": ["GET"],
            "responseRewrite": "buffered",
            "because": "test"
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::Invalid {
            field: "version",
            ..
        })
    ));
}

/// Template with duplicate rules (same host+path) is rejected.
#[test]
fn template_duplicate_rules_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [
            {
                "host": "a.b",
                "path": "/**",
                "method": ["GET"],
                "responseRewrite": "buffered",
                "because": "first"
            },
            {
                "host": "a.b",
                "path": "/**",
                "method": ["POST"],
                "responseRewrite": "buffered",
                "because": "second"
            }
        ]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::InvalidRule {
            field: "host+path",
            ..
        })
    ));
}

/// Template with empty carriers is rejected.
#[test]
fn template_empty_carriers_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": [],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "a.b",
            "path": "/**",
            "method": ["GET"],
            "responseRewrite": "buffered",
            "because": "test"
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::Invalid {
            field: "credential.carriers",
            ..
        })
    ));
}

/// Template with duplicate fake prefixes is rejected.
#[test]
fn template_duplicate_prefixes_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_", "t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "a.b",
            "path": "/**",
            "method": ["GET"],
            "responseRewrite": "buffered",
            "because": "test"
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::Invalid {
            field: "credential.fakePrefixes",
            ..
        })
    ));
}

/// Template with no rules is rejected.
#[test]
fn template_empty_rules_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": []
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::Invalid { field: "rules", .. })
    ));
}

/// Template with bad host in a rule is rejected.
#[test]
fn template_rule_bad_host_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "UPPER.com",
            "path": "/**",
            "method": ["GET"],
            "responseRewrite": "buffered",
            "because": "test"
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::InvalidRule { field: "host", .. })
    ));
}

/// Template with bad path in a rule is rejected.
#[test]
fn template_rule_bad_path_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "a.b",
            "path": "no-slash",
            "method": ["GET"],
            "responseRewrite": "buffered",
            "because": "test"
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::InvalidRule { field: "path", .. })
    ));
}

/// Template methods are canonicalized by sorting.
#[test]
fn template_methods_canonicalized() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "a.b",
            "path": "/**",
            "method": ["POST", "GET"],
            "responseRewrite": "buffered",
            "because": "test"
        }]
    }"#;
    let template = parse_template(source, "test").unwrap();
    let methods = &template.rules[0].methods;
    assert!(methods.windows(2).all(|pair| pair[0] <= pair[1]));
}

/// Rule with control characters in because is rejected.
#[test]
fn template_rule_because_control_chars_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "a.b",
            "path": "/**",
            "method": ["GET"],
            "responseRewrite": "buffered",
            "because": "has\nnewline"
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::InvalidRule {
            field: "because",
            ..
        })
    ));
}

/// Template with unknown fields inside a rule is rejected.
#[test]
fn template_rule_unknown_field_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "a.b",
            "path": "/**",
            "method": ["GET"],
            "responseRewrite": "buffered",
            "because": "test",
            "extra": true
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::Decode(_))
    ));
}

/// Template with duplicate methods in a rule is rejected.
#[test]
fn template_rule_duplicate_methods_rejected() {
    let source = r#"{
        "name": "test",
        "version": 1,
        "credential": {
            "id": "t",
            "carriers": ["header_token"],
            "fakePrefixes": ["t_"]
        },
        "credentialRequestDefault": "block",
        "requestDefault": "allow",
        "rules": [{
            "host": "a.b",
            "path": "/**",
            "method": ["GET", "GET"],
            "responseRewrite": "buffered",
            "because": "test"
        }]
    }"#;
    assert!(matches!(
        parse_template(source, "test"),
        Err(PlanError::InvalidRule {
            field: "method",
            ..
        })
    ));
}
