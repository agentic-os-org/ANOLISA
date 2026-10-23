//! Read-only package queries retain older records and report observed damage.
#![cfg(target_os = "linux")]

use aw_package::{Component, Payload};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/inventory");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target).unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            prefix: root.join("prefix"),
            root,
            _directory: directory,
        }
    }

    fn directory(&self, path: &Path) {
        let mut current = self.root.clone();
        for part in path.strip_prefix(&current).unwrap().components() {
            current.push(part);
            fs::create_dir_all(&current).unwrap();
            fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    fn record(&self, path: &Path, bytes: impl AsRef<[u8]>, mode: u32) {
        self.directory(path.parent().unwrap());
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn legacy(&self) {
        let mut files = BTreeMap::new();
        for name in ["bin/aw", "bin/aw-package"] {
            let path = self.prefix.join(name);
            self.record(&path, b"package fixture; never executed", 0o755);
            files.insert(
                name.to_owned(),
                Payload {
                    sha256: aw_package::digest(&path).unwrap(),
                    mode: 0o755,
                },
            );
        }
        let component = Component {
            format: 1,
            component: "aw-core".into(),
            version: "0.1.0-preview.1".into(),
            source_commit: "a".repeat(40),
            os: "linux".into(),
            arch: std::env::consts::ARCH.into(),
            provider_protocol: "aw-provider/v1alpha1".into(),
            requires: BTreeMap::new(),
            files,
        };
        self.record(&self.prefix.join(".aw-install.lock"), b"", 0o600);
        self.record(
            &self.prefix.join(".aw-packages/aw-core.json"),
            serde_json::to_vec(&component).unwrap(),
            0o644,
        );
    }

    fn run(&self, arguments: &[&str], prefix: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aw"));
        command
            .args(arguments)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"));
        if prefix {
            command.arg("--prefix").arg(&self.prefix);
        }
        command.output().unwrap()
    }

    fn registered(&self) -> PathBuf {
        let file = self.prefix.join("bin/example-mant");
        self.record(&file, b"standalone tool fixture", 0o755);
        let database = self.root.join("state/aw/packages");
        self.record(&database.join("lock"), b"", 0o600);
        let value = serde_json::json!({
            "schema_version": 1,
            "generation": 7,
            "installations": [{
                "installation_id": "standalone-fixture",
                "prefix": self.prefix,
                "owner_uid": fs::metadata(&self.root).unwrap().uid(),
                "scope": "user",
                "manager": "raw",
                "packages": [{
                    "package_id": "example.mant",
                    "version": "1.2.3",
                    "os": "linux",
                    "arch": std::env::consts::ARCH,
                    "files": {"bin/example-mant": {
                        "sha256": aw_package::digest(&file).unwrap(),
                        "mode": 0o755
                    }},
                    "dependencies": [],
                    "source": {}
                }]
            }]
        });
        self.record(
            &database.join("installed.json"),
            serde_json::to_vec(&value).unwrap(),
            0o644,
        );
        let paths = aw_package::database::DatabasePaths::user(Some(&self.root.join("state")), None)
            .unwrap();
        let pointer =
            aw_package::database::RegistrationPointer::new(&paths, "standalone-fixture".into())
                .unwrap();
        self.record(&self.prefix.join(".aw-install.lock"), b"", 0o600);
        self.record(
            &self.prefix.join(".aw-packages/aw-core.json"),
            serde_json::to_vec(&pointer).unwrap(),
            0o644,
        );
        database
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, u32, Vec<u8>)> {
    fn visit(root: &Path, path: &Path, found: &mut BTreeMap<PathBuf, (u64, u32, Vec<u8>)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let bytes = if metadata.is_file() {
            fs::read(path).unwrap()
        } else {
            Vec::new()
        };
        found.insert(
            path.strip_prefix(root).unwrap().into(),
            (metadata.ino(), metadata.mode(), bytes),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), found);
            }
        }
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found);
    found
}

#[test]
fn legacy_queries_preserve_every_file_and_do_not_create_a_database() {
    let fixture = Fixture::new();
    fixture.legacy();
    let before = snapshot(&fixture.root);
    for arguments in [
        vec!["list", "--installed", "--json"],
        vec!["list", "--installed", "--scope", "user", "--json"],
        vec!["doctor", "--json"],
        vec!["doctor", "--scope", "user", "--json"],
    ] {
        let output = fixture.run(&arguments, true);
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["format"], 1);
        assert_eq!(value["status"], "succeeded");
        assert!(value.get("mutation").is_none());
        assert_eq!(snapshot(&fixture.root), before);
    }
    let output = fixture.run(&["list", "--installed", "--json"], true);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let installation = &value["result"]["inventory"]["installations"][0];
    assert_eq!(installation["packages"][0]["package_id"], "aw-core");
    assert_eq!(installation["registration"], "legacy_unregistered");
}

