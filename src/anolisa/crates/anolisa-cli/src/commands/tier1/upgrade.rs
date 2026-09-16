//! `anolisa upgrade` — first-phase RPM/system-image upgrade orchestration
//! (issue #1411).
//!
//! `upgrade` is an orchestration layer over RPM-backed operations, not a generic
//! backend-migration mechanism. It consumes the exact read-only plan produced by
//! [`anolisa update --check`](super::update::check::compute_update_check_report)
//! and, unless `--dry-run` is given, applies it through `dnf`:
//!
//! 1. Update the RPM-owned `anolisa` CLI package, if a newer candidate exists.
//! 2. Update already-installed RPM-backed components.
//! 3. Install target-profile default components that are missing.
//! 4. Re-read rpmdb, refresh planned changes, and reconcile recorded RPM state
//!    so `anolisa status` reflects package-manager truth.
//!
//! Scope is deliberately narrow (first phase):
//! - RPM / system-image scenario only; user mode is rejected. `--dry-run` waives
//!   the root requirement but not the system-mode one, so a non-root preview is
//!   `sudo anolisa upgrade --dry-run` or
//!   `anolisa --install-mode system upgrade --dry-run` (matching `repair`).
//! - Raw-managed components are **skipped** with a clear reason — never updated,
//!   migrated, or overwritten.
//! - npm/raw upgrade and broad adapter/framework reconciliation are out of
//!   scope; adapter needs would be surfaced as warnings only if discovered.
//!
//! The command never re-derives the plan: the planner lives in `update::check`
//! and both commands share it, so `upgrade` and `update --check` can never
//! disagree about what is upgradable.

use std::collections::HashSet;

use chrono::{SecondsFormat, Utc};
use clap::Parser;
use serde::Serialize;

use anolisa_core::central_log::{CentralLog, LogKind, LogRecord, LogStatus, Severity};
use anolisa_core::domain::{
    Installation, InstallationScope, LifecycleStatus, ManagementRelation, NativePm, Observation,
    PackageIdentity, ProviderBinding,
};
use anolisa_core::execution::{CommandOutcomeStatus, ExecutionIntent};
use anolisa_core::facts::{JournalEvidence, JournalInventory};
use anolisa_core::lock::InstallLock;
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::{PackageInfo, PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};

use super::update::check::{
    ACTION_ERROR, ACTION_INSTALL, ACTION_NOOP, ACTION_RECONCILE, ACTION_UNSUPPORTED,
    ACTION_UNSUPPORTED_RPM, ACTION_UPDATE, CliCheck, ComponentCheck,
};
use crate::color::Palette;
use crate::commands::common;
use crate::commands::tier1::install::{
    inspect_datadir_contract_drift, refresh_datadir_contract_snapshot,
};
use crate::commands::tier1::rpm_install;
use crate::context::CliContext;
use crate::progress::{self, Activity, ProgressReporter};
use crate::response::{self, CliError};

use self::application::{UpgradeApplicationOutcome, UpgradePreviewStatus, UpgradeRequest};

mod application;

/// Command label for JSON envelopes and error routing.
const COMMAND: &str = "upgrade";

/// Upgrade backend this command covers. Always `rpm` in the first phase.
const BACKEND: &str = "rpm";

/// Reason attached to raw-managed components skipped by the RPM upgrade path.
/// Matches the wording called out in issue #1411 so the boundary reads the same
/// in the human summary and the JSON envelope.
const RAW_SKIP_REASON: &str = "raw-managed components are out of scope for RPM image upgrade";

// Stable status vocabulary for the result envelope.
const STATUS_OK: &str = "ok";
const STATUS_PARTIAL: &str = "partial";
const STATUS_FAILED: &str = "failed";
const STATUS_BLOCKED: &str = "blocked";

/// Arguments for `anolisa upgrade`.
///
/// `--dry-run` and `--json` are global flags read from [`CliContext`]; they are
/// intentionally not redefined here.
#[derive(Debug, Parser)]
pub struct UpgradeArgs {
    /// Target image/toolchain profile (defaults to the repository-declared
    /// default profile).
    #[arg(long, value_name = "TARGET")]
    pub target: Option<String>,

    /// Apply the upgrade non-interactively once the plan is known.
    ///
    /// The first phase is already non-interactive (it never prompts), so this
    /// flag is accepted for forward compatibility and to make the intent
    /// explicit in scripts; it does not currently change behaviour.
    #[arg(short = 'y', long = "assume-yes")]
    pub assume_yes: bool,
}

/// Dispatch `anolisa upgrade`.
///
/// # Errors
///
/// Returns [`CliError`] when the install mode is not `system`, real execution
/// is attempted without root, the configured RPM repository cannot be resolved,
/// state cannot be persisted, or the applied upgrade finished in a non-`ok`
/// status (partial / failed / blocked) — in the latter case the result has
/// already been rendered and only the exit code is propagated.
pub fn handle(args: UpgradeArgs, ctx: &CliContext) -> Result<(), CliError> {
    // A single activity spinner spans both planning and the apply transactions,
    // so a slow repo query or `dnf` transaction never looks like a hung process.
    // It is dropped (clearing the line) before `render_result` prints the final
    // structured output, and on every early-return path via RAII.
    let feedback = progress::feedback_for_stderr(ctx.json, ctx.quiet);
    let activity = Activity::start(feedback, "Planning upgrade...");

    let outcome = application::run(
        UpgradeRequest {
            target: args.target.as_deref(),
            intent: execution_intent(ctx.dry_run),
        },
        ctx,
        &activity,
    )?;

    // Clear the spinner before the final result is rendered to stdout.
    drop(activity);
    let result = project_outcome(outcome);
    render_result(ctx, &result);

    // A non-`ok` status has already been rendered (human or JSON); propagate a
    // non-zero exit without emitting a second envelope, mirroring the batch
    // install path.
    if result.status == STATUS_OK {
        Ok(())
    } else {
        Err(CliError::BatchPartial {
            command: COMMAND.to_string(),
        })
    }
}

fn execution_intent(dry_run: bool) -> ExecutionIntent {
    if dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    }
}

fn project_outcome(outcome: UpgradeApplicationOutcome) -> UpgradeResult {
    match outcome {
        UpgradeApplicationOutcome::Preview {
            status,
            report,
            warnings,
        } => upgrade_result(report, preview_status_label(status), true, warnings),
        UpgradeApplicationOutcome::Blocked { reason, report } => {
            let _reason = reason;
            upgrade_result(report, STATUS_BLOCKED, false, Vec::new())
        }
        UpgradeApplicationOutcome::Applied { report, outcome } => {
            let status = match outcome.status() {
                CommandOutcomeStatus::Completed => STATUS_OK,
                CommandOutcomeStatus::Partial { .. } => STATUS_PARTIAL,
                CommandOutcomeStatus::Failed { .. } => STATUS_FAILED,
            };
            upgrade_result(report, status, false, outcome.warnings().to_vec())
        }
    }
}

fn preview_status_label(status: UpgradePreviewStatus) -> &'static str {
    match status {
        UpgradePreviewStatus::Completed => STATUS_OK,
        UpgradePreviewStatus::Partial => STATUS_PARTIAL,
        UpgradePreviewStatus::Failed => STATUS_FAILED,
        UpgradePreviewStatus::Blocked => STATUS_BLOCKED,
    }
}

fn upgrade_result(
    report: UpgradeReport,
    status: &'static str,
    dry_run: bool,
    warnings: Vec<String>,
) -> UpgradeResult {
    UpgradeResult {
        target: report.target,
        backend: report.backend,
        status,
        dry_run,
        updated: report.updated,
        installed: report.installed,
        reconciled: report.reconciled,
        recorded: report.recorded,
        skipped: report.skipped,
        errors: report.errors,
        warnings,
    }
}

// ── plan conversion ────────────────────────────────────────────────────────

/// A single RPM package the upgrade will `dnf upgrade`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedUpdate {
    /// ANOLISA component name (or the CLI package name for the CLI update).
    name: String,
    /// RPM package to upgrade.
    package: String,
    /// EVR recorded before the upgrade (rpmdb truth from the check).
    from: String,
    /// Newest repo candidate EVR the check reported.
    to: String,
    /// Whether a missing state row should be recorded as an observed RPM default.
    adopt_if_missing: bool,
    /// Whether this package was resolved for a legacy RPM row without metadata.
    backfill_rpm_metadata: bool,
}

/// A missing default component the upgrade will `dnf install`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedInstall {
    name: String,
    package: String,
}

/// A target default already present in rpmdb but absent from ANOLISA state, with
/// no dnf transaction needed. Upgrade records it as observed state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedObservedDefault {
    name: String,
    package: String,
    installed: String,
}

/// Resolved RPM identity for a legacy state row that the final sweep may repair.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedLegacyReconciliation {
    name: String,
    package: String,
}

/// An item deliberately left untouched (raw-managed, non-RPM CLI).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkippedItem {
    name: String,
    reason: String,
}

/// An item that cannot be planned/applied (ambiguous mapping, missing package,
/// upstream query failure recorded by the check).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanError {
    name: String,
    reason: String,
}

/// The RPM transaction plan converted from an [`update --check`] report.
#[derive(Debug, Default, PartialEq, Eq)]
struct UpgradePlan {
    target: Option<String>,
    /// CLI package update, kept separate because it is applied first and is not
    /// recorded as an ANOLISA component object (the binary is owned by rpm).
    cli: Option<PlannedUpdate>,
    updates: Vec<PlannedUpdate>,
    installs: Vec<PlannedInstall>,
    observed_defaults: Vec<PlannedObservedDefault>,
    legacy_reconciliations: Vec<PlannedLegacyReconciliation>,
    skipped: Vec<SkippedItem>,
    errors: Vec<PlanError>,
}

