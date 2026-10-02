//! Unit tests for `anolisa upgrade`. The apply path is driven entirely through
//! an injected fake that implements both [`PackageQuery`] and
//! [`PackageTransaction`], so no live rpmdb/dnf is required. The fake records
//! transaction call order and supports read-only install preflight on dry-run.

use super::application::UpgradeChange;
use super::*;

use anolisa_core::state::InstallMode as StateInstallMode;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use anolisa_platform::pkg_query::{PackageInfo, PackageQueryError, PackageVersion};

use anolisa_core::adapter::contract::{
    ContractProvenance, ContractSourceKind, read_snapshot_provenance, write_snapshot_provenance,
};
use anolisa_core::central_log::LogFilter;
use anolisa_core::state::{
    InstalledObject, InstalledState, ObjectStatus, OperationRecord, Ownership, RpmMetadata,
    SubscriptionScope,
};
use anolisa_core::transaction::{
    DelegatedRecordAction, DelegatedRecoveryContext, Transaction, TransactionStep,
};

// `Activity` and `ProgressReporter` reach the tests through the module glob
// (`use super::*`); `NoopReporter` is the only progress item not re-imported by
// the upgrade module, so it is brought in explicitly here.
use crate::context::InstallMode;
use crate::progress::NoopReporter;

// ── fake host ────────────────────────────────────────────────────────────────

/// In-memory host: `query_installed` returns the configured post-transaction
/// version, `update`/`install` record their call order and may be made to fail
/// for a named package.
#[derive(Default)]
struct FakeHost {
    repository: Option<&'static str>,
    program: Option<&'static str>,
    /// package -> version returned by `query_installed` (post-transaction).
    installed: HashMap<String, PackageInfo>,
    /// packages whose `update` transaction fails.
    fail_update: HashSet<String>,
    /// packages whose install preflight or transaction fails.
    fail_install: HashSet<String>,
    /// packages whose `query_installed` returns `Ok(None)` (rpmdb miss).
    missing_after: HashSet<String>,
    /// packages whose installed query reports multiple/unparseable rows.
    ambiguous_after: HashSet<String>,
    /// packages whose installed query fails.
    fail_query: HashSet<String>,
    /// packages whose next installed query fails once, then recovers.
    fail_query_once: RefCell<HashSet<String>>,
    fail_post_query_once: RefCell<HashSet<String>>,
    query_sequence: RefCell<HashMap<String, VecDeque<Option<PackageInfo>>>>,
    /// package -> source repo returned by `installed_origin`.
    origins: HashMap<String, String>,
    /// packages whose `installed_origin` returns an error.
    fail_origin: HashSet<String>,
    /// ordered transaction log (`"update:<pkg>"` / `"install:<pkg>"`).
    calls: RefCell<Vec<String>>,
    /// package names inspected through `query_installed`.
    query_calls: RefCell<Vec<String>>,
    preflight_calls: RefCell<Vec<Vec<String>>>,
}

impl FakeHost {
    fn with_installed(mut self, package: &str, info: PackageInfo) -> Self {
        self.installed.insert(package.to_string(), info);
        self
    }

    fn with_origin(mut self, package: &str, origin: &str) -> Self {
        self.origins.insert(package.to_string(), origin.to_string());
        self
    }

    fn with_query_sequence(self, package: &str, observations: Vec<Option<PackageInfo>>) -> Self {
        self.query_sequence
            .borrow_mut()
            .insert(package.to_string(), observations.into());
        self
    }

    fn txn_calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn query_calls(&self) -> Vec<String> {
        self.query_calls.borrow().clone()
    }
}

impl PackageQuery for FakeHost {
    fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
        self.query_calls.borrow_mut().push(package.to_string());
        if self.ambiguous_after.contains(package) {
            return Err(PackageQueryError::UnexpectedOutput {
                command: "rpm".to_string(),
                detail: "2 installed versions".to_string(),
            });
        }
        if self.fail_query.contains(package)
            || self.fail_query_once.borrow_mut().remove(package)
            || (!self.calls.borrow().is_empty()
                && self.fail_post_query_once.borrow_mut().remove(package))
        {
            return Err(PackageQueryError::QueryFailed {
                command: "rpm".to_string(),
                code: Some(1),
                stderr: "rpmdb read failed".to_string(),
            });
        }
        if self.missing_after.contains(package) {
            return Ok(None);
        }
        if let Some(observations) = self.query_sequence.borrow_mut().get_mut(package)
            && let Some(observation) = observations.pop_front()
        {
            return Ok(observation);
        }
        Ok(self.installed.get(package).cloned())
    }

    fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
        Ok(Vec::new())
    }

    fn installed_origin(&self, package: &str) -> Result<Option<String>, PackageQueryError> {
        if self.fail_origin.contains(package) {
            return Err(PackageQueryError::QueryFailed {
                command: self.program.unwrap_or("dnf").to_string(),
                code: Some(1),
                stderr: "repo lookup failed".to_string(),
            });
        }
        Ok(self.origins.get(package).cloned())
    }
}

impl PackageTransaction for FakeHost {
    fn repository_source(&self) -> Option<&str> {
        self.repository
    }

    fn check_install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.preflight_calls.borrow_mut().push(
            packages
                .iter()
                .map(|package| (*package).to_string())
                .collect(),
        );
        if packages
            .iter()
            .any(|package| self.fail_install.contains(*package))
        {
            return Err(PackageTransactionError::TransactionFailed {
                command: self.program.unwrap_or("dnf").to_string(),
                operation: "install preflight".to_string(),
                code: Some(1),
                stderr: "cosh-ng conflicts with copilot-shell".to_string(),
            });
        }
        Ok(())
    }

    // Calls record the full package set per invocation
    // (`verb:pkg-a,pkg-b`), so tests can pin that a merged transaction
    // really shared one dnf run. A transaction fails as a whole when any of
    // its packages is in the matching `fail_*` set, mirroring dnf's
    // unit-of-work semantics.
    fn install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.calls
            .borrow_mut()
            .push(format!("install:{}", packages.join(",")));
        if packages.iter().any(|p| self.fail_install.contains(*p)) {
            return Err(PackageTransactionError::TransactionFailed {
                command: self.program.unwrap_or("dnf").to_string(),
                operation: "install".to_string(),
                code: Some(1),
                stderr: "boom".to_string(),
            });
        }
        Ok(())
    }

    fn update(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.calls
            .borrow_mut()
            .push(format!("update:{}", packages.join(",")));
        if packages.iter().any(|p| self.fail_update.contains(*p)) {
            return Err(PackageTransactionError::TransactionFailed {
                command: self.program.unwrap_or("dnf").to_string(),
                operation: "update".to_string(),
                code: Some(1),
                stderr: "boom".to_string(),
            });
        }
        Ok(())
    }

    fn reinstall(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        // `upgrade` never reinstalls packages; a call is a routing bug.
        self.calls
            .borrow_mut()
            .push(format!("reinstall:{}", packages.join(",")));
        Ok(())
    }

    fn remove(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        // `upgrade` never removes packages; a call is a routing bug.
        self.calls
            .borrow_mut()
            .push(format!("remove:{}", packages.join(",")));
        Ok(())
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

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

fn info_with_arch(name: &str, version: &str, release: Option<&str>, arch: &str) -> PackageInfo {
    let mut package = info(name, version, release);
    package.arch = arch.to_string();
    package
}

fn cli_check(
    action: &str,
    package: Option<&str>,
    installed: Option<&str>,
    available: Option<&str>,
    error: Option<&str>,
) -> CliCheck {
    CliCheck {
        package: package.map(str::to_string),
        installed: installed.map(str::to_string),
        available: available.map(str::to_string),
        action: action.to_string(),
        error: error.map(str::to_string),
    }
}

fn cli_noop() -> CliCheck {
    cli_check(
        ACTION_NOOP,
        Some("anolisa"),
        Some("1.0.0-1.al4"),
        None,
        None,
    )
}

fn component_check(
    component: &str,
    package: Option<&str>,
    ownership: Option<&str>,
    installed: Option<&str>,
    available: Option<&str>,
    action: &str,
    error: Option<&str>,
) -> ComponentCheck {
    ComponentCheck {
        component: component.to_string(),
        package: package.map(str::to_string),
        ownership: ownership.map(str::to_string),
        installed: installed.map(str::to_string),
        available: available.map(str::to_string),
        action: action.to_string(),
        error: error.map(str::to_string),
        absent_from_state: false,
        backfill_rpm_metadata: false,
    }
}

/// Build an installed component object as it would exist in state before an
/// upgrade, so a planned update has a row to refresh.
fn rpm_component(name: &str, package: &str, evr: &str, ownership: Ownership) -> InstalledObject {
    InstalledObject {
        kind: ObjectKind::Component,
        name: name.to_string(),
        version: evr.to_string(),
        status: ObjectStatus::Installed,
        manifest_digest: None,
        distribution_source: None,
        raw_package: None,
        install_backend: Some(
            if matches!(ownership, Ownership::RawManaged) {
                "raw"
            } else {
                "rpm"
            }
            .to_string(),
        ),
        ownership: Some(ownership),
        rpm_metadata: if matches!(ownership, Ownership::RawManaged) {
            None
        } else {
            Some(RpmMetadata {
                package_name: package.to_string(),
                evr: Some(evr.to_string()),
                arch: Some("x86_64".to_string()),
                source_repo: Some("@System".to_string()),
            })
        },
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

/// Write a state file under `layout` seeded with `objects` so an apply run can
/// find (and refresh) already-installed components. Seeds stay in the legacy
/// v4 shape so every apply run also exercises the load-boundary migration.
fn seed_state(layout: &FsLayout, objects: Vec<InstalledObject>) {
    // System mode, matching the only mode `upgrade` runs in — a user-mode
    // legacy file would migrate its records into a user scope keyed by the
    // test-runner uid.
    let mut state = InstalledState {
        install_mode: StateInstallMode::System,
        prefix: layout.prefix.clone(),
        ..Default::default()
    };
    for obj in objects {
        state.objects.push(obj);
    }
    crate::test_support::write_legacy_state(&state, &layout.state_dir.join("installed.toml"))
        .expect("seed state");
}

/// Load the persisted state as the v5 store (post-upgrade assertions).
fn load_store(layout: &FsLayout) -> StateStore {
    StateStore::load(&layout.state_dir.join("installed.toml"), 0).expect("load store")
}

fn record_successful_operation(layout: &FsLayout, operation_id: String) {
    let state_path = layout.state_dir.join("installed.toml");
    let mut store = load_store(layout);
    store.operations.push(OperationRecord {
        id: operation_id,
        command: "install cosh".to_string(),
        status: "ok".to_string(),
        started_at: "2026-07-14T00:00:00Z".to_string(),
        finished_at: Some("2026-07-14T00:00:01Z".to_string()),
        parent_operation_id: None,
    });
    store.save(&state_path).expect("save committed operation");
}

fn begin_delegated_pending(
    layout: &FsLayout,
    operation: &str,
    subject: &str,
    package: &str,
    transaction_target: &str,
    record_action: DelegatedRecordAction,
) -> PathBuf {
    let state_path = layout.state_dir.join("installed.toml");
    let mut journal = Transaction::begin_with_subject(
        operation,
        Some(subject),
        state_path,
        &rpm_install::journal_dir(layout),
    )
    .expect("begin new-format journal");
    journal
        .record_delegated_steps(
            DelegatedRecoveryContext {
                pm: NativePm::Rpm,
                package: Some(package.to_string()),
                record_action,
                pinned: None,
            },
            [
                TransactionStep::planned("delegated-txn", transaction_target, operation, None),
                TransactionStep::planned("delegated-record", subject, "write-record", None),
            ],
        )
        .expect("persist new-format steps");
    let path = journal.journal_path.clone();
    drop(journal);
    path
}

/// Find a component installation or panic with a readable message.
fn find_component<'a>(store: &'a StateStore, name: &str) -> &'a Installation {
    store
        .find(ObjectKind::Component, name)
        .unwrap_or_else(|| panic!("component '{name}' must be in state"))
}

/// Destructure a delegated installation into (package name, relation label,
/// recorded EVR, arch, source repo) for compact assertions.
fn delegated_parts(
    installation: &Installation,
) -> (
    Option<&str>,
    &'static str,
    Option<&str>,
    Option<&str>,
    Option<&str>,
) {
    match &installation.binding {
        ProviderBinding::Delegated {
            package,
            relation,
            last_observed,
            ..
        } => (
            package.resolved_name(),
            relation.label(),
            last_observed.as_ref().and_then(|o| o.evr.as_deref()),
            last_observed.as_ref().and_then(|o| o.arch.as_deref()),
            last_observed
                .as_ref()
                .and_then(|o| o.source_repo.as_deref()),
        ),
        ProviderBinding::Owned { .. } => panic!(
            "component '{}' must be delegated, found owned",
            installation.name
        ),
    }
}

/// Recorded EVR of a delegated component, the v5 analogue of the legacy
/// `obj.version` assertion.
fn observed_evr<'a>(store: &'a StateStore, name: &str) -> Option<&'a str> {
    delegated_parts(find_component(store, name)).2
}

