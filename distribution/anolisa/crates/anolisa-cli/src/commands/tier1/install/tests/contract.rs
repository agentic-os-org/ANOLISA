//! contract tests for the  command.

use super::super::tests::*;

use crate::commands::common;
use tempfile::tempdir;

#[test]
fn distribution_registry_contracts_parse_with_complete_layouts() {
    let registry =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manifests/components");
    for component in [
        "cosh-ng",
        "sec-core",
        "agentsight",
        "tokenless",
        "skillfs",
        "ws-ckpt",
        "agent-memory",
    ] {
        let manifest = anolisa_core::ComponentManifest::from_file(
            &registry.join(component).join("component.toml"),
        )
        .expect("distribution contract must parse");
        assert_eq!(manifest.component.name, component);
        assert!(!manifest.install.files.is_empty(), "{component} layout");
        for adapter in &manifest.adapters {
            let rpm = adapter.backends.rpm.as_ref().expect("RPM adapter metadata");
            assert!(
                !rpm.managed_packages.is_empty(),
                "{component} package inventory"
            );
        }
    }
}

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

#[test]
fn refresh_and_inspect_use_the_packaged_distribution_registry() {
    use anolisa_core::adapter::contract::{infer_contract_datadir_root, read_snapshot_provenance};

    let (tmp, ctx) = system_ctx_with_raw_repo(false);
    let layout = common::resolve_layout(&ctx);
    let packaged = tmp.path().join("distribution/share/anolisa");
    let source =
        anolisa_platform::fs_layout::FsLayout::component_contract_path(&packaged, "sec-core");
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir registry");
    let contract = component_manifest_toml("sec-core", "1.1.0", &["system"]);
    std::fs::write(&source, &contract).expect("write distribution contract");
    let ctx = ctx.with_packaged_data_root(packaged.clone());

    let before =
        inspect_datadir_contract_drift(&layout, "sec-core", COMMAND, ctx.packaged_data_probe());
    assert!(before.drifted, "missing snapshot must be refreshed");
    let outcome =
        refresh_datadir_contract_snapshot(&layout, "sec-core", COMMAND, ctx.packaged_data_probe());
    assert!(outcome.failure_detail().is_none(), "{:?}", outcome.warnings);
    assert!(outcome.warnings.is_empty());
    let snapshot = common::installed_component_manifest_path(&layout, "sec-core", COMMAND)
        .expect("snapshot path");
    assert_eq!(
        std::fs::read_to_string(&snapshot).expect("snapshot"),
        contract
    );
    let provenance = read_snapshot_provenance(&snapshot).expect("provenance");
    assert_eq!(provenance.source_path, source);
    assert_eq!(provenance.datadir_root, packaged);
    assert_eq!(
        infer_contract_datadir_root("sec-core", &snapshot, std::slice::from_ref(&packaged)),
        Some(packaged.clone())
    );
    assert_eq!(
        infer_contract_datadir_root("sec-core", &snapshot, std::slice::from_ref(&layout.datadir)),
        None
    );
    let after =
        inspect_datadir_contract_drift(&layout, "sec-core", COMMAND, ctx.packaged_data_probe());
    assert!(!after.drifted);
    assert!(after.warnings.is_empty());
}

#[test]
fn unreadable_distribution_contract_does_not_fall_through_to_another_root() {
    let (_tmp, ctx) = system_ctx_with_raw_repo(false);
    let layout = common::resolve_layout(&ctx);
    let package_datadir = layout.package_datadir().expect("package datadir");
    let blocked = anolisa_platform::fs_layout::FsLayout::component_contract_path(
        &package_datadir,
        "sec-core",
    );
    std::fs::create_dir_all(&blocked).expect("directory cannot be read as contract");
    let contract = component_manifest_toml("sec-core", "1.1.0", &["system"]);
    seed_datadir_contract(&layout, "sec-core", &contract);

    let result =
        refresh_datadir_contract_snapshot(&layout, "sec-core", COMMAND, ctx.packaged_data_probe());
    assert!(result.error_detail().is_some());
    let snapshot = common::installed_component_manifest_path(&layout, "sec-core", COMMAND)
        .expect("snapshot path");
    assert!(
        !snapshot.exists(),
        "an unreadable authority must not be masked"
    );
}
