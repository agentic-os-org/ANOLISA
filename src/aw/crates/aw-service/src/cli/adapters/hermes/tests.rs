use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    sync::atomic::{AtomicBool, Ordering},
};

mod cancellation;
mod dispatch;
mod trust;

#[test]
fn publication_retains_boundary_edits_and_open_descriptor_writes() {
    use std::io::Write;
    let cancelled = AtomicBool::new(false);
    let profile = Profile::new("original: keep\n");
    let live = profile.0.join("config.yaml");
    let candidate = profile.0.join("candidate.yaml");
    let backup = profile.0.join("config.yaml.aw-backup-test");
    fs::write(&candidate, "plugins: {enabled: [aw-native-hooks]}\n").unwrap();
    let replacement = profile.0.join("native-update.yaml");
    fs::write(&replacement, "operator: concurrent edit\n").unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(&replacement, &live).unwrap();
    let mut writer = fs::OpenOptions::new().append(true).open(&live).unwrap();
    let error = install::publish(&candidate, &live, b"original: keep\n", &backup, &cancelled)
        .unwrap_err()
        .to_string();
    let displaced = install::displaced(&backup);
    assert!(error.contains("candidate was published"));
    assert!(error.contains("configuration changed at publication"));
    assert!(error.contains(displaced.to_str().unwrap()));
    fs::remove_file(&candidate).unwrap();
    writer.write_all(b"late: descriptor write\n").unwrap();
    assert_eq!(
        fs::read(&displaced).unwrap(),
        b"operator: concurrent edit\nlate: descriptor write\n"
    );
    assert_eq!(
        fs::read(&live).unwrap(),
        b"plugins: {enabled: [aw-native-hooks]}\n"
    );
    fs::write(&candidate, "another candidate\n").unwrap();
    let missing = profile.0.join("missing.yaml");
    let failed_backup = profile.0.join("config.yaml.aw-backup-failed");
    assert!(install::publish(&candidate, &missing, b"", &failed_backup, &cancelled).is_err());
    assert!(!install::displaced(&failed_backup).exists());
    assert_eq!(
        fs::read(&displaced).unwrap(),
        b"operator: concurrent edit\nlate: descriptor write\n"
    );
}

#[test]
fn installation_preserves_a_concurrently_created_plugin_directory() {
    use std::os::unix::fs::MetadataExt;
    let cancelled = AtomicBool::new(false);
    let original = b"unknown: keep\n";
    let profile = Profile::new(std::str::from_utf8(original).unwrap());
    let plugins = profile.0.join("plugins");
    fs::create_dir(&plugins).unwrap();
    fs::set_permissions(&plugins, fs::Permissions::from_mode(0o700)).unwrap();
    let destination = plugins.join("aw-native-hooks");
    let mut inode = None;
    let result = install::install(&profile.0, &cancelled, |candidate, _, _| {
        fs::write(
            candidate.join("config.yaml"),
            "unknown: keep\nplugins: {enabled: [aw-native-hooks]}\n",
        )?;
        fs::create_dir(&destination)?;
        inode = Some(fs::metadata(&destination)?.ino());
        Ok(())
    });
    let error = result.unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(Some(fs::metadata(&destination).unwrap().ino()), inode);
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&plugins).unwrap().count(), 1);
    assert_eq!(fs::read(profile.0.join("config.yaml")).unwrap(), original);
    assert!(fs::read_dir(&profile.0).unwrap().all(|entry| {
        let name = entry.unwrap().file_name();
        let name = name.to_str().unwrap();
        !name.starts_with(".aw-config-") && !name.starts_with("config.yaml.aw-backup-")
    }));
}

#[test]
fn installation_with_enabled_missing_plugin_preserves_config() {
    use std::os::unix::fs::MetadataExt;
    let cancelled = AtomicBool::new(false);
    let original = "# keep\nplugins: {enabled: [aw-native-hooks]}\n";
    let profile = Profile::new(original);
    let config = profile.0.join("config.yaml");
    let inode = fs::metadata(&config).unwrap().ino();
    assert!(install::install(&profile.0, &cancelled, |_, _, _| panic!(
        "native writer must not run"
    ))
    .unwrap()
    .is_none());
    assert!(install::installed(&profile.0).is_ok());
    assert_eq!(fs::read(&config).unwrap(), original.as_bytes());
    assert_eq!(fs::metadata(&config).unwrap().ino(), inode);
    assert_eq!(fs::read_dir(&profile.0).unwrap().count(), 3);
}