fn system_ctx(prefix: PathBuf) -> CliContext {
    crate::test_support::context_for_root(
        &prefix,
        InstallMode::System,
        Some(prefix.clone()),
        Default::default(),
    )
}

fn user_ctx() -> CliContext {
    crate::test_support::context_for_root(
        std::path::Path::new("/tmp/anolisa-upgrade-validation"),
        InstallMode::User,
        None,
        Default::default(),
    )
}

fn run_typed_upgrade(
    ctx: &CliContext,
    layout: &FsLayout,
    plan: &UpgradePlan,
    host: &FakeHost,
    intent: ExecutionIntent,
    is_root: bool,
) -> UpgradeApplicationOutcome {
    application::run_with_dependencies(
        UpgradeRequest {
            target: plan.target.as_deref(),
            intent,
        },
        ctx,
        layout,
        plan,
        host,
        host,
        is_root,
        COMMAND,
        &NoopReporter,
    )
    .expect("typed upgrade outcome")
}

// ── argument parsing ─────────────────────────────────────────────────────────

#[test]
fn upgrade_parses_bare() {
    let args = UpgradeArgs::try_parse_from(["upgrade"]).expect("parse");
    assert!(args.target.is_none());
    assert!(!args.assume_yes);
}

#[test]
fn upgrade_parses_target() {
    let args =
        UpgradeArgs::try_parse_from(["upgrade", "--target", "agentic_os-latest"]).expect("parse");
    assert_eq!(args.target.as_deref(), Some("agentic_os-latest"));
}

#[test]
fn upgrade_parses_assume_yes_long_and_short() {
    assert!(
        UpgradeArgs::try_parse_from(["upgrade", "--assume-yes"])
            .expect("parse")
            .assume_yes
    );
    assert!(
        UpgradeArgs::try_parse_from(["upgrade", "-y"])
            .expect("parse")
            .assume_yes
    );
}

#[test]
fn upgrade_maps_dry_run_to_execution_intent() {
    assert_eq!(execution_intent(true), ExecutionIntent::Plan);
    assert_eq!(execution_intent(false), ExecutionIntent::Apply);
}

#[test]
fn typed_preview_reports_plan_without_side_effects() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = build_plan(
        Some("agentic_os-latest".to_string()),
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let outcome = run_typed_upgrade(&ctx, &layout, &plan, &host, ExecutionIntent::Plan, false);

    let UpgradeApplicationOutcome::Preview {
        status,
        report,
        warnings,
    } = outcome
    else {
        panic!("plan intent must return a preview");
    };
    assert_eq!(status, UpgradePreviewStatus::Completed);
    assert_eq!(report.updated.len(), 1);
    assert!(warnings.is_empty());
    assert!(host.txn_calls().is_empty());
    assert!(!layout.lock_file.exists());
    assert!(!layout.state_dir.join("installed.toml").exists());
    assert!(!layout.central_log.exists());
}

#[test]
fn typed_apply_blocked_preserves_reason_without_side_effects() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "mystery",
            None,
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );

    let outcome = run_typed_upgrade(&ctx, &layout, &plan, &host, ExecutionIntent::Apply, true);

    let UpgradeApplicationOutcome::Blocked { reason, report } = outcome else {
        panic!("plan errors must block apply");
    };
    assert_eq!(
        reason,
        "upgrade plan contains 1 error(s); no action was taken"
    );
    assert_eq!(report.errors.len(), 1);
    assert!(host.txn_calls().is_empty());
    assert!(!layout.lock_file.exists());
    assert!(!layout.state_dir.join("installed.toml").exists());
    assert!(!layout.central_log.exists());
}

#[test]
fn typed_noop_apply_has_no_operation_or_change() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = build_plan(None, &cli_noop(), &[]);

    let outcome = run_typed_upgrade(&ctx, &layout, &plan, &host, ExecutionIntent::Apply, true);

    let UpgradeApplicationOutcome::Applied { report, outcome } = outcome else {
        panic!("clean no-op must be an applied outcome");
    };
    assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
    assert_eq!(outcome.operation_id(), None);
    assert!(outcome.changes().is_empty());
    assert!(outcome.warnings().is_empty());
    assert!(report.errors.is_empty());
    assert!(host.txn_calls().is_empty());
    assert!(!layout.state_dir.join("installed.toml").exists());
    assert!(!layout.central_log.exists());
}

#[test]
fn typed_completed_apply_carries_change_and_persisted_operation() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );
    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let outcome = run_typed_upgrade(&ctx, &layout, &plan, &host, ExecutionIntent::Apply, true);

    let UpgradeApplicationOutcome::Applied { report, outcome } = outcome else {
        panic!("successful apply must return an applied outcome");
    };
    assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
    assert_eq!(
        outcome.changes(),
        &[UpgradeChange::PackageUpdated {
            name: "cosh".to_string(),
            package: "copilot-shell".to_string(),
        }]
    );
    assert!(outcome.warnings().is_empty());
    assert!(report.errors.is_empty());
    let store = load_store(&layout);
    assert_eq!(
        outcome.operation_id(),
        store
            .operations
            .last()
            .map(|operation| operation.id.as_str())
    );
}

#[test]
fn typed_partial_keeps_terminal_reason_separate_from_warning() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );
    let mut host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    host.fail_origin.insert("copilot-shell".to_string());
    let plan = build_plan(
        None,
        &cli_noop(),
        &[
            component_check(
                "cosh",
                Some("copilot-shell"),
                Some("rpm-managed"),
                Some("1.0.0-1.al4"),
                Some("1.1.0-1.al4"),
                ACTION_UPDATE,
                None,
            ),
            component_check(
                "sec-core",
                Some("agent-sec-core"),
                Some("rpm-managed"),
                Some("1.0.0-1.al4"),
                Some("1.1.0-1.al4"),
                ACTION_UPDATE,
                None,
            ),
        ],
    );

    let outcome = run_typed_upgrade(&ctx, &layout, &plan, &host, ExecutionIntent::Apply, true);

    let UpgradeApplicationOutcome::Applied { report, outcome } = outcome else {
        panic!("mixed apply must return an applied outcome");
    };
    let CommandOutcomeStatus::Partial { reason } = outcome.status() else {
        panic!("mixed success and error must be partial");
    };
    assert_eq!(
        reason,
        "upgrade completed with 1 item error(s); reconciliation is required"
    );
    assert_eq!(report.errors.len(), 1);
    assert_eq!(outcome.changes().len(), 1);
    assert_eq!(outcome.warnings().len(), 1);
    assert!(outcome.warnings()[0].contains("source repo"));
    assert_ne!(outcome.warnings()[0], *reason);
    assert!(outcome.operation_id().is_some());
}

#[test]
fn typed_failed_apply_carries_reason_without_fabricating_changes() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "sec-core",
            "agent-sec-core",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );
    let mut host = FakeHost::default().with_installed(
        "agent-sec-core",
        info("agent-sec-core", "1.0.0", Some("1.al4")),
    );
    host.program = Some("yum");
    host.fail_update.insert("agent-sec-core".to_string());
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "sec-core",
            Some("agent-sec-core"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let outcome = run_typed_upgrade(&ctx, &layout, &plan, &host, ExecutionIntent::Apply, true);

    let UpgradeApplicationOutcome::Applied { report, outcome } = outcome else {
        panic!("failed transaction must return an applied outcome");
    };
    assert_eq!(
        outcome.status(),
        &CommandOutcomeStatus::Failed {
            reason: "upgrade failed with 1 item error(s)".to_string(),
        }
    );
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].reason, "yum update failed (exit 1): boom");
    assert!(outcome.changes().is_empty());
    assert!(outcome.warnings().is_empty());
    assert!(outcome.operation_id().is_some());
}

// ── mode / privilege gating ──────────────────────────────────────────────────

#[test]
fn upgrade_rejects_user_mode() {
    let args = UpgradeArgs {
        target: None,
        assume_yes: false,
    };
    let err = super::handle(args, &user_ctx()).expect_err("user mode must be rejected");
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(err.reason().contains("system"));
}

#[test]
fn upgrade_real_execution_without_root_is_rejected_with_sudo_hint() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = build_plan(None, &cli_noop(), &[]);

    let err = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        false,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect_err("non-root real execution must be rejected");
    assert_eq!(err.code(), "EXECUTION_FAILED");
    assert!(err.reason().contains("sudo anolisa upgrade"));
    assert!(host.txn_calls().is_empty(), "no dnf on a rejected run");
}

/// The apply layer allows a dry-run without root (no dnf, no state write). Note
/// this is the *system-mode* apply path: at the dispatcher layer a non-root
/// `anolisa upgrade --dry-run` still resolves to user mode unless `--install-mode
/// system` is given (or run under sudo), and is rejected there — the
/// `system_only("upgrade", true)` policy only waives the root requirement for a
/// dry-run, not the system-mode requirement (same as `repair`).
#[test]
fn upgrade_dry_run_apply_runs_without_root_and_touches_nothing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = build_plan(
        Some("agentic_os-latest".to_string()),
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        false,
        true,
        COMMAND,
        &NoopReporter,
    )
    .expect("dry-run is allowed without root");
    assert!(result.dry_run);
    assert!(host.preflight_calls.borrow().is_empty());
    assert!(result.warnings.is_empty());
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.updated.len(), 1);
    assert!(
        host.txn_calls().is_empty(),
        "dry-run must not run a transaction"
    );
    assert!(
        !layout.state_dir.join("installed.toml").exists(),
        "dry-run must not write state"
    );
}

#[test]
fn upgrade_dry_run_preflights_missing_defaults_as_one_transaction() {
    for (packages, conflict) in [
        (vec![], false),
        (vec!["copilot-shell"], false),
        (vec!["copilot-shell"], true),
        (vec!["copilot-shell", "os-skills"], false),
        (vec!["copilot-shell", "os-skills"], true),
    ] {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = system_ctx(tmp.path().to_path_buf());
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut host = FakeHost::default()
            .with_installed("cosh-ng", info("cosh-ng", "0.24.1", Some("1.alnx4")));
        host.program = Some("yum");
        if conflict {
            host.fail_install.insert("copilot-shell".to_string());
        }
        let components: Vec<_> = packages
            .iter()
            .map(|package| {
                component_check(
                    if *package == "copilot-shell" {
                        "cosh"
                    } else {
                        package
                    },
                    Some(package),
                    None,
                    None,
                    None,
                    ACTION_INSTALL,
                    None,
                )
            })
            .collect();
        let plan = build_plan(None, &cli_noop(), &components);
        let result = run_upgrade_with_deps(
            &ctx,
            &layout,
            &plan,
            &host,
            &host,
            true,
            true,
            COMMAND,
            &NoopReporter,
        )
        .expect("preview renders solver failures");

        assert!(result.dry_run);
        if conflict {
            assert!(
                result
                    .errors
                    .iter()
                    .all(|error| error.reason.starts_with("yum install preflight failed"))
            );
            assert!(!result.errors.is_empty());
        }
        assert!(result.warnings.is_empty());
        assert_eq!(
            result.status,
            if conflict { STATUS_FAILED } else { STATUS_OK }
        );
        if conflict {
            assert!(result.installed.is_empty());
            assert_eq!(result.errors.len(), packages.len());
            assert_eq!(result.errors[0].name, "cosh");
            assert!(result.errors.iter().all(|error| {
                error
                    .reason
                    .contains("cosh-ng conflicts with copilot-shell")
            }));
        } else {
            assert_eq!(result.installed.len(), packages.len());
            assert!(result.errors.is_empty());
        }
        let expected_calls = if packages.is_empty() {
            vec![]
        } else {
            vec![packages]
        };
        assert_eq!(*host.preflight_calls.borrow(), expected_calls);
        assert!(host.txn_calls().is_empty());
        assert!(!layout.lock_file.exists());
        assert!(!layout.state_dir.exists());
        assert!(!rpm_install::journal_dir(&layout).exists());
    }
}

