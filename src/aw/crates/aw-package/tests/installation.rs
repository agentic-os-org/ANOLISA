//! Public CLI contracts, independent of local Agent or backend installations.
use std::process::Command;
#[test]
fn help_exposes_native_installation_and_dual_agent_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_aw-package"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "install --prefix",
        "configure --prefix",
        "--qoder",
        "--openclaw",
        "uninstall --prefix",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    assert!(!text.contains("python"));
}
#[test]
fn invalid_commands_and_duplicate_flags_fail_before_touching_disk() {
    for args in [
        vec!["unknown"],
        vec!["install", "--prefix", "/unused", "--prefix", "/other"],
        vec!["configure", "--prefix"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aw-package"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("aw-package:"));
    }
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
    // Enough small owned files to observe an actual deletion before signalling.
    for name in ["bin/aw".to_string(), "bin/aw-package".to_string()]
        .into_iter()
        .chain((0..4096).map(|index| format!("share/doc/file-{index:04}")))
    {
        let path = bundle.join("payload").join(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &name).unwrap();
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
            Command::new(env!("CARGO_BIN_EXE_aw-package"))
                .arg("uninstall")
                .arg("--prefix")
                .arg(&prefix)
                .current_dir(&root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        eprintln!("uninstaller PID={} command=aw-package uninstall --prefix {} cwd={} log={} ports=none deadline=30s stop=kill -KILL {}; then wait owned child", child.0.id(),prefix.display(),root.display(),log.display(),child.0.id());
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
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "uninstaller did not restore files"
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
        aw_package::uninstall(&prefix).unwrap();
        assert!(!receipt.exists());
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
    // A large first payload leaves time to deliver a real signal after creation.
    let source = bundle.join("payload/bin/aw");
    fs::File::create(&source)
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    fs::write(bundle.join("payload/bin/aw-package"), "fixture installer").unwrap();
    let files: std::collections::BTreeMap<_, _> = ["bin/aw", "bin/aw-package"]
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
            Command::new(env!("CARGO_BIN_EXE_aw-package"))
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
        let destination = prefix.join("bin/aw");
        while !destination.metadata().is_ok_and(|m| m.len() > 0) {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "installer exited before signal"
            );
            assert!(Instant::now() < deadline, "installer did not start copying");
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
