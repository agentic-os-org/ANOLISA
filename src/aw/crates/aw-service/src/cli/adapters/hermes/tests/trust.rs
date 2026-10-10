use super::*;

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
