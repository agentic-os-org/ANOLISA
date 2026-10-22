//! Local package plans and durable execution without Provider processes or remote resolution.

use aw_package::{
    database::{DatabasePaths, Inventory, InventorySource, RegistrationPointer},
    digest, plan,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

struct Fixture {
    directory: tempfile::TempDir,
    paths: DatabasePaths,
    prefix: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/package-management");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        let paths = DatabasePaths::user(Some(directory.path()), None).unwrap();
        let prefix = directory.path().join("installed");
        Self {
            directory,
            paths,
            prefix,
        }
    }

    fn bundle(&self, id: &str, version: &str, name: &str, requires: &[(&str, &str)]) -> PathBuf {
        let bundle = self.directory.path().join(format!("bundle-{id}"));
        let file = bundle.join("payload").join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, format!("ordinary tool {id}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let dependencies = requires
            .iter()
            .map(|(id, version)| json!({"package_id":id,"version":version}))
            .collect::<Vec<_>>();
        let description = json!({"schema_version":1,"package_id":id,"version":version,"os":"linux","arch":std::env::consts::ARCH,"files":{name:{"sha256":digest(&file).unwrap(),"mode":493}},"dependencies":dependencies});
        fs::write(
            bundle.join("package.json"),
            serde_json::to_vec_pretty(&description).unwrap(),
        )
        .unwrap();
        bundle
    }

    fn install(&self, bundle: &Path) {
        let plan = plan::install(&self.paths, bundle, &self.prefix).unwrap();
        assert!(plan.ready(), "{:?}", plan.problems);
        let report = plan::execute(&self.paths, &plan, &AtomicBool::new(false)).unwrap();
        assert!(report.committed);
    }

    fn legacy(&self) -> Vec<u8> {
        fs::create_dir_all(self.prefix.join("bin")).unwrap();
        let file = self.prefix.join("bin/aw");
        fs::write(&file, b"older AW executable").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(self.prefix.join(".aw-packages")).unwrap();
        fs::write(self.prefix.join(".aw-install.lock"), b"").unwrap();
        let record = json!({"format":1,"component":"aw-core","version":"vendor/dev-build","source_commit":"vendor-provided-build-label","os":"linux","arch":std::env::consts::ARCH,"provider_protocol":"aw-provider/v1alpha1","requires":{},"files":{"bin/aw":{"sha256":digest(&file).unwrap(),"mode":493}}});
        let bytes = serde_json::to_vec_pretty(&record).unwrap();
        fs::write(self.prefix.join(".aw-packages/aw-core.json"), &bytes).unwrap();
        bytes
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, u64, Vec<u8>)> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.is_file() {
            fs::read(&path).unwrap()
        } else {
            Vec::new()
        };
        result.insert(path.clone(), (metadata.mode(), metadata.ino(), bytes));
        if metadata.is_dir() {
            result.extend(snapshot(&path));
        }
    }
    result
}

