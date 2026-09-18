//! Unit tests for `update --check`. Driven entirely through the injected
//! [`PackageQuery`] fake plus in-memory state, so no live rpmdb/dnf is required.

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};

use anolisa_platform::pkg_query::{PackageInfo, PackageVersion};
use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};

use anolisa_core::domain::InstallationScope;
use anolisa_core::state::{
    InstalledObject, ObjectStatus, Ownership, RpmMetadata, SubscriptionScope,
};
use anolisa_core::state_migration::migrate_state;
use anolisa_core::state_store::StateStore;

use super::super::UpdateArgs;
use super::render::build_motd;

/// In-memory host implementing both [`PackageQuery`] and [`PackageTransaction`]:
/// the check must only ever call the query side, so any transaction call is a
/// routing bug the counter surfaces.
#[derive(Default)]
struct FakeHost {
    /// exe/capability path → owning package names.
    provides: HashMap<String, Vec<String>>,
    /// package → installed EVR info.
    installed: HashMap<String, PackageInfo>,
    /// package → repo candidate infos.
    available: HashMap<String, Vec<PackageInfo>>,
    /// packages whose `query_available` returns an error.
    available_errors: HashSet<String>,
    /// packages whose `query_installed` reports a multi-version drift.
    installed_multi: HashSet<String>,
    /// packages whose installed query cannot start rpm.
    installed_command_missing: HashSet<String>,
    /// packages whose installed query reports a hard rpmdb failure.
    installed_errors: HashSet<String>,
    installed_queries: RefCell<Vec<String>>,
    /// capabilities whose `what_provides_installed` returns a query error.
    provides_errors: HashSet<String>,
    /// capability → available repo provider package names.
    available_providers: HashMap<String, Vec<String>>,
    txn_calls: Cell<usize>,
}

impl FakeHost {
    fn with_cli_noop() -> Self {
        // A CLI that is RPM-owned and already current, so CLI status never
        // perturbs component-focused assertions.
        let mut host = FakeHost::default();
        host.provides
            .insert("/usr/bin/anolisa".to_string(), vec!["anolisa".to_string()]);
        host.installed.insert(
            "anolisa".to_string(),
            info("anolisa", "1.0.0", Some("1.al4")),
        );
        host
    }
}

impl PackageQuery for FakeHost {
    fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
        self.installed_queries
            .borrow_mut()
            .push(package.to_string());
        if self.installed_multi.contains(package) {
            return Err(PackageQueryError::UnexpectedOutput {
                command: "rpm".to_string(),
                detail: "2 installed versions".to_string(),
            });
        }
        if self.installed_command_missing.contains(package) {
            return Err(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            });
        }
        if self.installed_errors.contains(package) {
            return Err(PackageQueryError::QueryFailed {
                command: "rpm".to_string(),
                code: Some(1),
                stderr: "rpmdb unavailable".to_string(),
            });
        }
        Ok(self.installed.get(package).cloned())
    }

    fn query_available(&self, package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
        if self.available_errors.contains(package) {
            return Err(PackageQueryError::QueryFailed {
                command: "dnf".to_string(),
                code: Some(1),
                stderr: "repo unreachable".to_string(),
            });
        }
        Ok(self.available.get(package).cloned().unwrap_or_default())
    }

    fn what_provides_installed(&self, capability: &str) -> Result<Vec<String>, PackageQueryError> {
        if self.provides_errors.contains(capability) {
            return Err(PackageQueryError::QueryFailed {
                command: "rpm".to_string(),
                code: Some(1),
                stderr: "rpmdb query failed".to_string(),
            });
        }
        Ok(self.provides.get(capability).cloned().unwrap_or_default())
    }

    fn what_provides_available(&self, capability: &str) -> Result<Vec<String>, PackageQueryError> {
        Ok(self
            .available_providers
            .get(capability)
            .cloned()
            .unwrap_or_default())
    }
}

impl PackageTransaction for FakeHost {
    fn check_install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        panic!("this path must not preflight an install")
    }

    fn install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.txn_calls.set(self.txn_calls.get() + 1);
        Ok(())
    }
    fn update(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.txn_calls.set(self.txn_calls.get() + 1);
        Ok(())
    }
    fn reinstall(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.txn_calls.set(self.txn_calls.get() + 1);
        Ok(())
    }
    fn remove(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.txn_calls.set(self.txn_calls.get() + 1);
        Ok(())
    }
}

fn info(name: &str, version: &str, release: Option<&str>) -> PackageInfo {
    PackageInfo {
        name: name.to_string(),
        version: PackageVersion {
            epoch: None,
            version: version.to_string(),
            release: release.map(str::to_string),
        },
        arch: "x86_64".to_string(),
        origin: None,
    }
}

fn rpm_backend_with_package_map(
    component: &str,
    package: &str,
) -> crate::repo_config::BackendConfig {
    let mut package_map = BTreeMap::new();
    package_map.insert(component.to_string(), package.to_string());
    crate::repo_config::BackendConfig {
        base_url: "https://example.com/rpm/".to_string(),
        insecure: false,
        gpgcheck: None,
        scope: None,
        cache_ttl_secs: None,
        offline_fallback: None,
        package_map,
    }
}

fn rpm_component(
    component: &str,
    package: &str,
    evr: &str,
    ownership: Ownership,
) -> InstalledObject {
    InstalledObject {
        kind: ObjectKind::Component,
        name: component.to_string(),
        version: evr.to_string(),
        status: ObjectStatus::Installed,
        manifest_digest: None,
        distribution_source: None,
        raw_package: None,
        install_backend: Some("rpm".to_string()),
        ownership: Some(ownership),
        rpm_metadata: Some(RpmMetadata {
            package_name: package.to_string(),
            evr: Some(evr.to_string()),
            arch: Some("x86_64".to_string()),
            source_repo: Some("@System".to_string()),
        }),
        installed_at: "2026-06-01T10:00:00Z".to_string(),
        last_operation_id: None,
        managed: !matches!(ownership, Ownership::RpmObserved),
        adopted: matches!(ownership, Ownership::RpmObserved),
        subscription_scope: SubscriptionScope::None,
        enabled_features: Vec::new(),
        component_refs: Vec::new(),
        files: Vec::new(),
        external_modified_files: Vec::new(),
        services: Vec::new(),
        health: Vec::new(),
        provisioned_packages: Vec::new(),
    }
}

fn raw_component(component: &str, version: &str) -> InstalledObject {
    InstalledObject {
        kind: ObjectKind::Component,
        name: component.to_string(),
        version: version.to_string(),
        status: ObjectStatus::Installed,
        manifest_digest: None,
        distribution_source: Some("https://example.com/x".to_string()),
        raw_package: None,
        install_backend: Some("raw".to_string()),
        ownership: Some(Ownership::RawManaged),
        rpm_metadata: None,
        installed_at: "2026-06-01T10:00:00Z".to_string(),
        last_operation_id: None,
        managed: true,
        adopted: false,
        subscription_scope: SubscriptionScope::None,
        enabled_features: Vec::new(),
        component_refs: Vec::new(),
        files: Vec::new(),
        external_modified_files: Vec::new(),
        services: Vec::new(),
        health: Vec::new(),
        provisioned_packages: Vec::new(),
    }
}