#[test]
fn concurrent_installation_lock_leaves_profile_untouched() {
    use std::{
        fs::OpenOptions,
        os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    };
    let profile = Profile::new("unknown: keep\n");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(profile.0.join(".aw-install.lock"))
        .unwrap();
    // SAFETY: the descriptor is live and owned by this test.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(install_profile(&profile.0)
        .unwrap_err()
        .to_string()
        .contains("already held"));
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        b"unknown: keep\n"
    );
    assert!(!profile.0.join("plugins").exists());
}

struct Profile(PathBuf);

#[test]
fn profile_rejects_replaceable_ancestors_and_accepts_sticky_parents() {
    let parent = Profile::new("parent: keep\n");
    let leaf = parent.0.join("profile");
    fs::create_dir(&leaf).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(leaf.join("config.yaml"), "unknown: keep\n").unwrap();
    let alias = parent.0.join("alias");
    symlink(&leaf, &alias).unwrap();
    for mode in [0o775, 0o777] {
        fs::set_permissions(&parent.0, fs::Permissions::from_mode(mode)).unwrap();
        let error = install::profile(leaf.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("ancestor permits replacement"));
        assert!(error.contains(parent.0.to_str().unwrap()));
        assert_eq!(
            fs::read(leaf.join("config.yaml")).unwrap(),
            b"unknown: keep\n"
        );
    }
    for mode in [0o700, 0o755, 0o1777] {
        fs::set_permissions(&parent.0, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(install::profile(leaf.to_str().unwrap()).unwrap(), leaf);
    }
    // Resolve ancestor aliases before checking the paths Hermes will use.
    let nested = leaf.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(install::profile(alias.join("nested").to_str().unwrap()).is_err());
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        install::profile(alias.join("nested").to_str().unwrap()).unwrap(),
        nested
    );
    assert!(install::profile(alias.to_str().unwrap()).is_err());
}

