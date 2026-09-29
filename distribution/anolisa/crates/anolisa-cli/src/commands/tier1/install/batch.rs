//! `--all` batch install support for the `install` command.
//!
//! The batch is a multi-component plan: every component is planned
//! read-only first, and the fresh delegated installs (I2) merge into **one**
//! native transaction so dnf's solver resolves the whole set at once. A
//! merged transaction that fails without side effects degrades
//! automatically to per-item re-planning, isolating the offending
//! component; a failure after packages reached the rpmdb is never retried —
//! the journal stays `Partial` and `repair` reconciles (forward-only).
//! Everything that is not a fresh delegated install keeps the per-item
//! pipeline unchanged.

use serde::Serialize;

use anolisa_core::domain::NativePm;
use anolisa_core::execution::ExecutionIntent;
use anolisa_core::executor::{
    DelegatedExecutionTarget, PHASE_NATIVE_TXN, delegated_recovery_context,
    execute_delegated_steps_resumed,
};
use anolisa_core::facts::JournalEvidence;
use anolisa_core::lock::InstallLock;
use anolisa_core::planner::{NativeAction, NativeProbe, RecordWrite, Step};
use anolisa_core::providers::DelegatedProvider;
use anolisa_core::record_sink::{DelegatedIdentity, RecordContext, StoreRecordSink};
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_store::StateStore;
use anolisa_core::transaction::{
    Transaction, TransactionOutcomeStatus, TransactionStep, mint_operation_id,
};
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::pkg_transaction::PackageTransaction;
use anolisa_platform::privilege;

use crate::color::Palette;
use crate::commands::common;
use crate::commands::common::RepoPersistPolicy;
use crate::commands::tier1::recovery::LockedJournalGate;
use crate::commands::tier1::rpm_install;
use crate::context::CliContext;
use crate::progress;
use crate::repo_config::RepoConfig;
use crate::resolution::ComponentIndex;
use crate::response::{CliError, render_json, render_json_with_status};

use super::{COMMAND, InstallArgs};

// Dispatch pieces re-exported from the parent module for merged transaction
// effects and their shared guards.
use super::io_util::now_iso8601;
use super::{
    PlannedComponent, PlannedRoute, host_backends, normalized_repo_override, quarantined,
    require_configured_rpm_backend, revalidate_native_absence, step_label,
};

mod application;
#[cfg(test)]
use application::InstallBatchOutcome;
use application::{
    BatchApplicationOutcome, BatchEffectOutcome, BatchMemberOutcome, BatchMemberStatus,
    BatchOutputEvent, MemberApplicationOutcome, failed_item, member_application_outcome,
    partial_item, project_member_application,
};
// ── --all support ───────────────────────────────────────────────────

/// Wire shape for a batch entry.  `status` is one of:
/// `installed` | `planned` (dry-run) | `already-installed` | `failed` |
/// `skipped`.
#[derive(Serialize)]
pub(crate) struct AllSummaryItem {
    component: String,
    status: &'static str,
    reason: Option<String>,
    /// Planned step labels, present only on dry-run for members of the
    /// merged group — the per-item pipeline renders its own plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(crate) struct AllSummaryPayload {
    total: usize,
    installed: usize,
    planned: usize,
    /// Idempotent NoOps: the record already covered the request.
    already_installed: usize,
    failed: usize,
    skipped: usize,
    dry_run: bool,
    /// Packages that share one merged native transaction, present only when
    /// the batch found two or more fresh delegated installs to merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    merged_transaction: Option<Vec<String>>,
    items: Vec<AllSummaryItem>,
}

pub(crate) fn handle_all(args: InstallArgs, ctx: &CliContext) -> Result<(), CliError> {
    let mut output = |event| render_batch_event(ctx, event);
    let outcome = application::run(
        application::BatchRequest {
            args: &args,
            intent: super::execution_intent(ctx),
        },
        ctx,
        &mut output,
    )?;
    render_application_outcome(ctx, outcome)
}

fn render_application_outcome(
    ctx: &CliContext,
    outcome: BatchApplicationOutcome,
) -> Result<(), CliError> {
    let dry_run = outcome.is_preview();
    if outcome.items.is_empty() {
        if !ctx.quiet && !ctx.json {
            let color = Palette::new(ctx.no_color);
            println!(
                "{}",
                color.muted("no available components in component index; nothing to install")
            );
        }
        if ctx.json {
            return render_json(
                "install --all",
                AllSummaryPayload {
                    total: 0,
                    installed: 0,
                    planned: 0,
                    already_installed: 0,
                    failed: 0,
                    skipped: 0,
                    dry_run,
                    merged_transaction: None,
                    items: Vec::new(),
                },
            );
        }
        return Ok(());
    }

    let total = outcome.items.len();
    let installed = outcome
        .items
        .iter()
        .filter(|item| item.status == BatchMemberStatus::Installed)
        .count();
    let planned = outcome
        .items
        .iter()
        .filter(|item| item.status == BatchMemberStatus::Planned)
        .count();
    let already_installed = outcome
        .items
        .iter()
        .filter(|item| item.status == BatchMemberStatus::AlreadyInstalled)
        .count();
    let failed = outcome
        .items
        .iter()
        .filter(|item| item.status.is_unsuccessful())
        .count();
    let skipped = outcome
        .items
        .iter()
        .filter(|item| item.status == BatchMemberStatus::Skipped)
        .count();
    debug_assert_eq!(outcome.has_failures(), failed > 0);
    let items = outcome
        .items
        .into_iter()
        .map(|item| AllSummaryItem {
            component: item.component,
            status: item.status.as_str(),
            reason: item.reason,
            plan: item
                .plan
                .map(|steps| steps.iter().map(step_label).collect()),
        })
        .collect::<Vec<_>>();

    if ctx.json {
        render_json_with_status(
            "install --all",
            failed == 0,
            AllSummaryPayload {
                total,
                installed,
                planned,
                already_installed,
                failed,
                skipped,
                dry_run,
                merged_transaction: outcome.merged_transaction,
                items,
            },
        )?;
        return if failed > 0 {
            Err(CliError::BatchPartial {
                command: "install --all".to_string(),
            })
        } else {
            Ok(())
        };
    }

    if !ctx.quiet {
        let color = Palette::new(ctx.no_color);
        println!();
        let failed_names: Vec<&str> = items
            .iter()
            .filter(|i| i.status == "failed")
            .map(|i| i.component.as_str())
            .collect();
        let ok_word = if dry_run { "planned" } else { "installed" };
        let ok_count = if dry_run { planned } else { installed };
        let already_segment = if already_installed > 0 {
            format!("  already-installed={already_installed}")
        } else {
            String::new()
        };
        if failed_names.is_empty() {
            println!(
                "{} total={}  {ok_word}={}{already_segment}  skipped={}",
                color.label("summary:"),
                total,
                ok_count,
                skipped,
            );
        } else {
            println!(
                "{} total={}  {ok_word}={}{already_segment}  failed={} ({})  skipped={}",
                color.label("summary:"),
                total,
                ok_count,
                failed,
                failed_names.join(", "),
                skipped,
            );
            for item in items.iter().filter(|i| i.status == "failed") {
                if let Some(reason) = &item.reason {
                    eprintln!("{} {}: {reason}", color.err("failed:"), item.component);
                }
            }
        }
    }

    if failed > 0 {
        Err(CliError::BatchPartial {
            command: "install --all".to_string(),
        })
    } else {
        Ok(())
    }
}

