//! Application orchestration for the single-component `repair` lifecycle verb.

use std::fmt;

use anolisa_core::component_snapshot::{ComponentSnapshotRequest, SnapshotProbe};
use anolisa_core::domain::{InstallationScope, ProviderBinding};
use anolisa_core::execution::{
    CommandOutcome, CommandOutcomeStatus, ExecutionIntent, PreparedExecution,
};
use anolisa_core::facts::{FactsError, assemble_component_snapshot, lifecycle_facts_from_snapshot};
use anolisa_core::planner::{Intent, RecordWrite, Step, plan};
use anolisa_core::providers::{DelegatedProvider, ProviderError};
use anolisa_core::state::ObjectKind;
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::PackageTransaction;
use anolisa_platform::privilege;
use anolisa_platform::rpm_query::RpmPackageQuery;
use anolisa_platform::rpm_transaction::RpmTransaction;

use crate::commands::common;
use crate::commands::common::RepoPersistPolicy;
use crate::commands::tier1::install::RawEffectFactories;
use crate::commands::tier1::rpm_install;
use crate::commands::tier1::update::rpm_repo_source_for_update;
use crate::context::CliContext;
use crate::response::CliError;

use super::{
    Recovery, continue_after_locked_repair, plan_action, plan_error_to_cli, quarantined_record,
    recover_journal, repair_delegated, repair_owned_replay, repair_restore_quarantined,
    resolve_repair_package, rpm_tooling_missing_error,
};

/// CLI input mapped to the lifecycle execution protocol.
pub(super) struct ApplicationRequest<'a> {
    pub(super) component: &'a str,
    pub(super) intent: ExecutionIntent,
}

/// Stable repair action used by the application outcome and wire renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RepairAction {
    /// Record and observed reality already agree.
    NothingToRepair,
    /// Refresh the recorded native-package observation.
    RefreshObservation,
    /// Reinstall a missing managed native package.
    ReinstallPackage,
    /// Replay the recorded owned artifact over damaged files.
    ReplayOwnedFiles,
    /// Restore a quarantined record as observed delegated state.
    RestoreObservedRecord,
    /// Restore a quarantined record as owned state.
    RestoreOwnedRecord,
    /// Preview consumption of a blocking operation journal.
    RecoverJournal,
    /// Complete a legacy pending native-package install.
    RecoveredPendingInstall,
    /// Complete or settle a subject-bound operation journal.
    RecoveredJournal,
}

impl RepairAction {
    /// Returns the stable action label used by existing repair output.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::NothingToRepair => "nothing-to-repair",
            Self::RefreshObservation => "refresh-observation",
            Self::ReinstallPackage => "reinstall-package",
            Self::ReplayOwnedFiles => "replay-owned-files",
            Self::RestoreObservedRecord => "restore-observed-record",
            Self::RestoreOwnedRecord => "restore-owned-record",
            Self::RecoverJournal => "recover-journal",
            Self::RecoveredPendingInstall => "recovered-pending-install",
            Self::RecoveredJournal => "recovered-journal",
        }
    }

    fn change(self) -> RepairChange {
        match self {
            Self::NothingToRepair => {
                unreachable!("a no-op repair action cannot become an applied change")
            }
            Self::RefreshObservation => RepairChange::ObservationRefreshed,
            Self::ReinstallPackage => RepairChange::NativePackageReinstalled,
            Self::ReplayOwnedFiles => RepairChange::OwnedFilesReplayed,
            Self::RestoreObservedRecord => RepairChange::ObservedRecordRestored,
            Self::RestoreOwnedRecord => RepairChange::OwnedRecordRestored,
            Self::RecoverJournal | Self::RecoveredJournal => RepairChange::JournalRecovered,
            Self::RecoveredPendingInstall => RepairChange::PendingInstallRecovered,
        }
    }
}

impl fmt::Display for RepairAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Resolved component and version evidence carried to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RepairSubject {
    /// Resolved component identity.
    pub(super) component: String,
    /// Native or raw package identity when one participates in the repair.
    pub(super) package: Option<String>,
    /// Recorded version before reconciliation.
    pub(super) from_version: Option<String>,
    /// Observed or replayed version after reconciliation.
    pub(super) to_version: Option<String>,
}

