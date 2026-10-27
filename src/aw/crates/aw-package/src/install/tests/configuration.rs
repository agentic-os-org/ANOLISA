//! Configuration publication and installed-package validation contracts.

use super::*;

#[test]
fn configuration_holds_a_read_lock_through_publication() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let config = f.root.join("aw.yaml");
    let runtime = tempfile::Builder::new()
        .prefix("cfg-")
        .tempdir_in(f.root.parent().unwrap())
        .unwrap();
    fs::set_permissions(runtime.path(), Permissions::from_mode(0o700)).unwrap();
    let state = runtime.path().join("state");
    let settings = crate::Settings {
        prefix: &f.prefix,
        config: &config,
        state: &state,
        policy: crate::Policy::SecCore {
            socket: Path::new("/run/aw-test/sec.sock"),
        },
        qoder: Some(Path::new("/bin/true")),
        node: Some(Path::new("/bin/true")),
        openclaw: Some(Path::new("/bin/true")),
    };
    let held = paths::lock(&f.prefix).unwrap();
    assert!(
        matches!(crate::configure(&settings), Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert!(!config.exists());
    drop(held);
    let lock_path = f.prefix.join(".aw-install.lock");
    assert_eq!(
        lock_path.metadata().unwrap().permissions().mode() & 0o777,
        0o644
    );
    crate::configuration::configure_using(&settings, |path, bytes| {
        assert!(matches!(uninstall(&f.prefix), Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock));
        assert!(matches!(install(&f.provider, &f.prefix), Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock));
        let reader = paths::shared_lock(&f.prefix)?;
        assert_eq!(reader.metadata()?.permissions().mode() & 0o777, 0o644);
        assert_eq!(reader.metadata()?.ino(), lock_path.metadata()?.ino());
        paths::write_private(path, bytes)
    }).unwrap();
    let held = paths::lock(&f.prefix).unwrap();
    assert!(config.exists());
    drop(held);
    fs::remove_file(&config).unwrap();
    // A reader may not repair permissions or open the lock for writing.
    fs::set_permissions(&lock_path, Permissions::from_mode(0o444)).unwrap();
    crate::configure(&settings).unwrap();
    assert_eq!(
        lock_path.metadata().unwrap().permissions().mode() & 0o777,
        0o444
    );
    fs::remove_file(&config).unwrap();
    fs::remove_file(&lock_path).unwrap();
    assert!(crate::configure(&settings).is_err());
    assert!(!config.exists() && !lock_path.exists());
    symlink(f.prefix.join("bin/aw"), &lock_path).unwrap();
    assert!(crate::configure(&settings).is_err());
    assert!(!config.exists());
}

#[test]
fn configuration_rejects_state_file_collisions_before_publication() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let runtime = tempfile::Builder::new()
        .prefix("cfg-")
        .tempdir_in(f.root.parent().unwrap())
        .unwrap();
    fs::set_permissions(runtime.path(), Permissions::from_mode(0o700)).unwrap();
    let config = runtime.path().join("aw.yaml");
    for state in [&config, &config.join("state"), &f.prefix.join("bin/aw")] {
        let settings = crate::Settings {
            prefix: &f.prefix,
            config: &config,
            state,
            policy: crate::Policy::SecCore {
                socket: Path::new("/run/aw-test/sec.sock"),
            },
            qoder: Some(Path::new("/bin/true")),
            node: Some(Path::new("/bin/true")),
            openclaw: Some(Path::new("/bin/true")),
        };
        assert!(crate::configure(&settings)
            .unwrap_err()
            .to_string()
            .contains("state must be a directory"));
        assert!(!config.exists());
    }
}

