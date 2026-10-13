use super::*;
use std::collections::BTreeSet;

#[test]
fn callback_probe_ignores_project_modules_and_keeps_explicit_import_paths() {
    let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("python3"))
        .find(|path| path.is_file())
        .expect("Hermes tests require python3 on PATH");
    let project = Profile::new("{}");
    let native = Profile::new("{}");
    let package = native.0.join("hermes_cli");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    fs::write(package.join("main.py"), "").unwrap();
    fs::write(
        package.join("plugins.py"),
        "def _resolve_hook_callback_timeout(): return 30\n",
    )
    .unwrap();
    fs::write(
        project.0.join("hermes_cli.py"),
        "raise RuntimeError('project module must not be imported')\n",
    )
    .unwrap();
    let command = aw_exec::CommandSpec {
        program: "hermes".into(),
        args: vec![],
        cwd: project.0.clone(),
        environment: BTreeMap::from([("PYTHONPATH".into(), native.0.as_os_str().into())]),
    };
    let cache = tempfile::tempdir().unwrap();
    assert_eq!(
        callback_timeout(&command, &python, "default".as_ref(), cache.path()).unwrap(),
        30.0
    );
}

#[test]
fn profile_checks_optional_dotenv_without_following_links() {
    let profile = Profile::new("unknown: keep\n");
    let path = profile.0.to_str().unwrap();
    let dotenv = profile.0.join(".env");
    assert!(install::profile(path).is_ok());
    fs::write(&dotenv, "SENTINEL=keep\n").unwrap();
    for mode in [0o600, 0o644, 0o664, 0o666] {
        fs::set_permissions(&dotenv, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(install::profile(path).is_ok(), mode & 0o022 == 0);
        assert_eq!(fs::read(&dotenv).unwrap(), b"SENTINEL=keep\n");
    }
    fs::remove_file(&dotenv).unwrap();
    for target in [profile.0.join("config.yaml"), profile.0.join("missing")] {
        symlink(target, &dotenv).unwrap();
        assert!(install::profile(path)
            .unwrap_err()
            .to_string()
            .contains(".env"));
        fs::remove_file(&dotenv).unwrap();
    }
    fs::create_dir(&dotenv).unwrap();
    assert!(install::profile(path).is_err());
    fs::remove_dir(&dotenv).unwrap();
    let name = std::ffi::CString::new(dotenv.to_str().unwrap()).unwrap();
    // SAFETY: name is a valid C string inside this test's owned directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(install::profile(path).is_err());
}

#[test]
fn mismatched_or_incomplete_plugins_support_explicit_reinstallation() {
    let snapshot = |path: &std::path::Path| -> BTreeMap<_, _> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect()
    };
    for missing in [
        None,
        Some("plugin.yaml"),
        Some("__init__.py"),
        Some(".aw-owned"),
    ] {
        let profile = Profile::new("unknown: keep\n");
        install_profile(&profile.0).unwrap();
        let plugin = profile.0.join("plugins/aw-native-hooks");
        if let Some(name) = missing {
            fs::remove_file(plugin.join(name)).unwrap();
        } else {
            fs::write(plugin.join("__init__.py"), "# previous bundled revision\n").unwrap();
        }
        let saved = snapshot(&plugin);
        let original = fs::read(profile.0.join("config.yaml")).unwrap();
        let input = LaunchInput {
            document: json!({}),
            target: "hermes".into(),
            flags: BTreeMap::from([(
                "--native-profile".into(),
                profile.0.to_str().unwrap().into(),
            )]),
            command: aw_exec::CommandSpec {
                program: "/missing-hermes".into(),
                args: vec!["chat".into()],
                cwd: profile.0.clone(),
                environment: BTreeMap::new(),
            },
        };
        let launch_error = Hermes.prepare(input).err().unwrap().to_string();
        let install_error = install_profile(&profile.0).unwrap_err().to_string();
        for error in [launch_error, install_error] {
            assert!(error.contains(plugin.to_str().unwrap()), "{error}");
            assert!(error.contains("stop Hermes sessions"));
            assert!(error.contains("move this directory to a backup outside"));
            assert!(error.contains("rerun aw install"));
            if let Some(name) = missing {
                assert!(error.contains(name) && error.contains("No such file or directory"));
            }
        }
        assert_eq!(snapshot(&plugin), saved);
        let backup = profile.0.join("old-plugin");
        fs::rename(&plugin, &backup).unwrap();
        assert!(install_profile(&profile.0).unwrap().is_none());
        assert!(install::installed(&profile.0).is_ok());
        assert_eq!(snapshot(&backup), saved);
        assert_eq!(fs::read(profile.0.join("config.yaml")).unwrap(), original);
    }
}

#[test]
fn native_paths_check_interpreter_links_and_preserve_venv_selection() {
    let profile = Profile::new("{}");
    let bin = profile.0.join("bin");
    let shared = profile.0.join("shared");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&shared).unwrap();
    for path in [&bin, &shared] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let target = shared.join("python3");
    fs::write(&target, "interpreter fixture").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    let python = bin.join("python3");
    symlink(&target, &python).unwrap();
    let entry = bin.join("hermes");
    fs::write(
        &entry,
        format!("#!{}\nfrom hermes_cli.main import main\n", python.display()),
    )
    .unwrap();
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = aw_exec::CommandSpec {
        program: entry.clone(),
        args: vec![],
        cwd: profile.0.clone(),
        environment: BTreeMap::new(),
    };
    assert_eq!(python_entrypoint(&mut command).unwrap(), python);
    for path in [&target, &shared, &bin] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(python_entrypoint(&mut command)
            .unwrap_err()
            .to_string()
            .contains("permits modification"));
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(python_entrypoint(&mut command).unwrap(), python);
}

