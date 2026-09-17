//! `update all` batch support: every recorded component runs through the
//! U-series planner, and the delegated refreshes (U5) merge into **one**
//! native transaction so dnf's solver resolves the whole set at once.
//!
//! A merged transaction that fails degrades by fact: a member whose
//! installed EVR provably did not move re-plans individually so the
//! offending package fails with its own diagnostic; a member whose EVR
//! moved anyway keeps a `Partial` journal and routes to repair
//! (forward-only, never retried). Components that do not plan a delegated
//! refresh — owned records, NoOps, planning errors — keep the per-item
//! pipeline unchanged.

use std::collections::HashMap;

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
use crate::commands::tier1::recovery::LockedJournalGate;
use crate::commands::tier1::rpm_install;
use crate::context::CliContext;
use crate::response::{CliError, render_json, render_json_with_status};

use super::application::UpdateBatchOutcome;
use super::{
    AdapterAction, AdapterRevisionSnapshot, COMMAND, PlannedComponentUpdate, PlannedUpdateRoute,
    adapter_actions_after_update, adapter_revision_snapshot, append_update_log,
    complete_delegated_update, native_update_authorized, now_iso8601,
    render_adapter_action_notices, step_label, update_backends,
};

mod application;
use application::{
    BatchApplicationOutcome, BatchEffectOutcome, BatchMemberOutcome, BatchMemberStatus,
    BatchOutputEvent, MemberApplicationOutcome, batch_status, failed_item, partial_item,
};

const BATCH_COMMAND: &str = "update all";

/// Wire shape for a batch entry. `status` is one of:
/// `updated` | `planned` (dry-run) | `already-current` | `failed`.
#[derive(Serialize)]
struct UpdateAllItem {
    component: String,
    status: &'static str,
    reason: Option<String>,
    /// Planned step labels, present only on dry-run for members of the
    /// merged group — the per-item pipeline renders its own plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<Vec<String>>,
    /// Adapter follow-up produced by this component's update (issue #1885).
    /// Always present so every item has a stable shape; empty unless the item
    /// really updated and requires follow-up.
    adapter_actions: Vec<AdapterAction>,
}

#[derive(Serialize)]
struct UpdateAllPayload {
    total: usize,
    updated: usize,
    planned: usize,
    /// Idempotent NoOps: the record already covers the latest version.
    already_current: usize,
    failed: usize,
    dry_run: bool,
    /// Packages that share one merged native transaction, present only when
    /// the batch found two or more delegated refreshes to merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    merged_transaction: Option<Vec<String>>,
    items: Vec<UpdateAllItem>,
}

pub(crate) fn handle_update_all(ctx: &CliContext) -> Result<(), CliError> {
    let mut output = |event| render_batch_event(ctx, event);
    let outcome = application::run(
        application::BatchRequest {
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
                color.muted("no components are recorded in ANOLISA state; nothing to update")
            );
        }
        if ctx.json {
            return render_json(
                BATCH_COMMAND,
                UpdateAllPayload {
                    total: 0,
                    updated: 0,
                    planned: 0,
                    already_current: 0,
                    failed: 0,
                    dry_run,
                    merged_transaction: None,
                    items: Vec::new(),
                },
            );
        }
        return Ok(());
    }

    let total = outcome.items.len();
    let updated = outcome
        .items
        .iter()
        .filter(|item| item.status == BatchMemberStatus::Updated)
        .count();
    let planned = outcome
        .items
        .iter()
        .filter(|item| item.status == BatchMemberStatus::Planned)
        .count();
    let already_current = outcome
        .items
        .iter()
        .filter(|item| item.status == BatchMemberStatus::AlreadyCurrent)
        .count();
    let failed = outcome
        .items
        .iter()
        .filter(|item| item.status.is_unsuccessful())
        .count();
    debug_assert_eq!(outcome.has_failures(), failed > 0);
    let items = outcome
        .items
        .into_iter()
        .map(|item| UpdateAllItem {
            component: item.component,
            status: item.status.as_str(),
            reason: item.reason,
            plan: item
                .plan
                .map(|steps| steps.iter().map(step_label).collect()),
            adapter_actions: item.adapter_actions,
        })
        .collect::<Vec<_>>();

    if ctx.json {
        // The batch summary is the single, complete JSON response; a partial
        // failure returns BatchPartial so the exit code is non-zero without
        // a second render.
        render_json_with_status(
            BATCH_COMMAND,
            failed == 0,
            UpdateAllPayload {
                total,
                updated,
                planned,
                already_current,
                failed,
                dry_run,
                merged_transaction: outcome.merged_transaction,
                items,
            },
        )?;
        return if failed > 0 {
            Err(CliError::BatchPartial {
                command: BATCH_COMMAND.to_string(),
            })
        } else {
            Ok(())
        };
    }

    if !ctx.quiet {
        let color = Palette::new(ctx.no_color);
        println!();
        let ok_word = if dry_run { "planned" } else { "updated" };
        let ok_count = if dry_run { planned } else { updated };
        let current_segment = if already_current > 0 {
            format!("  already-current={already_current}")
        } else {
            String::new()
        };
        if failed == 0 {
            println!(
                "{} total={}  {ok_word}={ok_count}{current_segment}",
                color.label("summary:"),
                total,
            );
        } else {
            let failed_names: Vec<&str> = items
                .iter()
                .filter(|i| i.status == "failed")
                .map(|i| i.component.as_str())
                .collect();
            println!(
                "{} total={}  {ok_word}={ok_count}{current_segment}  failed={failed} ({})",
                color.label("summary:"),
                total,
                failed_names.join(", "),
            );
            for item in items.iter().filter(|i| i.status == "failed") {
                if let Some(reason) = &item.reason {
                    eprintln!("{} {}: {reason}", color.err("failed:"), item.component);
                }
            }
        }
        // Adapter follow-up notices (issue #1885) follow the summary so a
        // partially failed batch still names every affected component.
        let adapter_actions: Vec<AdapterAction> = items
            .iter()
            .flat_map(|item| item.adapter_actions.iter().cloned())
            .collect();
        if !adapter_actions.is_empty() {
            println!();
            render_adapter_action_notices(&adapter_actions, &color);
        }
    }

    if failed > 0 {
        Err(CliError::BatchPartial {
            command: BATCH_COMMAND.to_string(),
        })
    } else {
        Ok(())
    }
}

