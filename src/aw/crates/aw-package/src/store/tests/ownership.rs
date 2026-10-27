//! Manager selection cannot silently acquire another application's file ownership.

use crate::store::{
    database::{
        DatabasePaths, Inventory, InventorySource, PackageManager, RegistrationPointer, Scope,
    },
    plan, Error,
};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    prefix: PathBuf,
    bundle: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir_in("/tmp").unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let bundle = root.join("bundle");
        let payload = bundle.join("payload/bin/tool");
        fs::create_dir_all(payload.parent().unwrap()).unwrap();
        fs::write(&payload, b"fixture tool").unwrap();
        fs::set_permissions(&payload, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            bundle.join("package.json"),
            serde_json::to_vec(&json!({
                "schema_version":1,"package_id":"example.tool","version":"1.0.0",
                "os":"linux","arch":std::env::consts::ARCH,"dependencies":[],
                "files":{"bin/tool":{"sha256":crate::store::digest(&payload).unwrap(),"mode":493}}
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            _temporary: temporary,
            prefix: root.join("installed"),
            bundle,
            root,
        }
    }

    fn paths(&self, manager: PackageManager) -> DatabasePaths {
        DatabasePaths::user_for(manager, Some(&self.root), None).unwrap()
    }

    fn install(&self, manager: PackageManager) -> DatabasePaths {
        let paths = self.paths(manager);
        let addition = plan::install(&paths, &self.bundle, &self.prefix).unwrap();
        assert!(addition.ready(), "{:?}", addition.problems);
        plan::execute(&paths, &addition, &AtomicBool::new(false)).unwrap();
        paths
    }
}

#[test]
fn managers_use_separate_xdg_and_system_databases() {
    let state = Path::new("/state");
    let home = Path::new("/home/example");
    assert_eq!(
        DatabasePaths::user(Some(state), None).unwrap().root,
        state.join("aw/packages")
    );
    assert_eq!(
        DatabasePaths::user_for(PackageManager::Anolisa, Some(state), None)
            .unwrap()
            .root,
        state.join("anolisa/packages")
    );
    assert_eq!(
        DatabasePaths::user_for(PackageManager::Anolisa, None, Some(home))
            .unwrap()
            .root,
        home.join(".local/state/anolisa/packages")
    );
    assert_eq!(
        DatabasePaths::system().root,
        Path::new("/var/lib/aw/packages")
    );
    assert_eq!(
        DatabasePaths::system_for(PackageManager::Anolisa).root,
        Path::new("/var/lib/anolisa/packages")
    );
}

#[test]
fn product_files_are_owned_only_by_the_product_database() {
    let fixture = Fixture::new();
    let product = fixture.install(PackageManager::Anolisa);
    let pointer = RegistrationPointer::read(&fixture.prefix).unwrap().unwrap();
    assert_eq!(pointer.package_manager, PackageManager::Anolisa);
    assert_eq!(pointer.database_paths().unwrap().root, product.root);
    let inventory = Inventory::read(&product, Some(&fixture.prefix)).unwrap();
    assert!(inventory.doctor().unwrap().healthy());
    drop(inventory);
    let aw = fixture.paths(PackageManager::Aw);
    assert_eq!(
        Inventory::read(&aw, None).unwrap().source,
        InventorySource::Absent
    );
    assert!(aw.for_prefix(&fixture.prefix).is_err());
    assert!(plan::install(&aw, &fixture.bundle, &fixture.prefix).is_err());
    assert!(!aw.root.exists());
    let removal = plan::uninstall(&product, &fixture.prefix, &[]).unwrap();
    plan::execute(&product, &removal, &AtomicBool::new(false)).unwrap();
    assert!(!fixture.prefix.join("bin/tool").exists());
}

#[test]
fn current_locators_record_explicit_manager_and_authority() {
    let fixture = Fixture::new();
    let aw = fixture.install(PackageManager::Aw);
    let marker = fixture.prefix.join(".local-packages/installation.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    assert_eq!(value["package_manager"], json!("aw"));
    let pointer = RegistrationPointer::read(&fixture.prefix).unwrap().unwrap();
    assert_eq!(pointer.package_manager, PackageManager::Aw);
    assert_eq!(pointer.database_paths().unwrap().root, aw.root);
    let product = fixture.paths(PackageManager::Anolisa);
    assert!(product.for_prefix(&fixture.prefix).is_err());
    assert!(plan::uninstall(&product, &fixture.prefix, &[]).is_err());
    assert_eq!(
        DatabasePaths::system_for(PackageManager::Aw)
            .for_prefix(&fixture.prefix)
            .unwrap()
            .scope(),
        Scope::User
    );
    assert_eq!(pointer.scope, Scope::User);
    assert!(fixture.prefix.join("bin/tool").is_file());
}

#[test]
fn default_user_paths_can_follow_an_aw_system_locator_for_read_only_queries() {
    let fixture = Fixture::new();
    let records = fixture.prefix.join(".local-packages");
    fs::create_dir_all(&records).unwrap();
    let pointer =
        RegistrationPointer::new(&DatabasePaths::system(), "system-installation".into()).unwrap();
    fs::write(
        records.join("installation.json"),
        serde_json::to_vec(&pointer).unwrap(),
    )
    .unwrap();
    let resolved = fixture
        .paths(PackageManager::Aw)
        .for_prefix(&fixture.prefix)
        .unwrap();
    assert_eq!(resolved.scope(), Scope::System);
    assert_eq!(resolved.root, Path::new("/var/lib/aw/packages"));
    assert_eq!(resolved.manager(), PackageManager::Aw);
}

#[test]
fn execution_binds_manager_even_when_a_caller_reuses_the_same_root() {
    let fixture = Fixture::new();
    let product = fixture.paths(PackageManager::Anolisa);
    let addition = plan::install(&product, &fixture.bundle, &fixture.prefix).unwrap();
    let mut wrong_manager = fixture.paths(PackageManager::Aw);
    wrong_manager.root = product.root.clone();
    let failure = plan::execute(&wrong_manager, &addition, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(failure.error, Error::Invalid(_)));
    assert!(!fixture.prefix.exists());
    assert!(!product.root.exists());
}

#[test]
fn product_mutations_obey_the_same_runtime_leases_as_aw() {
    let fixture = Fixture::new();
    let product = fixture.install(PackageManager::Anolisa);
    let removal = plan::uninstall(&product, &fixture.prefix, &[]).unwrap();
    let leases = aw_exec::PackageLeases::for_paths([fixture.prefix.join("bin/tool")]).unwrap();
    let failure = plan::execute(&product, &removal, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(failure.error, Error::InUse(_)));
    assert!(fixture.prefix.join("bin/tool").is_file());
    drop(leases);
    plan::execute(&product, &removal, &AtomicBool::new(false)).unwrap();
    assert!(!fixture.prefix.join("bin/tool").exists());
}