/// Durable change completed by an applied repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RepairChange {
    /// A delegated record absorbed a fresh native observation.
    ObservationRefreshed,
    /// A missing managed native package was reinstalled.
    NativePackageReinstalled,
    /// Damaged ANOLISA-owned files were replayed.
    OwnedFilesReplayed,
    /// A quarantined record was restored as observed delegated state.
    ObservedRecordRestored,
    /// A quarantined record was restored as owned state.
    OwnedRecordRestored,
    /// A subject-bound journal was recovered or settled.
    JournalRecovered,
    /// A legacy pending package install was recovered.
    PendingInstallRecovered,
}

/// Typed application result consumed by the command renderer.
#[derive(Debug)]
pub(super) enum ApplicationOutcome {
    /// No repair work is required.
    NoOp {
        subject: RepairSubject,
        manifest_reconciliation: Option<&'static str>,
    },
    /// Plan-only result; no lock or effect executor was acquired.
    Preview {
        subject: RepairSubject,
        action: RepairAction,
        steps: Vec<Step>,
        manifest_reconciliation: Option<&'static str>,
    },
    /// Applied result with durable operation evidence.
    Applied {
        command: String,
        subject: RepairSubject,
        action: RepairAction,
        steps: Vec<Step>,
        outcome: CommandOutcome<RepairChange>,
        manifest_reconciliation: Option<&'static str>,
    },
}

/// Builds a completed typed repair outcome.
pub(super) fn applied(
    command: &str,
    subject: RepairSubject,
    action: RepairAction,
    steps: Vec<Step>,
    operation_id: String,
    warnings: Vec<String>,
    manifest_reconciliation: Option<&'static str>,
) -> ApplicationOutcome {
    ApplicationOutcome::Applied {
        command: command.to_string(),
        subject,
        action,
        steps,
        outcome: CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            Some(operation_id),
            vec![action.change()],
            warnings,
        ),
        manifest_reconciliation,
    }
}

/// Builds a repair outcome whose durable effects need reconciliation.
pub(super) fn partially_applied(
    command: &str,
    subject: RepairSubject,
    action: RepairAction,
    steps: Vec<Step>,
    operation_id: String,
    reason: String,
    manifest_reconciliation: Option<&'static str>,
) -> ApplicationOutcome {
    ApplicationOutcome::Applied {
        command: command.to_string(),
        subject,
        action,
        steps,
        outcome: CommandOutcome::new(
            CommandOutcomeStatus::Partial { reason },
            Some(operation_id),
            vec![action.change()],
            Vec::new(),
        ),
        manifest_reconciliation,
    }
}

/// Run one component repair against production package backends.
pub(super) fn run(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
) -> Result<ApplicationOutcome, CliError> {
    let command = format!("repair {}", request.component);
    let layout = common::resolve_layout(ctx);
    let (resolved, view) = common::resolve_mutation_target(request.component, ctx, &command)?;
    let is_delegated = matches!(
        view.writable
            .state
            .find(ObjectKind::Component, &resolved)
            .map(|record| &record.binding),
        Some(ProviderBinding::Delegated { .. })
    );
    if is_delegated
        && let Ok(repo_config) =
            common::load_repo_config(ctx, &layout, &command, RepoPersistPolicy::BestEffort)
    {
        let env = anolisa_env::EnvService::detect();
        if let Ok(Some(repo)) = rpm_repo_source_for_update(&repo_config, &env, &command) {
            let query = RpmPackageQuery::system_with_repo(repo.clone());
            let transaction = RpmTransaction::system_with_repo(repo);
            return run_with_dependencies(
                request,
                ctx,
                &query,
                &transaction,
                privilege::is_root(),
                RawEffectFactories::system(),
            );
        }
    }
    run_with_dependencies(
        request,
        ctx,
        &RpmPackageQuery::system(),
        &RpmTransaction::system(),
        privilege::is_root(),
        RawEffectFactories::system(),
    )
}

