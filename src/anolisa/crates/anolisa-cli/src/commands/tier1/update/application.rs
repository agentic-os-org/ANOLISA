//! Application orchestration for the single-component `update` lifecycle verb.

use std::path::Path;

use anolisa_core::domain::{InstallationScope, NativePm, OwnedArtifact, ProviderBinding};
use anolisa_core::execution::{
    CommandOutcome, CommandOutcomeStatus, ExecutionIntent, PreparedExecution,
};
use anolisa_core::executor::{DelegatedExecutionTarget, PHASE_NATIVE_TXN, execute_delegated_steps};
use anolisa_core::facts::JournalEvidence;
use anolisa_core::lock::InstallLock;
use anolisa_core::owned_executor::execute_owned_steps;
use anolisa_core::planner::{Intent, Step, UpdateRequest, plan};
use anolisa_core::providers::DelegatedProvider;
use anolisa_core::record_sink::{DelegatedIdentity, RecordContext, StoreRecordSink};
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_store::StateStore;
use anolisa_core::{Transaction, TransactionOutcomeStatus, TransactionStepStatus};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};
use anolisa_platform::privilege;

use crate::commands::common;
use crate::commands::tier1::install::{RawReplayOps, RawResolution};
use crate::commands::tier1::recovery::LockedJournalGate;
use crate::commands::tier1::rpm_install;
use crate::context::CliContext;
use crate::response::CliError;

use super::{
    AdapterAction, COMMAND, PlannedComponentUpdate, PlannedUpdateRoute,
    adapter_actions_after_update, adapter_revision_snapshot, append_update_log,
    complete_delegated_update, native_observed_version, native_update_authorized, now_iso8601,
    observe_update_facts, owned_error_to_cli, plan_component_update, plan_error_to_cli,
    tooling_missing_err, update_backends,
};

/// CLI input mapped to the lifecycle execution protocol.
pub(super) struct ApplicationRequest<'a> {
    pub(super) component: &'a str,
    pub(super) intent: ExecutionIntent,
}

/// Resolved component and version evidence carried to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UpdateSubject {
    pub(super) component: String,
    pub(super) package: Option<String>,
    pub(super) from_version: Option<String>,
    pub(super) to_version: Option<String>,
}

/// Durable provider change completed by an applied update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateChange {
    /// ANOLISA-owned files were replaced by the resolved artifact.
    OwnedArtifactUpdated,
    /// A delegated native package transaction changed the observed version.
    NativePackageUpdated,
}

/// Batch-facing classification of a single-component update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpdateBatchOutcome {
    /// The update completed or no change was required.
    Completed(super::UpdateOutcome),
    /// Effects occurred or cannot be excluded, so repair is required.
    Partial { reason: String },
    /// The update failed without a known partial state.
    Failed { reason: String },
}

/// Terminal application failure retained for batch classification.
#[derive(Debug)]
pub(super) enum ApplicationFailure {
    /// Effects occurred or cannot be excluded, so repair is required.
    Partial(CliError),
    /// The application failed without a known partial state.
    Failed(CliError),
}

impl ApplicationFailure {
    /// Classify an execution failure from its in-memory journal evidence.
    fn from_journal(error: CliError, journal: &Transaction) -> Self {
        let native_transaction_committed = journal.steps.iter().any(|step| {
            step.phase == PHASE_NATIVE_TXN && step.status == TransactionStepStatus::Done
        });
        if journal.status == TransactionOutcomeStatus::Partial || native_transaction_committed {
            Self::Partial(error)
        } else {
            Self::Failed(error)
        }
    }

    /// Restore the existing single-command error projection.
    pub(super) fn into_cli_error(self) -> CliError {
        match self {
            Self::Partial(error) | Self::Failed(error) => error,
        }
    }
}

impl From<CliError> for ApplicationFailure {
    fn from(error: CliError) -> Self {
        Self::Failed(error)
    }
}