#[test]
fn upgrade_non_root_preview_warns_without_running_dnf() {
    use anolisa_platform::command::{CommandOutput, CommandRunner};
    use anolisa_platform::rpm_repo::RpmRepoSource;
    use anolisa_platform::rpm_transaction::RpmTransaction;
    use std::cell::Cell;

    struct PrivilegeDeniedRunner<'a>(&'a Cell<usize>);

    impl CommandRunner for PrivilegeDeniedRunner<'_> {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            assert_eq!(program, "dnf");
            assert!(args.contains(&"--assumeno"));
            assert!(
                args.windows(3).any(|args| {
                    args == ["repository-packages", "anolisa-configured", "install"]
                })
            );
            self.0.set(self.0.get() + 1);
            Ok(CommandOutput {
                code: Some(1),
                stdout: String::new(),
                stderr: "This command has to be run with superuser privileges".to_string(),
            })
        }
    }

    let calls = Cell::new(0);
    let txn = RpmTransaction::with_runner_and_repo(
        PrivilegeDeniedRunner(&calls),
        RpmRepoSource::new(
            "anolisa-configured",
            "file:///unused-repository",
            Some(true),
        ),
    );
    assert!(txn.check_install(&["copilot-shell"]).is_err());
    calls.set(0);
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = UpgradePlan {
        installs: vec![PlannedInstall {
            name: "cosh".to_string(),
            package: "copilot-shell".to_string(),
        }],
        ..UpgradePlan::default()
    };
    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &txn,
        false,
        true,
        COMMAND,
        &NoopReporter,
    )
    .expect("non-root preview remains available");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].package, "copilot-shell");
    assert!(result.errors.is_empty());
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("conflicts have not been checked"));
    assert!(result.warnings[0].contains("sudo anolisa --install-mode system upgrade --dry-run"));
    assert_eq!(calls.get(), 0);
    assert!(!layout.lock_file.exists());
    assert!(!layout.state_dir.exists());
    assert!(!rpm_install::journal_dir(&layout).exists());
}

#[test]
fn upgrade_preview_redacts_configured_repo_credentials_from_preflight_error() {
    use anolisa_platform::command::{CommandOutput, CommandRunner};
    use anolisa_platform::rpm_repo::RpmRepoSource;
    use anolisa_platform::rpm_transaction::RpmTransaction;

    struct CredentialEchoRunner<'a>(&'a str);

    impl CommandRunner for CredentialEchoRunner<'_> {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            assert_eq!(program, "dnf");
            assert!(args.contains(&"--assumeno"));
            let repo_arg = format!("--repofrompath=anolisa-configured,{}", self.0);
            assert!(args.contains(&repo_arg.as_str()));
            Ok(CommandOutput {
                code: Some(1),
                stdout: format!("Added anolisa-configured repo from {}\n", self.0),
                stderr: "No match for argument: copilot-shell".to_string(),
            })
        }
    }

    for base_url in [
        "http://testuser:fakepassword123@repo.example.internal/anolisa/",
        "http://testuser:secret tail@repo.example.internal/private",
        "http://repo.example.internal/private secret-token",
    ] {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = system_ctx(tmp.path().to_path_buf());
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let host = FakeHost::default();
        let txn = RpmTransaction::with_runner_and_repo(
            CredentialEchoRunner(base_url),
            RpmRepoSource::new("anolisa-configured", base_url, Some(true)),
        );
        let plan = UpgradePlan {
            installs: vec![PlannedInstall {
                name: "cosh".to_string(),
                package: "copilot-shell".to_string(),
            }],
            ..UpgradePlan::default()
        };

        let result = run_upgrade_with_deps(
            &ctx,
            &layout,
            &plan,
            &host,
            &txn,
            true,
            true,
            COMMAND,
            &NoopReporter,
        )
        .expect("preview renders the sanitized preflight failure");

        let reason = &result.errors[0].reason;
        assert!(reason.starts_with("dnf install preflight failed"));
        let sanitized = concat!(
            "Added anolisa-configured repo from http://repo.example.internal\n",
            "No match for argument: copilot-shell"
        );
        assert!(
            reason.contains(sanitized),
            "preflight diagnostic was not sanitized: {reason}"
        );
        for secret in [
            "testuser",
            "fakepassword123",
            "secret",
            "tail@",
            "/anolisa/",
            "/private",
        ] {
            assert!(!reason.contains(secret), "{secret:?} leaked: {reason}");
        }
    }
}

#[test]
fn upgrade_rejects_pending_rpm_claim_before_any_transaction() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
        .expect("begin pending install");
    let host = FakeHost::default();
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );

    for dry_run in [true, false] {
        let err = run_upgrade_with_deps(
            &ctx,
            &layout,
            &plan,
            &host,
            &host,
            true,
            dry_run,
            COMMAND,
            &NoopReporter,
        )
        .expect_err("pending install must block the whole upgrade");
        assert!(err.reason().contains("repair cosh"));
    }
    assert!(host.txn_calls().is_empty());
}

#[test]
fn upgrade_rejects_new_format_install_journal_with_committed_operation() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(&layout, Vec::new());
    let state_path = layout.state_dir.join("installed.toml");
    let journal_path = begin_delegated_pending(
        &layout,
        "install",
        "cosh",
        "copilot-shell",
        "copilot-shell",
        DelegatedRecordAction::WriteManaged,
    );
    let journal = Transaction::load_journal(&journal_path).expect("load new-format journal");
    record_successful_operation(&layout, journal.operation_id);
    let state_before = std::fs::read(&state_path).expect("read state");
    let host = FakeHost::default();
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );

    let err = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect_err("new-format install journal must block upgrade");

    assert!(
        err.reason()
            .contains("sudo anolisa --install-mode system repair cosh")
    );
    assert!(
        err.reason()
            .contains(journal_path.to_string_lossy().as_ref())
    );
    assert!(host.txn_calls().is_empty());
    assert_eq!(
        std::fs::read(&state_path).expect("read state"),
        state_before
    );
}

#[test]
fn upgrade_rejects_new_format_update_journal_before_any_transaction() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );
    let state_path = layout.state_dir.join("installed.toml");
    let state_before = std::fs::read(&state_path).expect("read state");
    begin_delegated_pending(
        &layout,
        "update",
        "cosh",
        "copilot-shell",
        "copilot-shell",
        DelegatedRecordAction::Refresh,
    );
    let host = FakeHost::default();
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let err = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect_err("new-format update journal must block upgrade");

    assert!(
        err.reason()
            .contains("sudo anolisa --install-mode system repair cosh")
    );
    assert!(host.txn_calls().is_empty());
    assert_eq!(
        std::fs::read(&state_path).expect("read state"),
        state_before
    );
}

#[test]
fn upgrade_rejects_new_format_batch_member_journal() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(&layout, Vec::new());
    let state_path = layout.state_dir.join("installed.toml");
    let state_before = std::fs::read(&state_path).expect("read state");
    begin_delegated_pending(
        &layout,
        "install",
        "sec-core",
        "agent-sec-core",
        "copilot-shell,agent-sec-core",
        DelegatedRecordAction::WriteManaged,
    );
    let host = FakeHost::default();
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "sec-core",
            Some("agent-sec-core"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );

    let err = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect_err("a new-format batch member journal must block upgrade");

    assert!(
        err.reason()
            .contains("sudo anolisa --install-mode system repair sec-core")
    );
    assert!(host.txn_calls().is_empty());
    assert_eq!(
        std::fs::read(&state_path).expect("read state"),
        state_before
    );
}

#[test]
fn empty_plan_reconciliation_stops_for_pending_delegated_subject() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );
    let state_path = layout.state_dir.join("installed.toml");
    let state_before = std::fs::read(&state_path).expect("read state");
    begin_delegated_pending(
        &layout,
        "update",
        "cosh",
        "copilot-shell",
        "copilot-shell",
        DelegatedRecordAction::Refresh,
    );
    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    let plan = build_plan(None, &cli_noop(), &[]);

    let err = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect_err("root-wide reconciliation must not cross pending recovery");

    assert!(
        err.reason()
            .contains("sudo anolisa --install-mode system repair cosh")
    );
    assert!(host.query_calls().is_empty(), "no reconciliation query");
    assert!(host.txn_calls().is_empty(), "no dnf transaction");
    assert_eq!(
        std::fs::read(&state_path).expect("read state"),
        state_before,
        "rejected reconciliation must not rewrite state",
    );
}

#[test]
fn empty_plan_reconciliation_stops_for_pending_legacy_claim() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );
    let state_path = layout.state_dir.join("installed.toml");
    let state_before = std::fs::read(&state_path).expect("read state");
    rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
        .expect("begin pending legacy install");
    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    let plan = build_plan(None, &cli_noop(), &[]);

    let err = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect_err("root-wide reconciliation must not cross a legacy claim");

    assert!(
        err.reason()
            .contains("sudo anolisa --install-mode system repair cosh")
    );
    assert!(host.query_calls().is_empty(), "no reconciliation query");
    assert!(host.txn_calls().is_empty(), "no dnf transaction");
    assert_eq!(
        std::fs::read(&state_path).expect("read state"),
        state_before,
        "rejected reconciliation must not rewrite state",
    );
}

#[test]
fn committed_legacy_journal_does_not_block_empty_plan() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(&layout, Vec::new());
    let state_path = layout.state_dir.join("installed.toml");
    let pending =
        rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
            .expect("begin committed legacy install");
    let journal_path = pending.transaction.journal_path.clone();
    let operation_id = pending.transaction.operation_id.clone();
    drop(pending);
    record_successful_operation(&layout, operation_id);
    let state_before = std::fs::read(&state_path).expect("read state");
    let journal_before = std::fs::read(&journal_path).expect("read legacy journal");
    let host = FakeHost::default();
    let plan = build_plan(None, &cli_noop(), &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("committed legacy journal must not block upgrade");

    assert_eq!(result.status, STATUS_OK);
    assert!(result.reconciled.is_empty());
    assert!(result.errors.is_empty());
    assert!(host.query_calls().is_empty());
    assert!(host.txn_calls().is_empty());
    assert_eq!(
        std::fs::read(&state_path).expect("read state"),
        state_before,
        "a committed no-op must not rewrite state",
    );
    assert_eq!(
        std::fs::read(&journal_path).expect("read legacy journal"),
        journal_before,
        "upgrade must not rewrite a committed legacy journal",
    );
}

// ── plan conversion ──────────────────────────────────────────────────────────

#[test]
fn plan_classifies_update_install_skip_and_error() {
    let cli = cli_check(
        ACTION_UPDATE,
        Some("anolisa"),
        Some("0.5.0-1.al4"),
        Some("1.0.0-1.al4"),
        None,
    );
    let components = vec![
        // rpm update
        component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-observed"),
            Some("0.5.0-1.al4"),
            Some("1.0.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
        // installable default (resolved package)
        component_check(
            "agent-memory",
            Some("agent-memory"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        ),
        // raw-managed → skipped
        component_check(
            "local-tool",
            None,
            Some("raw-managed"),
            Some("0.5.0"),
            None,
            ACTION_UNSUPPORTED_RPM,
            None,
        ),
        // installable default without a resolved package → error
        component_check("mystery", None, None, None, None, ACTION_INSTALL, None),
        // item error surfaced by the check
        component_check(
            "broken",
            Some("broken-pkg"),
            Some("rpm-managed"),
            Some("1.0.0-1"),
            None,
            ACTION_ERROR,
            Some("rpmdb miss"),
        ),
        // noop → nothing
        component_check(
            "sec-core",
            Some("agent-sec-core"),
            Some("rpm-managed"),
            Some("1.0.0-1"),
            None,
            ACTION_NOOP,
            None,
        ),
    ];

    let plan = build_plan(Some("agentic_os-latest".to_string()), &cli, &components);

    assert_eq!(
        plan.cli.as_ref().map(|c| c.package.as_str()),
        Some("anolisa")
    );
    assert_eq!(plan.updates.len(), 1);
    assert_eq!(plan.updates[0].package, "copilot-shell");
    assert_eq!(plan.installs.len(), 1);
    assert_eq!(plan.installs[0].package, "agent-memory");
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].name, "local-tool");
    assert_eq!(plan.skipped[0].reason, RAW_SKIP_REASON);
    // "mystery" (install without package) + "broken" (check error) → 2 errors.
    assert_eq!(plan.errors.len(), 2);
    assert!(plan.has_errors());
}

#[test]
fn plan_treats_non_rpm_cli_as_skipped_not_error() {
    let cli = cli_check(
        ACTION_UNSUPPORTED,
        None,
        None,
        None,
        Some("anolisa CLI is not RPM-owned"),
    );
    let plan = build_plan(None, &cli, &[]);
    assert!(plan.cli.is_none());
    assert_eq!(plan.skipped.len(), 1);
    assert!(!plan.has_errors());
}

// ── apply: error blocking ────────────────────────────────────────────────────