impl UpgradePlan {
    /// Whether the plan carries an item error. Real execution aborts before any
    /// `dnf` transaction when this is true.
    fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Convert a check report's CLI + component rows into an [`UpgradePlan`].
///
/// This is a pure classification with no host access: it maps the check's action
/// vocabulary onto upgrade intents (see issue #1411 §"Plan conversion"). An
/// `install` row that carries no resolved RPM package becomes an item **error**
/// (it cannot be applied) rather than a silent no-op; unsupported/raw-managed
/// rows become skips and never abort.
fn build_plan(
    target: Option<String>,
    cli: &CliCheck,
    components: &[ComponentCheck],
) -> UpgradePlan {
    let mut plan = UpgradePlan {
        target,
        ..UpgradePlan::default()
    };

    // ── CLI ──
    match cli.action.as_str() {
        ACTION_UPDATE => match cli_update(cli) {
            Ok(update) => plan.cli = Some(update),
            Err(reason) => plan.errors.push(PlanError {
                name: cli_name(cli),
                reason,
            }),
        },
        ACTION_NOOP => {}
        ACTION_UNSUPPORTED => plan.skipped.push(SkippedItem {
            name: cli_name(cli),
            reason: cli.error.clone().unwrap_or_else(|| {
                "anolisa CLI is not RPM-managed; use `anolisa update self`".to_string()
            }),
        }),
        ACTION_ERROR => plan.errors.push(PlanError {
            name: cli_name(cli),
            reason: cli
                .error
                .clone()
                .unwrap_or_else(|| "CLI upgrade check failed".to_string()),
        }),
        // Any other/unknown action is treated as nothing to do.
        _ => {}
    }

    // ── components ──
    for component in components {
        match component.action.as_str() {
            ACTION_UPDATE => match component_update(component) {
                Ok(update) => {
                    if component.backfill_rpm_metadata {
                        plan.legacy_reconciliations
                            .push(PlannedLegacyReconciliation {
                                name: update.name.clone(),
                                package: update.package.clone(),
                            });
                    }
                    plan.updates.push(update);
                }
                Err(reason) => plan.errors.push(PlanError {
                    name: component.component.clone(),
                    reason,
                }),
            },
            ACTION_INSTALL => match component.package.clone() {
                Some(package) => plan.installs.push(PlannedInstall {
                    name: component.component.clone(),
                    package,
                }),
                // A default with no resolved RPM package cannot be installed.
                // The check leaves `package = None` when the component index has
                // no unambiguous mapping; refuse rather than guess a name.
                None => plan.errors.push(PlanError {
                    name: component.component.clone(),
                    reason: format!(
                        "no RPM package could be resolved for default component '{}'; cannot install it (repo component index is missing or ambiguous)",
                        component.component
                    ),
                }),
            },
            ACTION_UNSUPPORTED_RPM => plan.skipped.push(SkippedItem {
                name: component.component.clone(),
                reason: RAW_SKIP_REASON.to_string(),
            }),
            ACTION_RECONCILE => match component.package.clone() {
                Some(package) => plan.legacy_reconciliations.push(
                    PlannedLegacyReconciliation {
                        name: component.component.clone(),
                        package,
                    },
                ),
                None => plan.errors.push(PlanError {
                    name: component.component.clone(),
                    reason: "state reconciliation reported without an RPM package".to_string(),
                }),
            },
            ACTION_NOOP => {
                if component.absent_from_state {
                    match observed_default(component) {
                        Ok(observed) => plan.observed_defaults.push(observed),
                        Err(reason) => plan.errors.push(PlanError {
                            name: component.component.clone(),
                            reason,
                        }),
                    }
                }
            }
            ACTION_ERROR => plan.errors.push(PlanError {
                name: component.component.clone(),
                reason: component
                    .error
                    .clone()
                    .unwrap_or_else(|| "component upgrade check failed".to_string()),
            }),
            _ => {}
        }
    }

    plan
}

/// Human/JSON name for the CLI row (its RPM package, falling back to `anolisa`).
fn cli_name(cli: &CliCheck) -> String {
    cli.package.clone().unwrap_or_else(|| "anolisa".to_string())
}

/// Build the CLI [`PlannedUpdate`], or an error string when the check row is
/// missing the fields an update needs.
fn cli_update(cli: &CliCheck) -> Result<PlannedUpdate, String> {
    let package = cli
        .package
        .clone()
        .ok_or_else(|| "CLI update reported without an RPM package".to_string())?;
    let from = cli
        .installed
        .clone()
        .ok_or_else(|| "CLI update reported without an installed version".to_string())?;
    let to = cli
        .available
        .clone()
        .ok_or_else(|| "CLI update reported without a candidate version".to_string())?;
    Ok(PlannedUpdate {
        name: package.clone(),
        package,
        from,
        to,
        adopt_if_missing: false,
        backfill_rpm_metadata: false,
    })
}

/// Build a component [`PlannedUpdate`], or an error string when the check row is
/// missing the fields an update needs.
fn component_update(component: &ComponentCheck) -> Result<PlannedUpdate, String> {
    let package = component
        .package
        .clone()
        .ok_or_else(|| "update reported without an RPM package".to_string())?;
    let from = component
        .installed
        .clone()
        .ok_or_else(|| "update reported without an installed version".to_string())?;
    let to = component
        .available
        .clone()
        .ok_or_else(|| "update reported without a candidate version".to_string())?;
    Ok(PlannedUpdate {
        name: component.component.clone(),
        package,
        from,
        to,
        adopt_if_missing: component.absent_from_state,
        backfill_rpm_metadata: component.backfill_rpm_metadata,
    })
}

/// Build a no-transaction observed-default record plan, or an error string when
/// the check row is missing the fields needed to refresh rpmdb/state.
fn observed_default(component: &ComponentCheck) -> Result<PlannedObservedDefault, String> {
    let package = component
        .package
        .clone()
        .ok_or_else(|| "observed default reported without an RPM package".to_string())?;
    let installed = component
        .installed
        .clone()
        .ok_or_else(|| "observed default reported without an installed version".to_string())?;
    Ok(PlannedObservedDefault {
        name: component.component.clone(),
        package,
        installed,
    })
}

// ── result envelope ──────────────────────────────────────────────────────────

/// Execution report without a terminal classification.
#[derive(Debug)]
struct UpgradeReport {
    target: Option<String>,
    backend: &'static str,
    updated: Vec<UpdatedItem>,
    installed: Vec<InstalledItem>,
    /// RPM-backed state rows refreshed from rpmdb without a package transaction.
    reconciled: Vec<ReconciledItem>,
    recorded: Vec<RecordedItem>,
    skipped: Vec<SkippedResult>,
    errors: Vec<ErrorResult>,
}

/// Terminal classification derived from an applied upgrade report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppliedUpgradeStatus {
    Completed,
    Partial,
    Failed,
}

impl AppliedUpgradeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => STATUS_OK,
            Self::Partial => STATUS_PARTIAL,
            Self::Failed => STATUS_FAILED,
        }
    }
}

/// Internal executor result before the application layer builds its outcome.
#[derive(Debug)]
enum UpgradeEngineOutcome {
    Preview {
        status: UpgradePreviewStatus,
        report: UpgradeReport,
        warnings: Vec<String>,
    },
    Blocked {
        reason: String,
        report: UpgradeReport,
    },
    Applied {
        status: AppliedUpgradeStatus,
        operation_id: Option<String>,
        report: UpgradeReport,
        warnings: Vec<String>,
    },
}

/// Wire shape for `anolisa upgrade` (`--json`) and the human summary source.
#[derive(Debug, Serialize)]
struct UpgradeResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    backend: &'static str,
    status: &'static str,
    dry_run: bool,
    updated: Vec<UpdatedItem>,
    installed: Vec<InstalledItem>,
    /// RPM-backed state rows refreshed from rpmdb without a package transaction.
    reconciled: Vec<ReconciledItem>,
    recorded: Vec<RecordedItem>,
    skipped: Vec<SkippedResult>,
    errors: Vec<ErrorResult>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UpdatedItem {
    name: String,
    package: String,
    from: String,
    to: String,
}