#[test]
fn configuration_requires_an_existing_private_state_directory() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let runtime = tempfile::Builder::new()
        .prefix("cfg-")
        .tempdir_in(f.root.parent().unwrap())
        .unwrap();
    fs::set_permissions(runtime.path(), Permissions::from_mode(0o700)).unwrap();
    let config = runtime.path().join("aw.yaml");
    let state = runtime.path().join("state");
    fs::create_dir(&state).unwrap();
    let settings = crate::Settings {
        prefix: &f.prefix,
        config: &config,
        state: &state,
        policy: crate::Policy::SecCore {
            socket: Path::new("/run/aw-test/sec.sock"),
        },
        qoder: Some(Path::new("/bin/true")),
        node: Some(Path::new("/bin/true")),
        openclaw: Some(Path::new("/bin/true")),
    };
    for mode in [0o755, 0o710, 0o777, 0o1700, 0o2700, 0o4700] {
        fs::set_permissions(&state, Permissions::from_mode(mode)).unwrap();
        assert!(
            crate::configure(&settings).is_err(),
            "accepted mode {mode:o}"
        );
        assert!(!config.exists());
        assert_eq!(state.metadata().unwrap().mode() & 0o7777, mode);
    }
    fs::set_permissions(&state, Permissions::from_mode(0o700)).unwrap();
    crate::configure(&settings).unwrap();
    assert!(config.exists());
    assert_eq!(state.metadata().unwrap().mode() & 0o7777, 0o700);
}

#[test]
fn configuration_checks_launcher_access_and_accepts_readable_scripts() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let runtime = tempfile::Builder::new()
        .prefix("cfg-")
        .tempdir_in(f.root.parent().unwrap())
        .unwrap();
    fs::set_permissions(runtime.path(), Permissions::from_mode(0o700)).unwrap();
    let config = runtime.path().join("aw.yaml");
    let state = runtime.path().join("state");
    let qoder = runtime.path().join("qoder");
    let node = runtime.path().join("node");
    let openclaw = runtime.path().join("openclaw.mjs");
    for path in [&qoder, &node, &openclaw] {
        fs::write(path, "fixture").unwrap();
        fs::set_permissions(path, Permissions::from_mode(0o700)).unwrap();
    }
    let settings = crate::Settings {
        prefix: &f.prefix,
        config: &config,
        state: &state,
        policy: crate::Policy::SecCore {
            socket: Path::new("/run/aw-test/sec.sock"),
        },
        qoder: Some(&qoder),
        node: Some(&node),
        openclaw: Some(&openclaw),
    };
    for path in [&qoder, &node] {
        fs::set_permissions(path, Permissions::from_mode(0o600)).unwrap();
        assert!(
            crate::configure(&settings).is_err(),
            "accepted {}",
            path.display()
        );
        assert!(!config.exists());
        assert_eq!(path.metadata().unwrap().mode() & 0o7777, 0o600);
        fs::set_permissions(path, Permissions::from_mode(0o700)).unwrap();
    }
    fs::set_permissions(&openclaw, Permissions::from_mode(0o400)).unwrap();
    crate::configure(&settings).unwrap();
    let value: serde_json::Value = serde_yaml_ng::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        value["spec"]["agents"]["qoder"]["argv"],
        serde_json::json!([qoder])
    );
    assert_eq!(
        value["spec"]["agents"]["openclaw"]["argv"],
        serde_json::json!([node, openclaw, "gateway", "run"])
    );
    assert_eq!(openclaw.metadata().unwrap().mode() & 0o7777, 0o400);
}

