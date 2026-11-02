use asc_policy_types::Validate;
use asc_policy_types::authoring::*;
use serde_json::{Value as Json, json};

fn policy(rule: &Json) -> PolicyTemplate {
    serde_json::from_value(json!({"specVersion":"0.1","rules":[rule]})).unwrap()
}
fn rule() -> Json {
    json!({"effect":"block","category":"file","action":"write","target":{"type":"file","path":"/workspace/**"},"where":{"operation":{"eq":"delete"}}})
}

#[test]
fn general_rules_round_trip_with_conditions_explanations_and_history() {
    let mut value = rule();
    value["effect"] = json!("require_confirmation");
    value["action"] = json!("exec");
    value["where"] = json!({"and":[{"argsPrefix":{"eq":["push","--force"]}},{"not":{"cwd":{"in":["/tmp","/workspace"]}}}]});
    value["previous"] = json!({"or":[{"category":"file","action":"read","target":{"type":"file","path":"/secrets/**"},"where":{"program":{"eq":"/usr/bin/cat"}}},{"not":{"category":"file","action":"read","target":{"type":"file","path":"/public/**"}}}]});
    value["because"] = json!("Review before execution");
    let authored = policy(&value);
    authored.validate().unwrap();
    assert_eq!(
        serde_json::to_value(authored).unwrap(),
        json!({"specVersion":"0.1","rules":[value]})
    );
}

#[test]
fn normalization_glob_and_byte_limits_apply_to_typed_file_targets() {
    for path in [
        "/",
        "/protected",
        "/workspace/**",
        "/a/*/b",
        "/file?.txt",
        &format!("/{}", "a".repeat(4095)),
    ] {
        let mut value = rule();
        value["target"]["path"] = json!(path);
        policy(&value).validate().unwrap();
    }
    for path in [
        "",
        "relative",
        "/with\0nul",
        "/~/file",
        "/$HOME/file",
        "/trailing/",
        "/a//b",
        "/a/./b",
        "/a/../b",
        "/a/**b",
        "/a/[bc]*",
        "/a/{bc}*",
        "/a/\\*",
        &format!("/{}", "a".repeat(4096)),
    ] {
        let mut value = rule();
        value["target"]["path"] = json!(path);
        assert_eq!(
            policy(&value).validate().unwrap_err().path,
            "rules[0].target.path"
        );
    }
}

#[test]
fn operators_are_checked_against_action_attribute_and_operand_type() {
    for condition in [
        json!({"operation":{"in":["create","delete"]}}),
        json!({"mode":{"ge":0}}),
        json!({"ownerUid":{"eq":4_294_967_295_u64}}),
    ] {
        let mut value = rule();
        value["where"] = condition;
        policy(&value).validate().unwrap();
    }
    for condition in [
        json!({"cwd":{"eq":"/tmp"}}),
        json!({"operation":{"eq":true}}),
        json!({"operation":{"in":["delete",1]}}),
        json!({"operation":{"eq":"unlink"}}),
        json!({"mode":{"eq":-1}}),
        json!({"mode":{"eq":4_294_967_296_u64}}),
        json!({"program":{"eq":"/bin/*"}}),
        json!({"and":[]}),
    ] {
        let mut value = rule();
        value["where"] = condition;
        assert!(policy(&value).validate().is_err());
    }
}

#[test]
fn strict_shape_rejects_unknown_fields_and_multiple_expression_branches() {
    for condition in [
        json!({"unknown":{"eq":"x"}}),
        json!({"operation":{"eq":"delete","ne":"write"}}),
        json!({"and":[],"or":[]}),
    ] {
        let mut value = rule();
        value["where"] = condition;
        assert!(
            serde_json::from_value::<PolicyTemplate>(json!({"specVersion":"0.1","rules":[value]}))
                .is_err()
        );
    }
    let mut value = rule();
    value["previous"] = json!({"category":"file","action":"read","target":{"type":"file","path":"/secret"},"effect":"block"});
    assert!(
        serde_json::from_value::<PolicyTemplate>(json!({"specVersion":"0.1","rules":[value]}))
            .is_err()
    );
    assert!(
        serde_json::from_str::<PolicyTemplate>(
            r#"{"specVersion":"0.1","specVersion":"0.1","rules":[]}"#
        )
        .is_err()
    );
}

#[test]
fn recursive_predicates_have_a_depth_and_total_node_budget() {
    let mut value = rule();
    let mut predicate = json!({"operation":{"eq":"delete"}});
    for _ in 0..17 {
        predicate = json!({"not":predicate});
    }
    value["where"] = predicate;
    assert!(policy(&value).validate().is_err());
    let mut value = rule();
    value["where"] = json!({"or":vec![json!({"operation":{"eq":"delete"}});4096]});
    assert!(policy(&value).validate().is_err());
}

fn credential_rule() -> Json {
    json!({"effect":"allow","category":"network","action":"use_credential","target":{
        "type":"http_request","credentialId":"github","scheme":"https",
        "host":"api.github.com","path":"/**","methods":["GET","HEAD","POST","PUT","PATCH","DELETE"],
        "responseRewrite":"buffered"
    }})
}