#[derive(Debug, Serialize)]
struct InstalledItem {
    name: String,
    package: String,
    /// Installed EVR after `dnf install`; `None` on a dry-run preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReconciledItem {
    name: String,
    package: String,
    from: String,
    to: String,
    /// Drift that caused this no-transaction reconciliation.
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct RecordedItem {
    name: String,
    package: String,
    /// Installed EVR recorded into ANOLISA state; `None` on a dry-run preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Debug, Serialize)]
struct SkippedResult {
    name: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct ErrorResult {
    name: String,
    reason: String,
}

// ── apply ──────────────────────────────────────────────────────────────────

/// A component state refresh awaiting the state save. Most rows follow a
/// successful dnf update; already-current observed defaults use the same shape
/// after a read-only rpmdb refresh. The CLI update is intentionally not modelled
/// here because the binary is not an ANOLISA component object.
struct PendingUpdate {
    name: String,
    package: String,
    /// EVR before the upgrade, carried through so the result can show `from → to`.
    from: String,
    /// Post-transaction rpmdb truth used to refresh recorded version/EVR/arch.
    refreshed: PackageInfo,
    /// Installed package source repository, if rpm/dnf can report it separately.
    source_repo: Option<String>,
    /// True for a target default that was present in rpmdb but absent from state.
    adopt_if_missing: bool,
    /// True when the locked legacy row may still lack RPM package metadata.
    backfill_rpm_metadata: bool,
    /// True when no package update occurred and the item should be reported as a
    /// state recording rather than an update.
    record_only: bool,
}

/// A newly installed default whose dnf transaction and rpmdb re-read both
/// succeeded, awaiting the state save.
struct PendingInstall {
    name: String,
    package: String,
    refreshed: PackageInfo,
    source_repo: Option<String>,
}

/// A drifted RPM-backed state row awaiting the caller's single state save.
struct PendingReconciliation {
    name: String,
    package: String,
    from: String,
    refreshed: PackageInfo,
    source_repo: Option<String>,
    allow_metadata_backfill: bool,
    reason: &'static str,
}

#[derive(Default)]
struct ReconciliationInspection {
    pending: Vec<PendingReconciliation>,
    errors: Vec<ErrorResult>,
}

/// Outcome of the single state save: the items it actually recorded (reported as
/// updated/installed) plus per-item drift errors for changes it refused to make.
#[derive(Default)]
struct PersistOutcome {
    operation_id: Option<String>,
    updated: Vec<UpdatedItem>,
    installed: Vec<InstalledItem>,
    reconciled: Vec<ReconciledItem>,
    recorded: Vec<RecordedItem>,
    errors: Vec<ErrorResult>,
}

#[derive(Default)]
struct AuthorizedPlan<'a> {
    updates: Vec<&'a PlannedUpdate>,
    installs: Vec<&'a PlannedInstall>,
    observed_defaults: Vec<&'a PlannedObservedDefault>,
    errors: Vec<ErrorResult>,
}

struct UpgradeAudit {
    started_at: String,
    operation_id: String,
}

struct FinalizeUpgrade<'a> {
    ctx: &'a CliContext,
    layout: &'a FsLayout,
    command: &'a str,
    state: &'a mut StateStore,
    audit: &'a UpgradeAudit,
    query: &'a dyn PackageQuery,
    cli_updated: Option<&'a UpdatedItem>,
    updates: &'a [PendingUpdate],
    installs: &'a [PendingInstall],
    legacy_reconciliations: &'a [PlannedLegacyReconciliation],
    prior_errors: &'a [ErrorResult],
    warnings: &'a mut Vec<String>,
}

fn new_upgrade_audit() -> UpgradeAudit {
    let lock_ts = Utc::now();
    UpgradeAudit {
        started_at: now_iso8601(),
        operation_id: format!(
            "op-upgrade-{}-{}",
            lock_ts.format("%Y%m%d%H%M%S"),
            lock_ts.timestamp_subsec_nanos()
        ),
    }
}

/// Revalidate the plan against the locked ANOLISA state before any RPM
/// transaction runs. Only authorized items are handed to dnf.
fn authorize_plan<'a>(store: &StateStore, plan: &'a UpgradePlan) -> AuthorizedPlan<'a> {
    let mut authorized = AuthorizedPlan::default();

    for update in &plan.updates {
        match store.find(ObjectKind::Component, &update.name) {
            Some(installation)
                if is_matching_or_legacy_delegated(
                    installation,
                    &update.package,
                    update.backfill_rpm_metadata,
                ) =>
            {
                authorized.updates.push(update);
            }
            Some(installation) => authorized.errors.push(ErrorResult {
                name: update.name.clone(),
                reason: format!(
                    "component '{}' is now {} in ANOLISA state; refusing to run dnf update for '{}'",
                    update.name,
                    provenance_label(installation),
                    update.package
                ),
            }),
            None if update.adopt_if_missing => authorized.updates.push(update),
            None => authorized.errors.push(ErrorResult {
                name: update.name.clone(),
                reason: format!(
                    "component '{}' is no longer present in ANOLISA state; refusing to run dnf update for '{}'",
                    update.name, update.package
                ),
            }),
        }
    }

    for install in &plan.installs {
        match classify_install_slot(store, &install.name, &install.package) {
            InstallSlot::Absent | InstallSlot::MatchingRpm => authorized.installs.push(install),
            InstallSlot::Conflict(existing_ownership) => authorized.errors.push(ErrorResult {
                name: install.name.clone(),
                reason: format!(
                    "component '{}' already exists as {existing_ownership} in ANOLISA state; refusing to run dnf install for '{}'",
                    install.name, install.package
                ),
            }),
        }
    }

    for observed in &plan.observed_defaults {
        match store.find(ObjectKind::Component, &observed.name) {
            Some(installation) if is_matching_delegated(installation, &observed.package) => {
                authorized.observed_defaults.push(observed);
            }
            Some(installation) => authorized.errors.push(ErrorResult {
                name: observed.name.clone(),
                reason: format!(
                    "default component '{}' already exists as {} in ANOLISA state; refusing to record '{}' as observed",
                    observed.name,
                    provenance_label(installation),
                    observed.package
                ),
            }),
            None => authorized.observed_defaults.push(observed),
        }
    }

    authorized
}

/// Execute an upgrade plan with explicit host boundaries.
///
/// Plan intent renders read-only observations without taking the install lock.
/// Apply requires root, blocks on plan errors, and refreshes state after RPM work.
#[allow(clippy::too_many_arguments)]
fn execute_upgrade_plan(
    ctx: &CliContext,
    layout: &FsLayout,
    plan: &UpgradePlan,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    intent: ExecutionIntent,
    command: &str,
    reporter: &dyn ProgressReporter,
) -> Result<UpgradeEngineOutcome, CliError> {
    let preview_store = common::load_state_store(ctx, command)?;
    reject_upgrade_pending_claims(layout, &preview_store.operations, command)?;

    if intent == ExecutionIntent::Plan {
        // Dry-run reads state/rpmdb without taking the install lock, applying a
        // transaction, or constructing an operation to persist.
        return Ok(render_plan_preview(
            plan,
            layout,
            &preview_store,
            query,
            is_root.then_some(txn),
            command,
            ctx.packaged_data_probe(),
        ));
    }

    // Real execution needs root for the dnf transactions. Check up front so the
    // operator gets an actionable message rather than dnf's raw refusal.
    if !is_root {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: "applying an RPM image upgrade requires root privileges; re-run with sudo: `sudo anolisa upgrade`".to_string(),
        });
    }

    // Any planning error blocks the whole run before a single dnf transaction.
    if plan.has_errors() {
        return Ok(UpgradeEngineOutcome::Blocked {
            reason: format!(
                "upgrade plan contains {} error(s); no action was taken",
                plan.errors.len()
            ),
            report: blocked_report(plan),
        });
    }

    let mut errors: Vec<ErrorResult> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut updated: Vec<UpdatedItem> = Vec::new();
    let mut installed: Vec<InstalledItem> = Vec::new();
    let mut reconciled: Vec<ReconciledItem> = Vec::new();
    let mut recorded: Vec<RecordedItem> = Vec::new();
    let operation_id;
    // An empty transaction plan can still carry RPM state drift caused by an
    // external yum/dnf run, so every real invocation enters the same locked
    // finalize boundary. A true no-op returns before save/audit below.
    {
        let _lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to acquire install lock: {err}"),
        })?;
        let mut store = common::load_state_store(ctx, command)?;
        reject_upgrade_pending_claims(layout, &store.operations, command)?;
        let audit = new_upgrade_audit();

        let authorized = authorize_plan(&store, plan);
        errors.extend(authorized.errors);

        // Reliable total for the `i/total` counter: one step per `dnf`
        // transaction that will actually run — the CLI update plus one merged
        // transaction for all component updates and one for all missing
        // installs. Observed-default recording touches no transaction and is
        // folded into the finalize phase, so it is deliberately excluded — the
        // counter never advertises work that has no `dnf` step.
        let transaction_total = plan.cli.is_some() as usize
            + usize::from(!authorized.updates.is_empty())
            + usize::from(!authorized.installs.is_empty());
        let mut transaction_step = 0usize;

        // The CLI update is applied first and reported, but is never an ANOLISA
        // component object (the binary is rpm-owned). Held separately so the
        // audit step can count it toward the outcome even on a CLI-only upgrade.
        let mut cli_updated: Option<UpdatedItem> = None;
        // Component updates / installs whose dnf transaction and rpmdb re-read
        // both succeeded, held back until the single state save records them.
        let mut pending_updates: Vec<PendingUpdate> = Vec::new();
        let mut pending_installs: Vec<PendingInstall> = Vec::new();

        // 1. CLI package (rpm-owned binary). Reported but never recorded as a
        //    component object, so it is not gated on component state.
        if let Some(cli) = &plan.cli {
            transaction_step += 1;
            reporter.report(&format!(
                "Upgrading {} ({transaction_step}/{transaction_total})...",
                cli.name
            ));
            match txn.update(&[cli.package.as_str()]) {
                Ok(()) => match refresh_evr(query, &cli.package) {
                    Ok(to) => {
                        cli_updated = Some(UpdatedItem {
                            name: cli.name.clone(),
                            package: cli.package.clone(),
                            from: cli.from.clone(),
                            to,
                        })
                    }
                    Err(reason) => errors.push(ErrorResult {
                        name: cli.name.clone(),
                        reason,
                    }),
                },
                Err(err) => match query.query_installed(&cli.package) {
                    Ok(Some(info)) if info.version.to_string() != cli.from => {
                        warnings.push(format!(
                            "'{}' was upgraded despite the transaction failure; recording rpmdb truth",
                            cli.package
                        ));
                        cli_updated = Some(UpdatedItem {
                            name: cli.name.clone(),
                            package: cli.package.clone(),
                            from: cli.from.clone(),
                            to: info.version.to_string(),
                        });
                    }
                    Ok(Some(_)) => errors.push(ErrorResult {
                        name: cli.name.clone(),
                        reason: txn_error_reason(err),
                    }),
                    Ok(None) => errors.push(ErrorResult {
                        name: cli.name.clone(),
                        reason: format!(
                            "{}; '{}' is now absent from rpmdb",
                            txn_error_reason(err),
                            cli.package
                        ),
                    }),
                    Err(query_err) => errors.push(ErrorResult {
                        name: cli.name.clone(),
                        reason: format!(
                            "{}; verifying '{}' afterwards also failed ({query_err})",
                            txn_error_reason(err),
                            cli.package
                        ),
                    }),
                },
            }
        }

        // 2. Already-installed RPM-backed components authorized by locked
        //    state: one merged dnf transaction, so the solver resolves the
        //    whole set at once and the transaction commits or fails as a unit.
        if !authorized.updates.is_empty() {
            transaction_step += 1;
            let members = authorized
                .updates
                .iter()
                .map(|update| update.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            reporter.report(&format!(
                "Upgrading {members} ({transaction_step}/{transaction_total})..."
            ));
            let packages: Vec<&str> = authorized
                .updates
                .iter()
                .map(|update| update.package.as_str())
                .collect();
            match txn.update(&packages) {
                Ok(()) => {
                    for update in &authorized.updates {
                        stage_refreshed_update(
                            update,
                            query,
                            &mut pending_updates,
                            &mut errors,
                            &mut warnings,
                        );
                    }
                }
                // A one-member transaction has nothing to isolate: the member
                // is the offender, exactly like the historical per-item run.
                Err(err) if authorized.updates.len() == 1 => reconcile_failed_update(
                    authorized.updates[0],
                    &txn_error_reason(err),
                    "transaction",
                    query,
                    &mut pending_updates,
                    &mut errors,
                    &mut warnings,
                ),
                Err(err) => degrade_merged_updates(
                    &authorized.updates,
                    &txn_error_reason(err),
                    query,
                    txn,
                    reporter,
                    &mut pending_updates,
                    &mut errors,
                    &mut warnings,
                ),
            }
        }

        // 3. Missing default components authorized by locked state, merged
        //    into one dnf install for the same reason.
        if !authorized.installs.is_empty() {
            transaction_step += 1;
            let members = authorized
                .installs
                .iter()
                .map(|install| install.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            reporter.report(&format!(
                "Installing {members} ({transaction_step}/{transaction_total})..."
            ));
            let packages: Vec<&str> = authorized
                .installs
                .iter()
                .map(|install| install.package.as_str())
                .collect();
            match txn.install(&packages) {
                Ok(()) => {
                    for install in &authorized.installs {
                        stage_refreshed_install(
                            install,
                            query,
                            &mut pending_installs,
                            &mut errors,
                            &mut warnings,
                        );
                    }
                }
                Err(err) if authorized.installs.len() == 1 => reconcile_failed_install(
                    authorized.installs[0],
                    &txn_error_reason(err),
                    "transaction",
                    query,
                    &mut pending_installs,
                    &mut errors,
                    &mut warnings,
                ),
                Err(err) => degrade_merged_installs(
                    &authorized.installs,
                    &txn_error_reason(err),
                    query,
                    txn,
                    reporter,
                    &mut pending_installs,
                    &mut errors,
                    &mut warnings,
                ),
            }
        }

        // 4. Already-current target defaults authorized by locked state. No dnf
        //    transaction is needed; refresh rpmdb truth and record as observed.
        for observed in authorized.observed_defaults {
            match query.query_installed(&observed.package) {
                Ok(Some(info)) => {
                    let source_repo =
                        installed_origin_or_warn(query, &observed.package, &mut warnings);
                    pending_updates.push(PendingUpdate {
                        name: observed.name.clone(),
                        package: observed.package.clone(),
                        from: observed.installed.clone(),
                        refreshed: info,
                        source_repo,
                        adopt_if_missing: true,
                        backfill_rpm_metadata: false,
                        record_only: true,
                    });
                }
                Ok(None) => errors.push(ErrorResult {
                    name: observed.name.clone(),
                    reason: format!(
                        "default component '{}' resolved to package '{}' but it is absent from rpmdb; state was not recorded",
                        observed.name, observed.package
                    ),
                }),
                Err(err) => errors.push(ErrorResult {
                    name: observed.name.clone(),
                    reason: format!(
                        "default component '{}' resolved to package '{}' but reading its version failed ({err}); state was not recorded",
                        observed.name, observed.package
                    ),
                }),
            }
        }

        // Refresh ANOLISA state and write the durable audit while the same lock
        // that authorized the RPM work is still held.
        reporter.report("Finalizing ANOLISA state...");
        let outcome = finalize_upgrade(FinalizeUpgrade {
            ctx,
            layout,
            command,
            state: &mut store,
            audit: &audit,
            query,
            cli_updated: cli_updated.as_ref(),
            updates: &pending_updates,
            installs: &pending_installs,
            legacy_reconciliations: &plan.legacy_reconciliations,
            prior_errors: &errors,
            warnings: &mut warnings,
        })?;
        if let Some(item) = cli_updated {
            updated.push(item);
        }
        operation_id = outcome.operation_id;
        updated.extend(outcome.updated);
        installed.extend(outcome.installed);
        reconciled.extend(outcome.reconciled);
        recorded.extend(outcome.recorded);
        errors.extend(outcome.errors);
    }

    let succeeded = updated.len() + installed.len() + reconciled.len() + recorded.len();
    let status = apply_status(succeeded, errors.len());

    Ok(UpgradeEngineOutcome::Applied {
        status,
        operation_id,
        report: UpgradeReport {
            target: plan.target.clone(),
            backend: BACKEND,
            updated,
            installed,
            reconciled,
            recorded,
            skipped: skipped_results(plan),
            errors,
        },
        warnings,
    })
}

