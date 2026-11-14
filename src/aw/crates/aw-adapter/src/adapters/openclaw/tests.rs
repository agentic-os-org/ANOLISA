use super::*;

#[test]
fn final_gate_admission_requires_an_explicit_verified_model_api() {
    let mut settings = json!({"agents":{"defaults":{"model":{"primary":"fixture/model"}}},
        "models":{"providers":{"fixture":{"api":"openai-completions"}}}});
    assert!(verified_model_api(&settings));
    settings["agents"]["defaults"]["model"]["fallbacks"] = json!(["other/model"]);
    assert!(!verified_model_api(&settings));
    settings["agents"]["defaults"]["model"]["fallbacks"] = json!([]);
    settings["models"]["providers"]["fixture"]["api"] = json!("anthropic-messages");
    assert!(!verified_model_api(&settings));
}

#[test]
fn changed_agent_core_bundle_cannot_admit_the_final_gate() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("node_modules/openclaw");
    fs::create_dir_all(package.join("dist")).unwrap();
    fs::write(package.join("package.json"), br#"{"version":"2026.9.6"}"#).unwrap();
    fs::write(package.join("dist/agent-core-DG5aPUnb.mjs"), b"changed").unwrap();
    let command = aw_exec::CommandSpec {
        program: "/usr/bin/node".into(),
        args: vec![package.join("openclaw.mjs").into_os_string()],
        cwd: root.path().into(),
        environment: BTreeMap::new(),
    };
    assert!(!verified_bundle(&command, "2026.9.6"));
}

#[test]
fn after_identity_matches_native_completion_case_and_aliases() {
    let binding = NativeContext {
        target: "target".into(),
        instance_id: "instance".into(),
        cwd: "/tmp".into(),
    };
    for (raw, canonical) in [
        ("Custom/Tool", "custom/tool"),
        ("Bash", "exec"),
        ("apply-patch", "apply_patch"),
        ("CRON", "automations"),
    ] {
        let native = json!({"hook":"after_tool_call","event":{"toolName":raw,"params":{},"toolCallId":"call","runId":"run"},"context":{"sessionId":"session"}});
        let normalized = OpenClaw.normalize(&binding, "tool.after", &native).unwrap();
        assert_eq!(normalized["tool"]["name"], canonical);
        assert_eq!(normalized["tool"]["native_name"], raw);
        assert_eq!(normalized["native"]["event"]["toolName"], raw);
        let mut before = native;
        before["hook"] = json!("before_tool_call");
        assert_eq!(
            OpenClaw
                .normalize(&binding, "tool.before", &before)
                .unwrap()["tool"]["name"],
            raw
        );
        let mut final_result = before;
        final_result["hook"] = json!("final_tool_result");
        final_result["event"]["result"] =
            json!({"content":[{"type":"text","text":"checked"}],"isError":false});
        let normalized = OpenClaw
            .normalize(&binding, "security.violation", &final_result)
            .unwrap();
        assert_eq!(
            normalized["tool"]["result"]["content"][0]["text"],
            "checked"
        );
        final_result["event"]["result"]["content"] = Value::Null;
        assert!(OpenClaw
            .normalize(&binding, "security.violation", &final_result)
            .is_err());
    }
}

#[test]
fn generated_profile_marks_only_the_admitted_replacement_step() {
    use aw_core::admission::StepExecution;
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tokenless-validation");
    fs::create_dir_all(&parent).unwrap();
    let root = tempfile::tempdir_in(parent).unwrap();
    let launch = root.path().join("launch");
    fs::create_dir(&launch).unwrap();
    let files = crate::test_support::Artifacts(launch);
    let steps: Vec<_> = ["observe", "compress"]
        .into_iter()
        .map(|id| AdmittedStep {
            event: "tool.after".into(),
            step_id: id.into(),
            provider: "fixture".into(),
            event_tools: vec!["*".into()],
            step_tools: vec!["*".into()],
            on_error: "report".into(),
            execution: StepExecution::Provider {
                operation: "record".into(),
                effects: vec![if id == "compress" {
                    "replace_result"
                } else {
                    "observe"
                }
                .into()],
            },
        })
        .collect();
    let mut prepared = Prepared {
        version: "2026.10.1".into(),
        bundle_verified: false,
        input: LaunchInput {
            document: json!({"spec":{"execution":{"default_event_budget_ms":1000},"events":{}}}),
            target: "openclaw".into(),
            options: crate::NativeOptions::default(),
            command: aw_exec::CommandSpec {
                program: "/native/openclaw".into(),
                args: vec!["gateway".into(), "run".into()],
                cwd: root.path().into(),
                environment: BTreeMap::new(),
            },
        },
        settings: json!({"gateway":{"mode":"local"}}),
        profile: root.path().join("profile"),
        _lock: File::create(root.path().join("lock")).unwrap(),
    };
    assert!(!prepared
        .capabilities()
        .events
        .contains_key("security.violation"));
    prepared.version = "2026.9.6".into();
    prepared.bundle_verified = true;
    prepared.settings["agents"]["defaults"]["model"]["primary"] = json!("fixture/model");
    prepared.settings["models"]["providers"]["fixture"]["api"] = json!("openai-completions");
    assert!(prepared
        .capabilities()
        .events
        .contains_key("security.violation"));
    prepared.version = "2026.10.1".into();
    let plan = prepared
        .configure(&LaunchContext {
            files: &files,
            binding_path: &root.path().join("binding"),
            executable: Path::new("/aw"),
            steps: &steps,
        })
        .unwrap();
    let settings: Value =
        serde_json::from_slice(&fs::read(files.0.join("openclaw.json")).unwrap()).unwrap();
    let hooks = &settings["plugins"]["entries"][PLUGIN]["config"]["hooks"]["after"];
    assert_eq!(hooks[0]["replaceResult"], false);
    assert_eq!(hooks[1]["replaceResult"], true);
    assert_eq!(
        plan.events["tool.after"].replacement_step.as_deref(),
        Some("compress")
    );
    assert!(files.0.join("openclaw-plugin/hooks.mjs").is_file());
}