/// Run the repair application protocol with explicit package boundaries.
pub(super) fn run_with_dependencies(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    transaction: &dyn PackageTransaction,
    is_root: bool,
    effects: RawEffectFactories<'_>,
) -> Result<ApplicationOutcome, CliError> {
    repair_attempt(request, ctx, query, transaction, is_root, true, effects)
}

fn repair_attempt(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    transaction: &dyn PackageTransaction,
    is_root: bool,
    may_recover_journal: bool,
    effects: RawEffectFactories<'_>,
) -> Result<ApplicationOutcome, CliError> {
    let input = request.component;
    let command = format!("repair {input}");
    let layout = common::resolve_layout(ctx);
    let state_path = layout.state_dir.join("installed.toml");
    let journal_dir = rpm_install::journal_dir(&layout);
    let scope = match ctx.install_mode {
        crate::context::InstallMode::System => InstallationScope::System,
        crate::context::InstallMode::User => InstallationScope::User {
            uid: privilege::effective_uid(),
        },
    };
    let now = super::now_iso8601();

    let (resolved, view) = common::resolve_mutation_target(input, ctx, &command)?;
    let mut store = view.writable.state;
    effects.hydrate_owned_file_contracts(&mut store, &layout);
    let target = resolved.as_str();

    let native_package = match store.find(ObjectKind::Component, target) {
        Some(installation) => match &installation.binding {
            ProviderBinding::Delegated { package, .. } => match package.resolved_name() {
                Some(name) => Some(name.to_string()),
                None => Some(resolve_repair_package(target, ctx, query, &command)?),
            },
            ProviderBinding::Owned { .. } => None,
        },
        None => quarantined_record(&store, target).map(|quarantined| {
            quarantined
                .record
                .rpm_metadata
                .as_ref()
                .map(|metadata| metadata.package_name.trim())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| target.to_string())
        }),
    };
    let record_requires_native = match store.find(ObjectKind::Component, target) {
        Some(installation) => matches!(installation.binding, ProviderBinding::Delegated { .. }),
        None => quarantined_record(&store, target).is_some_and(|quarantined| {
            quarantined
                .record
                .rpm_metadata
                .as_ref()
                .is_some_and(|metadata| !metadata.package_name.trim().is_empty())
        }),
    };

    let manifest_drifted = matches!(
        store
            .find(ObjectKind::Component, target)
            .map(|record| &record.binding),
        Some(ProviderBinding::Delegated { .. })
    ) && {
        let inspection = super::inspect_datadir_contract_drift(
            &layout,
            target,
            &command,
            ctx.packaged_data_probe(),
        );
        if !ctx.quiet {
            for warning in &inspection.warnings {
                eprintln!("warning: {warning}");
            }
        }
        inspection.drifted
    };
    let manifest_reconciliation = manifest_drifted.then_some("component manifest drift");

    let provider = DelegatedProvider::new(query, transaction);
    let facts = match observe_repair_facts(
        target,
        scope,
        native_package.as_deref(),
        &now,
        &store,
        Some(&provider),
        &layout,
        &journal_dir,
    ) {
        Ok(facts) => facts,
        Err(FactsError::Probe(ProviderError::Query(PackageQueryError::CommandMissing {
            command: binary,
        }))) => {
            if record_requires_native {
                return Err(rpm_tooling_missing_error(&command, &binary, target));
            }
            observe_repair_facts(
                target,
                scope,
                native_package.as_deref(),
                &now,
                &store,
                None,
                &layout,
                &journal_dir,
            )
            .map_err(|error| CliError::Runtime {
                command: command.clone(),
                reason: error.to_string(),
            })?
        }
        Err(error) => {
            return Err(CliError::Runtime {
                command: command.clone(),
                reason: error.to_string(),
            });
        }
    };

    let lifecycle_plan = plan(&Intent::Repair, &facts)
        .map_err(|error| plan_error_to_cli(error, target, &command))?;
    match request.intent.prepare(lifecycle_plan) {
        PreparedExecution::NoOp { .. } => Ok(ApplicationOutcome::NoOp {
            subject: RepairSubject {
                component: target.to_string(),
                package: native_package,
                from_version: None,
                to_version: None,
            },
            manifest_reconciliation,
        }),
        PreparedExecution::Preview { steps, .. } => Ok(ApplicationOutcome::Preview {
            subject: RepairSubject {
                component: target.to_string(),
                package: native_package,
                from_version: None,
                to_version: None,
            },
            action: plan_action(&steps),
            steps,
            manifest_reconciliation,
        }),
        PreparedExecution::Apply { steps, .. } => apply_repair_plan(
            request,
            ctx,
            query,
            transaction,
            is_root,
            may_recover_journal,
            &command,
            &layout,
            &state_path,
            &journal_dir,
            scope,
            &now,
            target,
            native_package,
            manifest_drifted,
            store,
            provider,
            steps,
            effects,
        ),
    }
}

