//! Application orchestration for the single-component `install` lifecycle verb.

use std::collections::HashSet;

use anolisa_core::execution::{CommandOutcome, ExecutionIntent, PreparedExecution};
use anolisa_core::executor::PHASE_NATIVE_TXN;
use anolisa_core::planner::{NoOpReason, Plan, Step};
use anolisa_core::{Transaction, TransactionOutcomeStatus, TransactionStepStatus};
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::pkg_transaction::PackageTransaction;
use anolisa_platform::privilege;

use crate::context::CliContext;
use crate::progress::{self, Activity, ProgressReporter};
use crate::response::CliError;

use super::dispatch::{RpmdbProbe, execute_planned, host_backends, plan_component};
use super::{InstallArgs, InstallOutcome, RawEffectFactories};

/// Resolved command input plus whether the caller requested preview or apply.
pub(super) struct InstallRequest<'a> {
    pub(super) component: &'a str,
    pub(super) args: &'a InstallArgs,
    pub(super) intent: ExecutionIntent,
}

/// Component and resolved-provider evidence carried to the command renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstallSubject {
    /// Canonical component identity selected by resolution.
    pub(super) component: String,
    /// Provider-native package or raw package identity.
    pub(super) package: Option<String>,
    /// Effective installed or resolved version.
    pub(super) version: Option<String>,
    /// Selected backend family.
    pub(super) backend: String,
    /// Explicit version requested by the caller.
    pub(super) requested_version: Option<String>,
    /// Exact candidate version selected by resolution.
    pub(super) resolved_version: Option<String>,
    /// Repository that supplied a version-pinned candidate.
    pub(super) source_repo: Option<String>,
    /// Exact NEVRA or raw artifact URL selected by a version pin.
    pub(super) artifact: Option<String>,
}

/// Provider family whose install effects completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InstallChange {
    /// An owned artifact was installed and recorded.
    OwnedInstalled,
    /// A delegated native package was installed and recorded.
    DelegatedInstalled,
}