/// Build a v5 store from legacy fixtures via the load-boundary migration, so
/// these tests double as migration coverage for the check's inputs.
fn state_with(objects: Vec<InstalledObject>) -> StateStore {
    let migration = migrate_state(&objects, InstallationScope::System);
    assert!(
        migration.quarantined.is_empty(),
        "check fixtures must migrate cleanly; quarantined: {:?}",
        migration.quarantined
    );
    let mut store = StateStore::empty();
    store.installations = migration.active;
    store
}

fn run(
    host: &FakeHost,
    installed: &StateStore,
    target: Option<TargetProfile>,
    target_name: Option<String>,
) -> UpdateCheckReport {
    run_with_index(host, installed, target, target_name, None)
}

fn run_with_index(
    host: &FakeHost,
    installed: &StateStore,
    target: Option<TargetProfile>,
    target_name: Option<String>,
    component_index: Option<&crate::resolution::ComponentIndex>,
) -> UpdateCheckReport {
    run_with_index_and_backend(host, installed, target, target_name, component_index, None)
}

fn run_with_index_and_backend(
    host: &FakeHost,
    installed: &StateStore,
    target: Option<TargetProfile>,
    target_name: Option<String>,
    component_index: Option<&crate::resolution::ComponentIndex>,
    rpm_backend: Option<&crate::repo_config::BackendConfig>,
) -> UpdateCheckReport {
    let ctx = system_ctx();
    let view = StateView::load_with_writable_state(
        &ctx,
        CHECK_COMMAND,
        StateVisibility::UserPlusSystem,
        installed.clone(),
    )
    .expect("test state view");
    run_update_check(CheckInputs {
        view: &view,
        query: host,
        cli_exe_path: "/usr/bin/anolisa",
        arch: "x86_64",
        target_name,
        target,
        component_index,
        rpm_backend,
    })
    .expect("update check report")
}

/// Identity-only component index naming `components`, so profile defaults
/// pass identity validation while package resolution still flows through
/// `package_map` and RPM `Provides`.
fn identity_index(components: &[&str]) -> crate::resolution::ComponentIndex {
    let mut toml = String::from("schema_version = 2\n");
    for component in components {
        toml.push_str(&format!(
            "\n[[components]]\nname = \"{component}\"\ntargets = [{{ os = \"linux\", arch = \"x86_64\" }}]\n"
        ));
    }
    crate::resolution::ComponentIndex::from_toml_str(&toml, "test-components.toml")
        .expect("identity index parses")
}

fn system_ctx() -> CliContext {
    crate::test_support::context_for_root(
        std::path::Path::new("/tmp/anolisa-update-check-validation"),
        crate::context::InstallMode::System,
        None,
        Default::default(),
    )
}

// ── CLI parse surface ───────────────────────────────────────────────

use clap::Parser;

#[test]
fn update_check_parse_accepts_check_flag() {
    let args = UpdateArgs::try_parse_from(["update", "--check"]).expect("parse");
    assert!(args.check);
    assert!(args.component.is_none());
    assert!(args.command.is_none());
}

#[test]
fn update_check_rejects_self_target() {
    // With `--check` present, clap binds `self` to the positional rather than
    // dispatching the subcommand, so the rejection lands in `handle`.
    let args = UpdateArgs::try_parse_from(["update", "--check", "self"]).expect("parse");
    assert!(args.check);
    let err =
        super::super::handle(args, &system_ctx()).expect_err("`--check self` must be rejected");
    assert_eq!(err.code(), "INVALID_ARGUMENT");
}

#[test]
fn update_check_parse_motd_requires_check() {
    UpdateArgs::try_parse_from(["update", "--motd"])
        .expect_err("--motd is only valid together with --check");
}

#[test]
fn update_check_parse_target_requires_check() {
    UpdateArgs::try_parse_from(["update", "--target", "image-v1.0"])
        .expect_err("--target is only valid together with --check");
}

#[test]
fn update_check_rejects_component_argument() {
    let args = UpdateArgs {
        component: Some("cosh".to_string()),
        command: None,
        check: true,
        motd: false,
        refresh: false,
        target: None,
    };
    let err = super::super::handle(args, &system_ctx())
        .expect_err("component + --check must be rejected");
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(err.reason().contains("no component argument"));
}

// ── report shape and detection ──────────────────────────────────────

#[test]
fn update_check_json_shape_has_cli_components_summary() {
    let host = FakeHost::with_cli_noop();
    let state = state_with(vec![]);
    let report = run(&host, &state, None, None);
    let value = serde_json::to_value(&report).expect("serialize");
    assert!(value.get("cli").is_some(), "cli field present");
    assert!(
        value.get("components").is_some(),
        "components field present"
    );
    assert!(value.get("summary").is_some(), "summary field present");
    assert_eq!(value["backend"], "rpm");
    // A current CLI and no components → nothing to do.
    assert_eq!(value["upgrade_available"], false);
    assert_eq!(value["action_required"], false);
}

#[test]
fn update_check_rpm_component_update_candidate() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "1.0.0", Some("1.al4")),
    );
    host.available.insert(
        "copilot-shell".to_string(),
        vec![info("copilot-shell", "1.1.0", Some("1.al4"))],
    );
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.0.0-1.al4",
        Ownership::RpmObserved,
    )]);

    let report = run(&host, &state, None, None);
    let item = report
        .components
        .iter()
        .find(|c| c.component == "cosh")
        .expect("component present");
    assert_eq!(item.action, ACTION_UPDATE);
    assert_eq!(item.installed.as_deref(), Some("1.0.0-1.al4"));
    assert_eq!(item.available.as_deref(), Some("1.1.0-1.al4"));
    assert_eq!(item.ownership.as_deref(), Some("adopted"));
    assert_eq!(report.summary.updates, 1);
    assert!(report.upgrade_available);
    assert!(report.action_required);
    assert_eq!(
        host.txn_calls.get(),
        0,
        "check must never run a transaction"
    );
}

/// Regression for the P1 semver bug: real RPM EVRs that semver cannot parse
/// (two-segment version) must still be detected as upgradable.
#[test]
fn update_check_detects_non_semver_rpm_upgrade() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "0.5", Some("1.al4")),
    );
    host.available.insert(
        "copilot-shell".to_string(),
        vec![info("copilot-shell", "1.0.0", Some("1.al4"))],
    );
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "0.5-1.al4",
        Ownership::RpmObserved,
    )]);

    let report = run(&host, &state, None, None);
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_UPDATE);
    assert_eq!(item.available.as_deref(), Some("1.0.0-1.al4"));
    assert_eq!(report.summary.updates, 1);
}