impl Profile {
    fn new(config: &str) -> Self {
        let mut template = b"/tmp/aw-hermes-install-XXXXXX\0".to_vec();
        // SAFETY: mkdtemp receives a writable, terminated template; successful
        // creation gives this test exclusive ownership until Drop.
        let path = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        assert!(!path.is_null());
        let profile = Self(PathBuf::from(
            unsafe { std::ffi::CStr::from_ptr(path) }.to_str().unwrap(),
        ));
        fs::write(profile.0.join("config.yaml"), config).unwrap();
        fs::set_permissions(
            profile.0.join("config.yaml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        profile
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn install_preserves_unknown_values_credentials_and_original_backup() {
    let original = "# operator comment\nmodel: old-model\nplugins:\n  enabled: [existing]\n  disabled: [aw-native-hooks, other]\n  future: {nested: [true, 42]}\nunknown: preserved\n";
    let profile = Profile::new(original);
    fs::write(profile.0.join("auth.json"), "existing authorization").unwrap();
    fs::write(profile.0.join("state.db"), "existing sessions").unwrap();
    let backup = install_profile(&profile.0).unwrap().unwrap();
    assert_eq!(fs::read(&backup).unwrap(), original.as_bytes());
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let config = install::installed(&profile.0).unwrap();
    assert_eq!(config["unknown"].as_str(), Some("preserved"));
    assert_eq!(config["plugins"]["future"]["nested"][1].as_i64(), Some(42));
    assert_eq!(config["plugins"]["enabled"][0].as_str(), Some("existing"));
    assert_eq!(config["plugins"]["disabled"][0].as_str(), Some("other"));
    assert_eq!(
        fs::read(profile.0.join("auth.json")).unwrap(),
        b"existing authorization"
    );
    assert_eq!(
        fs::read(profile.0.join("state.db")).unwrap(),
        b"existing sessions"
    );
    let installed = fs::read(profile.0.join("config.yaml")).unwrap();
    assert!(install_profile(&profile.0).unwrap().is_none());
    assert_eq!(fs::read(profile.0.join("config.yaml")).unwrap(), installed);
}

#[test]
fn installation_refuses_foreign_plugin_and_symlink_config() {
    let profile = Profile::new("model: existing\n");
    let plugin = profile.0.join("plugins/aw-native-hooks");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("__init__.py"), "# operator owned").unwrap();
    assert!(install_profile(&profile.0).is_err());
    assert_eq!(
        fs::read(plugin.join("__init__.py")).unwrap(),
        b"# operator owned"
    );
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        b"model: existing\n"
    );
    fs::rename(profile.0.join("config.yaml"), profile.0.join("actual.yaml")).unwrap();
    symlink("actual.yaml", profile.0.join("config.yaml")).unwrap();
    assert!(install_profile(&profile.0).is_err());
}

#[test]
fn installation_rejects_invalid_native_selections_without_rewrite() {
    for invalid in [
        "plugins: {enabled: '*'}\n",
        "plugins: {disabled: [42]}\n",
        "[1, 2]\n",
    ] {
        let profile = Profile::new(invalid);
        assert!(install_profile(&profile.0).is_err());
        assert_eq!(
            fs::read(profile.0.join("config.yaml")).unwrap(),
            invalid.as_bytes()
        );
        assert!(!profile.0.join("plugins/aw-native-hooks").exists());
    }
}

fn binding() -> HookBinding {
    HookBinding {
        adapter: "hermes".into(),
        cwd: "/work".into(),
        socket: "/aw.sock".into(),
        events: BTreeMap::new(),
        binding: aw_service::Binding {
            identity: aw_service::Identity {
                generation: "one".into(),
                config_revision: "revision".into(),
            },
            target: "hermes".into(),
            instance_id: "instance".into(),
            audit_key: "audit".into(),
        },
    }
}

#[test]
fn native_ids_and_error_results_are_not_fabricated_or_decoded() {
    let native = json!({"hook_event_name":"post_tool_call", "cwd":"/work", "session_id":"session",
        "tool_name":"arbitrary.tool", "tool_input":{"nested":[true,"猫"]},
        "extra":{"tool_call_id":"call", "api_request_id":"request-one", "result":"{\"error\":\"blocked\"}", "status":"blocked"}});
    let event = Hermes.normalize(&binding(), "tool.after", &native).unwrap();
    assert!(event["tool"]["call_id"]
        .as_str()
        .unwrap()
        .starts_with("hermes:"));
    assert_eq!(event["tool"]["native_name"], "arbitrary.tool");
    assert_eq!(event["tool"]["result"], native["extra"]["result"]);
    assert_eq!(event["native"], native);
    let mut another = native.clone();
    another["extra"]["api_request_id"] = "request-two".into();
    assert_ne!(
        Hermes
            .normalize(&binding(), "tool.after", &another)
            .unwrap()["tool"]["call_id"],
        event["tool"]["call_id"]
    );
    let mut before = native.clone();
    before["hook_event_name"] = "pre_tool_call".into();
    assert_eq!(
        Hermes
            .normalize(&binding(), "tool.before", &before)
            .unwrap()["tool"]["call_id"],
        event["tool"]["call_id"]
    );
    let mut missing_request = native.clone();
    missing_request["extra"]["api_request_id"] = Value::Null;
    assert!(Hermes
        .normalize(&binding(), "tool.after", &missing_request)
        .is_err());
    let mut missing = native;
    missing["extra"]["tool_call_id"] = Value::Null;
    assert!(Hermes
        .normalize(&binding(), "tool.after", &missing)
        .is_err());
}

#[test]
fn native_cli_rejects_profile_switches_and_unverified_entrypoints() {
    let argv = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(check_args(&argv(&["chat", "--oneshot", "--query", "hello"])).is_ok());
    for args in [
        vec!["gateway", "run"],
        vec!["chat", "--safe-mode"],
        vec!["chat", "--profile=other"],
        vec!["chat", "--tui"],
        vec!["chat", "--native"],
        vec!["chat", "--tui-native"],
        vec!["chat", "--safe"],
        vec!["chat", "--nat"],
        vec!["chat", "--tui-n"],
        vec!["chat", "-c"],
        vec!["chat", "-w"],
        vec!["chat", "--worktree"],
        vec!["chat", "--in", "/other"],
        vec!["chat", "--help"],
        vec!["chat", "-h"],
    ] {
        assert!(check_args(&argv(&args)).is_err());
    }
    assert!(check_args(&argv(&["chat", "--", "--safe-mode"])).is_ok());
    assert!(check_args(&argv(&["chat", "--cli", "--query=--safe"])).is_ok());
    assert!(check_args(&argv(&["chat", "--query=--help"])).is_ok());
    assert!(check_args(&argv(&["chat", "--query=-h"])).is_ok());
}

fn install_profile(profile: &std::path::Path) -> Result<Option<PathBuf>> {
    let cancelled = AtomicBool::new(false);
    install::install(profile, &cancelled, |candidate, field, names| {
        let path = candidate.join("config.yaml");
        let mut value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&fs::read(&path)?)?;
        if value.get("plugins").is_none() {
            value.as_mapping_mut().unwrap().insert(
                "plugins".into(),
                serde_yaml_ng::Value::Mapping(Default::default()),
            );
        }
        value["plugins"][field.strip_prefix("plugins.").unwrap()] = serde_yaml_ng::to_value(names)?;
        fs::write(path, serde_yaml_ng::to_string(&value)?)?;
        Ok(())
    })
}

#[test]
fn native_writer_failure_and_concurrent_edits_leave_live_profile_untouched() {
    let cancelled = AtomicBool::new(false);
    let original = "plugins: {enabled: [], disabled: [aw-native-hooks]}\nunknown: keep\n";
    let profile = Profile::new(original);
    let mut calls = 0;
    let result = install::install(&profile.0, &cancelled, |candidate, _field, _names| {
        calls += 1;
        assert_ne!(candidate, profile.0);
        if calls == 2 {
            return Err("native writer failed".into());
        }
        fs::write(candidate.join("config.yaml"), "candidate partially changed")?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(calls, 2);
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        original.as_bytes()
    );
    assert!(!profile.0.join("plugins").exists());
    assert!(fs::read_dir(&profile.0).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".aw-config-")));
    assert!(fs::read_dir(&profile.0).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("config.yaml.aw-backup-")));
    let result = install::install(&profile.0, &cancelled, |candidate, _field, _names| {
        fs::write(
            candidate.join("config.yaml"),
            "plugins: {enabled: [aw-native-hooks]}\n",
        )?;
        fs::write(profile.0.join("config.yaml"), "operator: concurrent edit\n")?;
        Ok(())
    });
    let error = result.unwrap_err().to_string();
    assert!(error.contains("changed during installation"));
    assert!(!error.contains("backup"));
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        b"operator: concurrent edit\n"
    );
    assert!(!profile.0.join("plugins").exists());
    assert!(fs::read_dir(&profile.0).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("config.yaml.aw-backup-")));
    let invalid = Profile::new(original);
    assert!(install::install(&invalid.0, &cancelled, |candidate, _, _| {
        fs::write(candidate.join("config.yaml"), "invalid: [\n")?;
        Ok(())
    })
    .is_err());
    assert_eq!(
        fs::read(invalid.0.join("config.yaml")).unwrap(),
        original.as_bytes()
    );
    assert!(fs::read_dir(&invalid.0).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("config.yaml.aw-backup-")));
}