fn render_batch_event(ctx: &CliContext, event: BatchOutputEvent) {
    let color = Palette::new(ctx.no_color);
    match event {
        BatchOutputEvent::Warning(warning) => {
            progress::suspend_output(|| eprintln!("warning: {warning}"));
        }
        BatchOutputEvent::MergedGroup { members } if !ctx.quiet && !ctx.json => {
            progress::suspend_output(|| {
                println!("{} {members} (one rpm transaction)", color.label("==>"));
            });
        }
        BatchOutputEvent::PreviewPlan { component, steps } if !ctx.quiet && !ctx.json => {
            progress::suspend_output(|| {
                println!("install {component} (dry-run):");
                for step in &steps {
                    println!("  - {}", step_label(step));
                }
            });
        }
        BatchOutputEvent::Member { component } if !ctx.quiet && !ctx.json => {
            progress::suspend_output(|| println!("{} {component}", color.label("==>")));
        }
        BatchOutputEvent::MergedGroup { .. }
        | BatchOutputEvent::PreviewPlan { .. }
        | BatchOutputEvent::Member { .. } => {}
    }
}

/// One member of the merged delegated group: a fresh delegated install
/// whose native transaction can share the batch dnf run.
struct MergedItem {
    /// Component name as the batch addressed it (summary key).
    name: String,
    /// Native package the plan installs.
    package: String,
    /// The read-only planning result; its route holds the I2 steps.
    planned: PlannedComponent,
}

/// Arguments for one component of the batch, mirroring what the sequential
/// loop always passed: no per-component version pin, batch flags stripped.
fn per_component_args(name: &str, args: &InstallArgs) -> InstallArgs {
    InstallArgs {
        component: Some(name.to_string()),
        all: false,
        fail_fast: false,
        version: None,
        backend: args.backend.clone(),
        repo: args.repo.clone(),
        package: None,
    }
}

/// The native package of a mergeable plan: exactly the fresh delegated
/// install shape (I2) over a single package. Anything else — owned plans,
/// NoOps, repair-shaped step lists — keeps the per-item pipeline.
fn merged_package(planned: &PlannedComponent) -> Option<String> {
    let PlannedRoute::Delegated { steps } = &planned.route else {
        return None;
    };
    match steps.as_slice() {
        [
            Step::NativeTransaction {
                action: NativeAction::Install,
                packages,
                ..
            },
            Step::Observe { packages: observed },
            Step::WriteRecord(RecordWrite::DelegatedManaged),
        ] if packages.len() == 1 && observed == packages => Some(packages[0].clone()),
        _ => None,
    }
}

/// Execute the merged group against the live host: real backends pointed at
/// the configured ANOLISA repo, with clean failures re-planned per member.
fn execute_merged_group(
    group: Vec<MergedItem>,
    args: &InstallArgs,
    ctx: &CliContext,
    output: &mut dyn FnMut(BatchOutputEvent),
) -> BatchEffectOutcome {
    let per_args = per_component_args(&group[0].name, args);
    let mut suppressed_ctx = ctx.clone();
    suppressed_ctx.json = false;
    suppressed_ctx.quiet = true;
    let (query, txn) = match host_backends(&group[0].name, &per_args, &suppressed_ctx) {
        Ok(backends) => backends,
        Err(err) => {
            let reason = err.reason().to_string();
            return BatchEffectOutcome {
                items: group
                    .iter()
                    .map(|item| failed_item(&item.name, reason.clone()))
                    .collect(),
            };
        }
    };
    let mut degrade = |name: &str| {
        let per_args = per_component_args(name, args);
        let outcome = super::application::run_with_planned_components(
            super::application::InstallRequest {
                component: name,
                args: &per_args,
                intent: ExecutionIntent::Apply,
            },
            &suppressed_ctx,
            &std::collections::HashSet::new(),
        );
        Ok(member_application_outcome(outcome))
    };
    execute_merged_group_with_deps(
        group,
        args,
        ctx,
        &query,
        &txn,
        privilege::is_root(),
        &mut degrade,
        output,
    )
}

