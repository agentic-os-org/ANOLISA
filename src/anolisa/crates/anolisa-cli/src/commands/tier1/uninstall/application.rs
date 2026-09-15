//! Application orchestration for the single-component `uninstall` lifecycle verb.

use anolisa_core::component_snapshot::{
    ComponentSnapshot, ComponentSnapshotRequest, SnapshotProbe,
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
use anolisa_core::owned_executor::execute_owned_steps;
use anolisa_core::planner::{
    Facts, Intent, InvocationForm, RecordFacts, Step, UninstallRequest, plan,
};
use anolisa_core::providers::{DelegatedProvider, ProviderError};
use anolisa_core::record_sink::{DelegatedIdentity, RecordContext, StoreRecordSink};
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_store::StateStore;
use anolisa_core::{
    ComponentManifest, HookPhase, LifecycleOperation, LifecyclePlan, ResolvedLifecycleHooks,
    resolve_manifest_hooks,
};
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};
use anolisa_platform::privilege;
use anolisa_platform::rpm_query::RpmPackageQuery;
use anolisa_platform::rpm_transaction::RpmTransaction;

use crate::color::Palette;
use crate::commands::common;
use crate::commands::tier1::install::{RawEffectFactories, RawTeardownOps};
use crate::commands::tier1::recovery::LockedJournalGate;
use crate::commands::tier1::rpm_install;
use crate::context::{CliContext, InstallMode};
use crate::progress::{self, Activity, ProgressReporter};
use crate::response::CliError;

use super::{
    COMMAND, UninstallArgs, UninstallDisposition, append_uninstall_log, disposition_for,
    ensure_no_adapter_claims, now_iso8601, owned_teardown_error_to_cli, plan_error_to_cli,
    remove_component_manifest_snapshot, remove_manifest_snapshot_dir, scope_guard_command,
    tooling_missing_err,
};

/// CLI input mapped to the lifecycle protocol.
pub(super) struct ApplicationRequest<'a> {
    pub(super) args: &'a UninstallArgs,
    pub(super) intent: ExecutionIntent,
}

/// Resolved component and package identity carried to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UninstallSubject {
    pub(super) component: String,
    pub(super) package: Option<String>,
}

/// Durable change made by an applied uninstall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UninstallChange {
    /// ANOLISA-owned files were removed.
    OwnedFilesRemoved,
    /// The delegated native package was removed.
    NativePackageRemoved,
    /// The ANOLISA installation record was dropped.
    StateRecordDropped,
}

