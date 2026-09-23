//! Application orchestration for `anolisa upgrade`.

use anolisa_core::execution::{CommandOutcome, CommandOutcomeStatus, ExecutionIntent};
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::pkg_transaction::PackageTransaction;
use anolisa_platform::privilege;

use crate::commands::common::{self, RepoPersistPolicy};
use crate::context::{CliContext, InstallMode};
use crate::progress::ProgressReporter;
use crate::response::CliError;

use super::super::update::check;
use super::{
    AppliedUpgradeStatus, COMMAND, UpgradeEngineOutcome, UpgradePlan, UpgradeReport, build_plan,
    execute_upgrade_plan,
};

/// Typed input for one image upgrade request.
pub(super) struct UpgradeRequest<'a> {
    pub(super) target: Option<&'a str>,
    pub(super) intent: ExecutionIntent,
}

/// Durable change confirmed by an applied upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpgradeChange {
    /// An RPM package advanced to a newer version.
    PackageUpdated { name: String, package: String },
    /// A missing target-profile default was installed.
    DefaultInstalled { component: String, package: String },
    /// Existing ANOLISA state was reconciled with rpmdb truth.
    StateReconciled { component: String, package: String },
    /// An already-installed default was recorded in ANOLISA state.
    ObservedRecordCreated { component: String, package: String },
}

/// Typed terminal classification for a plan-only upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpgradePreviewStatus {
    /// Every planned observation completed without an item error.
    Completed,
    /// Some preview items were available while other observations failed.
    Partial,
    /// No preview item succeeded and at least one observation failed.
    Failed,
    /// Planning errors make the upgrade ineligible for application.
    Blocked,
}

/// Typed application result consumed by the compatibility renderer.
#[derive(Debug)]
pub(super) enum UpgradeApplicationOutcome {
    /// Plan-only result; no lock, transaction, or persistent mutation occurred.
    Preview {
        status: UpgradePreviewStatus,
        report: UpgradeReport,
        warnings: Vec<String>,
    },
    /// Apply was refused by item-level planning errors before any transaction.
    Blocked {
        reason: String,
        report: UpgradeReport,
    },
    /// Applied result with typed terminal and durable operation evidence.
    Applied {
        report: UpgradeReport,
        outcome: CommandOutcome<UpgradeChange>,
    },
}

/// Run an upgrade request against the production RPM backend.
pub(super) fn run(
    request: UpgradeRequest<'_>,
    ctx: &CliContext,
    reporter: &dyn ProgressReporter,
) -> Result<UpgradeApplicationOutcome, CliError> {
    if ctx.install_mode != InstallMode::System {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: "`anolisa upgrade` supports only system/RPM image scenarios; run `sudo anolisa upgrade` without `--install-mode user`".to_string(),
        });
    }

    let layout = common::resolve_layout(ctx);
    let report = check::compute_update_check_report(request.target, ctx, &layout)?;
    let repo_config = common::load_repo_config(ctx, &layout, COMMAND, RepoPersistPolicy::Require)?;
    let env = anolisa_env::EnvService::detect();
    let repo = super::super::update::rpm_repo_source_for_update(&repo_config, &env, COMMAND)?
        .ok_or_else(|| CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: "repo.toml has no [backends.rpm] table; `anolisa upgrade` needs the configured ANOLISA RPM repository".to_string(),
        })?;
    let (query, txn) = crate::commands::tier1::rpm_backends::system(Some(repo));
    let plan = build_plan(report.target.clone(), &report.cli, &report.components);

    run_with_dependencies(
        request,
        ctx,
        &layout,
        &plan,
        &query,
        &txn,
        privilege::is_root(),
        COMMAND,
        reporter,
    )
}

/// Run an upgrade request with explicit host boundaries.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_with_dependencies(
    request: UpgradeRequest<'_>,
    ctx: &CliContext,
    layout: &anolisa_platform::fs_layout::FsLayout,
    plan: &UpgradePlan,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    command: &str,
    reporter: &dyn ProgressReporter,
) -> Result<UpgradeApplicationOutcome, CliError> {
    let engine = execute_upgrade_plan(
        ctx,
        layout,
        plan,
        query,
        txn,
        is_root,
        request.intent,
        command,
        reporter,
    )?;

    Ok(match engine {
        UpgradeEngineOutcome::Preview {
            status,
            report,
            warnings,
        } => UpgradeApplicationOutcome::Preview {
            status,
            report,
            warnings,
        },
        UpgradeEngineOutcome::Blocked { reason, report } => {
            UpgradeApplicationOutcome::Blocked { reason, report }
        }
        UpgradeEngineOutcome::Applied {
            status,
            operation_id,
            report,
            warnings,
        } => {
            let changes = changes_from_report(&report);
            let error_count = report.errors.len();
            let status = match status {
                AppliedUpgradeStatus::Completed => CommandOutcomeStatus::Completed,
                AppliedUpgradeStatus::Partial => CommandOutcomeStatus::Partial {
                    reason: format!(
                        "upgrade completed with {error_count} item error(s); reconciliation is required"
                    ),
                },
                AppliedUpgradeStatus::Failed => CommandOutcomeStatus::Failed {
                    reason: format!("upgrade failed with {error_count} item error(s)"),
                },
            };
            UpgradeApplicationOutcome::Applied {
                report,
                outcome: CommandOutcome::new(status, operation_id, changes, warnings),
            }
        }
    })
}

fn changes_from_report(report: &UpgradeReport) -> Vec<UpgradeChange> {
    report
        .updated
        .iter()
        .map(|item| UpgradeChange::PackageUpdated {
            name: item.name.clone(),
            package: item.package.clone(),
        })
        .chain(
            report
                .installed
                .iter()
                .map(|item| UpgradeChange::DefaultInstalled {
                    component: item.name.clone(),
                    package: item.package.clone(),
                }),
        )
        .chain(
            report
                .reconciled
                .iter()
                .map(|item| UpgradeChange::StateReconciled {
                    component: item.name.clone(),
                    package: item.package.clone(),
                }),
        )
        .chain(
            report
                .recorded
                .iter()
                .map(|item| UpgradeChange::ObservedRecordCreated {
                    component: item.name.clone(),
                    package: item.package.clone(),
                }),
        )
        .collect()
}