#[test]
fn empty_policy_keeps_native_command_and_zero_hook_readiness() {
    let profile = Profile::new("unknown: keep\n");
    let artifacts_path = profile.0.join("launch");
    fs::create_dir(&artifacts_path).unwrap();
    let files = super::super::super::run::Artifacts(artifacts_path);
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
            flags: BTreeMap::new(),
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
    assert!(plan.events.is_empty());
    assert_eq!(plan.readiness.unwrap().hooks, 0);
    assert_eq!(
        plan.command.args[1..],
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
}

#[test]
fn managed_profile_is_rejected_before_install_config_or_native_probe() {
    let profile = Profile::new("unknown: keep\n");
    fs::write(profile.0.join(".container-mode"), "managed").unwrap();
    let config = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!("../../../../examples/aw.hermes.yaml"))
        .unwrap();
    let args = Arguments::parse(
        [
            "--native-profile".into(),
            profile.0.to_str().unwrap().into(),
            "--config".into(),
            "/missing-aw-config".into(),
            "--agent".into(),
            "hermes".into(),
        ]
        .into_iter(),
    )
    .unwrap();
    assert!(Hermes
        .install(&args, &config)
        .err()
        .unwrap()
        .to_string()
        .contains("managed-container"));
    assert!(install::profile(profile.0.to_str().unwrap())
        .unwrap_err()
        .to_string()
        .contains("managed-container"));
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        b"unknown: keep\n"
    );
    assert!(!profile.0.join("plugins").exists());
}

