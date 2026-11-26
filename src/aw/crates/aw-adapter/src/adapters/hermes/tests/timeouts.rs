use super::*;
use aw_core::admission::StepExecution;

fn prepared(profile: &Profile, mode: &str) -> Prepared {
    Prepared {
        revision: "a".repeat(40),
        gateway: false,
        input: LaunchInput {
            document: json!({"spec":{"agents":{"hermes":{"evidence":{"mode":mode}}},
                "execution":{"default_event_budget_ms":1001},"events":{}}}),
            target: "hermes".into(),
            options: crate::NativeOptions::default(),
            command: aw_exec::CommandSpec {
                program: "hermes".into(),
                args: vec!["chat".into()],
                cwd: profile.0.clone(),
                environment: BTreeMap::new(),
            },
        },
        profile: profile.0.clone(),
        python: "/usr/bin/python3".into(),
        callback_timeout: 6.0,
    }
}

fn action(event: &str) -> AdmittedStep {
    AdmittedStep {
        event: event.into(),
        step_id: "record".into(),
        provider: "fixture".into(),
        event_tools: vec!["*".into()],
        step_tools: vec!["*".into()],
        on_error: "report".into(),
        execution: StepExecution::Provider {
            operation: "record".into(),
            effects: vec!["observe".into()],
        },
    }
}

#[test]
fn strict_result_requires_the_verified_history_commit() {
    let profile = Profile::new("plugins: {}\n");
    let mut prepared = prepared(&profile, "auto");
    assert!(!prepared
        .capabilities()
        .failure_actions
        .contains_key("tool.after"));
    prepared.revision = VERIFIED_GATEWAY.into();
    for gateway in [false, true] {
        prepared.gateway = gateway;
        assert_eq!(
            prepared.capabilities().failure_actions["tool.after"],
            ["withhold_result"]
        );
    }
    let directory = profile.0.join("strict");
    fs::create_dir(&directory).unwrap();
    let files = crate::test_support::Artifacts(directory);
    let mut after = action("tool.after");
    after.on_error = "withhold_result".into();
    prepared
        .configure(&LaunchContext {
            files: &files,
            binding_path: &profile.0.join("binding"),
            executable: std::path::Path::new("/usr/bin/aw"),
            steps: &[after],
        })
        .unwrap();
    let launch: Value =
        serde_json::from_slice(&fs::read(files.0.join("hermes-launch.json")).unwrap()).unwrap();
    assert_eq!(launch["final_result"], true);
}

#[test]
fn observation_only_timeout_admission_matches_registered_budget() {
    let profile = Profile::new("unknown: keep\n");
    let mut prepared = prepared(&profile, "auto");
    for (timeout, accepted) in [
        (0.0, true),
        (1.0, false),
        (5.9, false),
        (6.0, true),
        (7.0, true),
    ] {
        prepared.callback_timeout = timeout;
        let result = prepared.validate_steps(&[]);
        assert_eq!(result.is_ok(), accepted, "timeout={timeout}: {result:?}");
        if let Err(error) = result {
            assert!(error.to_string().contains("plugins.hook_callback_timeout"));
        }
    }
    prepared.callback_timeout = 6.0;
    prepared.validate_steps(&[]).unwrap();
    let directory = profile.0.join("launch");
    fs::create_dir(&directory).unwrap();
    let files = crate::test_support::Artifacts(directory);
    let plan = prepared
        .configure(&LaunchContext {
            files: &files,
            binding_path: &profile.0.join("binding"),
            executable: std::path::Path::new("/usr/bin/aw"),
            steps: &[],
        })
        .unwrap();
    assert_eq!(plan.readiness.unwrap().hooks, 2);
    let launch: Value =
        serde_json::from_slice(&fs::read(files.0.join("hermes-launch.json")).unwrap()).unwrap();
    for hook in launch["hooks"].as_array().unwrap() {
        assert_eq!(hook["step"], adapter::OBSERVE_STEP);
        assert_eq!(hook["timeout"], 6);
    }
}