#[test]
fn update_check_rpm_component_up_to_date_is_noop() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    // Repo only offers the same version.
    host.available.insert(
        "copilot-shell".to_string(),
        vec![info("copilot-shell", "1.1.0", Some("1.al4"))],
    );
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.1.0-1.al4",
        Ownership::RpmManaged,
    )]);

    let report = run(&host, &state, None, None);
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_NOOP);
    assert_eq!(report.summary.updates, 0);
}

#[test]
fn update_check_raw_component_is_unsupported() {
    let host = FakeHost::with_cli_noop();
    let state = state_with(vec![raw_component("tokenless", "0.5.0")]);

    let report = run(&host, &state, None, None);
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_UNSUPPORTED_RPM);
    assert_eq!(item.ownership.as_deref(), Some("owned"));
    assert_eq!(report.summary.unsupported, 1);
    assert_eq!(report.summary.updates, 0);
    assert_eq!(
        host.txn_calls.get(),
        0,
        "raw component must not run a transaction"
    );
    assert!(
        !host
            .installed_queries
            .borrow()
            .iter()
            .any(|package| package == "tokenless"),
        "owned component must not request native package evidence"
    );
}

#[test]
fn update_check_missing_default_reports_install() {
    let mut host = FakeHost::with_cli_noop();
    host.available_providers.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string()],
    );
    host.available_providers.insert(
        "anolisa-component(sec-core)".to_string(),
        vec!["agent-sec-core".to_string()],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string(), "sec-core".to_string()],
    };

    let index = identity_index(&["cosh", "sec-core"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some("image-v1.0".to_string()),
        Some(&index),
    );
    assert_eq!(report.target.as_deref(), Some("image-v1.0"));
    assert_eq!(report.summary.missing_defaults, 2);
    assert!(
        report.components.iter().all(|c| c.action == ACTION_INSTALL),
        "absent defaults must be reported as installable"
    );
    // Installs are not "upgrades", but they still require action.
    assert!(!report.upgrade_available);
    assert!(report.action_required);
}

#[test]
fn update_check_present_default_is_not_reported() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "1.0.0", Some("1.al4")),
    );
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.0.0-1.al4",
        Ownership::RpmManaged,
    )]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let report = run(&host, &state, Some(profile), Some("image-v1.0".to_string()));
    assert_eq!(report.summary.missing_defaults, 0);
}

/// A default absent from ANOLISA state but installed on the host (declaring the
/// `anolisa-component(...)` provide) must not be reported as a missing install;
/// it is evaluated for upgrades instead.
#[test]
fn update_check_default_present_via_provide_is_not_missing() {
    let mut host = FakeHost::with_cli_noop();
    host.provides.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string()],
    );
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "2.6.1", Some("1")),
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );
    let item = report
        .components
        .iter()
        .find(|c| c.component == "cosh")
        .expect("cosh reported");
    assert_eq!(item.action, ACTION_NOOP);
    assert_eq!(item.ownership.as_deref(), Some("observed"));
    assert_eq!(
        report.summary.missing_defaults, 0,
        "a present-but-unadopted default is not missing"
    );
}

/// A present-but-unadopted default with a newer repo candidate is reported as an
/// upgrade, not an install.
#[test]
fn update_check_default_present_via_provide_reports_upgrade() {
    let mut host = FakeHost::with_cli_noop();
    host.provides.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string()],
    );
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "2.6.1", Some("1")),
    );
    host.available.insert(
        "copilot-shell".to_string(),
        vec![info("copilot-shell", "2.7.0", Some("1"))],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_UPDATE);
    assert_eq!(item.available.as_deref(), Some("2.7.0-1"));
    assert_eq!(report.summary.missing_defaults, 0);
    assert_eq!(report.summary.updates, 1);
}

/// A legacy default installed under its package name but lacking the
/// `anolisa-component(...)` provide is still recognised via the component index
/// mapping, so it is not falsely reported as missing.
#[test]
fn update_check_default_present_via_index_package_without_provide() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "2.6.1", Some("1")),
    );
    let index = crate::resolution::ComponentIndex::from_toml_str(
        "schema_version = 2\n\n[[components]]\nname = \"cosh\"\ntargets = [{ os = \"linux\", arch = \"x86_64\" }]\n\n[[components.backends]]\nkind = \"rpm\"\npackage = \"copilot-shell\"\n",
        "test-components.toml",
    )
    .expect("index parses");
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_NOOP);
    assert_eq!(item.ownership.as_deref(), Some("observed"));
    assert_eq!(report.summary.missing_defaults, 0);
}

#[test]
fn update_check_default_present_via_package_map_without_provide() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "2.6.1", Some("1")),
    );
    let backend = rpm_backend_with_package_map("cosh", "copilot-shell");
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index_and_backend(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
        Some(&backend),
    );

    let item = &report.components[0];
    assert_eq!(item.action, ACTION_NOOP);
    assert_eq!(item.package.as_deref(), Some("copilot-shell"));
    assert_eq!(item.ownership.as_deref(), Some("observed"));
    assert_eq!(report.summary.missing_defaults, 0);
}

/// A default absent from both ANOLISA state and rpmdb is still reported as an
/// install (the pre-fix behaviour for genuinely missing defaults).
#[test]
fn update_check_unresolved_missing_default_is_item_error() {
    let host = FakeHost::with_cli_noop();
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(
        item.error
            .as_deref()
            .unwrap_or("")
            .contains("cannot resolve")
    );
    assert_eq!(report.summary.missing_defaults, 0);
    assert_eq!(report.summary.errors, 1);
}

#[test]
fn update_check_missing_default_resolves_package_from_package_map() {
    let host = FakeHost::with_cli_noop();
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };
    let backend = rpm_backend_with_package_map("cosh", "copilot-shell");

    let index = identity_index(&["cosh"]);
    let report = run_with_index_and_backend(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
        Some(&backend),
    );

    let item = &report.components[0];
    assert_eq!(item.action, ACTION_INSTALL);
    assert_eq!(item.package.as_deref(), Some("copilot-shell"));
    assert_eq!(report.summary.missing_defaults, 1);
}

#[test]
fn update_check_missing_default_resolves_package_from_available_provide() {
    let mut host = FakeHost::with_cli_noop();
    host.available_providers.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string()],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );

    let item = &report.components[0];
    assert_eq!(item.action, ACTION_INSTALL);
    assert_eq!(item.package.as_deref(), Some("copilot-shell"));
    assert_eq!(report.summary.missing_defaults, 1);
}