#[test]
fn plan_errors_block_real_execution_before_dnf() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    // An install with no resolvable package is a plan error.
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "mystery",
            None,
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );
    assert!(plan.has_errors());

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("blocked plan renders rather than erroring");
    assert_eq!(result.status, STATUS_BLOCKED);
    assert!(!result.dry_run);
    assert!(
        host.txn_calls().is_empty(),
        "a blocked plan must not run any dnf transaction"
    );
    assert!(
        !layout.state_dir.join("installed.toml").exists(),
        "a blocked plan must not write state"
    );
}

// ── apply: raw boundary ──────────────────────────────────────────────────────

#[test]
fn raw_managed_component_is_skipped_and_never_transacted() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "local-tool",
            None,
            Some("raw-managed"),
            Some("0.5.0"),
            None,
            ACTION_UNSUPPORTED_RPM,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.skipped.len(), 1);
    assert_eq!(result.skipped[0].reason, RAW_SKIP_REASON);
    assert!(
        host.txn_calls().is_empty(),
        "a raw-managed component must never be transacted"
    );
}

// ── apply: transaction ordering ──────────────────────────────────────────────

#[test]
fn transaction_order_is_cli_then_updates_then_installs() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default()
        .with_installed("anolisa", info("anolisa", "1.0.0", Some("1.al4")))
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "1.1.0", Some("1.al4")),
        )
        .with_installed("agent-memory", info("agent-memory", "1.0.0", Some("1.al4")));
    // Seed the component being updated so its state row is refreshed cleanly.
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );

    let cli = cli_check(
        ACTION_UPDATE,
        Some("anolisa"),
        Some("0.5.0-1.al4"),
        Some("1.0.0-1.al4"),
        None,
    );
    let components = vec![
        component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
        component_check(
            "agent-memory",
            Some("agent-memory"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        ),
    ];
    let plan = build_plan(None, &cli, &components);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(
        host.txn_calls(),
        vec![
            "update:anolisa".to_string(),
            "update:copilot-shell".to_string(),
            "install:agent-memory".to_string(),
        ],
        "must apply CLI update, then component updates, then installs"
    );
}

// ── apply: state refresh for a new default ───────────────────────────────────

#[test]
fn stale_upgrade_plan_cannot_confirm_source_for_noop_transactions() {
    for action in [ACTION_UPDATE, ACTION_INSTALL] {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = system_ctx(tmp.path().to_path_buf());
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut host = FakeHost::default().with_installed(
            "copilot-shell",
            info("copilot-shell", "2.0.0", Some("1.al4")),
        );
        host.repository = Some("configured");
        if action == ACTION_UPDATE {
            seed_state(
                &layout,
                vec![rpm_component(
                    "cosh",
                    "copilot-shell",
                    "1.0.0-1.al4",
                    Ownership::RpmManaged,
                )],
            );
        }
        let plan = build_plan(
            None,
            &cli_noop(),
            &[component_check(
                "cosh",
                Some("copilot-shell"),
                (action == ACTION_UPDATE).then_some("rpm-managed"),
                (action == ACTION_UPDATE).then_some("1.0.0-1.al4"),
                Some("2.0.0-1.al4"),
                action,
                None,
            )],
        );
        let result = run_upgrade_with_deps(
            &ctx,
            &layout,
            &plan,
            &host,
            &host,
            true,
            false,
            COMMAND,
            &NoopReporter,
        )
        .unwrap();
        assert_eq!(result.status, STATUS_OK);
        assert_eq!(host.txn_calls().len(), 1);
        let store = load_store(&layout);
        let (_, _, evr, _, source) = delegated_parts(find_component(&store, "cosh"));
        assert_eq!(evr, Some("2.0.0-1.al4"));
        assert_eq!(
            source, None,
            "{action}: a stale plan is not proof of transaction provenance"
        );
    }
}

#[test]
fn upgrade_confirms_source_only_from_live_identity_or_proven_absence() {
    for action in [ACTION_UPDATE, ACTION_INSTALL] {
        for scenario in ["version", "architecture", "absent", "probe-error"] {
            let tmp = tempfile::tempdir().unwrap();
            let ctx = system_ctx(tmp.path().to_path_buf());
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            let post = info("copilot-shell", "2.0.0", Some("1.al4"));
            let mut before = post.clone();
            if scenario == "version" {
                before.version.version = "1.0.0".into();
            }
            if scenario == "architecture" {
                before.arch = "noarch".into();
            }
            let mut host = FakeHost::default().with_installed("copilot-shell", post);
            host.repository = Some("configured");
            if scenario == "probe-error" {
                host.fail_query_once
                    .borrow_mut()
                    .insert("copilot-shell".into());
            } else {
                host = host.with_query_sequence(
                    "copilot-shell",
                    vec![(scenario != "absent").then_some(before)],
                );
            }
            if action == ACTION_UPDATE {
                seed_state(
                    &layout,
                    vec![rpm_component(
                        "cosh",
                        "copilot-shell",
                        "1.0.0-1.al4",
                        Ownership::RpmManaged,
                    )],
                );
            }
            let plan = build_plan(
                None,
                &cli_noop(),
                &[component_check(
                    "cosh",
                    Some("copilot-shell"),
                    (action == ACTION_UPDATE).then_some("rpm-managed"),
                    (action == ACTION_UPDATE).then_some("1.0.0-1.al4"),
                    Some("2.0.0-1.al4"),
                    action,
                    None,
                )],
            );
            let result = run_upgrade_with_deps(
                &ctx,
                &layout,
                &plan,
                &host,
                &host,
                true,
                false,
                COMMAND,
                &NoopReporter,
            )
            .unwrap();
            assert_eq!(result.status, STATUS_OK, "{action}/{scenario}");
            let store = load_store(&layout);
            let (_, _, evr, _, source) = delegated_parts(find_component(&store, "cosh"));
            assert_eq!(evr, Some("2.0.0-1.al4"));
            assert_eq!(
                source,
                (scenario != "probe-error").then_some("configured"),
                "{action}/{scenario}"
            );
            if scenario == "probe-error" {
                assert!(
                    result
                        .warnings
                        .iter()
                        .any(|w| w.contains("before") && w.contains("copilot-shell"))
                );
            }
        }
    }
}

#[test]
fn upgrade_batch_and_retry_confirm_each_members_own_transition() {
    for action in [ACTION_UPDATE, ACTION_INSTALL] {
        for retry in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let ctx = system_ctx(tmp.path().to_path_buf());
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            let mut host = FakeHost {
                repository: Some("configured"),
                ..Default::default()
            };
            let mut checks = Vec::new();
            let mut records = Vec::new();
            for name in ["changed", "unchanged"] {
                host = host.with_installed(name, info(name, "2", Some("1")));
                let before = if action == ACTION_INSTALL {
                    None
                } else {
                    Some(info(name, "1", Some("1")))
                };
                if name == "changed" {
                    let snapshots = if retry {
                        vec![before.clone(), before]
                    } else {
                        vec![before]
                    };
                    host = host.with_query_sequence(name, snapshots);
                }
                if retry && name == "unchanged" {
                    if action == ACTION_INSTALL {
                        host.fail_install.insert(name.into());
                    } else {
                        host.fail_update.insert(name.into());
                    }
                }
                checks.push(component_check(
                    name,
                    Some(name),
                    (action == ACTION_UPDATE).then_some("rpm-managed"),
                    (action == ACTION_UPDATE).then_some("1-1"),
                    Some("2-1"),
                    action,
                    None,
                ));
                if action == ACTION_UPDATE {
                    records.push(rpm_component(name, name, "1-1", Ownership::RpmManaged));
                }
            }
            if !records.is_empty() {
                seed_state(&layout, records);
            }
            let plan = build_plan(None, &cli_noop(), &checks);
            let result = run_upgrade_with_deps(
                &ctx,
                &layout,
                &plan,
                &host,
                &host,
                true,
                false,
                COMMAND,
                &NoopReporter,
            )
            .unwrap();
            assert_eq!(
                result.status, STATUS_OK,
                "{action}/{retry}: {:?}",
                result.errors
            );
            assert_eq!(host.txn_calls().len(), if retry { 2 } else { 1 });
            let store = load_store(&layout);
            for name in ["changed", "unchanged"] {
                let (_, _, evr, _, source) = delegated_parts(find_component(&store, name));
                assert_eq!(evr, Some("2-1"));
                assert_eq!(
                    source,
                    (name == "changed").then_some("configured"),
                    "{action}/{retry}/{name}"
                );
            }
        }
    }
}

#[test]
fn installed_default_is_recorded_as_rpm_managed_after_refresh() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default()
        .with_installed("agent-memory", info("agent-memory", "1.0.0", Some("1.al4")))
        .with_origin("agent-memory", "alinux4-agentic-os");
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "agent-memory",
            Some("agent-memory"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].version.as_deref(), Some("1.0.0-1.al4"));

    let store = load_store(&layout);
    let (package, relation, evr, _arch, source_repo) =
        delegated_parts(find_component(&store, "agent-memory"));
    assert_eq!(relation, "managed", "upgrade ran the dnf install itself");
    assert_eq!(package, Some("agent-memory"));
    assert_eq!(evr, Some("1.0.0-1.al4"));
    assert_eq!(source_repo, Some("alinux4-agentic-os"));
}

#[test]
fn origin_lookup_failure_is_warning_not_apply_failure() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default()
        .with_installed("agent-memory", info("agent-memory", "1.0.0", Some("1.al4")));
    host.fail_origin.insert("agent-memory".to_string());
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "agent-memory",
            Some("agent-memory"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("could not determine source repo"));

    let store = load_store(&layout);
    let (_package, _relation, _evr, _arch, source_repo) =
        delegated_parts(find_component(&store, "agent-memory"));
    assert_eq!(source_repo, None);
}

#[test]
fn origin_lookup_failure_drops_source_after_external_update() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    host.fail_origin.insert("copilot-shell".to_string());
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.warnings.len(), 1);

    let store = load_store(&layout);
    let (_package, _relation, evr, _arch, source_repo) =
        delegated_parts(find_component(&store, "cosh"));
    assert_eq!(evr, Some("1.1.0-1.al4"));
    assert_eq!(source_repo, None);
}

// ── apply: partial failure ───────────────────────────────────────────────────

#[test]
fn partial_transaction_failure_is_reported_and_not_claimed_as_success() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    // The second component update fails at the dnf layer.
    host.fail_update.insert("agent-sec-core".to_string());
    // Seed the component that upgrades cleanly so its update is recorded.
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );

    let components = vec![
        component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
        component_check(
            "sec-core",
            Some("agent-sec-core"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
    ];
    let plan = build_plan(None, &cli_noop(), &components);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(
        result.status, STATUS_PARTIAL,
        "must not claim clean success"
    );
    assert_eq!(result.updated.len(), 1);
    assert_eq!(result.updated[0].name, "cosh");
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, "sec-core");

    // The durable operation record must reflect the partial outcome, not `ok`,
    // even though the cosh update landed in state.
    let store = load_store(&layout);
    assert_eq!(
        store.operations.last().map(|op| op.status.as_str()),
        Some("partial"),
        "a partial upgrade must not record an `ok` operation"
    );
}

// ── apply: state drift under the lock ────────────────────────────────────────

/// A default install whose name already belongs to a raw-managed component in
/// state (a concurrent install after planning) must be refused before dnf runs.
#[test]
fn install_does_not_overwrite_existing_raw_managed_component() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default()
        .with_installed("agent-memory", info("agent-memory", "1.0.0", Some("1.al4")));
    // A raw-managed component of the same name appeared in state after planning.
    seed_state(
        &layout,
        vec![rpm_component(
            "agent-memory",
            "agent-memory",
            "0.9.0",
            Ownership::RawManaged,
        )],
    );

    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "agent-memory",
            Some("agent-memory"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_FAILED, "nothing landed cleanly");
    assert!(result.installed.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, "agent-memory");
    assert!(
        host.txn_calls().is_empty(),
        "stale state must block dnf before RPM mutation"
    );

    // State must still describe the original owned component.
    let store = load_store(&layout);
    let installation = find_component(&store, "agent-memory");
    match &installation.binding {
        ProviderBinding::Owned { artifact } => assert_eq!(artifact.version, "0.9.0"),
        other => panic!("component must stay owned, found {other:?}"),
    }
}

/// A component update whose state row vanished after planning must be rejected
/// under the install lock before dnf mutates the host.
#[test]
fn update_state_drift_missing_object_is_reported_as_error() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "1.1.0", Some("1.al4")),
        )
        .with_origin("copilot-shell", "alinux4-agentic-os");
    // No state is seeded, so the planned update has no row to refresh.

    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(
        result.status, STATUS_FAILED,
        "a dnf success with no state refresh must not be reported as ok"
    );
    assert!(
        result.updated.is_empty(),
        "drifted update is not reported updated"
    );
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, "cosh");
    assert!(
        host.txn_calls().is_empty(),
        "stale state must block dnf before RPM mutation"
    );
}