/// Compatibility projection used by the existing executor-focused tests.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_upgrade_with_deps(
    ctx: &CliContext,
    layout: &FsLayout,
    plan: &UpgradePlan,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    dry_run: bool,
    command: &str,
    reporter: &dyn ProgressReporter,
) -> Result<UpgradeResult, CliError> {
    application::run_with_dependencies(
        UpgradeRequest {
            target: plan.target.as_deref(),
            intent: execution_intent(dry_run),
        },
        ctx,
        layout,
        plan,
        query,
        txn,
        is_root,
        command,
        reporter,
    )
    .map(project_outcome)
}

/// Re-read one upgraded package from rpmdb and stage its state refresh. A
/// package the transaction covered but rpmdb cannot confirm becomes an error
/// that routes to repair — never a silent record.
fn stage_refreshed_update(
    update: &PlannedUpdate,
    query: &dyn PackageQuery,
    pending_updates: &mut Vec<PendingUpdate>,
    errors: &mut Vec<ErrorResult>,
    warnings: &mut Vec<String>,
) {
    match query.query_installed(&update.package) {
        Ok(Some(info)) => stage_update_observation(
            update,
            info,
            query,
            pending_updates,
            warnings,
        ),
        Ok(None) => errors.push(ErrorResult {
            name: update.name.clone(),
            reason: format!(
                "dnf upgraded '{}' but it is no longer in rpmdb under that name; run `sudo anolisa --install-mode system repair {}`",
                update.package, update.name
            ),
        }),
        Err(err) => errors.push(ErrorResult {
            name: update.name.clone(),
            reason: format!(
                "dnf upgraded '{}' but reading the new version failed ({err}); run `sudo anolisa --install-mode system repair {}`",
                update.package, update.name
            ),
        }),
    }
}

fn stage_update_observation(
    update: &PlannedUpdate,
    info: PackageInfo,
    query: &dyn PackageQuery,
    pending_updates: &mut Vec<PendingUpdate>,
    warnings: &mut Vec<String>,
) {
    let source_repo = installed_origin_or_warn(query, &update.package, warnings);
    pending_updates.push(PendingUpdate {
        name: update.name.clone(),
        package: update.package.clone(),
        from: update.from.clone(),
        refreshed: info,
        source_repo,
        adopt_if_missing: update.adopt_if_missing,
        backfill_rpm_metadata: update.backfill_rpm_metadata,
        record_only: false,
    });
}

/// Re-read one freshly installed package from rpmdb and stage its record.
fn stage_refreshed_install(
    install: &PlannedInstall,
    query: &dyn PackageQuery,
    pending_installs: &mut Vec<PendingInstall>,
    errors: &mut Vec<ErrorResult>,
    warnings: &mut Vec<String>,
) {
    match query.query_installed(&install.package) {
        Ok(Some(info)) => {
            stage_install_observation(install, info, query, pending_installs, warnings)
        }
        Ok(None) => errors.push(ErrorResult {
            name: install.name.clone(),
            reason: format!(
                "dnf installed '{}' but it is not present in rpmdb; state was not recorded",
                install.package
            ),
        }),
        Err(err) => errors.push(ErrorResult {
            name: install.name.clone(),
            reason: format!(
                "dnf installed '{}' but reading its version failed ({err}); state was not recorded",
                install.package
            ),
        }),
    }
}

fn stage_install_observation(
    install: &PlannedInstall,
    info: PackageInfo,
    query: &dyn PackageQuery,
    pending_installs: &mut Vec<PendingInstall>,
    warnings: &mut Vec<String>,
) {
    let source_repo = installed_origin_or_warn(query, &install.package, warnings);
    pending_installs.push(PendingInstall {
        name: install.name.clone(),
        package: install.package.clone(),
        refreshed: info,
        source_repo,
    });
}

