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

fn structured_policy(targets: &Json) -> Json {
    json!({
        "specVersion": "0.2",
        "fileProtection": {
            "topology": "static_at_apply",
            "targets": targets
        }
    })
}

#[test]
fn structured_file_protection_round_trips_all_contract_variants() {
    let value = structured_policy(&json!([
        {
            "path": "/etc/agent/config.yaml",
            "kind": "exact_file",
            "protections": {"delete": true, "ordinaryWrite": true}
        },
        {
            "path": "/workspace/important",
            "kind": "directory_tree",
            "protections": {"delete": true, "ordinaryWrite": false},
            "exclusions": [
                {"path": "/workspace/important/cache.db", "kind": "exact_entry"},
                {"path": "/workspace/important/tmp", "kind": "directory_subtree"}
            ]
        }
    ]));
    let authored: PolicyTemplate = serde_json::from_value(value.clone()).unwrap();
    authored.validate().unwrap();
    assert_eq!(serde_json::to_value(authored).unwrap(), value);
}

#[test]
fn policy_versions_select_exactly_one_authoring_representation() {
    let legacy = json!({"specVersion":"0.1","rules":[rule()]});
    let decoded: PolicyTemplate = serde_json::from_value(legacy.clone()).unwrap();
    decoded.validate().unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), legacy);

    for value in [
        json!({"specVersion":"0.1","rules":[rule()],"fileProtection":{"topology":"static_at_apply","targets":[]}}),
        json!({"specVersion":"0.2","rules":[rule()],"fileProtection":{"topology":"static_at_apply","targets":[]}}),
        json!({"specVersion":"0.2"}),
        json!({"specVersion":"0.3","rules":[rule()]}),
    ] {
        let decoded: PolicyTemplate = serde_json::from_value(value).unwrap();
        assert!(decoded.validate().is_err());
    }
}

