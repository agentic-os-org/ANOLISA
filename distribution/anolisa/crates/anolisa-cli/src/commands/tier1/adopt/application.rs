//! Application orchestration for the delegated-only `adopt` lifecycle verb.

use anolisa_core::component_snapshot::{
    ComponentSnapshot, ComponentSnapshotRequest, ProbeEvidence, SnapshotProbe, StateSnapshot,
};
use anolisa_core::domain::{InstallationScope, ManagementRelation, NativePm, ProviderBinding};
use anolisa_core::execution::{
    CommandOutcome, CommandOutcomeStatus, ExecutionIntent, PreparedExecution,
};
use anolisa_core::executor::{DelegatedExecutionTarget, execute_delegated_steps};
use anolisa_core::facts::{
    FactsError, JournalEvidence, assemble_component_snapshot, lifecycle_facts_from_snapshot,
};
use anolisa_core::lock::InstallLock;
use anolisa_core::planner::{Intent, NoOpReason, PlanError, Step, plan};
use anolisa_core::providers::{DelegatedProvider, ProviderError};
use anolisa_core::record_sink::{DelegatedIdentity, RecordContext, StoreRecordSink};
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_store::StateStore;
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};
use anolisa_platform::privilege;

use crate::commands::common;
use crate::commands::common::RepoPersistPolicy;
use crate::commands::tier1::install::{
    installed_version_label, now_iso8601, rpm_package_candidates_with_index,
    snapshot_datadir_contract,
};
use crate::commands::tier1::recovery::LockedJournalGate;
use crate::commands::tier1::rpm_install;
use crate::context::{CliContext, InstallMode};
use crate::resolution::load_optional_component_index;
use crate::response::CliError;

use super::COMMAND;

/// Resolved command input plus whether the caller requested preview or apply.
pub(super) struct AdoptRequest<'a> {
    pub(super) target: &'a str,
    pub(super) package: Option<&'a str>,
    pub(super) intent: ExecutionIntent,
}

/// Component identity and display facts carried to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AdoptSubject {
    pub(super) component: String,
    pub(super) package: Option<String>,
    pub(super) version: Option<String>,
}

/// State transition committed by a successful adopt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdoptChange {
    /// A new delegated-adopted record was created.
    RecordCreated,
    /// An observed delegated record was upgraded to adopted.
    ObservationAdopted,
}

/// Typed application result consumed by the CLI renderer.
pub(super) enum AdoptOutcome {
    /// The component was already adopted.
    NoOp {
        subject: AdoptSubject,
        reason: NoOpReason,
    },
    /// Plan-only result; no lock or effect executor was acquired.
    Preview {
        subject: AdoptSubject,
        steps: Vec<Step>,
    },
    /// Applied result with durable operation evidence.
    Applied {
        subject: AdoptSubject,
        steps: Vec<Step>,
        outcome: CommandOutcome<AdoptChange>,
    },
}

/// Adopt runs no native transaction; refuse if a plan ever routes one here.
struct NoNativeTransaction;

impl NoNativeTransaction {
    fn refused(operation: &str, packages: &[&str]) -> PackageTransactionError {
        PackageTransactionError::TransactionFailed {
            command: COMMAND.to_string(),
            operation: operation.to_string(),
            code: None,
            stderr: format!(
                "adopt never runs a native transaction (attempted {operation} {})",
                packages.join(" ")
            ),
        }
    }
}

impl PackageTransaction for NoNativeTransaction {
    fn check_install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        Err(Self::refused("install preflight", packages))
    }

    fn install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        Err(Self::refused("install", packages))
    }

    fn update(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        Err(Self::refused("update", packages))
    }

    fn reinstall(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        Err(Self::refused("reinstall", packages))
    }

    fn remove(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        Err(Self::refused("remove", packages))
    }
}

/// What the pre-lock plan decided this adopt writes.
pub(super) enum AdoptShape {
    /// A1: no record existed; write a fresh delegated-adopted one.
    Fresh,
    /// A6: upgrade an observed record without changing package identity.
    UpgradeObserved { package: String },
}