/// Typed application result consumed by the command renderer.
pub(super) enum ApplicationOutcome {
    /// The owned record already points at the latest resolved artifact.
    NoOp { subject: UpdateSubject },
    /// Plan-only result; no lock or effect executor was acquired.
    Preview {
        subject: UpdateSubject,
        steps: Vec<Step>,
    },
    /// Applied result with durable operation evidence.
    Applied {
        command: String,
        subject: UpdateSubject,
        steps: Vec<Step>,
        outcome: CommandOutcome<UpdateChange>,
        adapter_actions: Vec<AdapterAction>,
    },
}

impl ApplicationOutcome {
    /// Project the result to the batch-member terminal classification.
    pub(super) fn batch_outcome(&self) -> UpdateBatchOutcome {
        match self {
            Self::NoOp { .. } => {
                UpdateBatchOutcome::Completed(super::UpdateOutcome::AlreadyCurrent)
            }
            Self::Preview { .. } => UpdateBatchOutcome::Completed(super::UpdateOutcome::Updated),
            Self::Applied {
                subject, outcome, ..
            } => match outcome.status() {
                CommandOutcomeStatus::Completed => {
                    UpdateBatchOutcome::Completed(if outcome.changes().is_empty() {
                        super::UpdateOutcome::AlreadyCurrent
                    } else {
                        super::UpdateOutcome::Updated
                    })
                }
                CommandOutcomeStatus::Partial { reason } => UpdateBatchOutcome::Partial {
                    reason: format!(
                        "the update of '{}' committed, but {reason}; run `anolisa repair {}` to reconcile",
                        subject.component, subject.component
                    ),
                },
                CommandOutcomeStatus::Failed { reason } => UpdateBatchOutcome::Failed {
                    reason: reason.clone(),
                },
            },
        }
    }

    /// Returns warnings that a batch caller must surface without rendering a
    /// per-component result envelope.
    pub(super) fn warnings(&self) -> &[String] {
        match self {
            Self::NoOp { .. } | Self::Preview { .. } => &[],
            Self::Applied { outcome, .. } => outcome.warnings(),
        }
    }

    /// Returns adapter follow-up actions produced by an applied update.
    pub(super) fn adapter_actions(&self) -> &[AdapterAction] {
        match self {
            Self::Applied {
                adapter_actions, ..
            } => adapter_actions,
            Self::NoOp { .. } | Self::Preview { .. } => &[],
        }
    }
}

/// Run one component update against production package backends.
pub(super) fn run(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
) -> Result<ApplicationOutcome, CliError> {
    let (query, txn) = update_backends(request.component, ctx)?;
    run_with_dependencies(request, ctx, &query, &txn, privilege::is_root())
}

/// Run one batch member while preserving its terminal classification.
pub(super) fn run_for_batch(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
) -> Result<ApplicationOutcome, ApplicationFailure> {
    let (query, txn) = update_backends(request.component, ctx)?;
    run_with_dependencies_classified(request, ctx, &query, &txn, privilege::is_root())
}

/// Run the single-component application protocol with explicit host boundaries.
pub(super) fn run_with_dependencies(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
) -> Result<ApplicationOutcome, CliError> {
    run_with_dependencies_classified(request, ctx, query, txn, is_root)
        .map_err(ApplicationFailure::into_cli_error)
}

/// Run the application protocol while preserving its terminal classification.
pub(super) fn run_with_dependencies_classified(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
) -> Result<ApplicationOutcome, ApplicationFailure> {
    let planned = plan_component_update(request.component, ctx, query, txn)?;
    let prepared = request.intent.prepare(planned.plan.clone());
    execute_planned_update(planned, prepared, ctx, query, txn, is_root)
}

