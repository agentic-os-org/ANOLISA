//! Seed committed service files separately from first-install filesystem acceptance.

use super::Fixture;
use aw_package::database::{
    DatabasePaths, Installation, InstalledDatabase, InstalledPackage, Inventory, Manager,
    Registration, RegistrationPointer, Scope,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

/// Seed the committed state needed to exercise service-held package locks.
pub(super) fn service(fixture: &mut Fixture, root: &Path) {
    fixture.prefix = root.join("prefix");
    fixture.config = root.join("aw.yaml");
    fixture.state = root.join("state");
    fs::create_dir_all(fixture.prefix.join("bin")).unwrap();
    fs::create_dir(fixture.prefix.join(".local-packages")).unwrap();
    for directory in [
        fixture.prefix.clone(),
        fixture.prefix.join("bin"),
        fixture.prefix.join(".local-packages"),
    ] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let executable = fixture.prefix.join("bin/aw");
    super::copy_executable(fixture.bundle.join("payload/bin/aw"), &executable);
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let prefix_lock = fixture.prefix.join(".local-install.lock");
    fs::write(&prefix_lock, []).unwrap();
    fs::set_permissions(&prefix_lock, fs::Permissions::from_mode(0o600)).unwrap();

    // Keep transaction data on the workspace filesystem used by package acceptance.
    let state_home = fixture.root.join("package-state");
    let paths = DatabasePaths::user(Some(&state_home), None).unwrap();
    fs::create_dir_all(paths.root.join("transactions")).unwrap();
    for directory in [
        &state_home,
        &state_home.join("aw"),
        &paths.root,
        &paths.root.join("transactions"),
    ] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let database_lock = paths.root.join("lock");
    fs::write(&database_lock, []).unwrap();
    fs::set_permissions(&database_lock, fs::Permissions::from_mode(0o600)).unwrap();
    let descriptor = fixture.bundle.join("package.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&descriptor).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("schema_version");
    record["source"] =
        serde_json::json!({"bundle_sha256": aw_package::digest(&descriptor).unwrap()});
    let package: InstalledPackage = serde_json::from_value(record).unwrap();
    let installation_id = "aw-service-test".to_owned();
    let database = InstalledDatabase {
        schema_version: 1,
        generation: 1,
        installations: vec![Installation {
            installation_id: installation_id.clone(),
            prefix: fixture.prefix.clone(),
            resources: None,
            owner_uid: fs::metadata(&fixture.prefix).unwrap().uid(),
            scope: Scope::User,
            manager: Manager::Raw,
            registration: Registration::Registered,
            packages: vec![package],
        }],
    };
    fs::write(
        paths.installed(),
        serde_json::to_vec_pretty(&database).unwrap(),
    )
    .unwrap();
    fs::set_permissions(paths.installed(), fs::Permissions::from_mode(0o644)).unwrap();
    let pointer = RegistrationPointer::new(&paths, installation_id).unwrap();
    // Match the removal plan's exact bytes so an unchanged locator needs no replacement.
    let marker = fixture.prefix.join(".local-packages/installation.json");
    fs::write(&marker, serde_json::to_vec_pretty(&pointer).unwrap()).unwrap();
    fs::set_permissions(marker, fs::Permissions::from_mode(0o644)).unwrap();
    let inventory = Inventory::read(&paths, Some(&fixture.prefix))
        .expect("service package fixture must be a valid registered database");
    assert_eq!(inventory.generation, Some(1));
}
