//! contract tests for the  command.

use super::super::tests::*;

use crate::commands::common;
use tempfile::tempdir;

#[test]
fn adopt_snapshots_datadir_contract() {
    let (_tmp, ctx) = system_ctx_with_raw_repo(false);
    let layout = common::resolve_layout(&ctx);
    let contract = component_manifest_toml("copilot-shell", "2.3.0", &["system"]);
    seed_datadir_contract(&layout, "copilot-shell", &contract);

    let q = FakeQuery {
        installed: vec![(
            "copilot-shell".to_string(),
            pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"),
        )],
        origins: vec![("copilot-shell".to_string(), "@System".to_string())],
        ..Default::default()
    };
    crate::commands::tier1::adopt::adopt_with_query("copilot-shell", None, &ctx, &q)
        .expect("adopt ok");

    let snapshot = common::installed_component_manifest_path(&layout, "copilot-shell", COMMAND)
        .expect("snapshot path");
    assert!(
        snapshot.exists(),
        "adopt must snapshot the datadir contract to {snapshot:?}"
    );
    let content = std::fs::read_to_string(&snapshot).expect("read snapshot");
    assert_eq!(content, contract, "snapshot must be a verbatim copy");
}

#[test]
fn adopt_without_datadir_contract_succeeds_with_warning() {
    let (_tmp, ctx) = system_ctx_with_raw_repo(false);
    let layout = common::resolve_layout(&ctx);
    // Deliberately do NOT seed a datadir contract.

    let q = FakeQuery {
        installed: vec![(
            "copilot-shell".to_string(),
            pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"),
        )],
        origins: vec![("copilot-shell".to_string(), "@System".to_string())],
        ..Default::default()
    };
    crate::commands::tier1::adopt::adopt_with_query("copilot-shell", None, &ctx, &q)
        .expect("adopt must succeed even without a contract");

    let snapshot = common::installed_component_manifest_path(&layout, "copilot-shell", COMMAND)
        .expect("snapshot path");
    assert!(
        !snapshot.exists(),
        "no snapshot when the datadir contract is absent"
    );
}

#[test]
fn delegated_install_snapshots_datadir_contract() {
    let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
    let layout = common::resolve_layout(&ctx);
    let contract = component_manifest_toml("copilot-shell", "2.3.0", &["system"]);
    seed_datadir_contract(&layout, "copilot-shell", &contract);

    let fake = FakeInstaller::new(
        "copilot-shell",
        pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"),
    )
    .with_origin("anolisa");
    let mut a = args("copilot-shell");
    a.backend = Some("rpm".to_string());

    let outcome = install_component_with_deps("copilot-shell", &a, &ctx, &fake, &fake, true)
        .expect("delegated install ok");
    assert_eq!(outcome, InstallOutcome::Installed);

    let snapshot = common::installed_component_manifest_path(&layout, "copilot-shell", COMMAND)
        .expect("snapshot path");
    assert!(
        snapshot.exists(),
        "delegated install must snapshot the datadir contract to {snapshot:?}"
    );
    let content = std::fs::read_to_string(&snapshot).expect("read snapshot");
    assert_eq!(content, contract, "snapshot must be a verbatim copy");
}

#[test]
fn delegated_install_without_datadir_contract_succeeds() {
    let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
    let layout = common::resolve_layout(&ctx);
    // No datadir contract seeded.

    let fake = FakeInstaller::new(
        "copilot-shell",
        pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"),
    )
    .with_origin("anolisa");
    let mut a = args("copilot-shell");
    a.backend = Some("rpm".to_string());

    let outcome = install_component_with_deps("copilot-shell", &a, &ctx, &fake, &fake, true)
        .expect("delegated install must succeed without a contract");
    assert_eq!(outcome, InstallOutcome::Installed);

    let snapshot = common::installed_component_manifest_path(&layout, "copilot-shell", COMMAND)
        .expect("snapshot path");
    assert!(
        !snapshot.exists(),
        "no snapshot when the datadir contract is absent"
    );
}