pub(super) fn install_profile(profile: &std::path::Path) -> Result<Option<PathBuf>> {
    install_profile_with(profile, |_| Err("unexpected plugin expansion".into()))
}

fn install_profile_with(
    profile: &std::path::Path,
    expand: impl FnMut(&[Vec<String>; 2]) -> Result<[BTreeSet<usize>; 2]>,
) -> Result<Option<PathBuf>> {
    let cancelled = AtomicBool::new(false);
    install::install(profile, &cancelled, expand, |candidate, field, names| {
        let path = candidate.join("config.yaml");
        let mut value = install::config(candidate)?;
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
fn installation_expands_aw_selections_and_preserves_other_templates() {
    let original = "unknown: ${SECRET}\nplugins: {enabled: [existing, '${AW_PLUGIN}'], disabled: ['${env:AW_PLUGIN}', '${OTHER_PLUGIN}']}\n";
    let profile = Profile::new(original);
    let expand = |names: &[Vec<String>; 2]| -> Result<[BTreeSet<usize>; 2]> {
        Ok(std::array::from_fn(|i| {
            names[i]
                .iter()
                .enumerate()
                .filter_map(|(index, name)| {
                    matches!(
                        name.as_str(),
                        "aw-native-hooks" | "${AW_PLUGIN}" | "${env:AW_PLUGIN}"
                    )
                    .then_some(index)
                })
                .collect()
        }))
    };
    let backup = install_profile_with(&profile.0, expand).unwrap().unwrap();
    assert_eq!(fs::read(backup).unwrap(), original.as_bytes());
    let config = install::installed(&profile.0).unwrap();
    assert_eq!(
        config["plugins"]["enabled"],
        serde_yaml_ng::to_value(["existing", "aw-native-hooks"]).unwrap()
    );
    assert_eq!(
        config["plugins"]["disabled"],
        serde_yaml_ng::to_value(["${OTHER_PLUGIN}"]).unwrap()
    );
    assert_eq!(config["unknown"].as_str(), Some("${SECRET}"));
    let installed = fs::read(profile.0.join("config.yaml")).unwrap();
    assert!(install_profile_with(&profile.0, expand).unwrap().is_none());
    assert_eq!(fs::read(profile.0.join("config.yaml")).unwrap(), installed);
}

#[test]
fn installation_rejects_invalid_native_selections_without_rewrite() {
    for invalid in [
        "plugins: {enabled: '*'}\n",
        "plugins: {disabled: [42]}\n",
        "plugins: {<<: 42}\n",
        "plugins: {<<: [{enabled: []}, 42]}\n",
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
    let original = "plugins: {enabled: ['${AW_PLUGIN}']}\n";
    let profile = Profile::new(original);
    let error = install_profile_with(&profile.0, |_| Ok([BTreeSet::from([1]), BTreeSet::new()]))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid plugin selection index"));
    assert_eq!(
        fs::read(profile.0.join("config.yaml")).unwrap(),
        original.as_bytes()
    );
    assert!(!profile.0.join("plugins/aw-native-hooks").exists());
}

#[test]
fn installation_releases_lock_with_an_inherited_descriptor() {
    use std::os::fd::{FromRawFd, OwnedFd};
    let profile = Profile::new("unknown: keep\n");
    let mut inherited = None;
    install::install(
        &profile.0,
        &AtomicBool::new(false),
        |_| Err("unexpected plugin expansion".into()),
        |candidate, _, _| {
            let lock = profile.0.join(".aw-install.lock");
            let descriptor = fs::read_dir("/proc/self/fd")?
                .filter_map(std::result::Result::ok)
                .find(|entry| fs::read_link(entry.path()).ok().as_ref() == Some(&lock))
                .ok_or("installation lock descriptor missing")?;
            let fd = descriptor.file_name().to_str().unwrap().parse::<i32>()?;
            // Model a concurrent fork retaining this open-file description until exec.
            // SAFETY: dup returns a new owned descriptor; OwnedFd closes it on exit.
            let fd = unsafe { libc::dup(fd) };
            assert!(fd >= 0);
            inherited = Some(unsafe { OwnedFd::from_raw_fd(fd) });
            fs::write(
                candidate.join("config.yaml"),
                "plugins: {enabled: [aw-native-hooks]}\n",
            )?;
            Ok(())
        },
    )
    .unwrap();
    assert!(inherited.is_some());
    assert!(install_profile(&profile.0).unwrap().is_none());
    drop(inherited);
}

#[test]
fn profile_rejects_non_utf8_canonical_targets_before_installation() {
    use std::os::unix::ffi::OsStringExt;
    let root = Profile::new("unknown: keep\n");
    let target = root.0.join(OsString::from_vec(vec![0xff]));
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    let profile = target.join("profile");
    fs::create_dir(&profile).unwrap();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(profile.join("config.yaml"), "unknown: keep\n").unwrap();
    let alias = root.0.join("mapped");
    symlink(&target, &alias).unwrap();
    let error = install::profile(alias.join("profile").to_str().unwrap())
        .unwrap_err()
        .to_string();
    assert!(error.contains("UTF-8"), "{error}");
    assert_eq!(
        fs::read(profile.join("config.yaml")).unwrap(),
        b"unknown: keep\n"
    );
    assert_eq!(fs::read_dir(&profile).unwrap().count(), 1);
}