#[allow(clippy::too_many_arguments)]
fn reconcile_failed_update(
    update: &PlannedUpdate,
    failure_reason: &str,
    failure_kind: &str,
    query: &dyn PackageQuery,
    pending_updates: &mut Vec<PendingUpdate>,
    errors: &mut Vec<ErrorResult>,
    warnings: &mut Vec<String>,
) {
    match query.query_installed(&update.package) {
        Ok(Some(info)) if info.version.to_string() != update.from => {
            warnings.push(format!(
                "'{}' was upgraded despite the {failure_kind} failure; recording rpmdb truth",
                update.package
            ));
            stage_update_observation(update, info, query, pending_updates, warnings);
        }
        Ok(Some(_)) => errors.push(ErrorResult {
            name: update.name.clone(),
            reason: failure_reason.to_string(),
        }),
        Ok(None) => errors.push(ErrorResult {
            name: update.name.clone(),
            reason: format!(
                "{failure_reason}; '{}' is now absent from rpmdb — run `sudo anolisa --install-mode system repair {}`",
                update.package, update.name
            ),
        }),
        Err(err) => errors.push(ErrorResult {
            name: update.name.clone(),
            reason: format!(
                "{failure_reason}; verifying '{}' afterwards also failed ({err}) — run `sudo anolisa --install-mode system repair {}`",
                update.package, update.name
            ),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn reconcile_failed_install(
    install: &PlannedInstall,
    failure_reason: &str,
    failure_kind: &str,
    query: &dyn PackageQuery,
    pending_installs: &mut Vec<PendingInstall>,
    errors: &mut Vec<ErrorResult>,
    warnings: &mut Vec<String>,
) {
    match query.query_installed(&install.package) {
        Ok(Some(info)) => {
            warnings.push(format!(
                "'{}' was installed despite the {failure_kind} failure; recording rpmdb truth",
                install.package
            ));
            stage_install_observation(install, info, query, pending_installs, warnings);
        }
        Ok(None) => errors.push(ErrorResult {
            name: install.name.clone(),
            reason: failure_reason.to_string(),
        }),
        Err(err) => errors.push(ErrorResult {
            name: install.name.clone(),
            reason: format!(
                "{failure_reason}; verifying '{}' afterwards also failed ({err}); state was not recorded — run `sudo anolisa --install-mode system repair {}`",
                install.package, install.name
            ),
        }),
    }
}

/// A merged `dnf update` failed. Only an exact pre-transaction EVR authorizes
/// an individual retry; moved or indeterminate rpmdb state stays forward-only.
#[allow(clippy::too_many_arguments)]
fn degrade_merged_updates(
    updates: &[&PlannedUpdate],
    merged_reason: &str,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    reporter: &dyn ProgressReporter,
    pending_updates: &mut Vec<PendingUpdate>,
    errors: &mut Vec<ErrorResult>,
    warnings: &mut Vec<String>,
) {
    warnings.push(format!(
        "merged dnf update failed ({merged_reason}); checking members for safe individual retry"
    ));
    for update in updates {
        match query.query_installed(&update.package) {
            Ok(Some(info)) if info.version.to_string() != update.from => {
                warnings.push(format!(
                    "'{}' was upgraded despite the merged transaction failure; recording rpmdb truth",
                    update.package
                ));
                stage_update_observation(update, info, query, pending_updates, warnings);
            }
            Ok(Some(_)) => {
                reporter.report(&format!("Retrying {} individually...", update.name));
                match txn.update(&[update.package.as_str()]) {
                    Ok(()) => {
                        stage_refreshed_update(update, query, pending_updates, errors, warnings);
                    }
                    Err(err) => reconcile_failed_update(
                        update,
                        &txn_error_reason(err),
                        "retry",
                        query,
                        pending_updates,
                        errors,
                        warnings,
                    ),
                }
            }
            Ok(None) => errors.push(ErrorResult {
                name: update.name.clone(),
                reason: format!(
                    "merged dnf update failed ({merged_reason}) and '{}' is now absent from rpmdb; run `sudo anolisa --install-mode system repair {}`",
                    update.package, update.name
                ),
            }),
            Err(err) => errors.push(ErrorResult {
                name: update.name.clone(),
                reason: format!(
                    "merged dnf update failed ({merged_reason}) and verifying '{}' afterwards also failed ({err}); run `sudo anolisa --install-mode system repair {}`",
                    update.package, update.name
                ),
            }),
        }
    }
}

/// A merged `dnf install` failed. Degrade by fact: a member whose package is
/// present anyway landed on the host — record rpmdb truth (forward-only); an
/// absent member provably kept a clean slot and retries alone.
#[allow(clippy::too_many_arguments)]
fn degrade_merged_installs(
    installs: &[&PlannedInstall],
    merged_reason: &str,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    reporter: &dyn ProgressReporter,
    pending_installs: &mut Vec<PendingInstall>,
    errors: &mut Vec<ErrorResult>,
    warnings: &mut Vec<String>,
) {
    warnings.push(format!(
        "merged dnf install failed ({merged_reason}); retrying its members individually"
    ));
    for install in installs {
        match query.query_installed(&install.package) {
            Ok(Some(info)) => {
                warnings.push(format!(
                    "'{}' was installed despite the merged transaction failure; recording rpmdb truth",
                    install.package
                ));
                stage_install_observation(install, info, query, pending_installs, warnings);
            }
            Ok(None) => {
                reporter.report(&format!("Retrying {} individually...", install.name));
                match txn.install(&[install.package.as_str()]) {
                    Ok(()) => {
                        stage_refreshed_install(install, query, pending_installs, errors, warnings);
                    }
                    Err(err) => reconcile_failed_install(
                        install,
                        &txn_error_reason(err),
                        "retry",
                        query,
                        pending_installs,
                        errors,
                        warnings,
                    ),
                }
            }
            Err(err) => errors.push(ErrorResult {
                name: install.name.clone(),
                reason: format!(
                    "merged dnf install failed ({merged_reason}) and verifying '{}' afterwards also failed ({err}); state was not recorded",
                    install.package
                ),
            }),
        }
    }
}

fn reject_upgrade_pending_claims(
    layout: &FsLayout,
    operations: &[OperationRecord],
    command: &str,
) -> Result<(), CliError> {
    let journal_dir = rpm_install::journal_dir(layout);
    let evidence = JournalEvidence::new(&journal_dir, operations);
    let inventory = JournalInventory::load(evidence).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: err.to_string(),
    })?;

    // Reconciliation can write any delegated row, so recovery gates the root;
    // legacy parsing runs first to retain its proven component repair target.
    if let Some(pending) =
        rpm_install::find_pending_claim_in_inventory(layout, &[], command, &inventory)?
    {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "a previous RPM install for component '{}' (package '{}') is pending recovery; run `sudo anolisa --install-mode system repair {}` before retrying",
                pending.component, pending.package, pending.component
            ),
        });
    }

    let Some(entry) = inventory
        .entries()
        .iter()
        .find(|entry| entry.is_effectively_pending())
    else {
        return Ok(());
    };
    let reason = match entry.transaction().subject.as_deref() {
        Some(component) => format!(
            "component '{component}' has a pending lifecycle operation at {}; run `sudo anolisa --install-mode system repair {component}` before upgrading this state root",
            entry.path().display(),
        ),
        None => format!(
            "an unattributed lifecycle operation is pending at {}; upgrade cannot prove which component it owns — inspect the journal and settle recovery before retrying",
            entry.path().display()
        ),
    };
    Err(CliError::Runtime {
        command: command.to_string(),
        reason,
    })
}

/// Status for a completed real run: `ok` when nothing errored, `partial` when
/// some items succeeded and some failed, `failed` when nothing succeeded.
fn apply_status(succeeded: usize, error_count: usize) -> AppliedUpgradeStatus {
    if error_count == 0 {
        AppliedUpgradeStatus::Completed
    } else if succeeded > 0 {
        AppliedUpgradeStatus::Partial
    } else {
        AppliedUpgradeStatus::Failed
    }
}

/// Re-read the installed EVR of `package` after a transaction, mapping the
/// "gone/duplicate/query-failed" branches to a human reason string.
fn refresh_evr(query: &dyn PackageQuery, package: &str) -> Result<String, String> {
    match query.query_installed(package) {
        Ok(Some(info)) => Ok(info.version.to_string()),
        Ok(None) => Err(format!(
            "dnf upgraded '{package}' but it is no longer in rpmdb under that name"
        )),
        Err(err) => Err(format!(
            "dnf upgraded '{package}' but reading the new version failed: {err}"
        )),
    }
}

fn installed_origin_or_warn(
    query: &dyn PackageQuery,
    package: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    match query.installed_origin(package) {
        Ok(origin) => origin,
        Err(err) => {
            warnings.push(format!(
                "could not determine source repo for '{package}': {err}"
            ));
            None
        }
    }
}