/// A profile default the index does not know is rejected even when a
/// site-local `package_map` entry could name a package for it: package
/// selection must not create a component identity (issue #2630).
#[test]
fn update_check_default_absent_from_index_is_unsupported_despite_package_map() {
    let host = FakeHost::with_cli_noop();
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["site-only".to_string()],
    };
    let backend = rpm_backend_with_package_map("site-only", "site-package");

    let index = identity_index(&["cosh"]);
    let report = run_with_index_and_backend(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
        Some(&backend),
    );

    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(
        item.error
            .as_deref()
            .unwrap_or("")
            .contains("unsupported component 'site-only'"),
        "package_map must not authorize the identity: {:?}",
        item.error
    );
    assert_eq!(report.summary.missing_defaults, 0);
    assert_eq!(report.summary.errors, 1);
}

/// A profile default the index does not know is rejected even when an RPM
/// repository provider declares `anolisa-component(...)` for it: Provides
/// resolves packages, never identities (issue #2630).
#[test]
fn update_check_default_absent_from_index_is_unsupported_despite_provides() {
    let mut host = FakeHost::with_cli_noop();
    host.available_providers.insert(
        "anolisa-component(site-only)".to_string(),
        vec!["site-package".to_string()],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["site-only".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );

    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(
        item.error
            .as_deref()
            .unwrap_or("")
            .contains("unsupported component 'site-only'"),
        "an RPM provider must not authorize the identity: {:?}",
        item.error
    );
    assert_eq!(report.summary.missing_defaults, 0);
    assert_eq!(report.summary.errors, 1);
}

/// Without any component index a profile default absent from state cannot be
/// validated at all — even a `package_map` entry plus a matching provider
/// yields the explicit index-unavailable item error, never an install item.
#[test]
fn update_check_default_without_index_is_unvalidated() {
    let mut host = FakeHost::with_cli_noop();
    host.available_providers.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string()],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };
    let backend = rpm_backend_with_package_map("cosh", "copilot-shell");

    let report = run_with_index_and_backend(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        None,
        Some(&backend),
    );

    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(
        item.error
            .as_deref()
            .unwrap_or("")
            .contains("component index is unavailable"),
        "an unvalidatable default must name the missing index: {:?}",
        item.error
    );
    assert_eq!(report.summary.missing_defaults, 0);
    assert_eq!(report.summary.errors, 1);
}

/// Index naming `cosh` with `copilot-shell` declared as its rpm-package
/// alias, for the alias-default tests below.
fn alias_index() -> crate::resolution::ComponentIndex {
    crate::resolution::ComponentIndex::from_toml_str(
        "schema_version = 2\n\n[[components]]\nname = \"cosh\"\ntargets = [{ os = \"linux\", arch = \"x86_64\" }]\n\n[[components.aliases]]\nkind = \"rpm-package\"\nname = \"copilot-shell\"\n",
        "test-components.toml",
    )
    .expect("alias index parses")
}

/// An alias profile default whose canonical component is already tracked in
/// state must not be re-reported as absent (issue #2630): identity
/// resolution and the state comparison both run on the canonical name, for
/// RPM-backed and raw-backed records alike.
#[test]
fn update_check_alias_default_with_installed_canonical_is_not_reported() {
    for object in [
        rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        ),
        raw_component("cosh", "1.0.0"),
    ] {
        let mut host = FakeHost::with_cli_noop();
        host.installed.insert(
            "copilot-shell".to_string(),
            info("copilot-shell", "1.0.0", Some("1.al4")),
        );
        let state = state_with(vec![object]);
        let profile = TargetProfile {
            default_components: vec!["copilot-shell".to_string()],
        };

        let index = alias_index();
        let report = run_with_index(
            &host,
            &state,
            Some(profile),
            Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
            Some(&index),
        );

        assert_eq!(report.summary.missing_defaults, 0);
        assert!(
            report.components.iter().all(|c| !c.absent_from_state),
            "the alias default must dedupe against the tracked canonical record: {:?}",
            report
                .components
                .iter()
                .map(|c| (&c.component, &c.action, c.absent_from_state))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            report
                .components
                .iter()
                .filter(|c| c.component == "cosh")
                .count(),
            1,
            "the tracked component is reported exactly once"
        );
    }
}

/// Defaults spelling the same component canonically and via an alias
/// evaluate once, under the canonical name.
#[test]
fn update_check_duplicate_alias_defaults_evaluate_once_as_canonical() {
    let mut host = FakeHost::with_cli_noop();
    host.available_providers.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string()],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["copilot-shell".to_string(), "cosh".to_string()],
    };

    let index = alias_index();
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );

    assert_eq!(report.summary.missing_defaults, 1);
    assert_eq!(
        report.components.len(),
        1,
        "one evaluation for one identity"
    );
    let item = &report.components[0];
    assert_eq!(item.component, "cosh", "reported under the canonical name");
    assert_eq!(item.action, ACTION_INSTALL);
}

/// A default already tracked in state under its exact (legacy) name is not
/// re-normalized into a second identity: the installed loop above owns that
/// record, so the default neither duplicates it under the canonical name nor
/// — with no index at all — produces a spurious validation error.
#[test]
fn update_check_exact_legacy_default_is_not_renormalized() {
    let index = alias_index();
    for index in [Some(&index), None] {
        let host = FakeHost::with_cli_noop();
        // "copilot-shell" is the record's exact name and, in the index, an
        // alias of "cosh".
        let state = state_with(vec![raw_component("copilot-shell", "1.0.0")]);
        let profile = TargetProfile {
            default_components: vec!["copilot-shell".to_string()],
        };

        let report = run_with_index(
            &host,
            &state,
            Some(profile),
            Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
            index,
        );

        assert_eq!(report.summary.missing_defaults, 0);
        assert_eq!(
            report.summary.errors,
            0,
            "the exact record must preempt index validation: {:?}",
            report
                .components
                .iter()
                .map(|c| (&c.component, &c.action, &c.error))
                .collect::<Vec<_>>()
        );
        assert!(
            report
                .components
                .iter()
                .all(|c| !c.absent_from_state && c.component != "cosh"),
            "no second identity may appear: {:?}",
            report
                .components
                .iter()
                .map(|c| (&c.component, &c.action, c.absent_from_state))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn update_check_ambiguous_missing_default_is_item_error() {
    let mut host = FakeHost::with_cli_noop();
    host.available_providers.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string(), "cosh-alt".to_string()],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );

    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(item.error.as_deref().unwrap_or("").contains("multiple"));
    assert_eq!(report.summary.missing_defaults, 0);
    assert_eq!(report.summary.errors, 1);
}

/// A failed rpmdb provide query for a default must not be reported as an
/// installable missing default — "cannot determine" is an item error.
#[test]
fn update_check_default_provide_query_error_is_item_error() {
    let mut host = FakeHost::with_cli_noop();
    host.provides_errors
        .insert("anolisa-component(cosh)".to_string());
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(
        item.error
            .as_deref()
            .unwrap_or("")
            .contains("cannot determine"),
        "the probe failure must be the reported error: {:?}",
        item.error
    );
    assert_eq!(
        report.summary.missing_defaults, 0,
        "an indeterminate probe must not count as a missing default"
    );
    assert_eq!(report.summary.errors, 1);
}

/// Multiple installed providers for a default's component capability is
/// ambiguous and must be an item error, not a silently-picked first package.
#[test]
fn update_check_default_multiple_providers_is_item_error() {
    let mut host = FakeHost::with_cli_noop();
    host.provides.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string(), "copilot-shell-ng".to_string()],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let index = identity_index(&["cosh"]);
    let report = run_with_index(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
        Some(&index),
    );
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(
        item.error.as_deref().unwrap().contains("multiple"),
        "error should explain the ambiguity"
    );
    assert_eq!(report.summary.missing_defaults, 0);
    assert_eq!(report.summary.errors, 1);
    assert_eq!(
        host.txn_calls.get(),
        0,
        "an ambiguous default must not run a transaction"
    );
}