#[test]
fn generic_install_plan_is_read_only_and_does_not_require_aw_core() {
    let fixture = Fixture::new();
    let bundle = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    let before = snapshot(fixture.directory.path());
    let plan = plan::install(&fixture.paths, &bundle, &fixture.prefix).unwrap();
    assert!(plan.ready(), "{:?}", plan.problems);
    assert_eq!(plan.added[0].package_id, "example.base");
    assert_eq!(plan.files[0].path, "bin/base");
    assert!(!fixture.prefix.exists());
    assert!(!fixture.paths.root.exists());
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn independent_tool_install_append_remove_and_reinstall_keep_one_database_authority() {
    let fixture = Fixture::new();
    let base = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    let mant = fixture.bundle(
        "example.mant",
        "7.4.2",
        "bin/mant",
        &[("example.base", "2.1.0")],
    );
    fixture.install(&base);
    fixture.install(&mant);
    fs::write(fixture.prefix.join("user-owned.txt"), b"preserve me").unwrap();
    let blocked =
        plan::uninstall(&fixture.paths, &fixture.prefix, &["example.base".into()]).unwrap();
    assert!(blocked
        .problems
        .iter()
        .any(|problem| problem.code == "required_by"));
    let before = snapshot(fixture.directory.path());
    let removal = plan::uninstall(&fixture.paths, &fixture.prefix, &[]).unwrap();
    assert!(removal.ready(), "{:?}", removal.problems);
    assert_eq!(before, snapshot(fixture.directory.path()));
    plan::execute(&fixture.paths, &removal, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        fs::read(fixture.prefix.join("user-owned.txt")).unwrap(),
        b"preserve me"
    );
    assert!(!fixture.prefix.join("bin/base").exists());
    assert!(!fixture.prefix.join("bin/mant").exists());
    let inventory = Inventory::read(&fixture.paths, Some(&fixture.prefix)).unwrap();
    assert_eq!(inventory.source, InventorySource::Database);
    assert!(inventory.installations[0].packages.is_empty());
    assert!(inventory.doctor().unwrap().healthy());
    let id = inventory.installations[0].installation_id.clone();
    drop(inventory);
    assert_eq!(
        RegistrationPointer::read(&fixture.prefix)
            .unwrap()
            .unwrap()
            .installation_id,
        id
    );
    fixture.install(&base);
    let inventory = Inventory::read(&fixture.paths, Some(&fixture.prefix)).unwrap();
    assert_eq!(inventory.installations[0].installation_id, id);
    assert_eq!(inventory.installations[0].packages.len(), 1);
    assert!(inventory.doctor().unwrap().healthy());
}

#[test]
fn exact_dependencies_and_duplicate_packages_block_install_without_writes() {
    let fixture = Fixture::new();
    let base = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    let mant = fixture.bundle(
        "example.mant",
        "7.4.2",
        "bin/mant",
        &[("example.base", "2.0.0")],
    );
    let before = snapshot(fixture.directory.path());
    let missing = plan::install(&fixture.paths, &mant, &fixture.prefix).unwrap();
    assert!(missing
        .problems
        .iter()
        .any(|problem| problem.code == "dependency"));
    assert!(missing.dependencies[0].installation_id.is_none());
    assert_eq!(before, snapshot(fixture.directory.path()));
    fixture.install(&base);
    let before = snapshot(fixture.directory.path());
    let mismatch = plan::install(&fixture.paths, &mant, &fixture.prefix).unwrap();
    assert!(mismatch
        .problems
        .iter()
        .any(|problem| problem.code == "dependency"));
    assert!(mismatch.dependencies[0].installation_id.is_some());
    let duplicate = plan::install(&fixture.paths, &base, &fixture.prefix).unwrap();
    assert!(duplicate
        .problems
        .iter()
        .any(|problem| problem.code == "already_installed"));
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn existing_unknown_files_owned_paths_and_symlink_parents_are_never_replaced() {
    let fixture = Fixture::new();
    let base = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    fixture.install(&base);
    let overlap = fixture.bundle("example.other", "1.0.0", "bin/base", &[]);
    let before = snapshot(fixture.directory.path());
    let blocked = plan::install(&fixture.paths, &overlap, &fixture.prefix).unwrap();
    assert!(blocked
        .problems
        .iter()
        .any(|problem| problem.code == "file_owner"));
    assert_eq!(before, snapshot(fixture.directory.path()));
    let tool = fixture.bundle("example.tool", "1.0.0", "bin/tool", &[]);
    fs::write(fixture.prefix.join("bin/tool"), b"mine").unwrap();
    let blocked = plan::install(&fixture.paths, &tool, &fixture.prefix).unwrap();
    assert!(blocked
        .problems
        .iter()
        .any(|problem| problem.code == "file_exists"));
    fs::remove_file(fixture.prefix.join("bin/tool")).unwrap();
    let other = fixture.directory.path().join("other-prefix");
    fs::create_dir(&other).unwrap();
    symlink(fixture.prefix.join("bin"), other.join("bin")).unwrap();
    let blocked = plan::install(&fixture.paths, &tool, &other).unwrap();
    assert!(blocked
        .problems
        .iter()
        .any(|problem| problem.code == "file_exists"));
    assert_eq!(
        fs::read(fixture.prefix.join("bin/base")).unwrap(),
        b"ordinary tool example.base\n"
    );
}

#[test]
fn stale_database_or_changed_bundle_is_rejected_before_payload_publication() {
    let fixture = Fixture::new();
    let base = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    fixture.install(&base);
    let mant = fixture.bundle("example.mant", "7.4.2", "bin/mant", &[]);
    let stale = plan::install(&fixture.paths, &mant, &fixture.prefix).unwrap();
    let other = fixture.bundle("example.other", "1.0.0", "bin/other", &[]);
    fixture.install(&other);
    let error = plan::execute(&fixture.paths, &stale, &AtomicBool::new(false)).unwrap_err();
    assert!(error.error.to_string().contains("changed after planning"));
    assert!(!fixture.prefix.join("bin/mant").exists());
    let stale = plan::install(&fixture.paths, &mant, &fixture.prefix).unwrap();
    fs::write(mant.join("payload/bin/mant"), b"changed source").unwrap();
    assert!(plan::execute(&fixture.paths, &stale, &AtomicBool::new(false)).is_err());
    assert!(!fixture.prefix.join("bin/mant").exists());
    assert!(Inventory::read(&fixture.paths, None)
        .unwrap()
        .doctor()
        .unwrap()
        .healthy());
}

#[test]
fn registered_reverse_dependencies_in_other_prefixes_prevent_removal() {
    let fixture = Fixture::new();
    let base = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    fixture.install(&base);
    let id = Inventory::read(&fixture.paths, None).unwrap().installations[0]
        .installation_id
        .clone();
    let other_prefix = fixture.directory.path().join("second");
    let other = fixture.bundle("example.other", "1.0.0", "bin/other", &[]);
    let second = plan::install(&fixture.paths, &other, &other_prefix).unwrap();
    plan::execute(&fixture.paths, &second, &AtomicBool::new(false)).unwrap();
    let mut database: Value =
        serde_json::from_slice(&fs::read(fixture.paths.installed()).unwrap()).unwrap();
    database["installations"][1]["packages"][0]["dependencies"] =
        json!([{"installation_id":id,"package_id":"example.base","version":"2.1.0"}]);
    fs::write(
        fixture.paths.installed(),
        serde_json::to_vec(&database).unwrap(),
    )
    .unwrap();
    let before = snapshot(fixture.directory.path());
    let removal = plan::uninstall(&fixture.paths, &fixture.prefix, &[]).unwrap();
    assert!(removal.problems.iter().any(
        |problem| problem.code == "required_by" && problem.path.as_ref() == Some(&other_prefix)
    ));
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn legacy_append_archives_exact_bytes_and_preserves_opaque_old_build_facts() {
    let fixture = Fixture::new();
    fixture.legacy();
    // This caller's executable uses the current runtime contract with historical record bytes.
    let executable = fixture.prefix.join("bin/aw");
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let record = fixture.prefix.join(".aw-packages/aw-core.json");
    let mut component: aw_package::Component =
        serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    component.files.get_mut("bin/aw").unwrap().sha256 = digest(&executable).unwrap();
    let bytes = serde_json::to_vec_pretty(&component).unwrap();
    fs::write(record, &bytes).unwrap();
    let bundle = fixture.bundle("example.mant", "7.4.2", "bin/mant", &[]);
    let before = snapshot(fixture.directory.path());
    let addition = plan::install(&fixture.paths, &bundle, &fixture.prefix).unwrap();
    assert!(addition.ready(), "{:?}", addition.problems);
    assert!(addition.register_legacy);
    assert_eq!(before, snapshot(fixture.directory.path()));
    plan::execute(&fixture.paths, &addition, &AtomicBool::new(false)).unwrap();
    let pointer = RegistrationPointer::read(&fixture.prefix).unwrap().unwrap();
    let attempts = fs::read_dir(
        fixture
            .paths
            .root
            .join("legacy")
            .join(&pointer.installation_id),
    )
    .unwrap()
    .map(|entry| entry.unwrap().path())
    .collect::<Vec<_>>();
    assert_eq!(attempts.len(), 1);
    assert_eq!(fs::read(attempts[0].join("aw-core.json")).unwrap(), bytes);
    assert!(serde_json::from_slice::<aw_package::Component>(
        &fs::read(fixture.prefix.join(".aw-packages/aw-core.json")).unwrap()
    )
    .is_err());
    let inventory = Inventory::read(&fixture.paths, Some(&fixture.prefix)).unwrap();
    let core = inventory.installations[0]
        .packages
        .iter()
        .find(|package| package.package_id == "aw-core")
        .unwrap();
    assert_eq!(core.version, "vendor/dev-build");
    assert_eq!(
        core.source.source_commit.as_deref(),
        Some("vendor-provided-build-label")
    );
    assert!(inventory.doctor().unwrap().healthy());
}

#[test]
fn unknown_legacy_runtime_blocks_registration_before_any_writes() {
    let fixture = Fixture::new();
    fixture.legacy();
    let bundle = fixture.bundle("example.mant", "7.4.2", "bin/mant", &[]);
    let before = snapshot(fixture.directory.path());
    let addition = plan::install(&fixture.paths, &bundle, &fixture.prefix).unwrap();
    let blocker = addition
        .problems
        .iter()
        .find(|problem| problem.code == "runtime_unknown")
        .unwrap();
    assert!(blocker.message.contains("separate empty prefix"));
    assert!(!addition.ready());
    assert!(plan::execute(&fixture.paths, &addition, &AtomicBool::new(false)).is_err());
    assert_eq!(before, snapshot(fixture.directory.path()));
    assert!(!fixture.paths.root.exists());
    assert!(RegistrationPointer::read(&fixture.prefix)
        .unwrap()
        .is_none());
}

#[test]
fn modified_payload_and_unknown_old_runtime_block_destructive_plans() {
    let fixture = Fixture::new();
    fixture.legacy();
    let removal = plan::uninstall(&fixture.paths, &fixture.prefix, &[]).unwrap();
    assert!(removal
        .problems
        .iter()
        .any(|problem| problem.code == "runtime_unknown"));
    let fixture = Fixture::new();
    let base = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    fixture.install(&base);
    fs::write(fixture.prefix.join("bin/base"), b"user changed this").unwrap();
    let before = snapshot(fixture.directory.path());
    let removal = plan::uninstall(&fixture.paths, &fixture.prefix, &[]).unwrap();
    assert!(removal
        .problems
        .iter()
        .any(|problem| problem.code == "file_changed"));
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn malformed_or_competing_bundle_descriptions_and_unsafe_sources_are_rejected() {
    let fixture = Fixture::new();
    let bundle = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    fs::write(bundle.join("manifest.json"), b"{}").unwrap();
    assert!(plan::install(&fixture.paths, &bundle, &fixture.prefix).is_err());
    fs::remove_file(bundle.join("manifest.json")).unwrap();
    let payload = bundle.join("payload/bin/base");
    fs::remove_file(&payload).unwrap();
    symlink("/dev/zero", &payload).unwrap();
    assert!(plan::install(&fixture.paths, &bundle, &fixture.prefix).is_err());
    assert!(!fixture.paths.root.exists());
    assert!(!fixture.prefix.exists());
}

#[test]
fn a_cancelled_plan_does_not_create_database_or_prefix() {
    let fixture = Fixture::new();
    let bundle = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
    let plan = plan::install(&fixture.paths, &bundle, &fixture.prefix).unwrap();
    let before = snapshot(fixture.directory.path());
    let error = plan::execute(&fixture.paths, &plan, &AtomicBool::new(true)).unwrap_err();
    assert!(matches!(error.error, aw_package::Error::Cancelled));
    assert!(!error.report.committed);
    assert_eq!(before, snapshot(fixture.directory.path()));
}

#[test]
fn configure_uses_registered_packages_and_the_prefix_recorded_database() {
    let fixture = Fixture::new();
    let core = fixture.bundle("aw-core", "3.1.0", "bin/aw", &[]);
    fixture.install(&core);
    let config = fixture.directory.path().join("aw.yaml");
    aw_package::configure(&aw_package::Settings {
        prefix: &fixture.prefix,
        config: &config,
        state: &fixture.directory.path().join("state"),
        policy: aw_package::Policy::None,
        qoder: Some(Path::new("/bin/true")),
        node: None,
        openclaw: None,
    })
    .unwrap();
    assert!(config.is_file());
    assert_eq!(fs::metadata(&config).unwrap().mode() & 0o777, 0o600);
    assert!(RegistrationPointer::read(&fixture.prefix)
        .unwrap()
        .is_some());
}

#[test]
fn externally_managed_installations_reject_raw_install_and_uninstall() {
    for manager in ["rpm", "deb"] {
        for empty in [false, true] {
            let fixture = Fixture::new();
            let base = fixture.bundle("example.base", "2.1.0", "bin/base", &[]);
            fixture.install(&base);
            let mut database: Value =
                serde_json::from_slice(&fs::read(fixture.paths.installed()).unwrap()).unwrap();
            database["installations"][0]["manager"] = json!(manager);
            if empty {
                database["installations"][0]["packages"] = json!([]);
            }
            fs::write(
                fixture.paths.installed(),
                serde_json::to_vec(&database).unwrap(),
            )
            .unwrap();
            let extra = fixture.bundle("example.extra", "1.0.0", "bin/extra", &[]);
            let before = snapshot(fixture.directory.path());
            let addition = plan::install(&fixture.paths, &extra, &fixture.prefix).unwrap();
            let removal = plan::uninstall(&fixture.paths, &fixture.prefix, &[]).unwrap();
            for plan in [&addition, &removal] {
                assert!(!plan.ready(), "{manager}, empty={empty}");
                let blocker = if empty && plan.action == plan::Action::Uninstall {
                    "not_installed"
                } else {
                    "native_manager"
                };
                assert!(plan.problems.iter().any(|problem| problem.code == blocker));
                let failure =
                    plan::execute(&fixture.paths, plan, &AtomicBool::new(false)).unwrap_err();
                assert!(!failure.report.committed);
            }
            assert_eq!(before, snapshot(fixture.directory.path()));
            assert!(!fixture.prefix.join("bin/extra").exists());
            assert!(fixture.prefix.join("bin/base").is_file());
        }
    }
}
