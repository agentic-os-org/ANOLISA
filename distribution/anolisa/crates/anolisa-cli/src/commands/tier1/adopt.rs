//! `anolisa adopt <component>` — take an already-installed system RPM under
//! ANOLISA tracking as a delegated-adopted record.
//!
//! The command handler maps CLI input into the lifecycle application layer
//! and renders its typed outcome. Adoption fetches nothing and runs no
//! `dnf`/`rpm` transaction.

mod application;

use clap::Parser;
use serde::Serialize;

use anolisa_core::execution::ExecutionIntent;
use anolisa_core::planner::{NoOpReason, Step};
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::rpm_query::RpmPackageQuery;

use crate::context::CliContext;
use crate::response::{CliError, render_json};

use self::application::{AdoptOutcome, AdoptRequest};

/// Command label for JSON envelopes and error routing.
const COMMAND: &str = "adopt";

/// Arguments for `anolisa adopt <component>`.
#[derive(Debug, Parser)]
pub struct AdoptArgs {
    /// Component to record as an existing system RPM
    #[arg(value_name = "COMPONENT")]
    pub component: String,
    /// Pin the RPM package name when the component maps to several candidates
    #[arg(long, value_name = "NAME")]
    pub package: Option<String>,
}

/// Dispatch `adopt <component>` against the live host.
pub fn handle(args: AdoptArgs, ctx: &CliContext) -> Result<(), CliError> {
    let query = RpmPackageQuery::system();
    adopt_with_query(&args.component, args.package.as_deref(), ctx, &query)
}

/// Core of [`handle`] with the package query injected so tests avoid the live rpmdb.
pub(crate) fn adopt_with_query(
    target: &str,
    cli_override: Option<&str>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
) -> Result<(), CliError> {
    let intent = if ctx.dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    };
    let outcome = application::run(
        AdoptRequest {
            target,
            package: cli_override,
            intent,
        },
        ctx,
        query,
    )?;
    render_outcome(ctx, outcome)
}

fn render_outcome(ctx: &CliContext, outcome: AdoptOutcome) -> Result<(), CliError> {
    let payload = match outcome {
        AdoptOutcome::NoOp { subject, reason } => {
            debug_assert_eq!(reason, NoOpReason::AlreadyAdopted);
            AdoptResultPayload {
                component: subject.component,
                package: subject.package,
                version: subject.version,
                action: "already-adopted",
                operation_id: None,
                dry_run: ctx.dry_run,
                plan: Vec::new(),
            }
        }
        AdoptOutcome::Preview { subject, steps } => AdoptResultPayload {
            component: subject.component,
            package: subject.package,
            version: subject.version,
            action: "planned",
            operation_id: None,
            dry_run: true,
            plan: steps.iter().map(step_label).collect(),
        },
        AdoptOutcome::Applied {
            subject,
            steps,
            outcome,
        } => {
            for warning in outcome.warnings() {
                eprintln!("warning: {warning}");
            }
            AdoptResultPayload {
                component: subject.component,
                package: subject.package,
                version: subject.version,
                action: "adopted",
                operation_id: outcome.operation_id().map(str::to_string),
                dry_run: false,
                plan: steps.iter().map(step_label).collect(),
            }
        }
    };
    render_result(ctx, &payload)
}

/// Human-facing label for a plan step (preview rendering). Adopt plans only
/// carry observe/record steps; anything else falls back to its debug form.
fn step_label(step: &Step) -> String {
    match step {
        Step::Observe { packages } => format!("observe {}", packages.join(" ")),
        Step::WriteRecord(write) => format!("record: {}", write.label()),
        other => format!("{other:?}"),
    }
}

/// JSON payload for a completed (or previewed, or idempotent) adopt.
#[derive(Debug, Serialize)]
struct AdoptResultPayload {
    component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// `adopted` | `planned` (dry-run) | `already-adopted`.
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    dry_run: bool,
    plan: Vec<String>,
}