/// If the resolved provider package is absent from rpmdb (a mid-check race), the
/// default is an item error, not a missing install.
#[test]
fn update_check_default_resolved_package_absent_is_item_error() {
    let mut host = FakeHost::with_cli_noop();
    // A provider is declared, but the package itself is not in the installed
    // map, so `query_installed` returns `Ok(None)`.
    host.provides.insert(
        "anolisa-component(cosh)".to_string(),
        vec!["copilot-shell".to_string()],
    );
    let state = state_with(vec![]);
    let profile = TargetProfile {
        default_components: vec!["cosh".to_string()],
    };

    let report = run(
        &host,
        &state,
        Some(profile),
        Some(DEFAULT_TARGET_PROFILE_NAME.to_string()),
    );
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert_eq!(report.summary.missing_defaults, 0);
    assert_eq!(report.summary.errors, 1);
}

/// The index mapping is de-duplicated, so a backend and an alias sharing a name
/// are queried only once.
#[test]
fn update_check_index_rpm_packages_are_deduplicated() {
    let index = crate::resolution::ComponentIndex::from_toml_str(
        "schema_version = 2\n\n[[components]]\nname = \"cosh\"\ntargets = [{ os = \"linux\", arch = \"x86_64\" }]\n\n[[components.backends]]\nkind = \"rpm\"\npackage = \"copilot-shell\"\n\n[[components.aliases]]\nkind = \"rpm-package\"\nname = \"copilot-shell\"\n",
        "test-components.toml",
    )
    .expect("index parses");
    assert_eq!(
        index_rpm_packages(Some(&index), "cosh"),
        vec!["copilot-shell".to_string()],
    );
}

#[test]
fn update_check_repo_query_failure_is_item_error() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "1.0.0", Some("1.al4")),
    );
    host.available_errors.insert("copilot-shell".to_string());
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.0.0-1.al4",
        Ownership::RpmObserved,
    )]);

    let report = run(&host, &state, None, None);
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(item.error.is_some());
    assert_eq!(report.summary.errors, 1);
}

#[test]
fn update_check_resolves_legacy_rpm_component_without_metadata() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "2.7.0", Some("1.alnx4")),
    );
    let backend = rpm_backend_with_package_map("cosh", "copilot-shell");
    let mut component = rpm_component(
        "cosh",
        "copilot-shell",
        "2.6.1-1.alnx4",
        Ownership::RpmManaged,
    );
    component.rpm_metadata = None;
    let state = state_with(vec![component]);

    let report = run_with_index_and_backend(&host, &state, None, None, None, Some(&backend));

    let item = &report.components[0];
    assert_eq!(item.action, ACTION_RECONCILE);
    assert_eq!(item.package.as_deref(), Some("copilot-shell"));
    assert_eq!(item.installed.as_deref(), Some("2.7.0-1.alnx4"));
    assert!(item.backfill_rpm_metadata);
    let json = serde_json::to_value(&report).expect("serialize report");
    assert_eq!(json["components"][0]["action"], ACTION_RECONCILE);
    assert!(json["components"][0]["backfill_rpm_metadata"].is_null());
    assert_eq!(json["summary"]["reconciliations"], 1);
    assert!(report.action_required);
    assert_eq!(report.summary.reconciliations, 1);
    assert_eq!(report.summary.errors, 0);

    let motd = build_motd(&report).expect("reconciliation appears in MOTD");
    assert!(motd.contains("ANOLISA state reconciliation is required."));
    assert!(motd.contains("1 component requires state reconciliation"));

    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let path = cache_path(&layout);
    write_cache(&path, &report, cache_now).expect("write cache");
    let cached = read_cache(&path).expect("read cache");
    assert!(cached.report.action_required);
    assert_eq!(cached.report.summary.reconciliations, 1);
    assert_eq!(cached.report.components[0].action, ACTION_RECONCILE);
    assert!(build_motd(&cached.report).is_some());
}

#[test]
fn update_check_component_missing_from_rpmdb_is_item_error() {
    let host = FakeHost::with_cli_noop();
    // No installed entry for the package → rpmdb miss.
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.0.0-1.al4",
        Ownership::RpmObserved,
    )]);

    let report = run(&host, &state, None, None);
    let item = &report.components[0];
    assert_eq!(item.action, ACTION_ERROR);
    assert!(item.error.as_deref().unwrap().contains("forget"));
    assert_eq!(report.summary.errors, 1);
}

#[test]
fn update_check_component_native_probe_runs_once() {
    let mut host = FakeHost::with_cli_noop();
    host.installed.insert(
        "copilot-shell".to_string(),
        info("copilot-shell", "1.0.0", Some("1.al4")),
    );
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.0.0-1.al4",
        Ownership::RpmObserved,
    )]);

    let report = run(&host, &state, None, None);

    assert_eq!(report.components[0].action, ACTION_NOOP);
    assert_eq!(
        host.installed_queries
            .borrow()
            .iter()
            .filter(|package| package.as_str() == "copilot-shell")
            .count(),
        1
    );
}

#[test]
fn update_check_component_multiple_versions_remains_item_error() {
    let mut host = FakeHost::with_cli_noop();
    host.installed_multi.insert("copilot-shell".to_string());
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.0.0-1.al4",
        Ownership::RpmObserved,
    )]);

    let report = run(&host, &state, None, None);
    let item = &report.components[0];

    assert_eq!(item.action, ACTION_ERROR);
    assert_eq!(
        item.error.as_deref(),
        Some("rpmdb reports multiple installed versions for this package")
    );
    assert_eq!(report.summary.errors, 1);
}