/// Run the complete observe → plan → lock → re-observe → execute protocol.
pub(super) fn run(
    request: AdoptRequest<'_>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
) -> Result<AdoptOutcome, CliError> {
    let command = format!("{COMMAND} {}", request.target);
    if ctx.install_mode == InstallMode::User {
        return Err(CliError::InvalidArgument {
            command,
            reason: "adopt is available only in system mode".to_string(),
        });
    }
    let layout = common::resolve_layout(ctx);
    let state_path = layout.state_dir.join("installed.toml");
    let journal_dir = rpm_install::journal_dir(&layout);
    let scope = InstallationScope::System;
    let now = now_iso8601();
    let env = anolisa_env::EnvService::detect();

    let (component, view) = common::resolve_adopt_target(request.target, ctx, &command)?;
    let store = view.writable.state;
    let initial_snapshot = observe_snapshot(
        &component,
        scope,
        None,
        &now,
        &store,
        None,
        &layout,
        &journal_dir,
        &command,
    )?;

    let active = match initial_snapshot.state() {
        ProbeEvidence::Absent { .. } => None,
        ProbeEvidence::Present {
            value: StateSnapshot::Active(installation),
            ..
        } => Some(installation.as_ref()),
        ProbeEvidence::Present {
            value: StateSnapshot::Quarantined(_),
            ..
        } => {
            return Err(plan_error_to_cli(
                PlanError::NeedsAttention,
                &component,
                &component,
                &command,
            ));
        }
        ProbeEvidence::Unavailable { reason, .. } => {
            return Err(CliError::Runtime {
                command,
                reason: reason.clone(),
            });
        }
        ProbeEvidence::NotRequested => {
            unreachable!("observe_snapshot always requests state evidence")
        }
    };
    match initial_snapshot.pending_journal() {
        ProbeEvidence::Present { .. } => {
            return Err(plan_error_to_cli(
                PlanError::PendingOperation,
                &component,
                &component,
                &command,
            ));
        }
        ProbeEvidence::Absent { .. } => {}
        ProbeEvidence::Unavailable { reason, .. } => {
            return Err(CliError::Runtime {
                command,
                reason: reason.clone(),
            });
        }
        ProbeEvidence::NotRequested => {
            unreachable!("observe_snapshot always requests pending journal evidence")
        }
    }

    // Active records resolve against their recorded package identity. Fresh
    // adopts use the CLI/index/provides candidate chain.
    let active_binding = active.map(|installation| installation.binding.clone());
    let prior_version = active.map(installed_version_label);
    let (native_package, shape): (Option<String>, AdoptShape) = match &active_binding {
        Some(ProviderBinding::Owned { .. }) => (None, AdoptShape::Fresh),
        Some(ProviderBinding::Delegated {
            package: recorded,
            relation,
            ..
        }) => {
            let recorded_name = recorded.resolved_name().map(str::to_string);
            if let (Some(requested), Some(previous)) = (request.package, recorded_name.as_deref())
                && !previous.is_empty()
                && previous != requested
                && !matches!(relation, ManagementRelation::Managed { .. })
            {
                return Err(CliError::InvalidArgument {
                    command,
                    reason: format!(
                        "component '{component}' is already adopted from RPM package '{previous}', not '{requested}'; adopt will not silently repoint it to a different package — run `anolisa forget {component}` first, then adopt the new package"
                    ),
                });
            }
            let package = request
                .package
                .map(str::to_string)
                .or(recorded_name)
                .unwrap_or_else(|| component.clone());
            let shape = if matches!(relation, ManagementRelation::Observed) {
                AdoptShape::UpgradeObserved {
                    package: package.clone(),
                }
            } else {
                AdoptShape::Fresh
            };
            (Some(package), shape)
        }
        None => {
            let package = resolve_fresh_adopt(
                request.package,
                ctx,
                &layout,
                &env,
                &component,
                query,
                &command,
            )?;
            (Some(package), AdoptShape::Fresh)
        }
    };

    let transaction = NoNativeTransaction;
    let provider = DelegatedProvider::new(query, &transaction);
    let snapshot = observe_snapshot(
        &component,
        scope,
        native_package.as_deref(),
        &now,
        &store,
        Some(&provider),
        &layout,
        &journal_dir,
        &command,
    )?;
    let active_adapter_claims = store
        .adapter_claims
        .iter()
        .filter(|claim| claim.component == component)
        .map(|claim| claim.framework.clone())
        .collect();
    let facts = lifecycle_facts_from_snapshot(&snapshot, active_adapter_claims, None)
        .map_err(|err| adopt_facts_error(err, &command, &component))?;
    let package_label = native_package.clone().unwrap_or_else(|| component.clone());
    let prepared = request.intent.prepare(
        plan(&Intent::Adopt, &facts)
            .map_err(|err| plan_error_to_cli(err, &component, &package_label, &command))?,
    );
    let subject = AdoptSubject {
        component: component.clone(),
        package: native_package,
        version: prior_version,
    };

    match prepared {
        PreparedExecution::NoOp { reason } => Ok(AdoptOutcome::NoOp { subject, reason }),
        PreparedExecution::Preview { steps, .. } => Ok(AdoptOutcome::Preview { subject, steps }),
        PreparedExecution::Apply { .. } => execute_adopt_plan(
            &component,
            &package_label,
            &shape,
            ctx,
            &layout,
            &state_path,
            &journal_dir,
            scope,
            &now,
            &provider,
            &command,
        ),
    }
}

