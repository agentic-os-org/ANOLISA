use super::*;

#[test]
fn observation_only_launch_registers_two_noop_callbacks() {
    let profile = Profile::new("unknown: keep\n");
    let directory = profile.0.join("observe-launch");
    fs::create_dir(&directory).unwrap();
    let files = crate::test_support::Artifacts(directory);
    let mut prepared = Prepared {
        input: LaunchInput {
            document: json!({"spec":{"agents":{"hermes":{"evidence":{"mode":"auto"}}},
                "execution":{"default_event_budget_ms":1000},"events":{}}}),
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
        callback_timeout: 30.0,
    };
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
    assert_eq!(launch["hooks"].as_array().unwrap().len(), 2);
    for hook in launch["hooks"].as_array().unwrap() {
        assert_eq!(hook["step"], adapter::OBSERVE_STEP);
        assert_eq!(hook["on_error"], "report");
        assert_eq!(hook["replace_result"], false);
    }
}

#[test]
fn installation_uses_the_adapter_selection_snapshot() {
    let profile = Profile::new("unknown: keep\n");
    let validator = aw_config::Validator::new().unwrap();
    let example = validator
        .parse(include_bytes!(
            "../../../../../aw-service/examples/aw.hermes.yaml"
        ))
        .unwrap();
    let mut document = example.as_value().clone();
    document["spec"]["agents"]["hermes"]["argv"] = json!(["/snapshot-hermes", "gateway"]);
    let snapshot = validator
        .parse(&serde_json::to_vec(&document).unwrap())
        .unwrap();
    let path = profile.0.join("aw.json");
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let selected = adapter::select(
        snapshot.as_value()["spec"]["agents"]["hermes"]["adapter"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    document["spec"]["agents"]["hermes"]["adapter"] = json!("qoder");
    document["spec"]["agents"]["hermes"]["argv"] = json!(["/replacement-hermes", "chat"]);
    let replacement = serde_json::to_vec(&document).unwrap();
    validator.parse(&replacement).unwrap();
    let saved = profile.0.join("replacement.json");
    fs::write(&saved, replacement).unwrap();
    fs::rename(saved, &path).unwrap();
    let options = crate::InstallOptions {
        target: "hermes".into(),
        profile: Some(profile.0.to_str().unwrap().into()),
    };
    let error = selected
        .install(
            &options,
            &snapshot,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("local chat command only"), "{error}");
    assert!(!profile.0.join("plugins").exists());
    assert!(!profile.0.join(".aw-install.lock").exists());
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        b"unknown: keep\n"
    );
}

#[test]
fn empty_policy_keeps_native_command_and_zero_hook_readiness() {
    let profile = Profile::new("unknown: keep\n");
    let artifacts_path = profile.0.join("launch");
    fs::create_dir(&artifacts_path).unwrap();
    let files = crate::test_support::Artifacts(artifacts_path);
    let command = aw_exec::CommandSpec {
        program: "hermes".into(),
        args: vec!["chat".into(), "--cli".into()],
        cwd: profile.0.clone(),
        environment: BTreeMap::from([("PROFILE_SENTINEL".into(), "keep".into())]),
    };
    let mut prepared = Prepared {
        input: LaunchInput {
            document: json!({"spec":{"providers":{},"events":{}}}),
            target: "hermes".into(),
            options: crate::NativeOptions::default(),
            command,
        },
        profile: profile.0.clone(),
        python: "/usr/bin/python3".into(),
        callback_timeout: 30.0,
    };
    prepared.validate_steps(&[]).unwrap();
    let plan = prepared
        .configure(&LaunchContext {
            files: &files,
            binding_path: &profile.0.join("binding.json"),
            executable: std::path::Path::new("/usr/bin/aw"),
            steps: &[],
        })
        .unwrap();
    assert_eq!(plan.command.program, OsString::from("/usr/bin/python3"));
    assert_eq!(plan.command.cwd, profile.0);
    assert!(files.0.join("hermes-entrypoint.py").is_file());
    assert_eq!(plan.command.args[0], "-X");
    assert_eq!(
        plan.command.args[1],
        std::ffi::OsString::from(format!(
            "pycache_prefix={}",
            files.0.join("pycache").display()
        ))
    );
    assert!(plan.events.is_empty());
    assert_eq!(plan.readiness.unwrap().hooks, 0);
    assert_eq!(
        plan.command.args[3..],
        [
            OsString::from("hermes"),
            OsString::from("chat"),
            OsString::from("--cli")
        ]
    );
    assert_eq!(
        plan.provider_environment[&OsString::from("PROFILE_SENTINEL")],
        "keep"
    );
    let launch: Value =
        serde_json::from_slice(&fs::read(files.0.join("hermes-launch.json")).unwrap()).unwrap();
    assert_eq!(launch["hooks"], json!([]));
    use std::os::unix::ffi::OsStringExt;
    let invalid = profile.0.join(OsString::from_vec(b"invalid-\xff".to_vec()));
    let valid_directory = profile.0.join("other-launch");
    for (executable, binding, directory, expected) in [
        (
            invalid.as_path(),
            profile.0.as_path(),
            valid_directory.as_path(),
            "executable",
        ),
        (
            profile.0.as_path(),
            invalid.as_path(),
            valid_directory.as_path(),
            "binding",
        ),
        (
            profile.0.as_path(),
            profile.0.as_path(),
            invalid.as_path(),
            "launch directory",
        ),
    ] {
        let invalid_files = crate::test_support::Artifacts(directory.to_path_buf());
        let error = prepared
            .configure(&LaunchContext {
                files: &invalid_files,
                binding_path: binding,
                executable,
                steps: &[],
            })
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("UTF-8"), "{error}");
    }
}

#[test]
fn generated_profile_marks_only_the_admitted_replacement_step() {
    use aw_provider::admission::StepExecution;
    let profile = Profile::new("unknown: keep\n");
    let launch = profile.0.join("launch");
    fs::create_dir(&launch).unwrap();
    let files = crate::test_support::Artifacts(launch);
    let steps: Vec<_> = ["observe", "compress"]
        .into_iter()
        .map(|id| AdmittedStep {
            event: "tool.after".into(),
            step_id: id.into(),
            provider: "fixture".into(),
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
        input: LaunchInput {
            document: json!({"spec":{"execution":{"default_event_budget_ms":1000},"events":{}}}),
            target: "hermes".into(),
            options: crate::NativeOptions::default(),
            command: aw_exec::CommandSpec {
                program: "/hermes".into(),
                args: vec!["chat".into(), "--cli".into()],
                cwd: profile.0.clone(),
                environment: BTreeMap::new(),
            },
        },
        profile: profile.0.clone(),
        python: "/usr/bin/python3".into(),
        callback_timeout: 30.0,
    };
    let plan = prepared
        .configure(&LaunchContext {
            files: &files,
            binding_path: &profile.0.join("binding"),
            executable: std::path::Path::new("/aw"),
            steps: &steps,
        })
        .unwrap();
    let launch: Value =
        serde_json::from_slice(&fs::read(files.0.join("hermes-launch.json")).unwrap()).unwrap();
    assert_eq!(launch["hooks"][0]["replace_result"], false);
    assert_eq!(launch["hooks"][1]["replace_result"], true);
    assert_eq!(
        plan.events["tool.after"].replacement_step.as_deref(),
        Some("compress")
    );
}