#[expect(clippy::too_many_arguments)]
fn apply_repair_plan(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    transaction: &dyn PackageTransaction,
    is_root: bool,
    may_recover_journal: bool,
    command: &str,
    layout: &anolisa_platform::fs_layout::FsLayout,
    state_path: &std::path::Path,
    journal_dir: &std::path::Path,
    scope: InstallationScope,
    now: &str,
    target: &str,
    native_package: Option<String>,
    manifest_drifted: bool,
    store: anolisa_core::state_store::StateStore,
    provider: DelegatedProvider<'_>,
    steps: Vec<Step>,
    effects: RawEffectFactories<'_>,
) -> Result<ApplicationOutcome, CliError> {
    if matches!(steps.as_slice(), [Step::RecoverJournal]) {
        if !may_recover_journal {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "another operation journal for '{target}' is still pending after the last recovery; run `anolisa repair {target}` again"
                ),
            });
        }
        return match recover_journal(
            request.component,
            target,
            ctx,
            layout,
            state_path,
            journal_dir,
            scope,
            now,
            &provider,
            command,
        )? {
            Recovery::Recovered(outcome) => Ok(*outcome),
            Recovery::Cleared => {
                repair_attempt(request, ctx, query, transaction, is_root, false, effects)
            }
        };
    }

    if matches!(steps.as_slice(), [Step::WriteRecord(RecordWrite::Owned)]) {
        let execution = repair_restore_quarantined(
            target,
            ctx,
            layout,
            state_path,
            journal_dir,
            scope,
            now,
            &steps,
            command,
        )?;
        return continue_after_locked_repair(
            execution,
            may_recover_journal,
            target,
            command,
            || repair_attempt(request, ctx, query, transaction, is_root, true, effects),
        );
    }

    if steps.iter().any(|step| matches!(step, Step::PlaceFiles)) {
        let prior = match store
            .find(ObjectKind::Component, target)
            .map(|record| &record.binding)
        {
            Some(ProviderBinding::Owned { artifact }) => artifact.clone(),
            _ => {
                return Err(CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "internal: planner produced an owned plan but the record for '{target}' is not owned"
                    ),
                });
            }
        };
        let execution = repair_owned_replay(
            target,
            ctx,
            layout,
            state_path,
            journal_dir,
            scope,
            now,
            &steps,
            prior,
            command,
            effects,
        )?;
        return continue_after_locked_repair(
            execution,
            may_recover_journal,
            target,
            command,
            || repair_attempt(request, ctx, query, transaction, is_root, true, effects),
        );
    }

    let package = native_package.ok_or_else(|| CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "internal: planner produced a delegated plan but no probe target was resolved for '{target}'"
        ),
    })?;
    let needs_transaction = steps
        .iter()
        .any(|step| matches!(step, Step::NativeTransaction { .. }));
    if needs_transaction && !is_root {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "reinstalling system RPM '{package}' requires root privileges; re-run with sudo: `sudo anolisa repair {target}`"
            ),
        });
    }
    let from_version = match store.find(ObjectKind::Component, target) {
        Some(installation) => match &installation.binding {
            ProviderBinding::Delegated { last_observed, .. } => {
                last_observed.as_ref().map(|observation| {
                    observation
                        .evr
                        .clone()
                        .unwrap_or_else(|| observation.version.clone())
                })
            }
            ProviderBinding::Owned { .. } => None,
        },
        None => {
            quarantined_record(&store, target).map(|quarantined| quarantined.record.version.clone())
        }
    };
    drop(store);

    let execution = repair_delegated(
        target,
        ctx,
        layout,
        state_path,
        journal_dir,
        scope,
        now,
        &steps,
        &package,
        from_version,
        &provider,
        manifest_drifted,
        command,
    )?;
    continue_after_locked_repair(execution, may_recover_journal, target, command, || {
        repair_attempt(request, ctx, query, transaction, is_root, true, effects)
    })
}

