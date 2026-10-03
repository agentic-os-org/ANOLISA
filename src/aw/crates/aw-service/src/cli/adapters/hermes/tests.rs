use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
};

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
    ] {
        assert!(check_args(&argv(&args)).is_err());
    }
    assert!(check_args(&argv(&["chat", "--", "--safe-mode"])).is_ok());
    assert!(check_args(&argv(&["chat", "--cli", "--query=--safe"])).is_ok());
}

fn install_profile(profile: &std::path::Path) -> Result<Option<PathBuf>> {
    install::install(profile, |candidate, field, names| {
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
    let original = "plugins: {enabled: [], disabled: [aw-native-hooks]}\nunknown: keep\n";
    let profile = Profile::new(original);
    let mut calls = 0;
    let result = install::install(&profile.0, |candidate, _field, _names| {
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
    let result = install::install(&profile.0, |candidate, _field, _names| {
        fs::write(
            candidate.join("config.yaml"),
            "plugins: {enabled: [aw-native-hooks]}\n",
        )?;
        fs::write(profile.0.join("config.yaml"), "operator: concurrent edit\n")?;
        Ok(())
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("changed during installation"));
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        b"operator: concurrent edit\n"
    );
    assert!(!profile.0.join("plugins").exists());
}