#[expect(clippy::too_many_arguments)]
fn observe_snapshot(
    component: &str,
    scope: InstallationScope,
    native_package: Option<&str>,
    observed_at: &str,
    store: &StateStore,
    provider: Option<&DelegatedProvider<'_>>,
    layout: &anolisa_platform::fs_layout::FsLayout,
    journal_dir: &std::path::Path,
    command: &str,
) -> Result<ComponentSnapshot, CliError> {
    let mut probes = vec![SnapshotProbe::State, SnapshotProbe::PendingJournal];
    if native_package.is_some() {
        probes.push(SnapshotProbe::NativePackage);
    }
    assemble_component_snapshot(
        ComponentSnapshotRequest::new(component, scope, probes),
        native_package,
        observed_at,
        store,
        provider,
        layout,
        journal_dir,
    )
    .map_err(|err| adopt_facts_error(err, command, component))
}

fn resolve_fresh_adopt(
    cli_override: Option<&str>,
    ctx: &CliContext,
    layout: &anolisa_platform::fs_layout::FsLayout,
    env: &anolisa_env::EnvFacts,
    component: &str,
    query: &dyn PackageQuery,
    command: &str,
) -> Result<String, CliError> {
    let repo_config =
        common::load_repo_config(ctx, layout, COMMAND, RepoPersistPolicy::BestEffort).ok();
    let rpm_backend = repo_config
        .as_ref()
        .and_then(|config| config.backends.get("rpm"));
    let component_index = repo_config
        .as_ref()
        .and_then(|config| load_optional_component_index(layout, env, config));

    let candidates = rpm_package_candidates_with_index(
        cli_override,
        rpm_backend,
        component_index.as_ref(),
        query,
        component,
    )
    .map_err(|err| match err {
        PackageQueryError::CommandMissing { command: binary } => {
            rpm_tooling_missing_error(command, &binary, component)
        }
        err => pkg_query_err(err, command),
    })?;
    match candidates.as_slice() {
        [] => Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "no RPM package is mapped for component '{component}'; add an rpm backend entry to the repo-side component index or publish Provides: anolisa-component({component})"
            ),
        }),
        [single] => Ok(single.package.clone()),
        many => Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "multiple RPM candidates match '{component}': {}; cannot adopt unambiguously — pin one with `--package <name>`",
                many.iter()
                    .map(|target| target.package.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

#[expect(clippy::too_many_arguments)]
fn execute_adopt_plan(
    component: &str,
    package: &str,
    shape: &AdoptShape,
    ctx: &CliContext,
    layout: &anolisa_platform::fs_layout::FsLayout,
    state_path: &std::path::Path,
    journal_dir: &std::path::Path,
    scope: InstallationScope,
    now: &str,
    provider: &DelegatedProvider<'_>,
    command: &str,
) -> Result<AdoptOutcome, CliError> {
    let lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;
    adopt_authorized(&store, component, shape, command)?;

    let snapshot = observe_snapshot(
        component,
        scope,
        Some(package),
        now,
        &store,
        Some(provider),
        layout,
        journal_dir,
        command,
    )?;
    let active_adapter_claims = store
        .adapter_claims
        .iter()
        .filter(|claim| claim.component == component)
        .map(|claim| claim.framework.clone())
        .collect();
    let locked_facts = lifecycle_facts_from_snapshot(&snapshot, active_adapter_claims, None)
        .map_err(|err| adopt_facts_error(err, command, component))?;
    let locked_steps = match ExecutionIntent::Apply.prepare(
        plan(&Intent::Adopt, &locked_facts)
            .map_err(|err| plan_error_to_cli(err, component, package, command))?,
    ) {
        PreparedExecution::Apply { steps, .. } => steps,
        PreparedExecution::NoOp { .. } => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "the facts for '{component}' changed while this adopt was resolving; nothing was changed — re-run `anolisa adopt {component}`"
                ),
            });
        }
        PreparedExecution::Preview { .. } => {
            unreachable!("ExecutionIntent::Apply cannot produce a preview")
        }
    };
    let prior_version = store
        .find(ObjectKind::Component, component)
        .map(installed_version_label);

    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let mut journal_gate = LockedJournalGate::load(&lock, evidence, command)?;
    let mut journal = journal_gate.begin(COMMAND, component, state_path.to_path_buf(), command)?;
    let operation_id = journal.operation_id.clone();
    let context = RecordContext {
        kind: ObjectKind::Component,
        name: component.to_string(),
        scope,
        now: now.to_string(),
        operation_id: Some(operation_id.clone()),
        delegated: Some(DelegatedIdentity {
            pm: NativePm::Rpm,
            package: package.to_string(),
        }),
        owned_artifact: None,
    };
    let delegated_outcome = {
        let mut sink = StoreRecordSink::new(&mut store, state_path, context);
        execute_delegated_steps(
            &locked_steps,
            DelegatedExecutionTarget::new(NativePm::Rpm, Some(package)),
            provider,
            &mut sink,
            &mut journal,
            now,
        )
    }
    .map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("adopt of '{component}' failed: {err}"),
    })?;

    let mut warnings = Vec::new();
    store.operations.push(OperationRecord {
        id: operation_id.clone(),
        command: command.to_string(),
        status: "ok".to_string(),
        started_at: now.to_string(),
        finished_at: Some(now_iso8601()),
        parent_operation_id: None,
    });
    if let Err(err) = store.save(state_path) {
        warnings.push(format!("failed to record operation history: {err}"));
    }
    warnings.extend(snapshot_datadir_contract(
        layout,
        component,
        command,
        ctx.packaged_data_probe(),
    ));

    let version = delegated_outcome
        .observation
        .as_ref()
        .map(|observation| observation.version.clone())
        .or(prior_version);
    let change = match shape {
        AdoptShape::Fresh => AdoptChange::RecordCreated,
        AdoptShape::UpgradeObserved { .. } => AdoptChange::ObservationAdopted,
    };
    Ok(AdoptOutcome::Applied {
        subject: AdoptSubject {
            component: component.to_string(),
            package: Some(package.to_string()),
            version,
        },
        steps: locked_steps,
        outcome: CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            Some(operation_id),
            vec![change],
            warnings,
        ),
    })
}