fn execute_planned_update(
    planned: PlannedComponentUpdate,
    prepared: PreparedExecution,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
) -> Result<ApplicationOutcome, ApplicationFailure> {
    let PlannedComponentUpdate {
        command,
        target,
        native_package,
        scope,
        now,
        owned_execution,
        owned_versions,
        native_from,
        plan: _,
        route,
    } = planned;
    let layout = common::resolve_layout(ctx);
    let state_path = layout.state_dir.join("installed.toml");
    let journal_dir = rpm_install::journal_dir(&layout);

    match prepared {
        PreparedExecution::NoOp { .. } => {
            debug_assert!(matches!(route, PlannedUpdateRoute::AlreadyCurrent));
            let (from_version, to_version) = owned_versions
                .map(|(from, to)| (Some(from), Some(to)))
                .unwrap_or((None, None));
            let package = owned_execution
                .map(|(resolution, _)| resolution.package)
                .or(native_package);
            Ok(ApplicationOutcome::NoOp {
                subject: UpdateSubject {
                    component: target,
                    package,
                    from_version,
                    to_version,
                },
            })
        }
        PreparedExecution::Preview { steps, .. } => {
            let (package, from_version, to_version) = match route {
                PlannedUpdateRoute::Owned => (
                    owned_execution
                        .map(|(resolution, _)| resolution.package)
                        .or(native_package),
                    owned_versions.as_ref().map(|(from, _)| from.clone()),
                    owned_versions.map(|(_, to)| to),
                ),
                PlannedUpdateRoute::Delegated { .. } => (native_package, native_from, None),
                PlannedUpdateRoute::AlreadyCurrent => {
                    unreachable!("a no-op plan cannot become preview-ready")
                }
            };
            Ok(ApplicationOutcome::Preview {
                subject: UpdateSubject {
                    component: target,
                    package,
                    from_version,
                    to_version,
                },
                steps,
            })
        }
        PreparedExecution::Apply { steps, .. } => match route {
            PlannedUpdateRoute::Owned => {
                let (resolution, prior) = owned_execution.ok_or_else(|| CliError::Runtime {
                    command: command.clone(),
                    reason: format!(
                        "internal: planner produced an owned plan but no resolution was prepared for '{target}'"
                    ),
                })?;
                apply_owned(
                    &target,
                    ctx,
                    &layout,
                    &state_path,
                    &journal_dir,
                    scope,
                    &now,
                    steps,
                    resolution,
                    prior,
                    &command,
                )
            }
            PlannedUpdateRoute::Delegated { .. } => apply_delegated(
                &target,
                ctx,
                &layout,
                &state_path,
                &journal_dir,
                scope,
                &now,
                native_package,
                &command,
                query,
                txn,
                is_root,
            ),
            PlannedUpdateRoute::AlreadyCurrent => {
                unreachable!("a no-op route cannot become apply-ready")
            }
        },
    }
}