#[test]
fn observed_default_update_missing_from_state_is_recorded_as_rpm_observed() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "1.1.0", Some("1.al4")),
        )
        .with_origin("copilot-shell", "alinux4-agentic-os");
    let mut observed_default = component_check(
        "cosh",
        Some("copilot-shell"),
        Some("rpm-observed"),
        Some("1.0.0-1.al4"),
        Some("1.1.0-1.al4"),
        ACTION_UPDATE,
        None,
    );
    observed_default.absent_from_state = true;
    let plan = build_plan(None, &cli_noop(), &[observed_default]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.updated.len(), 1);
    assert!(result.errors.is_empty());

    let store = load_store(&layout);
    let (package, relation, evr, _arch, source_repo) =
        delegated_parts(find_component(&store, "cosh"));
    assert_eq!(relation, "observed", "ANOLISA does not own its removal");
    assert_eq!(package, Some("copilot-shell"));
    assert_eq!(evr, Some("1.1.0-1.al4"));
    assert_eq!(source_repo, Some("alinux4-agentic-os"));
}

#[test]
fn observed_default_noop_missing_from_state_is_recorded_without_dnf() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    let mut observed_default = component_check(
        "cosh",
        Some("copilot-shell"),
        Some("rpm-observed"),
        Some("1.1.0-1.al4"),
        None,
        ACTION_NOOP,
        None,
    );
    observed_default.absent_from_state = true;
    let plan = build_plan(None, &cli_noop(), &[observed_default]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert!(result.updated.is_empty(), "no package update happened");
    assert_eq!(result.recorded.len(), 1);
    assert_eq!(result.recorded[0].name, "cosh");
    assert_eq!(result.recorded[0].version.as_deref(), Some("1.1.0-1.al4"));
    assert!(
        host.txn_calls().is_empty(),
        "recording an observed default must not run dnf"
    );

    let store = load_store(&layout);
    let (package, relation, evr, _arch, _source_repo) =
        delegated_parts(find_component(&store, "cosh"));
    assert_eq!(relation, "observed");
    assert_eq!(package, Some("copilot-shell"));
    assert_eq!(evr, Some("1.1.0-1.al4"));
}

// ── audit for CLI-only / component-less runs ─────────────────────────────────

/// A CLI-only upgrade really mutates the system (dnf updates the CLI RPM) but
/// records no ANOLISA component object; it must still leave a durable operation
/// record so the change is auditable via `anolisa logs`, not a silent `ok`.
#[test]
fn cli_only_upgrade_records_durable_audit() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host =
        FakeHost::default().with_installed("anolisa", info("anolisa", "1.0.0", Some("1.al4")));
    let cli = cli_check(
        ACTION_UPDATE,
        Some("anolisa"),
        Some("0.5.0-1.al4"),
        Some("1.0.0-1.al4"),
        None,
    );
    let plan = build_plan(None, &cli, &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.updated.len(), 1);
    assert_eq!(result.updated[0].name, "anolisa");
    assert!(result.installed.is_empty());

    let store = load_store(&layout);
    assert_eq!(
        store.operations.last().map(|op| op.status.as_str()),
        Some("ok"),
        "a CLI-only upgrade must still leave an operation record"
    );
    assert!(
        store.find(ObjectKind::Component, "anolisa").is_none(),
        "the CLI is not recorded as a component object"
    );
}

/// A CLI update that succeeds while a component transaction fails records a
/// `partial` operation, even though no component state object changed.
#[test]
fn cli_success_with_component_failure_records_partial_audit() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host =
        FakeHost::default().with_installed("anolisa", info("anolisa", "1.0.0", Some("1.al4")));
    host.fail_update.insert("copilot-shell".to_string());

    let cli = cli_check(
        ACTION_UPDATE,
        Some("anolisa"),
        Some("0.5.0-1.al4"),
        Some("1.0.0-1.al4"),
        None,
    );
    let components = vec![component_check(
        "cosh",
        Some("copilot-shell"),
        Some("rpm-managed"),
        Some("1.0.0-1.al4"),
        Some("1.1.0-1.al4"),
        ACTION_UPDATE,
        None,
    )];
    let plan = build_plan(None, &cli, &components);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_PARTIAL);
    assert_eq!(result.updated.len(), 1);
    assert_eq!(result.updated[0].name, "anolisa");
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, "cosh");

    let store = load_store(&layout);
    assert_eq!(
        store.operations.last().map(|op| op.status.as_str()),
        Some("partial"),
        "CLI success + component failure must record a partial operation"
    );
}

// ── batch RPM state reconciliation (issue #1472) ───────────────────────────

#[test]
fn empty_plan_reconciles_all_drifted_rpm_components() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let components = [
        ("sec-core", "agent-sec-core", "0.7.1", "0.8.0"),
        ("agentsight", "agentsight", "0.7.1", "0.8.0"),
        ("tokenless", "tokenless", "0.6.1", "0.7.0"),
        ("agent-memory", "agent-memory", "0.2.1", "0.2.3"),
        ("cosh", "copilot-shell", "2.6.1", "2.7.0"),
        ("skillfs", "skillfs", "0.3.2", "0.3.3"),
    ];
    let objects = components
        .iter()
        .map(|(name, package, old, _)| {
            rpm_component(
                name,
                package,
                &format!("{old}-1.alnx4"),
                Ownership::RpmManaged,
            )
        })
        .collect();
    seed_state(&layout, objects);

    let mut host = FakeHost::default();
    for (_, package, _, new) in components {
        host.installed
            .insert(package.to_string(), info(package, new, Some("1.alnx4")));
    }
    let plan = build_plan(None, &cli_noop(), &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("reconcile empty plan");

    assert_eq!(result.status, STATUS_OK);
    assert!(result.updated.is_empty());
    assert_eq!(result.reconciled.len(), 6);
    assert!(host.txn_calls().is_empty(), "reconcile must not call dnf");

    let store = load_store(&layout);
    assert_eq!(store.operations.len(), 1, "one upgrade operation");
    let operation_id = store.operations[0].id.clone();
    for (name, _package, _old, new) in components {
        let expected = format!("{new}-1.alnx4");
        let installation = find_component(&store, name);
        let (_package, _relation, evr, arch, _source_repo) = delegated_parts(installation);
        assert_eq!(evr, Some(expected.as_str()));
        assert_eq!(arch, Some("x86_64"));
        assert_eq!(
            installation.last_operation_id.as_deref(),
            Some(operation_id.as_str())
        );
    }
}

#[test]
fn empty_plan_reconciles_missing_manifest_provenance() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let component = "manifest-sync-test";
    let package = "manifest-sync-test-rpm";
    let evr = "2.0.0-1.alnx4";
    seed_state(
        &layout,
        vec![rpm_component(
            component,
            package,
            evr,
            Ownership::RpmManaged,
        )],
    );

    let package_datadir = layout.package_datadir().expect("package datadir");
    let source = FsLayout::component_contract_path(&package_datadir, component);
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
    std::fs::write(&source, "framework = \"same\"\n").expect("write source contract");
    let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
    std::fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("mkdir snapshot");
    std::fs::write(&snapshot, "framework = \"same\"\n").expect("write matching snapshot");
    let provenance_path = FsLayout::provenance_path_for_snapshot(&snapshot);

    let host = FakeHost::default().with_installed(package, info(package, "2.0.0", Some("1.alnx4")));
    let plan = build_plan(None, &cli_noop(), &[]);

    let preview = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        false,
        true,
        COMMAND,
        &NoopReporter,
    )
    .expect("preview manifest drift");

    assert_eq!(preview.reconciled.len(), 1);
    assert_eq!(preview.reconciled[0].reason, "component manifest drift");
    let json = serde_json::to_value(&preview).expect("serialize preview");
    assert_eq!(json["reconciled"][0]["reason"], "component manifest drift");
    assert_eq!(
        std::fs::read_to_string(&snapshot).expect("read unchanged snapshot"),
        "framework = \"same\"\n",
        "dry-run must not refresh the snapshot"
    );
    assert!(
        !provenance_path.exists(),
        "dry-run must not create provenance"
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("reconcile manifest drift");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.reconciled.len(), 1);
    assert_eq!(result.reconciled[0].name, component);
    assert!(
        host.txn_calls().is_empty(),
        "manifest sync must not call dnf"
    );
    assert_eq!(
        std::fs::read_to_string(&snapshot).expect("read refreshed snapshot"),
        "framework = \"same\"\n"
    );
    let provenance = read_snapshot_provenance(&snapshot).expect("read refreshed provenance");
    assert_eq!(provenance.source_kind, ContractSourceKind::Datadir);
    assert_eq!(provenance.source_path, source);
    assert_eq!(provenance.datadir_root, package_datadir);
}

#[test]
fn empty_plan_reconciles_stale_manifest_provenance() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let component = "manifest-stale-provenance";
    let package = "manifest-stale-provenance-rpm";
    let evr = "2.0.0-1.alnx4";
    seed_state(
        &layout,
        vec![rpm_component(
            component,
            package,
            evr,
            Ownership::RpmManaged,
        )],
    );

    let package_datadir = layout.package_datadir().expect("package datadir");
    let source = FsLayout::component_contract_path(&package_datadir, component);
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
    std::fs::write(&source, "framework = \"same\"\n").expect("write source contract");
    let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
    std::fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("mkdir snapshot");
    std::fs::write(&snapshot, "framework = \"same\"\n").expect("write matching snapshot");
    let stale_root = layout.datadir.clone();
    write_snapshot_provenance(
        &snapshot,
        &ContractProvenance {
            schema_version: 1,
            source_kind: ContractSourceKind::Datadir,
            source_path: FsLayout::component_contract_path(&stale_root, component),
            datadir_root: stale_root,
        },
    )
    .expect("write stale provenance");

    let host = FakeHost::default().with_installed(package, info(package, "2.0.0", Some("1.alnx4")));
    let plan = build_plan(None, &cli_noop(), &[]);

    let preview = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        false,
        true,
        COMMAND,
        &NoopReporter,
    )
    .expect("preview provenance drift");

    assert_eq!(preview.reconciled.len(), 1);
    assert_eq!(preview.reconciled[0].reason, "component manifest drift");
    let stale = read_snapshot_provenance(&snapshot).expect("read stale provenance");
    assert_ne!(stale.datadir_root, package_datadir);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("reconcile provenance drift");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.reconciled.len(), 1);
    assert!(
        host.txn_calls().is_empty(),
        "provenance sync must not call dnf"
    );
    let provenance = read_snapshot_provenance(&snapshot).expect("read refreshed provenance");
    assert_eq!(provenance.source_path, source);
    assert_eq!(provenance.datadir_root, package_datadir);
}

#[derive(Clone, Copy)]
enum BlockedManifestArtifact {
    Snapshot,
    Provenance,
}

fn assert_manifest_refresh_failure(blocked: BlockedManifestArtifact) {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let component = "manifest-refresh-failure";
    let package = "manifest-refresh-failure-rpm";
    let evr = "2.0.0-1.alnx4";
    let mut objects = vec![rpm_component(
        component,
        package,
        evr,
        Ownership::RpmManaged,
    )];
    if matches!(blocked, BlockedManifestArtifact::Provenance) {
        objects.push(rpm_component(
            "state-drift-success",
            "state-drift-success-rpm",
            "1.0.0-1.alnx4",
            Ownership::RpmManaged,
        ));
    }
    seed_state(&layout, objects);

    let package_datadir = layout.package_datadir().expect("package datadir");
    let source = FsLayout::component_contract_path(&package_datadir, component);
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
    std::fs::write(&source, "framework = \"new\"\n").expect("write source contract");
    let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
    let provenance_path = FsLayout::provenance_path_for_snapshot(&snapshot);
    let marker = match blocked {
        BlockedManifestArtifact::Snapshot => {
            std::fs::create_dir_all(&snapshot).expect("create blocking snapshot directory");
            snapshot.join("keep")
        }
        BlockedManifestArtifact::Provenance => {
            std::fs::create_dir_all(snapshot.parent().expect("snapshot parent"))
                .expect("mkdir snapshot");
            std::fs::write(&snapshot, "framework = \"old\"\n").expect("write old snapshot");
            std::fs::create_dir(&provenance_path).expect("create blocking provenance directory");
            provenance_path.join("keep")
        }
    };
    std::fs::write(&marker, "unchanged").expect("write artifact marker");

    let mut host =
        FakeHost::default().with_installed(package, info(package, "2.0.0", Some("1.alnx4")));
    if matches!(blocked, BlockedManifestArtifact::Provenance) {
        host.installed.insert(
            "state-drift-success-rpm".to_string(),
            info("state-drift-success-rpm", "2.0.0", Some("1.alnx4")),
        );
    }
    let plan = build_plan(None, &cli_noop(), &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("render failed manifest reconciliation");

    let (expected_status, expected_log_status, expected_reconciled) = match blocked {
        BlockedManifestArtifact::Snapshot => (STATUS_FAILED, LogStatus::Failed, 0),
        BlockedManifestArtifact::Provenance => (STATUS_PARTIAL, LogStatus::Partial, 1),
    };
    assert_eq!(result.status, expected_status);
    assert_eq!(result.reconciled.len(), expected_reconciled);
    assert!(
        result.reconciled.iter().all(|item| item.name != component),
        "failed manifest item must not remain reconciled"
    );
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, component);
    assert!(result.errors[0].reason.contains("manifest reconciliation"));
    assert!(
        host.txn_calls().is_empty(),
        "failed manifest sync must not call dnf"
    );
    let json = serde_json::to_value(&result).expect("serialize failed result");
    assert_eq!(json["status"], expected_status);
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read unchanged marker"),
        "unchanged"
    );
    if matches!(blocked, BlockedManifestArtifact::Provenance) {
        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("read restored snapshot"),
            "framework = \"old\"\n"
        );
    }

    let store = load_store(&layout);
    assert_eq!(
        store
            .operations
            .last()
            .map(|operation| operation.status.as_str()),
        Some(expected_status)
    );
    let records = CentralLog::open(layout.central_log)
        .query(&LogFilter::default())
        .expect("query central log");
    assert_eq!(
        records.last().and_then(|record| record.status.as_ref()),
        Some(&expected_log_status)
    );
}