/// Typed application result consumed by the command renderer.
pub(super) enum ApplicationOutcome {
    /// The planner found no work. Uninstall has no such row today.
    NoOp { subject: UninstallSubject },
    /// Plan-only result; no lock or effect executor was acquired.
    Preview {
        subject: UninstallSubject,
        disposition: UninstallDisposition,
        steps: Vec<Step>,
    },
    /// Applied result with durable operation evidence.
    Applied {
        subject: UninstallSubject,
        disposition: UninstallDisposition,
        steps: Vec<Step>,
        outcome: CommandOutcome<UninstallChange>,
    },
    /// Legacy purge plan preview.
    PurgePreview { plan: LifecyclePlan },
    /// Purge execution remains gated pending manifest-driven discovery.
    PurgeUnsupported { command: String, hint: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedRoute {
    Delegated,
    Owned,
}

enum LockedAuthority {
    Delegated {
        package: Option<String>,
        managed: bool,
    },
    Owned {
        artifact: anolisa_core::domain::OwnedArtifact,
    },
}

/// Run one uninstall against production package backends.
pub(super) fn run(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
) -> Result<ApplicationOutcome, CliError> {
    let query = RpmPackageQuery::system();
    let txn = RpmTransaction::system();
    let mut activity = Activity::start(
        progress::feedback_for_stderr(ctx.json, ctx.quiet),
        &format!("Preparing to uninstall {}...", request.args.component),
    );
    run_with_dependencies(
        request,
        ctx,
        &query,
        &txn,
        privilege::is_root(),
        &mut activity,
        RawEffectFactories::system(),
    )
}

/// Run the lifecycle protocol with explicit package and privilege boundaries.
pub(super) fn run_with_dependencies(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    reporter: &mut dyn ProgressReporter,
    effects: RawEffectFactories<'_>,
) -> Result<ApplicationOutcome, CliError> {
    reporter.report(&format!(
        "Preparing to uninstall {}...",
        request.args.component
    ));
    if request.args.purge {
        reporter.finish();
        return plan_purge(request, ctx);
    }
    uninstall_component(request, ctx, query, txn, is_root, reporter, effects)
}

fn uninstall_component(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    reporter: &mut dyn ProgressReporter,
    effects: RawEffectFactories<'_>,
) -> Result<ApplicationOutcome, CliError> {
    let args = request.args;
    let input = args.component.as_str();
    let command = format!("{COMMAND} {input}");
    let scope_command = scope_guard_command(args, input);
    let layout = common::resolve_layout(ctx);
    let state_path = layout.state_dir.join("installed.toml");
    let journal_dir = rpm_install::journal_dir(&layout);
    let uid = privilege::effective_uid();
    let scope = match ctx.install_mode {
        InstallMode::System => InstallationScope::System,
        InstallMode::User => InstallationScope::User { uid },
    };
    let now = now_iso8601();

    let (resolved, view) = common::resolve_mutation_target(input, ctx, &scope_command)?;
    let store = view.writable.state;
    let target = resolved.as_str();
    reporter.report(&format!("Resolving {target}..."));
    reject_dropped_capability(&store, target, &command)?;

    if args.force && !matches!(request.intent, ExecutionIntent::Plan) {
        progress::suspend_output(|| {
            eprintln!("warning: --force is a spec stub today and has no behavioral effect yet");
        });
    }

    if args.remove_system_package
        && !ctx.json
        && matches!(
            store
                .find(ObjectKind::Component, target)
                .map(|record| &record.binding),
            Some(ProviderBinding::Owned { .. })
        )
    {
        progress::suspend_output(|| {
            eprintln!(
                "warning: --remove-system-package has no effect for raw component '{target}' (there is no system RPM to remove)"
            );
        });
    }

    let (native_package, record_is_managed) = record_package_authority(&store, target, &command)?;
    let provider = DelegatedProvider::new(query, txn);
    let facts = observe_uninstall_facts(
        target,
        scope,
        native_package.as_deref(),
        &now,
        &store,
        Some(&provider),
        &layout,
        &journal_dir,
        &command,
        record_is_managed,
        args.remove_system_package,
    )?;
    let lifecycle_intent = Intent::Uninstall(UninstallRequest {
        remove_system_package: args.remove_system_package,
        invocation: InvocationForm::SingleNamed,
    });
    let prepared = request.intent.prepare(
        plan(&lifecycle_intent, &facts)
            .map_err(|err| plan_error_to_cli(err, target, &command, &store))?,
    );
    let subject = UninstallSubject {
        component: target.to_string(),
        package: native_package.clone(),
    };

    match prepared {
        PreparedExecution::NoOp { .. } => {
            reporter.finish();
            Ok(ApplicationOutcome::NoOp { subject })
        }
        PreparedExecution::Preview { steps, notes } => {
            reporter.finish();
            Ok(ApplicationOutcome::Preview {
                subject,
                disposition: disposition_for(&steps, &notes),
                steps,
            })
        }
        PreparedExecution::Apply { steps, .. } => {
            let route = route_for(&steps);
            require_native_root(
                &steps,
                is_root,
                target,
                native_package.as_deref(),
                args.remove_system_package,
                &command,
            )?;
            reporter.report(&format!("Uninstalling {target}..."));
            execute_apply(
                target,
                args.remove_system_package,
                route,
                ctx,
                &layout,
                &state_path,
                &journal_dir,
                scope,
                &now,
                &lifecycle_intent,
                &provider,
                is_root,
                &command,
                reporter,
                effects,
            )
        }
    }
}

#[expect(clippy::too_many_arguments)]
fn execute_apply(
    target: &str,
    remove_system_package: bool,
    expected_route: PlannedRoute,
    ctx: &CliContext,
    layout: &anolisa_platform::fs_layout::FsLayout,
    state_path: &std::path::Path,
    journal_dir: &std::path::Path,
    scope: InstallationScope,
    now: &str,
    intent: &Intent,
    provider: &DelegatedProvider<'_>,
    is_root: bool,
    command: &str,
    reporter: &mut dyn ProgressReporter,
    effects: RawEffectFactories<'_>,
) -> Result<ApplicationOutcome, CliError> {
    let lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;
    ensure_no_adapter_claims(&store, target, command)?;
    let legacy_manifest_dir = if matches!(expected_route, PlannedRoute::Delegated) {
        store
            .find(ObjectKind::Component, target)
            .map(|installation| {
                common::legacy_component_manifest_dir_for_installation(
                    layout,
                    installation,
                    command,
                )
            })
            .transpose()?
            .flatten()
    } else {
        None
    };
    let authority = locked_authority(&store, target, expected_route, command)?;
    let (native_package, record_is_managed) = match &authority {
        LockedAuthority::Delegated { package, managed } => (package.clone(), *managed),
        LockedAuthority::Owned { .. } => (None, false),
    };

    let facts = observe_uninstall_facts(
        target,
        scope,
        native_package.as_deref(),
        now,
        &store,
        Some(provider),
        layout,
        journal_dir,
        command,
        record_is_managed,
        remove_system_package,
    )?;
    let (steps, notes) = match ExecutionIntent::Apply.prepare(
        plan(intent, &facts).map_err(|err| plan_error_to_cli(err, target, command, &store))?,
    ) {
        PreparedExecution::Apply { steps, notes } => (steps, notes),
        PreparedExecution::NoOp { .. } => {
            return Err(changed_while_locked(target, command));
        }
        PreparedExecution::Preview { .. } => {
            unreachable!("ExecutionIntent::Apply cannot produce a preview")
        }
    };
    if route_for(&steps) != expected_route {
        return Err(provider_authority_changed(target, command));
    }
    require_native_root(
        &steps,
        is_root,
        target,
        native_package.as_deref(),
        remove_system_package,
        command,
    )?;
    let disposition = disposition_for(&steps, &notes);

    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let mut journal_gate = LockedJournalGate::load(&lock, evidence, command)?;
    let mut journal = journal_gate.begin(COMMAND, target, state_path.to_path_buf(), command)?;
    let operation_id = journal.operation_id.clone();

    let warnings = match authority {
        LockedAuthority::Delegated { package, managed } => {
            let context = RecordContext {
                kind: ObjectKind::Component,
                name: target.to_string(),
                scope,
                now: now.to_string(),
                operation_id: Some(operation_id.clone()),
                delegated: package.as_deref().map(|package| DelegatedIdentity {
                    pm: NativePm::Rpm,
                    package: package.to_string(),
                }),
                owned_artifact: None,
            };
            {
                let mut sink = StoreRecordSink::new(&mut store, state_path, context);
                execute_delegated_steps(
                    &steps,
                    DelegatedExecutionTarget::new(NativePm::Rpm, package.as_deref()),
                    provider,
                    &mut sink,
                    &mut journal,
                    now,
                )
            }
            .map_err(|err| match err {
                anolisa_core::executor::ExecutionError::TransactionFailed {
                    source:
                        ProviderError::Transaction(PackageTransactionError::CommandMissing {
                            command: binary,
                        }),
                    ..
                } => tooling_missing_err(
                    command,
                    &binary,
                    package.as_deref().unwrap_or(target),
                    target,
                    managed,
                ),
                other => CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "uninstall of '{target}' failed: {other}; the native transaction is never undone automatically — run `anolisa repair {target}` to reconcile"
                    ),
                },
            })?;

            if let Err(err) = remove_component_manifest_snapshot(layout, target, command) {
                progress::suspend_output(|| eprintln!("warning: {err}"));
            }
            if let Some(dir) = legacy_manifest_dir
                && let Err(err) = remove_manifest_snapshot_dir(&dir, command)
            {
                progress::suspend_output(|| eprintln!("warning: {err}"));
            }
            Vec::new()
        }
        LockedAuthority::Owned { artifact } => {
            let hooks = installed_uninstall_hooks(layout, target);
            let outcome = {
                let mut ops = RawTeardownOps::new(
                    ctx,
                    effects,
                    layout,
                    target.to_string(),
                    operation_id.clone(),
                    artifact,
                    hooks,
                    &mut store,
                    state_path,
                );
                execute_owned_steps(&steps, &mut ops, &mut journal)
            }
            .map_err(|err| owned_teardown_error_to_cli(err, target, scope, command))?;
            outcome.warnings
        }
    };
    reporter.report(&format!("Finalizing {target} uninstall..."));