#[expect(clippy::too_many_arguments)]
fn apply_delegated(
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    journal_dir: &Path,
    scope: InstallationScope,
    now: &str,
    native_package: Option<String>,
    command: &str,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
) -> Result<ApplicationOutcome, ApplicationFailure> {
    // Native transactions require root. Refuse before acquiring the lock so
    // the user sees the CLI policy instead of dnf's mid-transaction error.
    if !is_root {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "updating system RPM '{}' requires root privileges; re-run with sudo: `sudo anolisa update {target}`",
                native_package.as_deref().unwrap_or(target)
            ),
        }
        .into());
    }

    let provider = DelegatedProvider::new(query, txn);
    let _lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;
    if !native_update_authorized(&store, target, native_package.as_deref()) {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "component '{target}' changed while this update was planning; nothing was changed — re-run `anolisa update {target}`"
            ),
        }
        .into());
    }
    // The lock-time snapshot is the execution authority. It prevents a
    // pre-lock package or journal observation from authorizing stale steps.
    let locked_facts = observe_update_facts(
        target,
        scope,
        native_package.as_deref(),
        now,
        &store,
        &provider,
        layout,
        journal_dir,
        command,
    )?;
    let locked_plan = plan(
        &Intent::Update(UpdateRequest {
            owned_resolution: None,
        }),
        &locked_facts,
    )
    .map_err(|err| plan_error_to_cli(err, target, command, None))?;
    let steps = match ExecutionIntent::Apply.prepare(locked_plan) {
        PreparedExecution::Apply { steps, .. } => steps,
        PreparedExecution::NoOp { .. } => {
            unreachable!("delegated update plans always retain the native transaction")
        }
        PreparedExecution::Preview { .. } => {
            unreachable!("ExecutionIntent::Apply cannot produce a preview")
        }
    };
    let from_version = native_observed_version(&locked_facts);
    let prior_adapter_revisions = adapter_revision_snapshot(ctx, layout, target);

    let package = native_package.unwrap_or_else(|| target.to_string());
    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let mut journal_gate = LockedJournalGate::load(&_lock, evidence, command)?;
    let mut journal = journal_gate.begin(COMMAND, target, state_path.to_path_buf(), command)?;
    let operation_id = journal.operation_id.clone();
    let context = RecordContext {
        kind: ObjectKind::Component,
        name: target.to_string(),
        scope,
        now: now.to_string(),
        operation_id: Some(operation_id.clone()),
        delegated: Some(DelegatedIdentity {
            pm: NativePm::Rpm,
            package: package.clone(),
        }),
        owned_artifact: None,
    };
    let execution_result = {
        let mut sink = StoreRecordSink::new(&mut store, state_path, context);
        execute_delegated_steps(
            &steps,
            DelegatedExecutionTarget::new(NativePm::Rpm, Some(&package)),
            &provider,
            &mut sink,
            &mut journal,
            now,
        )
    };
    let execution = match execution_result {
        Ok(execution) => execution,
        Err(err) => {
            let error = match err {
                anolisa_core::executor::ExecutionError::TransactionFailed {
                    source:
                        anolisa_core::providers::ProviderError::Transaction(
                            PackageTransactionError::CommandMissing { command: bin },
                        ),
                    ..
                } => tooling_missing_err(command, &bin, target),
                other => CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "update of '{target}' failed: {other}; the native transaction is never undone automatically — run `anolisa repair {target}` to reconcile"
                    ),
                },
            };
            return Err(ApplicationFailure::from_journal(error, &journal));
        }
    };

    let completion_failure = complete_delegated_update(
        layout,
        ctx,
        target,
        &package,
        command,
        &mut store,
        state_path,
        OperationRecord {
            id: operation_id.clone(),
            command: command.to_string(),
            status: String::new(),
            started_at: now.to_string(),
            finished_at: Some(now_iso8601()),
            parent_operation_id: None,
        },
    );
    let to_version = execution.observation.as_ref().map(|observation| {
        observation
            .evr
            .clone()
            .unwrap_or_else(|| observation.version.clone())
    });
    let updated = match (&from_version, &to_version) {
        (Some(from), Some(to)) => from != to,
        _ => true,
    };

    append_update_log(
        layout,
        ctx,
        target,
        command,
        &operation_id,
        now,
        &package,
        to_version.as_deref(),
        completion_failure.as_deref(),
    );
    let changes = updated
        .then_some(UpdateChange::NativePackageUpdated)
        .into_iter()
        .collect();
    let status = match completion_failure {
        Some(reason) => CommandOutcomeStatus::Partial { reason },
        None => CommandOutcomeStatus::Completed,
    };
    let adapter_actions = if updated && matches!(&status, CommandOutcomeStatus::Completed) {
        adapter_actions_after_update(ctx, &store, target, &prior_adapter_revisions)
    } else {
        Vec::new()
    };

    Ok(ApplicationOutcome::Applied {
        command: command.to_string(),
        subject: UpdateSubject {
            component: target.to_string(),
            package: Some(package),
            from_version,
            to_version,
        },
        steps,
        outcome: CommandOutcome::new(status, Some(operation_id), changes, Vec::new()),
        adapter_actions,
    })
}