/// Inspect existing RPM-backed component rows against rpmdb without mutating
/// state. Callers decide whether to preview or apply the returned changes.
// Keep every read-only evidence source explicit so preview and apply share the
// same reconciliation path without hiding mutable dependencies in a bundle.
#[allow(clippy::too_many_arguments)]
fn inspect_rpm_reconciliations(
    layout: &FsLayout,
    store: &StateStore,
    query: &dyn PackageQuery,
    excluded: &HashSet<String>,
    legacy_reconciliations: &[PlannedLegacyReconciliation],
    warnings: &mut Vec<String>,
    command: &str,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> ReconciliationInspection {
    let mut inspection = ReconciliationInspection::default();

    for installation in &store.installations {
        if installation.kind != ObjectKind::Component || excluded.contains(&installation.name) {
            continue;
        }
        let ProviderBinding::Delegated {
            package: identity,
            last_observed,
            ..
        } = &installation.binding
        else {
            continue;
        };
        let (package, allow_metadata_backfill) = match identity
            .resolved_name()
            .map(str::trim)
            .filter(|package| !package.is_empty())
        {
            Some(package) => (package.to_string(), false),
            None => match legacy_reconciliations
                .iter()
                .find(|candidate| candidate.name == installation.name)
            {
                Some(candidate) => (candidate.package.clone(), true),
                None => continue,
            },
        };

        let refreshed = match query.query_installed(&package) {
            Ok(Some(info)) => info,
            Ok(None) => {
                inspection.errors.push(ErrorResult {
                    name: installation.name.clone(),
                    reason: format!(
                        "RPM package '{package}' recorded for component '{}' is not present in rpmdb; state was not reconciled",
                        installation.name
                    ),
                });
                continue;
            }
            Err(PackageQueryError::UnexpectedOutput { detail, .. }) => {
                inspection.errors.push(ErrorResult {
                    name: installation.name.clone(),
                    reason: format!(
                        "rpm returned unexpected output for package '{package}' recorded for component '{}': {detail}; refusing to reconcile without one installed version",
                        installation.name
                    ),
                });
                continue;
            }
            Err(err) => {
                inspection.errors.push(ErrorResult {
                    name: installation.name.clone(),
                    reason: format!(
                        "failed to query RPM package '{package}' recorded for component '{}': {err}; state was not reconciled",
                        installation.name
                    ),
                });
                continue;
            }
        };

        let to = refreshed.version.to_string();
        // Current when the observation cache matches rpmdb (EVR + arch) and
        // the package identity is already resolved to this package. A
        // never-observed or unresolved record is drift by definition.
        let observation_current = identity.resolved_name() == Some(package.as_str())
            && last_observed.as_ref().is_some_and(|observed| {
                observed.evr.as_deref() == Some(to.as_str())
                    && observed.arch.as_deref() == Some(refreshed.arch.as_str())
            });
        // A same-version external RPM upgrade can still replace the packaged
        // component contract; compare it with the state snapshot so the drift
        // is reconciled even when the observation cache is current.
        let manifest = inspect_datadir_contract_drift(
            layout,
            &installation.name,
            command,
            packaged_data_probe,
        );
        warnings.extend(manifest.warnings);
        let reason = match (!observation_current, manifest.drifted) {
            (true, true) => "RPM state and component manifest drift",
            (true, false) => "RPM state drift",
            (false, true) => "component manifest drift",
            (false, false) => continue,
        };
        let source_repo = installed_origin_or_warn(query, &package, warnings);
        inspection.pending.push(PendingReconciliation {
            name: installation.name.clone(),
            package,
            from: recorded_version_label(last_observed.as_ref()),
            refreshed,
            source_repo,
            allow_metadata_backfill,
            reason,
        });
    }

    inspection
}

/// Display label for the recorded (pre-refresh) version of a delegated row.
fn recorded_version_label(observed: Option<&Observation>) -> String {
    observed
        .map(|observation| {
            observation
                .evr
                .clone()
                .unwrap_or_else(|| observation.version.clone())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn reconciliation_result(pending: &PendingReconciliation) -> ReconciledItem {
    ReconciledItem {
        name: pending.name.clone(),
        package: pending.package.clone(),
        from: pending.from.clone(),
        to: pending.refreshed.version.to_string(),
        reason: pending.reason,
    }
}

/// Whether a reconciliation's drift verdict obligates a contract snapshot
/// refresh (as opposed to a pure RPM state refresh).
fn reconciliation_requires_manifest_refresh(item: &ReconciledItem) -> bool {
    matches!(
        item.reason,
        "component manifest drift" | "RPM state and component manifest drift"
    )
}

/// Human-readable reason for a failed `dnf` transaction.
fn txn_error_reason(err: PackageTransactionError) -> String {
    match err {
        PackageTransactionError::CommandMissing { command } => {
            format!("{command} not found; cannot run the RPM transaction")
        }
        PackageTransactionError::PermissionDenied { command } => {
            format!("permission denied running {command}; re-run with sudo")
        }
        PackageTransactionError::TransactionFailed {
            operation,
            code,
            stderr,
            ..
        } => format!(
            "dnf {operation} failed (exit {}): {}",
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr.trim(),
        ),
    }
}

/// Build the dry-run preview from the transaction plan plus read-only rpmdb
/// reconciliation detection.
fn render_plan_preview(
    plan: &UpgradePlan,
    layout: &FsLayout,
    store: &StateStore,
    query: &dyn PackageQuery,
    install_preflight: Option<&dyn PackageTransaction>,
    command: &str,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> UpgradeEngineOutcome {
    let mut updated: Vec<UpdatedItem> = Vec::new();
    if let Some(cli) = &plan.cli {
        updated.push(planned_to_updated(cli));
    }
    updated.extend(plan.updates.iter().map(planned_to_updated));

    let mut installed: Vec<InstalledItem> = plan
        .installs
        .iter()
        .map(|install| InstalledItem {
            name: install.name.clone(),
            package: install.package.clone(),
            version: None,
        })
        .collect();
    let recorded: Vec<RecordedItem> = plan
        .observed_defaults
        .iter()
        .map(|observed| RecordedItem {
            name: observed.name.clone(),
            package: observed.package.clone(),
            version: None,
        })
        .collect();

    if plan.has_errors() {
        return UpgradeEngineOutcome::Preview {
            status: UpgradePreviewStatus::Blocked,
            report: UpgradeReport {
                target: plan.target.clone(),
                backend: BACKEND,
                updated,
                installed,
                reconciled: Vec::new(),
                recorded,
                skipped: skipped_results(plan),
                errors: plan_errors(plan),
            },
            warnings: Vec::new(),
        };
    }

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    if !installed.is_empty() {
        if let Some(txn) = install_preflight {
            // Match the merged install transaction so conflicts between defaults
            // are checked by the same native solver as conflicts with installed RPMs.
            let packages: Vec<&str> = installed.iter().map(|item| item.package.as_str()).collect();
            if let Err(err) = txn.check_install(&packages) {
                let reason = txn_error_reason(err);
                errors.extend(installed.drain(..).map(|item| ErrorResult {
                    name: item.name,
                    reason: reason.clone(),
                }));
            }
        } else {
            // DNF requires root even with --assumeno; keep unprivileged previews usable.
            warnings.push(
                "RPM install conflicts have not been checked: DNF preflight requires root; rerun with sudo (preserving any --target option), e.g. `sudo anolisa --install-mode system upgrade --dry-run`".to_string(),
            );
        }
    }

    // Planned component work will refresh these rows during finalize, so a
    // preview must not report the same component as both updated and reconciled.
    let excluded: HashSet<String> = plan
        .updates
        .iter()
        .map(|item| item.name.clone())
        .chain(plan.installs.iter().map(|item| item.name.clone()))
        .chain(plan.observed_defaults.iter().map(|item| item.name.clone()))
        .collect();
    let inspection = inspect_rpm_reconciliations(
        layout,
        store,
        query,
        &excluded,
        &plan.legacy_reconciliations,
        &mut warnings,
        command,
        packaged_data_probe,
    );
    let reconciled = inspection
        .pending
        .iter()
        .map(reconciliation_result)
        .collect::<Vec<_>>();
    errors.extend(inspection.errors);

    let status = match apply_status(
        updated.len() + installed.len() + recorded.len() + reconciled.len(),
        errors.len(),
    ) {
        AppliedUpgradeStatus::Completed => UpgradePreviewStatus::Completed,
        AppliedUpgradeStatus::Partial => UpgradePreviewStatus::Partial,
        AppliedUpgradeStatus::Failed => UpgradePreviewStatus::Failed,
    };

    UpgradeEngineOutcome::Preview {
        status,
        report: UpgradeReport {
            target: plan.target.clone(),
            backend: BACKEND,
            updated,
            installed,
            reconciled,
            recorded,
            skipped: skipped_results(plan),
            errors,
        },
        warnings,
    }
}

/// Build a blocked report: the plan carried errors, so no dnf ran.
fn blocked_report(plan: &UpgradePlan) -> UpgradeReport {
    UpgradeReport {
        target: plan.target.clone(),
        backend: BACKEND,
        updated: Vec::new(),
        installed: Vec::new(),
        reconciled: Vec::new(),
        recorded: Vec::new(),
        skipped: skipped_results(plan),
        errors: plan_errors(plan),
    }
}

fn planned_to_updated(update: &PlannedUpdate) -> UpdatedItem {
    UpdatedItem {
        name: update.name.clone(),
        package: update.package.clone(),
        from: update.from.clone(),
        to: update.to.clone(),
    }
}

fn skipped_results(plan: &UpgradePlan) -> Vec<SkippedResult> {
    plan.skipped
        .iter()
        .map(|item| SkippedResult {
            name: item.name.clone(),
            reason: item.reason.clone(),
        })
        .collect()
}

fn plan_errors(plan: &UpgradePlan) -> Vec<ErrorResult> {
    plan.errors
        .iter()
        .map(|item| ErrorResult {
            name: item.name.clone(),
            reason: item.reason.clone(),
        })
        .collect()
}

// ── state persistence + audit ────────────────────────────────────────────────

/// Refresh ANOLISA state for successful component work and write the durable
/// audit (operation record + central log) while the caller holds the install
/// lock used to authorize the RPM transactions.
///
/// Audit is decoupled from component-state changes: because a real dnf
/// transaction may have run even when nothing lands in `installed.toml` (a
/// CLI-only upgrade, or a run where every component drifted/failed), and some
/// already-current defaults only need a state record, this appends an operation
/// record and central-log entry whenever real work was attempted. The recorded
/// status is the true `ok` / `partial` / `failed` outcome of the whole command
/// (`cli_updated` and `prior_errors` from the transaction phase are folded in),
/// not just the component-persistence result.
///
/// Each pending change is still re-validated against the locked state before it
/// is recorded:
/// - **updates**: the component must still exist and still be the same RPM
///   package (mirrors the single-component update guard); otherwise the new EVR
///   is not grafted on and the item is reported as an error.
/// - **installs**: a name absent from state is inserted as rpm-managed; a name
///   already present under the *same* RPM package is refreshed idempotently; a
///   name present under a **different** backend/package (e.g. a raw-managed
///   component, or a different RPM) is left untouched and reported as an error,
///   so a concurrent install can never be silently overwritten with an
///   rpm-managed record.
///
/// Only the changes actually recorded are returned as updated/installed; every
/// refused change is returned as an error so the caller downgrades the overall
/// status rather than claiming a clean `ok` while state is stale. RPM-owned
/// files are never recorded — dnf owns the file transaction.
fn finalize_upgrade(req: FinalizeUpgrade<'_>) -> Result<PersistOutcome, CliError> {
    let FinalizeUpgrade {
        ctx,
        layout,
        command,
        state,
        audit,
        query,
        cli_updated,
        updates,
        installs,
        legacy_reconciliations,
        prior_errors,
        warnings,
    } = req;
    let mut outcome = PersistOutcome::default();

    for update in updates {
        let evr = update.refreshed.version.to_string();
        let Some(installation) = state.find_mut(ObjectKind::Component, &update.name) else {
            if update.adopt_if_missing {
                state.upsert(new_observed_delegated_component(
                    &update.name,
                    &update.package,
                    &update.refreshed,
                    update.source_repo.as_deref(),
                    &audit.started_at,
                    &audit.operation_id,
                ));
                if update.record_only {
                    outcome.recorded.push(RecordedItem {
                        name: update.name.clone(),
                        package: update.package.clone(),
                        version: Some(evr),
                    });
                } else {
                    outcome.updated.push(UpdatedItem {
                        name: update.name.clone(),
                        package: update.package.clone(),
                        from: update.from.clone(),
                        to: evr,
                    });
                }
                continue;
            }
            outcome.errors.push(ErrorResult {
                name: update.name.clone(),
                reason: format!(
                    "dnf upgraded '{}' but component '{}' vanished from ANOLISA state during the upgrade; run `sudo anolisa --install-mode system repair {}` to refresh it",
                    update.package, update.name, update.name
                ),
            });
            continue;
        };
        // Refuse to graft the new EVR onto a row that is no longer this RPM
        // package (a concurrent backend change), mirroring the single-component
        // update guard.
        if !is_matching_or_legacy_delegated(
            installation,
            &update.package,
            update.backfill_rpm_metadata,
        ) {
            outcome.errors.push(ErrorResult {
                name: update.name.clone(),
                reason: format!(
                    "dnf upgraded '{}' but component '{}' changed ownership/package in ANOLISA state during the upgrade; state was not refreshed — run `sudo anolisa --install-mode system repair {}`",
                    update.package, update.name, update.name
                ),
            });
            continue;
        }
        refresh_delegated_observation(
            installation,
            &update.package,
            &update.refreshed,
            update.source_repo.as_deref(),
            &audit.started_at,
            &audit.operation_id,
        );
        if update.record_only {
            outcome.recorded.push(RecordedItem {
                name: update.name.clone(),
                package: update.package.clone(),
                version: Some(evr),
            });
        } else {
            outcome.updated.push(UpdatedItem {
                name: update.name.clone(),
                package: update.package.clone(),
                from: update.from.clone(),
                to: evr,
            });
        }
    }

    for install in installs {
        let evr = install.refreshed.version.to_string();
        match classify_install_slot(state, &install.name, &install.package) {
            InstallSlot::Absent => {
                state.upsert(new_managed_delegated_component(
                    &install.name,
                    &install.package,
                    &install.refreshed,
                    install.source_repo.as_deref(),
                    &audit.started_at,
                    &audit.operation_id,
                ));
                outcome.installed.push(InstalledItem {
                    name: install.name.clone(),
                    package: install.package.clone(),
                    version: Some(evr),
                });
            }
            // A row already present under the same RPM package: treat the
            // install as an idempotent refresh rather than a duplicate insert.
            InstallSlot::MatchingRpm => {
                if let Some(installation) = state.find_mut(ObjectKind::Component, &install.name) {
                    refresh_delegated_observation(
                        installation,
                        &install.package,
                        &install.refreshed,
                        install.source_repo.as_deref(),
                        &audit.started_at,
                        &audit.operation_id,
                    );
                }
                outcome.installed.push(InstalledItem {
                    name: install.name.clone(),
                    package: install.package.clone(),
                    version: Some(evr),
                });
            }
            // A row present under a different backend/package: never overwrite
            // it (that would erase raw-managed or foreign-RPM provenance).
            InstallSlot::Conflict(existing_ownership) => {
                outcome.errors.push(ErrorResult {
                    name: install.name.clone(),
                    reason: format!(
                        "dnf installed '{}' but a {existing_ownership} component named '{}' already exists in ANOLISA state; refusing to overwrite it with an rpm-managed record — run `anolisa status {}`",
                        install.package, install.name, install.name
                    ),
                });
            }
        }
    }

    // Pending update/install/default changes are already reflected in memory.
    // Excluding their names makes the reporting invariant explicit: one
    // component cannot be both transaction-updated and reconciled in this run.
    let excluded: HashSet<String> = outcome
        .updated
        .iter()
        .map(|item| item.name.clone())
        .chain(outcome.installed.iter().map(|item| item.name.clone()))
        .chain(outcome.recorded.iter().map(|item| item.name.clone()))
        .collect();
    let inspection = inspect_rpm_reconciliations(
        layout,
        state,
        query,
        &excluded,
        legacy_reconciliations,
        warnings,
        command,
        ctx.packaged_data_probe(),
    );
    // Apply errors do not exclude a component from the sweep: a transient
    // post-transaction query may recover here and still reconcile state. If
    // the retry fails again, keep the original item error instead of counting
    // the same component twice in the result and durable operation summary.
    let prior_error_names: HashSet<&str> = prior_errors
        .iter()
        .map(|error| error.name.as_str())
        .collect();
    outcome.errors.extend(
        inspection
            .errors
            .into_iter()
            .filter(|error| !prior_error_names.contains(error.name.as_str())),
    );

    for reconciliation in inspection.pending {
        let Some(installation) = state.find_mut(ObjectKind::Component, &reconciliation.name) else {
            outcome.errors.push(ErrorResult {
                name: reconciliation.name.clone(),
                reason: format!(
                    "component '{}' disappeared from ANOLISA state during RPM reconciliation for package '{}'; state was not changed",
                    reconciliation.name, reconciliation.package
                ),
            });
            continue;
        };
        if !is_matching_or_legacy_delegated(
            installation,
            &reconciliation.package,
            reconciliation.allow_metadata_backfill,
        ) {
            outcome.errors.push(ErrorResult {
                name: reconciliation.name.clone(),
                reason: format!(
                    "component '{}' changed ownership/package during RPM reconciliation for package '{}'; state was not changed",
                    reconciliation.name, reconciliation.package
                ),
            });
            continue;
        }

        refresh_delegated_observation(
            installation,
            &reconciliation.package,
            &reconciliation.refreshed,
            reconciliation.source_repo.as_deref(),
            &audit.started_at,
            &audit.operation_id,
        );
        outcome
            .reconciled
            .push(reconciliation_result(&reconciliation));
    }

    // Count the whole command's outcome, not just component state: the CLI
    // update (not an ANOLISA object) and the transaction-phase errors are folded
    // in so the durable record shows the true `ok` / `partial` / `failed`.
    let initial_total_success = cli_updated.is_some() as usize
        + outcome.updated.len()
        + outcome.installed.len()
        + outcome.reconciled.len()
        + outcome.recorded.len();
    let initial_total_errors = prior_errors.len() + outcome.errors.len();

    if initial_total_success == 0 && initial_total_errors == 0 {
        // No transaction actually happened (e.g. only skips/noops reached here);
        // nothing to audit and nothing to persist.
        return Ok(outcome);
    }

    // Contract snapshot refreshes happen only after this state save, so count
    // every pending refresh as an error for now: a crash or failed final save
    // can then never leave a durable `ok` that overstates what completed.
    let required_manifest_refreshes = outcome
        .reconciled
        .iter()
        .filter(|item| reconciliation_requires_manifest_refresh(item))
        .count();
    let transaction_manifest_refreshes =
        outcome.updated.len() + outcome.installed.len() + outcome.recorded.len();
    let provisional_status = apply_status(
        initial_total_success - required_manifest_refreshes,
        initial_total_errors + required_manifest_refreshes + transaction_manifest_refreshes,
    );

    // Always append the operation record and save when real work or an item
    // error occurred, even if no component object changed (for example a
    // CLI-only upgrade or failed rpmdb query). The record keeps the attempt
    // auditable via `anolisa logs`.
    state.operations.push(OperationRecord {
        id: audit.operation_id.clone(),
        command: command.to_string(),
        status: provisional_status.as_str().to_string(),
        started_at: audit.started_at.clone(),
        finished_at: Some(now_iso8601()),
        parent_operation_id: None,
    });

    let state_path = layout.state_dir.join("installed.toml");
    state.save(&state_path).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to save state: {err}"),
    })?;
    outcome.operation_id = Some(audit.operation_id.clone());

    // Refresh package-owned component contracts only after state persisted.
    // This heals stale snapshots left by external yum/dnf upgrades without
    // exposing a new contract when the corresponding state write failed.
    for (component, package) in outcome
        .updated
        .iter()
        .map(|item| (item.name.as_str(), item.package.as_str()))
        .chain(
            outcome
                .installed
                .iter()
                .map(|item| (item.name.as_str(), item.package.as_str())),
        )
        .chain(
            outcome
                .recorded
                .iter()
                .map(|item| (item.name.as_str(), item.package.as_str())),
        )
    {
        let refresh = refresh_datadir_contract_snapshot(
            layout,
            component,
            command,
            ctx.packaged_data_probe(),
        );
        let failure_detail = refresh.error_detail();
        warnings.extend(refresh.warnings);
        if let Some(detail) = failure_detail {
            outcome.errors.push(ErrorResult {
                name: component.to_string(),
                reason: format!(
                    "component manifest refresh after transaction for package '{package}' did not complete: {detail}"
                ),
            });
        }
    }

    let mut reconciled = Vec::with_capacity(outcome.reconciled.len());
    for item in std::mem::take(&mut outcome.reconciled) {
        if !reconciliation_requires_manifest_refresh(&item) {
            reconciled.push(item);
            continue;
        }
        let refresh = refresh_datadir_contract_snapshot(
            layout,
            &item.name,
            command,
            ctx.packaged_data_probe(),
        );
        let failure_detail = refresh.failure_detail();
        warnings.extend(refresh.warnings);
        if let Some(detail) = failure_detail {
            outcome.errors.push(ErrorResult {
                name: item.name,
                reason: format!(
                    "component manifest reconciliation for package '{}' did not complete: {detail}",
                    item.package
                ),
            });
            continue;
        }
        reconciled.push(item);
    }
    outcome.reconciled = reconciled;

    let total_success = cli_updated.is_some() as usize
        + outcome.updated.len()
        + outcome.installed.len()
        + outcome.reconciled.len()
        + outcome.recorded.len();
    let total_errors = prior_errors.len() + outcome.errors.len();
    let final_status = apply_status(total_success, total_errors);
    let mut persisted_status = final_status;
    let mut final_status_save_failure = None;
    if final_status != provisional_status {
        if let Some(operation) = state.operations.last_mut() {
            operation.status = final_status.as_str().to_string();
        }
        if let Err(err) = state.save(&state_path) {
            if let Some(operation) = state.operations.last_mut() {
                operation.status = provisional_status.as_str().to_string();
            }
            persisted_status = provisional_status;
            let detail = format!(
                "could not finalize the upgrade operation as '{}': {err}",
                final_status.as_str()
            );
            warnings.push(detail.clone());
            final_status_save_failure = Some(detail);
        }
    }

    let (log_status, severity) = match persisted_status {
        AppliedUpgradeStatus::Completed => (LogStatus::Ok, Severity::Info),
        AppliedUpgradeStatus::Partial => (LogStatus::Partial, Severity::Warn),
        AppliedUpgradeStatus::Failed => (LogStatus::Failed, Severity::Error),
    };

    // Audit log is best-effort: state already persisted, so a log failure
    // downgrades to a stderr warning rather than unwinding.
    let recorded = outcome.updated.len()
        + outcome.installed.len()
        + outcome.reconciled.len()
        + outcome.recorded.len();
    let log = CentralLog::open(layout.central_log.clone());
    let mut objects: Vec<String> = cli_updated.map(|c| c.package.clone()).into_iter().collect();
    objects.extend(outcome.updated.iter().map(|u| u.name.clone()));
    objects.extend(outcome.installed.iter().map(|i| i.name.clone()));
    objects.extend(outcome.reconciled.iter().map(|i| i.name.clone()));
    objects.extend(outcome.recorded.iter().map(|i| i.name.clone()));
    let record = LogRecord {
        kind: LogKind::Operation,
        operation_id: Some(audit.operation_id.clone()),
        command: command.to_string(),
        source: "anolisa-cli".to_string(),
        component: None,
        severity,
        message: format!(
            "applied RPM image upgrade ({total_success} succeeded, {recorded} component state change(s) recorded, {total_errors} error(s))"
        ),
        actor: "cli".to_string(),
        install_mode: Some(ctx.install_mode.as_str().to_string()),
        started_at: audit.started_at.clone(),
        finished_at: Some(now_iso8601()),
        status: Some(log_status),
        objects,
        backup_ids: Vec::new(),
        warnings: warnings.to_vec(),
        details: serde_json::Value::Null,
    };
    if let Err(err) = log.append(&record)
        && !ctx.quiet
    {
        // Routed through `suspend_output` so this warning never collides with
        // the "Finalizing ANOLISA state..." spinner frame (issue #1452).
        progress::suspend_output(|| {
            eprintln!("warning: failed to write central log: {err}");
        });
    }

    if let Some(detail) = final_status_save_failure {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "upgrade changes were saved with conservative status '{}', but {detail}",
                provisional_status.as_str()
            ),
        });
    }

    Ok(outcome)
}