#[test]
fn adopt_snapshots_packaged_datadir_contract() {
    let (_tmp, ctx) = system_ctx_with_raw_repo(false);
    let layout = common::resolve_layout(&ctx);

    // Seed the contract in a separate "packaged" dir (not layout.datadir).
    let packaged = _tmp.path().join("packaged_share_anolisa");
    let contract = component_manifest_toml("copilot-shell", "2.3.0", &["system"]);
    let contract_dir = packaged.join("components").join("copilot-shell");
    std::fs::create_dir_all(&contract_dir).expect("mkdir packaged contract");
    std::fs::write(contract_dir.join("component.toml"), &contract)
        .expect("write packaged contract");

    let ctx = ctx.with_packaged_data_root(packaged);

    let q = FakeQuery {
        installed: vec![(
            "copilot-shell".to_string(),
            pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"),
        )],
        origins: vec![("copilot-shell".to_string(), "@System".to_string())],
        ..Default::default()
    };
    crate::commands::tier1::adopt::adopt_with_query("copilot-shell", None, &ctx, &q)
        .expect("adopt ok");

    let snapshot = common::installed_component_manifest_path(&layout, "copilot-shell", COMMAND)
        .expect("snapshot path");
    assert!(
        snapshot.exists(),
        "adopt must snapshot from packaged datadir to {snapshot:?}"
    );
    let content = std::fs::read_to_string(&snapshot).expect("read snapshot");
    assert_eq!(
        content, contract,
        "snapshot must be a verbatim copy of the packaged contract"
    );
}

#[test]
fn adopt_snapshots_fhs_package_datadir_contract() {
    let tmp = tempdir().expect("tempdir");
    let prefix = tmp.path().join("sys");
    let ctx = ctx_with_prefix(false, Some(prefix));
    let layout = common::resolve_layout(&ctx);
    seed_repo_config_with_index(&layout, TEST_INDEX_COMPONENTS);

    let contract = component_manifest_toml("copilot-shell", "2.3.0", &["system"]);
    let package_datadir = layout.package_datadir().expect("package datadir");
    let contract_dir = package_datadir.join("components").join("copilot-shell");
    std::fs::create_dir_all(&contract_dir).expect("mkdir package contract");
    std::fs::write(contract_dir.join("component.toml"), &contract).expect("write package contract");

    assert_ne!(
        package_datadir, layout.datadir,
        "test requires package datadir to differ from raw install datadir"
    );

    let q = FakeQuery {
        installed: vec![(
            "copilot-shell".to_string(),
            pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"),
        )],
        origins: vec![("copilot-shell".to_string(), "@System".to_string())],
        ..Default::default()
    };
    crate::commands::tier1::adopt::adopt_with_query("copilot-shell", None, &ctx, &q)
        .expect("adopt ok");

    let snapshot = common::installed_component_manifest_path(&layout, "copilot-shell", COMMAND)
        .expect("snapshot path");
    assert!(
        snapshot.exists(),
        "adopt must snapshot from FHS package datadir to {snapshot:?}"
    );
    let content = std::fs::read_to_string(&snapshot).expect("read snapshot");
    assert_eq!(
        content, contract,
        "snapshot must be a verbatim copy of the FHS package contract"
    );
}

#[test]
fn snapshot_datadir_contract_writes_provenance() {
    let (_tmp, ctx) = system_ctx_with_raw_repo(false);
    let layout = common::resolve_layout(&ctx);
    let contract = component_manifest_toml("sec-core", "1.0.0", &["system"]);
    seed_datadir_contract(&layout, "sec-core", &contract);

    let warnings =
        snapshot_datadir_contract(&layout, "sec-core", COMMAND, ctx.packaged_data_probe());
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let snapshot = common::installed_component_manifest_path(&layout, "sec-core", COMMAND)
        .expect("snapshot path");
    assert!(snapshot.exists(), "component.toml snapshot must exist");

    let prov_path = anolisa_platform::fs_layout::FsLayout::provenance_path_for_snapshot(&snapshot);
    assert!(
        prov_path.exists(),
        "provenance.toml must exist alongside snapshot"
    );

    let prov: anolisa_core::adapter::contract::ContractProvenance =
        toml::from_str(&std::fs::read_to_string(&prov_path).expect("read prov"))
            .expect("parse prov");
    assert_eq!(prov.schema_version, 1);
    assert_eq!(
        prov.source_kind,
        anolisa_core::adapter::contract::ContractSourceKind::Datadir,
    );
    assert_eq!(prov.datadir_root, layout.datadir);
    assert_eq!(
        prov.source_path,
        anolisa_platform::fs_layout::FsLayout::component_contract_path(&layout.datadir, "sec-core"),
    );
}
