//! Instrumentation profile contract validation, fixtures, and SourceCapability compilation.

use aw_contracts::{
    canonical,
    instrumentation::{
        CallShape, Codec, FieldBinding, InstrumentationProfile, Origin, Runtime, SCHEMA_NAME,
        VERSION,
    },
    source::{
        project_capability_cells, CapabilityStatus, FailureAction, HookAuthority, HookEffect,
        SourceKind, HOOK_EVENTS,
    },
    Registry,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn python_profile() -> Value {
    json!({
        "version": VERSION,
        "id": "example.client.python",
        "origin": "builtin",
        "runtime": "python",
        "package": {
            "name": "example-sdk",
            "versions": ">=1.9,<2"
        },
        "targets": [
            {
                "id": "session-call-tool",
                "module": "example_sdk.client.session",
                "qualname": "ClientSession.call_tool",
                "call": "async_iter",
                "hooks": {
                    "tool.before": {
                        "mode": "control",
                        "effects": ["observe", "block", "replace_input"],
                        "failure_actions": []
                    },
                    "tool.after": {
                        "mode": "control",
                        "effects": ["observe", "replace_result"],
                        "failure_actions": ["withhold_result"]
                    }
                },
                "fields": {
                    "tool_name": { "from": "arg.0", "codec": "text" },
                    "input": { "from": "arg.1", "codec": "json" },
                    "result": { "from": "result", "codec": "json" }
                },
                "stream": {
                    "buffer": true,
                    "max_items": 1024,
                    "max_bytes": 1048576
                }
            }
        ]
    })
}

fn parse(value: &Value) -> Result<InstrumentationProfile, aw_contracts::Error> {
    InstrumentationProfile::parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn valid_python_and_js_profiles_round_trip_canonically() {
    let vectors: Value = canonical::parse(include_bytes!(
        "fixtures/instrumentation-profile-vectors.json"
    ))
    .unwrap();
    let valid = vectors["valid"].as_array().unwrap();
    assert!(valid.len() >= 2);

    let registry = Registry::new().unwrap();
    let mut saw_python = false;
    let mut saw_js = false;
    for entry in valid {
        let name = entry["name"].as_str().unwrap();
        let doc = &entry["document"];
        let expected_bytes = canonical::bytes(doc).unwrap();
        let parsed = InstrumentationProfile::parse(&expected_bytes)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let from_val = InstrumentationProfile::from_value(doc.clone()).unwrap();
        let via_registry = registry.validate_instrumentation_profile(doc).unwrap();
        assert_eq!(parsed, from_val, "{name}");
        assert_eq!(parsed, via_registry, "{name}");
        assert_eq!(parsed.version(), VERSION, "{name}");
        assert_eq!(
            parsed.to_canonical_bytes().unwrap(),
            expected_bytes,
            "{name}"
        );
        parsed.validate().unwrap();
        match parsed.runtime() {
            Runtime::Python => saw_python = true,
            Runtime::Js => saw_js = true,
        }
    }
    assert!(saw_python && saw_js);

    let py = parse(&python_profile()).unwrap();
    assert_eq!(py.id(), "example.client.python");
    assert_eq!(py.origin(), Origin::Builtin);
    assert_eq!(py.runtime(), Runtime::Python);
    assert_eq!(py.package().name(), "example-sdk");
    assert_eq!(py.package().versions(), ">=1.9,<2");
    assert_eq!(py.targets().len(), 1);
    let target = &py.targets()[0];
    assert_eq!(target.id(), "session-call-tool");
    assert_eq!(target.module(), "example_sdk.client.session");
    assert_eq!(target.qualname(), "ClientSession.call_tool");
    assert_eq!(target.call(), CallShape::AsyncIter);
    assert_eq!(target.code_sha256(), None);
    let stream = target.stream().unwrap();
    assert!(stream.buffer());
    assert_eq!(stream.max_items(), 1024);
    assert_eq!(stream.max_bytes(), 1_048_576);
    assert_eq!(
        target.field("tool_name"),
        Some(&FieldBinding::From {
            from: "arg.0".into(),
            codec: Codec::Text,
        })
    );
    assert_eq!(SCHEMA_NAME, "instrumentation-profile-v1alpha1");
}

#[test]
fn invalid_vectors_cover_and_reject_rules_r1_through_r15() {
    let vectors: Value = canonical::parse(include_bytes!(
        "fixtures/instrumentation-profile-vectors.json"
    ))
    .unwrap();
    let invalid = vectors["invalid"].as_array().unwrap();
    let mut covered_rules = BTreeSet::new();
    for entry in invalid {
        let name = entry["name"].as_str().unwrap();
        let rule = entry["rule"].as_str().unwrap();
        covered_rules.insert(rule.to_owned());
        let doc = &entry["document"];
        let result = parse(doc);
        assert!(
            result.is_err(),
            "vector `{name}` ({rule}) was expected to fail parse"
        );
    }
    for n in 1..=15 {
        let rule = format!("R{n}");
        assert!(
            covered_rules.contains(&rule),
            "missing invalid vector for rule {rule}"
        );
    }
}

#[test]
fn unknown_fields_and_duplicate_keys_are_rejected_at_every_level() {
    let mut at_root = python_profile();
    at_root["unexpected"] = json!(true);
    assert!(parse(&at_root).is_err(), "root unknown field");

    let mut at_package = python_profile();
    at_package["package"]["unexpected"] = json!(true);
    assert!(parse(&at_package).is_err(), "package unknown field");

    let mut at_target = python_profile();
    at_target["targets"][0]["unexpected"] = json!(true);
    assert!(parse(&at_target).is_err(), "target unknown field");

    let mut at_hook = python_profile();
    at_hook["targets"][0]["hooks"]["tool.before"]["unexpected"] = json!(true);
    assert!(parse(&at_hook).is_err(), "hook capability unknown field");

    let mut at_from_field = python_profile();
    at_from_field["targets"][0]["fields"]["tool_name"]["unexpected"] = json!(true);
    assert!(parse(&at_from_field).is_err(), "from field unknown field");

    let mut at_const_field = python_profile();
    at_const_field["targets"][0]["fields"]["tool_name"] = json!({
        "const": "example-tool",
        "unexpected": true
    });
    assert!(parse(&at_const_field).is_err(), "const field unknown field");

    let mut at_stream = python_profile();
    at_stream["targets"][0]["stream"]["unexpected"] = json!(true);
    assert!(parse(&at_stream).is_err(), "stream unknown field");

    // R16: duplicate keys rejected by canonical::parse.
    let raw = serde_json::to_string(&python_profile()).unwrap();
    let duplicate = raw.replacen('{', "{\"runtime\":\"python\",", 1);
    assert!(
        InstrumentationProfile::parse(duplicate.as_bytes()).is_err(),
        "duplicate key at root"
    );
}

#[test]
fn compiler_maps_runtime_merges_conservatively_and_projects_capability_cells() {
    let py_doc = json!({
        "version": VERSION,
        "id": "example.multi.python",
        "origin": "builtin",
        "runtime": "python",
        "package": {
            "name": "example-sdk",
            "versions": ">=1.0,<2"
        },
        "targets": [
            {
                "id": "path-a",
                "module": "example_sdk.client.a",
                "qualname": "ClientA.call_tool",
                "call": "async",
                "hooks": {
                    "tool.before": {
                        "mode": "control",
                        "effects": ["observe", "block", "replace_input"],
                        "failure_actions": ["report", "block"]
                    },
                    "tool.after": {
                        "mode": "control",
                        "effects": ["observe", "replace_result"],
                        "failure_actions": ["report", "withhold_result"]
                    },
                    "input.submit": {
                        "mode": "control",
                        "effects": ["observe", "block"],
                        "failure_actions": ["block"]
                    }
                },
                "fields": {
                    "tool_name": { "from": "arg.0", "codec": "text" },
                    "input": { "from": "arg.1", "codec": "json" },
                    "result": { "from": "result", "codec": "json" },
                    "prompt": { "from": "arg.2", "codec": "text" }
                }
            },
            {
                "id": "path-b",
                "module": "example_sdk.client.b",
                "qualname": "ClientB.call_tool",
                "call": "async",
                "hooks": {
                    "tool.before": {
                        "mode": "control",
                        "effects": ["observe", "block"],
                        "failure_actions": ["report"]
                    },
                    "tool.after": {
                        "mode": "observe",
                        "effects": ["observe"],
                        "failure_actions": ["report"]
                    }
                },
                "fields": {
                    "tool_name": { "from": "arg.0", "codec": "text" },
                    "input": { "from": "arg.1", "codec": "json" },
                    "result": { "from": "result", "codec": "json" }
                }
            }
        ]
    });

    let py_profile = parse(&py_doc).unwrap();
    let py_cap = py_profile
        .source_capability("example-adapter", "sdk")
        .unwrap();
    py_cap.validate().unwrap();
    assert_eq!(py_cap.source_id(), "example.multi.python");
    assert_eq!(py_cap.kind(), SourceKind::Python);
    assert_eq!(py_cap.adapter(), "example-adapter");
    assert_eq!(py_cap.contract(), "example.multi.python");
    assert_eq!(py_cap.entrypoint(), "sdk");

    // Two control targets intersect effects and failure_actions.
    let before = py_cap.hook("tool.before").unwrap();
    assert_eq!(before.mode, HookAuthority::Control);
    assert_eq!(before.effects, vec![HookEffect::Observe, HookEffect::Block]);
    assert_eq!(before.failure_actions, vec![FailureAction::Report]);

    // Control + observe target downgrades to observe and drops control effects/actions.
    let after = py_cap.hook("tool.after").unwrap();
    assert_eq!(after.mode, HookAuthority::Observe);
    assert_eq!(after.effects, vec![HookEffect::Observe]);
    assert_eq!(after.failure_actions, vec![FailureAction::Report]);

    // Single-target event preserves full declared capabilities.
    let submit = py_cap.hook("input.submit").unwrap();
    assert_eq!(submit.mode, HookAuthority::Control);
    assert_eq!(submit.effects, vec![HookEffect::Observe, HookEffect::Block]);
    assert_eq!(submit.failure_actions, vec![FailureAction::Block]);

    // Projection into 16-Hook capability cells marks declared events Supported, others Unsupported.
    let cells = project_capability_cells("example-adapter", &[py_cap]).unwrap();
    assert_eq!(cells.len(), 16);
    let declared = BTreeSet::from(["tool.before", "tool.after", "input.submit"]);
    for event in HOOK_EVENTS {
        let cell = &cells[event];
        cell.validate().unwrap();
        if declared.contains(event) {
            assert_eq!(cell.status, CapabilityStatus::Supported, "{event}");
            assert_eq!(cell.sources, vec![SourceKind::Python], "{event}");
        } else {
            assert_eq!(cell.status, CapabilityStatus::Unsupported, "{event}");
            assert!(cell.sources.is_empty(), "{event}");
        }
    }

    // JS runtime maps to SourceKind::Js.
    let js_doc = json!({
        "version": VERSION,
        "id": "example.client.js",
        "origin": "builtin",
        "runtime": "js",
        "package": {
            "name": "@example/agent",
            "versions": ">=1.0"
        },
        "targets": [
            {
                "id": "run-tool",
                "module": "@example/agent/tools",
                "qualname": "ToolRunner.execute",
                "call": "async",
                "hooks": {
                    "tool.before": {
                        "mode": "control",
                        "effects": ["observe", "block"],
                        "failure_actions": []
                    }
                },
                "fields": {
                    "tool_name": { "from": "arg.0", "codec": "text" },
                    "input": { "from": "arg.1", "codec": "json" }
                }
            }
        ]
    });
    let js_profile = parse(&js_doc).unwrap();
    let js_cap = js_profile
        .source_capability("example-js-adapter", "loader")
        .unwrap();
    js_cap.validate().unwrap();
    assert_eq!(js_cap.kind(), SourceKind::Js);

    // Invalid adapter or entrypoint is rejected by SourceCapability::new.
    assert!(js_profile
        .source_capability("bad adapter", "loader")
        .is_err());
    assert!(js_profile
        .source_capability("example-js-adapter", " ")
        .is_err());
}

#[test]
fn schema_validator_directly_rejects_expressible_lexical_and_structural_violations() {
    let registry = Registry::new().unwrap();
    let vectors: Value = canonical::parse(include_bytes!(
        "fixtures/instrumentation-profile-vectors.json"
    ))
    .unwrap();
    let invalid = vectors["invalid"].as_array().unwrap();

    let schema_enforced_names: BTreeSet<&str> = BTreeSet::from([
        "r1_wrong_version",
        "r2_invalid_profile_id",
        "r3_invalid_origin",
        "r4_invalid_runtime",
        "r5_invalid_python_package_name",
        "r5_invalid_js_package_name_uppercase",
        "r5_versions_with_whitespace",
        "r5_versions_invalid_operator",
        "r5_versions_too_many_comparators",
        "r6_empty_targets",
        "r7_python_module_dunder_segment",
        "r7_js_module_relative_path",
        "r7_js_module_absolute_path",
        "r7_js_module_node_builtin",
        "r7_js_module_url",
        "r7_js_module_parent_traversal",
        "r8_qualname_dunder",
        "r8_qualname_wildcard",
        "r8_qualname_too_deep",
        "r9_invalid_call_shape",
        "r10_empty_hooks",
        "r10_non_instrumentable_runtime_observed",
        "r10_non_instrumentable_runtime_exited",
        "r10_non_instrumentable_coverage_changed",
        "r12_kwarg_rejected_for_js",
        "r12_arg_index_out_of_range",
        "r12_too_many_path_segments",
        "r12_dunder_path_segment",
        "r12_segment_index_out_of_range",
        "r13_invalid_codec",
        "r14_stream_max_items_exceeded",
        "r14_stream_max_bytes_exceeded",
        "r15_invalid_code_sha256_uppercase",
        "r15_custom_origin_missing_code_sha256",
    ]);

    let mut checked = BTreeSet::new();
    for entry in invalid {
        let name = entry["name"].as_str().unwrap();
        if schema_enforced_names.contains(name) {
            checked.insert(name);
            assert!(
                registry.validate(SCHEMA_NAME, &entry["document"]).is_err(),
                "Registry::validate should reject schema-expressible vector `{name}`"
            );
        }
    }
    assert_eq!(checked, schema_enforced_names);

    // Additional R12 lexical edge cases (invalid root, leading-zero indices) and line terminators.
    for bad_path in [
        "unknown.0",
        "arg.00",
        "arg.01",
        "arg.0.01",
        "kwarg.__secret",
        "arg.0\n",
    ] {
        let mut doc = python_profile();
        doc["targets"][0]["fields"]["tool_name"]["from"] = json!(bad_path);
        assert!(
            registry.validate(SCHEMA_NAME, &doc).is_err(),
            "schema should reject path `{bad_path}`"
        );
    }

    for suffix in ["\n", "\r", "\u{2028}", "\u{2029}"] {
        let mut bad_pkg = python_profile();
        bad_pkg["package"]["name"] = json!(format!("example-sdk{suffix}"));
        assert!(registry.validate(SCHEMA_NAME, &bad_pkg).is_err());

        let mut bad_mod = python_profile();
        bad_mod["targets"][0]["module"] = json!(format!("example_sdk.client{suffix}"));
        assert!(registry.validate(SCHEMA_NAME, &bad_mod).is_err());

        let mut bad_qual = python_profile();
        bad_qual["targets"][0]["qualname"] = json!(format!("ClientSession.call_tool{suffix}"));
        assert!(registry.validate(SCHEMA_NAME, &bad_qual).is_err());
    }
}

#[test]
fn security_violation_requires_candidate_output_before_capability_compilation() {
    let mut doc = python_profile();
    doc["targets"][0]["hooks"] = json!({
        "security.violation": {
            "mode": "control",
            "effects": ["observe", "block"],
            "failure_actions": ["block"]
        }
    });
    doc["targets"][0]["fields"] = json!({});

    // Parsing must reject the candidate before it can reach SourceCapability or resolution.
    assert!(matches!(
        parse(&doc),
        Err(aw_contracts::Error::Invariant(
            "security.violation target requires output field"
        ))
    ));

    doc["targets"][0]["fields"] = json!({
        "output": { "from": "result", "codec": "text" }
    });
    let profile = parse(&doc).unwrap();
    let source = profile.source_capability("example-adapter", "sdk").unwrap();
    source.validate().unwrap();
    let gate = source.hook("security.violation").unwrap();
    assert_eq!(gate.mode, HookAuthority::Control);
    assert_eq!(gate.effects, vec![HookEffect::Observe, HookEffect::Block]);
    assert_eq!(gate.failure_actions, vec![FailureAction::Block]);
}

#[test]
fn iterator_security_gate_reading_result_requires_buffering() {
    for call in ["async_iter", "sync_iter"] {
        for path in ["result", "result.text"] {
            let mut doc = python_profile();
            let target = &mut doc["targets"][0];
            target["call"] = json!(call);
            target["hooks"] = json!({
                "security.violation": {
                    "mode": "control",
                    "effects": ["observe", "block"],
                    "failure_actions": ["block"]
                }
            });
            target["fields"] = json!({
                "output": { "from": path, "codec": "text" }
            });
            target.as_object_mut().unwrap().remove("stream");
            for buffer in [None, Some(false), Some(true)] {
                if let Some(buffer) = buffer {
                    doc["targets"][0]["stream"] = json!({
                        "buffer": buffer, "max_items": 64, "max_bytes": 4096
                    });
                }
                let parsed = parse(&doc);
                if buffer == Some(true) {
                    let source = parsed
                        .unwrap()
                        .source_capability("example-adapter", "sdk")
                        .unwrap();
                    source.validate().unwrap();
                    let gate = source.hook("security.violation").unwrap();
                    assert_eq!(gate.mode, HookAuthority::Control);
                    assert!(gate.supports_effect(HookEffect::Block));
                } else {
                    assert!(matches!(
                        parsed,
                        Err(aw_contracts::Error::Invariant(
                            "iterator security.violation gate reading result requires buffered stream"
                        ))
                    ), "{call}, {path}, buffer={buffer:?}");
                }
            }
        }
    }
}

#[test]
fn iterator_security_observation_or_argument_gate_allows_unbuffered_stream() {
    for (mode, path) in [("observe", "result"), ("control", "arg.0")] {
        let mut doc = python_profile();
        let target = &mut doc["targets"][0];
        target["hooks"] = json!({
            "security.violation": {
                "mode": mode,
                "effects": if mode == "control" { json!(["observe", "block"]) }
                    else { json!(["observe"]) },
                "failure_actions": []
            }
        });
        target["fields"] = json!({
            "output": { "from": path, "codec": "text" }
        });
        target["stream"]["buffer"] = json!(false);
        let profile = parse(&doc).unwrap();
        let source = profile.source_capability("example-adapter", "sdk").unwrap();
        source.validate().unwrap();
    }
}

#[test]
fn js_modules_must_belong_to_the_declared_package() {
    let mut doc = python_profile();
    doc["runtime"] = json!("js");
    doc["package"]["name"] = json!("@example/sdk");
    for module in ["@example/other/tools", "@example/sdk-extra/tools"] {
        doc["targets"][0]["module"] = json!(module);
        assert!(matches!(
            parse(&doc),
            Err(aw_contracts::Error::Invariant(
                "js target module must belong to the declared package"
            ))
        ));
    }
    for module in ["@example/sdk", "@example/sdk/tools"] {
        doc["targets"][0]["module"] = json!(module);
        parse(&doc).unwrap();
    }
}

#[test]
fn js_module_package_prefix_214_215_boundary_matches_between_schema_and_rust() {
    let registry = Registry::new().unwrap();
    let make_js_doc = |module: String| {
        json!({
            "version": VERSION,
            "id": "example.js.boundary",
            "origin": "builtin",
            "runtime": "js",
            "package": {
                "name": "@example/agent",
                "versions": ">=1.0"
            },
            "targets": [
                {
                    "id": "t1",
                    "module": module,
                    "qualname": "Client.run",
                    "call": "async",
                    "hooks": {
                        "session.start": {
                            "mode": "observe",
                            "effects": ["observe"],
                            "failure_actions": []
                        }
                    },
                    "fields": {}
                }
            ]
        })
    };

    let accepted = [
        "a".repeat(214),
        format!("{}/ok", "a".repeat(214)),
        format!("@a/{}", "b".repeat(211)),
        format!("@a/{}/ok", "b".repeat(211)),
        format!("@{}/b", "a".repeat(211)),
        format!("@{}/b/ok", "a".repeat(211)),
    ];
    for module in accepted {
        let mut doc = make_js_doc(module.clone());
        let package_segments = if module.starts_with('@') { 2 } else { 1 };
        doc["package"]["name"] = json!(module
            .split('/')
            .take(package_segments)
            .collect::<Vec<_>>()
            .join("/"));
        assert!(
            registry.validate(SCHEMA_NAME, &doc).is_ok(),
            "schema should accept 214-byte prefix module `{module}`"
        );
        assert!(
            InstrumentationProfile::from_value(doc).is_ok(),
            "from_value should accept 214-byte prefix module `{module}`"
        );
    }

    let rejected = [
        "a".repeat(215),
        format!("{}/ok", "a".repeat(215)),
        format!("@a/{}", "b".repeat(212)),
        format!("@a/{}/ok", "b".repeat(212)),
        format!("@{}/b", "a".repeat(212)),
        format!("@{}/b/ok", "a".repeat(212)),
        format!("@{}/bb", "a".repeat(211)),
        format!("@{}/bb/ok", "a".repeat(211)),
    ];
    for module in rejected {
        let doc = make_js_doc(module.clone());
        assert!(
            registry.validate(SCHEMA_NAME, &doc).is_err(),
            "schema should reject 215-byte prefix module `{module}`"
        );
        assert!(
            InstrumentationProfile::from_value(doc).is_err(),
            "from_value should reject 215-byte prefix module `{module}`"
        );
    }
}
