use super::*;

#[test]
fn structured_steps_require_matching_provider_and_private_config_evidence() {
    let doc = document();
    let adapter = capabilities("qoder");
    assert_eq!(
        admit_policy(&doc, "qoder", &adapter, &BTreeMap::new()).unwrap_err(),
        "enabled step lacks Provider evidence"
    );
    let mut renamed = providers(&doc);
    let policy = renamed.remove("policy").unwrap();
    renamed.insert("other".into(), policy);
    assert_eq!(
        admit_policy(&doc, "qoder", &adapter, &renamed).unwrap_err(),
        "enabled step lacks Provider evidence"
    );
    let private_config = &doc["spec"]["providers"]["policy"]["config"];
    let missing_op = "Provider does not declare the requested operation";
    let missing_event = "Provider operation does not support the requested event";
    let missing_effect = "requested effect is not supported by the Provider";
    for (field, replacement, expected) in [
        ("name", json!("different-operation"), missing_op),
        ("events", json!(["tool.after"]), missing_event),
        ("effects", json!(["observe"]), missing_effect),
    ] {
        let mut operations = operations();
        operations[0][field] = replacement;
        let providers = evidence(private_config, operations);
        assert_eq!(
            admit_policy(&doc, "qoder", &adapter, &providers).unwrap_err(),
            expected
        );
    }

    let wrong = evidence(
        &json!({"secret": "must-not-appear-in-errors"}),
        operations(),
    );
    assert_eq!(
        admit_policy(&doc, "qoder", &adapter, &wrong).unwrap_err(),
        "Provider validation evidence does not match its private configuration"
    );
    let candidate = ENGINE.preflight(&bytes(&doc), "qoder", adapter).unwrap();
    let error = ENGINE.admit(candidate, &wrong).unwrap_err().to_string();
    assert!(error.contains("private configuration"));
    assert!(!error.contains("must-not-appear-in-errors") && !error.contains("threshold"));
}

#[test]
fn boolean_and_native_hooks_need_no_provider_description() {
    for explicit in [false, true] {
        let mut doc = document();
        let provider = doc["spec"]["providers"]["policy"].as_object_mut().unwrap();
        provider.remove("protocol");
        provider.remove("config");
        if explicit {
            provider.insert("protocol".into(), json!(aw_config::HOOK_PROTOCOL));
        }
        for event in ["tool.before", "tool.after"] {
            let step = doc["spec"]["events"][event]["steps"][0]
                .as_object_mut()
                .unwrap();
            step.remove("operation");
            if explicit {
                step.insert("operation".into(), json!("invoke"));
            }
        }
        doc["spec"]["events"]["tool.before"]["steps"][0]["reason_code"] = json!("rule_matched");
        for adapter in ADAPTERS {
            let policy =
                admit_policy(&doc, adapter, &capabilities(adapter), &BTreeMap::new()).unwrap();
            assert_eq!(policy.steps().len(), 2);
            assert_eq!(
                policy
                    .steps()
                    .iter()
                    .find(|step| step.event == "tool.before")
                    .unwrap()
                    .execution,
                StepExecution::Hook {
                    effects: vec!["observe".into(), "block".into()],
                    reason_code: Some("rule_matched".into()),
                }
            );
            assert_eq!(
                policy
                    .steps()
                    .iter()
                    .find(|step| step.event == "tool.after")
                    .unwrap()
                    .execution,
                StepExecution::Hook {
                    effects: vec!["observe".into()],
                    reason_code: None,
                }
            );
        }
    }

    let mut doc = document();
    doc["spec"]["providers"]["policy"]["protocol"] = json!("native-hook/v1alpha1");
    doc["spec"]["providers"]["policy"]["config"] = json!({});
    for event in ["tool.before", "tool.after"] {
        doc["spec"]["events"][event]["steps"] = json!([
            {"id": "raw", "provider": "policy", "native": {}, "on_error": "report"}
        ]);
    }
    for adapter in ADAPTERS {
        let policy = admit_policy(&doc, adapter, &capabilities(adapter), &BTreeMap::new()).unwrap();
        assert_eq!(policy.steps().len(), 2);
        assert!(policy
            .steps()
            .iter()
            .all(|step| step.execution == StepExecution::Native));
    }
    let mut unsupported = capabilities("qoder");
    unsupported.events.remove("tool.after");
    assert_eq!(
        admit_policy(&doc, "qoder", &unsupported, &BTreeMap::new()).unwrap_err(),
        "Adapter evidence lacks an enabled event"
    );
}