/// Core of the merged execution with the backends and the degrade pipeline
/// injected, so tests drive every branch without a live dnf.
///
/// One native transaction covers every member; each member keeps its own
/// journal (subject = component) so an interruption leaves per-component
/// pending journals that route the next intent to repair. After the
/// transaction commits, each member's remaining steps (observe, record) run
/// with its own record sink. A transaction failure degrades by fact:
/// members whose package is absent re-plan individually through `degrade`
/// (no side effects for them — D9), members whose package landed anyway get
/// a `Partial` journal and a repair hint (forward-only, never retried).
// Keep host effects explicit so tests can independently script transaction,
// privilege, fallback, and output ordering at this boundary.
#[allow(clippy::too_many_arguments)]
fn execute_merged_group_with_deps(
    group: Vec<MergedItem>,
    args: &InstallArgs,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    degrade: &mut dyn FnMut(&str) -> Result<MemberApplicationOutcome, CliError>,
    output: &mut dyn FnMut(BatchOutputEvent),
) -> BatchEffectOutcome {
    const BATCH_COMMAND: &str = "install --all";
    let all_failed = |group: &[MergedItem], reason: &str| -> BatchEffectOutcome {
        BatchEffectOutcome {
            items: group
                .iter()
                .map(|item| failed_item(&item.name, reason.to_string()))
                .collect(),
        }
    };

    // Environment preflight, mirroring the single-component delegated path:
    // a configured ANOLISA rpm repo and root are required before anything
    // runs.
    let layout = common::resolve_layout(ctx);
    let repo_config =
        match common::load_repo_config(ctx, &layout, BATCH_COMMAND, RepoPersistPolicy::Require) {
            Ok(config) => config,
            Err(err) => return all_failed(&group, &err.reason()),
        };
    let index_base_override = match normalized_repo_override(args) {
        Ok(override_url) => override_url,
        Err(err) => return all_failed(&group, &err.reason()),
    };
    if let Err(err) =
        require_configured_rpm_backend(&repo_config, index_base_override.as_deref(), BATCH_COMMAND)
    {
        return all_failed(&group, &err.reason());
    }
    if !is_root {
        return all_failed(
            &group,
            "installing an RPM-backed component runs dnf and requires root",
        );
    }

    let state_path = layout.state_dir.join("installed.toml");
    let journal_dir = rpm_install::journal_dir(&layout);
    let now = now_iso8601();

    let _lock = match InstallLock::acquire(&layout.lock_file) {
        Ok(lock) => lock,
        Err(err) => return all_failed(&group, &format!("failed to acquire install lock: {err}")),
    };
    let mut store =
        match StateStore::load_for_layout(&state_path, privilege::effective_uid(), &layout) {
            Ok(store) => store,
            Err(err) => {
                return all_failed(&group, &format!("failed to load installed state: {err}"));
            }
        };
    let provider = DelegatedProvider::new(query, txn);
    let evidence = JournalEvidence::new(&journal_dir, &store.operations);
    let mut journal_gate = match LockedJournalGate::load(&_lock, evidence, BATCH_COMMAND) {
        Ok(gate) => gate,
        Err(err) => return all_failed(&group, &err.reason()),
    };

    // Settle membership before solving: a stale slot must not prevent the
    // remaining packages from installing or create a recovery journal.
    let mut items: Vec<BatchMemberOutcome> = Vec::with_capacity(group.len());
    let mut eligible: Vec<MergedItem> = Vec::with_capacity(group.len());
    for item in group {
        let target = &item.planned.component;
        if store.find(ObjectKind::Component, target).is_some() || quarantined(&store, target) {
            items.push(failed_item(
                &item.name,
                format!(
                    "a record for '{target}' appeared while this install was resolving; nothing was changed — re-run `anolisa install {target}`"
                ),
            ));
            continue;
        }
        if let Err(err) = revalidate_native_absence(
            Some(&item.package),
            &provider,
            &now,
            target,
            BATCH_COMMAND,
            None,
        ) {
            items.push(failed_item(&item.name, err.reason().to_string()));
            continue;
        }
        match journal_gate.ensure_clear(target, BATCH_COMMAND) {
            Ok(()) => eligible.push(item),
            Err(err) => items.push(failed_item(&item.name, err.reason().to_string())),
        }
    }
    if eligible.is_empty() {
        return BatchEffectOutcome { items };
    }
    let packages: Vec<&str> = eligible.iter().map(|item| item.package.as_str()).collect();
    if let Err(err) = super::check_rpm_install(&provider, &packages, BATCH_COMMAND) {
        items.extend(all_failed(&eligible, &err.reason()).items);
        return BatchEffectOutcome { items };
    }

    let mut active: Vec<(MergedItem, Transaction)> = Vec::with_capacity(eligible.len());
    for item in eligible {
        let target = &item.planned.component;
        match journal_gate.begin(COMMAND, target, state_path.clone(), BATCH_COMMAND) {
            Ok(journal) => active.push((item, journal)),
            Err(err) => items.push(failed_item(&item.name, err.reason().to_string())),
        }
    }
    if active.is_empty() {
        return BatchEffectOutcome { items };
    }

    // Journal the shared transaction step in every member's journal before
    // dnf runs: an interruption mid-transaction leaves each component with a
    // pending journal naming the merged package set.
    let all_packages: Vec<String> = active
        .iter()
        .map(|(item, _)| item.package.clone())
        .collect();
    let txn_label = all_packages.join(",");
    for (item, journal) in &mut active {
        let journal_result = delegated_recovery_context(
            DelegatedExecutionTarget::new(NativePm::Rpm, Some(&item.package)),
            item.planned.route.steps(),
        )
        .map_err(|err| err.to_string())
        .and_then(|context| {
            journal
                .record_delegated_steps(
                    context,
                    [TransactionStep::planned(
                        PHASE_NATIVE_TXN,
                        txn_label.clone(),
                        "install",
                        None,
                    )],
                )
                .map_err(|err| err.to_string())
        });
        if let Err(err) = journal_result {
            // A journal that cannot be written now would also not absorb the
            // transaction outcome; refusing the whole group before any side
            // effect is the honest move.
            let reason = format!(
                "failed to journal the merged transaction for '{}': {err}",
                item.name
            );
            for (member, mut journal) in active {
                let _ = journal.finish(TransactionOutcomeStatus::Failed);
                items.push(failed_item(&member.name, reason.clone()));
            }
            return BatchEffectOutcome { items };
        }
    }

    match provider.transact(NativeAction::Install, &all_packages) {
        Ok(()) => {
            // The members' per-component operations link back to one parent
            // batch operation: the audit trail must show that these records
            // came out of a single native transaction, not N independent
            // installs. The parent owns no journal — each member journals
            // for itself.
            let batch_operation_id = mint_operation_id("install-all");
            let members_start = items.len();
            for (item, mut journal) in active {
                let target = item.planned.component.clone();
                if let Err(err) = journal.mark_done(0) {
                    items.push(partial_item(
                        &item.name,
                        format!(
                            "install of '{target}' failed: the merged native transaction committed but its journal could not be updated: {err}; run `anolisa repair {target}` to reconcile"
                        ),
                    ));
                    continue;
                }
                let context = RecordContext {
                    kind: ObjectKind::Component,
                    name: target.clone(),
                    scope: item.planned.scope,
                    now: now.clone(),
                    operation_id: Some(journal.operation_id.clone()),
                    delegated: Some(DelegatedIdentity {
                        pm: NativePm::Rpm,
                        package: item.package.clone(),
                    }),
                    owned_artifact: None,
                };
                // The member's own plan tail: observe its package, write its
                // record. The shared transaction step is already done.
                let tail = &item.planned.route.steps()[1..];
                let result = {
                    let mut sink = StoreRecordSink::new(&mut store, &state_path, context);
                    execute_delegated_steps_resumed(
                        tail,
                        DelegatedExecutionTarget::new(NativePm::Rpm, Some(&item.package))
                            .with_confirmed_source(provider.repository_source()),
                        &provider,
                        &mut sink,
                        &mut journal,
                        &now,
                        true,
                    )
                };
                match result {
                    Ok(_) => {
                        store.operations.push(OperationRecord {
                            id: journal.operation_id.clone(),
                            command: format!("{COMMAND} {target}"),
                            status: "ok".to_string(),
                            started_at: now.clone(),
                            finished_at: Some(now_iso8601()),
                            parent_operation_id: Some(batch_operation_id.clone()),
                        });
                        for warning in super::io_util::snapshot_datadir_contract(
                            &layout,
                            &target,
                            COMMAND,
                            ctx.packaged_data_probe(),
                        ) {
                            output(BatchOutputEvent::Warning(warning));
                        }
                        items.push(BatchMemberOutcome {
                            component: item.name.clone(),
                            status: BatchMemberStatus::Installed,
                            reason: None,
                            plan: None,
                        });
                    }
                    Err(err) => items.push(partial_item(
                        &item.name,
                        format!(
                            "install of '{target}' failed: {err}; the native transaction is never undone automatically — run `anolisa repair {target}` to reconcile"
                        ),
                    )),
                }
            }
            // The parent record itself: `partial` when any member's tail
            // failed after the shared transaction committed, so the history
            // reads the same as the members' journals.
            let any_member_failed = items[members_start..]
                .iter()
                .any(|item| item.status.is_unsuccessful());
            store.operations.push(OperationRecord {
                id: batch_operation_id,
                command: BATCH_COMMAND.to_string(),
                status: if any_member_failed { "partial" } else { "ok" }.to_string(),
                started_at: now.clone(),
                finished_at: Some(now_iso8601()),
                parent_operation_id: None,
            });
            // Operation history is best-effort bookkeeping on top of the
            // committed records, exactly like the single-component path.
            if let Err(err) = store.save(&state_path) {
                output(BatchOutputEvent::Warning(format!(
                    "failed to record operation history: {err}"
                )));
            }
            BatchEffectOutcome { items }
        }
        Err(source) => {
            // Forward-only classification: re-observe each member and let
            // the facts decide. A package that landed anyway means real side
            // effects — Partial journal, repair reconciles. An absent
            // package proves its slot is clean — degrade to a per-item
            // re-plan that isolates the offender.
            let reason = source.to_string();
            let mut clean: Vec<String> = Vec::new();
            for (item, mut journal) in active {
                let target = &item.planned.component;
                let journal_outcome = match provider.observe(&item.package, &now) {
                    Ok(NativeProbe::Absent) => {
                        clean.push(item.name.clone());
                        TransactionOutcomeStatus::Failed
                    }
                    _ => {
                        items.push(partial_item(
                            &item.name,
                            format!(
                                "merged native transaction failed after '{}' reached the rpmdb: {reason}; run `anolisa repair {target}` to reconcile",
                                item.package
                            ),
                        ));
                        TransactionOutcomeStatus::Partial
                    }
                };
                if let Err(err) = journal
                    .mark_failed(0, &reason)
                    .and_then(|()| journal.finish(journal_outcome))
                {
                    output(BatchOutputEvent::Warning(format!(
                        "failed to journal the merged transaction outcome for '{}': {err}",
                        item.name
                    )));
                }
            }
            drop(store);
            drop(_lock);

            if clean.is_empty() {
                return BatchEffectOutcome { items };
            }
            output(BatchOutputEvent::Warning(format!(
                "merged rpm transaction failed ({reason}); retrying {} component(s) individually",
                clean.len()
            )));
            let mut fail_fast_tripped = false;
            for name in clean {
                if fail_fast_tripped {
                    items.push(BatchMemberOutcome {
                        component: name,
                        status: BatchMemberStatus::Skipped,
                        reason: Some("--fail-fast: not attempted".to_string()),
                        plan: None,
                    });
                    continue;
                }
                match degrade(&name) {
                    Ok(outcome) => {
                        for warning in outcome.warnings {
                            output(BatchOutputEvent::Warning(warning));
                        }
                        let item = project_member_application(
                            &name,
                            ExecutionIntent::Apply,
                            outcome.outcome,
                        );
                        if args.fail_fast && item.status.is_unsuccessful() {
                            fail_fast_tripped = true;
                        }
                        items.push(item);
                    }
                    Err(err) => {
                        items.push(failed_item(&name, err.reason().to_string()));
                        if args.fail_fast {
                            fail_fast_tripped = true;
                        }
                    }
                }
            }
            BatchEffectOutcome { items }
        }
    }
}