fn render_batch_event(ctx: &CliContext, event: BatchOutputEvent) {
    let color = Palette::new(ctx.no_color);
    match event {
        BatchOutputEvent::Warning(warning) => {
            eprintln!("warning: {warning}");
        }
        BatchOutputEvent::MergedGroup { members } if !ctx.quiet && !ctx.json => {
            println!("{} {members} (one rpm transaction)", color.label("==>"));
        }
        BatchOutputEvent::PreviewPlan {
            component,
            from_version,
            steps,
        } if !ctx.quiet && !ctx.json => {
            println!(
                "{} {} {}",
                color.command("update"),
                component,
                color.muted("(dry-run — nothing updated)"),
            );
            if let Some(from) = from_version {
                println!("{} {from}", color.label("current:"));
            }
            for step in &steps {
                println!("  - {}", step_label(step));
            }
        }
        BatchOutputEvent::Member { component } if !ctx.quiet && !ctx.json => {
            println!("{} {component}", color.label("==>"));
        }
        BatchOutputEvent::MergedGroup { .. }
        | BatchOutputEvent::PreviewPlan { .. }
        | BatchOutputEvent::Member { .. } => {}
    }
}

/// One member of the merged delegated group: a U5 refresh whose native
/// transaction can share the batch dnf run.
struct MergedUpdate {
    /// Component name as the batch addressed it (summary key).
    name: String,
    /// Native package the plan updates.
    package: String,
    /// The read-only planning result; its route holds the U5 steps.
    planned: PlannedComponentUpdate,
}

/// The native package of a mergeable plan: exactly the delegated refresh
/// shape (U5) over a single package. Anything else keeps the per-item
/// pipeline.
fn merged_update_package(planned: &PlannedComponentUpdate) -> Option<String> {
    let PlannedUpdateRoute::Delegated { steps } = &planned.route else {
        return None;
    };
    match steps.as_slice() {
        [
            Step::NativeTransaction {
                action: NativeAction::Update,
                packages,
                ..
            },
            Step::Observe { packages: observed },
            Step::WriteRecord(RecordWrite::RefreshObservation),
        ] if packages.len() == 1 && observed == packages => Some(packages[0].clone()),
        _ => None,
    }
}