#[test]
fn empty_plan_reports_snapshot_refresh_failure() {
    assert_manifest_refresh_failure(BlockedManifestArtifact::Snapshot);
}

#[test]
fn empty_plan_reports_provenance_refresh_failure() {
    assert_manifest_refresh_failure(BlockedManifestArtifact::Provenance);
}

fn assert_updated_manifest_refresh_failure(blocked: BlockedManifestArtifact) {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let component = "manifest-update-failure";
    let package = "manifest-update-failure-rpm";
    let from = "1.0.0-1.alnx4";
    let to = "2.0.0-1.alnx4";
    seed_state(
        &layout,
        vec![rpm_component(
            component,
            package,
            from,
            Ownership::RpmManaged,
        )],
    );

    let package_datadir = layout.package_datadir().expect("package datadir");
    let source = FsLayout::component_contract_path(&package_datadir, component);
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
    std::fs::write(&source, "framework = \"new\"\n").expect("write source contract");
    let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
    let provenance_path = FsLayout::provenance_path_for_snapshot(&snapshot);
    let marker = match blocked {
        BlockedManifestArtifact::Snapshot => {
            std::fs::create_dir_all(&snapshot).expect("create blocking snapshot directory");
            snapshot.join("keep")
        }
        BlockedManifestArtifact::Provenance => {
            std::fs::create_dir_all(snapshot.parent().expect("snapshot parent"))
                .expect("mkdir snapshot");
            std::fs::write(&snapshot, "framework = \"old\"\n").expect("write old snapshot");
            std::fs::create_dir(&provenance_path).expect("create blocking provenance directory");
            provenance_path.join("keep")
        }
    };
    std::fs::write(&marker, "unchanged").expect("write artifact marker");

    let host = FakeHost::default().with_installed(package, info(package, "2.0.0", Some("1.alnx4")));
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            component,
            Some(package),
            Some("rpm-managed"),
            Some(from),
            Some(to),
            ACTION_UPDATE,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("render failed post-transaction manifest refresh");

    assert_eq!(result.status, STATUS_PARTIAL);
    assert_eq!(result.updated.len(), 1);
    assert_eq!(result.updated[0].name, component);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, component);
    assert!(result.errors[0].reason.contains("manifest refresh"));
    assert_eq!(host.txn_calls(), vec![format!("update:{package}")]);
    let json = serde_json::to_value(&result).expect("serialize failed result");
    assert_eq!(json["status"], STATUS_PARTIAL);
    assert_eq!(json["updated"][0]["name"], component);
    assert_eq!(json["errors"][0]["name"], component);
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read unchanged marker"),
        "unchanged"
    );
    if matches!(blocked, BlockedManifestArtifact::Provenance) {
        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("read restored snapshot"),
            "framework = \"old\"\n"
        );
    }

    let store = load_store(&layout);
    assert_eq!(observed_evr(&store, component), Some(to));
    assert_eq!(
        store
            .operations
            .last()
            .map(|operation| operation.status.as_str()),
        Some(STATUS_PARTIAL)
    );
    let records = CentralLog::open(layout.central_log)
        .query(&LogFilter::default())
        .expect("query central log");
    assert_eq!(
        records.last().and_then(|record| record.status.as_ref()),
        Some(&LogStatus::Partial)
    );
}

#[test]
fn updated_transaction_reports_snapshot_refresh_failure() {
    assert_updated_manifest_refresh_failure(BlockedManifestArtifact::Snapshot);
}

#[test]
fn updated_transaction_reports_provenance_refresh_failure() {
    assert_updated_manifest_refresh_failure(BlockedManifestArtifact::Provenance);
}

#[test]
fn empty_plan_reconciles_legacy_rpm_component_and_backfills_metadata() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut legacy = rpm_component(
        "cosh",
        "copilot-shell",
        "2.6.1-1.alnx4",
        Ownership::RpmManaged,
    );
    legacy.rpm_metadata = None;
    seed_state(&layout, vec![legacy]);

    let host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "2.7.0", Some("1.alnx4")),
        )
        .with_origin("copilot-shell", "anolisa");
    let check = component_check(
        "cosh",
        Some("copilot-shell"),
        Some("rpm-managed"),
        Some("2.7.0-1.alnx4"),
        None,
        ACTION_RECONCILE,
        None,
    );
    let plan = build_plan(None, &cli_noop(), &[check]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("reconcile legacy component");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.reconciled.len(), 1);
    assert_eq!(result.reconciled[0].name, "cosh");
    assert!(host.txn_calls().is_empty());
    let store = load_store(&layout);
    let (package, _relation, evr, arch, source_repo) =
        delegated_parts(find_component(&store, "cosh"));
    assert_eq!(
        package,
        Some("copilot-shell"),
        "package identity backfilled"
    );
    assert_eq!(evr, Some("2.7.0-1.alnx4"));
    assert_eq!(arch, Some("x86_64"));
    assert_eq!(source_repo, Some("anolisa"));
}

#[test]
fn planned_update_backfills_legacy_rpm_metadata() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut legacy = rpm_component(
        "cosh",
        "copilot-shell",
        "2.6.1-1.alnx4",
        Ownership::RpmManaged,
    );
    legacy.rpm_metadata = None;
    seed_state(&layout, vec![legacy]);

    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "2.7.0", Some("1.alnx4")),
    );
    let mut check = component_check(
        "cosh",
        Some("copilot-shell"),
        Some("rpm-managed"),
        Some("2.6.1-1.alnx4"),
        Some("2.7.0-1.alnx4"),
        ACTION_UPDATE,
        None,
    );
    check.backfill_rpm_metadata = true;
    let plan = build_plan(None, &cli_noop(), &[check]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("update legacy component");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.updated.len(), 1);
    assert!(result.reconciled.is_empty());
    assert_eq!(host.txn_calls(), vec!["update:copilot-shell"]);
    let store = load_store(&layout);
    let (package, _relation, evr, _arch, _source_repo) =
        delegated_parts(find_component(&store, "cosh"));
    assert_eq!(
        package,
        Some("copilot-shell"),
        "package identity backfilled"
    );
    assert_eq!(evr, Some("2.7.0-1.alnx4"));
}

#[test]
fn empty_plan_without_drift_is_true_noop() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut object = rpm_component(
        "cosh",
        "copilot-shell",
        "2.7.0-1.alnx4",
        Ownership::RpmManaged,
    );
    object.last_operation_id = Some("op-before".to_string());
    seed_state(&layout, vec![object]);
    let state_path = layout.state_dir.join("installed.toml");
    let before = std::fs::read(&state_path).expect("state bytes");
    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "2.7.0", Some("1.alnx4")),
    );
    let plan = build_plan(None, &cli_noop(), &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("true noop");

    assert_eq!(result.status, STATUS_OK);
    assert!(result.reconciled.is_empty());
    assert!(result.errors.is_empty());
    assert!(host.txn_calls().is_empty());
    let after = std::fs::read(&state_path).expect("state bytes");
    assert_eq!(after, before, "a drift-free run must not save state");
}

#[test]
fn planned_update_is_not_reported_as_reconciled() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![
            rpm_component(
                "cosh",
                "copilot-shell",
                "2.6.1-1.alnx4",
                Ownership::RpmManaged,
            ),
            rpm_component(
                "sec-core",
                "agent-sec-core",
                "0.7.1-1.alnx4",
                Ownership::RpmManaged,
            ),
        ],
    );
    let host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "2.7.0", Some("1.alnx4")),
        )
        .with_installed(
            "agent-sec-core",
            info("agent-sec-core", "0.8.0", Some("1.alnx4")),
        );
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("2.6.1-1.alnx4"),
            Some("2.7.0-1.alnx4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("update and reconcile");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.updated.len(), 1);
    assert_eq!(result.updated[0].name, "cosh");
    assert_eq!(result.reconciled.len(), 1);
    assert_eq!(result.reconciled[0].name, "sec-core");
    assert_eq!(host.txn_calls(), vec!["update:copilot-shell"]);

    let store = load_store(&layout);
    assert_eq!(store.operations.len(), 1, "one operation and one save path");
    assert_eq!(
        store.operations[0].status, STATUS_OK,
        "a component without a contract remains a successful update"
    );
    let operation_id = store.operations[0].id.clone();
    for name in ["cosh", "sec-core"] {
        assert_eq!(
            find_component(&store, name).last_operation_id.as_deref(),
            Some(operation_id.as_str()),
            "all state changes share the upgrade operation",
        );
    }
}

#[test]
fn post_transaction_query_failure_is_not_double_counted() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "2.6.1-1.alnx4",
            Ownership::RpmManaged,
        )],
    );
    let mut host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "2.7.0", Some("1.alnx4")),
    );
    host.fail_query.insert("copilot-shell".to_string());
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("2.6.1-1.alnx4"),
            Some("2.7.0-1.alnx4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("post-transaction query failure");

    assert_eq!(host.txn_calls(), vec!["update:copilot-shell"]);
    assert_eq!(
        host.query_calls(),
        vec!["copilot-shell", "copilot-shell", "copilot-shell"]
    );
    assert_eq!(result.status, STATUS_FAILED);
    assert!(result.updated.is_empty());
    assert!(result.reconciled.is_empty());
    assert_eq!(result.errors.len(), 1, "one component yields one error");
    assert_eq!(result.errors[0].name, "cosh");

    let store = load_store(&layout);
    assert_eq!(observed_evr(&store, "cosh"), Some("2.6.1-1.alnx4"));
    assert_eq!(store.operations.len(), 1);
    assert_eq!(store.operations[0].status, STATUS_FAILED);
}

#[test]
fn post_transaction_query_retry_can_reconcile() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "2.6.1-1.alnx4",
            Ownership::RpmManaged,
        )],
    );
    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "2.7.0", Some("1.alnx4")),
    );
    host.fail_post_query_once
        .borrow_mut()
        .insert("copilot-shell".to_string());
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("2.6.1-1.alnx4"),
            Some("2.7.0-1.alnx4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("reconcile after query recovery");

    assert_eq!(host.txn_calls(), vec!["update:copilot-shell"]);
    assert_eq!(
        host.query_calls(),
        vec!["copilot-shell", "copilot-shell", "copilot-shell"]
    );
    assert_eq!(result.status, STATUS_PARTIAL);
    assert!(result.updated.is_empty());
    assert_eq!(result.reconciled.len(), 1);
    assert_eq!(result.reconciled[0].name, "cosh");
    assert_eq!(result.errors.len(), 1, "the original apply error remains");

    let store = load_store(&layout);
    let installation = find_component(&store, "cosh");
    assert_eq!(observed_evr(&store, "cosh"), Some("2.7.0-1.alnx4"));
    assert_eq!(store.operations.len(), 1);
    assert_eq!(store.operations[0].status, STATUS_PARTIAL);
    assert_eq!(
        installation.last_operation_id.as_deref(),
        Some(store.operations[0].id.as_str())
    );
}