#[expect(clippy::too_many_arguments)]
pub(super) fn apply_owned(
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    journal_dir: &Path,
    scope: InstallationScope,
    now: &str,
    steps: Vec<Step>,
    resolution: RawResolution,
    prior: OwnedArtifact,
    command: &str,
) -> Result<ApplicationOutcome, ApplicationFailure> {
    apply_owned_with_hydration(
        target,
        ctx,
        layout,
        state_path,
        journal_dir,
        scope,
        now,
        steps,
        resolution,
        prior,
        command,
        common::hydrate_owned_file_contracts,
    )
}

#[expect(clippy::too_many_arguments)]
pub(super) fn apply_owned_with_hydration(
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    journal_dir: &Path,
    scope: InstallationScope,
    now: &str,
    steps: Vec<Step>,
    resolution: RawResolution,
    prior: OwnedArtifact,
    command: &str,
    hydrate: impl FnOnce(&mut StateStore, &FsLayout) -> usize,
) -> Result<ApplicationOutcome, ApplicationFailure> {
    // A user prefix may be writable without root; permission failures belong
    // to the exact owned-executor step so compensation remains honest.
    let resolve_warnings = resolution.warnings.clone();
    let package = resolution.package.clone();
    let from_version = prior.version.clone();
    let to_version = resolution.entry.version.clone();

    let _lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;
    // Hydrate a disposable view so legacy required capabilities participate
    // in rollback without persisting inferred metadata for other components.
    let mut prior_view = store.clone();
    hydrate(&mut prior_view, layout);
    let prior = match prior_view
        .find(ObjectKind::Component, target)
        .map(|record| &record.binding)
    {
        Some(ProviderBinding::Owned { artifact }) if artifact.version == prior.version => {
            artifact.clone()
        }
        Some(ProviderBinding::Owned { artifact }) => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "component '{target}' changed from {} to {} while this update was resolving; nothing was changed — re-run `anolisa update {target}`",
                    prior.version, artifact.version
                ),
            }
            .into());
        }
        _ => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "component '{target}' is no longer an owned installation; nothing was changed — re-run `anolisa update {target}`"
                ),
            }
            .into());
        }
    };
    let prior_adapter_revisions = adapter_revision_snapshot(ctx, layout, target);

    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let mut journal_gate = LockedJournalGate::load(&_lock, evidence, command)?;
    let mut journal = journal_gate.begin(COMMAND, target, state_path.to_path_buf(), command)?;
    let operation_id = journal.operation_id.clone();
    let execution_result = {
        let mut ops = RawReplayOps::new(
            ctx,
            layout,
            target.to_string(),
            scope,
            now.to_string(),
            operation_id.clone(),
            resolution,
            prior,
            &mut store,
            state_path,
        )
        .with_runtime_preflight();
        let result = execute_owned_steps(&steps, &mut ops, &mut journal);
        if result.is_ok() {
            // Backups are rollback scratch; failures retain them for recovery.
            ops.discard_backups();
        }
        result
    };
    let execution = match execution_result {
        Ok(execution) => execution,
        Err(err) => {
            let error = owned_error_to_cli(err, target, scope, command);
            return Err(ApplicationFailure::from_journal(error, &journal));
        }
    };

    // The record commit is authoritative; operation history remains
    // best-effort bookkeeping, matching delegated completion.
    store.operations.push(OperationRecord {
        id: operation_id.clone(),
        command: command.to_string(),
        status: "ok".to_string(),
        started_at: now.to_string(),
        finished_at: Some(now_iso8601()),
        parent_operation_id: None,
    });
    if let Err(err) = store.save(state_path) {
        eprintln!("warning: failed to record operation history: {err}");
    }
    let warnings = resolve_warnings
        .into_iter()
        .chain(execution.warnings)
        .collect();

    append_update_log(
        layout,
        ctx,
        target,
        command,
        &operation_id,
        now,
        &package,
        Some(&to_version),
        None,
    );
    let adapter_actions =
        adapter_actions_after_update(ctx, &store, target, &prior_adapter_revisions);

    Ok(ApplicationOutcome::Applied {
        command: command.to_string(),
        subject: UpdateSubject {
            component: target.to_string(),
            package: Some(package),
            from_version: Some(from_version),
            to_version: Some(to_version),
        },
        steps,
        outcome: CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            Some(operation_id),
            vec![UpdateChange::OwnedArtifactUpdated],
            warnings,
        ),
        adapter_actions,
    })
}