    store.operations.push(OperationRecord {
        id: operation_id.clone(),
        command: command.to_string(),
        status: "ok".to_string(),
        started_at: now.to_string(),
        finished_at: Some(now_iso8601()),
        parent_operation_id: None,
    });
    if let Err(err) = store.save(state_path) {
        progress::suspend_output(|| {
            eprintln!("warning: failed to record operation history: {err}");
        });
    }

    if !ctx.json && !ctx.quiet {
        let color = Palette::new(ctx.no_color);
        for warning in &warnings {
            progress::suspend_output(|| eprintln!("{} {warning}", color.warn("warning:")));
        }
        if matches!(disposition, UninstallDisposition::AlreadyAbsent) {
            progress::suspend_output(|| {
                eprintln!(
                    "{} RPM package '{}' is not present in rpmdb (already removed by a manual `rpm -e`); dropping ANOLISA state only",
                    color.warn("warning:"),
                    native_package.as_deref().unwrap_or(target),
                );
            });
        }
    }

    append_uninstall_log(
        layout,
        ctx,
        target,
        command,
        &operation_id,
        now,
        &disposition,
        native_package.as_deref(),
    );
    reporter.finish();

    let changes = changes_for(disposition);
    Ok(ApplicationOutcome::Applied {
        subject: UninstallSubject {
            component: target.to_string(),
            package: native_package,
        },
        disposition,
        steps,
        outcome: CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            Some(operation_id),
            changes,
            warnings,
        ),
    })
}