/// Execute the merged group against the live host: real backends pointed at
/// the configured ANOLISA repo, degrade re-plans through the per-item
/// pipeline.
fn execute_merged_updates(
    group: Vec<MergedUpdate>,
    ctx: &CliContext,
    output: &mut dyn FnMut(BatchOutputEvent),
) -> BatchEffectOutcome {
    let mut suppressed_ctx = ctx.clone();
    suppressed_ctx.json = false;
    suppressed_ctx.quiet = true;
    let (query, txn) = match update_backends(&group[0].name, &suppressed_ctx) {
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
        let outcome = super::application::run_for_batch(
            super::application::ApplicationRequest {
                component: name,
                intent: ExecutionIntent::Apply,
            },
            &suppressed_ctx,
        );
        Ok(application::member_application_outcome(outcome))
    };
    execute_merged_updates_with_deps(
        group,
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
/// transaction commits, each member's remaining steps (observe, refresh)
/// run with its own record sink. A transaction failure degrades by fact:
/// members whose installed EVR did not move re-plan individually through
/// `degrade` (their slot is provably untouched), members whose EVR moved
/// anyway get a `Partial` journal and a repair hint (forward-only, never
/// retried).
fn execute_merged_updates_with_deps(
    group: Vec<MergedUpdate>,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    degrade: &mut dyn FnMut(&str) -> Result<MemberApplicationOutcome, CliError>,
    output: &mut dyn FnMut(BatchOutputEvent),
) -> BatchEffectOutcome {
    let all_failed = |group: &[MergedUpdate], reason: &str| -> BatchEffectOutcome {
        BatchEffectOutcome {
            items: group
                .iter()
                .map(|item| failed_item(&item.name, reason.to_string()))
                .collect(),
        }
    };

    if !is_root {
        return all_failed(
            &group,
            "updating system RPMs requires root privileges; re-run with sudo: `sudo anolisa update all`",
        );
    }

    let layout = common::resolve_layout(ctx);
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
    let evidence = JournalEvidence::new(&journal_dir, &store.operations);
    let mut journal_gate = match LockedJournalGate::load(&_lock, evidence, BATCH_COMMAND) {
        Ok(gate) => gate,
        Err(err) => return all_failed(&group, &err.reason()),
    };

    // Re-validate each member's authority under the lock and open its
    // journal, mirroring the single-component race check.
    let mut items: Vec<BatchMemberOutcome> = Vec::with_capacity(group.len());
    let mut active: Vec<(MergedUpdate, Transaction)> = Vec::with_capacity(group.len());
    for item in group {
        let target = item.planned.target.as_str();
        if !native_update_authorized(&store, target, Some(&item.package)) {
            items.push(failed_item(
                &item.name,
                format!(
                    "component '{target}' changed while this update was planning; nothing was changed — re-run `anolisa update {target}`"
                ),
            ));
            continue;
        }
        match journal_gate.begin(COMMAND, target, state_path.clone(), BATCH_COMMAND) {
            Ok(journal) => active.push((item, journal)),
            Err(err) => items.push(failed_item(&item.name, err.reason().to_string())),
        }
    }
    if active.is_empty() {
        return BatchEffectOutcome { items };
    }
    let prior_adapter_revisions: HashMap<String, AdapterRevisionSnapshot> = active
        .iter()
        .map(|(item, _)| {
            let target = item.planned.target.clone();
            (
                target.clone(),
                adapter_revision_snapshot(ctx, &layout, &target),
            )
        })
        .collect();

    // Journal the shared transaction step in every member's journal before
    // dnf runs: an interruption mid-transaction leaves each component with a
    // pending journal naming the merged package set.
    let all_packages: Vec<String> = active
        .iter()
        .map(|(item, _)| item.package.clone())
        .collect();
    let txn_label = all_packages.join(",");
    for (item, journal) in &mut active {
        let plan_steps = match &item.planned.route {
            PlannedUpdateRoute::Delegated { steps } => steps.as_slice(),
            _ => unreachable!("merged members are classified as delegated"),
        };
        let journal_result = delegated_recovery_context(
            DelegatedExecutionTarget::new(NativePm::Rpm, Some(&item.package)),
            plan_steps,
        )
        .map_err(|err| err.to_string())
        .and_then(|context| {
            journal
                .record_delegated_steps(
                    context,
                    [TransactionStep::planned(
                        PHASE_NATIVE_TXN,
                        txn_label.clone(),
                        "update",
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

    let provider = DelegatedProvider::new(query, txn);
    // Capture live native identity, not the possibly stale planning or state EVR.
    // A failed/ambiguous pre-read cannot establish provenance; the existing
    // transaction and post-observation path still decides the member's outcome.
    let before: HashMap<_, _> = active
        .iter()
        .map(|(item, _)| {
            let observation = provider.observe(&item.package, &now);
            if let Err(err) = &observation {
                output(BatchOutputEvent::Warning(format!(
                    "cannot establish '{}' identity before update ({err}); repository source will not be inferred",
                    item.package
                )));
            }
            (item.package.clone(), observation)
        })
        .collect();
    match provider.transact(NativeAction::Update, &all_packages) {
        Ok(()) => {
            // The members' per-component operations link back to one parent
            // batch operation: the audit trail must show that these refreshes
            // came out of a single native transaction, not N independent
            // updates. The parent owns no journal — each member journals
            // for itself. Like the members, the parent is persisted as
            // `partial` (still running) before any member completes, so an
            // exit mid-batch never leaves member records pointing at a
            // parent that was never written.
            let batch_operation_id = mint_operation_id("update-all");
            let members_start = items.len();
            store.operations.push(OperationRecord {
                id: batch_operation_id.clone(),
                command: BATCH_COMMAND.to_string(),
                status: "partial".to_string(),
                started_at: now.clone(),
                finished_at: None,
                parent_operation_id: None,
            });
            if let Err(err) = store.save(&state_path) {
                output(BatchOutputEvent::Warning(format!(
                    "failed to record operation history: {err}"
                )));
            }
            for (item, mut journal) in active {
                let target = item.planned.target.clone();
                if let Err(err) = journal.mark_done(0) {
                    items.push(partial_item(
                        &item.name,
                        format!(
                            "update of '{target}' failed: the merged native transaction committed but its journal could not be updated: {err}; run `anolisa repair {target}` to reconcile"
                        ),
                    ));
                    continue;
                }
                let operation_id = journal.operation_id.clone();
                let context = RecordContext {
                    kind: ObjectKind::Component,
                    name: target.clone(),
                    scope: item.planned.scope,
                    now: now.clone(),
                    operation_id: Some(operation_id.clone()),
                    delegated: Some(DelegatedIdentity {
                        pm: NativePm::Rpm,
                        package: item.package.clone(),
                    }),
                    owned_artifact: None,
                };
                // The member's own plan tail: observe its package, refresh
                // its record. The shared transaction step is already done.
                let tail = match &item.planned.route {
                    PlannedUpdateRoute::Delegated { steps } => &steps[1..],
                    _ => unreachable!("merged members are classified as delegated"),
                };
                let result = {
                    let mut sink = StoreRecordSink::new(&mut store, &state_path, context);
                    let mut execution_target =
                        DelegatedExecutionTarget::new(NativePm::Rpm, Some(&item.package));
                    if let Some(Ok(prior)) = before.get(&item.package) {
                        execution_target = execution_target.with_update_from(prior);
                    }
                    execute_delegated_steps_resumed(
                        tail,
                        execution_target,
                        &provider,
                        &mut sink,
                        &mut journal,
                        &now,
                        true,
                    )
                };
                match result {
                    Ok(outcome) => {
                        // Same completion semantics as the single-component
                        // path: persist the member's operation as `partial`,
                        // refresh its contract snapshot, promote to `ok`. A
                        // refresh failure only demotes this member (and the
                        // batch summary below) — every other member keeps
                        // its own outcome.
                        let command = format!("{COMMAND} {target}");
                        let completion_failure = complete_delegated_update(
                            &layout,
                            ctx,
                            &target,
                            &item.package,
                            BATCH_COMMAND,
                            &mut store,
                            &state_path,
                            OperationRecord {
                                id: operation_id.clone(),
                                command: command.clone(),
                                status: String::new(),
                                started_at: now.clone(),
                                finished_at: Some(now_iso8601()),
                                parent_operation_id: Some(batch_operation_id.clone()),
                            },
                        );
                        let to_version = outcome
                            .observation
                            .as_ref()
                            .map(|o| o.evr.clone().unwrap_or_else(|| o.version.clone()));
                        append_update_log(
                            &layout,
                            ctx,
                            &target,
                            &command,
                            &operation_id,
                            &now,
                            &item.package,
                            to_version.as_deref(),
                            completion_failure.as_deref(),
                        );
                        if let Some(reason) = completion_failure {
                            items.push(partial_item(
                                &item.name,
                                format!(
                                    "the update of '{target}' committed, but {reason}; run `anolisa repair {target}` to reconcile"
                                ),
                            ));
                            continue;
                        }
                        let moved = match (&item.planned.native_from, &to_version) {
                            (Some(from), Some(to)) => from != to,
                            _ => true,
                        };
                        // A member whose EVR did not move changed nothing,
                        // so it cannot require adapter follow-up (issue #1885).
                        let adapter_actions = if moved {
                            let before = prior_adapter_revisions
                                .get(&target)
                                .cloned()
                                .unwrap_or_default();
                            adapter_actions_after_update(ctx, &store, &target, &before)
                        } else {
                            Vec::new()
                        };
                        items.push(BatchMemberOutcome {
                            component: item.name.clone(),
                            status: if moved {
                                BatchMemberStatus::Updated
                            } else {
                                BatchMemberStatus::AlreadyCurrent
                            },
                            reason: None,
                            plan: None,
                            adapter_actions,
                        });
                    }
                    Err(err) => items.push(partial_item(
                        &item.name,
                        format!(
                            "update of '{target}' failed: {err}; the native transaction is never undone automatically — run `anolisa repair {target}` to reconcile"
                        ),
                    )),
                }
            }
            // Finalize the parent record by id: `partial` when any member's
            // tail failed after the shared transaction committed, so the
            // history reads the same as the members' journals.
            let any_member_failed = items[members_start..]
                .iter()
                .any(|item| item.status.is_unsuccessful());
            if let Some(parent) = store
                .operations
                .iter_mut()
                .rfind(|operation| operation.id == batch_operation_id)
            {
                parent.status = if any_member_failed { "partial" } else { "ok" }.to_string();
                parent.finished_at = Some(now_iso8601());
            }
            // Operation history is best-effort bookkeeping on top of the
            // committed record refreshes, exactly like the single-component
            // path.
            if let Err(err) = store.save(&state_path) {
                output(BatchOutputEvent::Warning(format!(
                    "failed to record operation history: {err}"
                )));
            }
            BatchEffectOutcome { items }
        }
        Err(source) => {
            // Forward-only classification: only the exact pre-transaction
            // EVR proves a member untouched. Every other observation remains
            // Partial because retrying could apply a second native update.
            let reason = source.to_string();
            let mut clean: Vec<String> = Vec::new();
            for (item, mut journal) in active {
                let target = item.planned.target.as_str();
                let uncertain_reason = match provider.observe(&item.package, &now) {
                    Ok(NativeProbe::Present { observation, .. }) => {
                        let seen = observation.evr.unwrap_or(observation.version);
                        match &item.planned.native_from {
                            Some(from) if &seen == from => None,
                            Some(_) => Some("changed in the rpmdb".to_string()),
                            None => {
                                Some("has no planning-time EVR for a safe comparison".to_string())
                            }
                        }
                    }
                    Ok(NativeProbe::Absent) => Some("is absent from the rpmdb".to_string()),
                    Ok(NativeProbe::MultipleVersions { .. }) => {
                        Some("has multiple installed versions in the rpmdb".to_string())
                    }
                    Ok(NativeProbe::NotProbed) => {
                        Some("could not be re-observed in the rpmdb".to_string())
                    }
                    Err(err) => Some(format!("could not be re-observed in the rpmdb: {err}")),
                };
                let journal_outcome = if let Some(uncertain_reason) = uncertain_reason {
                    items.push(partial_item(
                        &item.name,
                        format!(
                            "merged native transaction failed and '{}' {uncertain_reason}: {reason}; run `anolisa repair {target}` to reconcile",
                            item.package,
                        ),
                    ));
                    TransactionOutcomeStatus::Partial
                } else {
                    clean.push(item.name.clone());
                    TransactionOutcomeStatus::Failed
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
            for name in clean {
                match degrade(&name) {
                    Ok(outcome) => {
                        for warning in outcome.warnings {
                            output(BatchOutputEvent::Warning(warning));
                        }
                        let item = match outcome.outcome {
                            UpdateBatchOutcome::Completed(member) => BatchMemberOutcome {
                                component: name.clone(),
                                status: batch_status(member, ExecutionIntent::Apply),
                                reason: None,
                                plan: None,
                                adapter_actions: outcome.adapter_actions,
                            },
                            UpdateBatchOutcome::Partial { reason } => partial_item(&name, reason),
                            UpdateBatchOutcome::Failed { reason } => failed_item(&name, reason),
                        };
                        items.push(item);
                    }
                    Err(err) => items.push(failed_item(&name, err.reason().to_string())),
                }
            }
            BatchEffectOutcome { items }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::collections::HashMap as StdHashMap;

    use anolisa_core::domain::InstallationScope;
    use anolisa_core::facts::{JournalEvidence, pending_journal_for};
    use anolisa_core::planner::Plan;
    use anolisa_core::state::{
        InstallMode as StateInstallMode, InstalledObject, InstalledState, Ownership,
    };
    use anolisa_platform::fs_layout::FsLayout;
    use anolisa_platform::pkg_query::{PackageInfo, PackageQueryError};
    use anolisa_platform::pkg_transaction::PackageTransactionError;

    use super::super::UpdateOutcome;
    use super::super::tests::{
        FakeRpm, ctx, load_store, pkg_info, raw_manifest_with_adapter, rpm_object,
        seed_package_contract_and_stale_snapshot,
    };
    use crate::context::InstallMode;

    const NOW: &str = "2026-07-17T00:00:00Z";

    /// Multi-package host fake: rpmdb reads come from a static map, and the
    /// transaction records each call's full package set so tests can pin
    /// that a merged batch shared one dnf run.
    struct FakeHost {
        installed: StdHashMap<String, PackageInfo>,
        before_update: StdHashMap<String, PackageInfo>,
        repository: Option<&'static str>,
        multiple_versions: Vec<String>,
        calls: RefCell<Vec<(String, Vec<String>)>>,
        fail_update: bool,
        on_update: Option<Box<dyn Fn()>>,
    }

    impl FakeHost {
        fn new(installed: &[(&str, PackageInfo)], fail_update: bool) -> Self {
            Self {
                installed: installed
                    .iter()
                    .map(|(name, info)| ((*name).to_string(), info.clone()))
                    .collect(),
                before_update: StdHashMap::new(),
                repository: None,
                multiple_versions: Vec::new(),
                calls: RefCell::new(Vec::new()),
                fail_update,
                on_update: None,
            }
        }

        fn with_multiple_versions(mut self, packages: &[&str]) -> Self {
            self.multiple_versions = packages
                .iter()
                .map(|package| (*package).to_string())
                .collect();
            self
        }

        fn with_on_update(mut self, callback: Box<dyn Fn()>) -> Self {
            self.on_update = Some(callback);
            self
        }
    }

    impl PackageQuery for FakeHost {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            if self.multiple_versions.iter().any(|name| name == package) {
                return Err(PackageQueryError::UnexpectedOutput {
                    command: "rpm".to_string(),
                    detail: "2 installed versions".to_string(),
                });
            }
            if self.calls.borrow().is_empty()
                && let Some(info) = self.before_update.get(package)
            {
                return Ok(Some(info.clone()));
            }
            Ok(self.installed.get(package).cloned())
        }
        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }
        fn installed_origin(&self, _package: &str) -> Result<Option<String>, PackageQueryError> {
            Ok(None)
        }
    }

    impl PackageTransaction for FakeHost {
        fn repository_source(&self) -> Option<&str> {
            self.repository
        }
        fn check_install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("this path must not preflight an install")
        }

        fn install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("merged update must not run a dnf install");
        }
        fn update(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            self.calls.borrow_mut().push((
                "update".to_string(),
                packages.iter().map(|p| (*p).to_string()).collect(),
            ));
            if self.fail_update {
                return Err(PackageTransactionError::TransactionFailed {
                    command: "dnf".to_string(),
                    operation: "update".to_string(),
                    code: Some(1),
                    stderr: "fake dnf update failure".to_string(),
                });
            }
            if let Some(callback) = &self.on_update {
                callback();
            }
            Ok(())
        }
        fn reinstall(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("merged update must not run a dnf reinstall");
        }
        fn remove(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("merged update must not run a dnf remove");
        }
    }

    /// Seed one state file holding several v4 objects (the store migrates
    /// them on load).
    fn seed_many(ctx: &CliContext, objects: Vec<InstalledObject>) {
        let layout = common::resolve_layout(ctx);
        std::fs::create_dir_all(&layout.state_dir).expect("mkdir state");
        let mut state = InstalledState {
            install_mode: StateInstallMode::System,
            prefix: layout.prefix.clone(),
            ..Default::default()
        };
        for obj in objects {
            state.upsert_object(obj);
        }
        state
            .save(&layout.state_dir.join("installed.toml"))
            .expect("seed state");
    }

    fn managed_pair(layout_ctx: &CliContext) {
        seed_many(
            layout_ctx,
            vec![
                rpm_object(
                    "cosh",
                    "copilot-shell",
                    "1.0.0-1.al4",
                    Ownership::RpmManaged,
                    anolisa_core::state::ObjectStatus::Installed,
                ),
                rpm_object(
                    "sec-core",
                    "agent-sec-core",
                    "1.0.0-1.al4",
                    Ownership::RpmManaged,
                    anolisa_core::state::ObjectStatus::Installed,
                ),
            ],
        );
    }

    fn u5_planned(component: &str, package: &str, from_evr: &str) -> PlannedComponentUpdate {
        let steps = vec![
            Step::NativeTransaction {
                pm: NativePm::Rpm,
                action: NativeAction::Update,
                packages: vec![package.to_string()],
            },
            Step::Observe {
                packages: vec![package.to_string()],
            },
            Step::WriteRecord(RecordWrite::RefreshObservation),
        ];
        PlannedComponentUpdate {
            command: format!("update {component}"),
            target: component.to_string(),
            native_package: Some(package.to_string()),
            scope: InstallationScope::System,
            now: NOW.to_string(),
            owned_execution: None,
            owned_versions: None,
            native_from: Some(from_evr.to_string()),
            plan: Plan::Execute {
                steps: steps.clone(),
                notes: Vec::new(),
            },
            route: PlannedUpdateRoute::Delegated { steps },
        }
    }

    fn u5_item(component: &str, package: &str, from_evr: &str) -> MergedUpdate {
        MergedUpdate {
            name: component.to_string(),
            package: package.to_string(),
            planned: u5_planned(component, package, from_evr),
        }
    }

    fn find<'a>(items: &'a [BatchMemberOutcome], name: &str) -> &'a BatchMemberOutcome {
        items
            .iter()
            .find(|item| item.component == name)
            .unwrap_or_else(|| panic!("no summary item for {name}"))
    }

    fn applied_member(outcome: UpdateOutcome) -> MemberApplicationOutcome {
        MemberApplicationOutcome {
            outcome: UpdateBatchOutcome::Completed(outcome),
            warnings: Vec::new(),
            adapter_actions: Vec::new(),
        }
    }

    #[test]
    fn merged_degrade_preserves_partial_application_failure() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        let merged_host = FakeHost::new(
            &[
                (
                    "copilot-shell",
                    pkg_info("copilot-shell", "1.0.0", Some("1.al4"), "x86_64"),
                ),
                (
                    "agent-sec-core",
                    pkg_info("agent-sec-core", "1.0.0", Some("1.al4"), "x86_64"),
                ),
            ],
            true,
        );
        let retry_host = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "1.0.0", Some("1.al4"), "x86_64")),
        )
        .post_update_missing();
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            if name != "cosh" {
                return Ok(applied_member(UpdateOutcome::AlreadyCurrent));
            }
            let outcome = super::super::application::run_with_dependencies_classified(
                super::super::application::ApplicationRequest {
                    component: name,
                    intent: ExecutionIntent::Apply,
                },
                &c,
                &retry_host,
                &retry_host,
                true,
                crate::test_support::raw_effects(),
            );
            Ok(application::member_application_outcome(outcome))
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &merged_host,
            &merged_host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        let cosh = find(&items, "cosh");
        assert_eq!(cosh.status, BatchMemberStatus::Partial);
        assert!(
            cosh.reason
                .as_deref()
                .is_some_and(|reason| reason.contains("repair")),
            "degraded application failure must retain repair guidance: {:?}",
            cosh.reason
        );
        assert_eq!(
            retry_host.update_call_count(),
            1,
            "the retry transaction ran"
        );
    }

    #[test]
    fn merged_update_package_accepts_only_the_u5_shape() {
        assert_eq!(
            merged_update_package(&u5_planned("cosh", "copilot-shell", "1.0.0-1.al4")).as_deref(),
            Some("copilot-shell")
        );

        let mut owned = u5_planned("cosh", "copilot-shell", "1.0.0-1.al4");
        owned.route = PlannedUpdateRoute::Owned;
        assert!(merged_update_package(&owned).is_none());

        let mut noop = u5_planned("cosh", "copilot-shell", "1.0.0-1.al4");
        noop.route = PlannedUpdateRoute::AlreadyCurrent;
        assert!(merged_update_package(&noop).is_none());

        // An install-shaped delegated plan must never merge into an update
        // transaction.
        let mut install_shaped = u5_planned("cosh", "copilot-shell", "1.0.0-1.al4");
        install_shaped.route = PlannedUpdateRoute::Delegated {
            steps: vec![
                Step::NativeTransaction {
                    pm: NativePm::Rpm,
                    action: NativeAction::Install,
                    packages: vec!["copilot-shell".to_string()],
                },
                Step::Observe {
                    packages: vec!["copilot-shell".to_string()],
                },
                Step::WriteRecord(RecordWrite::RefreshObservation),
            ],
        };
        assert!(merged_update_package(&install_shaped).is_none());
    }

    #[test]
    fn merged_updates_share_one_dnf_transaction_and_refresh_each_record() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        // rpmdb already shows the upgraded EVRs the merged transaction
        // produces.
        let host = FakeHost::new(
            &[
                (
                    "copilot-shell",
                    pkg_info("copilot-shell", "1.1.0", Some("1.al4"), "x86_64"),
                ),
                (
                    "agent-sec-core",
                    pkg_info("agent-sec-core", "1.1.0", Some("1.al4"), "x86_64"),
                ),
            ],
            false,
        );
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a committed merged transaction must not degrade ({name})");
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        assert_eq!(
            host.calls.borrow().as_slice(),
            &[(
                "update".to_string(),
                vec!["copilot-shell".to_string(), "agent-sec-core".to_string()]
            )],
            "both packages must share one dnf update"
        );
        assert_eq!(find(&items, "cosh").status, BatchMemberStatus::Updated);
        assert_eq!(find(&items, "sec-core").status, BatchMemberStatus::Updated);

        // Each record absorbed its own fresh observation.
        let store = load_store(&c);
        for (component, evr) in [("cosh", "1.1.0-1.al4"), ("sec-core", "1.1.0-1.al4")] {
            let record = store
                .find(ObjectKind::Component, component)
                .unwrap_or_else(|| panic!("record for {component}"));
            match &record.binding {
                anolisa_core::domain::ProviderBinding::Delegated { last_observed, .. } => {
                    assert_eq!(
                        last_observed.as_ref().expect("observation").evr.as_deref(),
                        Some(evr)
                    );
                }
                other => panic!("expected delegated binding, got {other:?}"),
            }
        }

        // The history shows one parent batch operation and each member's
        // operation linked to it — the audit trail names the shared
        // transaction instead of two unrelated updates.
        let parent = store
            .operations
            .iter()
            .find(|op| op.command == BATCH_COMMAND)
            .expect("parent batch operation");
        assert!(parent.id.starts_with("op-update-all-"), "{}", parent.id);
        assert_eq!(parent.status, "ok");
        assert!(parent.parent_operation_id.is_none());
        let members: Vec<_> = store
            .operations
            .iter()
            .filter(|op| op.command != BATCH_COMMAND)
            .collect();
        assert_eq!(members.len(), 2, "one operation per member");
        for op in members {
            assert_eq!(op.parent_operation_id.as_deref(), Some(parent.id.as_str()));
        }

        // Both journals settled; nothing pending blocks the next intent.
        let layout = common::resolve_layout(&c);
        let journal_dir = rpm_install::journal_dir(&layout);
        for component in ["cosh", "sec-core"] {
            assert!(
                pending_journal_for(JournalEvidence::new(&journal_dir, &[]), component)
                    .expect("scan journals")
                    .is_none()
            );
        }
    }

    #[test]
    fn merged_update_records_only_changed_sources_using_live_pre_transaction_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        let mut host = FakeHost::new(
            &[
                (
                    "copilot-shell",
                    pkg_info("copilot-shell", "1.1.0", Some("1.al4"), "x86_64"),
                ),
                (
                    "agent-sec-core",
                    pkg_info("agent-sec-core", "1.1.0", Some("1.al4"), "x86_64"),
                ),
            ],
            false,
        );
        host.before_update.insert(
            "copilot-shell".into(),
            pkg_info("copilot-shell", "1.0.0", Some("1.al4"), "x86_64"),
        );
        host.repository = Some("configured");
        let result = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            true,
            &mut |_| panic!("no retry"),
            &mut |_| {},
        );
        assert_eq!(result.items.len(), 2);
        let store = load_store(&c);
        // sec-core had already changed externally; the no-op transaction
        // establishes no source for that artifact and must discard stale history.
        for (component, source) in [("cosh", Some("configured")), ("sec-core", None)] {
            let record = store.find(ObjectKind::Component, component).unwrap();
            let anolisa_core::domain::ProviderBinding::Delegated {
                last_observed: Some(observation),
                ..
            } = &record.binding
            else {
                panic!("missing observation")
            };
            assert_eq!(observation.source_repo.as_deref(), source, "{component}");
        }
    }

    /// Every successful merged member refreshes its own contract snapshot,
    /// exactly like the single-component path — repair no longer has to
    /// reconcile a post-update drift.
    #[test]
    fn merged_updates_refresh_each_contract_snapshot() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        let (_, cosh_snapshot) = seed_package_contract_and_stale_snapshot(&c, "cosh");
        let (_, sec_snapshot) = seed_package_contract_and_stale_snapshot(&c, "sec-core");
        let host = FakeHost::new(
            &[
                (
                    "copilot-shell",
                    pkg_info("copilot-shell", "1.1.0", Some("1.al4"), "x86_64"),
                ),
                (
                    "agent-sec-core",
                    pkg_info("agent-sec-core", "1.1.0", Some("1.al4"), "x86_64"),
                ),
            ],
            false,
        );
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a committed merged transaction must not degrade ({name})");
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        assert_eq!(find(&items, "cosh").status, BatchMemberStatus::Updated);
        assert_eq!(find(&items, "sec-core").status, BatchMemberStatus::Updated);
        for snapshot in [&cosh_snapshot, &sec_snapshot] {
            assert_eq!(
                std::fs::read_to_string(snapshot).expect("read snapshot"),
                "framework = \"new\"\n",
                "each member must refresh its own snapshot"
            );
        }
        let store = load_store(&c);
        let parent = store
            .operations
            .iter()
            .find(|op| op.command == BATCH_COMMAND)
            .expect("parent batch operation");
        assert_eq!(parent.status, "ok");
    }

    /// A member whose contract refresh cannot complete is demoted alone: its
    /// item fails with a repair pointer and its operation reads `partial`,
    /// while the other member keeps its refreshed snapshot and `ok` status;
    /// the parent batch operation aggregates to `partial`.
    #[test]
    fn merged_member_contract_refresh_failure_only_demotes_that_member() {
        use anolisa_platform::fs_layout::FsLayout;

        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        // cosh publishes a contract, but a non-empty directory blocks its
        // snapshot slot; sec-core's refresh is healthy.
        let layout = common::resolve_layout(&c);
        let package_datadir = layout.package_datadir().expect("package datadir");
        let cosh_contract = FsLayout::component_contract_path(&package_datadir, "cosh");
        std::fs::create_dir_all(cosh_contract.parent().expect("contract parent"))
            .expect("mkdir contract");
        std::fs::write(&cosh_contract, "framework = \"new\"\n").expect("write cosh contract");
        let cosh_snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, "cosh");
        std::fs::create_dir_all(&cosh_snapshot).expect("create blocking snapshot directory");
        std::fs::write(cosh_snapshot.join("keep"), b"x").expect("write blocking marker");
        let (_, sec_snapshot) = seed_package_contract_and_stale_snapshot(&c, "sec-core");
        let host = FakeHost::new(
            &[
                (
                    "copilot-shell",
                    pkg_info("copilot-shell", "1.1.0", Some("1.al4"), "x86_64"),
                ),
                (
                    "agent-sec-core",
                    pkg_info("agent-sec-core", "1.1.0", Some("1.al4"), "x86_64"),
                ),
            ],
            false,
        );
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a committed merged transaction must not degrade ({name})");
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        let cosh_item = find(&items, "cosh");
        assert_eq!(cosh_item.status, BatchMemberStatus::Partial);
        let reason = cosh_item.reason.as_deref().expect("failure reason");
        assert!(
            reason.contains("committed") && reason.contains("repair"),
            "reason must state the update committed and point at repair: {reason}"
        );
        assert_eq!(find(&items, "sec-core").status, BatchMemberStatus::Updated);
        assert_eq!(
            std::fs::read_to_string(&sec_snapshot).expect("read snapshot"),
            "framework = \"new\"\n",
            "the healthy member must keep its refreshed snapshot"
        );

        let store = load_store(&c);
        let parent = store
            .operations
            .iter()
            .find(|op| op.command == BATCH_COMMAND)
            .expect("parent batch operation");
        assert_eq!(parent.status, "partial", "one demoted member is partial");
        let member_status = |command: &str| {
            store
                .operations
                .iter()
                .find(|op| op.command == command)
                .unwrap_or_else(|| panic!("no operation for {command}"))
                .status
                .clone()
        };
        assert_eq!(member_status("update cosh"), "partial");
        assert_eq!(member_status("update sec-core"), "ok");
    }

    #[test]
    fn pending_journal_injected_after_batch_planning_blocks_dnf_and_state_write() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        let layout = common::resolve_layout(&c);
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(&layout);
        let pending = Transaction::begin_with_subject(
            COMMAND,
            Some("cosh"),
            state_path.clone(),
            &journal_dir,
        )
        .expect("inject pending journal after planning");
        drop(pending);
        let state_before = std::fs::read(&state_path).expect("read state");
        let journals_before = std::fs::read_dir(&journal_dir)
            .expect("journal dir")
            .count();
        let host = FakeHost::new(
            &[(
                "copilot-shell",
                pkg_info("copilot-shell", "1.0.0", Some("1.al4"), "x86_64"),
            )],
            false,
        );
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a recovery-blocked member must not degrade ({name})");
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![u5_item("cosh", "copilot-shell", "1.0.0-1.al4")],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        assert!(host.calls.borrow().is_empty(), "dnf must not run");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, BatchMemberStatus::Failed);
        assert!(
            items[0]
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("anolisa repair cosh"))
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
    fn merged_update_failure_degrades_unmoved_members() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        // Both packages still read their pre-update EVR after the failure:
        // provably untouched slots, so both re-plan individually.
        let host = FakeHost::new(
            &[
                (
                    "copilot-shell",
                    pkg_info("copilot-shell", "1.0.0", Some("1.al4"), "x86_64"),
                ),
                (
                    "agent-sec-core",
                    pkg_info("agent-sec-core", "1.0.0", Some("1.al4"), "x86_64"),
                ),
            ],
            true,
        );
        let degraded = RefCell::new(Vec::new());
        let events = RefCell::new(Vec::new());
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            degraded.borrow_mut().push(name.to_string());
            events.borrow_mut().push(format!("degrade:{name}"));
            if name == "cosh" {
                Err(CliError::Runtime {
                    command: "update cosh".to_string(),
                    reason: "repo unreachable for copilot-shell".to_string(),
                })
            } else {
                Ok(applied_member(UpdateOutcome::Updated))
            }
        };
        let mut output = |event| {
            if let BatchOutputEvent::Warning(warning) = event {
                events.borrow_mut().push(format!("warning:{warning}"));
            }
        };

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        let events = events.borrow();
        assert!(events[0].contains("retrying 2 component(s) individually"));
        assert_eq!(events[1..], ["degrade:cosh", "degrade:sec-core"]);
        assert_eq!(degraded.borrow().as_slice(), ["cosh", "sec-core"]);
        let cosh = find(&items, "cosh");
        assert_eq!(cosh.status, BatchMemberStatus::Failed);
        assert!(cosh.reason.as_deref().unwrap().contains("repo unreachable"));
        assert_eq!(find(&items, "sec-core").status, BatchMemberStatus::Updated);

        // Clean failure journals never stay pending.
        let layout = common::resolve_layout(&c);
        let journal_dir = rpm_install::journal_dir(&layout);
        for component in ["cosh", "sec-core"] {
            assert!(
                pending_journal_for(JournalEvidence::new(&journal_dir, &[]), component)
                    .expect("scan journals")
                    .is_none()
            );
        }
    }

    #[test]
    fn merged_update_failure_keeps_partial_journal_for_moved_member() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        // copilot-shell's EVR moved despite the failed merged transaction —
        // real side effects; agent-sec-core is provably untouched.
        let host = FakeHost::new(
            &[
                (
                    "copilot-shell",
                    pkg_info("copilot-shell", "1.1.0", Some("1.al4"), "x86_64"),
                ),
                (
                    "agent-sec-core",
                    pkg_info("agent-sec-core", "1.0.0", Some("1.al4"), "x86_64"),
                ),
            ],
            true,
        );
        let degraded = RefCell::new(Vec::new());
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            degraded.borrow_mut().push(name.to_string());
            Ok(applied_member(UpdateOutcome::Updated))
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        // Forward-only for the moved member: no retry, repair reconciles.
        assert_eq!(degraded.borrow().as_slice(), ["sec-core"]);
        let cosh = find(&items, "cosh");
        assert_eq!(cosh.status, BatchMemberStatus::Partial);
        let reason = cosh.reason.as_deref().unwrap();
        assert!(reason.contains("changed in the rpmdb"), "got: {reason}");
        assert!(reason.contains("anolisa repair cosh"), "got: {reason}");
        assert_eq!(find(&items, "sec-core").status, BatchMemberStatus::Updated);

        let layout = common::resolve_layout(&c);
        let journal_dir = rpm_install::journal_dir(&layout);
        let pending = pending_journal_for(JournalEvidence::new(&journal_dir, &[]), "cosh")
            .expect("scan journals")
            .expect("partial journal for the moved member must stay pending");
        let journal = Transaction::load_journal(&pending).expect("load partial journal");
        let recovery = journal
            .delegated_recovery
            .expect("per-subject recovery contract");
        assert_eq!(recovery.package.as_deref(), Some("copilot-shell"));
        assert_eq!(
            recovery.record_action,
            anolisa_core::transaction::DelegatedRecordAction::Refresh
        );
        assert_eq!(journal.steps[0].target, "copilot-shell,agent-sec-core");
        assert!(
            pending_journal_for(JournalEvidence::new(&journal_dir, &[]), "sec-core")
                .expect("scan journals")
                .is_none()
        );
    }

    #[test]
    fn merged_update_failure_keeps_partial_journal_for_multiple_versions() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed_many(
            &c,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
                anolisa_core::state::ObjectStatus::Installed,
            )],
        );
        let host = FakeHost::new(&[], true).with_multiple_versions(&["copilot-shell"]);
        let degraded = RefCell::new(Vec::new());
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            degraded.borrow_mut().push(name.to_string());
            Ok(applied_member(UpdateOutcome::Updated))
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![u5_item("cosh", "copilot-shell", "1.0.0-1.al4")],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        assert_eq!(host.calls.borrow().len(), 1, "dnf must run only once");
        assert!(
            degraded.borrow().is_empty(),
            "an indeterminate rpmdb state must not trigger a second update"
        );
        let cosh = find(&items, "cosh");
        assert_eq!(cosh.status, BatchMemberStatus::Partial);
        assert!(
            cosh.reason
                .as_deref()
                .is_some_and(|reason| reason.contains("anolisa repair cosh"))
        );

        let layout = common::resolve_layout(&c);
        let journal_dir = rpm_install::journal_dir(&layout);
        assert!(
            pending_journal_for(JournalEvidence::new(&journal_dir, &[]), "cosh")
                .expect("scan journals")
                .is_some(),
            "an indeterminate result must retain its Partial journal"
        );
    }

    #[test]
    fn merged_update_without_root_fails_every_member_before_dnf() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        let host = FakeHost::new(&[], false);
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a refused merged group must not degrade ({name})");
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            false,
            &mut degrade,
            &mut output,
        )
        .items;

        assert!(host.calls.borrow().is_empty(), "dnf must not run");
        for component in ["cosh", "sec-core"] {
            let item = find(&items, component);
            assert_eq!(item.status, BatchMemberStatus::Failed);
            assert!(item.reason.as_deref().unwrap().contains("requires root"));
        }
    }

    #[test]
    fn merged_update_revalidates_authority_under_the_lock() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        // Only cosh is recorded; sec-core's plan is stale (its record is
        // gone), so its slot must be refused under the lock while cosh
        // proceeds.
        seed_many(
            &c,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
                anolisa_core::state::ObjectStatus::Installed,
            )],
        );
        let host = FakeHost::new(
            &[(
                "copilot-shell",
                pkg_info("copilot-shell", "1.1.0", Some("1.al4"), "x86_64"),
            )],
            false,
        );
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("must not degrade ({name})");
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        let sec = find(&items, "sec-core");
        assert_eq!(sec.status, BatchMemberStatus::Failed);
        assert!(
            sec.reason
                .as_deref()
                .unwrap()
                .contains("changed while this update was planning")
        );
        assert_eq!(find(&items, "cosh").status, BatchMemberStatus::Updated);
        // The refused member never entered the transaction.
        assert_eq!(
            host.calls.borrow().as_slice(),
            &[("update".to_string(), vec!["copilot-shell".to_string()])]
        );
    }

    /// A merged batch does not infer drift from directory bytes when its fake
    /// native backend supplies no authoritative file inventory.
    #[test]
    fn merged_updates_require_authoritative_inventory_for_adapter_actions() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        managed_pair(&c);
        let layout = common::resolve_layout(&c);

        // Both components publish trusted adapter contracts and v1 bundles.
        let cosh_root = layout.datadir.join("adapters/cosh/openclaw");
        let sec_root = layout.datadir.join("adapters/sec-core/openclaw");
        std::fs::create_dir_all(&cosh_root).expect("cosh bundle root");
        std::fs::create_dir_all(&sec_root).expect("sec-core bundle root");
        std::fs::write(cosh_root.join("plugin.json"), b"v1").expect("cosh bundle file");
        std::fs::write(sec_root.join("plugin.json"), b"v1").expect("sec-core bundle file");
        for component in ["cosh", "sec-core"] {
            let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
            std::fs::create_dir_all(snapshot.parent().expect("snapshot parent"))
                .expect("snapshot dir");
            std::fs::write(snapshot, raw_manifest_with_adapter(component, "1.0.0"))
                .expect("adapter contract");
        }

        let changed_cosh_root = cosh_root.clone();
        let host = FakeHost::new(
            &[
                (
                    "copilot-shell",
                    pkg_info("copilot-shell", "1.1.0", Some("1.al4"), "x86_64"),
                ),
                (
                    "agent-sec-core",
                    pkg_info("agent-sec-core", "1.1.0", Some("1.al4"), "x86_64"),
                ),
            ],
            false,
        )
        .with_on_update(Box::new(move || {
            std::fs::write(changed_cosh_root.join("plugin.json"), b"v2")
                .expect("replace cosh bundle");
        }));
        let mut degrade = |name: &str| -> Result<MemberApplicationOutcome, CliError> {
            panic!("a committed merged transaction must not degrade ({name})");
        };
        let mut output = |_: BatchOutputEvent| {};

        let items = execute_merged_updates_with_deps(
            vec![
                u5_item("cosh", "copilot-shell", "1.0.0-1.al4"),
                u5_item("sec-core", "agent-sec-core", "1.0.0-1.al4"),
            ],
            &c,
            &host,
            &host,
            true,
            &mut degrade,
            &mut output,
        )
        .items;

        assert_eq!(find(&items, "cosh").status, BatchMemberStatus::Updated);
        assert_eq!(find(&items, "sec-core").status, BatchMemberStatus::Updated);
        assert!(
            find(&items, "cosh").adapter_actions.is_empty()
                && find(&items, "sec-core").adapter_actions.is_empty(),
            "unmanaged directory bytes must not be treated as package revisions"
        );
    }

    /// The `plan` key is dry-run-only wire surface: absent entirely for
    /// executed items so existing consumers of the batch summary never see
    /// a new key outside preview mode. `adapter_actions` is the opposite:
    /// a stable key on every item (issue #1885), empty unless follow-up is
    /// required.
    #[test]
    fn summary_item_serializes_plan_only_on_dry_run_members() {
        let executed = UpdateAllItem {
            component: "cosh".to_string(),
            status: "updated",
            reason: None,
            plan: None,
            adapter_actions: Vec::new(),
        };
        let json = serde_json::to_value(&executed).expect("serialize");
        assert!(json.get("plan").is_none(), "{json}");
        assert_eq!(json["adapter_actions"], serde_json::json!([]), "{json}");

        let planned = u5_planned("cosh", "copilot-shell", "1.0.0-1.al4");
        let steps = match &planned.route {
            PlannedUpdateRoute::Delegated { steps } => steps.as_slice(),
            _ => unreachable!("u5 plans are delegated"),
        };
        let previewed = UpdateAllItem {
            component: "cosh".to_string(),
            status: "planned",
            reason: None,
            plan: Some(steps.iter().map(step_label).collect()),
            adapter_actions: vec![AdapterAction {
                component: "cosh".to_string(),
                framework: "openclaw".to_string(),
                reason: anolisa_core::adapter::claim::SOURCE_REVISION_CHANGED_REASON.to_string(),
                command: "anolisa adapter enable cosh openclaw".to_string(),
            }],
        };
        let json = serde_json::to_value(&previewed).expect("serialize");
        assert_eq!(json["plan"][0], "RPM package manager: update copilot-shell");
        assert_eq!(json["plan"][1], "observe copilot-shell");
        assert_eq!(
            json["adapter_actions"],
            serde_json::json!([{
                "component": "cosh",
                "framework": "openclaw",
                "reason": "adapter source revision changed since enable",
                "command": "anolisa adapter enable cosh openclaw",
            }]),
            "each batch item carries its own adapter actions (issue #1885): {json}"
        );
    }
}