#[test]
fn update_check_component_missing_rpm_command_keeps_existing_error() {
    let mut host = FakeHost::with_cli_noop();
    host.installed_command_missing
        .insert("copilot-shell".to_string());
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.0.0-1.al4",
        Ownership::RpmObserved,
    )]);

    let report = run(&host, &state, None, None);
    let item = &report.components[0];

    assert_eq!(item.action, ACTION_ERROR);
    assert_eq!(
        item.error.as_deref(),
        Some("rpm/dnf not found; cannot query the installed version")
    );
    assert_eq!(report.summary.errors, 1);
}

#[test]
fn update_check_component_rpm_failure_keeps_source_detail() {
    let mut host = FakeHost::with_cli_noop();
    host.installed_errors.insert("copilot-shell".to_string());
    let state = state_with(vec![rpm_component(
        "cosh",
        "copilot-shell",
        "1.0.0-1.al4",
        Ownership::RpmObserved,
    )]);

    let report = run(&host, &state, None, None);
    let item = &report.components[0];

    assert_eq!(item.action, ACTION_ERROR);
    assert_eq!(
        item.error.as_deref(),
        Some("rpm query failed: rpm failed (code Some(1)): rpmdb unavailable")
    );
    assert_eq!(report.summary.errors, 1);
}

#[test]
fn update_check_cli_update_available() {
    let mut host = FakeHost::default();
    host.provides
        .insert("/usr/bin/anolisa".to_string(), vec!["anolisa".to_string()]);
    host.installed.insert(
        "anolisa".to_string(),
        info("anolisa", "0.5.0", Some("1.al4")),
    );
    host.available.insert(
        "anolisa".to_string(),
        vec![info("anolisa", "1.0.0", Some("1.al4"))],
    );
    let state = state_with(vec![]);

    let report = run(&host, &state, None, None);
    assert_eq!(report.cli.action, ACTION_UPDATE);
    assert_eq!(report.cli.package.as_deref(), Some("anolisa"));
    assert_eq!(report.cli.installed.as_deref(), Some("0.5.0-1.al4"));
    assert_eq!(report.cli.available.as_deref(), Some("1.0.0-1.al4"));
    assert_eq!(report.summary.updates, 1);
    assert!(report.upgrade_available);
}

#[test]
fn update_check_cli_not_rpm_owned_is_unsupported() {
    // No provider for the exe path → not RPM-owned.
    let host = FakeHost::default();
    let state = state_with(vec![]);
    let report = run(&host, &state, None, None);
    assert_eq!(report.cli.action, ACTION_UNSUPPORTED);
    assert!(report.cli.package.is_none());
    assert_eq!(report.summary.unsupported, 1);
}

// ── MOTD rendering ──────────────────────────────────────────────────

#[test]
fn update_check_motd_text_lists_upgrades_and_installs() {
    let report = UpdateCheckReport {
        target: Some("image-v1.0".to_string()),
        backend: "rpm".to_string(),
        upgrade_available: true,
        action_required: true,
        cli: cli_unsupported("test"),
        components: Vec::new(),
        summary: CheckSummary {
            updates: 1,
            reconciliations: 0,
            missing_defaults: 1,
            unsupported: 0,
            errors: 0,
        },
    };
    let text = build_motd(&report).expect("motd text present");
    assert!(text.contains("ANOLISA toolchain update is available."));
    assert!(text.contains("1 component can be upgraded"));
    assert!(text.contains("1 new default component can be installed"));
    assert!(text.contains("Run: \"sudo anolisa upgrade\" to apply"));
    assert!(text.contains("\"anolisa update --check\" for details"));
}

#[test]
fn update_check_motd_is_silent_when_nothing_to_do() {
    let report = UpdateCheckReport {
        target: None,
        backend: "rpm".to_string(),
        upgrade_available: false,
        action_required: false,
        cli: CliCheck {
            package: Some("anolisa".to_string()),
            installed: Some("1.0.0-1.al4".to_string()),
            available: None,
            action: ACTION_NOOP.to_string(),
            error: None,
        },
        components: Vec::new(),
        summary: CheckSummary::default(),
    };
    assert!(build_motd(&report).is_none());
}

// ── cache ───────────────────────────────────────────────────────────

fn report_with_target(target: Option<&str>) -> UpdateCheckReport {
    UpdateCheckReport {
        target: target.map(str::to_string),
        backend: "rpm".to_string(),
        upgrade_available: true,
        action_required: true,
        cli: cli_unsupported("test"),
        components: Vec::new(),
        summary: CheckSummary {
            updates: 1,
            ..Default::default()
        },
    }
}

fn cache_now() -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-09-11T12:00:00Z")
        .expect("fixed test time")
        .with_timezone(&Utc)
}

#[test]
fn update_check_cache_round_trips_and_respects_ttl() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let path = cache_path(&layout);

    write_cache(&path, &report_with_target(None), cache_now).expect("write cache");

    let cache = read_cache(&path).expect("cache readable");
    assert_eq!(cache.report.summary.updates, 1);
    assert!(
        is_fresh(&cache.generated_at, cache_now),
        "just-written cache is fresh"
    );

    // A far-past timestamp is stale.
    assert!(!is_fresh("2000-01-01T00:00:00Z", cache_now));
}

/// A cached report must not be reused for a different (or absent) target — the
/// MOTD fast path is keyed by target as well as freshness.
#[test]
fn update_check_cache_usable_requires_matching_target() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let path = cache_path(&layout);
    write_cache(&path, &report_with_target(Some("image-v1.0")), cache_now).expect("write cache");
    let cache = read_cache(&path).expect("cache readable");

    assert!(
        cache_is_usable(&cache, Some("image-v1.0"), cache_now),
        "same target reuses the cache"
    );
    assert!(
        !cache_is_usable(&cache, None, cache_now),
        "a plain MOTD must not reuse a targeted report"
    );
    assert!(
        !cache_is_usable(&cache, Some("image-v2.0"), cache_now),
        "a different target must not reuse the report"
    );
}

fn cache_args() -> UpdateArgs {
    UpdateArgs {
        component: None,
        command: None,
        check: true,
        motd: true,
        refresh: false,
        target: None,
    }
}