/// The current ANOLISA-state slot for a to-be-installed default, classified
/// under the install lock so the install decision cannot race a concurrent write.
enum InstallSlot {
    /// No object with this name exists — a clean insert.
    Absent,
    /// An object exists under the same RPM package — an idempotent refresh.
    MatchingRpm,
    /// An object exists under a different backend/package (carries its ownership
    /// label for the error) — must not be overwritten.
    Conflict(&'static str),
}

fn classify_install_slot(store: &StateStore, name: &str, package: &str) -> InstallSlot {
    match store.find(ObjectKind::Component, name) {
        None => InstallSlot::Absent,
        Some(existing) if is_matching_delegated(existing, package) => InstallSlot::MatchingRpm,
        Some(existing) => InstallSlot::Conflict(provenance_label(existing)),
    }
}

/// Provenance label for guard-refusal messages: `owned` for an ANOLISA-owned
/// artifact, the management relation (`managed` / `adopted` / `observed`) for
/// a delegated record.
fn provenance_label(installation: &Installation) -> &'static str {
    match &installation.binding {
        ProviderBinding::Owned { .. } => "owned",
        ProviderBinding::Delegated { relation, .. } => relation.label(),
    }
}

fn is_matching_delegated(installation: &Installation, package: &str) -> bool {
    matches!(
        &installation.binding,
        ProviderBinding::Delegated { package: identity, .. }
            if identity.resolved_name() == Some(package)
    )
}

