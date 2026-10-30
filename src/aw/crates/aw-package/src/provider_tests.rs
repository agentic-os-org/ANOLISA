//! Provider admission applies to library callers as well as the unified CLI.

use crate::store::{database::PackageManager, digest};
use crate::{database::DatabasePaths, plan, transaction, Error, Policy, Settings};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::atomic::AtomicBool};

struct Fixture {
    directory: tempfile::TempDir,
    paths: DatabasePaths,
    prefix: PathBuf,
    bundle: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = DatabasePaths::user(Some(&root), None).unwrap();
        let bundle = root.join("bundle");
        fs::create_dir_all(bundle.join("payload/bin")).unwrap();
        let file = bundle.join("payload/bin/provider");
        fs::write(&file, b"Provider fixture").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let description = json!({
            "schema_version":1,"package_id":"external.provider","version":"3.2.1",
            "os":"linux","arch":std::env::consts::ARCH,
            "files":{"bin/provider":{"sha256":digest(&file).unwrap(),"mode":493}},
            "dependencies":[],"provider":{"protocol":"aw-provider/v1alpha1"}
        });
        fs::write(
            bundle.join("package.json"),
            serde_json::to_vec(&description).unwrap(),
        )
        .unwrap();
        Self {
            directory,
            paths,
            prefix: root.join("installed"),
            bundle,
        }
    }

    fn change(&self, update: impl FnOnce(&mut Value)) {
        let path = self.bundle.join("package.json");
        let mut value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        update(&mut value);
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn absent(&self) {
        assert!(!self.paths.root.exists());
        assert!(!self.prefix.exists());
    }
}

#[test]
fn ordinary_and_new_core_bundles_are_rejected_by_library_install() {
    for (id, declared) in [
        ("ordinary.tool", false),
        ("aw-core", false),
        ("aw-core", true),
    ] {
        let fixture = Fixture::new();
        fixture.change(|value| {
            value["package_id"] = json!(id);
            if !declared {
                value.as_object_mut().unwrap().remove("provider");
            }
        });
        let error = plan::install(&fixture.paths, &fixture.bundle, &fixture.prefix).unwrap_err();
        assert!(error.to_string().contains("local Provider extensions"));
        fixture.absent();
    }
}

#[test]
fn independent_provider_install_preserves_plan_json_and_owned_cleanup() {
    let fixture = Fixture::new();
    let addition = plan::install(&fixture.paths, &fixture.bundle, &fixture.prefix).unwrap();
    let shared =
        crate::store::plan::install(&fixture.paths, &fixture.bundle, &fixture.prefix).unwrap();
    assert_eq!(
        serde_json::to_value(&addition).unwrap(),
        serde_json::to_value(shared).unwrap()
    );
    assert_eq!(addition.added[0].package_id, "external.provider");
    fixture.absent();
    plan::execute(&fixture.paths, &addition, &AtomicBool::new(false)).unwrap();
    fs::write(fixture.prefix.join("user-file"), b"keep").unwrap();
    let inventory = crate::database::Inventory::read(&fixture.paths, None).unwrap();
    assert!(inventory.doctor().unwrap().healthy());
    drop(inventory);
    let removal = plan::uninstall(
        &fixture.paths,
        &fixture.prefix,
        &["external.provider".into()],
    )
    .unwrap();
    plan::execute(&fixture.paths, &removal, &AtomicBool::new(false)).unwrap();
    assert!(!fixture.prefix.join("bin/provider").exists());
    assert_eq!(fs::read(fixture.prefix.join("user-file")).unwrap(), b"keep");
    assert_eq!(
        transaction::recover(&fixture.paths, &AtomicBool::new(false))
            .unwrap()
            .committed,
        0
    );
}

#[test]
fn foreign_manager_cannot_use_aw_mutation_or_recovery_entries() {
    let fixture = Fixture::new();
    let root = fixture.directory.path().canonicalize().unwrap();
    let foreign = DatabasePaths::user_for(PackageManager::Anolisa, Some(&root), None).unwrap();
    assert!(plan::install(&foreign, &fixture.bundle, &fixture.prefix).is_err());
    assert!(plan::uninstall(&foreign, &fixture.prefix, &[]).is_err());
    assert!(transaction::recover(&foreign, &AtomicBool::new(false)).is_err());
    let addition = plan::install(&fixture.paths, &fixture.bundle, &fixture.prefix).unwrap();
    let failure = plan::execute(&foreign, &addition, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(failure.error, Error::Invalid(_)));
    assert!(!foreign.root.exists());
    fixture.absent();
}

