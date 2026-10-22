//! Unified-binary bundles retain compatibility with previously owned CLI files.

use super::*;

pub(super) fn with_legacy_cli(bundle: &Path) {
    let path = bundle.join("payload/bin/aw-package");
    fs::write(&path, b"old owned installer").unwrap();
    modify_manifest(bundle, |manifest| {
        manifest
            .components
            .iter_mut()
            .find(|component| component.component == "aw-core")
            .unwrap()
            .files
            .insert(
                "bin/aw-package".into(),
                Payload {
                    sha256: digest(&path).unwrap(),
                    mode: 0o755,
                },
            );
    });
}

#[test]
fn unified_core_installs_without_legacy_cli_and_uninstall_preserves_unowned_old_names() {
    let fixture = Fixture::new();
    let manifest = inspect(&fixture.core).unwrap();
    assert_eq!(manifest.components[0].files.len(), 1);
    assert!(manifest.components[0].files.contains_key("bin/aw"));
    install(&fixture.core, &fixture.prefix).unwrap();
    assert!(!fixture.prefix.join("bin/aw-package").exists());
    let unowned = fixture.prefix.join("bin/aw-package");
    fs::write(&unowned, b"user-owned file, never enrolled").unwrap();
    install(&fixture.provider, &fixture.prefix).unwrap();
    uninstall(&fixture.prefix).unwrap();
    assert_eq!(
        fs::read(unowned).unwrap(),
        b"user-owned file, never enrolled"
    );
    assert!(!fixture.prefix.join("bin/aw").exists());
}

#[test]
fn legacy_bundle_retains_cli_file_ownership_when_extended_and_uninstalled() {
    let fixture = Fixture::new();
    with_legacy_cli(&fixture.core);
    assert!(inspect(&fixture.core).unwrap().components[0]
        .files
        .contains_key("bin/aw-package"));
    install(&fixture.core, &fixture.prefix).unwrap();
    let database =
        crate::database::DatabasePaths::user(Some(&fixture.root.join("state")), None).unwrap();
    let inventory = crate::database::Inventory::read(&database, Some(&fixture.prefix)).unwrap();
    assert!(inventory.installations[0].packages[0]
        .files
        .contains_key("bin/aw-package"));
    assert!(inventory.doctor().unwrap().healthy());
    drop(inventory);
    install(&fixture.provider, &fixture.prefix).unwrap();
    assert_eq!(
        fs::read(fixture.prefix.join("bin/aw-package")).unwrap(),
        b"old owned installer"
    );
    uninstall(&fixture.prefix).unwrap();
    assert!(!fixture.prefix.join("bin/aw-package").exists());
    assert!(!fixture.prefix.join("bin/aw").exists());
}

#[test]
fn optional_legacy_cli_still_requires_executable_mode_and_matching_digest() {
    for corrupt_mode in [true, false] {
        let fixture = Fixture::new();
        with_legacy_cli(&fixture.core);
        modify_manifest(&fixture.core, |manifest| {
            let payload = manifest.components[0]
                .files
                .get_mut("bin/aw-package")
                .unwrap();
            if corrupt_mode {
                payload.mode = 0o644;
            } else {
                payload.sha256 = "0".repeat(64);
            }
        });
        assert!(inspect(&fixture.core).is_err());
        assert!(install(&fixture.core, &fixture.prefix).is_err());
        assert!(!fixture.prefix.exists());
    }
}

#[test]
fn core_manifest_requires_aw_even_when_an_old_installer_is_present() {
    let fixture = Fixture::new();
    with_legacy_cli(&fixture.core);
    modify_manifest(&fixture.core, |manifest| {
        manifest.components[0].files.remove("bin/aw");
    });
    assert!(inspect(&fixture.core).is_err());
    assert!(install(&fixture.core, &fixture.prefix).is_err());
    assert!(!fixture.prefix.exists());
}

#[test]
fn provider_owned_old_cli_name_keeps_the_general_data_file_mode() {
    let fixture = Fixture::new();
    let path = fixture.provider.join("payload/bin/aw-package");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"provider-owned data").unwrap();
    modify_manifest(&fixture.provider, |manifest| {
        manifest.components[0].files.insert(
            "bin/aw-package".into(),
            Payload {
                sha256: digest(&path).unwrap(),
                mode: 0o644,
            },
        );
    });
    install(&fixture.core, &fixture.prefix).unwrap();
    install(&fixture.provider, &fixture.prefix).unwrap();
    let database =
        crate::database::DatabasePaths::user(Some(&fixture.root.join("state")), None).unwrap();
    let inventory = crate::database::Inventory::read(&database, Some(&fixture.prefix)).unwrap();
    assert!(inventory.doctor().unwrap().healthy());
    drop(inventory);
    uninstall(&fixture.prefix).unwrap();
    assert!(!fixture.prefix.join("bin/aw-package").exists());
}