#[test]
fn update_check_cache_clock_drives_real_handler_paths() {
    for (scenario, expected_events) in [
        ("fresh", vec!["check"]),
        ("ttl", vec!["check"]),
        ("offset", vec!["check"]),
        ("explicit_target", vec!["check"]),
        ("expired", vec!["check", "compute", "write"]),
        ("future", vec!["check", "compute", "write"]),
        ("target_mismatch", vec!["check", "compute", "write"]),
        ("target_missing", vec!["check", "compute", "write"]),
        ("invalid_time", vec!["compute", "write"]),
        ("missing", vec!["compute", "write"]),
        ("corrupt", vec!["compute", "write"]),
        ("refresh", vec!["compute", "write"]),
        ("json", vec!["compute", "write"]),
        ("human", vec!["compute", "write"]),
    ] {
        let sandbox = crate::test_support::TestSandbox::new();
        let mut ctx = sandbox.context(crate::context::InstallMode::System);
        let layout = common::resolve_layout(&ctx);
        let path = cache_path(&layout);
        let mut args = cache_args();
        let mut cache = UpdateCheckCache {
            generated_at: cache_now().to_rfc3339(),
            report: report_with_target(Some(DEFAULT_TARGET_PROFILE_NAME)),
        };
        match scenario {
            "ttl" => {
                cache.generated_at =
                    (cache_now() - chrono::Duration::seconds(CACHE_TTL_SECS)).to_rfc3339()
            }
            "expired" => {
                cache.generated_at = (cache_now()
                    - chrono::Duration::seconds(CACHE_TTL_SECS)
                    - chrono::Duration::nanoseconds(1))
                .to_rfc3339()
            }
            "future" => {
                cache.generated_at = (cache_now() + chrono::Duration::nanoseconds(1)).to_rfc3339()
            }
            "offset" => cache.generated_at = "2026-09-11T20:00:00+08:00".into(),
            "invalid_time" => cache.generated_at = "not a timestamp".into(),
            "target_mismatch" => cache.report.target = Some("image-other".into()),
            "target_missing" => cache.report.target = None,
            "explicit_target" => {
                args.target = Some("image-custom".into());
                cache.report.target = args.target.clone();
            }
            "refresh" => args.refresh = true,
            "json" => ctx.json = true,
            "human" => args.motd = false,
            _ => {}
        }
        if scenario != "missing" {
            std::fs::create_dir_all(path.parent().unwrap()).expect("cache parent");
            let body = if scenario == "corrupt" {
                "invalid JSON".to_string()
            } else {
                serde_json::to_string(&cache).unwrap()
            };
            std::fs::write(&path, body).expect("seed cache");
        }
        let before = std::fs::read(&path).ok();
        let events = RefCell::new(Vec::new());
        let written_at =
            cache_now() + chrono::Duration::seconds(61) + chrono::Duration::nanoseconds(123);
        let report = report_with_target(Some(effective_target_name(args.target.as_deref())));
        handle_update_check_with(
            &args,
            &ctx,
            || {
                let writing = events.borrow().contains(&"compute");
                events
                    .borrow_mut()
                    .push(if writing { "write" } else { "check" });
                if writing {
                    assert!(path.parent().unwrap().is_dir(), "mkdir before write clock");
                    written_at
                } else {
                    cache_now()
                }
            },
            |target, actual_ctx, actual_layout| {
                assert_eq!(target, args.target.as_deref());
                assert!(std::ptr::eq(actual_ctx, &ctx));
                assert_eq!(actual_layout.cache_dir, layout.cache_dir);
                assert!(!events.borrow().contains(&"compute"));
                events.borrow_mut().push("compute");
                assert_eq!(std::fs::read(&path).ok(), before, "no write before compute");
                Ok(report.clone())
            },
        )
        .expect(scenario);
        assert_eq!(*events.borrow(), expected_events, "{scenario}");
        if expected_events == ["check"] {
            assert_eq!(
                std::fs::read(&path).ok(),
                before,
                "cache hit must not rewrite"
            );
        } else {
            let saved = read_cache(&path).expect("new cache");
            assert_eq!(saved.generated_at, "2026-09-11T12:01:01Z", "{scenario}");
            assert_eq!(
                serde_json::to_value(saved.report).unwrap(),
                serde_json::to_value(&report).unwrap()
            );
        }
    }
}