#[test]
fn aw_cannot_claim_or_recover_an_ordinary_software_database() {
    let fixture = Fixture::new();
    fixture.change(|value| {
        value.as_object_mut().unwrap().remove("provider");
    });
    let addition =
        crate::store::plan::install(&fixture.paths, &fixture.bundle, &fixture.prefix).unwrap();
    crate::store::plan::execute(&fixture.paths, &addition, &AtomicBool::new(false)).unwrap();
    let before = fs::read(fixture.paths.installed()).unwrap();
    assert!(crate::database::Inventory::read(&fixture.paths, None).is_err());
    assert!(plan::uninstall(&fixture.paths, &fixture.prefix, &[]).is_err());
    assert!(transaction::recover(&fixture.paths, &AtomicBool::new(false)).is_err());
    assert_eq!(fs::read(fixture.paths.installed()).unwrap(), before);
    assert!(fixture.prefix.join("bin/provider").is_file());
}

#[test]
fn previous_manifest_bundles_are_rejected_before_any_writes() {
    for id in ["aw-core", "ordinary.tool"] {
        let fixture = Fixture::new();
        let path = fixture.bundle.join("package.json");
        let file = fixture.bundle.join("payload/bin/aw");
        fs::copy(fixture.bundle.join("payload/bin/provider"), &file).unwrap();
        let manifest = json!({"format":1,"components":[{
            "format":1,"component":id,"version":"0.1.0",
            "source_commit":"a".repeat(40),"os":"linux","arch":std::env::consts::ARCH,
            "provider_protocol":"aw-provider/v1alpha1","requires":{},
            "files":{"bin/aw":{"sha256":digest(&file).unwrap(),"mode":493}}
        }]});
        fs::remove_file(path).unwrap();
        fs::write(
            fixture.bundle.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let result = plan::install(&fixture.paths, &fixture.bundle, &fixture.prefix);
        assert!(result.is_err());
        fixture.absent();
    }
}

#[test]
fn historical_receipts_cannot_be_queried_configured_migrated_or_removed_by_aw() {
    let fixture = Fixture::new();
    let file = fixture.bundle.join("payload/bin/aw");
    fs::copy(fixture.bundle.join("payload/bin/provider"), &file).unwrap();
    fs::remove_file(fixture.bundle.join("package.json")).unwrap();
    let manifest = json!({"format":1,"components":[{
        "format":1,"component":"aw-core","version":"0.1.0",
        "source_commit":"a".repeat(40),"os":"linux","arch":std::env::consts::ARCH,
        "provider_protocol":"aw-provider/v1alpha1","requires":{},
        "files":{"bin/aw":{"sha256":digest(&file).unwrap(),"mode":493}}
    }]});
    fs::write(
        fixture.bundle.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let records = fixture.prefix.join(".aw-packages");
    fs::create_dir_all(&records).unwrap();
    let receipt = records.join("aw-core.json");
    fs::write(
        &receipt,
        serde_json::to_vec(&manifest["components"][0]).unwrap(),
    )
    .unwrap();
    let before = fs::read(&receipt).unwrap();
    assert!(crate::database::Inventory::read(&fixture.paths, Some(&fixture.prefix)).is_err());
    assert!(plan::uninstall(&fixture.paths, &fixture.prefix, &[]).is_err());
    let incoming = Fixture::new();
    assert!(plan::install(&fixture.paths, &incoming.bundle, &fixture.prefix).is_err());
    let root = fixture.directory.path().canonicalize().unwrap();
    let config = root.join("aw.yaml");
    assert!(crate::configure(&Settings {
        prefix: &fixture.prefix,
        config: &config,
        state: &root.join("state"),
        policy: Policy::None,
        qoder: Some(std::path::Path::new("/bin/true")),
        node: None,
        openclaw: None,
    })
    .is_err());
    assert_eq!(fs::read(receipt).unwrap(), before);
    assert!(!fixture.paths.root.exists());
    assert!(!config.exists());
}

#[test]
fn unmanaged_configuration_needs_no_raw_registration_or_installed_core() {
    let fixture = Fixture::new();
    let root = fixture.directory.path().canonicalize().unwrap();
    let config = root.join("aw.yaml");
    crate::configure(&Settings {
        prefix: &fixture.prefix,
        config: &config,
        state: &root.join("state"),
        policy: Policy::None,
        qoder: Some(std::path::Path::new("/bin/true")),
        node: None,
        openclaw: None,
    })
    .unwrap();
    assert!(config.is_file());
    fixture.absent();
}

#[test]
fn checked_planning_binds_admission_to_the_descriptor_used_for_execution() {
    let fixture = Fixture::new();
    let addition = crate::store::plan::install_checked(
        &fixture.paths,
        &fixture.bundle,
        &fixture.prefix,
        |input| {
            assert!(input.packages[0].provider.is_some());
            fixture.change(|value| {
                value.as_object_mut().unwrap().remove("provider");
            });
            Ok(())
        },
    )
    .unwrap();
    let failure = crate::store::plan::execute(&fixture.paths, &addition, &AtomicBool::new(false))
        .unwrap_err();
    assert!(failure
        .error
        .to_string()
        .contains("bundle description changed"));
    assert!(!fixture.prefix.join("bin/provider").exists());
}