fn render_result(ctx: &CliContext, payload: &AdoptResultPayload) -> Result<(), CliError> {
    if ctx.json {
        return render_json(COMMAND, payload);
    }
    if ctx.quiet {
        return Ok(());
    }
    if payload.dry_run {
        println!("adopt {} (dry-run):", payload.component);
        for label in &payload.plan {
            println!("  - {label}");
        }
        return Ok(());
    }
    match (payload.action, &payload.version) {
        ("already-adopted", Some(version)) => {
            println!("{} {version} is already adopted", payload.component);
        }
        ("already-adopted", None) => println!("{} is already adopted", payload.component),
        (_, Some(version)) => println!("adopted {} {version}", payload.component),
        (_, None) => println!("adopted {}", payload.component),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use self::application::{AdoptChange, AdoptShape, adopt_authorized};
    use super::*;
    use crate::commands::common;
    use crate::commands::tier1::rpm_install;
    use crate::context::InstallMode;

    use std::cell::Cell;
    use std::path::PathBuf;

    use anolisa_core::domain::{
        InstallationScope, ManagementRelation, NativePm, PackageIdentity, ProviderBinding,
    };
    use anolisa_core::state::{
        InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind, ObjectStatus,
        Ownership, RpmMetadata,
    };
    use anolisa_core::state_store::StateStore;
    use anolisa_core::transaction::{Transaction, TransactionOutcomeStatus};
    use anolisa_platform::pkg_query::{PackageInfo, PackageQueryError, PackageVersion};

    /// In-memory [`PackageQuery`] for the adopt tests. Adopt runs no
    /// transaction, so a query alone drives every branch (candidate chain +
    /// probe + origin lookup).
    #[derive(Default)]
    struct FakeQuery {
        /// package name → installed info reported by `query_installed`.
        installed: Vec<(String, PackageInfo)>,
        /// package names that report several installed versions.
        multi_version: Vec<String>,
        /// capability → provider package names for `what_provides_installed`.
        provides: Vec<(String, Vec<String>)>,
        /// capability → provider package names for `what_provides_available`.
        available_provides: Vec<(String, Vec<String>)>,
        /// package → declared provides capabilities.
        package_provides: Vec<(String, Vec<String>)>,
        /// package → source repo for `installed_origin`.
        origins: Vec<(String, String)>,
        calls: Cell<usize>,
    }

    impl PackageQuery for FakeQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            self.calls.set(self.calls.get() + 1);
            if self.multi_version.iter().any(|p| p == package) {
                return Err(PackageQueryError::UnexpectedOutput {
                    command: "rpm".to_string(),
                    detail: "2 installed versions".to_string(),
                });
            }
            Ok(self
                .installed
                .iter()
                .find(|(p, _)| p == package)
                .map(|(_, info)| info.clone()))
        }
        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            self.calls.set(self.calls.get() + 1);
            Ok(Vec::new())
        }
        fn installed_origin(&self, package: &str) -> Result<Option<String>, PackageQueryError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self
                .origins
                .iter()
                .find(|(p, _)| p == package)
                .map(|(_, o)| o.clone()))
        }
        fn what_provides_installed(
            &self,
            capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self
                .provides
                .iter()
                .find(|(c, _)| c == capability)
                .map(|(_, v)| v.clone())
                .unwrap_or_default())
        }
        fn what_provides_available(
            &self,
            capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self
                .available_provides
                .iter()
                .find(|(c, _)| c == capability)
                .map(|(_, v)| v.clone())
                .unwrap_or_default())
        }
        fn provided_capabilities_installed(
            &self,
            package: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self
                .package_provides
                .iter()
                .find(|(p, _)| p == package)
                .map(|(_, v)| v.clone())
                .unwrap_or_default())
        }
    }

    struct DisappearingQuery {
        installed: PackageInfo,
        calls: Cell<usize>,
    }

    impl PackageQuery for DisappearingQuery {
        fn query_installed(
            &self,
            _package: &str,
        ) -> Result<Option<PackageInfo>, PackageQueryError> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            Ok((call == 0).then(|| self.installed.clone()))
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }

        fn installed_origin(&self, _package: &str) -> Result<Option<String>, PackageQueryError> {
            Ok(None)
        }

        fn what_provides_installed(
            &self,
            _capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            Ok(Vec::new())
        }

        fn what_provides_available(
            &self,
            _capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            Ok(Vec::new())
        }

        fn provided_capabilities_installed(
            &self,
            _package: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            Ok(Vec::new())
        }
    }

    fn pkg_info(name: &str, version: &str, release: Option<&str>, arch: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: PackageVersion {
                epoch: None,
                version: version.to_string(),
                release: release.map(str::to_string),
            },
            arch: arch.to_string(),
            origin: None,
        }
    }

    fn component_provider(component: &str, package: &str) -> (String, Vec<String>) {
        (
            format!("anolisa-component({component})"),
            vec![package.to_string()],
        )
    }

    fn ctx(prefix: PathBuf, install_mode: InstallMode, dry_run: bool) -> CliContext {
        // Identity resolution consults the component index for fresh adopt
        // targets; a seeded local index keeps fixture names supported. The
        // user-mode refusal test asserts an untouched prefix, so only the
        // system-mode fixtures seed it.
        if install_mode == InstallMode::System {
            crate::commands::tier1::install::tests::seed_repo_config_with_index(
                &anolisa_platform::fs_layout::FsLayout::system(Some(prefix.clone())),
                crate::commands::tier1::install::tests::TEST_INDEX_COMPONENTS,
            );
        }
        crate::test_support::context_for_root(
            &prefix,
            install_mode,
            Some(prefix.clone()),
            crate::test_support::TestContextOptions {
                dry_run,
                ..Default::default()
            },
        )
    }

    /// A tracked component object with the given provenance, as legacy v4
    /// state; loading it exercises the migration into the v5 store. `adopted`
    /// splits `RpmObserved` into the Adopted vs Observed relations.
    fn component_object(name: &str, ownership: Ownership, adopted: bool) -> InstalledObject {
        let is_rpm = matches!(ownership, Ownership::RpmManaged | Ownership::RpmObserved);
        InstalledObject {
            kind: ObjectKind::Component,
            name: name.to_string(),
            version: "1.0.0-1.al8".to_string(),
            status: if adopted {
                ObjectStatus::Adopted
            } else {
                ObjectStatus::Installed
            },
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some(if is_rpm { "rpm" } else { "raw" }.to_string()),
            ownership: Some(ownership),
            rpm_metadata: is_rpm.then(|| RpmMetadata {
                package_name: name.to_string(),
                evr: Some("1.0.0-1.al8".to_string()),
                arch: Some("x86_64".to_string()),
                source_repo: Some("@System".to_string()),
            }),
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: Some("op-prior".to_string()),
            managed: matches!(ownership, Ownership::RawManaged | Ownership::RpmManaged),
            adopted,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: Vec::new(),
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        }
    }

    /// Write a seed state (creating the state dir) so the lock-held write
    /// path has somewhere to land.
    fn seed(ctx: &CliContext, objs: Vec<InstalledObject>) {
        let layout = common::resolve_layout(ctx);
        std::fs::create_dir_all(&layout.state_dir).expect("mkdir state");
        let mut state = InstalledState {
            install_mode: StateInstallMode::System,
            prefix: layout.prefix.clone(),
            ..Default::default()
        };
        for obj in objs {
            state.objects.push(obj);
        }
        crate::test_support::write_legacy_state(&state, &layout.state_dir.join("installed.toml"))
            .expect("seed state");
    }

    fn load_store(ctx: &CliContext) -> StateStore {
        let layout = common::resolve_layout(ctx);
        StateStore::load(&layout.state_dir.join("installed.toml"), 0).expect("load store")
    }

    /// The delegated binding pieces of a recorded component, for assertions.
    fn delegated_parts(
        store: &StateStore,
        name: &str,
    ) -> (String, ManagementRelation, Option<String>) {
        let installation = store
            .find(ObjectKind::Component, name)
            .expect("component recorded");
        match &installation.binding {
            ProviderBinding::Delegated {
                package,
                relation,
                last_observed,
                ..
            } => (
                package.resolved_name().unwrap_or_default().to_string(),
                relation.clone(),
                last_observed.as_ref().and_then(|o| o.evr.clone()),
            ),
            other => panic!("expected a delegated binding, got {other:?}"),
        }
    }

    /// A unique installed RPM with no prior state is recorded as
    /// delegated-adopted with a fresh observation (A1).
    #[test]
    fn adopt_records_unique_rpm_as_adopted() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&c, Vec::new());
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64"),
            )],
            provides: vec![component_provider("copilot-shell", "copilot-shell")],
            origins: vec![("copilot-shell".to_string(), "@System".to_string())],
            ..Default::default()
        };
        let application_outcome = application::run(
            AdoptRequest {
                target: "copilot-shell",
                package: None,
                intent: ExecutionIntent::Apply,
            },
            &c,
            &q,
        )
        .expect("adopt ok");
        let AdoptOutcome::Applied {
            subject,
            steps,
            outcome,
        } = application_outcome
        else {
            panic!("fresh adopt must return an applied outcome");
        };
        assert_eq!(subject.component, "copilot-shell");
        assert_eq!(subject.package.as_deref(), Some("copilot-shell"));
        assert_eq!(subject.version.as_deref(), Some("2.2.0"));
        assert!(!steps.is_empty());
        assert_eq!(
            outcome.status(),
            &anolisa_core::execution::CommandOutcomeStatus::Completed
        );
        assert!(outcome.operation_id().is_some());
        assert_eq!(outcome.changes(), &[AdoptChange::RecordCreated]);

        let store = load_store(&c);
        let (package, relation, evr) = delegated_parts(&store, "copilot-shell");
        assert_eq!(package, "copilot-shell");
        assert!(
            matches!(relation, ManagementRelation::Adopted { .. }),
            "adopt records the adopted relation, got {relation:?}",
        );
        assert_eq!(evr.as_deref(), Some("2.2.0-1.al8"), "fresh observation");
        assert!(
            store
                .operations
                .iter()
                .any(|o| o.command == "adopt copilot-shell"),
            "an operation record must be appended",
        );
    }

    /// Re-adopting an observed component upgrades the management relation in
    /// place (A6) without refreshing the cached observation — install/adopt
    /// never refresh EVR implicitly; that is repair's job.
    #[test]
    fn adopt_upgrades_observed_record_to_adopted() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![component_object(
                "copilot-shell",
                Ownership::RpmObserved,
                false,
            )],
        );
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.0.0", Some("1.al8"), "x86_64"),
            )],
            ..Default::default()
        };
        adopt_with_query("copilot-shell", None, &c, &q).expect("upgrade ok");

        let store = load_store(&c);
        let (package, relation, evr) = delegated_parts(&store, "copilot-shell");
        assert_eq!(package, "copilot-shell");
        assert!(
            matches!(relation, ManagementRelation::Adopted { .. }),
            "observed must upgrade to adopted, got {relation:?}",
        );
        assert_eq!(
            evr.as_deref(),
            Some("1.0.0-1.al8"),
            "the cached observation is preserved, not refreshed",
        );
    }

    #[test]
    fn adopt_rejects_observed_package_removed_before_locked_execution() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![component_object(
                "copilot-shell",
                Ownership::RpmObserved,
                false,
            )],
        );
        let query = DisappearingQuery {
            installed: pkg_info("copilot-shell", "2.0.0", Some("1.al8"), "x86_64"),
            calls: Cell::new(0),
        };

        let result = adopt_with_query("copilot-shell", None, &c, &query);

        result.expect_err("the locked observation must reject the vanished package");
        let store = load_store(&c);
        let (_, relation, _) = delegated_parts(&store, "copilot-shell");
        assert_eq!(relation, ManagementRelation::Observed);
        assert_eq!(query.calls.get(), 2);
    }

    /// Re-adopting an already-adopted component is a NoOp (A7): nothing is
    /// rewritten, no operation is recorded.
    #[test]
    fn adopt_already_adopted_is_a_noop() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![component_object(
                "copilot-shell",
                Ownership::RpmObserved,
                true,
            )],
        );
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "9.9.9", Some("1.al8"), "x86_64"),
            )],
            ..Default::default()
        };
        let outcome = application::run(
            AdoptRequest {
                target: "copilot-shell",
                package: None,
                intent: ExecutionIntent::Apply,
            },
            &c,
            &q,
        )
        .expect("noop ok");
        assert!(matches!(
            outcome,
            AdoptOutcome::NoOp {
                reason: NoOpReason::AlreadyAdopted,
                ..
            }
        ));

        let store = load_store(&c);
        let (_, relation, evr) = delegated_parts(&store, "copilot-shell");
        assert!(matches!(relation, ManagementRelation::Adopted { .. }));
        assert_eq!(
            evr.as_deref(),
            Some("1.0.0-1.al8"),
            "a NoOp must not refresh the observation",
        );
        assert!(
            store.operations.is_empty(),
            "a NoOp must not append an operation record",
        );
    }

    /// Re-adopting a tracked component with `--package` pointing at a
    /// *different* RPM is a package-identity migration, not a refresh: it is
    /// refused up front and steers the user through forget→adopt.
    #[test]
    fn adopt_refuses_repointing_observed_to_different_package() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![component_object(
                "copilot-shell",
                Ownership::RpmObserved,
                true,
            )],
        );
        let q = FakeQuery {
            installed: vec![(
                "anolisa-other".to_string(),
                pkg_info("anolisa-other", "9.9.9", Some("1.al8"), "x86_64"),
            )],
            provides: vec![component_provider("copilot-shell", "anolisa-other")],
            ..Default::default()
        };
        let err = adopt_with_query("copilot-shell", Some("anolisa-other"), &c, &q)
            .expect_err("repointing to a different package must be refused");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("copilot-shell")
                && err.reason().contains("anolisa-other")
                && err.reason().contains("forget"),
            "refusal must name both packages and point at forget: {}",
            err.reason(),
        );
        // The state must be untouched — no repoint, no EVR bump.
        let store = load_store(&c);
        let (package, _, evr) = delegated_parts(&store, "copilot-shell");
        assert_eq!(
            package, "copilot-shell",
            "package identity must be preserved when the repoint is refused",
        );
        assert_eq!(evr.as_deref(), Some("1.0.0-1.al8"), "EVR unchanged");
    }

    /// The repoint refusal must also fire on `--dry-run`: the preview cannot
    /// promise a plan the real run would reject.
    #[test]
    fn adopt_dry_run_refuses_repointing_observed_to_different_package() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        seed(
            &c,
            vec![component_object(
                "copilot-shell",
                Ownership::RpmObserved,
                true,
            )],
        );
        let q = FakeQuery {
            installed: vec![(
                "anolisa-other".to_string(),
                pkg_info("anolisa-other", "9.9.9", Some("1.al8"), "x86_64"),
            )],
            provides: vec![component_provider("copilot-shell", "anolisa-other")],
            ..Default::default()
        };
        let err = adopt_with_query("copilot-shell", Some("anolisa-other"), &c, &q)
            .expect_err("dry-run must refuse the repoint, matching the real run");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("copilot-shell")
                && err.reason().contains("anolisa-other")
                && err.reason().contains("forget"),
            "dry-run refusal must match the real run: {}",
            err.reason(),
        );
        let store = load_store(&c);
        let (package, _, _) = delegated_parts(&store, "copilot-shell");
        assert_eq!(package, "copilot-shell");
    }

    /// A raw-managed component is not silently converted; adopt points at
    /// uninstall (A4).
    #[test]
    fn adopt_refuses_raw_managed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![component_object(
                "copilot-shell",
                Ownership::RawManaged,
                false,
            )],
        );
        let err = adopt_with_query("copilot-shell", None, &c, &FakeQuery::default())
            .expect_err("raw must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("uninstall"),
            "raw refusal points at uninstall: {}",
            err.reason()
        );
    }

    /// An rpm-managed component is refused; adopt points at repair (A5).
    #[test]
    fn adopt_refuses_rpm_managed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![component_object(
                "copilot-shell",
                Ownership::RpmManaged,
                false,
            )],
        );
        let err = adopt_with_query("copilot-shell", None, &c, &FakeQuery::default())
            .expect_err("rpm-managed must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("repair"),
            "rpm-managed refusal points at repair: {}",
            err.reason()
        );
    }

    /// A tracked component whose package left rpmdb points at forget.
    #[test]
    fn adopt_of_tracked_but_absent_points_at_forget() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![component_object(
                "copilot-shell",
                Ownership::RpmObserved,
                false,
            )],
        );
        // Query reports nothing installed: the observed package is gone.
        let err = adopt_with_query("copilot-shell", None, &c, &FakeQuery::default())
            .expect_err("tracked-but-absent must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("forget"),
            "absence of a tracked package points at forget: {}",
            err.reason()
        );
    }

    /// Adoption is system-scope; user mode is refused by the planner.
    #[test]
    fn adopt_refuses_in_user_mode() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::User, false);
        let query = FakeQuery::default();
        let err = adopt_with_query("copilot-shell", None, &c, &query)
            .expect_err("user mode must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("system"),
            "user-mode refusal mentions system scope: {}",
            err.reason()
        );
        assert_eq!(query.calls.get(), 0, "user refusal must not query rpmdb");
        assert_eq!(
            std::fs::read_dir(tmp.path()).expect("sandbox root").count(),
            0,
            "user refusal must not create filesystem state"
        );
    }

    /// No installed RPM under the name: adopt does not install, points at
    /// install (A2).
    #[test]
    fn adopt_refuses_absent_package() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let q = FakeQuery {
            available_provides: vec![component_provider("copilot-shell", "copilot-shell")],
            ..Default::default()
        };
        let err = adopt_with_query("copilot-shell", None, &c, &q)
            .expect_err("absent package must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("install copilot-shell"),
            "absent refusal points at the install command: {}",
            err.reason()
        );
    }

    /// Multiple provider packages cannot be adopted unambiguously.
    #[test]
    fn adopt_refuses_ambiguous_candidates() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let q = FakeQuery {
            provides: vec![(
                "anolisa-component(copilot-shell)".to_string(),
                vec!["pkg-a".to_string(), "pkg-b".to_string()],
            )],
            ..Default::default()
        };
        let err =
            adopt_with_query("copilot-shell", None, &c, &q).expect_err("ambiguous must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("--package"),
            "ambiguous refusal points at --package: {}",
            err.reason()
        );
    }

    /// A same-name multi-version rpmdb is refused rather than adopted
    /// blindly (A3).
    #[test]
    fn adopt_refuses_multi_version() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64"),
            )],
            available_provides: vec![component_provider("copilot-shell", "copilot-shell")],
            multi_version: vec!["copilot-shell".to_string()],
            ..Default::default()
        };
        let err = adopt_with_query("copilot-shell", None, &c, &q)
            .expect_err("multi-version must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("multiple installed versions"));
    }

    /// `--dry-run` previews without writing any state.
    #[test]
    fn adopt_dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64"),
            )],
            provides: vec![component_provider("copilot-shell", "copilot-shell")],
            ..Default::default()
        };
        let outcome = application::run(
            AdoptRequest {
                target: "copilot-shell",
                package: None,
                intent: ExecutionIntent::Plan,
            },
            &c,
            &q,
        )
        .expect("dry-run ok");
        assert!(matches!(outcome, AdoptOutcome::Preview { ref steps, .. } if !steps.is_empty()));
        let layout = common::resolve_layout(&c);
        assert!(
            !layout.state_dir.join("installed.toml").exists(),
            "dry-run must not write state",
        );
    }

    /// A pending operation journal blocks adopt before any rpmdb resolution.
    #[test]
    fn adopt_refuses_pending_rpm_install_claim() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&c);
        rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
            .expect("begin pending install");
        let q = FakeQuery::default();

        let err = adopt_with_query("cosh", Some("copilot-shell"), &c, &q)
            .expect_err("adopt must not bypass a pending managed install");
        assert!(err.reason().contains("repair cosh"));
    }

    /// Historical evidence that is not an exact component identity must not
    /// authorize an adopt once the component index cannot vouch for the name
    /// (issue #2630): a terminal journal left behind by a finished operation
    /// and a dropped legacy capability row both fail identity validation
    /// before the candidate chain runs, even though a `package_map` entry or
    /// rpmdb Provides metadata could still resolve a package — otherwise a
    /// fresh adopt would mint a component record the index never authorized.
    #[test]
    fn historical_state_evidence_does_not_authorize_adopt_identity() {
        for fixture in ["terminal-journal", "legacy-capability"] {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
            let layout = common::resolve_layout(&c);
            let state_path = layout.state_dir.join("installed.toml");
            std::fs::create_dir_all(&layout.state_dir).expect("state dir");
            // Bait for both fixtures: rpmdb Provides and an installed package
            // could back the name if the candidate chain were ever consulted.
            let q = FakeQuery {
                installed: vec![(
                    "ghost-pkg".to_string(),
                    pkg_info("ghost-pkg", "1.0.0", Some("1.al8"), "x86_64"),
                )],
                provides: vec![component_provider("ghost", "ghost-pkg")],
                ..Default::default()
            };
            let (expected_code, expected_reason) = match fixture {
                "terminal-journal" => {
                    // The seeded index stays available but does not know
                    // "ghost"; a package_map entry additionally baits the
                    // candidate chain.
                    std::fs::write(
                        layout.etc_dir.join("repo.toml"),
                        format!(
                            "schema_version = 1\ndefault_backend = \"raw\"\n\n\
                             [backends.raw]\nbase_url = \"file://{}\"\n\n\
                             [backends.rpm]\nbase_url = \"https://repo.example/anolisa\"\n\n\
                             [backends.rpm.package_map]\nghost = \"ghost-pkg\"\n",
                            layout.etc_dir.join("test-index-repo").join("v1").display()
                        ),
                    )
                    .expect("write repo.toml");
                    let mut journal = Transaction::begin_with_subject(
                        "install",
                        Some("ghost"),
                        state_path.clone(),
                        &rpm_install::journal_dir(&layout),
                    )
                    .expect("begin subject journal");
                    journal
                        .finish(TransactionOutcomeStatus::Failed)
                        .expect("finish journal");
                    ("INVALID_ARGUMENT", "unsupported component 'ghost'")
                }
                _ => {
                    // No repository publishes an index, so nothing can
                    // validate the name; the dropped capability row must not
                    // stand in for that validation.
                    std::fs::write(
                        layout.etc_dir.join("repo.toml"),
                        format!(
                            "schema_version = 1\ndefault_backend = \"raw\"\n\n\
                             [backends.raw]\nbase_url = \"file://{}\"\n",
                            tmp.path().join("no-such-repo/v1").display()
                        ),
                    )
                    .expect("write repo.toml");
                    seed(
                        &c,
                        vec![InstalledObject {
                            kind: ObjectKind::Capability,
                            name: "ghost".to_string(),
                            version: "0.1.0".to_string(),
                            status: ObjectStatus::Installed,
                            manifest_digest: None,
                            distribution_source: None,
                            raw_package: None,
                            install_backend: None,
                            ownership: None,
                            rpm_metadata: None,
                            installed_at: "2026-06-01T10:00:00Z".to_string(),
                            last_operation_id: None,
                            managed: true,
                            adopted: false,
                            subscription_scope: Default::default(),
                            enabled_features: Vec::new(),
                            component_refs: Vec::new(),
                            files: Vec::new(),
                            external_modified_files: Vec::new(),
                            services: Vec::new(),
                            health: Vec::new(),
                            provisioned_packages: Vec::new(),
                        }],
                    );
                    ("EXECUTION_FAILED", "component index is unavailable")
                }
            };

            let err = adopt_with_query("ghost", None, &c, &q)
                .expect_err("historical evidence must not authorize the adopt identity");
            assert_eq!(err.code(), expected_code, "fixture: {fixture}");
            assert!(
                err.reason().contains(expected_reason),
                "identity validation must reject the name (fixture: {fixture}): {}",
                err.reason()
            );
            assert_eq!(
                q.calls.get(),
                0,
                "the refusal must precede any rpmdb resolution (fixture: {fixture})"
            );
            let store = StateStore::load(&state_path, 0).expect("load state");
            assert!(
                store.find(ObjectKind::Component, "ghost").is_none(),
                "no component record may be written (fixture: {fixture})"
            );
        }
    }

    /// `--package` pins the RPM name, bypassing the candidate chain.
    #[test]
    fn adopt_with_package_override_adopts_named() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&c, Vec::new());
        let q = FakeQuery {
            installed: vec![(
                "custom-pkg".to_string(),
                pkg_info("custom-pkg", "3.0.0", Some("1"), "x86_64"),
            )],
            provides: vec![component_provider("copilot-shell", "custom-pkg")],
            ..Default::default()
        };
        adopt_with_query("copilot-shell", Some("custom-pkg"), &c, &q).expect("adopt ok");
        let store = load_store(&c);
        let (package, _, _) = delegated_parts(&store, "copilot-shell");
        assert_eq!(package, "custom-pkg", "the pinned package is recorded");
    }

    // ── in-lock re-validation (concurrent-writer refusals) ──

    fn empty_store() -> StateStore {
        StateStore::load(std::path::Path::new("/nonexistent/installed.toml"), 0)
            .expect("empty store")
    }

    fn store_with(binding: ProviderBinding) -> StateStore {
        let mut store = empty_store();
        store.upsert(anolisa_core::domain::Installation {
            kind: ObjectKind::Component,
            name: "copilot-shell".to_string(),
            scope: InstallationScope::System,
            binding,
            status: anolisa_core::domain::LifecycleStatus::Installed,
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        });
        store
    }

    fn owned_binding() -> ProviderBinding {
        ProviderBinding::Owned {
            artifact: anolisa_core::domain::OwnedArtifact {
                version: "1.0.0".to_string(),
                distribution_source: None,
                raw_package: None,
                manifest_digest: None,
                files: Vec::new(),
                services: Vec::new(),
                external_modified_files: Vec::new(),
                provisioned_packages: Vec::new(),
            },
        }
    }

    fn delegated_binding(relation: ManagementRelation) -> ProviderBinding {
        ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package: PackageIdentity::Resolved {
                name: "copilot-shell".to_string(),
            },
            relation,
            last_observed: None,
        }
    }

    /// A fresh adopt planned against an empty store must refuse when a
    /// concurrent raw install recorded the component first.
    #[test]
    fn adopt_authorized_refuses_concurrent_raw_install() {
        let store = store_with(owned_binding());
        let err = adopt_authorized(&store, "copilot-shell", &AdoptShape::Fresh, "adopt x")
            .expect_err("a record that appeared under the lock must refuse the adopt");
        assert!(
            err.reason().contains("appeared") && err.reason().contains("nothing was changed"),
            "got: {}",
            err.reason()
        );
    }

    /// An observed→adopted upgrade must refuse when a concurrent managed
    /// install replaced the observed record — adopt must never silently
    /// downgrade managed provenance.
    #[test]
    fn adopt_authorized_refuses_concurrent_managed_install() {
        let store = store_with(delegated_binding(ManagementRelation::Managed {
            since: "2026-06-01T10:00:00Z".to_string(),
        }));
        let shape = AdoptShape::UpgradeObserved {
            package: "copilot-shell".to_string(),
        };
        let err = adopt_authorized(&store, "copilot-shell", &shape, "adopt x")
            .expect_err("a record that changed under the lock must refuse the adopt");
        assert!(err.reason().contains("changed"), "got: {}", err.reason());
    }

    /// The happy paths pass re-validation: an empty store for a fresh adopt,
    /// a matching observed record for an upgrade.
    #[test]
    fn adopt_authorized_allows_planned_shapes() {
        adopt_authorized(
            &empty_store(),
            "copilot-shell",
            &AdoptShape::Fresh,
            "adopt x",
        )
        .expect("fresh adopt over an empty store");
        let store = store_with(delegated_binding(ManagementRelation::Observed));
        let shape = AdoptShape::UpgradeObserved {
            package: "copilot-shell".to_string(),
        };
        adopt_authorized(&store, "copilot-shell", &shape, "adopt x")
            .expect("upgrade over the matching observed record");
    }

    /// `AdoptArgs` parses the positional component and the optional
    /// `--package`.
    #[test]
    fn adopt_parses_positional_and_package_flag() {
        use clap::Parser;
        let args = AdoptArgs::try_parse_from(["adopt", "copilot-shell", "--package", "pkg-x"])
            .expect("parse");
        assert_eq!(args.component, "copilot-shell");
        assert_eq!(args.package.as_deref(), Some("pkg-x"));

        let bare = AdoptArgs::try_parse_from(["adopt", "copilot-shell"]).expect("parse");
        assert_eq!(bare.package, None);
    }
}