#[test]
fn admission_never_starts_configured_programs() {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("aw-core-admission-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let program = directory.join("probe");
    let marker = directory.join("probe.ran");
    fs::write(&program, "#!/bin/sh\n: > \"$0.ran\"\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

    let mut doc = document();
    let argv = json!([program.to_str().unwrap()]);
    for adapter in ADAPTERS {
        doc["spec"]["agents"][adapter]["argv"] = argv.clone();
    }
    doc["spec"]["providers"]["policy"]["transport"]["argv"] = argv;
    let providers = providers(&doc);
    for adapter in ADAPTERS {
        assert!(admit_policy(&doc, adapter, &capabilities(adapter), &providers).is_ok());
        assert!(admit_policy(&doc, adapter, &capabilities(adapter), &BTreeMap::new()).is_err());
    }
    assert!(!marker.exists(), "admission started a configured program");
    assert!(Command::new(&program).status().unwrap().success());
    assert!(marker.exists());
    fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn revision_is_the_digest_of_the_exact_input_bytes_and_snapshot_remains_bound() {
    let doc = document();
    let compact = bytes(&doc);
    let check_qoder = |raw: &[u8]| {
        ENGINE
            .preflight(raw, "qoder", capabilities("qoder"))
            .unwrap()
    };
    let first = check_qoder(&compact);
    let again = check_qoder(&compact);
    assert_eq!(first.revision(), again.revision());
    assert_eq!(first.revision(), digest(&compact));

    // Semantically equal documents with different bytes keep distinct revisions.
    let pretty = serde_json::to_vec_pretty(&doc).unwrap();
    let reformatted = check_qoder(&pretty);
    assert_eq!(
        reformatted.configuration().as_value(),
        first.configuration().as_value()
    );
    assert_eq!(reformatted.requirements(), first.requirements());
    assert_ne!(reformatted.revision(), first.revision());
    assert_eq!(reformatted.revision(), digest(&pretty));

    let yaml = include_bytes!("../../../aw-config/examples/aw.minimal.yaml");
    let json = bytes(CONFIGURATION.parse(yaml).unwrap().as_value());
    let from_yaml = check_qoder(yaml);
    let from_json = check_qoder(&json);
    assert_eq!(
        from_yaml.configuration().as_value(),
        from_json.configuration().as_value()
    );
    assert_eq!(from_yaml.revision(), digest(yaml));
    assert_ne!(from_yaml.revision(), from_json.revision());

    let mut adapter = capabilities("agentscope");
    adapter
        .failure_actions
        .insert("tool.after".into(), vec!["withhold_result".into()]);
    let candidate = ENGINE
        .preflight(&compact, "agentscope", adapter.clone())
        .unwrap();
    let revision = candidate.revision().to_owned();
    let configuration = candidate.configuration().as_value().clone();
    let requirements = candidate.requirements().to_vec();
    let policy = ENGINE.admit(candidate, &providers(&doc)).unwrap();
    assert_eq!(policy.revision(), revision);
    assert_eq!(policy.target(), "agentscope");
    assert_eq!(policy.configuration().as_value(), &configuration);
    assert_eq!(policy.steps(), requirements.as_slice());
    let bound = policy.capabilities();
    assert_eq!(
        (&bound.adapter, &bound.version, &bound.entrypoint),
        (&adapter.adapter, &adapter.version, &adapter.entrypoint)
    );
    assert_eq!(bound.events, adapter.events);
    assert_eq!(bound.failure_actions, adapter.failure_actions);
    let debug = format!("{policy:?}");
    assert!(debug.contains(&revision) && !debug.contains("threshold"));
}

#[test]
fn invalid_documents_fail_before_capability_rules() {
    for input in [
        b"{".as_slice(),
        b"{}",
        b"a: 1\na: 2\n",
        b"apiVersion: aw/v1alpha1\n---\nkind: AWConfiguration\n",
    ] {
        assert!(matches!(
            ENGINE.preflight(input, "qoder", capabilities("qoder")),
            Err(AdmissionError::Configuration(_))
        ));
    }
    for (pointer, replacement) in [
        ("/spec/agents/qoder/adapter", json!("unknown-adapter")),
        (
            "/spec/events/tool.before/steps/0/provider",
            json!("missing"),
        ),
        ("/spec/events/tool.before/steps/0/effects", json!(["ask"])),
        ("/spec/execution/guarantee", json!("protected")),
        ("/spec/providers/policy/protocol", json!("unknown/v1")),
        ("/spec/providers/policy/transport/type", json!("http")),
        ("/spec/providers/policy/transport/location", json!("daemon")),
    ] {
        let mut doc = document();
        *doc.pointer_mut(pointer).unwrap() = replacement;
        assert!(matches!(
            ENGINE.preflight(&bytes(&doc), "qoder", capabilities("qoder")),
            Err(AdmissionError::Configuration(_))
        ));
    }
}
