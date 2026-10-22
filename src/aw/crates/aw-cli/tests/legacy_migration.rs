//! A compiled historical CLI must refuse every write entrypoint after registration.

#![cfg(target_os = "linux")]

#[path = "packages/support.rs"]
#[allow(dead_code)]
// This separate black-box target uses only package fixture and bounded invocation.
mod support;

use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use support::Fixture;

fn invoke(command: &Command) -> aw_exec::Output {
    let mut environment: BTreeMap<_, _> = std::env::vars_os().collect();
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            environment.insert(name.to_owned(), value.to_owned());
        } else {
            environment.remove(name);
        }
    }
    aw_exec::run(
        &aw_exec::CommandSpec {
            program: command.get_program().into(),
            args: command.get_args().map(Into::into).collect(),
            cwd: command.get_current_dir().unwrap().into(),
            environment,
        },
        b"",
        aw_exec::Limits {
            input_bytes: 0,
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        },
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap()
}

fn older(fixture: &Fixture, binary: &Path, action: &str, prefix: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(&fixture.root)
        .arg(action)
        .arg("--prefix")
        .arg(prefix);
    if action == "install" {
        command.arg("--bundle").arg(&fixture.bundle);
    }
    if action == "configure" {
        command
            .arg("--config")
            .arg(fixture.root.join("rejected-config.yaml"))
            .arg("--state-dir")
            .arg(fixture.root.join("rejected-state"))
            .args(["--qoder", "/bin/true"]);
    }
    command
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, Option<String>)> {
    fn visit(path: &Path, root: &Path, result: &mut BTreeMap<PathBuf, (u32, Option<String>)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        let digest = if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(&entry.unwrap().path(), root, result);
            }
            None
        } else {
            assert!(metadata.is_file());
            Some(aw_package::digest(path).unwrap())
        };
        result.insert(
            path.strip_prefix(root).unwrap().into(),
            (metadata.permissions().mode(), digest),
        );
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn succeeded(command: &Command) {
    let output = invoke(command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn historical_installer() -> PathBuf {
    let old = std::env::var_os("AW_LEGACY_PACKAGE_CLI")
        .expect("run scripts/check.py on Linux to build the mandatory historical installer");
    let old = PathBuf::from(old);
    assert!(old.is_absolute() && old.is_file());
    old
}

fn historical_bundle(fixture: &Fixture, old: &Path, old_core: bool) {
    // The supported historical Core format included both executable names.
    let installer = fixture.bundle.join("payload/bin/aw-package");
    fs::copy(old, &installer).unwrap();
    fs::set_permissions(&installer, fs::Permissions::from_mode(0o755)).unwrap();
    let manifest_path = fixture.bundle.join("manifest.json");
    let mut manifest: aw_package::Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest.components[0].files.insert(
        "bin/aw-package".into(),
        aw_package::Payload {
            sha256: aw_package::digest(&installer).unwrap(),
            mode: 0o755,
        },
    );
    if old_core {
        let executable = fixture.bundle.join("payload/bin/aw");
        let historical = old.with_file_name("aw");
        assert_ne!(
            aw_package::digest(&historical).unwrap(),
            aw_package::digest(&executable).unwrap()
        );
        fs::copy(historical, &executable).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        manifest.components[0]
            .files
            .get_mut("bin/aw")
            .unwrap()
            .sha256 = aw_package::digest(&executable).unwrap();
    }
    fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

fn independent_tool(fixture: &Fixture) -> PathBuf {
    let bundle = fixture.root.join("independent-tool");
    let payload = bundle.join("payload/share/example.txt");
    fs::create_dir_all(payload.parent().unwrap()).unwrap();
    fs::write(&payload, b"independent local package\n").unwrap();
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(
        bundle.join("package.json"),
        serde_json::to_vec(&json!({
            "schema_version":1,"package_id":"example-tool","version":"1.2.3",
            "os":"linux","arch":std::env::consts::ARCH,"dependencies":[],
            "files":{"share/example.txt":{"sha256":aw_package::digest(&payload).unwrap(),"mode":0o644}}
        }))
        .unwrap(),
    )
    .unwrap();
    bundle
}

#[test]
fn real_historical_installer_refuses_all_writes_after_package_record_migration() {
    let old = historical_installer();
    let fixture = Fixture::new();
    // Current AW bytes in a historical record remain manageable after registration.
    historical_bundle(&fixture, &old, false);
    succeeded(&older(&fixture, &old, "install", &fixture.prefix));
    // First prove configuration works before publishing the registration pointer.
    succeeded(&older(&fixture, &old, "configure", &fixture.prefix));
    fs::rename(fixture.root.join("rejected-config.yaml"), &fixture.config).unwrap();
    assert!(!fixture.root.join("rejected-state").exists());
    fs::create_dir(&fixture.state).unwrap();
    fs::write(fixture.state.join("runtime-data"), b"keep runtime data").unwrap();
    let spare = fixture.root.join("old-uninstall-proof");
    succeeded(&older(&fixture, &old, "install", &spare));
    succeeded(&older(&fixture, &old, "uninstall", &spare));
    assert!(!spare.join("bin/aw").exists());

    let record_path = fixture.prefix.join(".aw-packages/aw-core.json");
    let original_record = fs::read(&record_path).unwrap();
    fs::write(fixture.prefix.join("user-data"), b"keep my files").unwrap();
    let bundle = independent_tool(&fixture);
    let mut migrate = fixture.command();
    migrate
        .args(["install", "--prefix"])
        .arg(&fixture.prefix)
        .arg("--bundle")
        .arg(&bundle);
    succeeded(&migrate);
    let pointer = aw_package::database::RegistrationPointer::read(&fixture.prefix)
        .unwrap()
        .expect("successful migration must publish the old-writer rejection pointer");
    let archives = pointer
        .database_root
        .join("legacy")
        .join(&pointer.installation_id);
    let attempts = fs::read_dir(archives)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        fs::read(attempts[0].join("aw-core.json")).unwrap(),
        original_record
    );
    assert_eq!(
        fs::read_dir(record_path.parent().unwrap()).unwrap().count(),
        1
    );
    assert!(fixture.prefix.join("share/example.txt").is_file());
    let before = snapshot(&fixture.root);
    for action in ["install", "uninstall", "configure"] {
        let output = invoke(&older(&fixture, &old, action, &fixture.prefix));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "historical {action} unexpectedly succeeded"
        );
        assert!(
            error.contains("unknown field") && error.contains("schema_version"),
            "historical {action} rejected for an unrelated reason: {error}"
        );
        assert_eq!(
            snapshot(&fixture.root),
            before,
            "historical {action} changed bytes or modes"
        );
    }
    succeeded(
        fixture
            .command()
            .args(["uninstall", "--yes", "--prefix"])
            .arg(&fixture.prefix),
    );
    assert!(!fixture.prefix.join("bin/aw").exists());
    assert!(!fixture.prefix.join("share/example.txt").exists());
    assert_eq!(
        fs::read(fixture.prefix.join("user-data")).unwrap(),
        b"keep my files"
    );
    assert!(fixture.config.is_file());
    assert!(fixture.state.join("runtime-data").is_file());
}

#[test]
fn historical_core_registration_is_rejected_without_disabling_its_installer() {
    let old = historical_installer();
    let fixture = Fixture::new();
    historical_bundle(&fixture, &old, true);
    succeeded(&older(&fixture, &old, "install", &fixture.prefix));
    let bundle = independent_tool(&fixture);
    let before = snapshot(&fixture.root);
    for dry_run in [true, false] {
        let mut command = fixture.command();
        command
            .args(["install", "--json", "--prefix"])
            .arg(&fixture.prefix)
            .arg("--bundle")
            .arg(&bundle);
        if dry_run {
            command.arg("--dry-run");
        }
        let output = invoke(&command);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["error"]["code"], "plan_blocked");
        let problems = report["result"]["plan"]["problems"].as_array().unwrap();
        assert!(problems
            .iter()
            .any(|problem| problem["code"] == "runtime_unknown"
                && problem["message"]
                    .as_str()
                    .unwrap()
                    .contains("separate empty prefix")));
        assert_eq!(
            snapshot(&fixture.root),
            before,
            "blocked registration changed disk state"
        );
    }
    assert!(!fixture.root.join("package-state").exists());
    assert!(
        aw_package::database::RegistrationPointer::read(&fixture.prefix)
            .unwrap()
            .is_none()
    );
    succeeded(&older(&fixture, &old, "configure", &fixture.prefix));
    assert!(fixture.root.join("rejected-config.yaml").is_file());
    succeeded(&older(&fixture, &old, "uninstall", &fixture.prefix));
    assert!(!fixture.prefix.join("bin/aw").exists());
    assert!(!fixture.prefix.join("bin/aw-package").exists());
    assert!(fixture.root.join("rejected-config.yaml").is_file());
}