#[test]
fn list_keeps_recorded_packages_while_doctor_reports_missing_files() {
    let fixture = Fixture::new();
    fixture.legacy();
    fs::remove_file(fixture.prefix.join("bin/aw")).unwrap();
    let before = snapshot(&fixture.root);
    let list = fixture.run(&["list", "--installed", "--json"], true);
    assert!(list.status.success(), "{:?}", list);
    let doctor = fixture.run(&["doctor", "--json"], true);
    assert_eq!(doctor.status.code(), Some(1));
    assert!(doctor.stderr.is_empty());
    let value: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(value["status"], "failed");
    assert_eq!(value["error"]["code"], "integrity_check");
    assert!(!value["result"]["report"]["issues"]
        .as_array()
        .unwrap()
        .is_empty());
    let quiet = fixture.run(&["doctor", "--quiet"], true);
    assert_eq!(quiet.status.code(), Some(1));
    assert!(quiet.stdout.is_empty() && !quiet.stderr.is_empty());
    assert_eq!(snapshot(&fixture.root), before);
}

#[test]
fn an_absent_database_is_reported_without_creating_state() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["list", "--installed", "--json"],
        vec!["doctor", "--json"],
    ] {
        let output = fixture.run(&arguments, false);
        assert!(output.status.success(), "{:?}", output);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "succeeded");
        assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
    }
}

#[test]
fn syntax_errors_never_create_state_or_claim_structured_results() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["list"],
        vec!["list", "--available", "--json"],
        vec!["list", "--installed", "--installed"],
        vec!["doctor", "--repair", "--json"],
        vec!["doctor", "--json", "--quiet"],
        vec!["doctor", "--scope", "machine"],
    ] {
        let output = fixture.run(&arguments, false);
        assert_eq!(output.status.code(), Some(1), "{:?}", output);
        assert!(output.stdout.is_empty() && !output.stderr.is_empty());
    }
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[test]
fn invalid_database_paths_report_a_failure_without_fallback_or_writes() {
    let fixture = Fixture::new();
    let output = Command::new(env!("CARGO_BIN_EXE_aw"))
        .args(["list", "--installed", "--json"])
        .env("XDG_STATE_HOME", "relative-state")
        .env("HOME", fixture.root.join("home"))
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "failed");
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[test]
fn registered_packages_are_listed_and_checked_without_selecting_a_prefix() {
    let fixture = Fixture::new();
    fixture.registered();
    let before = snapshot(&fixture.root);
    let output = fixture.run(&["list", "--installed", "--json"], false);
    assert!(output.status.success(), "{:?}", output);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["inventory"]["generation"], 7);
    let package = &value["result"]["inventory"]["installations"][0]["packages"][0];
    assert_eq!(package["package_id"], "example.mant");
    assert_eq!(package["version"], "1.2.3");
    let doctor = fixture.run(&["doctor", "--json"], false);
    assert!(doctor.status.success(), "{:?}", doctor);
    let result: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(result["result"]["report"]["files_checked"], 1);
    assert_eq!(snapshot(&fixture.root), before);
}

#[test]
fn unfinished_transactions_explain_the_recovery_location_without_repairing_it() {
    let fixture = Fixture::new();
    let database = fixture.registered();
    let pending = database.join("transactions/unfinished");
    fixture.record(&pending.join("journal.json"), b"uncommitted fixture", 0o600);
    let before = snapshot(&fixture.root);
    for arguments in [
        vec!["list", "--installed", "--json"],
        vec!["doctor", "--json"],
    ] {
        let output = fixture.run(&arguments, false);
        assert_eq!(output.status.code(), Some(1), "{:?}", output);
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "needs_recovery");
        assert_eq!(
            value["error"]["recovery_path"],
            database.join("transactions").to_str().unwrap()
        );
    }
    assert_eq!(snapshot(&fixture.root), before);
}

#[test]
fn human_query_output_escapes_untrusted_directory_names() {
    let mut fixture = Fixture::new();
    fixture.prefix = fixture.root.join("prefix\n\u{1b}[2J\u{202e}");
    fixture.legacy();
    let output = fixture.run(&["list", "--installed", "--color", "never"], true);
    assert!(output.status.success(), "{:?}", output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains('\u{1b}') && !text.contains('\u{202e}'));
    assert!(text.contains("prefix\\n") && text.contains("older installation records"));
}

#[test]
fn a_selected_directory_without_installation_records_is_not_a_healthy_installation() {
    let fixture = Fixture::new();
    let output = fixture.run(&["doctor", "--json"], true);
    assert_eq!(output.status.code(), Some(1), "{:?}", output);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "failed");
    assert!(!fixture.prefix.exists());
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}
