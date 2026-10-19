use super::*;
use aw_provider::admission::StepExecution;

fn prepared(profile: &Profile, mode: &str) -> Prepared {
    Prepared {
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
        on_error: "report".into(),
        execution: StepExecution::Provider {
            operation: "record".into(),
            effects: vec!["observe".into()],
        },
    }
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