#[test]
fn native_entrypoint_uses_its_venv_python_and_rejects_shell_wrappers() {
    let profile = Profile::new("unknown: keep\n");
    let executable = profile.0.join("hermes");
    let command = aw_exec::CommandSpec {
        program: executable.as_os_str().into(),
        args: vec![],
        cwd: profile.0.clone(),
        environment: BTreeMap::new(),
    };
    fs::write(
        &executable,
        "#!/usr/bin/python3\nfrom hermes_cli.main import main\nmain()\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        python_entrypoint(&command).unwrap(),
        PathBuf::from("/usr/bin/python3")
    );
    fs::write(
        &executable,
        "#!/bin/sh\nexec /usr/bin/python3 -m hermes_cli.main\n",
    )
    .unwrap();
    assert!(python_entrypoint(&command)
        .unwrap_err()
        .to_string()
        .contains("Python console-script"));
}

#[test]
fn native_entrypoint_skips_non_executable_path_matches() {
    let profile = Profile::new("unknown: keep\n");
    let first = profile.0.join("first");
    let second = profile.0.join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(first.join("hermes"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(first.join("hermes"), fs::Permissions::from_mode(0o600)).unwrap();
    let script = second.join("hermes");
    fs::write(
        &script,
        "#!/usr/bin/python3\nfrom hermes_cli.main import main\nmain()\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let command = aw_exec::CommandSpec {
        program: "hermes".into(),
        args: vec![],
        cwd: profile.0.clone(),
        environment: BTreeMap::from([(
            "PATH".into(),
            std::env::join_paths([&first, &second]).unwrap(),
        )]),
    };
    assert_eq!(
        python_entrypoint(&command).unwrap(),
        PathBuf::from("/usr/bin/python3")
    );
    fs::set_permissions(&script, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(python_entrypoint(&command)
        .unwrap_err()
        .to_string()
        .contains("not found in PATH"));
}

#[test]
fn installation_rejects_writable_plugin_and_config_files_without_rewriting() {
    let profile = Profile::new("unknown: keep\n");
    install_profile(&profile.0).unwrap();
    for directory in [
        profile.0.clone(),
        profile.0.join("plugins"),
        profile.0.join("plugins/aw-native-hooks"),
    ] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    }
    for name in [
        "plugins/aw-native-hooks/plugin.yaml",
        "plugins/aw-native-hooks/__init__.py",
        "plugins/aw-native-hooks/.aw-owned",
        "config.yaml",
    ] {
        let path = profile.0.join(name);
        let original = fs::read(&path).unwrap();
        for mode in [0o664, 0o666] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(install::installed(&profile.0)
                .unwrap_err()
                .to_string()
                .contains("not writable by others"));
            assert!(install_profile(&profile.0)
                .unwrap_err()
                .to_string()
                .contains("not writable by others"));
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(install::installed(&profile.0).is_ok());
        assert!(install_profile(&profile.0).unwrap().is_none());
    }
}