#[test]
fn update_check_cache_report_failure_keeps_old_cache() {
    for (motd, json) in [(true, false), (true, true), (false, false), (false, true)] {
        let sandbox = crate::test_support::TestSandbox::new();
        let mut ctx = sandbox.context(crate::context::InstallMode::System);
        ctx.json = json;
        let path = cache_path(&common::resolve_layout(&ctx));
        let mut args = cache_args();
        args.motd = motd;
        write_cache(
            &path,
            &report_with_target(Some(DEFAULT_TARGET_PROFILE_NAME)),
            || cache_now() - chrono::Duration::days(1),
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        let events = RefCell::new(Vec::new());
        let result = handle_update_check_with(
            &args,
            &ctx,
            || {
                assert!(events.borrow().is_empty(), "no write clock after failure");
                events.borrow_mut().push("check");
                cache_now()
            },
            |_, _, _| {
                events.borrow_mut().push("compute");
                Err(CliError::Runtime {
                    command: CHECK_COMMAND.into(),
                    reason: "scripted report failure".into(),
                })
            },
        );
        if motd {
            result.expect("MOTD failure stays silent even with JSON");
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code(), "EXECUTION_FAILED");
            assert!(matches!(error, CliError::Runtime { command, reason }
                if command == CHECK_COMMAND && reason == "scripted report failure"));
        }
        assert_eq!(
            *events.borrow(),
            if motd && !json {
                vec!["check", "compute"]
            } else {
                vec!["compute"]
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn update_check_cache_write_failures_preserve_lazy_clock_and_success() {
    for parent_conflict in [true, false] {
        let sandbox = crate::test_support::TestSandbox::new();
        let ctx = sandbox.context(crate::context::InstallMode::System);
        let path = cache_path(&common::resolve_layout(&ctx));
        let parent = path.parent().unwrap();
        let blocker = if parent_conflict {
            std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
            parent.to_path_buf()
        } else {
            std::fs::create_dir_all(&path).unwrap();
            path.join("sentinel")
        };
        std::fs::write(&blocker, "preserve me").unwrap();
        let events = RefCell::new(Vec::new());
        handle_update_check_with(
            &cache_args(),
            &ctx,
            || {
                assert!(!parent_conflict, "mkdir failure must not read clock");
                assert!(parent.is_dir());
                events.borrow_mut().push("write");
                cache_now()
            },
            |_, _, _| {
                events.borrow_mut().push("compute");
                Ok(report_with_target(Some(DEFAULT_TARGET_PROFILE_NAME)))
            },
        )
        .expect("best-effort cache write");
        assert_eq!(
            *events.borrow(),
            if parent_conflict {
                vec!["compute"]
            } else {
                vec!["compute", "write"]
            }
        );
        assert_eq!(std::fs::read_to_string(&blocker).unwrap(), "preserve me");
    }
}

// ── repo config read-only + target profile ──────────────────────────

fn isolated_packaged_data_probe() -> crate::packaged::PackagedDataProbe {
    crate::packaged::PackagedDataProbe::from_inputs(None, None)
}

/// `--check` must load repo config without writing it. Here the config already
/// exists locally, so the read-only load returns it and touches nothing else;
/// the missing-config no-write guarantee is covered by
/// `repo_config::tests::load_dry_run_fetches_without_writing`.
#[test]
fn update_check_read_only_load_uses_existing_config_without_writing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let ctx = crate::test_support::context_for_root(
        tmp.path(),
        crate::context::InstallMode::System,
        Some(tmp.path().to_path_buf()),
        Default::default(),
    );
    std::fs::create_dir_all(&layout.etc_dir).expect("mkdir etc");
    let repo_toml = layout.etc_dir.join("repo.toml");
    std::fs::write(
        &repo_toml,
        "schema_version = 1\ndefault_backend = \"rpm\"\n\n[backends.rpm]\nbase_url = \"https://repo.example/$os/$arch/os\"\n",
    )
    .expect("write repo.toml");

    let cfg = load_repo_config_read_only(&ctx, &layout).expect("read-only load");
    assert_eq!(cfg.default_backend, "rpm");
    // No scratch file was left behind by the read-only path.
    assert!(!repo_toml.with_extension("toml.tmp").exists());
}

#[test]
fn update_check_target_profile_parses_default_components() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let dir = layout.etc_dir.join("profiles");
    std::fs::create_dir_all(&dir).expect("mkdir profiles");
    std::fs::write(
        dir.join("image-v1.0.toml"),
        "schema_version = 1\nname = \"image-v1.0\"\ndefault_components = [\"cosh\", \"sec-core\"]\n",
    )
    .expect("write profile");

    let probe = isolated_packaged_data_probe();
    let profile =
        load_target_profile_by_name(&layout, "image-v1.0", &probe).expect("profile loads");
    assert_eq!(profile.default_components, vec!["cosh", "sec-core"]);
}

#[test]
fn update_check_target_profile_rejects_traversal() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let probe = isolated_packaged_data_probe();
    let err = load_target_profile_by_name(&layout, "../escape", &probe)
        .expect_err("must reject traversal");
    assert_eq!(err.code(), "INVALID_ARGUMENT");
}

// ── default target profile + user-mode gating ───────────────────────

fn user_ctx() -> CliContext {
    crate::test_support::context_for_root(
        std::path::Path::new("/tmp/anolisa-update-check-validation"),
        crate::context::InstallMode::User,
        None,
        Default::default(),
    )
}

/// A non-system install mode is out of scope for the RPM upgrade check and must
/// be rejected explicitly rather than emit a misleading RPM report.
#[test]
fn update_check_rejects_user_mode() {
    let args = UpdateArgs {
        component: None,
        command: None,
        check: true,
        motd: false,
        refresh: false,
        target: None,
    };
    let err = handle_update_check_with(
        &args,
        &user_ctx(),
        || panic!("user mode must not read cache clock"),
        |_, _, _| panic!("user mode must not compute a report"),
    )
    .expect_err("user mode must be rejected");
    assert_eq!(err.code(), "INVALID_ARGUMENT");
}

/// The MOTD path must stay silent in user mode: a login banner returns `Ok(())`
/// without touching rpm/dnf or erroring.
#[test]
fn update_check_motd_user_mode_is_silent() {
    let args = UpdateArgs {
        component: None,
        command: None,
        check: true,
        motd: true,
        refresh: false,
        target: None,
    };
    handle_update_check_with(
        &args,
        &user_ctx(),
        || panic!("user MOTD must not read cache clock"),
        |_, _, _| panic!("user MOTD must not compute a report"),
    )
    .expect("motd in user mode is a silent Ok");
}

/// An omitted `--target` maps to the release default name for both the report
/// and the MOTD cache key.
#[test]
fn update_check_effective_target_defaults_to_release_profile() {
    assert_eq!(effective_target_name(None), DEFAULT_TARGET_PROFILE_NAME);
    assert_eq!(effective_target_name(Some("image-v1.0")), "image-v1.0");
}

/// The compiled-in default profile parses and declares at least one default.
#[test]
fn update_check_builtin_default_profile_parses() {
    let profile = load_builtin_default_profile().expect("builtin default profile parses");
    assert!(
        !profile.default_components.is_empty(),
        "default profile must declare at least one default component"
    );
}

/// With `--target` omitted, resolution yields the release default name and falls
/// back to the built-in profile when no disk profile exists.
#[test]
fn update_check_omitted_target_uses_builtin_default() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let probe = isolated_packaged_data_probe();
    let (name, profile) =
        load_effective_target_profile(&layout, None, &probe).expect("default target resolves");
    assert_eq!(name, DEFAULT_TARGET_PROFILE_NAME);
    assert!(!profile.default_components.is_empty());
}

/// Omitted `--target` and explicit `--target agentic_os-latest` share the same
/// lookup path, so an on-disk latest profile overrides the built-in fallback.
#[test]
fn update_check_omitted_target_uses_disk_default_profile() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let dir = layout.etc_dir.join("profiles");
    std::fs::create_dir_all(&dir).expect("mkdir profiles");
    std::fs::write(
        dir.join(format!("{DEFAULT_TARGET_PROFILE_NAME}.toml")),
        format!(
            "schema_version = 1\nname = \"{DEFAULT_TARGET_PROFILE_NAME}\"\ndefault_components = [\"disk-only\"]\n"
        ),
    )
    .expect("write default profile");

    let probe = isolated_packaged_data_probe();
    let (name, profile) =
        load_effective_target_profile(&layout, None, &probe).expect("default target resolves");
    assert_eq!(name, DEFAULT_TARGET_PROFILE_NAME);
    assert_eq!(profile.default_components, vec!["disk-only"]);
}

/// An explicit `--target agentic_os-latest` with no on-disk profile falls back to
/// the compiled-in default rather than erroring.
#[test]
fn update_check_explicit_default_target_falls_back_to_builtin() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let probe = isolated_packaged_data_probe();
    let profile = load_target_profile_by_name(&layout, DEFAULT_TARGET_PROFILE_NAME, &probe)
        .expect("explicit default falls back to builtin");
    assert!(!profile.default_components.is_empty());
}

/// A missing custom (non-default) profile is a hard invalid-argument error whose
/// message names the profile.
#[test]
fn update_check_explicit_custom_target_missing_is_invalid_argument() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let probe = isolated_packaged_data_probe();
    let err = load_target_profile_by_name(&layout, "no-such-profile", &probe)
        .expect_err("missing custom target must error");
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(err.reason().contains("no-such-profile"));
}

/// The MOTD cache written for the default target is reused when `--target` is
/// omitted, since both resolve to the same effective name.
#[test]
fn update_check_cache_usable_for_omitted_target_matches_default() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let path = cache_path(&layout);
    write_cache(
        &path,
        &report_with_target(Some(DEFAULT_TARGET_PROFILE_NAME)),
        cache_now,
    )
    .expect("write cache");
    let cache = read_cache(&path).expect("cache readable");
    assert!(
        cache_is_usable(&cache, Some(effective_target_name(None)), cache_now),
        "omitted target reuses the default-target cache"
    );
}