#[test]
fn one_configuration_maps_both_agents_and_rejects_long_socket() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let config = f.root.join("aw.yaml");
    let state = f.root.join("state");
    let socket = f.root.join("sec.sock");
    let mut settings = crate::Settings {
        prefix: &f.prefix,
        config: &config,
        state: &state,
        policy: crate::Policy::SecCore { socket: &socket },
        qoder: Some(Path::new("/bin/true")),
        node: Some(Path::new("/bin/true")),
        openclaw: Some(Path::new("/bin/true")),
    };
    // Keep Unix sockets short even when the test checkout lives in a long directory.
    let runtime = tempfile::tempdir().unwrap();
    fs::set_permissions(runtime.path(), Permissions::from_mode(0o700)).unwrap();
    let state = runtime.path().join("state");
    settings.state = &state;
    let socket = runtime.path().join("sec.sock");
    settings.policy = crate::Policy::SecCore { socket: &socket };
    crate::configure(&settings).unwrap();
    let value: serde_json::Value = serde_yaml_ng::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(value["spec"]["agents"].as_object().unwrap().len(), 2);
    assert_eq!(
        value["spec"]["events"]["tool.before"]["steps"][0]["match"]["tools"],
        serde_json::json!(["native:qoder:Bash", "native:openclaw:exec"])
    );
    assert_eq!(
        value["spec"]["providers"]["security"]["config"]["unmapped_tool"],
        "error"
    );
    assert_eq!(
        value["spec"]["events"]["tool.before"]["steps"][0]["on_error"],
        "block"
    );

    assert_eq!(
        value["spec"]["providers"]["security"]["config"]["tools"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert!(crate::configure(&settings).is_err());
    fs::remove_file(&config).unwrap();
    let long = runtime.path().join("s".repeat(108));
    settings.policy = crate::Policy::SecCore { socket: &long };
    assert!(crate::configure(&settings)
        .unwrap_err()
        .to_string()
        .contains("sec-core"));
    assert!(!config.exists());
}

#[test]
fn configure_rejects_damaged_payloads_and_allows_retry() {
    // A short external state path keeps this independent of checkout path length.
    let runtime = tempfile::tempdir().unwrap();
    fs::set_permissions(runtime.path(), Permissions::from_mode(0o700)).unwrap();
    for split in [false, true] {
        for name in [
            "bin/aw",
            "libexec/aw/providers/sec-core/aw-provider-sec-core",
            "libexec/aw/providers/sec-core/agent-sec-cli",
            "libexec/aw/providers/sec-core/agent-sec-daemon",
            "libexec/aw/providers/sec-core/agent-sec-daemon-health",
        ] {
            for damage in ["bytes", "missing", "mode", "setuid"] {
                let f = Fixture::new();
                if split {
                    install(&f.core, &f.prefix).unwrap();
                    install(&f.provider, &f.prefix).unwrap();
                } else {
                    install(&f.combined, &f.prefix).unwrap();
                }
                let before = receipts(&f.prefix).unwrap();
                let path = f.prefix.join(name);
                let original = fs::read(&path).unwrap();
                let permissions = path.metadata().unwrap().permissions();
                if damage == "missing" {
                    fs::remove_file(&path).unwrap();
                } else if damage == "bytes" {
                    fs::write(&path, "modified executable").unwrap();
                } else {
                    fs::set_permissions(
                        &path,
                        Permissions::from_mode(if damage == "mode" { 0o644 } else { 0o4755 }),
                    )
                    .unwrap();
                }
                let config = f.root.join("aw.yaml");
                let settings = crate::Settings {
                    prefix: &f.prefix,
                    config: &config,
                    state: &runtime.path().join("state"),
                    policy: crate::Policy::SecCore {
                        socket: Path::new("/run/aw-test/sec.sock"),
                    },
                    qoder: Some(Path::new("/bin/true")),
                    node: Some(Path::new("/bin/true")),
                    openclaw: Some(Path::new("/bin/true")),
                };
                let error = crate::configure(&settings).unwrap_err().to_string();
                let reason = match damage {
                    "bytes" => "checksum mismatch",
                    "missing" => "modified or missing",
                    _ => "mode mismatch",
                };
                let package = if name == "bin/aw" {
                    "aw-core"
                } else {
                    "aw-provider-sec-core"
                };
                assert!(
                    error.contains(reason)
                        && error.contains(&path.display().to_string())
                        && error.contains(&format!("package {package}")),
                    "split={split} damage={damage} file={name}: {error}"
                );
                assert!(!config.exists());
                assert_eq!(receipts(&f.prefix).unwrap(), before);
                fs::write(&path, original).unwrap();
                fs::set_permissions(path, permissions).unwrap();
                crate::configure(&settings).unwrap();
            }
        }
    }
}

#[test]
fn tokenless_templates_require_its_package_and_keep_security_before_compression() {
    let f = Fixture::new();
    install(&f.combined, &f.prefix).unwrap();
    let config = f.root.join("tokenless.yaml");
    let state = &f.state;
    let mut settings = crate::Settings {
        prefix: &f.prefix,
        config: &config,
        state,
        policy: crate::Policy::Tokenless,
        qoder: Some(Path::new("/bin/true")),
        node: None,
        openclaw: None,
    };
    assert!(crate::configure(&settings)
        .unwrap_err()
        .to_string()
        .contains("install the Tokenless Provider"));
    let provider = bundle(
        &f.root.join("tokenless-provider"),
        &["aw-provider-tokenless"],
    );
    install(&provider, &f.prefix).unwrap();
    crate::configure(&settings).unwrap();
    let document: serde_json::Value =
        serde_yaml_ng::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        document["spec"]["events"]["tool.before"]["steps"][0]["operation"],
        "pre_tool"
    );
    let provider = &document["spec"]["providers"]["tokenless"];
    let argv = provider["transport"]["argv"].as_array().unwrap();
    assert_eq!(argv[5], "--rtk");
    assert_eq!(
        argv[6],
        serde_json::json!(f.prefix.join("libexec/aw/providers/tokenless/rtk"))
    );
    assert_eq!(argv[7], "--rtk-sha256");
    assert_eq!(
        argv[8],
        crate::digest(&f.prefix.join("libexec/aw/providers/tokenless/rtk")).unwrap()
    );
    let mut expected = argv[..5].to_vec();
    expected.insert(1, serde_json::json!("--view"));
    assert_eq!(provider["view"]["protocol"], "aw-provider-view/v1alpha1");
    assert_eq!(
        provider["view"]["transport"]["argv"],
        serde_json::json!(expected)
    );
    assert_eq!(provider["view"]["implementation"], "tokenless");
    assert_eq!(
        document["spec"]["events"]["tool.after"]["steps"][0]["effects"],
        serde_json::json!(["replace_result"])
    );
    fs::remove_file(&config).unwrap();
    settings.policy = crate::Policy::SecCoreTokenless {
        socket: Path::new("/run/aw-test/sec.sock"),
    };
    crate::configure(&settings).unwrap();
    let document: serde_json::Value =
        serde_yaml_ng::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        document["spec"]["events"]["tool.before"]["steps"][0]["provider"],
        "security"
    );
    assert_eq!(
        document["spec"]["events"]["tool.before"]["steps"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        document["spec"]["events"]["tool.before"]["steps"][1]["operation"],
        "pre_tool"
    );
    assert_eq!(
        document["spec"]["events"]["tool.before"]["steps"][0]["on_error"],
        "block"
    );
    assert!(document["spec"]["providers"]["security"]
        .get("view")
        .is_none());
    assert_eq!(
        document["spec"]["events"]["tool.after"]["steps"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        document["spec"]["events"]["tool.after"]["steps"][0]["provider"],
        "security"
    );
    assert_eq!(
        document["spec"]["events"]["tool.after"]["steps"][0]["operation"],
        "observe_tool"
    );
    assert_eq!(
        document["spec"]["events"]["tool.after"]["steps"][1]["provider"],
        "tokenless"
    );
}

#[test]
fn desired_tokenless_document_does_not_require_installed_payloads() {
    let f = Fixture::new();
    let prefix = f.root.join("not-installed");
    let config = f.root.join("not-published.yaml");
    let state = f.root.join("not-created");
    let desired = crate::document(&crate::Settings {
        prefix: &prefix,
        config: &config,
        state: &state,
        policy: crate::Policy::Tokenless,
        qoder: Some(Path::new("/bin/true")),
        node: None,
        openclaw: None,
    })
    .unwrap();
    assert_eq!(
        desired["spec"]["providers"]["tokenless"]["transport"]["argv"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert!(!prefix.exists() && !config.exists() && !state.exists());
}

#[test]
fn managed_tokenless_configuration_rejects_path_delimiters_before_publication() {
    let f = Fixture::new();
    let all = bundle(
        &f.root.join("tokenless-bundle"),
        &["aw-core", "aw-provider-tokenless"],
    );
    let prefix = f.root.join("prefix:rtk");
    install(&all, &prefix).unwrap();
    let config = f.root.join("colon.yaml");
    let state = f.root.join("state");
    let mut settings = crate::Settings {
        prefix: &prefix,
        config: &config,
        state: &state,
        policy: crate::Policy::Tokenless,
        qoder: Some(Path::new("/bin/true")),
        node: None,
        openclaw: None,
    };
    assert!(crate::document(&settings).is_ok());
    assert!(crate::configure(&settings)
        .unwrap_err()
        .to_string()
        .contains("managed RTK directory cannot be represented in PATH"));
    assert!(!config.exists() && !state.exists());
    settings.policy = crate::Policy::None;
    crate::configure(&settings).unwrap();
    assert!(config.exists());
}