/// Typed single-component result consumed by the CLI renderer.
pub(super) enum InstallApplicationOutcome {
    /// The existing record already covers the request.
    NoOp {
        subject: InstallSubject,
        reason: NoOpReason,
    },
    /// Plan-only result; no lock or effect executor was acquired.
    Preview {
        subject: InstallSubject,
        steps: Vec<Step>,
        warnings: Vec<String>,
    },
    /// Applied result with durable operation evidence.
    Applied {
        subject: InstallSubject,
        steps: Vec<Step>,
        outcome: CommandOutcome<InstallChange>,
    },
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
    pub(super) fn from_journal(error: CliError, journal: &Transaction) -> Self {
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

impl InstallApplicationOutcome {
    /// Collapse the typed result to the legacy batch-member classification.
    pub(super) fn batch_outcome(&self) -> InstallOutcome {
        match self {
            Self::NoOp { .. } => InstallOutcome::AlreadyInstalled,
            Self::Preview { .. } | Self::Applied { .. } => InstallOutcome::Installed,
        }
    }

    /// Returns warnings that a batch caller must surface without rendering a
    /// per-component result envelope.
    pub(super) fn warnings(&self) -> &[String] {
        match self {
            Self::NoOp { .. } => &[],
            Self::Preview { warnings, .. } => warnings,
            Self::Applied { outcome, .. } => outcome.warnings(),
        }
    }
}

/// Run one component against production host dependencies.
pub(super) fn run(
    request: InstallRequest<'_>,
    ctx: &CliContext,
) -> Result<InstallApplicationOutcome, CliError> {
    run_with_planned_components(request, ctx, &HashSet::new())
        .map_err(ApplicationFailure::into_cli_error)
}

/// Run one batch member without moving batch-level orchestration into this layer.
pub(super) fn run_with_planned_components(
    request: InstallRequest<'_>,
    ctx: &CliContext,
    planned_components: &HashSet<String>,
) -> Result<InstallApplicationOutcome, ApplicationFailure> {
    let mut activity = Activity::start(
        progress::feedback_for_stderr(ctx.json, ctx.quiet),
        &format!("Preparing to install {}...", request.component),
    );
    let (query, txn) = host_backends(request.component, request.args, ctx)?;
    let env = anolisa_env::EnvService::detect();
    let rpmdb = RpmdbProbe::for_host(&env);
    run_with_dependencies_classified(
        request,
        ctx,
        &env,
        &rpmdb,
        &query,
        &txn,
        privilege::is_root(),
        planned_components,
        &mut activity,
        RawEffectFactories::system(),
    )
}

/// Run the application protocol with explicit host dependencies.
// Tests vary every boundary independently; keeping the parameters explicit
// makes the application contract visible instead of hiding it in a test bag.
#[expect(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn run_with_dependencies(
    request: InstallRequest<'_>,
    ctx: &CliContext,
    env: &anolisa_env::EnvFacts,
    rpmdb: &RpmdbProbe,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    planned_components: &HashSet<String>,
    reporter: &mut dyn ProgressReporter,
    effects: RawEffectFactories<'_>,
) -> Result<InstallApplicationOutcome, CliError> {
    run_with_dependencies_classified(
        request,
        ctx,
        env,
        rpmdb,
        query,
        txn,
        is_root,
        planned_components,
        reporter,
        effects,
    )
    .map_err(ApplicationFailure::into_cli_error)
}

/// Run the application protocol while preserving its terminal classification.
#[expect(clippy::too_many_arguments)]
pub(super) fn run_with_dependencies_classified(
    request: InstallRequest<'_>,
    ctx: &CliContext,
    env: &anolisa_env::EnvFacts,
    rpmdb: &RpmdbProbe,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    planned_components: &HashSet<String>,
    reporter: &mut dyn ProgressReporter,
    effects: RawEffectFactories<'_>,
) -> Result<InstallApplicationOutcome, ApplicationFailure> {
    let planned = plan_component(request.component, request.args, ctx, env, rpmdb, query, txn)?;
    let prepared = prepare_plan(&planned.plan, request.intent);
    debug_assert!(matches!(
        (&planned.route, &prepared),
        (
            super::dispatch::PlannedRoute::AlreadyInstalled { .. },
            PreparedExecution::NoOp { .. }
        ) | (
            super::dispatch::PlannedRoute::Delegated { .. }
                | super::dispatch::PlannedRoute::Owned { .. },
            PreparedExecution::Preview { .. } | PreparedExecution::Apply { .. }
        )
    ));
    execute_planned(
        planned,
        prepared,
        request.args,
        ctx,
        env,
        rpmdb,
        query,
        txn,
        is_root,
        planned_components,
        reporter,
        effects,
    )
}

fn prepare_plan(plan: &Plan, intent: ExecutionIntent) -> PreparedExecution {
    intent.prepare(plan.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_intent_never_prepares_install_effects() {
        let plan = Plan::Execute {
            steps: vec![Step::DownloadVerify, Step::PlaceFiles],
            notes: Vec::new(),
        };

        assert!(matches!(
            prepare_plan(&plan, ExecutionIntent::Plan),
            PreparedExecution::Preview { .. }
        ));
        assert!(matches!(
            prepare_plan(&plan, ExecutionIntent::Apply),
            PreparedExecution::Apply { .. }
        ));
    }

    #[test]
    fn no_op_route_never_becomes_apply_ready() {
        let plan = Plan::NoOp {
            reason: NoOpReason::AlreadyInstalled,
        };

        for intent in [ExecutionIntent::Plan, ExecutionIntent::Apply] {
            assert!(matches!(
                prepare_plan(&plan, intent),
                PreparedExecution::NoOp {
                    reason: NoOpReason::AlreadyInstalled
                }
            ));
        }
    }
}
