//! `anolisa forget <component>` — drop a component's ANOLISA state record
//! without touching the underlying package or files.
//!
//! `forget` is the escape hatch for stale state: after a manual `rpm -e` (the
//! `missing` case from `anolisa status`), or whenever the operator wants ANOLISA
//! to stop tracking a component, `forget` removes the state record and records
//! the operation. It also resolves quarantined records — legacy state the
//! migration refused to classify — when the operator decides they are not
//! worth repairing. It performs **no** package operation — no `dnf remove`,
//! no `rpm -e` — and leaves package/component files on disk. An
//! observed/managed RPM stays installed in rpmdb; an owned component's files
//! stay on disk (use `anolisa uninstall` to remove those).

use clap::Parser;
use serde::Serialize;

use anolisa_core::execution::{CommandOutcomeStatus, ExecutionIntent};

use crate::color::Palette;
use crate::context::CliContext;
use crate::response::{CliError, render_json};

use self::application::{ForgetApplicationOutcome, ForgetRequest};

mod application;

/// Command label for JSON envelopes and error routing.
const COMMAND: &str = "forget";

/// Arguments for `anolisa forget <component>`.
#[derive(Debug, Parser)]
pub struct ForgetArgs {
    /// Component whose ANOLISA state record should be dropped
    #[arg(value_name = "COMPONENT")]
    pub component: String,
}

/// Wire shape for a `forget <component>` result (`--json`) and its dry-run
/// preview.
#[derive(Serialize)]
struct ForgetPayload {
    component: String,
    /// Provenance of the dropped record, for the audit trail:
    /// `owned` | `managed` | `adopted` | `observed` | `quarantined`.
    provenance: &'static str,
    install_mode: String,
    /// Whether the state record was actually removed (false on dry-run).
    forgotten: bool,
    dry_run: bool,
    /// `None` on dry-run (nothing recorded).
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
}

/// Dispatch `forget <component>`: drop the ANOLISA state record, run no package
/// operation.
///
/// # Errors
///
/// Returns [`CliError`] when the component is absent, still has enabled adapter
/// receipts, or the state write fails.
pub fn handle(args: ForgetArgs, ctx: &CliContext) -> Result<(), CliError> {
    let outcome = application::run(
        ForgetRequest {
            component: &args.component,
            intent: execution_intent(ctx.dry_run),
        },
        ctx,
    )?;
    let payload = match outcome {
        ForgetApplicationOutcome::Preview { subject } => ForgetPayload {
            component: subject.component,
            provenance: subject.provenance,
            install_mode: subject.install_mode,
            forgotten: false,
            dry_run: true,
            operation_id: None,
        },
        ForgetApplicationOutcome::Applied { subject, outcome } => {
            debug_assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
            for warning in outcome.warnings() {
                eprintln!("warning: {warning}");
            }
            ForgetPayload {
                component: subject.component,
                provenance: subject.provenance,
                install_mode: subject.install_mode,
                forgotten: true,
                dry_run: false,
                operation_id: outcome.operation_id().map(str::to_string),
            }
        }
    };
    render_forget(ctx, &payload);
    Ok(())
}

fn execution_intent(dry_run: bool) -> ExecutionIntent {
    if dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    }
}