fn is_matching_or_legacy_delegated(
    installation: &Installation,
    package: &str,
    allow_metadata_backfill: bool,
) -> bool {
    match &installation.binding {
        ProviderBinding::Owned { .. } => false,
        ProviderBinding::Delegated {
            package: identity, ..
        } => match identity.resolved_name().map(str::trim) {
            Some(resolved) if !resolved.is_empty() => resolved == package,
            // An unresolved/blank identity may only be claimed when the plan
            // explicitly carries a metadata backfill for this row.
            _ => allow_metadata_backfill,
        },
    }
}

/// Refresh a delegated row from post-transaction rpmdb truth: resolve the
/// package identity (metadata backfill for legacy rows), replace the
/// observation cache, and stamp the operation. The caller has already proven
/// the row matches this package via [`is_matching_or_legacy_delegated`].
fn refresh_delegated_observation(
    installation: &mut Installation,
    package: &str,
    refreshed: &PackageInfo,
    source_repo: Option<&str>,
    observed_at: &str,
    operation_id: &str,
) {
    if let ProviderBinding::Delegated {
        package: identity,
        last_observed,
        ..
    } = &mut installation.binding
    {
        *identity = PackageIdentity::Resolved {
            name: package.to_string(),
        };
        let mut observation = observation_from(refreshed, source_repo, observed_at);
        // A failed origin lookup must not erase a previously known source repo.
        if observation.source_repo.is_none() {
            observation.source_repo = last_observed
                .as_ref()
                .and_then(|prior| prior.source_repo.clone());
        }
        *last_observed = Some(observation);
    }
    installation.status = LifecycleStatus::Installed;
    installation.last_operation_id = Some(operation_id.to_string());
}

/// Build a fresh [`Observation`] from post-transaction rpmdb truth.
fn observation_from(
    info: &PackageInfo,
    source_repo: Option<&str>,
    observed_at: &str,
) -> Observation {
    Observation {
        version: info.version.version.clone(),
        evr: Some(info.version.to_string()),
        arch: Some(info.arch.clone()),
        source_repo: source_repo
            .map(str::to_string)
            .or_else(|| info.origin.clone()),
        observed_at: observed_at.to_string(),
    }
}

/// Build an observed delegated record for a target default that was already
/// installed on the host but absent from ANOLISA state. `upgrade` updated the
/// RPM package, but ANOLISA still does not own its removal.
fn new_observed_delegated_component(
    name: &str,
    package: &str,
    refreshed: &PackageInfo,
    source_repo: Option<&str>,
    installed_at: &str,
    operation_id: &str,
) -> Installation {
    new_delegated_component(
        name,
        package,
        ManagementRelation::Observed,
        refreshed,
        source_repo,
        installed_at,
        operation_id,
    )
}

/// Build a managed delegated record for a newly installed default. Mirrors
/// the delegated-install path: ANOLISA ran the native transaction, so default
/// uninstall delegates removal back to dnf.
fn new_managed_delegated_component(
    name: &str,
    package: &str,
    refreshed: &PackageInfo,
    source_repo: Option<&str>,
    installed_at: &str,
    operation_id: &str,
) -> Installation {
    new_delegated_component(
        name,
        package,
        ManagementRelation::Managed {
            since: installed_at.to_string(),
        },
        refreshed,
        source_repo,
        installed_at,
        operation_id,
    )
}

fn new_delegated_component(
    name: &str,
    package: &str,
    relation: ManagementRelation,
    refreshed: &PackageInfo,
    source_repo: Option<&str>,
    installed_at: &str,
    operation_id: &str,
) -> Installation {
    Installation {
        kind: ObjectKind::Component,
        name: name.to_string(),
        scope: InstallationScope::System,
        binding: ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package: PackageIdentity::Resolved {
                name: package.to_string(),
            },
            relation,
            last_observed: Some(observation_from(refreshed, source_repo, installed_at)),
        },
        status: LifecycleStatus::Installed,
        installed_at: installed_at.to_string(),
        last_operation_id: Some(operation_id.to_string()),
        subscription_scope: Default::default(),
        enabled_features: Vec::new(),
        health: Vec::new(),
    }
}

/// RFC3339 UTC timestamp, seconds precision (matches the install/update paths).
fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ── rendering ────────────────────────────────────────────────────────────────

/// Render the upgrade result as JSON (envelope) or a human summary.
fn render_result(ctx: &CliContext, result: &UpgradeResult) {
    if ctx.json {
        // `ok` reflects the status so machine callers can gate on the envelope
        // flag as well as the inner `status` field; a serialize failure is
        // ignored so an already-applied upgrade is not reported as failed.
        let _ = response::render_json_with_status(COMMAND, result.status == STATUS_OK, result);
        return;
    }
    if ctx.quiet {
        return;
    }
    let color = Palette::new(ctx.no_color);

    let mut header = format!("upgrade (backend: {}", result.backend);
    if let Some(target) = &result.target {
        header.push_str(&format!(", target: {target}"));
    }
    header.push_str(if result.dry_run {
        ") (dry-run — nothing applied)"
    } else {
        ")"
    });
    println!("{}", color.command(header));

    for item in &result.updated {
        let verb = if result.dry_run {
            "would update"
        } else {
            "updated"
        };
        println!(
            "  {} {} {} → {} {}",
            color.ok(format!("✓ {verb}")),
            item.name,
            item.from,
            item.to,
            color.muted(format!("({})", item.package)),
        );
    }
    for item in &result.installed {
        let verb = if result.dry_run {
            "would install".to_string()
        } else {
            format!("installed {}", item.version.as_deref().unwrap_or("-"))
        };
        println!(
            "  {} {} {}",
            color.ok(format!("✓ {verb}")),
            item.name,
            color.muted(format!("({})", item.package)),
        );
    }
    for item in &result.reconciled {
        let verb = if result.dry_run {
            "would reconcile"
        } else {
            "reconciled"
        };
        println!(
            "  {} {} {} → {} {}",
            color.ok(format!("✓ {verb}")),
            item.name,
            item.from,
            item.to,
            color.muted(format!("({}; {})", item.package, item.reason)),
        );
    }
    for item in &result.recorded {
        let verb = if result.dry_run {
            "would record".to_string()
        } else {
            format!("recorded {}", item.version.as_deref().unwrap_or("-"))
        };
        println!(
            "  {} {} {}",
            color.ok(format!("✓ {verb}")),
            item.name,
            color.muted(format!("({})", item.package)),
        );
    }
    for item in &result.skipped {
        println!(
            "  {} {} {}",
            color.muted("• skipped"),
            item.name,
            color.muted(format!("({})", item.reason)),
        );
    }
    for item in &result.errors {
        println!(
            "  {} {} {}",
            color.err("✗ error"),
            item.name,
            color.warn(format!("({})", item.reason)),
        );
    }

    println!(
        "{} {} updated, {} installed, {} reconciled, {} recorded, {} skipped, {} error(s) [{}]",
        color.label("summary:"),
        result.updated.len(),
        result.installed.len(),
        result.reconciled.len(),
        result.recorded.len(),
        result.skipped.len(),
        result.errors.len(),
        result.status,
    );

    for warning in &result.warnings {
        eprintln!("{} {warning}", color.warn("warning:"));
    }
}

#[cfg(test)]
mod tests;
