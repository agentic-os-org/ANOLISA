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
        socket: Path::new("/run/aw-test/sec.sock"),
        qoder: Path::new("/bin/true"),
        node: Path::new("/bin/true"),
        openclaw: Path::new("/bin/true"),
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
            socket: Path::new("/run/aw-test/sec.sock"),
            qoder: Path::new("/bin/true"),
            node: Path::new("/bin/true"),
            openclaw: Path::new("/bin/true"),
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
        socket: Path::new("/run/aw-test/sec.sock"),
        qoder: Path::new("/bin/true"),
        node: Path::new("/bin/true"),
        openclaw: Path::new("/bin/true"),
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
        socket: Path::new("/run/aw-test/sec.sock"),
        qoder: &qoder,
        node: &node,
        openclaw: &openclaw,
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
        socket: &socket,
        qoder: Path::new("/bin/true"),
        node: Path::new("/bin/true"),
        openclaw: Path::new("/bin/true"),
    };
    // Keep Unix sockets short even when the test checkout lives in a long directory.
    let runtime = tempfile::tempdir().unwrap();
    fs::set_permissions(runtime.path(), Permissions::from_mode(0o700)).unwrap();
    let state = runtime.path().join("state");
    settings.state = &state;
    let socket = runtime.path().join("sec.sock");
    settings.socket = &socket;
    crate::configure(&settings).unwrap();
    let value: serde_json::Value = serde_yaml_ng::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(value["spec"]["agents"].as_object().unwrap().len(), 2);
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
    settings.socket = &long;
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
            "bin/aw-package",
            "libexec/aw/providers/sec-core/aw-provider-sec-core",
            "libexec/aw/providers/sec-core/agent-sec-cli",
            "libexec/aw/providers/sec-core/agent-sec-daemon",
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
                    socket: Path::new("/run/aw-test/sec.sock"),
                    qoder: Path::new("/bin/true"),
                    node: Path::new("/bin/true"),
                    openclaw: Path::new("/bin/true"),
                };
                assert!(crate::configure(&settings)
                    .unwrap_err()
                    .to_string()
                    .contains("modified or missing"));
                assert!(!config.exists());
                assert_eq!(receipts(&f.prefix).unwrap(), before);
                fs::write(&path, original).unwrap();
                fs::set_permissions(path, permissions).unwrap();
                crate::configure(&settings).unwrap();
            }
        }
    }
}