#[test]
fn credential_rules_round_trip_and_preserve_optional_explanations() {
    for rewrite in ["buffered", "none"] {
        let mut value = credential_rule();
        value["target"]["responseRewrite"] = json!(rewrite);
        let template = policy(&value);
        template.validate().unwrap();
        assert!(template.uses_credentials());
        assert_eq!(
            serde_json::to_value(template).unwrap(),
            json!({"specVersion":"0.1","rules":[value]})
        );
    }
    assert!(!policy(&rule()).uses_credentials());
}

#[test]
fn only_native_policy_sources_classify_managed_credentials() {
    assert!(PolicySource::SecCore(policy(&credential_rule())).uses_credentials());

    let cedar: PolicySource = serde_json::from_value(json!({
        "language": "cedar",
        "content": {
            "schema": {"schemaId": "seccore.system", "revision": 1},
            "text": "permit(principal, action, resource);"
        }
    }))
    .unwrap();
    assert!(!cedar.uses_credentials());
}

#[test]
fn credential_intent_is_classified_before_mismatched_rules_are_rejected() {
    for field in ["category", "action", "target"] {
        let mut value = rule();
        value[field] = credential_rule()[field].clone();
        let template = policy(&value);
        assert!(template.uses_credentials(), "{field}");
        assert!(template.validate().is_err(), "{field}");
    }
    for field in ["category", "action", "target"] {
        let mut event =
            json!({"category":"file","action":"read","target":{"type":"file","path":"/secret"}});
        event[field] = credential_rule()[field].clone();
        let mut value = rule();
        value["previous"] = json!({"not":event});
        assert!(policy(&value).uses_credentials(), "historical {field}");
        assert!(policy(&value).validate().is_err(), "historical {field}");
    }
}

#[test]
fn credential_policies_reject_conditions_effects_and_mixed_domains_as_a_whole() {
    for (field, invalid) in [
        ("effect", json!("block")),
        ("effect", json!("require_confirmation")),
        ("where", json!({"program":{"eq":"/bin/agent"}})),
        (
            "previous",
            json!({"category":"file","action":"read","target":{"type":"file","path":"/secret"}}),
        ),
        ("category", json!("file")),
        ("action", json!("read")),
        ("target", json!({"type":"file","path":"/secret"})),
        ("because", json!("line\nbreak")),
        ("because", json!("a".repeat(257))),
        ("because", json!("")),
    ] {
        let mut value = credential_rule();
        value[field] = invalid;
        assert!(policy(&value).validate().is_err(), "{field}");
    }
    for rules in [
        vec![rule(), credential_rule()],
        vec![credential_rule(), rule()],
    ] {
        let template: PolicyTemplate =
            serde_json::from_value(json!({"specVersion":"0.1","rules":rules})).unwrap();
        assert!(template.uses_credentials());
        assert!(template.validate().is_err());
    }
}

#[test]
fn credential_targets_enforce_store_identifiers_and_gateway_matching_bounds() {
    for (field, invalid) in [
        ("credentialId", json!("")),
        ("credentialId", json!("GitHub")),
        ("credentialId", json!("-token")),
        ("credentialId", json!("token-")),
        ("credentialId", json!("token_id")),
        ("credentialId", json!("a".repeat(64))),
        ("host", json!("*.github.com")),
        ("host", json!("GitHub.com")),
        ("host", json!("localhost")),
        ("host", json!("a..b")),
        ("host", json!("-a.com")),
        ("host", json!(format!("{}.com", "a".repeat(64)))),
        (
            "host",
            json!(format!(
                "{}.{}.{}.{}",
                "a".repeat(63),
                "b".repeat(63),
                "c".repeat(63),
                "d".repeat(63)
            )),
        ),
        ("path", json!("/")),
        ("path", json!("repos")),
        ("path", json!("/a/../b")),
        ("path", json!("/a//b")),
        ("path", json!("/a%2fb")),
        ("path", json!("/a/")),
        ("path", json!("/a*b")),
        ("path", json!(format!("/{}", "a".repeat(512)))),
        ("methods", json!([])),
        ("methods", json!(["GET", "GET"])),
    ] {
        let mut value = credential_rule();
        value["target"][field] = invalid;
        assert_eq!(
            policy(&value).validate().unwrap_err().path,
            format!("rules[0].target.{field}")
        );
    }
    for (field, valid) in [
        ("credentialId", json!("a".repeat(63))),
        (
            "credentialId",
            json!("00000000-0000-4000-8000-000000000001"),
        ),
        (
            "host",
            json!(format!(
                "{}.{}.{}.{}",
                "a".repeat(63),
                "b".repeat(63),
                "c".repeat(63),
                "d".repeat(61)
            )),
        ),
        ("path", json!(format!("/{}", "a".repeat(511)))),
        ("methods", json!(["POST", "GET"])),
    ] {
        let mut value = credential_rule();
        value["target"][field] = valid;
        policy(&value).validate().unwrap();
    }
    for (field, unsupported) in [
        ("scheme", json!("http")),
        ("methods", json!(["OPTIONS"])),
        ("methods", json!(["get"])),
        ("responseRewrite", json!("passthrough")),
        ("extra", json!(true)),
    ] {
        let mut value = credential_rule();
        value["target"][field] = unsupported;
        assert!(
            serde_json::from_value::<PolicyTemplate>(json!({"specVersion":"0.1","rules":[value]}))
                .is_err()
        );
    }
}