#[cfg(test)]
mod tests {
    use anolisa_core::planner::{NoOpReason, Plan};

    use super::*;

    fn applied_outcome(status: CommandOutcomeStatus, warnings: Vec<String>) -> ApplicationOutcome {
        ApplicationOutcome::Applied {
            command: "update cosh".to_string(),
            subject: UpdateSubject {
                component: "cosh".to_string(),
                package: Some("cosh".to_string()),
                from_version: Some("2.6.0".to_string()),
                to_version: Some("2.7.0".to_string()),
            },
            steps: Vec::new(),
            outcome: CommandOutcome::new(
                status,
                Some("operation-1".to_string()),
                vec![UpdateChange::NativePackageUpdated],
                warnings,
            ),
            adapter_actions: Vec::new(),
        }
    }

    #[test]
    fn plan_intent_never_prepares_update_effects() {
        let plan = Plan::Execute {
            steps: vec![Step::DownloadVerify, Step::PlaceFiles],
            notes: Vec::new(),
        };

        assert!(matches!(
            ExecutionIntent::Plan.prepare(plan.clone()),
            PreparedExecution::Preview { .. }
        ));
        assert!(matches!(
            ExecutionIntent::Apply.prepare(plan),
            PreparedExecution::Apply { .. }
        ));
    }

    #[test]
    fn already_latest_never_becomes_apply_ready() {
        let plan = Plan::NoOp {
            reason: NoOpReason::AlreadyLatest,
        };

        for intent in [ExecutionIntent::Plan, ExecutionIntent::Apply] {
            assert!(matches!(
                intent.prepare(plan.clone()),
                PreparedExecution::NoOp {
                    reason: NoOpReason::AlreadyLatest
                }
            ));
        }
    }

    #[test]
    fn completed_batch_outcome_preserves_non_terminal_warnings() {
        let outcome = applied_outcome(
            CommandOutcomeStatus::Completed,
            vec!["service state needs verification".to_string()],
        );

        assert_eq!(
            outcome.batch_outcome(),
            UpdateBatchOutcome::Completed(super::super::UpdateOutcome::Updated)
        );
        assert_eq!(outcome.warnings(), ["service state needs verification"]);
    }

    #[test]
    fn partial_batch_outcome_uses_terminal_reason_not_warning_order() {
        let outcome = applied_outcome(
            CommandOutcomeStatus::Partial {
                reason: "manifest reconciliation did not complete".to_string(),
            },
            vec!["service state needs verification".to_string()],
        );

        assert_eq!(
            outcome.batch_outcome(),
            UpdateBatchOutcome::Partial {
                reason: "the update of 'cosh' committed, but manifest reconciliation did not complete; run `anolisa repair cosh` to reconcile".to_string()
            }
        );
        assert_eq!(outcome.warnings(), ["service state needs verification"]);
    }

    #[test]
    fn failed_batch_outcome_uses_its_terminal_reason() {
        let outcome = applied_outcome(
            CommandOutcomeStatus::Failed {
                reason: "native package transaction failed".to_string(),
            },
            vec!["service state needs verification".to_string()],
        );

        assert_eq!(
            outcome.batch_outcome(),
            UpdateBatchOutcome::Failed {
                reason: "native package transaction failed".to_string()
            }
        );
    }
}
