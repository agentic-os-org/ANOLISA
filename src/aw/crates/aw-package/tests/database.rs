//! Read-only package inventory, legacy compatibility and bounded file verification.

#[path = "database/doctor_metadata.rs"]
mod doctor_metadata;

use aw_package::{
    database::{
        DatabasePaths, DoctorIssueCode, Inventory, InventorySource, Registration,
        RegistrationPointer, MAX_DATABASE_BYTES,
    },
    description::PackageDescription,
    digest, Error,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{symlink, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

struct Fixture {
    directory: tempfile::TempDir,
    paths: DatabasePaths,
    prefix: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/package-database");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let paths = DatabasePaths::user(Some(directory.path()), None).unwrap();
        let prefix = directory.path().join("install");
        Self {
            directory,
            paths,
            prefix,
        }
    }

    fn file(&self, name: &str, bytes: &[u8]) -> Value {
        let path = self.prefix.join(name);
        self.record(&path, bytes, 0o755);
        json!({"sha256":digest(&path).unwrap(),"mode":493})
    }

    fn package(&self) -> Value {
        json!({"package_id":"example.tool","version":"1.2.3","os":"linux","arch":"aarch64", "files":{"bin/tool":self.file("bin/tool", b"never executed\n")}, "dependencies":[], "source":{"bundle_sha256":"a".repeat(64)}})
    }

    fn installation(&self) -> Value {
        // SAFETY: geteuid only queries the fixture process identity.
        json!({"installation_id":"install-1", "prefix":self.prefix, "owner_uid":unsafe { libc::geteuid() }, "scope":"user", "manager":"raw", "packages":[self.package()]})
    }

    fn database(&self, installations: Vec<Value>) -> Value {
        let value = json!({"schema_version":1,"generation":7,"installations":installations});
        self.record(&self.paths.root.join("lock"), b"", 0o600);
        self.write(&value);
        value
    }

    fn write(&self, value: &Value) {
        self.record(
            &self.paths.installed(),
            serde_json::to_vec(value).unwrap(),
            0o644,
        );
    }

    fn directory(&self, path: &Path) {
        let mut current = self.directory.path().to_owned();
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

    fn registration(&self, prefix: &Path, id: &str) {
        self.record(&prefix.join(".aw-install.lock"), b"", 0o600);
        let pointer = RegistrationPointer::new(&self.paths, id.into()).unwrap();
        self.record(
            &prefix.join(".aw-packages/aw-core.json"),
            serde_json::to_vec(&pointer).unwrap(),
            0o644,
        );
    }

    fn legacy(&self, version: &str) {
        let files = json!({"bin/aw":self.file("bin/aw", b"aw"),"bin/aw-package":self.file("bin/aw-package", b"package")});
        self.record(&self.prefix.join(".aw-install.lock"), b"", 0o600);
        let record = json!({"format":1,"component":"aw-core","version":version,"source_commit":"producer-supplied-build", "os":"linux","arch":std::env::consts::ARCH,"provider_protocol":"aw-provider/v1alpha1","requires":{},"files":files});
        self.record(
            &self.prefix.join(".aw-packages/aw-core.json"),
            serde_json::to_vec(&record).unwrap(),
            0o644,
        );
    }

    fn read(&self) -> aw_package::Result<Inventory> {
        Inventory::read(&self.paths, None)
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, u64, i64, i64, Vec<u8>)> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.is_file() {
            fs::read(&path).unwrap()
        } else {
            Vec::new()
        };
        result.insert(
            path.clone(),
            (
                metadata.mode(),
                metadata.ino(),
                metadata.mtime(),
                metadata.ctime(),
                bytes,
            ),
        );
        if metadata.is_dir() {
            result.extend(snapshot(&path));
        }
    }
    result
}

#[test]
fn database_paths_follow_xdg_and_reject_relative_values_without_creating_state() {
    let fixture = Fixture::new();
    let home = fixture.directory.path();
    assert_eq!(
        DatabasePaths::user(None, Some(home)).unwrap().root,
        home.join(".local/state/aw/packages")
    );
    assert_eq!(
        DatabasePaths::user(Some(Path::new("")), Some(home))
            .unwrap()
            .root,
        home.join(".local/state/aw/packages")
    );
    assert!(DatabasePaths::user(Some(Path::new("relative")), Some(home)).is_err());
    assert!(DatabasePaths::user(None, None).is_err());
    assert!(DatabasePaths::user(None, Some(Path::new("relative"))).is_err());
    assert_eq!(
        DatabasePaths::system().root,
        Path::new("/var/lib/aw/packages")
    );
    assert_eq!(fs::read_dir(home).unwrap().count(), 0);
}

#[test]
fn absent_database_is_empty_and_never_discovers_legacy_prefixes_implicitly() {
    let fixture = Fixture::new();
    fixture.legacy("1.2.3");
    let before = snapshot(fixture.directory.path());
    let inventory = fixture.read().unwrap();
    assert_eq!(inventory.source, InventorySource::Absent);
    assert!(inventory.installations.is_empty());
    assert!(!fixture.paths.root.exists());
    assert_eq!(before, snapshot(fixture.directory.path()));
    assert!(Inventory::read(
        &fixture.paths,
        Some(&fixture.directory.path().join("missing"))
    )
    .is_err());
}

#[test]
fn registered_inventory_and_doctor_are_read_only_and_keep_packages_at_distinct_prefixes() {
    let fixture = Fixture::new();
    let first = fixture.installation();
    let mut second = first.clone();
    let prefix2 = fixture.directory.path().join("other");
    fixture.directory(&prefix2.join("bin"));
    fs::copy(fixture.prefix.join("bin/tool"), prefix2.join("bin/tool")).unwrap();
    second["installation_id"] = json!("install-2");
    second["prefix"] = json!(prefix2);
    fixture.database(vec![first, second]);
    fixture.registration(&fixture.prefix, "install-1");
    fixture.registration(&prefix2, "install-2");
    let before = snapshot(fixture.directory.path());
    let inventory = fixture.read().unwrap();
    assert_eq!(inventory.source, InventorySource::Database);
    assert_eq!(inventory.generation, Some(7));
    assert_eq!(inventory.installations.len(), 2);
    let report = inventory.doctor().unwrap();
    assert!(report.healthy(), "{:?}", report.issues);
    assert_eq!(report.files_checked, 2);
    let selected = Inventory::read(&fixture.paths, Some(&fixture.prefix)).unwrap();
    assert_eq!(selected.installations.len(), 1);
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn explicit_legacy_prefix_reads_stable_versions_and_records_without_registering_them() {
    let fixture = Fixture::new();
    fixture.legacy("1.2.3");
    let before = snapshot(fixture.directory.path());
    for _ in 0..2 {
        let inventory = Inventory::read(&fixture.paths, Some(&fixture.prefix)).unwrap();
        assert_eq!(inventory.source, InventorySource::Legacy);
        let installation = &inventory.installations[0];
        assert_eq!(
            installation.installation_id,
            format!("legacy:{}", fixture.prefix.display())
        );
        assert_eq!(installation.registration, Registration::LegacyUnregistered);
        assert_eq!(installation.packages[0].version, "1.2.3");
        assert_eq!(installation.packages[0].source.bundle_sha256, None);
        assert_eq!(installation.packages[0].provider, None);
        assert!(inventory.doctor().unwrap().healthy());
    }
    assert!(!fixture.paths.root.exists());
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn existing_record_layout_accepts_new_core_files_without_the_removed_cli() {
    let fixture = Fixture::new();
    fixture.legacy("1.2.3");
    let record = fixture.prefix.join(".aw-packages/aw-core.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    value["files"]
        .as_object_mut()
        .unwrap()
        .remove("bin/aw-package");
    fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::remove_file(fixture.prefix.join("bin/aw-package")).unwrap();
    let inventory = Inventory::read(&fixture.paths, Some(&fixture.prefix)).unwrap();
    assert_eq!(inventory.installations[0].packages[0].files.len(), 1);
    assert!(inventory.doctor().unwrap().healthy());
}

#[test]
fn registered_prefix_rejects_live_legacy_records_instead_of_merging_or_hiding_them() {
    let fixture = Fixture::new();
    fixture.legacy("1.2.3");
    fixture.database(vec![fixture.installation()]);
    let before = snapshot(fixture.directory.path());
    let error = Inventory::read(&fixture.paths, Some(&fixture.prefix)).unwrap_err();
    assert!(error.to_string().contains("older package records conflict"));
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn an_oversized_legacy_directory_cannot_hide_mixed_database_state() {
    let fixture = Fixture::new();
    fixture.database(vec![fixture.installation()]);
    let legacy = fixture.prefix.join(".aw-packages");
    fixture.directory(&legacy);
    for number in 0..4097 {
        fs::write(legacy.join(format!("extra-{number}.json")), b"{}").unwrap();
    }
    let error = fixture.read().unwrap_err();
    assert!(error
        .to_string()
        .contains("too many package directory entries"));
}

#[test]
fn installed_listing_survives_payload_damage_and_doctor_reports_every_independent_failure() {
    let fixture = Fixture::new();
    let mut installation = fixture.installation();
    installation["packages"][0]["files"]["bin/missing"] = fixture.file("bin/missing", b"gone");
    fixture.database(vec![installation]);
    fs::remove_file(fixture.prefix.join("bin/missing")).unwrap();
    fs::write(fixture.prefix.join("bin/tool"), b"modified").unwrap();
    fs::set_permissions(
        fixture.prefix.join("bin/tool"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let inventory = fixture.read().unwrap();
    assert_eq!(inventory.installations.len(), 1);
    let report = inventory.doctor().unwrap();
    assert!(!report.healthy());
    for expected in [
        DoctorIssueCode::Missing,
        DoctorIssueCode::ModeMismatch,
        DoctorIssueCode::DigestMismatch,
    ] {
        assert!(
            report.issues.iter().any(|issue| issue.code == expected),
            "{:?}",
            report.issues
        );
    }
    assert_eq!(report.files_checked, 2);
}

#[test]
fn listing_retains_records_when_the_entire_registered_prefix_is_missing_or_non_directory() {
    for replace_with_file in [false, true] {
        let fixture = Fixture::new();
        fixture.database(vec![fixture.installation()]);
        fs::remove_dir_all(&fixture.prefix).unwrap();
        if replace_with_file {
            fs::write(&fixture.prefix, b"not a directory").unwrap();
        }
        let inventory = fixture.read().unwrap();
        assert_eq!(inventory.installations.len(), 1);
        let report = inventory.doctor().unwrap();
        assert!(!report.healthy());
        assert_eq!(report.files_checked, 1);
    }
}

#[test]
fn doctor_never_follows_payload_symlinks_or_blocks_on_a_fifo() {
    let fixture = Fixture::new();
    let mut installation = fixture.installation();
    installation["packages"][0]["files"]["bin/pipe"] = fixture.file("bin/pipe", b"pipe");
    fixture.database(vec![installation]);
    fs::remove_file(fixture.prefix.join("bin/tool")).unwrap();
    fs::remove_file(fixture.prefix.join("bin/pipe")).unwrap();
    symlink("/dev/zero", fixture.prefix.join("bin/tool")).unwrap();
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(fixture.prefix.join("bin/pipe").as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a terminated fixture-owned pathname; no descriptor is retained.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let report = fixture.read().unwrap().doctor().unwrap();
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.code == DoctorIssueCode::Symlink));
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.code == DoctorIssueCode::Nonregular));
}

#[test]
fn unsafe_database_files_and_ancestor_symlinks_are_rejected() {
    for target in ["installed.json", "lock"] {
        let fixture = Fixture::new();
        fixture.database(vec![fixture.installation()]);
        let path = fixture.paths.root.join(target);
        let saved = fixture.directory.path().join("saved");
        fs::rename(&path, &saved).unwrap();
        symlink(&saved, &path).unwrap();
        assert!(fixture.read().is_err(), "{target}");
    }
    let fixture = Fixture::new();
    let real = fixture.directory.path().join("real");
    fixture.directory(&real);
    let link = fixture.directory.path().join("linked");
    symlink(&real, &link).unwrap();
    let paths = DatabasePaths::user(Some(&link), None).unwrap();
    assert!(Inventory::read(&paths, None).is_err());
    assert_eq!(fs::read_dir(real).unwrap().count(), 0);
}

#[test]
fn pending_transactions_and_half_initialized_databases_are_visible_failures() {
    let fixture = Fixture::new();
    fixture.database(vec![fixture.installation()]);
    let transaction = fixture.paths.root.join("transactions/pending-1");
    fixture.directory(&transaction);
    assert!(
        matches!(fixture.read(), Err(Error::NeedsRecovery(path)) if path == fixture.paths.root.join("transactions"))
    );
    fs::remove_dir_all(fixture.paths.root.join("transactions")).unwrap();
    fs::remove_file(fixture.paths.installed()).unwrap();
    assert!(fixture
        .read()
        .unwrap_err()
        .to_string()
        .contains("incomplete"));
    fixture.database(Vec::new());
    fs::remove_file(fixture.paths.root.join("lock")).unwrap();
    assert!(fixture
        .read()
        .unwrap_err()
        .to_string()
        .contains("incomplete"));
    assert!(!fixture.paths.root.join("lock").exists());
}

#[test]
fn malformed_records_reject_unknown_schema_fields_duplicates_and_escaping_paths() {
    let fixture = Fixture::new();
    let original = fixture.database(vec![fixture.installation()]);
    let mut invalid = Vec::new();
    let mut value = original.clone();
    value["schema_version"] = json!(2);
    invalid.push(value);
    let mut value = original.clone();
    value["unknown"] = json!(true);
    invalid.push(value);
    let mut value = original.clone();
    value["installations"][0]["unknown"] = json!(true);
    invalid.push(value);
    let mut value = original.clone();
    value["installations"][0]["prefix"] = json!("/tmp/../outside");
    invalid.push(value);
    let mut value = original.clone();
    value["installations"][0]["packages"][0]["files"]["bin/../../outside"] =
        json!({"sha256":"a".repeat(64),"mode":493});
    invalid.push(value);
    let mut value = original.clone();
    value["installations"]
        .as_array_mut()
        .unwrap()
        .push(original["installations"][0].clone());
    invalid.push(value);
    let mut value = original.clone();
    let package = value["installations"][0]["packages"][0].clone();
    value["installations"][0]["packages"]
        .as_array_mut()
        .unwrap()
        .push(package);
    invalid.push(value);
    for value in invalid {
        fixture.write(&value);
        assert!(fixture.read().is_err(), "accepted {value}");
    }
    fixture.write(&original);
    let text = String::from_utf8(serde_json::to_vec(&original).unwrap())
        .unwrap()
        .replace(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
        );
    fs::write(fixture.paths.installed(), text).unwrap();
    assert!(fixture
        .read()
        .unwrap_err()
        .to_string()
        .contains("duplicate JSON key"));
}

#[test]
fn bounded_reads_reject_oversized_metadata_and_shared_queries_respect_existing_writers() {
    let fixture = Fixture::new();
    fixture.database(vec![fixture.installation()]);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.paths.root.join("lock"))
        .unwrap();
    // SAFETY: lock owns a live descriptor; LOCK_NB bounds acquisition.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(
        matches!(fixture.read(), Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    drop(lock);
    OpenOptions::new()
        .write(true)
        .open(fixture.paths.installed())
        .unwrap()
        .set_len(MAX_DATABASE_BYTES + 1)
        .unwrap();
    assert!(fixture.read().unwrap_err().to_string().contains("exceeds"));
}

#[test]
fn generic_package_descriptions_allow_external_names_stable_versions_and_no_provider() {
    let fixture = Fixture::new();
    let mut package = fixture.package();
    package.as_object_mut().unwrap().remove("source");
    package["schema_version"] = json!(1);
    let path = fixture.directory.path().join("package.json");
    fs::write(&path, serde_json::to_vec(&package).unwrap()).unwrap();
    let description = PackageDescription::read(&path).unwrap();
    assert_eq!(description.package_id, "example.tool");
    assert_eq!(description.version, "1.2.3");
    assert!(description.provider.is_none());
    assert!(description.source_commit.is_none());
    let before = snapshot(fixture.directory.path());
    PackageDescription::read(&path).unwrap();
    assert_eq!(before, snapshot(fixture.directory.path()));
    let original = fs::read_to_string(&path).unwrap();
    let repeated = original.replace(
        "\"files\":{",
        &format!("\"files\":{{\"bin/tool\":{},", package["files"]["bin/tool"]),
    );
    fs::write(&path, repeated).unwrap();
    assert!(PackageDescription::read(&path)
        .unwrap_err()
        .to_string()
        .contains("duplicate JSON key"));
    for version in ["1", "01.0.0", "1.0.0-01", "1.0.0+"] {
        package["version"] = json!(version);
        fs::write(&path, serde_json::to_vec(&package).unwrap()).unwrap();
        assert!(PackageDescription::read(&path).is_err(), "{version}");
    }
}

#[test]
fn installed_database_rejects_unknown_fields_inside_payload_metadata() {
    let fixture = Fixture::new();
    let mut installation = fixture.installation();
    installation["packages"][0]["files"]["bin/tool"]["extra"] = json!(true);
    fixture.database(vec![installation]);
    let before = snapshot(fixture.directory.path());
    let error = fixture.read().unwrap_err();
    assert!(error.to_string().contains("unknown field `extra`"));
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn package_description_rejects_unknown_fields_inside_payload_metadata() {
    let fixture = Fixture::new();
    let mut package = fixture.package();
    package.as_object_mut().unwrap().remove("source");
    package["schema_version"] = json!(1);
    package["files"]["bin/tool"]["extra"] = json!(true);
    let path = fixture.directory.path().join("package.json");
    fs::write(&path, serde_json::to_vec(&package).unwrap()).unwrap();
    let before = snapshot(fixture.directory.path());
    let error = PackageDescription::read(&path).unwrap_err();
    assert!(error.to_string().contains("unknown field `extra`"));
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn new_package_description_needs_only_the_unified_aw_binary() {
    let fixture = Fixture::new();
    let path = fixture.directory.path().join("package.json");
    let value = json!({"schema_version":1,"package_id":"aw-core","version":"1.2.3","os":"linux","arch":std::env::consts::ARCH,"files":{"bin/aw":fixture.file("bin/aw", b"unified CLI")},"dependencies":[]});
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let description = PackageDescription::read(&path).unwrap();
    assert_eq!(description.files.len(), 1);
    assert!(description.files.contains_key("bin/aw"));
    assert!(description.provider.is_none());
}

#[test]
fn native_manager_records_are_listed_but_never_claim_raw_verification() {
    let fixture = Fixture::new();
    let mut installation = fixture.installation();
    installation["manager"] = json!("rpm");
    fixture.database(vec![installation]);
    let report = fixture.read().unwrap().doctor().unwrap();
    assert!(!report.healthy());
    assert_eq!(report.files_checked, 0);
    assert_eq!(report.issues[0].code, DoctorIssueCode::NativeManager);
}

#[test]
fn registered_prefix_locator_selects_its_database_and_rejects_conflicting_authority() {
    let fixture = Fixture::new();
    fixture.database(vec![fixture.installation()]);
    let records = fixture.prefix.join(".aw-packages");
    fixture.directory(&records);
    fixture.record(&fixture.prefix.join(".aw-install.lock"), b"", 0o600);
    let marker = records.join("aw-core.json");
    let pointer = RegistrationPointer::new(&fixture.paths, "install-1".into()).unwrap();
    fixture.record(&marker, serde_json::to_vec(&pointer).unwrap(), 0o644);
    let other = DatabasePaths::user(
        Some(&fixture.directory.path().join("different-state")),
        None,
    )
    .unwrap();
    let selected = other.for_prefix(&fixture.prefix).unwrap();
    assert_eq!(selected.root, fixture.paths.root);
    assert!(Inventory::read(&selected, Some(&fixture.prefix))
        .unwrap()
        .doctor()
        .unwrap()
        .healthy());
    let mut wrong = pointer.clone();
    wrong.installation_id = "different-installation".into();
    fs::write(&marker, serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert!(fixture
        .read()
        .unwrap_err()
        .to_string()
        .contains("does not match"));
    fs::write(&marker, serde_json::to_vec(&pointer).unwrap()).unwrap();
    fs::write(records.join("aw-provider-tokenless.json"), b"{}").unwrap();
    assert!(RegistrationPointer::read(&fixture.prefix)
        .unwrap_err()
        .to_string()
        .contains("conflicts"));
}

#[test]
fn system_locator_cannot_redirect_ordinary_readers_to_an_arbitrary_database() {
    let fixture = Fixture::new();
    let mut pointer = RegistrationPointer::new(&fixture.paths, "install-1".into()).unwrap();
    pointer.scope = aw_package::database::Scope::System;
    assert!(pointer.database_paths().is_err());
    pointer.database_root = DatabasePaths::system().root;
    assert_eq!(
        pointer.database_paths().unwrap().scope(),
        aw_package::database::Scope::System
    );
    pointer.scope = aw_package::database::Scope::Unknown;
    assert!(pointer.database_paths().is_err());
}