#[test]
fn mixed_routes_validate_the_synthesized_event_budget() {
    let profile = Profile::new("unknown: keep\n");
    for (observed, configured) in [("tool.before", "tool.after"), ("tool.after", "tool.before")] {
        let mut prepared = prepared(&profile, "auto");
        prepared.input.document["spec"]["events"][observed] = json!({"budget_ms":2001});
        let steps = [action(configured)];
        assert!(prepared.validate_steps(&steps).is_err(), "{observed}");
        prepared.callback_timeout = 7.0;
        prepared.validate_steps(&steps).unwrap();
        prepared.input.document["spec"]["events"][observed]["budget_ms"] = json!(55001);
        assert!(prepared.validate_steps(&steps).is_err(), "{observed}");
    }
}

#[test]
fn disabled_observation_leaves_only_action_timeouts_to_validate() {
    let profile = Profile::new("unknown: keep\n");
    let mut prepared = prepared(&profile, "off");
    prepared.callback_timeout = 1.0;
    prepared.validate_steps(&[]).unwrap();
    assert!(prepared.validate_steps(&[action("tool.before")]).is_err());
    prepared.callback_timeout = 6.0;
    prepared.validate_steps(&[action("tool.before")]).unwrap();
    prepared.input.document["spec"]["agents"]["hermes"]
        .as_object_mut()
        .unwrap()
        .remove("evidence");
    prepared.callback_timeout = 1.0;
    prepared.validate_steps(&[]).unwrap();
}

#[test]
fn reloadable_routes_preserve_the_full_hermes_budget() {
    let profile = Profile::new("unknown: keep\n");
    let mut prepared = prepared(&profile, "off");
    let validator = aw_config::Validator::new().unwrap();
    prepared.input.document = validator
        .parse(include_bytes!(
            "../../../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap()
        .as_value()
        .clone();
    prepared.input.document["spec"]["agents"] = json!({"hermes": {
        "adapter":"hermes", "argv":["hermes","chat"]}});
    prepared.input.document["spec"]["execution"]["default_event_budget_ms"] = json!(55000);
    prepared.input.options.reloadable = true;
    for (timeout, accepted) in [(0.0, true), (17.0, false), (59.9, false), (60.0, true)] {
        prepared.callback_timeout = timeout;
        let result = prepared.validate_steps(&[]);
        assert_eq!(result.is_ok(), accepted, "timeout={timeout}: {result:?}");
        if let Err(error) = result {
            assert!(error.to_string().contains("60 seconds"));
        }
    }
    prepared.callback_timeout = 60.0;
    let directory = profile.0.join("launch");
    fs::create_dir(&directory).unwrap();
    let files = crate::test_support::Artifacts(directory);
    let plan = prepared
        .configure(&LaunchContext {
            files: &files,
            binding_path: &profile.0.join("binding"),
            executable: std::path::Path::new("/usr/bin/aw"),
            steps: &[],
        })
        .unwrap();
    assert_eq!(plan.readiness.unwrap().hooks, 2);
    let launch: Value =
        serde_json::from_slice(&fs::read(files.0.join("hermes-launch.json")).unwrap()).unwrap();
    for hook in launch["hooks"].as_array().unwrap() {
        assert_eq!(hook["timeout"], 59);
    }
    prepared.input.document["spec"]["execution"]["default_event_budget_ms"] = json!(55001);
    assert!(prepared.validate_steps(&[]).is_err());
}

#[test]
fn hermes_gateway_refuses_skill_check_for_each_failure_action() {
    let validator = aw_config::Validator::new().unwrap();
    let mut config = validator
        .parse(include_bytes!(
            "../../../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap()
        .as_value()
        .clone();
    config["spec"]["agents"]["hermes"] = json!({"adapter":"hermes","argv":["hermes","chat"]});
    config["spec"]["providers"]["security"] = json!({
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":["/bin/true"]},
        "timeout_ms":1000,"max_output_bytes":4096,"config":{}
    });
    let hermes = crate::adapter::catalog::HERMES;
    for failure in ["report", "block"] {
        config["spec"]["events"]["tool.before"]["steps"] = json!([{
            "id":"skill","provider":"security","operation":"check_skill",
            "match":{"tools":["native:hermes:skill_view"]},
            "effects":["observe","block"],"on_error":failure
        }]);
        let error =
            crate::reload::validate(&config, "hermes", &hermes.capabilities("0.23.1", "gateway"))
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not support Skill checking"),
            "{error}"
        );
        crate::reload::validate(&config, "hermes", &hermes.capabilities("0.23.1", "chat")).unwrap();
    }
}