fn adopt_facts_error(err: FactsError, command: &str, component: &str) -> CliError {
    match err {
        FactsError::Probe(ProviderError::Query(PackageQueryError::CommandMissing {
            command: binary,
        })) => rpm_tooling_missing_error(command, &binary, component),
        err => CliError::Runtime {
            command: command.to_string(),
            reason: err.to_string(),
        },
    }
}

/// Refuse lock-time drift from the pre-lock record shape.
pub(super) fn adopt_authorized(
    store: &StateStore,
    component: &str,
    shape: &AdoptShape,
    command: &str,
) -> Result<(), CliError> {
    let drift = |detail: String| CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "{detail} while this adopt was resolving; nothing was changed — re-run `anolisa adopt {component}`"
        ),
    };
    match shape {
        AdoptShape::Fresh => {
            if store.find(ObjectKind::Component, component).is_some()
                || store.quarantined.iter().any(|quarantined| {
                    quarantined.record.kind == ObjectKind::Component
                        && quarantined.record.name == component
                })
            {
                return Err(drift(format!("a record for '{component}' appeared")));
            }
        }
        AdoptShape::UpgradeObserved { package } => {
            let Some(installation) = store.find(ObjectKind::Component, component) else {
                return Err(drift(format!("the record for '{component}' disappeared")));
            };
            match &installation.binding {
                ProviderBinding::Delegated {
                    relation: ManagementRelation::Observed,
                    package: recorded,
                    ..
                } if recorded.resolved_name().is_none_or(|name| name == package) => {}
                _ => return Err(drift(format!("the record for '{component}' changed"))),
            }
        }
    }
    Ok(())
}