/// Human/JSON renderer for a forget result.
fn render_forget(ctx: &CliContext, payload: &ForgetPayload) {
    if ctx.json {
        // Errors here are unreachable for a plain Serialize struct; ignore the
        // Result so an (already-persisted) forget is not reported as failed.
        let _ = render_json(COMMAND, payload);
        return;
    }
    if ctx.quiet {
        return;
    }
    let color = Palette::new(ctx.no_color);
    if payload.dry_run {
        println!(
            "{} {} {} {}",
            color.command("forget"),
            payload.component,
            color.muted(format!("({})", payload.provenance)),
            color.muted("(dry-run — ANOLISA state not modified)"),
        );
        println!(
            "  {}",
            color.muted("no package operation would be performed")
        );
        return;
    }
    println!(
        "{} {} {}",
        color.ok("✓ forgot"),
        payload.component,
        color.muted(format!("({})", payload.provenance)),
    );
    println!(
        "    {} ANOLISA stopped tracking this component; no package operation was performed",
        color.label("note:"),
    );
    // Tailor the residue reminder to what forget deliberately left behind.
    match payload.provenance {
        "owned" => println!(
            "    {} ANOLISA-owned files remain on disk; forget dropped their inventory, so 'anolisa uninstall' can no longer remove them — delete them manually (next time, run 'anolisa uninstall' instead of 'forget' when you want ANOLISA to remove files)",
            color.label("note:"),
        ),
        "quarantined" => println!(
            "    {} whatever backed the quarantined record — files or a package — remains on the system untouched",
            color.label("note:"),
        ),
        _ => println!(
            "    {} the RPM package remains installed; use dnf/rpm directly if you want to remove it",
            color.label("note:"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::PathBuf;

    use anolisa_core::adapter::claim::{AdapterClaim, ClaimStatus, DriverPayload, OpenClawClaim};
    use anolisa_core::state::{
        InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind, ObjectStatus,
        Ownership, RpmMetadata,
    };
    use anolisa_core::state_store::StateStore;
    use anolisa_core::transaction::Transaction;

    use crate::commands::common;
    use crate::commands::tier1::rpm_install;
    use crate::context::InstallMode;

    fn ctx(prefix: PathBuf, install_mode: InstallMode, dry_run: bool) -> CliContext {
        // Identity resolution consults the component index for names absent
        // from state; a seeded local index keeps fixture names supported.
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

    /// An adopted rpm-observed component object (legacy v4 shape; loading it
    /// exercises the migration into the v5 store).
    fn rpm_observed_object(component: &str, package: &str, evr: &str) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: component.to_string(),
            version: evr.to_string(),
            status: ObjectStatus::Adopted,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some("rpm".to_string()),
            ownership: Some(Ownership::RpmObserved),
            rpm_metadata: Some(RpmMetadata {
                package_name: package.to_string(),
                evr: Some(evr.to_string()),
                arch: Some("x86_64".to_string()),
                source_repo: Some("@System".to_string()),
            }),
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: Some("op-prior".to_string()),
            managed: false,
            adopted: true,
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

    /// A legacy object with no classifiable evidence: no backend, no
    /// ownership, no rpm metadata, no source, no files. The migration
    /// quarantines it (rule R4h).
    fn unclassifiable_object(component: &str) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: component.to_string(),
            version: "0.0.1".to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: None,
            ownership: None,
            rpm_metadata: None,
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: None,
            managed: false,
            adopted: false,
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

    fn sample_claim(component: &str, framework: &str) -> AdapterClaim {
        AdapterClaim {
            claim_schema: 1,
            component: component.to_string(),
            framework: framework.to_string(),
            plugin_id: None,
            adapter_type: None,
            enabled_at: "2026-06-01T10:00:00Z".to_string(),
            resource_root: PathBuf::from("/tmp/anolisa-forget-test"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: 1,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: Vec::new(),
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "state".to_string(),
                plugin_resource: "plugin".to_string(),
                skill_resources: Vec::new(),
                config_resources: Vec::new(),

                displaced_plugins: Vec::new(),
            }),
        }
    }

    fn seed(ctx: &CliContext, objs: Vec<InstalledObject>, claims: Vec<AdapterClaim>) {
        let layout = common::resolve_layout(ctx);
        std::fs::create_dir_all(&layout.state_dir).expect("mkdir state");
        let mut state = InstalledState {
            install_mode: match ctx.install_mode {
                InstallMode::System => StateInstallMode::System,
                InstallMode::User => StateInstallMode::User,
            },
            prefix: layout.prefix.clone(),
            ..Default::default()
        };
        for obj in objs {
            state.upsert_object(obj);
        }
        for claim in claims {
            state.upsert_adapter_claim(claim);
        }
        state
            .save(&layout.state_dir.join("installed.toml"))
            .expect("seed state");
    }

    fn load_store(ctx: &CliContext) -> StateStore {
        let layout = common::resolve_layout(ctx);
        StateStore::load(&layout.state_dir.join("installed.toml"), 0).expect("load store")
    }

    fn seed_manifest_snapshot(ctx: &CliContext, component: &str) -> PathBuf {
        let layout = common::resolve_layout(ctx);
        let snapshot = common::installed_component_manifest_path(&layout, component, COMMAND)
            .expect("snapshot path");
        let dir = snapshot.parent().expect("snapshot dir").to_path_buf();
        std::fs::create_dir_all(&dir).expect("mkdir snapshot dir");
        std::fs::write(&snapshot, "component snapshot").expect("write snapshot");
        let provenance =
            anolisa_platform::fs_layout::FsLayout::provenance_path_for_snapshot(&snapshot);
        std::fs::write(provenance, "schema_version = 1\n").expect("write provenance");
        dir
    }

    #[test]
    fn local_dry_run_maps_to_plan_intent() {
        assert_eq!(execution_intent(true), ExecutionIntent::Plan);
        assert_eq!(execution_intent(false), ExecutionIntent::Apply);
    }

    #[test]
    fn forget_preview_returns_typed_subject_without_persistent_effects() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            Vec::new(),
        );
        let snapshot_dir = seed_manifest_snapshot(&c, "copilot-shell");
        let layout = common::resolve_layout(&c);
        let operations_before = load_store(&c).operations.len();
        assert!(!layout.lock_file.exists(), "fixture must start unlocked");
        assert!(
            !layout.central_log.exists(),
            "fixture must start without a central log"
        );

        let outcome = application::run(
            ForgetRequest {
                component: "copilot-shell",
                intent: ExecutionIntent::Plan,
            },
            &c,
        )
        .expect("preview");

        let ForgetApplicationOutcome::Preview { subject } = outcome else {
            panic!("plan intent must return a preview");
        };
        assert_eq!(subject.component, "copilot-shell");
        assert_eq!(subject.provenance, "adopted");
        assert_eq!(subject.install_mode, "system");
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "copilot-shell")
                .is_some(),
            "preview must keep the state record"
        );
        assert_eq!(load_store(&c).operations.len(), operations_before);
        assert!(snapshot_dir.exists(), "preview must keep the snapshot");
        assert!(!layout.lock_file.exists(), "preview must not create a lock");
        assert!(
            !layout.central_log.exists(),
            "preview must not create a central log"
        );
    }

    #[test]
    fn forget_quarantined_preview_is_effect_free() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&c, vec![unclassifiable_object("mystery")], Vec::new());
        let layout = common::resolve_layout(&c);

        let outcome = application::run(
            ForgetRequest {
                component: "mystery",
                intent: ExecutionIntent::Plan,
            },
            &c,
        )
        .expect("quarantined preview");

        let ForgetApplicationOutcome::Preview { subject } = outcome else {
            panic!("plan intent must return a preview");
        };
        assert_eq!(subject.component, "mystery");
        assert_eq!(subject.provenance, "quarantined");
        assert!(
            load_store(&c)
                .quarantined
                .iter()
                .any(|entry| entry.record.name == "mystery"),
            "preview must keep the quarantined record"
        );
        assert!(!layout.lock_file.exists(), "preview must not create a lock");
        assert!(
            !layout.central_log.exists(),
            "preview must not create a central log"
        );
    }

    #[test]
    fn forget_apply_returns_completed_typed_outcome() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            Vec::new(),
        );
        let snapshot_dir = seed_manifest_snapshot(&c, "copilot-shell");

        let outcome = application::run(
            ForgetRequest {
                component: "copilot-shell",
                intent: ExecutionIntent::Apply,
            },
            &c,
        )
        .expect("apply");

        let ForgetApplicationOutcome::Applied { subject, outcome } = outcome else {
            panic!("apply intent must return an applied outcome");
        };
        assert_eq!(subject.component, "copilot-shell");
        assert_eq!(subject.provenance, "adopted");
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert!(outcome.operation_id().is_some());
        assert_eq!(
            outcome.changes(),
            &[application::ForgetChange::StateRecordDropped]
        );
        assert!(outcome.warnings().is_empty());
        let after = load_store(&c);
        assert!(
            after.find(ObjectKind::Component, "copilot-shell").is_none(),
            "apply must drop the state record"
        );
        assert!(
            after
                .operations
                .iter()
                .any(|operation| Some(operation.id.as_str()) == outcome.operation_id()),
            "the typed operation id must match persisted state"
        );
        assert!(!snapshot_dir.exists(), "apply must remove the snapshot");
    }

    #[test]
    fn central_log_failure_is_a_non_terminal_outcome_warning() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            Vec::new(),
        );
        let layout = common::resolve_layout(&c);
        std::fs::create_dir_all(&layout.central_log).expect("block central log file creation");

        let outcome = application::run(
            ForgetRequest {
                component: "copilot-shell",
                intent: ExecutionIntent::Apply,
            },
            &c,
        )
        .expect("central log failure stays non-terminal");

        let ForgetApplicationOutcome::Applied { outcome, .. } = outcome else {
            panic!("apply intent must return an applied outcome");
        };
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(outcome.warnings().len(), 1);
        assert!(
            outcome.warnings()[0].contains("failed to write central log"),
            "got: {:?}",
            outcome.warnings()
        );
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "copilot-shell")
                .is_none(),
            "central log failure must not undo persisted state"
        );
    }

    /// forget drops the state record and records the operation; no package
    /// operation is involved (there is no package query/transaction at all).
    #[test]
    fn forget_drops_object_and_records_operation() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            Vec::new(),
        );
        let snapshot_dir = seed_manifest_snapshot(&c, "copilot-shell");

        handle(
            ForgetArgs {
                component: "copilot-shell".to_string(),
            },
            &c,
        )
        .expect("forget ok");

        let after = load_store(&c);
        assert!(
            after.find(ObjectKind::Component, "copilot-shell").is_none(),
            "state record must be dropped",
        );
        assert!(
            after
                .operations
                .iter()
                .any(|o| o.command == "forget copilot-shell"),
            "an operation record must be appended",
        );
        assert!(
            !snapshot_dir.exists(),
            "component manifest snapshot dir must be removed",
        );
    }

    #[test]
    fn forget_repaired_cosh_ng_removes_the_legacy_snapshot() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object("cosh", "cosh-ng", "0.13.0-1.al8")],
            Vec::new(),
        );
        let layout = common::resolve_layout(&c);
        let legacy_snapshot = common::installed_component_manifest_path(&layout, "cosh", COMMAND)
            .expect("legacy snapshot path");
        std::fs::create_dir_all(legacy_snapshot.parent().expect("snapshot dir"))
            .expect("create snapshot dir");
        std::fs::write(
            &legacy_snapshot,
            r#"
            [component]
            name = "cosh"
            version = "0.13.0"

            [backends.rpm]
            package = "cosh-ng"
            "#,
        )
        .expect("write legacy snapshot");

        handle(
            ForgetArgs {
                component: "cosh-ng".to_string(),
            },
            &c,
        )
        .expect("forget repaired cosh-ng");

        assert!(!legacy_snapshot.exists());
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "cosh-ng")
                .is_none()
        );
    }

    /// forget is the documented exit for quarantined records: it drops the
    /// quarantine entry like any other record.
    #[test]
    fn forget_drops_quarantined_record() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&c, vec![unclassifiable_object("mystery")], Vec::new());
        // Sanity: the migration must have quarantined the seed.
        assert!(
            load_store(&c)
                .quarantined
                .iter()
                .any(|q| q.record.name == "mystery"),
            "seed must migrate into quarantine",
        );

        handle(
            ForgetArgs {
                component: "mystery".to_string(),
            },
            &c,
        )
        .expect("forget of a quarantined record ok");

        let after = load_store(&c);
        assert!(
            after.quarantined.iter().all(|q| q.record.name != "mystery"),
            "quarantined record must be dropped",
        );
        assert!(
            after
                .operations
                .iter()
                .any(|o| o.command == "forget mystery"),
            "an operation record must be appended",
        );
    }

    /// Forgetting an absent but index-supported component routes to
    /// NOT_INSTALLED (exit 2).
    #[test]
    fn forget_absent_supported_component_routes_to_not_installed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let err = handle(
            ForgetArgs {
                component: "agentsight".to_string(),
            },
            &c,
        )
        .expect_err("absent component must error");
        assert_eq!(err.code(), "NOT_INSTALLED");
        assert_eq!(err.exit_code(), 2);
        assert!(err.reason().contains("not installed"));
    }

    /// A name neither state nor the component index knows is rejected as an
    /// unsupported component, not reported as merely not installed
    /// (issue #2630).
    #[test]
    fn forget_unsupported_component_is_rejected() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let err = handle(
            ForgetArgs {
                component: "ghost".to_string(),
            },
            &c,
        )
        .expect_err("unsupported component must error");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("unsupported component 'ghost'"),
            "got: {}",
            err.reason()
        );
    }

    /// A component with an adapter receipt is refused until the adapter is
    /// disabled — forget must not silently orphan a registered plugin.
    #[test]
    fn forget_refuses_with_enabled_adapter_claim() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            vec![sample_claim("copilot-shell", "openclaw")],
        );
        let err = handle(
            ForgetArgs {
                component: "copilot-shell".to_string(),
            },
            &c,
        )
        .expect_err("enabled adapter must block forget");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("adapter disable"),
            "reason must point at adapter disable: {}",
            err.reason()
        );
        // The component must still be present — forget refused.
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "copilot-shell")
                .is_some(),
        );
    }

    /// Dry-run must preview the same adapter-claim refusal as a real run.
    /// A success preview would tell the operator the drop is clear, then the
    /// real forget would fail and leave adapters still claiming the record.
    #[test]
    fn forget_dry_run_refuses_with_enabled_adapter_claim() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            vec![sample_claim("copilot-shell", "openclaw")],
        );
        let snapshot_dir = seed_manifest_snapshot(&c, "copilot-shell");
        let err = handle(
            ForgetArgs {
                component: "copilot-shell".to_string(),
            },
            &c,
        )
        .expect_err("dry-run must preview the same adapter refusal as execute");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.reason().contains("adapter disable"),
            "reason must point at adapter disable: {}",
            err.reason()
        );
        assert!(
            err.reason().contains("openclaw"),
            "reason must name the blocking framework: {}",
            err.reason()
        );
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "copilot-shell")
                .is_some(),
            "dry-run refusal must leave the state record",
        );
        assert!(
            snapshot_dir.exists(),
            "dry-run refusal must leave the manifest snapshot",
        );
    }

    /// A receipt on a different component must not block this forget, including
    /// on dry-run. The preview still succeeds and still leaves this record.
    #[test]
    fn forget_dry_run_ignores_unrelated_adapter_claim() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        seed(
            &c,
            vec![
                rpm_observed_object("copilot-shell", "copilot-shell", "2.2.0-1.al8"),
                rpm_observed_object("tokenless", "tokenless", "1.0.0-1.al8"),
            ],
            vec![sample_claim("tokenless", "openclaw")],
        );
        handle(
            ForgetArgs {
                component: "copilot-shell".to_string(),
            },
            &c,
        )
        .expect("unrelated adapter receipt must not block this dry-run");
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "copilot-shell")
                .is_some(),
            "dry-run must not drop the targeted record",
        );
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "tokenless")
                .is_some(),
            "dry-run must not touch the unrelated record",
        );
    }

    /// `persist_forget` enforces the adapter-claim guard under the lock, not only
    /// in `handle`. Calling it directly — bypassing the pre-lock fast-fail, as a
    /// concurrent `adapter enable` effectively would — on a state that already
    /// holds a claim must refuse and leave the record intact. This is what closes
    /// the enable-during-forget race; a regression that drops the locked check
    /// fails here while the `handle`-level test above would still pass.
    #[test]
    fn persist_forget_rechecks_adapter_claim_under_lock() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            vec![sample_claim("copilot-shell", "openclaw")],
        );
        let err = application::persist_forget(&c, "copilot-shell", "forget copilot-shell")
            .expect_err("locked claim check must refuse");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("adapter disable"),
            "reason must point at adapter disable: {}",
            err.reason()
        );
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "copilot-shell")
                .is_some(),
            "record must remain when the locked claim check refuses",
        );
    }

    #[test]
    fn persist_forget_rechecks_record_presence_under_lock() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object("tokenless", "tokenless", "1.0.0-1.al8")],
            Vec::new(),
        );

        let err = application::persist_forget(&c, "copilot-shell", "forget copilot-shell")
            .expect_err("locked record check must refuse a disappeared component");

        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("disappeared from state"),
            "got: {}",
            err.reason()
        );
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "tokenless")
                .is_some(),
            "the unrelated state record must remain"
        );
    }

    #[test]
    fn pending_journal_blocks_forget_without_dropping_state_or_snapshot() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            Vec::new(),
        );
        let snapshot_dir = seed_manifest_snapshot(&c, "copilot-shell");
        let layout = common::resolve_layout(&c);
        let journal_dir = rpm_install::journal_dir(&layout);
        let journal = Transaction::begin_with_subject(
            "update",
            Some("copilot-shell"),
            layout.state_dir.join("installed.toml"),
            &journal_dir,
        )
        .expect("pending journal");

        let err = application::persist_forget(&c, "copilot-shell", "forget copilot-shell")
            .expect_err("locked pending recovery check must block forget");

        assert!(err.reason().contains("anolisa repair copilot-shell"));
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "copilot-shell")
                .is_some(),
            "record must remain",
        );
        assert!(snapshot_dir.exists(), "snapshot must remain");
        assert!(
            Transaction::load_journal(&journal.journal_path)
                .expect("reload journal")
                .is_pending(),
            "forget must not settle the journal",
        );
    }

    #[test]
    fn journal_without_installation_reports_repair_before_nothing_to_forget() {
        for dry_run in [false, true] {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let c = ctx(tmp.path().to_path_buf(), InstallMode::System, dry_run);
            seed(&c, Vec::new(), Vec::new());
            let layout = common::resolve_layout(&c);
            let journal = Transaction::begin_with_subject(
                "install",
                Some("cosh"),
                layout.state_dir.join("installed.toml"),
                &rpm_install::journal_dir(&layout),
            )
            .expect("pending journal");
            let err = handle(
                ForgetArgs {
                    component: "cosh".to_string(),
                },
                &c,
            )
            .expect_err("forget must preserve pending evidence");
            assert!(err.reason().contains("anolisa repair cosh"), "{err}");
            assert!(!err.reason().contains("nothing to forget"));
            assert!(
                Transaction::load_journal(&journal.journal_path)
                    .unwrap()
                    .is_pending()
            );
        }
    }

    /// Dry-run leaves the state record in place.
    #[test]
    fn forget_dry_run_leaves_state_untouched() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        seed(
            &c,
            vec![rpm_observed_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
            )],
            Vec::new(),
        );
        let snapshot_dir = seed_manifest_snapshot(&c, "copilot-shell");
        handle(
            ForgetArgs {
                component: "copilot-shell".to_string(),
            },
            &c,
        )
        .expect("dry-run ok");
        assert!(
            load_store(&c)
                .find(ObjectKind::Component, "copilot-shell")
                .is_some(),
            "dry-run must not remove the state record",
        );
        assert!(
            snapshot_dir.exists(),
            "dry-run must not remove the manifest snapshot dir",
        );
    }

    fn seed_component_index(ctx: &CliContext, index: &str) {
        let layout = common::resolve_layout(ctx);
        let repo_v1 = layout.prefix.join("repo").join("v1");
        fs::create_dir_all(&repo_v1).expect("mkdir repo");
        fs::write(repo_v1.join("components-v2.toml"), index).expect("write components-v2.toml");
        fs::create_dir_all(&layout.etc_dir).expect("mkdir etc");
        fs::write(
            layout.etc_dir.join("repo.toml"),
            format!(
                "schema_version = 1\n\
                 default_backend = \"raw\"\n\
                 \n\
                 [backends.raw]\n\
                 base_url = \"file://{}\"\n",
                repo_v1.display()
            ),
        )
        .expect("write repo.toml");
    }

    /// CLI surface: `forget <component>` parses to the positional.
    #[test]
    fn forget_parses_positional_component() {
        use clap::Parser as _;
        let a = ForgetArgs::try_parse_from(["forget", "copilot-shell"]).expect("parse");
        assert_eq!(a.component, "copilot-shell");
    }

    /// Forget by package alias (e.g., "copilot-shell") must resolve to the
    /// canonical component name ("cosh") before addressing state.
    #[test]
    fn forget_via_package_alias_succeeds() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);

        seed_component_index(
            &c,
            r#"
schema_version = 2

[[components]]
name = "cosh"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.backends]]
kind = "rpm"
package = "copilot-shell"
legacy_adopt = true