#[test]
fn reconcile_preserves_rpm_ownership_and_metadata() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut managed = rpm_component(
        "cosh",
        "copilot-shell",
        "2.6.1-1.alnx4",
        Ownership::RpmManaged,
    );
    managed.manifest_digest = Some("sha256:managed".to_string());
    managed.enabled_features = vec!["shell-hooks".to_string()];
    let mut observed = rpm_component(
        "os-skills",
        "os-skills",
        "0.6.1-1.alnx4",
        Ownership::RpmObserved,
    );
    observed.status = ObjectStatus::Adopted;
    observed.installed_at = "2026-05-01T00:00:00Z".to_string();
    observed.manifest_digest = Some("sha256:observed".to_string());
    if let Some(metadata) = observed.rpm_metadata.as_mut() {
        metadata.evr = None;
        metadata.arch = None;
    }
    let mut raw = rpm_component("local-tool", "local-tool", "1.0.0", Ownership::RawManaged);
    raw.enabled_features = vec!["local".to_string()];
    seed_state(&layout, vec![managed, observed, raw]);
    let raw_before = find_component(&load_store(&layout), "local-tool").clone();
    let host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info_with_arch("copilot-shell", "2.7.0", Some("1.alnx4"), "aarch64"),
        )
        .with_origin("copilot-shell", "anolisa-updates")
        .with_installed(
            "os-skills",
            info_with_arch("os-skills", "0.6.1", Some("1.alnx4"), "noarch"),
        )
        .with_origin("os-skills", "anolisa-updates");
    let plan = build_plan(None, &cli_noop(), &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("preserving reconcile");

    assert_eq!(result.reconciled.len(), 2);
    let store = load_store(&layout);

    // The managed row keeps its relation and features; only the observation
    // cache is refreshed from rpmdb.
    let actual_managed = find_component(&store, "cosh");
    let (package, relation, evr, arch, source_repo) = delegated_parts(actual_managed);
    assert_eq!(
        relation, "managed",
        "reconcile must not change the relation"
    );
    assert_eq!(package, Some("copilot-shell"));
    assert_eq!(evr, Some("2.7.0-1.alnx4"));
    assert_eq!(arch, Some("aarch64"));
    assert_eq!(source_repo, Some("anolisa-updates"));
    assert_eq!(actual_managed.enabled_features, vec!["shell-hooks"]);

    // The adopted row (legacy rpm-observed with adoption consent) keeps its
    // relation; the never-recorded EVR/arch are backfilled from rpmdb.
    let actual_observed = find_component(&store, "os-skills");
    let (package, relation, evr, arch, source_repo) = delegated_parts(actual_observed);
    assert_eq!(
        relation, "adopted",
        "reconcile must not change the relation"
    );
    assert_eq!(package, Some("os-skills"));
    assert_eq!(evr, Some("0.6.1-1.alnx4"));
    assert_eq!(arch, Some("noarch"));
    assert_eq!(source_repo, Some("anolisa-updates"));
    assert_eq!(
        actual_observed.installed_at, "2026-05-01T00:00:00Z",
        "reconcile must not restamp the install time"
    );

    assert_eq!(
        find_component(&store, "local-tool"),
        &raw_before,
        "owned state is untouched",
    );
    assert_eq!(
        host.query_calls().into_iter().collect::<HashSet<_>>(),
        HashSet::from(["copilot-shell".to_string(), "os-skills".to_string()]),
        "owned components are not queried",
    );
}

#[test]
fn reconcile_missing_or_ambiguous_rpm_is_not_written() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let objects = vec![
        rpm_component(
            "missing",
            "missing-pkg",
            "1.0.0-1.alnx4",
            Ownership::RpmManaged,
        ),
        rpm_component(
            "ambiguous",
            "ambiguous-pkg",
            "1.0.0-1.alnx4",
            Ownership::RpmManaged,
        ),
        rpm_component(
            "broken",
            "broken-pkg",
            "1.0.0-1.alnx4",
            Ownership::RpmManaged,
        ),
    ];
    seed_state(&layout, objects);
    let installations_before = load_store(&layout).installations;
    let mut host = FakeHost::default();
    host.missing_after.insert("missing-pkg".to_string());
    host.ambiguous_after.insert("ambiguous-pkg".to_string());
    host.fail_query.insert("broken-pkg".to_string());
    let plan = build_plan(None, &cli_noop(), &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("failed reconciliation audit");

    assert_eq!(result.status, STATUS_FAILED);
    assert!(result.reconciled.is_empty());
    assert_eq!(result.errors.len(), 3);
    for (name, package) in [
        ("missing", "missing-pkg"),
        ("ambiguous", "ambiguous-pkg"),
        ("broken", "broken-pkg"),
    ] {
        let error = result
            .errors
            .iter()
            .find(|error| error.name == name)
            .expect("item error");
        assert!(
            error.reason.contains(package),
            "package identity is visible"
        );
    }
    let store = load_store(&layout);
    assert_eq!(
        store.installations, installations_before,
        "failed items remain byte-for-byte facts"
    );
    assert_eq!(store.operations.len(), 1);
    assert_eq!(store.operations[0].status, STATUS_FAILED);

    let partial_tmp = tempfile::tempdir().expect("partial tmpdir");
    let partial_ctx = system_ctx(partial_tmp.path().to_path_buf());
    let partial_layout = FsLayout::system(Some(partial_tmp.path().to_path_buf()));
    seed_state(
        &partial_layout,
        vec![
            rpm_component("good", "good-pkg", "1.0.0-1.alnx4", Ownership::RpmManaged),
            rpm_component(
                "missing",
                "missing-pkg",
                "1.0.0-1.alnx4",
                Ownership::RpmManaged,
            ),
        ],
    );
    let mut partial_host =
        FakeHost::default().with_installed("good-pkg", info("good-pkg", "2.0.0", Some("1.alnx4")));
    partial_host.missing_after.insert("missing-pkg".to_string());
    let partial_result = run_upgrade_with_deps(
        &partial_ctx,
        &partial_layout,
        &plan,
        &partial_host,
        &partial_host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("partial reconciliation");
    assert_eq!(partial_result.status, STATUS_PARTIAL);
    assert_eq!(partial_result.reconciled.len(), 1);
    assert_eq!(partial_result.errors.len(), 1);
}

#[test]
fn reconcile_origin_failure_drops_source_after_external_update() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "2.6.1-1.alnx4",
            Ownership::RpmManaged,
        )],
    );
    let mut host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "2.7.0", Some("1.alnx4")),
    );
    host.fail_origin.insert("copilot-shell".to_string());
    let plan = build_plan(None, &cli_noop(), &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("origin warning");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.reconciled.len(), 1);
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("copilot-shell"));
    let store = load_store(&layout);
    let (_package, _relation, evr, _arch, source_repo) =
        delegated_parts(find_component(&store, "cosh"));
    assert_eq!(evr, Some("2.7.0-1.alnx4"));
    assert_eq!(source_repo, None);
}

#[test]
fn blocked_dry_run_does_not_inspect_rpm_reconciliation() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "2.6.1-1.alnx4",
            Ownership::RpmManaged,
        )],
    );
    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "2.7.0", Some("1.alnx4")),
    );
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "broken",
            None,
            Some("rpm-managed"),
            None,
            None,
            ACTION_ERROR,
            Some("planning failed"),
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        false,
        true,
        COMMAND,
        &NoopReporter,
    )
    .expect("blocked dry-run renders");

    assert_eq!(result.status, STATUS_BLOCKED);
    assert!(result.reconciled.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert!(host.query_calls().is_empty());
    assert!(host.txn_calls().is_empty());
}

#[test]
fn upgrade_dry_run_reports_reconcile_without_writes() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "2.6.1-1.alnx4",
            Ownership::RpmManaged,
        )],
    );
    let state_path = layout.state_dir.join("installed.toml");
    let before = std::fs::read(&state_path).expect("state bytes");
    let host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "2.7.0", Some("1.alnx4")),
    );
    let plan = build_plan(None, &cli_noop(), &[]);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        false,
        true,
        COMMAND,
        &NoopReporter,
    )
    .expect("dry-run reconcile preview");

    assert!(result.dry_run);
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.reconciled.len(), 1);
    assert_eq!(result.reconciled[0].name, "cosh");
    assert_eq!(result.reconciled[0].from, "2.6.1-1.alnx4");
    assert_eq!(result.reconciled[0].to, "2.7.0-1.alnx4");
    let json = serde_json::to_value(&result).expect("serialize result");
    assert_eq!(json["reconciled"][0]["package"], "copilot-shell");
    assert_eq!(json["reconciled"][0]["reason"], "RPM state drift");
    assert!(host.txn_calls().is_empty());
    assert_eq!(std::fs::read(&state_path).expect("state bytes"), before);
    assert!(
        !layout.lock_file.exists(),
        "dry-run must not acquire the install lock"
    );
    let store = StateStore::load(&state_path, 0).expect("store");
    assert!(store.operations.is_empty());
    assert!(find_component(&store, "cosh").last_operation_id.is_none());
}

// ── progress reporting (issue #1452) ─────────────────────────────────────────

/// Records the phase messages emitted during the apply loop so tests can assert
/// the exact sequence without a TTY or animated-frame timing.
#[derive(Default)]
struct RecordingReporter {
    messages: RefCell<Vec<String>>,
}

impl RecordingReporter {
    fn messages(&self) -> Vec<String> {
        self.messages.borrow().clone()
    }
}

impl ProgressReporter for RecordingReporter {
    fn report(&self, message: &str) {
        self.messages.borrow_mut().push(message.to_string());
    }
}

/// A real apply reports each transaction item with a reliable `i/total`, then a
/// finalize phase, in order: CLI → component update → install → finalizing.
#[test]
fn apply_reports_each_phase_with_reliable_counts() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default()
        .with_installed("anolisa", info("anolisa", "1.0.0", Some("1.al4")))
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "1.1.0", Some("1.al4")),
        )
        .with_installed("agent-memory", info("agent-memory", "1.0.0", Some("1.al4")));
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );

    let cli = cli_check(
        ACTION_UPDATE,
        Some("anolisa"),
        Some("0.5.0-1.al4"),
        Some("1.0.0-1.al4"),
        None,
    );
    let components = vec![
        component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
        component_check(
            "agent-memory",
            Some("agent-memory"),
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        ),
    ];
    let plan = build_plan(None, &cli, &components);
    let reporter = RecordingReporter::default();

    let result = run_upgrade_with_deps(
        &ctx, &layout, &plan, &host, &host, true, false, COMMAND, &reporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_OK);
    assert_eq!(
        reporter.messages(),
        vec![
            "Upgrading anolisa (1/3)...".to_string(),
            "Upgrading cosh (2/3)...".to_string(),
            "Installing agent-memory (3/3)...".to_string(),
            "Finalizing ANOLISA state...".to_string(),
        ],
        "each transaction reports a reliable i/total, then finalizing"
    );
}

/// A dry-run plans only; it must not report any apply-phase progress.
#[test]
fn dry_run_reports_no_apply_phase() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = build_plan(
        Some("agentic_os-latest".to_string()),
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );
    let reporter = RecordingReporter::default();

    let result = run_upgrade_with_deps(
        &ctx, &layout, &plan, &host, &host, false, true, COMMAND, &reporter,
    )
    .expect("dry-run");
    assert!(result.dry_run);
    assert!(
        reporter.messages().is_empty(),
        "dry-run must not report an apply phase that never happens"
    );
}

