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