fn plan_error_to_cli(err: PlanError, component: &str, package: &str, command: &str) -> CliError {
    let command = command.to_string();
    match err {
        PlanError::AdoptRequiresSystemScope => CliError::InvalidArgument {
            command,
            reason: format!(
                "adopt records a system RPM and requires system scope; re-run as `sudo anolisa adopt {component}`"
            ),
        },
        PlanError::NothingToAdopt => CliError::InvalidArgument {
            command,
            reason: format!(
                "no installed RPM '{package}' found for component '{component}'; adopt only records an already-installed system RPM — run `sudo anolisa install {component}` to install it"
            ),
        },
        PlanError::AmbiguousPackage => CliError::InvalidArgument {
            command,
            reason: format!(
                "RPM package '{package}' has multiple installed versions; refusing to adopt a single version automatically — resolve the duplicate first"
            ),
        },
        PlanError::ProvenanceConflict => CliError::InvalidArgument {
            command,
            reason: format!(
                "component '{component}' is already tracked as a raw install; run `anolisa uninstall {component}` first to re-adopt it as a system package"
            ),
        },
        PlanError::AlreadyManaged => CliError::InvalidArgument {
            command,
            reason: format!(
                "component '{component}' is already tracked as rpm-managed; run `anolisa repair {component}` to refresh its state from rpmdb"
            ),
        },
        PlanError::TrackedButAbsent => CliError::InvalidArgument {
            command,
            reason: format!(
                "'{component}' is tracked but its package is no longer installed; run `anolisa forget {component}` to drop the record, then install again"
            ),
        },
        PlanError::NeedsAttention => CliError::InvalidArgument {
            command,
            reason: format!(
                "the record for '{component}' was quarantined by the state migration; run `anolisa repair {component}` to resolve it"
            ),
        },
        PlanError::PendingOperation => CliError::Runtime {
            command,
            reason: format!(
                "a previous operation on '{component}' is pending recovery; run `anolisa repair {component}` before retrying"
            ),
        },
        other => CliError::InvalidArgument {
            command,
            reason: format!("cannot adopt '{component}': {other:?}"),
        },
    }
}

fn rpm_tooling_missing_error(command: &str, binary: &str, component: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "cannot adopt '{component}': {binary} not found on PATH — adopt reads rpmdb to record the installed package; install rpm/dnf and retry"
        ),
    }
}

fn pkg_query_err(err: PackageQueryError, command: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!("rpm query failed: {err}"),
    }
}
