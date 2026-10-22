//! Unified package CLI contracts without local Agent or backend installations.
use std::process::Command;
#[test]
fn help_exposes_native_installation_and_dual_agent_configuration() {
    for (arguments, expected) in [
        (vec!["install", "--help"], vec!["--prefix", "--bundle"]),
        (
            vec!["config", "init", "--help"],
            vec!["--prefix", "--qoder", "--openclaw"],
        ),
        (vec!["uninstall", "--help"], vec!["--prefix", "--yes"]),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for option in expected {
            assert!(text.contains(option), "missing {option}");
        }
        assert!(!text.contains("aw-package"));
    }
}
#[test]
fn invalid_commands_and_duplicate_flags_fail_before_touching_disk() {
    for args in [
        vec!["unknown"],
        vec!["install", "--prefix", "/unused", "--prefix", "/other"],
        vec!["config", "init", "--prefix"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr).unwrap().contains("aw:"));
    }
}

#[test]
fn non_utf8_arguments_are_rejected_without_panicking() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let invalid = OsStr::from_bytes(b"/tmp/prefix-\xff");
    let arguments = ["install".into(), "--prefix".into(), invalid.to_os_string()];
    let output = Command::new(env!("CARGO_BIN_EXE_aw"))
        .args(arguments)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("aw:"), "{stderr}");
    assert!(stderr.contains("UTF-8"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

struct Installer(std::process::Child);
impl Drop for Installer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn termination_signals_restore_cli_uninstall_and_allow_retry() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::Stdio,
        time::{Duration, Instant},
    };
    let target = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::Builder::new()
        .prefix("uninstall-signal-test-")
        .tempdir_in(target)
        .unwrap();
    let root = directory.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let bundle = root.join("bundle");
    let mut files = std::collections::BTreeMap::new();
    // Per-file journal fsyncs leave a bounded window to observe deletion before signalling.
    for name in ["bin/aw".to_string()]
        .into_iter()
        .chain((0..64).map(|index| format!("share/doc/file-{index:04}")))
    {
        let path = bundle.join("payload").join(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if name == "bin/aw" {
            fs::copy(env!("CARGO_BIN_EXE_aw"), &path).unwrap();
        } else {
            fs::write(&path, &name).unwrap();
        }
        files.insert(
            name,
            aw_package::Payload {
                sha256: aw_package::digest(&path).unwrap(),
                mode: 0o755,
            },
        );
    }
    let component = aw_package::Component {
        format: 1,
        component: "aw-core".into(),
        version: "0.1.0-preview.1".into(),
        source_commit: "a".repeat(40),
        os: "linux".into(),
        arch: std::env::consts::ARCH.into(),
        provider_protocol: "aw-provider/v1alpha1".into(),
        requires: Default::default(),
        files,
    };
    fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec(&aw_package::Manifest {
            format: 1,
            components: vec![component.clone()],
        })
        .unwrap(),
    )
    .unwrap();
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let prefix = root.join(format!("prefix-{signal}"));
        aw_package::install(&bundle, &prefix).unwrap();
        let receipt = prefix.join(".aw-packages/aw-core.json");
        let receipt_before = fs::read(&receipt).unwrap();
        let log = root.join(format!("stderr-{signal}"));
        let mut child = Installer(
            Command::new(env!("CARGO_BIN_EXE_aw"))
                .env(
                    "XDG_STATE_HOME",
                    root.join(format!("package-state-{signal}")),
                )
                .args(["uninstall", "--yes"])
                .arg("--prefix")
                .arg(&prefix)
                .current_dir(&root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        eprintln!("uninstaller PID={} command=aw uninstall --yes --prefix {} cwd={} log={} ports=none deadline=30s stop=kill -KILL {}; then wait owned child", child.0.id(),prefix.display(),root.display(),log.display(),child.0.id());
        let deadline = Instant::now() + Duration::from_secs(30);
        while prefix.join("bin/aw").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "uninstaller exited before deletion"
            );
            assert!(
                Instant::now() < deadline,
                "uninstaller did not start deleting"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // SAFETY: this is the unreaped child owned by the test.
        assert_eq!(unsafe { libc::kill(child.0.id() as i32, signal) }, 0);
        let rollback_deadline = Instant::now() + Duration::from_secs(45);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < rollback_deadline,
                "uninstaller did not restore files: {}",
                fs::read_to_string(&log).unwrap()
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            status.code(),
            Some(1),
            "must unwind rather than die by signal"
        );
        assert!(fs::read_to_string(log).unwrap().contains("interrupted"));
        assert_eq!(fs::read(&receipt).unwrap(), receipt_before);
        for (name, payload) in &component.files {
            let path = prefix.join(name);
            assert_eq!(aw_package::digest(&path).unwrap(), payload.sha256);
            assert_eq!(
                path.metadata().unwrap().permissions().mode() & 0o7777,
                payload.mode
            );
        }
        assert!(!fs::read_dir(&prefix).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("uninstall-")));
        let retried = Command::new(env!("CARGO_BIN_EXE_aw"))
            .env(
                "XDG_STATE_HOME",
                root.join(format!("package-state-{signal}")),
            )
            .args(["uninstall", "--yes", "--prefix"])
            .arg(&prefix)
            .output()
            .unwrap();
        assert!(
            retried.status.success(),
            "{}",
            String::from_utf8_lossy(&retried.stderr)
        );
        let pointer: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(pointer["schema_version"], 1);
        assert!(component
            .files
            .keys()
            .all(|name| !prefix.join(name).exists()));
    }
}