fn plan_purge(
    request: ApplicationRequest<'_>,
    ctx: &CliContext,
) -> Result<ApplicationOutcome, CliError> {
    let args = request.args;
    let operation = LifecycleOperation::Purge;
    let input = args.component.as_str();
    let command = format!("{} {}", operation.as_str(), input);
    let scope_command = scope_guard_command(args, input);
    let (resolved, view) = common::resolve_mutation_target(input, ctx, &scope_command)?;
    let installed = view.writable.state;
    let target = resolved.as_str();
    reject_dropped_capability(&installed, target, &command)?;

    if matches!(request.intent, ExecutionIntent::Apply) {
        ensure_no_adapter_claims(&installed, target, &command)?;
        if args.force {
            eprintln!("warning: --force is a spec stub today and has no behavioral effect yet");
        }
    }

    let plan = LifecyclePlan::for_component_purge(target, &installed);
    match request.intent {
        ExecutionIntent::Plan => Ok(ApplicationOutcome::PurgePreview { plan }),
        ExecutionIntent::Apply => Ok(ApplicationOutcome::PurgeUnsupported {
            command,
            hint: "purge execute is gated pending manifest-driven config/cache/state              discovery; run with --dry-run to preview the plan, or use              `anolisa uninstall <component>` for the file-removal subset"
                .to_string(),
        }),
    }
}