fn component_names_for_target(
    index: &ComponentIndex,
    backend: &str,
    os: &str,
    arch: &str,
) -> Vec<String> {
    index
        .components
        .iter()
        .filter(|entry| {
            entry.backends.iter().any(|item| item.kind == backend)
                && entry.supports_target(os, arch)
        })
        .map(|entry| entry.name.clone())
        .collect()
}

/// Load the component index and return names of components that support the
/// current target and selected backend. When `backend` is `None`, the repo's
/// default backend is used.
pub(crate) fn resolve_all_components(
    ctx: &CliContext,
    backend: Option<&str>,
    index_base_override: Option<&str>,
) -> Result<Vec<String>, CliError> {
    let layout = common::resolve_layout(ctx);
    let env = anolisa_env::EnvService::detect();
    let repo_config =
        common::load_repo_config(ctx, &layout, "install --all", RepoPersistPolicy::Require)?;
    // A `--repo` override's published index is the enumeration authority,
    // matching identity and package resolution for each member install.
    let index = match index_base_override {
        Some(base_url) => crate::resolution::load_component_index_from_base(&layout, base_url),
        None => crate::resolution::load_component_index(&layout, &env, &repo_config),
    }
    .map_err(|err| CliError::Runtime {
        command: "install --all".to_string(),
        reason: format!("failed to load component index: {err}"),
    })?;
    // An override enumerates its own published index and pins the DNF/raw
    // source, so an explicitly named backend need not be configured in
    // repo.toml — the name only filters index rows by backend kind. It must
    // still be a known backend: a typo yields the same INVALID_ARGUMENT as
    // the single-component path, never a successful empty batch.
    let selected_backend = match (index_base_override, backend) {
        (Some(_), Some(name)) => RepoConfig::known_backend_name(name)
            .map_err(|err| CliError::InvalidArgument {
                command: "install --all".to_string(),
                reason: format!("{err}"),
            })?
            .to_string(),
        _ => repo_config
            .select_backend(backend)
            .map_err(|err| CliError::InvalidArgument {
                command: "install --all".to_string(),
                reason: format!("{err}"),
            })?
            .0
            .to_string(),
    };
    Ok(component_names_for_target(
        &index,
        &selected_backend,
        &env.os,
        &env.arch,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use anolisa_core::domain::{InstallationScope, ManagementRelation, ProviderBinding};
    use anolisa_core::facts::{JournalEvidence, pending_journal_for};
    use anolisa_core::planner::{InstallRequest, Plan, ProviderTarget};
    use anolisa_platform::pkg_query::{PackageInfo, PackageQueryError};
    use anolisa_platform::pkg_transaction::PackageTransactionError;

    use super::super::RpmdbProbe;
    use super::super::tests::{
        FakeInstaller, FakeQuery, args, linux_env, load_store, pkg_info,
        system_ctx_with_configured_rpm_repo,
    };
    use super::super::types::InstallOutcome;
    use crate::progress::NoopReporter;

    const NOW: &str = "2026-07-17T00:00:00Z";

    #[test]
    fn batch_component_selection_uses_host_target() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let index_path = manifest_dir.join("../../manifests/components-v2.toml");
        let index = ComponentIndex::load(index_path).expect("component index template must parse");

        assert_eq!(
            component_names_for_target(&index, "raw", "linux", "aarch64"),
            ["cosh-ng", "tokenless"]
        );
        assert_eq!(
            component_names_for_target(&index, "raw", "macos", "aarch64"),
            ["cosh-ng", "agentsight", "tokenless"]
        );
    }

    /// Multi-package transaction fake: records each backend call with its
    /// full package set, so tests can pin that a batch shared one native
    /// transaction. `fail_install` fails the install verb as a whole.
    struct BatchTxn {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        fail_install: bool,
        fail_preflight: bool,
        preflight_calls: RefCell<Vec<Vec<String>>>,
        attempted: Rc<Cell<bool>>,
    }

    impl BatchTxn {
        fn new(fail_install: bool) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_install,
                fail_preflight: false,
                preflight_calls: RefCell::new(Vec::new()),
                attempted: Rc::new(Cell::new(false)),
            }
        }

        fn query_after_attempt(
            &self,
            installed: Vec<(String, PackageInfo)>,
        ) -> TransactionAwareQuery {
            TransactionAwareQuery {
                installed,
                attempted: Rc::clone(&self.attempted),
            }
        }
    }

    struct TransactionAwareQuery {
        installed: Vec<(String, PackageInfo)>,
        attempted: Rc<Cell<bool>>,
    }

    impl PackageQuery for TransactionAwareQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            if !self.attempted.get() {
                return Ok(None);
            }
            Ok(self
                .installed
                .iter()
                .find(|(name, _)| name == package)
                .map(|(_, info)| info.clone()))
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }
    }

    impl PackageTransaction for BatchTxn {
        fn check_install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            self.preflight_calls.borrow_mut().push(
                packages
                    .iter()
                    .map(|package| (*package).to_string())
                    .collect(),
            );
            if self.fail_preflight && packages.contains(&"pkg-a") {
                return Err(PackageTransactionError::TransactionFailed {
                    command: "dnf".to_string(),
                    operation: "install preflight".to_string(),
                    code: Some(1),
                    stderr: "pkg-a conflicts with pkg-b".to_string(),
                });
            }
            Ok(())
        }

        fn install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            self.attempted.set(true);
            self.calls.borrow_mut().push((
                "install".to_string(),
                packages.iter().map(|p| (*p).to_string()).collect(),
            ));
            if self.fail_install {
                return Err(PackageTransactionError::TransactionFailed {
                    command: "dnf".to_string(),
                    operation: "install".to_string(),
                    code: Some(1),
                    stderr: "fake dnf install failure".to_string(),
                });
            }
            Ok(())
        }
        fn update(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("merged install must not run a dnf update");
        }
        fn reinstall(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("merged install must not run a dnf reinstall");
        }
        fn remove(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("merged install must not run a dnf remove");
        }
    }

    fn batch_args() -> InstallArgs {
        InstallArgs {
            component: None,
            all: true,
            fail_fast: false,
            version: None,
            backend: Some("rpm".to_string()),
            repo: None,
            package: None,
        }
    }

    fn i2_planned(component: &str, package: &str) -> PlannedComponent {
        let steps = vec![
            Step::NativeTransaction {
                pm: NativePm::Rpm,
                action: NativeAction::Install,
                packages: vec![package.to_string()],
            },
            Step::Observe {
                packages: vec![package.to_string()],
            },
            Step::WriteRecord(RecordWrite::DelegatedManaged),
        ];
        PlannedComponent {
            command: format!("install {component}"),
            component: component.to_string(),
            family: "rpm".to_string(),
            native_package: Some(package.to_string()),
            delegated_pin: None,
            scope: InstallationScope::System,
            now: NOW.to_string(),
            store: StateStore::empty(),
            request: InstallRequest {
                target: ProviderTarget::Delegated {
                    pm: NativePm::Rpm,
                    package: package.to_string(),
                    artifact: None,
                },
                requested_version: None,
            },
            plan: Plan::Execute {
                steps: steps.clone(),
                notes: Vec::new(),
            },
            route: PlannedRoute::Delegated { steps },
        }
    }

    fn i2_item(component: &str, package: &str) -> MergedItem {
        MergedItem {
            name: component.to_string(),
            package: package.to_string(),
            planned: i2_planned(component, package),
        }
    }

    fn item_status<'a>(items: &'a [BatchMemberOutcome], name: &str) -> &'a BatchMemberOutcome {
        items
            .iter()
            .find(|item| item.component == name)
            .unwrap_or_else(|| panic!("no summary item for {name}"))
    }

    fn applied_member() -> MemberApplicationOutcome {
        MemberApplicationOutcome {
            outcome: InstallBatchOutcome::Completed(InstallOutcome::Installed),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn single_member_batch_preserves_partial_application_failure() {
        let (_tmp, c) = system_ctx_with_configured_rpm_repo(false);
        let layout = common::resolve_layout(&c);
        let fake = FakeInstaller::new(
            "copilot-shell",
            pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"),
        )
        .failing_state_save(layout.state_dir.join("installed.toml"));
        let mut install_args = args("copilot-shell");
        install_args.backend = Some("rpm".to_string());
        let mut reporter = NoopReporter;

        let result = super::super::application::run_with_dependencies_classified(
            super::super::application::InstallRequest {
                component: "copilot-shell",
                args: &install_args,
                intent: ExecutionIntent::Apply,
            },
            &c,
            &linux_env(),
            &RpmdbProbe::absent(),
            &fake,
            &fake,
            true,
            &std::collections::HashSet::new(),
            &mut reporter,
            crate::test_support::raw_effects(),
        );
        let member = member_application_outcome(result);
        let item =
            project_member_application("copilot-shell", ExecutionIntent::Apply, member.outcome);

        assert_eq!(item.status, BatchMemberStatus::Partial);
        assert!(
            item.reason
                .as_deref()
                .is_some_and(|reason| reason.contains("repair")),
            "post-transaction record failure must retain repair guidance: {:?}",
            item.reason
        );
        assert_eq!(fake.install_calls.get(), 1, "the native transaction ran");
    }

    #[test]
    fn merged_package_accepts_only_the_fresh_delegated_shape() {
        assert_eq!(
            merged_package(&i2_planned("a", "pkg-a")).as_deref(),
            Some("pkg-a")
        );

        let mut owned = i2_planned("a", "pkg-a");
        owned.route = PlannedRoute::Owned {
            steps: vec![Step::PlaceFiles],
        };
        assert!(merged_package(&owned).is_none());

        // A delegated plan that is not the fresh-install shape (e.g. an
        // adopt-shaped observe + record) stays per-item.
        let mut adopt_shaped = i2_planned("a", "pkg-a");
        adopt_shaped.route = PlannedRoute::Delegated {
            steps: vec![
                Step::Observe {
                    packages: vec!["pkg-a".to_string()],
                },
                Step::WriteRecord(RecordWrite::DelegatedAdopted),
            ],
        };
        assert!(merged_package(&adopt_shaped).is_none());

        let mut noop = i2_planned("a", "pkg-a");
        noop.route = PlannedRoute::AlreadyInstalled { version: None };
        assert!(merged_package(&noop).is_none());
    }

    #[test]
    fn merged_solver_conflict_leaves_no_pending_members() {
        let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
        let mut txn = BatchTxn::new(false);
        txn.fail_preflight = true;
        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a"), i2_item("b", "pkg-b")],
            &batch_args(),
            &ctx,
            &FakeQuery::default(),
            &txn,
            true,
            &mut |_| panic!("a solver refusal must not retry through apply"),
            &mut |_| {},
        );
        assert_eq!(effect.items.len(), 2);
        for item in effect.items {
            assert_eq!(item.status, BatchMemberStatus::Failed);
            assert!(item.reason.unwrap().contains("pkg-a conflicts with pkg-b"));
        }
        assert!(txn.calls.borrow().is_empty());
        let layout = common::resolve_layout(&ctx);
        let journals = rpm_install::journal_dir(&layout);
        assert!(!journals.exists());
        assert!(load_store(&ctx).find(ObjectKind::Component, "a").is_none());
    }

    #[test]
    fn merged_group_shares_one_native_transaction_and_records_each_member() {
        let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
        let txn = BatchTxn::new(false);
        let query = txn.query_after_attempt(vec![
            (
                "pkg-a".to_string(),
                pkg_info("pkg-a", "1.0.0", Some("1.al4"), "x86_64"),
            ),
            (
                "pkg-b".to_string(),
                pkg_info("pkg-b", "2.0.0", Some("1.al4"), "x86_64"),
            ),
        ]);
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a committed merged transaction must not degrade ({name})");
        };

        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a"), i2_item("b", "pkg-b")],
            &batch_args(),
            &ctx,
            &query,
            &txn,
            true,
            &mut degrade,
            &mut |_| {},
        );
        let items = effect.items;

        // One dnf invocation carried both packages — the whole point of D9.
        assert_eq!(
            txn.calls.borrow().as_slice(),
            &[(
                "install".to_string(),
                vec!["pkg-a".to_string(), "pkg-b".to_string()]
            )]
        );
        assert!(
            items
                .iter()
                .all(|item| item.status == BatchMemberStatus::Installed),
            "expected both installed, got {:?}",
            items
                .iter()
                .map(|i| (i.component.as_str(), i.status))
                .collect::<Vec<_>>()
        );

        // Each member got its own managed record with its own observation.
        let store = load_store(&ctx);
        for (component, package, evr) in
            [("a", "pkg-a", "1.0.0-1.al4"), ("b", "pkg-b", "2.0.0-1.al4")]
        {
            let record = store
                .find(ObjectKind::Component, component)
                .unwrap_or_else(|| panic!("record for {component}"));
            match &record.binding {
                ProviderBinding::Delegated {
                    package: identity,
                    relation,
                    last_observed,
                    ..
                } => {
                    assert_eq!(identity.resolved_name(), Some(package));
                    assert!(matches!(relation, ManagementRelation::Managed { .. }));
                    let observed = last_observed.as_ref().expect("fresh observation");
                    assert_eq!(observed.evr.as_deref(), Some(evr));
                }
                other => panic!("expected delegated binding, got {other:?}"),
            }
        }
        // The history shows one parent batch operation and each member's
        // operation linked to it — the audit trail names the shared
        // transaction instead of two unrelated installs.
        let parent = store
            .operations
            .iter()
            .find(|op| op.command == "install --all")
            .expect("parent batch operation");
        assert!(parent.id.starts_with("op-install-all-"), "{}", parent.id);
        assert_eq!(parent.status, "ok");
        assert!(parent.parent_operation_id.is_none());
        let members: Vec<_> = store
            .operations
            .iter()
            .filter(|op| op.command != "install --all")
            .collect();
        assert_eq!(members.len(), 2, "one operation per member");
        for op in members {
            assert_eq!(op.parent_operation_id.as_deref(), Some(parent.id.as_str()));
        }

        // Both journals closed clean: nothing pending blocks the next intent.
        let layout = common::resolve_layout(&ctx);
        let journal_dir = rpm_install::journal_dir(&layout);
        for component in ["a", "b"] {
            assert!(
                pending_journal_for(JournalEvidence::new(&journal_dir, &[]), component)
                    .expect("scan journals")
                    .is_none(),
                "journal for {component} must be settled"
            );
        }
    }

    #[test]
    fn pending_journal_injected_after_batch_planning_blocks_dnf_and_state_write() {
        let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        StateStore::empty().save(&state_path).expect("seed state");
        let journal_dir = rpm_install::journal_dir(&layout);
        let pending =
            Transaction::begin_with_subject(COMMAND, Some("a"), state_path.clone(), &journal_dir)
                .expect("inject pending journal after planning");
        drop(pending);
        let state_before = std::fs::read(&state_path).expect("read state");
        let journals_before = std::fs::read_dir(&journal_dir)
            .expect("journal dir")
            .count();
        let query = FakeQuery::default();
        let txn = BatchTxn::new(false);
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a recovery-blocked member must not degrade ({name})");
        };

        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a")],
            &batch_args(),
            &ctx,
            &query,
            &txn,
            true,
            &mut degrade,
            &mut |_| {},
        );
        let items = effect.items;

        assert!(txn.calls.borrow().is_empty(), "dnf must not run");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, BatchMemberStatus::Failed);
        assert!(
            items[0]
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("anolisa repair a"))
        );
        assert_eq!(
            std::fs::read(&state_path).expect("read state"),
            state_before
        );
        assert_eq!(
            std::fs::read_dir(&journal_dir)
                .expect("journal dir")
                .count(),
            journals_before,
            "the locked executor must not create a second journal"
        );
    }

    #[test]
    fn merged_txn_failure_with_no_side_effects_degrades_per_item() {
        let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
        // Both packages stay absent on re-observe: the failed transaction
        // provably left no side effects, so every member re-plans.
        let query = FakeQuery::default();
        let txn = BatchTxn::new(true);
        let degraded = RefCell::new(Vec::new());
        let events = RefCell::new(Vec::new());
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            degraded.borrow_mut().push(name.to_string());
            events.borrow_mut().push(format!("degrade:{name}"));
            if name == "a" {
                Err(CliError::Runtime {
                    command: "install a".to_string(),
                    reason: "no match for pkg-a".to_string(),
                })
            } else {
                Ok(applied_member())
            }
        };
        let mut output = |event| {
            if let BatchOutputEvent::Warning(warning) = event {
                events.borrow_mut().push(format!("warning:{warning}"));
            }
        };

        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a"), i2_item("b", "pkg-b")],
            &batch_args(),
            &ctx,
            &query,
            &txn,
            true,
            &mut degrade,
            &mut output,
        );
        let events = events.borrow();
        assert!(events[0].contains("retrying 2 component(s) individually"));
        assert_eq!(events[1..], ["degrade:a", "degrade:b"]);
        let items = effect.items;

        // The offender fails with its own error; the innocent member lands.
        assert_eq!(degraded.borrow().as_slice(), ["a", "b"]);
        let a = item_status(&items, "a");
        assert_eq!(a.status, BatchMemberStatus::Failed);
        assert!(a.reason.as_deref().unwrap().contains("no match for pkg-a"));
        assert_eq!(
            item_status(&items, "b").status,
            BatchMemberStatus::Installed
        );

        // Clean failure: no record was written and no pending journal blocks
        // the per-item retry or any later intent.
        let store = load_store(&ctx);
        assert!(store.find(ObjectKind::Component, "a").is_none());
        let layout = common::resolve_layout(&ctx);
        let journal_dir = rpm_install::journal_dir(&layout);
        for component in ["a", "b"] {
            assert!(
                pending_journal_for(JournalEvidence::new(&journal_dir, &[]), component)
                    .expect("scan journals")
                    .is_none(),
                "failed merged journal for {component} must not stay pending"
            );
        }
    }

    #[test]
    fn merged_txn_failure_with_landed_package_routes_to_repair() {
        let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
        // dnf failed but pkg-a reached the rpmdb anyway: real side effects
        // for `a`, provably none for `b`.
        let txn = BatchTxn::new(true);
        let query = txn.query_after_attempt(vec![(
            "pkg-a".to_string(),
            pkg_info("pkg-a", "1.0.0", Some("1.al4"), "x86_64"),
        )]);
        let degraded = RefCell::new(Vec::new());
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            degraded.borrow_mut().push(name.to_string());
            Ok(applied_member())
        };

        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a"), i2_item("b", "pkg-b")],
            &batch_args(),
            &ctx,
            &query,
            &txn,
            true,
            &mut degrade,
            &mut |_| {},
        );
        let items = effect.items;

        // Forward-only for the landed member: no retry, repair reconciles.
        assert_eq!(degraded.borrow().as_slice(), ["b"]);
        let a = item_status(&items, "a");
        assert_eq!(a.status, BatchMemberStatus::Partial);
        let reason = a.reason.as_deref().unwrap();
        assert!(reason.contains("reached the rpmdb"), "got: {reason}");
        assert!(reason.contains("anolisa repair a"), "got: {reason}");
        assert_eq!(
            item_status(&items, "b").status,
            BatchMemberStatus::Installed
        );

        // The landed member's journal stays pending (Partial) so the next
        // intent routes to repair; the clean member's does not.
        let layout = common::resolve_layout(&ctx);
        let journal_dir = rpm_install::journal_dir(&layout);
        let pending = pending_journal_for(JournalEvidence::new(&journal_dir, &[]), "a")
            .expect("scan journals")
            .expect("partial journal for a must stay pending");
        let journal = Transaction::load_journal(&pending).expect("load partial journal");
        let recovery = journal
            .delegated_recovery
            .expect("per-subject recovery contract");
        assert_eq!(recovery.package.as_deref(), Some("pkg-a"));
        assert_eq!(
            recovery.record_action,
            anolisa_core::transaction::DelegatedRecordAction::WriteManaged
        );
        assert_eq!(journal.steps[0].target, "pkg-a,pkg-b");
        assert!(
            pending_journal_for(JournalEvidence::new(&journal_dir, &[]), "b")
                .expect("scan journals")
                .is_none()
        );
    }

    #[test]
    fn merged_group_without_root_fails_every_member_before_dnf() {
        let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
        let query = FakeQuery::default();
        let txn = BatchTxn::new(false);
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a refused merged group must not degrade ({name})");
        };

        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a"), i2_item("b", "pkg-b")],
            &batch_args(),
            &ctx,
            &query,
            &txn,
            false,
            &mut degrade,
            &mut |_| {},
        );
        let items = effect.items;

        assert!(txn.calls.borrow().is_empty(), "dnf must not run");
        for component in ["a", "b"] {
            let item = item_status(&items, component);
            assert_eq!(item.status, BatchMemberStatus::Failed);
            assert!(item.reason.as_deref().unwrap().contains("requires root"));
        }
    }

    #[test]
    fn merged_group_revalidates_slots_under_the_lock() {
        let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
        let txn = BatchTxn::new(false);
        let query = txn.query_after_attempt(vec![
            (
                "pkg-a".to_string(),
                pkg_info("pkg-a", "1.0.0", Some("1.al4"), "x86_64"),
            ),
            (
                "pkg-b".to_string(),
                pkg_info("pkg-b", "2.0.0", Some("1.al4"), "x86_64"),
            ),
        ]);
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("must not degrade ({name})");
        };

        // First merged run records both members.
        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a"), i2_item("b", "pkg-b")],
            &batch_args(),
            &ctx,
            &query,
            &txn,
            true,
            &mut degrade,
            &mut |_| {},
        );
        let items = effect.items;
        assert!(
            items
                .iter()
                .all(|item| item.status == BatchMemberStatus::Installed)
        );

        // A second merged run over the same members must notice the records
        // that appeared since its (stale) planning and refuse each slot —
        // without running dnf again.
        let mut txn = BatchTxn::new(false);
        txn.fail_preflight = true;
        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a"), i2_item("b", "pkg-b")],
            &batch_args(),
            &ctx,
            &query,
            &txn,
            true,
            &mut degrade,
            &mut |_| {},
        );
        let items = effect.items;
        assert!(txn.calls.borrow().is_empty(), "dnf must not run twice");
        assert!(txn.preflight_calls.borrow().is_empty());
        for component in ["a", "b"] {
            let item = item_status(&items, component);
            assert_eq!(item.status, BatchMemberStatus::Failed);
            assert!(
                item.reason
                    .as_deref()
                    .unwrap()
                    .contains("appeared while this install was resolving")
            );
        }
    }

    #[test]
    fn merged_preflight_excludes_members_with_new_records_or_pending_journals() {
        for pending in [false, true] {
            let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
            let layout = common::resolve_layout(&ctx);
            let state_path = layout.state_dir.join("installed.toml");
            let journal_dir = rpm_install::journal_dir(&layout);
            if pending {
                Transaction::begin_with_subject(COMMAND, Some("a"), state_path, &journal_dir)
                    .expect("inject a pending operation after planning");
            } else {
                let txn = BatchTxn::new(false);
                let query = txn.query_after_attempt(vec![(
                    "pkg-a".to_string(),
                    pkg_info("pkg-a", "1.0.0", Some("1.al4"), "x86_64"),
                )]);
                let installed = execute_merged_group_with_deps(
                    vec![i2_item("a", "pkg-a")],
                    &batch_args(),
                    &ctx,
                    &query,
                    &txn,
                    true,
                    &mut |_| panic!("initial install must succeed"),
                    &mut |_| {},
                );
                assert_eq!(installed.items[0].status, BatchMemberStatus::Installed);
            }
            let mut txn = BatchTxn::new(false);
            txn.fail_preflight = true;
            let query = txn.query_after_attempt(vec![(
                "pkg-b".to_string(),
                pkg_info("pkg-b", "2.0.0", Some("1.al4"), "x86_64"),
            )]);
            let outcome = execute_merged_group_with_deps(
                vec![i2_item("a", "pkg-a"), i2_item("b", "pkg-b")],
                &batch_args(),
                &ctx,
                &query,
                &txn,
                true,
                &mut |_| panic!("eligible member must not degrade"),
                &mut |_| {},
            );
            let rejected = item_status(&outcome.items, "a");
            assert_eq!(rejected.status, BatchMemberStatus::Failed);
            assert!(rejected.reason.as_deref().unwrap().contains(if pending {
                "anolisa repair a"
            } else {
                "appeared while this install was resolving"
            }));
            assert_eq!(
                item_status(&outcome.items, "b").status,
                BatchMemberStatus::Installed
            );
            assert_eq!(
                *txn.preflight_calls.borrow(),
                vec![vec!["pkg-b".to_string()]]
            );
            assert_eq!(
                *txn.calls.borrow(),
                vec![("install".to_string(), vec!["pkg-b".to_string()])]
            );
            assert!(load_store(&ctx).find(ObjectKind::Component, "b").is_some());
            assert_eq!(
                pending_journal_for(JournalEvidence::new(&journal_dir, &[]), "a")
                    .expect("inspect a's journal")
                    .is_some(),
                pending
            );
            assert!(
                pending_journal_for(JournalEvidence::new(&journal_dir, &[]), "b")
                    .expect("inspect b's journal")
                    .is_none()
            );
        }
    }

    #[test]
    fn merged_group_rechecks_native_absence_under_lock() {
        let (_tmp, ctx) = system_ctx_with_configured_rpm_repo(false);
        let layout = common::resolve_layout(&ctx);
        let fake = FakeInstaller::new("pkg-a", pkg_info("pkg-a", "1.0.0", Some("1.al4"), "x86_64"))
            .package_appears_under_lock(layout.lock_file.clone());
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a stale merged plan must not degrade ({name})");
        };

        let effect = execute_merged_group_with_deps(
            vec![i2_item("a", "pkg-a")],
            &batch_args(),
            &ctx,
            &fake,
            &fake,
            true,
            &mut degrade,
            &mut |_| {},
        );
        let items = effect.items;

        assert_eq!(fake.install_calls.get(), 0, "dnf must not run");
        let item = item_status(&items, "a");
        assert_eq!(item.status, BatchMemberStatus::Failed);
        assert!(
            item.reason
                .as_deref()
                .is_some_and(|reason| reason.contains("appeared"))
        );
    }

    /// The `plan` key is dry-run-only wire surface: absent entirely for
    /// executed items so existing consumers of the batch summary never see
    /// a new key outside preview mode.
    #[test]
    fn summary_item_serializes_plan_only_on_dry_run_members() {
        let executed = AllSummaryItem {
            component: "a".to_string(),
            status: "installed",
            reason: None,
            plan: None,
        };
        let json = serde_json::to_value(&executed).expect("serialize");
        assert!(json.get("plan").is_none(), "{json}");

        let previewed = AllSummaryItem {
            component: "a".to_string(),
            status: "planned",
            reason: None,
            plan: Some(
                i2_planned("a", "pkg-a")
                    .route
                    .steps()
                    .iter()
                    .map(step_label)
                    .collect(),
            ),
        };
        let json = serde_json::to_value(&previewed).expect("serialize");
        assert_eq!(json["plan"][0], "RPM package manager: install pkg-a");
        assert_eq!(json["plan"][1], "observe pkg-a");
    }
}