[[components.aliases]]
kind = "rpm-package"
name = "copilot-shell"
"#,
        );

        seed(
            &c,
            vec![rpm_observed_object("cosh", "copilot-shell", "2.2.0-1.al8")],
            Vec::new(),
        );
        let _snapshot_dir = seed_manifest_snapshot(&c, "cosh");

        handle(
            ForgetArgs {
                component: "copilot-shell".to_string(),
            },
            &c,
        )
        .expect("forget via alias");

        let after = load_store(&c);
        assert!(
            after.find(ObjectKind::Component, "cosh").is_none(),
            "state record for 'cosh' must be dropped",
        );
    }

    #[test]
    fn quarantined_exact_name_wins_over_repo_alias() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed_component_index(
            &c,
            r#"
schema_version = 2

[[components]]
name = "cosh"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.aliases]]
kind = "rpm-package"
name = "legacy-name"
"#,
        );
        seed(
            &c,
            vec![
                unclassifiable_object("legacy-name"),
                rpm_observed_object("cosh", "copilot-shell", "2.2.0-1.al8"),
            ],
            Vec::new(),
        );

        handle(
            ForgetArgs {
                component: "legacy-name".to_string(),
            },
            &c,
        )
        .expect("forget exact quarantine");

        let after = load_store(&c);
        assert!(
            after
                .quarantined
                .iter()
                .all(|entry| entry.record.name != "legacy-name")
        );
        assert!(
            after.find(ObjectKind::Component, "cosh").is_some(),
            "the alias target must not be forgotten",
        );
    }
}