/// A per-item transaction failure still reports the item (its message is emitted
/// before the failing `dnf` call) and still reaches the finalize phase, so the
/// reporter contract holds on the error path.
#[test]
fn apply_reports_phases_even_when_an_item_fails() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "1.1.0", Some("1.al4")),
        )
        .with_installed(
            "agent-sec-core",
            info("agent-sec-core", "1.0.0", Some("1.al4")),
        );
    host.fail_update.insert("agent-sec-core".to_string());
    // Both components are seeded so they are authorized against locked state and
    // both reach the `dnf` layer; only the second one's transaction fails there.
    seed_state(
        &layout,
        vec![
            rpm_component(
                "cosh",
                "copilot-shell",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
            rpm_component(
                "sec-core",
                "agent-sec-core",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
        ],
    );
    let components = vec![
        component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
        component_check(
            "sec-core",
            Some("agent-sec-core"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
    ];
    let plan = build_plan(None, &cli_noop(), &components);
    let reporter = RecordingReporter::default();

    let result = run_upgrade_with_deps(
        &ctx, &layout, &plan, &host, &host, true, false, COMMAND, &reporter,
    )
    .expect("apply");
    assert_eq!(result.status, STATUS_PARTIAL);
    assert_eq!(
        reporter.messages(),
        vec![
            "Upgrading cosh, sec-core (1/1)...".to_string(),
            "Retrying sec-core individually...".to_string(),
            "Finalizing ANOLISA state...".to_string(),
        ],
        "the merged transaction is announced once, the degraded retry names its member, and the finalize phase still runs"
    );
}

/// All authorized component updates share one dnf transaction, so the solver
/// resolves the whole set at once instead of per-item runs.
#[test]
fn merged_update_shares_one_dnf_transaction() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "1.1.0", Some("1.al4")),
        )
        .with_installed(
            "agent-sec-core",
            info("agent-sec-core", "1.1.0", Some("1.al4")),
        );
    seed_state(
        &layout,
        vec![
            rpm_component(
                "cosh",
                "copilot-shell",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
            rpm_component(
                "sec-core",
                "agent-sec-core",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
        ],
    );
    let components = vec![
        component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
        component_check(
            "sec-core",
            Some("agent-sec-core"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
    ];
    let plan = build_plan(None, &cli_noop(), &components);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.updated.len(), 2);
    assert_eq!(
        host.txn_calls(),
        vec!["update:copilot-shell,agent-sec-core".to_string()],
        "both packages must share one dnf update"
    );
}

/// All missing-default installs share one dnf transaction as well.
#[test]
fn merged_install_shares_one_dnf_transaction() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default()
        .with_installed("agent-memory", info("agent-memory", "1.0.0", Some("1.al4")))
        .with_installed("agentsight", info("agentsight", "2.0.0", Some("1.al4")));
    let components = vec![
        component_check(
            "agent-memory",
            Some("agent-memory"),
            None,
            None,
            Some("1.0.0-1.al4"),
            ACTION_INSTALL,
            None,
        ),
        component_check(
            "agentsight",
            Some("agentsight"),
            None,
            None,
            Some("2.0.0-1.al4"),
            ACTION_INSTALL,
            None,
        ),
    ];
    let plan = build_plan(None, &cli_noop(), &components);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.installed.len(), 2);
    assert_eq!(
        host.txn_calls(),
        vec!["install:agent-memory,agentsight".to_string()],
        "both packages must share one dnf install"
    );
}

#[test]
fn single_update_failure_records_moved_rpmdb_truth() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default().with_installed(
        "copilot-shell",
        info("copilot-shell", "1.1.0", Some("1.al4")),
    );
    host.fail_update.insert("copilot-shell".to_string());
    seed_state(
        &layout,
        vec![rpm_component(
            "cosh",
            "copilot-shell",
            "1.0.0-1.al4",
            Ownership::RpmManaged,
        )],
    );
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.updated.len(), 1);
    assert!(result.errors.is_empty());
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("despite the transaction failure"))
    );
}

#[test]
fn single_install_failure_records_landed_rpmdb_truth() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default()
        .with_installed("agentsight", info("agentsight", "2.0.0", Some("1.al4")));
    host.fail_install.insert("agentsight".to_string());
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "agentsight",
            Some("agentsight"),
            None,
            None,
            Some("2.0.0-1.al4"),
            ACTION_INSTALL,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.status, STATUS_OK);
    assert_eq!(result.installed.len(), 1);
    assert!(result.errors.is_empty());
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("despite the transaction failure"))
    );
}

#[test]
fn single_install_failure_reports_repair_when_rpmdb_recheck_fails() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default();
    host.fail_install.insert("agentsight".to_string());
    host.fail_query.insert("agentsight".to_string());
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "agentsight",
            Some("agentsight"),
            None,
            None,
            Some("2.0.0-1.al4"),
            ACTION_INSTALL,
            None,
        )],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.status, STATUS_FAILED);
    assert!(result.installed.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert!(
        result.errors[0]
            .reason
            .contains("sudo anolisa --install-mode system repair agentsight")
    );
    assert_eq!(host.query_calls(), vec!["agentsight", "agentsight"]);
}

/// A merged update failure isolates the offender: the member whose slot
/// provably did not move retries alone and lands, the offender fails with
/// its own diagnostic, and nothing is retried twice.
#[test]
fn merged_update_failure_retries_clean_members_individually() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    // Both packages sit at their pre-upgrade EVR, so after the merged
    // failure both slots read as clean and both retry; only copilot-shell's
    // own transaction keeps failing.
    let mut host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "1.0.0", Some("1.al4")),
        )
        .with_installed(
            "agent-sec-core",
            info("agent-sec-core", "1.0.0", Some("1.al4")),
        );
    host.fail_update.insert("copilot-shell".to_string());
    seed_state(
        &layout,
        vec![
            rpm_component(
                "cosh",
                "copilot-shell",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
            rpm_component(
                "sec-core",
                "agent-sec-core",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
        ],
    );
    let components = vec![
        component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
        component_check(
            "sec-core",
            Some("agent-sec-core"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
    ];
    let plan = build_plan(None, &cli_noop(), &components);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.status, STATUS_PARTIAL);
    assert_eq!(
        host.txn_calls(),
        vec![
            "update:copilot-shell,agent-sec-core".to_string(),
            "update:copilot-shell".to_string(),
            "update:agent-sec-core".to_string(),
        ],
        "one merged attempt, then one isolated retry per clean member"
    );
    assert_eq!(result.updated.len(), 1);
    assert_eq!(result.updated[0].name, "sec-core");
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, "cosh");
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("merged RPM update failed")),
        "the degrade must be announced as a warning: {:?}",
        result.warnings
    );
}

#[test]
fn merged_update_failure_does_not_retry_an_absent_member() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default().with_installed(
        "agent-sec-core",
        info("agent-sec-core", "1.0.0", Some("1.al4")),
    );
    host.missing_after.insert("copilot-shell".to_string());
    host.fail_update.insert("copilot-shell".to_string());
    seed_state(
        &layout,
        vec![
            rpm_component(
                "cosh",
                "copilot-shell",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
            rpm_component(
                "sec-core",
                "agent-sec-core",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
        ],
    );
    let components = vec![
        component_check(
            "cosh",
            Some("copilot-shell"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
        component_check(
            "sec-core",
            Some("agent-sec-core"),
            Some("rpm-managed"),
            Some("1.0.0-1.al4"),
            Some("1.1.0-1.al4"),
            ACTION_UPDATE,
            None,
        ),
    ];
    let plan = build_plan(None, &cli_noop(), &components);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.status, STATUS_PARTIAL);
    assert_eq!(
        host.txn_calls(),
        vec![
            "update:copilot-shell,agent-sec-core".to_string(),
            "update:agent-sec-core".to_string(),
        ],
        "an absent member must not receive a second native update"
    );
    assert_eq!(result.updated.len(), 1);
    assert_eq!(result.updated[0].name, "sec-core");
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, "cosh");
    assert!(
        result.errors[0]
            .reason
            .contains("sudo anolisa --install-mode system repair cosh")
    );
}

#[test]
fn failed_isolated_update_retry_records_late_rpmdb_movement() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let mut host = FakeHost::default()
        .with_installed(
            "copilot-shell",
            info("copilot-shell", "1.1.0", Some("1.al4")),
        )
        .with_installed(
            "agent-sec-core",
            info("agent-sec-core", "1.0.0", Some("1.al4")),
        )
        .with_query_sequence(
            "copilot-shell",
            vec![
                Some(info("copilot-shell", "1.0.0", Some("1.al4"))),
                Some(info("copilot-shell", "1.0.0", Some("1.al4"))),
                Some(info("copilot-shell", "1.1.0", Some("1.al4"))),
            ],
        );
    host.fail_update.insert("copilot-shell".to_string());
    seed_state(
        &layout,
        vec![
            rpm_component(
                "cosh",
                "copilot-shell",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
            rpm_component(
                "sec-core",
                "agent-sec-core",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
            ),
        ],
    );
    let plan = build_plan(
        None,
        &cli_noop(),
        &[
            component_check(
                "cosh",
                Some("copilot-shell"),
                Some("rpm-managed"),
                Some("1.0.0-1.al4"),
                Some("1.1.0-1.al4"),
                ACTION_UPDATE,
                None,
            ),
            component_check(
                "sec-core",
                Some("agent-sec-core"),
                Some("rpm-managed"),
                Some("1.0.0-1.al4"),
                Some("1.1.0-1.al4"),
                ACTION_UPDATE,
                None,
            ),
        ],
    );

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.updated.len(), 2);
    assert!(result.errors.is_empty());
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("despite the retry failure"))
    );
}

/// A merged install failure records members that landed anyway from rpmdb
/// truth (forward-only) and retries only the provably clean ones.
#[test]
fn merged_install_failure_records_landed_member_from_rpmdb_truth() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    // agent-memory reached the rpmdb despite the failed merged transaction;
    // agentsight stayed absent for good (its retry then reads back nothing).
    let mut host = FakeHost::default()
        .with_installed("agent-memory", info("agent-memory", "1.0.0", Some("1.al4")));
    host.fail_install.insert("agent-memory".to_string());
    let components = vec![
        component_check(
            "agent-memory",
            Some("agent-memory"),
            None,
            None,
            Some("1.0.0-1.al4"),
            ACTION_INSTALL,
            None,
        ),
        component_check(
            "agentsight",
            Some("agentsight"),
            None,
            None,
            Some("2.0.0-1.al4"),
            ACTION_INSTALL,
            None,
        ),
    ];
    let plan = build_plan(None, &cli_noop(), &components);

    let result = run_upgrade_with_deps(
        &ctx,
        &layout,
        &plan,
        &host,
        &host,
        true,
        false,
        COMMAND,
        &NoopReporter,
    )
    .expect("apply");

    assert_eq!(result.status, STATUS_PARTIAL);
    assert_eq!(
        host.txn_calls(),
        vec![
            "install:agent-memory,agentsight".to_string(),
            "install:agentsight".to_string(),
        ],
        "the landed member is never retried; only the clean one is"
    );
    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].name, "agent-memory");
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("'agent-memory' was installed despite")),
        "recording rpmdb truth must be announced: {:?}",
        result.warnings
    );
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, "agentsight");
    assert!(
        result.errors[0].reason.contains("not present in rpmdb"),
        "got: {}",
        result.errors[0].reason
    );
}

/// A blocked plan (planning error) never reaches the apply loop, so no phase is
/// reported — real execution aborts before any `dnf` transaction.
#[test]
fn blocked_plan_reports_no_apply_phase() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let ctx = system_ctx(tmp.path().to_path_buf());
    let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
    let host = FakeHost::default();
    let plan = build_plan(
        None,
        &cli_noop(),
        &[component_check(
            "mystery",
            None,
            None,
            None,
            None,
            ACTION_INSTALL,
            None,
        )],
    );
    assert!(plan.has_errors());
    let reporter = RecordingReporter::default();

    let result = run_upgrade_with_deps(
        &ctx, &layout, &plan, &host, &host, true, false, COMMAND, &reporter,
    )
    .expect("blocked plan renders");
    assert_eq!(result.status, STATUS_BLOCKED);
    assert!(
        reporter.messages().is_empty(),
        "a blocked plan must not report an apply phase"
    );
}

#[test]
fn confirmed_repository_is_recorded_only_for_successful_native_changes() {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let install = PlannedInstall {
        name: "cosh".into(),
        package: "copilot-shell".into(),
    };
    let host = FakeHost::default()
        .with_installed("copilot-shell", info("copilot-shell", "2.0", Some("1")));
    let mut installs = Vec::new();
    stage_refreshed_install(
        &install,
        &host,
        &Ok(None),
        Some("configured"),
        &mut installs,
        &mut errors,
        &mut warnings,
    );
    assert_eq!(
        observation_from(
            &installs[0].refreshed,
            installs[0].source_repo.as_deref(),
            "now"
        )
        .source_repo
        .as_deref(),
        Some("configured"),
    );
    reconcile_failed_install(
        &install,
        "partial failure",
        "transaction",
        &host,
        &mut installs,
        &mut errors,
        &mut warnings,
    );
    assert_eq!(
        observation_from(
            &installs[1].refreshed,
            installs[1].source_repo.as_deref(),
            "now"
        )
        .source_repo,
        None,
    );
    for (from, expected) in [("1.0-1", Some("configured")), ("2.0-1", None)] {
        let update = PlannedUpdate {
            name: "cosh".into(),
            package: "copilot-shell".into(),
            from: from.into(),
            to: "2.0-1".into(),
            adopt_if_missing: false,
            backfill_rpm_metadata: false,
        };
        let mut updates = Vec::new();
        stage_refreshed_update(
            &update,
            &host,
            &Ok(Some(info(
                "copilot-shell",
                from.split_once('-').unwrap().0,
                Some("1"),
            ))),
            Some("configured"),
            &mut updates,
            &mut errors,
            &mut warnings,
        );
        assert_eq!(
            observation_from(
                &updates[0].refreshed,
                updates[0].source_repo.as_deref(),
                "now"
            )
            .source_repo
            .as_deref(),
            expected,
        );
    }
    assert!(errors.is_empty());
}