#[test]
fn gateway_arguments_preserve_literal_values_and_version_prefix() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(
        check_args(&args(&["gateway", "run", "--port", "12345"])).unwrap(),
        args(&["--version"])
    );
    assert_eq!(
        check_args(&args(&["/native/openclaw.mjs", "gateway", "run"])).unwrap(),
        args(&["/native/openclaw.mjs", "--version"])
    );
    for input in [
        vec!["agent", "exec"],
        vec!["gateway", "stop"],
        vec!["gateway", "run", "--force"],
        vec!["gateway", "run", "--profile=another"],
        vec!["--dev", "gateway", "run"],
    ] {
        assert!(check_args(&args(&input)).is_err(), "{input:?}");
    }
}

#[test]
fn native_settings_require_local_gateway_and_enabled_unowned_plugin() {
    let config = json!({"gateway":{"mode":"local"},"plugins":{"allow":["existing"],
        "entries":{"existing":{"enabled":true}}}});
    check_settings(&config).unwrap();
    for path in ["$include", "disabled", "owned", "remote", "denied"] {
        let mut modified = config.clone();
        match path {
            "$include" => modified["models"] = json!({"$include":"models.json"}),
            "disabled" => modified["plugins"]["enabled"] = json!(false),
            "owned" => modified["plugins"]["entries"][PLUGIN] = json!({"enabled":false}),
            "remote" => modified["gateway"]["mode"] = json!("remote"),
            _ => modified["plugins"]["deny"] = json!([PLUGIN]),
        }
        assert!(check_settings(&modified).is_err(), "{path}");
    }
}

#[test]
fn budget_stays_below_native_before_timeout_and_accepts_integer_floats() {
    let mut config = json!({"spec":{"execution":{"default_event_budget_ms":5000.0},"events":{}}});
    assert_eq!(budget(&config, "tool.before").unwrap(), 5000);
    config["spec"]["events"]["tool.before"] = json!({"budget_ms":12001});
    assert!(budget(&config, "tool.before").is_err());
}

#[test]
fn response_returns_native_block_without_granting_permissions() {
    let output = OpenClaw.reply(true);
    assert!(matches!(output.exit, Exit::Code(0)));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["block"],
        true
    );
    assert_eq!(OpenClaw.reply(false).stdout, b"{}");
}

#[test]
fn normalized_events_obey_provider_contract_and_scope_call_ids_to_runs() {
    let binding = NativeContext {
        target: "target".into(),
        instance_id: "instance".into(),
        cwd: "/tmp".into(),
    };
    let mut native = json!({"hook":"before_tool_call","event":{"toolName":"exec",
        "toolCallId":"call","runId":"first","params":{"command":"literal $HOME"}},
        "context":{"sessionId":"session"}});
    let first = OpenClaw
        .normalize(&binding, "tool.before", &native)
        .unwrap();
    let protocol = aw_provider::Protocol::new().unwrap();
    protocol
        .bind_invocation(
            json!({"api_version":"aw-provider/v1alpha1","method":"invoke",
        "request_id":"request","operation":"check","config_revision":"a".repeat(64),
        "budget_ms":1000,"allowed_effects":["observe","block"],"config":{},"event":first}),
        )
        .unwrap();
    assert_eq!(
        OpenClaw
            .normalize(&binding, "tool.before", &native)
            .unwrap()["tool"]["call_id"],
        first["tool"]["call_id"]
    );
    native["event"]["runId"] = json!("second");
    assert_ne!(
        OpenClaw
            .normalize(&binding, "tool.before", &native)
            .unwrap()["tool"]["call_id"],
        first["tool"]["call_id"]
    );
    native["context"] = json!({});
    assert!(OpenClaw
        .normalize(&binding, "tool.before", &native)
        .is_err());
}