#[expect(clippy::too_many_arguments)]
fn observe_uninstall_facts(
    component: &str,
    scope: InstallationScope,
    native_package: Option<&str>,
    observed_at: &str,
    store: &StateStore,
    provider: Option<&DelegatedProvider<'_>>,
    layout: &anolisa_platform::fs_layout::FsLayout,
    journal_dir: &std::path::Path,
    command: &str,
    record_is_managed: bool,
    remove_system_package: bool,
) -> Result<Facts, CliError> {
    let snapshot = match observe_snapshot(
        component,
        scope,
        native_package,
        observed_at,
        store,
        provider,
        layout,
        journal_dir,
    ) {
        Ok(snapshot) => snapshot,
        Err(FactsError::Probe(ProviderError::Query(PackageQueryError::CommandMissing {
            command: binary,
        }))) => {
            if record_is_managed || remove_system_package {
                return Err(tooling_missing_err(
                    command,
                    &binary,
                    native_package.unwrap_or(component),
                    component,
                    record_is_managed,
                ));
            }
            observe_snapshot(
                component,
                scope,
                native_package,
                observed_at,
                store,
                None,
                layout,
                journal_dir,
            )
            .map_err(|err| CliError::Runtime {
                command: command.to_string(),
                reason: err.to_string(),
            })?
        }
        Err(err) => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: err.to_string(),
            });
        }
    };
    lifecycle_facts_from_snapshot(&snapshot, active_adapter_claims(store, component), None).map_err(
        |err| CliError::Runtime {
            command: command.to_string(),
            reason: err.to_string(),
        },
    )
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
) -> Result<ComponentSnapshot, FactsError> {
    let mut probes = vec![SnapshotProbe::State, SnapshotProbe::PendingJournal];
    if native_package.is_some() && provider.is_some() && matches!(scope, InstallationScope::System)
    {
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
}

fn active_adapter_claims(store: &StateStore, component: &str) -> Vec<String> {
    store
        .adapter_claims
        .iter()
        .filter(|claim| claim.component == component)
        .map(|claim| claim.framework.clone())
        .collect()
}

fn record_package_authority(
    store: &StateStore,
    target: &str,
    command: &str,
) -> Result<(Option<String>, bool), CliError> {
    match store
        .find(ObjectKind::Component, target)
        .map(|record| &record.binding)
    {
        Some(ProviderBinding::Delegated {
            package, relation, ..
        }) => {
            let package = package.resolved_name().ok_or_else(|| CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "the record for '{target}' has no resolved package name; run `anolisa repair {target}` first"
                ),
            })?;
            Ok((
                Some(package.to_string()),
                matches!(relation, ManagementRelation::Managed { .. }),
            ))
        }
        Some(ProviderBinding::Owned { .. }) | None => Ok((None, false)),
    }
}

fn locked_authority(
    store: &StateStore,
    target: &str,
    expected_route: PlannedRoute,
    command: &str,
) -> Result<LockedAuthority, CliError> {
    match expected_route {
        PlannedRoute::Owned => match store
            .find(ObjectKind::Component, target)
            .map(|record| &record.binding)
        {
            Some(ProviderBinding::Owned { artifact }) => Ok(LockedAuthority::Owned {
                artifact: artifact.clone(),
            }),
            _ => Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "component '{target}' is no longer an owned installation; nothing was changed — re-run `anolisa uninstall {target}`"
                ),
            }),
        },
        PlannedRoute::Delegated => match store
            .find(ObjectKind::Component, target)
            .map(|record| &record.binding)
        {
            Some(ProviderBinding::Delegated {
                package, relation, ..
            }) => {
                let package = package.resolved_name().ok_or_else(|| CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "the locked record for '{target}' has no resolved package name; run `anolisa repair {target}` first"
                    ),
                })?;
                Ok(LockedAuthority::Delegated {
                    package: Some(package.to_string()),
                    managed: matches!(relation, ManagementRelation::Managed { .. }),
                })
            }
            Some(ProviderBinding::Owned { .. }) => Err(provider_authority_changed(target, command)),
            None if store.record_facts(ObjectKind::Component, target) != RecordFacts::Absent => {
                Ok(LockedAuthority::Delegated {
                    package: None,
                    managed: false,
                })
            }
            None => Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "component '{target}' disappeared from state during uninstall; nothing removed"
                ),
            }),
        },
    }
}