#[test]
fn structured_targets_enforce_paths_scopes_protections_and_cardinality() {
    let valid_target = json!({
        "path": "/workspace/tree",
        "kind": "directory_tree",
        "protections": {"delete": false, "ordinaryWrite": true},
        "exclusions": [{"path": "/workspace/tree/cache", "kind": "directory_subtree"}]
    });
    for path in [
        "",
        "relative",
        "/with\0nul",
        "/trailing/",
        "/a//b",
        "/a/./b",
        "/a/../b",
        "/a/*",
        "/a/?",
        "/a/[b]",
        "/a/{b}",
        "/a/\\b",
    ] {
        let mut target = valid_target.clone();
        target["path"] = json!(path);
        assert!(
            serde_json::from_value::<PolicyTemplate>(structured_policy(&json!([target])))
                .unwrap()
                .validate()
                .is_err()
        );
    }

    let mut no_protection = valid_target.clone();
    no_protection["protections"] = json!({"delete":false,"ordinaryWrite":false});
    let mut exact_with_exclusion = valid_target.clone();
    exact_with_exclusion["kind"] = json!("exact_file");
    let mut non_descendant = valid_target.clone();
    non_descendant["exclusions"][0]["path"] = json!("/workspace/treehouse/cache");
    for targets in [
        json!([]),
        json!([no_protection]),
        json!([exact_with_exclusion]),
        json!([non_descendant]),
        Json::Array(vec![valid_target.clone(); 65]),
    ] {
        assert!(
            serde_json::from_value::<PolicyTemplate>(structured_policy(&targets))
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[test]
fn structured_paths_reject_duplicate_targets_and_duplicate_or_conflicting_exclusions() {
    let target = json!({
        "path": "/workspace/tree",
        "kind": "directory_tree",
        "protections": {"delete": true, "ordinaryWrite": false},
        "exclusions": []
    });
    assert_eq!(
        serde_json::from_value::<PolicyTemplate>(structured_policy(&json!([
            target.clone(),
            target.clone()
        ])))
        .unwrap()
        .validate()
        .unwrap_err()
        .path,
        "fileProtection.targets[1].path"
    );

    for second_kind in ["exact_entry", "directory_subtree"] {
        let mut with_exclusions = target.clone();
        with_exclusions["exclusions"] = json!([
            {"path":"/workspace/tree/cache","kind":"exact_entry"},
            {"path":"/workspace/tree/cache","kind":second_kind}
        ]);
        let error =
            serde_json::from_value::<PolicyTemplate>(structured_policy(&json!([with_exclusions])))
                .unwrap()
                .validate()
                .unwrap_err();
        assert_eq!(error.path, "fileProtection.targets[0].exclusions[1].path");
        let classification = if second_kind == "exact_entry" {
            "duplicates"
        } else {
            "conflicts with"
        };
        assert_eq!(
            error.message,
            format!("{classification} exclusion path at index 0")
        );
    }
}

#[test]
fn omitted_legacy_rules_parse_as_empty_but_fail_typed_validation() {
    let decoded: PolicyTemplate = serde_json::from_value(json!({"specVersion":"0.1"})).unwrap();
    assert!(decoded.rules.is_empty());
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        json!({"specVersion":"0.1"})
    );
    let error = decoded.validate().unwrap_err();
    assert_eq!(error.path, "rules");
    assert_eq!(error.message, "requires 1..1024 rules");
}

#[test]
fn structured_dtos_reject_unknown_fields_wrong_casing_and_untyped_values() {
    let valid = structured_policy(&json!([{
        "path":"/tree", "kind":"directory_tree",
        "protections":{"delete":true,"ordinaryWrite":false},
        "exclusions":[{"path":"/tree/cache","kind":"exact_entry"}]
    }]));
    for (pointer, replacement) in [
        ("/fileProtection/extra", json!(true)),
        ("/fileProtection/topology", json!("dynamic")),
        ("/fileProtection/targets/0/extra", json!(true)),
        ("/fileProtection/targets/0/kind", json!("ExactFile")),
        (
            "/fileProtection/targets/0/protections/ordinary_write",
            json!(true),
        ),
        (
            "/fileProtection/targets/0/protections/delete",
            json!("true"),
        ),
        ("/fileProtection/targets/0/exclusions/0/extra", json!(true)),
        (
            "/fileProtection/targets/0/exclusions/0/kind",
            json!("exact_file"),
        ),
        ("/fileProtection/targets", json!(["/tree"])),
    ] {
        let mut value = valid.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        value.pointer_mut(parent).unwrap()[field] = replacement;
        assert!(
            serde_json::from_value::<PolicyTemplate>(value).is_err(),
            "{pointer}"
        );
    }
    for pointer in [
        "/fileProtection/targets/0/protections",
        "/fileProtection/targets/0/exclusions/0",
    ] {
        let mut value = valid.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(if pointer.ends_with("protections") {
                "ordinaryWrite"
            } else {
                "kind"
            });
        assert!(serde_json::from_value::<PolicyTemplate>(value).is_err());
    }
}

#[test]
fn structured_exclusion_boundary_errors_retain_exact_indices() {
    let mut value = structured_policy(&json!([{
        "path":"/", "kind":"directory_tree",
        "protections":{"delete":true,"ordinaryWrite":false},
        "exclusions":[{"path":"/child","kind":"exact_entry"}]
    }]));
    serde_json::from_value::<PolicyTemplate>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    value["fileProtection"]["targets"][0]["path"] = json!("/tree");
    for path in [
        "/tree",
        "/treehouse/child",
        "/tree/../child",
        "/tree/*",
        "/tree/with\0nul",
        &format!("/tree/{}", "a".repeat(4091)),
    ] {
        value["fileProtection"]["targets"][0]["exclusions"][0]["path"] = json!(path);
        let error = serde_json::from_value::<PolicyTemplate>(value.clone())
            .unwrap()
            .validate()
            .unwrap_err();
        assert_eq!(error.path, "fileProtection.targets[0].exclusions[0].path");
    }
}