#[test]
fn termination_signals_roll_back_cli_install_and_allow_retry() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::Stdio,
        time::{Duration, Instant},
    };
    let target = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    fs::create_dir_all(&target).unwrap();
    let directory = tempfile::Builder::new()
        .prefix("signal-test-")
        .tempdir_in(target)
        .unwrap();
    let root = directory.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let bundle = root.join("bundle");
    fs::create_dir_all(bundle.join("payload/bin")).unwrap();
    // A large payload leaves time to signal after the registration gate is published.
    let source = bundle.join("payload/bin/aw");
    fs::File::create(&source)
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
    let files: std::collections::BTreeMap<_, _> = ["bin/aw"]
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                aw_package::Payload {
                    sha256: aw_package::digest(&bundle.join("payload").join(name)).unwrap(),
                    mode: 0o755,
                },
            )
        })
        .collect();
    let manifest = aw_package::Manifest {
        format: 1,
        components: vec![aw_package::Component {
            format: 1,
            component: "aw-core".into(),
            version: "0.1.0-preview.1".into(),
            source_commit: "a".repeat(40),
            os: "linux".into(),
            arch: std::env::consts::ARCH.into(),
            provider_protocol: "aw-provider/v1alpha1".into(),
            requires: Default::default(),
            files,
        }],
    };
    fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let prefix = root.join(format!("prefix-{signal}"));
        let log = root.join(format!("stderr-{signal}"));
        let mut child = Installer(
            Command::new(env!("CARGO_BIN_EXE_aw"))
                .env(
                    "XDG_STATE_HOME",
                    root.join(format!("package-state-{signal}")),
                )
                .arg("install")
                .arg("--bundle")
                .arg(&bundle)
                .arg("--prefix")
                .arg(&prefix)
                .stdout(Stdio::null())
                .stderr(fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        eprintln!(
            "installer PID={} signal={} deadline=60s; cleanup=kill/wait owned child",
            child.0.id(),
            signal
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        let gate = prefix.join(".aw-packages/aw-core.json");
        while !gate.is_file() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "installer exited before signal: {}",
                fs::read_to_string(&log).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "installer did not publish its registration gate"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // SAFETY: this is the live child we own; no process-group or pattern targeting.
        assert_eq!(unsafe { libc::kill(child.0.id() as i32, signal) }, 0);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "installer did not finish rollback"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            status.code(),
            Some(1),
            "must unwind normally instead of dying by signal"
        );
        assert!(fs::read_to_string(&log).unwrap().contains("interrupted"));
        assert!(
            !prefix.exists(),
            "interrupted installation must retire its prefix"
        );
        aw_package::install(&bundle, &prefix).unwrap();
        for (name, payload) in &manifest.components[0].files {
            assert_eq!(
                aw_package::digest(&prefix.join(name)).unwrap(),
                payload.sha256
            );
        }
    }
}