fn require_native_root(
    steps: &[Step],
    is_root: bool,
    target: &str,
    native_package: Option<&str>,
    remove_system_package: bool,
    command: &str,
) -> Result<(), CliError> {
    if is_root
        || !steps
            .iter()
            .any(|step| matches!(step, Step::NativeTransaction { .. }))
    {
        return Ok(());
    }
    let flag_suffix = if remove_system_package {
        " --remove-system-package"
    } else {
        ""
    };
    Err(CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "removing system RPM '{}' requires root privileges; re-run with sudo: `sudo anolisa uninstall {target}{flag_suffix}`",
            native_package.unwrap_or(target)
        ),
    })
}

fn route_for(steps: &[Step]) -> PlannedRoute {
    if steps.iter().all(|step| {
        matches!(
            step,
            Step::NativeTransaction { .. }
                | Step::Observe { .. }
                | Step::WriteRecord(_)
                | Step::DropRecord
        )
    }) {
        PlannedRoute::Delegated
    } else {
        PlannedRoute::Owned
    }
}

fn changes_for(disposition: UninstallDisposition) -> Vec<UninstallChange> {
    let mut changes = match disposition {
        UninstallDisposition::NativeRemove => vec![UninstallChange::NativePackageRemoved],
        UninstallDisposition::OwnedRemoval => vec![UninstallChange::OwnedFilesRemoved],
        UninstallDisposition::StateOnly | UninstallDisposition::AlreadyAbsent => Vec::new(),
    };
    changes.push(UninstallChange::StateRecordDropped);
    changes
}

fn installed_uninstall_hooks(
    layout: &anolisa_platform::fs_layout::FsLayout,
    target: &str,
) -> ResolvedLifecycleHooks {
    match common::installed_component_manifest_path(layout, target, COMMAND)
        .ok()
        .and_then(|path| ComponentManifest::from_file(&path).ok())
    {
        Some(manifest) => ResolvedLifecycleHooks {
            pre_uninstall: resolve_manifest_hooks(
                &manifest.install.hooks,
                layout,
                target,
                HookPhase::PreUninstall,
            )
            .unwrap_or_default(),
            post_uninstall: resolve_manifest_hooks(
                &manifest.install.hooks,
                layout,
                target,
                HookPhase::PostUninstall,
            )
            .unwrap_or_default(),
        },
        None => ResolvedLifecycleHooks::default(),
    }
}

fn reject_dropped_capability(
    store: &StateStore,
    target: &str,
    command: &str,
) -> Result<(), CliError> {
    if store.find(ObjectKind::Component, target).is_none()
        && store.dropped_capabilities.iter().any(|name| name == target)
    {
        return Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "'{target}' is a legacy capability state entry from an older release; the capability concept is removed. The entry is pruned automatically on the next install/uninstall; use `anolisa list` to see components"
            ),
        });
    }
    Ok(())
}

fn changed_while_locked(target: &str, command: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "component '{target}' changed while this uninstall was waiting for the lock; nothing was changed — re-run `anolisa uninstall {target}`"
        ),
    }
}

fn provider_authority_changed(target: &str, command: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "component '{target}' changed provider authority while this uninstall was waiting for the lock; nothing was changed — re-run `anolisa uninstall {target}`"
        ),
    }
}