#[expect(clippy::too_many_arguments)]
fn observe_repair_facts(
    component: &str,
    scope: InstallationScope,
    native_package: Option<&str>,
    observed_at: &str,
    store: &anolisa_core::state_store::StateStore,
    provider: Option<&DelegatedProvider<'_>>,
    layout: &anolisa_platform::fs_layout::FsLayout,
    journal_dir: &std::path::Path,
) -> Result<anolisa_core::planner::Facts, FactsError> {
    let mut probes = vec![
        SnapshotProbe::State,
        SnapshotProbe::OwnedFiles,
        SnapshotProbe::PendingJournal,
    ];
    if native_package.is_some() && provider.is_some() && matches!(scope, InstallationScope::System)
    {
        probes.push(SnapshotProbe::NativePackage);
    }
    let snapshot = assemble_component_snapshot(
        ComponentSnapshotRequest::new(component, scope, probes),
        native_package,
        observed_at,
        store,
        provider,
        layout,
        journal_dir,
    )?;
    let active_adapter_claims = store
        .adapter_claims
        .iter()
        .filter(|claim| claim.component == component)
        .map(|claim| claim.framework.clone())
        .collect();
    lifecycle_facts_from_snapshot(&snapshot, active_adapter_claims, None)
}

#[cfg(test)]
mod tests {
    use anolisa_core::planner::{NoOpReason, Plan};

    use super::*;

    #[test]
    fn plan_intent_never_prepares_repair_effects() {
        let plan = Plan::Execute {
            steps: vec![Step::RecoverJournal],
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
    fn healthy_repair_never_becomes_apply_ready() {
        let plan = Plan::NoOp {
            reason: NoOpReason::NothingToRepair,
        };

        for intent in [ExecutionIntent::Plan, ExecutionIntent::Apply] {
            assert!(matches!(
                intent.prepare(plan.clone()),
                PreparedExecution::NoOp {
                    reason: NoOpReason::NothingToRepair
                }
            ));
        }
    }

    #[test]
    fn reconciliation_failure_is_a_typed_partial_outcome() {
        let result = partially_applied(
            "repair cosh",
            RepairSubject {
                component: "cosh".to_string(),
                package: Some("cosh".to_string()),
                from_version: Some("2.6.0".to_string()),
                to_version: Some("2.7.0".to_string()),
            },
            RepairAction::RefreshObservation,
            vec![Step::WriteRecord(RecordWrite::RefreshObservation)],
            "operation-1".to_string(),
            "component manifest reconciliation did not complete".to_string(),
            Some("component manifest drift"),
        );

        let ApplicationOutcome::Applied { outcome, .. } = result else {
            panic!("expected applied repair outcome");
        };
        assert_eq!(
            outcome.status(),
            &CommandOutcomeStatus::Partial {
                reason: "component manifest reconciliation did not complete".to_string(),
            }
        );
        assert!(outcome.warnings().is_empty());
    }

    #[test]
    fn completed_repair_keeps_non_terminal_warnings() {
        let result = applied(
            "repair cosh",
            RepairSubject {
                component: "cosh".to_string(),
                package: Some("cosh".to_string()),
                from_version: Some("2.6.0".to_string()),
                to_version: Some("2.7.0".to_string()),
            },
            RepairAction::RefreshObservation,
            vec![Step::WriteRecord(RecordWrite::RefreshObservation)],
            "operation-1".to_string(),
            vec!["service state needs verification".to_string()],
            None,
        );

        let ApplicationOutcome::Applied { outcome, .. } = result else {
            panic!("expected applied repair outcome");
        };
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(outcome.warnings(), ["service state needs verification"]);
    }
}
