//! `anolisa repair <component>` — make record and reality agree again.
//!
//! Repair is the reconciliation verb of the thin-shell pipeline: the handler
//! assembles facts (including the integrity probe over owned files), asks
//! the planner (decision rows R1–R7), and routes the step sequence to the
//! matching executor. The decision table covers:
//!
//! - a pending operation journal → consume it (R1: recover or terminate);
//! - an owned record with damaged files → replay the recorded version (R2);
//! - a delegated record with a present package → absorb a fresh observation,
//!   backfilling a legacy record's package name (R3);
//! - a managed record whose package was removed externally → reinstall it
//!   through the native manager (R4);
//! - a quarantined record whose package the native db still knows → rebuild
//!   it as an observed delegated record (R5);
//! - a quarantined record whose owned files still verify intact → rebuild it
//!   as an active owned record (R6);
//! - everything else → an explicit refusal naming the way out.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{SecondsFormat, Utc};
use clap::Parser;
use serde::Serialize;

use anolisa_core::central_log::{CentralLog, LogKind, LogRecord, LogStatus, Severity};
use anolisa_core::domain::{
    Installation, InstallationScope, LifecycleStatus, ManagementRelation, NativePm, Observation,
    ProviderBinding,
};
use anolisa_core::executor::{DelegatedExecutionTarget, RecordSink, execute_delegated_steps};
use anolisa_core::facts::{JournalEvidence, JournalInventory};
use anolisa_core::lock::InstallLock;
use anolisa_core::owned_executor::{OwnedExecutionError, execute_owned_steps};
use anolisa_core::planner::{NativeProbe, PlanError, RecordWrite, Step};
use anolisa_core::providers::{DelegatedProvider, ProviderError};
use anolisa_core::record_sink::{DelegatedIdentity, RecordContext, StoreRecordSink};
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_migration::QuarantinedObject;
use anolisa_core::state_store::StateStore;
use anolisa_core::transaction::{
    DelegatedRecordAction, DelegatedRecoveryContext, Transaction, TransactionOutcomeStatus,
};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
#[cfg(test)]
use anolisa_platform::pkg_transaction::PackageTransaction;
use anolisa_platform::pkg_transaction::PackageTransactionError;
use anolisa_platform::privilege;

use crate::color::Palette;
use crate::commands::common;
use crate::commands::common::RepoPersistPolicy;
use crate::commands::tier1::install::RawEffectFactories;
use crate::commands::tier1::install::{
    QuarantineRestoreOps, RawReplayOps, ResolveInputs, inspect_datadir_contract_drift,
    refresh_datadir_contract_snapshot, resolve_raw, resolve_raw_inputs_for_component,
    rpm_package_candidates_with_index, snapshot_datadir_contract,
};
use crate::commands::tier1::recovery::LockedJournalGate;
use crate::commands::tier1::rpm_install::{self, PendingRpmInstall};
use crate::context::CliContext;
use crate::resolution::load_optional_component_index;
use crate::response::{CliError, render_json};

mod application;

/// Command label for JSON envelopes and error routing.
const COMMAND: &str = "repair";

/// Arguments for `anolisa repair <component>`.
#[derive(Debug, Parser)]
pub struct RepairArgs {
    /// Component whose record should be reconciled with reality
    #[arg(value_name = "COMPONENT")]
    pub component: String,
}

/// Wire shape for a `repair <component>` result (`--json`) and its dry-run
/// preview.
#[derive(Debug, Serialize)]
struct RepairResultPayload {
    component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    /// What repair did (or would do): `nothing-to-repair`,
    /// `refresh-observation`, `reinstall-package`, `replay-owned-files`,
    /// `restore-observed-record`, `restore-owned-record`, `recover-journal`,
    /// `recovered-pending-install`, or `recovered-journal`.
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    from_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_version: Option<String>,
    dry_run: bool,
    plan: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    /// Manifest-only reconciliation planned or completed alongside the state
    /// repair (`component manifest drift` when the package-owned contract no
    /// longer matches the state snapshot).
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_reconciliation: Option<&'static str>,
}

/// Dispatch `repair <component>` against the live host.
///
/// # Errors
///
/// Returns [`CliError`] when the component is untracked, its record is
/// unrecoverable, the native database is ambiguous, or an executor fails.
pub fn handle(args: RepairArgs, ctx: &CliContext) -> Result<(), CliError> {
    let outcome = application::run(
        application::ApplicationRequest {
            component: &args.component,
            intent: execution_intent(ctx),
        },
        ctx,
    )?;
    render_application_outcome(ctx, outcome)
}

/// Core of [`handle`] with the package backends injected so tests drive
/// every branch without a live rpmdb/dnf or real privileges.
#[cfg(test)]
pub(crate) fn repair_with_deps(
    target: &str,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
) -> Result<(), CliError> {
    let outcome = application::run_with_dependencies(
        application::ApplicationRequest {
            component: target,
            intent: execution_intent(ctx),
        },
        ctx,
        query,
        txn,
        is_root,
        crate::test_support::raw_effects(),
    )?;
    render_application_outcome(ctx, outcome)
}

fn execution_intent(ctx: &CliContext) -> anolisa_core::execution::ExecutionIntent {
    if ctx.dry_run {
        anolisa_core::execution::ExecutionIntent::Plan
    } else {
        anolisa_core::execution::ExecutionIntent::Apply
    }
}

enum LockedRepairExecution {
    Completed(Box<application::ApplicationOutcome>),
    RecoveryAppeared,
}

fn continue_after_locked_repair(
    execution: LockedRepairExecution,
    may_recover_journal: bool,
    target: &str,
    command: &str,
    retry: impl FnOnce() -> Result<application::ApplicationOutcome, CliError>,
) -> Result<application::ApplicationOutcome, CliError> {
    match execution {
        LockedRepairExecution::Completed(outcome) => Ok(*outcome),
        LockedRepairExecution::RecoveryAppeared if may_recover_journal => retry(),
        LockedRepairExecution::RecoveryAppeared => Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "another operation journal for '{target}' appeared after the last recovery; run `anolisa repair {target}` again"
            ),
        }),
    }
}

/// Execute a delegated repair plan (R3/R4/R5) under the install lock, with
/// the plan's authority re-validated against the locked read.
#[expect(clippy::too_many_arguments)]
fn repair_delegated(
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    journal_dir: &Path,
    scope: InstallationScope,
    now: &str,
    steps: &[Step],
    package: &str,
    from_version: Option<String>,
    provider: &DelegatedProvider<'_>,
    manifest_drifted: bool,
    command: &str,
) -> Result<LockedRepairExecution, CliError> {
    let _lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;
    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let mut journal_gate = LockedJournalGate::load(&_lock, evidence, command)?;
    if journal_gate.pending_path(target).is_some() {
        return Ok(LockedRepairExecution::RecoveryAppeared);
    }
    if !delegated_repair_authorized(&store, target, package, steps) {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "component '{target}' changed while this repair was planning; nothing was changed — re-run `anolisa repair {target}`"
            ),
        });
    }

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
            package: package.to_string(),
        }),
        owned_artifact: None,
    };
    let outcome = {
        let mut sink = StoreRecordSink::new(&mut store, state_path, context);
        execute_delegated_steps(
            steps,
            DelegatedExecutionTarget::new(NativePm::Rpm, Some(package)),
            provider,
            &mut sink,
            &mut journal,
            now,
        )
    }
    .map_err(|err| match err {
        anolisa_core::executor::ExecutionError::TransactionFailed {
            source:
                ProviderError::Transaction(PackageTransactionError::CommandMissing { command: bin }),
            ..
        } => rpm_tooling_missing_error(command, &bin, target),
        other => CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "repair of '{target}' failed: {other}; the native transaction is never undone automatically — run `anolisa repair {target}` again to reconcile"
            ),
        },
    })?;

    // Operation history is best-effort bookkeeping on top of the committed
    // record: the repair already succeeded, so a history-write failure
    // degrades to a warning instead of unwinding anything. With a drifted
    // contract snapshot the status stays `partial` until the refresh below
    // completes, so a durable status can never overstate what happened.
    store.operations.push(OperationRecord {
        id: operation_id.clone(),
        command: command.to_string(),
        status: if manifest_drifted { "partial" } else { "ok" }.to_string(),
        started_at: now.to_string(),
        finished_at: Some(now_iso8601()),
        parent_operation_id: None,
    });
    if let Err(err) = store.save(state_path) {
        eprintln!("warning: failed to record operation history: {err}");
    }

    let mut completion_failure = None;
    if manifest_drifted {
        let refresh =
            refresh_datadir_contract_snapshot(layout, target, command, ctx.packaged_data_probe());
        if !ctx.quiet {
            for warning in &refresh.warnings {
                eprintln!("warning: {warning}");
            }
        }
        completion_failure = refresh
            .failure_detail()
            .map(|detail| format!("component manifest reconciliation did not complete: {detail}"));
        if completion_failure.is_none() {
            if let Some(operation) = store.operations.last_mut() {
                operation.status = "ok".to_string();
            }
            if let Err(err) = store.save(state_path) {
                completion_failure = Some(format!(
                    "component manifest reconciliation completed, but the repair operation could not be finalized as ok: {err}"
                ));
            }
        }
    }

    let to_version = outcome
        .observation
        .as_ref()
        .map(|o| o.evr.clone().unwrap_or_else(|| o.version.clone()));

    if let Some(reason) = completion_failure {
        append_repair_log(
            layout,
            ctx,
            target,
            command,
            &operation_id,
            now,
            LogStatus::Partial,
            format!(
                "repaired component {target} ({package}): {}, but the component manifest reconciliation did not complete",
                plan_action(steps)
            ),
        );
        return Ok(LockedRepairExecution::Completed(Box::new(
            application::partially_applied(
                command,
                application::RepairSubject {
                    component: target.to_string(),
                    package: Some(package.to_string()),
                    from_version,
                    to_version,
                },
                plan_action(steps),
                steps.to_vec(),
                operation_id,
                format!("the record for '{target}' was repaired, but {reason}"),
                Some("component manifest drift"),
            ),
        )));
    }

    append_repair_log(
        layout,
        ctx,
        target,
        command,
        &operation_id,
        now,
        LogStatus::Ok,
        format!(
            "repaired component {target} ({package}): {}",
            plan_action(steps)
        ),
    );

    Ok(LockedRepairExecution::Completed(Box::new(
        application::applied(
            command,
            application::RepairSubject {
                component: target.to_string(),
                package: Some(package.to_string()),
                from_version,
                to_version,
            },
            plan_action(steps),
            steps.to_vec(),
            operation_id,
            Vec::new(),
            manifest_drifted.then_some("component manifest drift"),
        ),
    )))
}

/// Execute the quarantine-restore plan (R6) under the install lock: rebuild
/// the active owned record from the quarantined legacy record.
#[expect(clippy::too_many_arguments)]
fn repair_restore_quarantined(
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    journal_dir: &Path,
    scope: InstallationScope,
    now: &str,
    steps: &[Step],
    command: &str,
) -> Result<LockedRepairExecution, CliError> {
    let _lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;
    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let mut journal_gate = LockedJournalGate::load(&_lock, evidence, command)?;
    if journal_gate.pending_path(target).is_some() {
        return Ok(LockedRepairExecution::RecoveryAppeared);
    }
    // Re-validate under the lock: the quarantined record must still exist
    // and no active record may have claimed the name in the window.
    let version = match (
        quarantined_record(&store, target),
        store.find(ObjectKind::Component, target),
    ) {
        (Some(q), None) => q.record.version.clone(),
        _ => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "component '{target}' changed while this repair was planning; nothing was changed — re-run `anolisa repair {target}`"
                ),
            });
        }
    };

    let mut journal = journal_gate.begin(COMMAND, target, state_path.to_path_buf(), command)?;
    let operation_id = journal.operation_id.clone();

    {
        let mut ops = QuarantineRestoreOps::new(
            target.to_string(),
            scope,
            operation_id.clone(),
            &mut store,
            state_path,
        );
        execute_owned_steps(steps, &mut ops, &mut journal)
    }
    .map_err(|err| owned_error_to_cli(err, target, scope, command))?;

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

    append_repair_log(
        layout,
        ctx,
        target,
        command,
        &operation_id,
        now,
        LogStatus::Ok,
        format!("restored quarantined record for component {target} as owned ({version})"),
    );

    Ok(LockedRepairExecution::Completed(Box::new(
        application::applied(
            command,
            application::RepairSubject {
                component: target.to_string(),
                package: None,
                from_version: None,
                to_version: Some(version),
            },
            application::RepairAction::RestoreOwnedRecord,
            steps.to_vec(),
            operation_id,
            Vec::new(),
            None,
        ),
    )))
}

/// Execute an owned replay plan (R2) against the raw backend, pinned to the
/// recorded version — repair re-places what is installed, it never upgrades.
///
/// The store is re-read under the install lock so the backup/remove set can
/// never come from a stale snapshot; a version drift under the lock aborts
/// before anything is touched.
#[expect(clippy::too_many_arguments)]
fn repair_owned_replay(
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    journal_dir: &Path,
    scope: InstallationScope,
    now: &str,
    steps: &[Step],
    prior: anolisa_core::domain::OwnedArtifact,
    command: &str,
    effects: RawEffectFactories<'_>,
) -> Result<LockedRepairExecution, CliError> {
    // No root pre-check: `--prefix` may point at a user-writable tree, and a
    // genuine permission problem fails the exact step and unwinds honestly.

    // Re-resolve the recorded package at the recorded version to recover the
    // artifact URL and its published sha256.
    let repo_config = common::load_repo_config(ctx, layout, command, RepoPersistPolicy::Require)?;
    let env = anolisa_env::EnvService::detect();
    let inputs = ResolveInputs {
        version: Some(prior.version.as_str()),
        ..resolve_raw_inputs_for_component(
            target.to_string(),
            "raw",
            prior.raw_package.as_deref(),
            &env,
            &repo_config,
            command,
        )?
    };
    let resolution = resolve_raw(ctx, layout, &env, inputs).map_err(|e| e.with_command(command))?;
    let resolve_warnings = resolution.warnings.clone();
    let package = resolution.package.clone();
    let version = prior.version.clone();

    let _lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;
    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let mut journal_gate = LockedJournalGate::load(&_lock, evidence, command)?;
    if journal_gate.pending_path(target).is_some() {
        return Ok(LockedRepairExecution::RecoveryAppeared);
    }
    let prior = match store
        .find(ObjectKind::Component, target)
        .map(|record| &record.binding)
    {
        Some(ProviderBinding::Owned { artifact }) if artifact.version == prior.version => {
            artifact.clone()
        }
        _ => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "component '{target}' changed while this repair was planning; nothing was changed — re-run `anolisa repair {target}`"
                ),
            });
        }
    };
    let mut journal = journal_gate.begin(COMMAND, target, state_path.to_path_buf(), command)?;
    let operation_id = journal.operation_id.clone();

    let outcome = {
        let mut ops = RawReplayOps::new(
            ctx,
            effects,
            layout,
            target.to_string(),
            scope,
            now.to_string(),
            operation_id.clone(),
            resolution,
            prior,
            &mut store,
            state_path,
        );
        let result = execute_owned_steps(steps, &mut ops, &mut journal);
        if result.is_ok() {
            // Per-operation backups are rollback scratch; a failed plan keeps
            // them on disk for forensics.
            ops.discard_backups();
        }
        result
    }
    .map_err(|err| owned_error_to_cli(err, target, scope, command))?;

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

    for warning in resolve_warnings.iter().chain(outcome.warnings.iter()) {
        eprintln!("warning: {warning}");
    }

    append_repair_log(
        layout,
        ctx,
        target,
        command,
        &operation_id,
        now,
        LogStatus::Ok,
        format!("replayed owned files for component {target} at {version}"),
    );

    Ok(LockedRepairExecution::Completed(Box::new(
        application::applied(
            command,
            application::RepairSubject {
                component: target.to_string(),
                package: Some(package),
                from_version: None,
                to_version: Some(version),
            },
            application::RepairAction::ReplayOwnedFiles,
            steps.to_vec(),
            operation_id,
            Vec::new(),
            None,
        ),
    )))
}

/// What consuming a pending journal (R1) left behind.
enum Recovery {
    /// The interrupted operation was completed (its record committed) and
    /// the result rendered; nothing further to do.
    Recovered(Box<application::ApplicationOutcome>),
    /// The journal was settled without a record change; the caller replans
    /// once against the now-unblocked facts.
    Cleared,
}

/// Consume the pending journal attributed to `target` (R1), under the lock.
///
/// A legacy pending RPM install claim is completed the way the old recovery
/// path did — re-observe, and either commit the managed record or terminate
/// the journal. Any other journal is classified by its steps: a delegated
/// operation whose package survived is settled (the replan absorbs the fresh
/// observation, or its missing record is committed here); an owned
/// operation's compensation state died with the interrupted process, so its
/// journal is terminated and the replan decides whether files need replaying
/// (R2) or nothing does.
#[expect(clippy::too_many_arguments)]
fn recover_journal(
    input: &str,
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    journal_dir: &Path,
    scope: InstallationScope,
    now: &str,
    provider: &DelegatedProvider<'_>,
    command: &str,
) -> Result<Recovery, CliError> {
    let _lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;

    // Re-find the journal under the lock; a concurrent repair may have
    // consumed it, which simply unblocks the replan.
    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let inventory = JournalInventory::load(evidence).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: err.to_string(),
    })?;
    let Some(blocker) = inventory.blocking_for(target) else {
        return Ok(Recovery::Cleared);
    };

    // Legacy pending RPM install claim (the pre-pipeline durable-intent
    // journal): conservative blocking alone does not establish ownership,
    // so recover only an exact, unique component or package claim.
    if blocker.transaction().subject.is_none() {
        let claims = if input == target {
            vec![target]
        } else {
            vec![target, input]
        };
        let pending = rpm_install::find_pending_claim_in_inventory(
            layout,
            &claims,
            command,
            &inventory,
        )?
        .filter(|pending| pending.transaction.journal_path == blocker.path())
        .ok_or_else(|| CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "pending journal at {} has no subject attributable to '{target}'; it was left pending because automatic recovery would be unsafe",
                blocker.path().display()
            ),
        })?;
        return recover_legacy_rpm_install(
            pending, ctx, layout, &mut store, state_path, scope, now, provider, command,
        );
    }

    let entry = inventory
        .recoverable_for(target)
        .ok_or_else(|| CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "pending journal at {} does not belong to '{target}'; it was left pending",
                blocker.path().display()
            ),
        })?;
    let path = entry.path().to_path_buf();
    let mut journal = entry.transaction().clone();

    // Preserve strict rejection of subject-bound journals that imitate only
    // part of the legacy RPM protocol.
    if let Some(pending) = rpm_install::parse_pending(journal.clone(), &path, layout, command)? {
        return recover_legacy_rpm_install(
            pending, ctx, layout, &mut store, state_path, scope, now, provider, command,
        );
    }

    let Some(recovery) = delegated_recovery_context(&journal, &store, target, command)? else {
        if journal.operation == "install" {
            let committed = store
                .find(ObjectKind::Component, target)
                .is_some_and(|record| {
                    record.scope == scope
                        && record.status == LifecycleStatus::Installed
                        && record.last_operation_id.as_deref()
                            == Some(journal.operation_id.as_str())
                        && matches!(record.binding, ProviderBinding::Owned { .. })
                });
            if committed {
                journal
                    .finish(TransactionOutcomeStatus::Ok)
                    .map_err(|err| journal_finish_error(&journal, command, err))?;
                return Ok(Recovery::Cleared);
            }
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "pending fresh owned install for '{target}' has no durable recovery context; the journal was left pending because automatic recovery would be unsafe"
                ),
            });
        }
        journal
            .finish(TransactionOutcomeStatus::Failed)
            .map_err(|err| journal_finish_error(&journal, command, err))?;
        return Ok(Recovery::Cleared);
    };

    if recovery.record_action == DelegatedRecordAction::Drop {
        return recover_delegated_drop(
            target, ctx, layout, state_path, scope, now, provider, command, &mut store, journal,
            recovery,
        );
    }
    let package = recovery.package.as_deref().ok_or_else(|| CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "pending delegated operation for '{target}' has no subject package; the journal was left pending because automatic recovery would be unsafe"
        ),
    })?;

    match provider
        .observe(package, now)
        .map_err(|err| probe_error(err, command, target))?
    {
        NativeProbe::Present { observation, .. } => {
            // A pinned operation resolved a specific artifact. Recovery must
            // hold the same guarantee as the in-process path: refuse to record
            // a package whose installed EVR/arch differs from the pin (a
            // different build committed before the crash, or the wrong version
            // is present). The journal is left pending for manual reconcile —
            // recovery never falls back to whatever is installed.
            if let Some(pin) = &recovery.pinned {
                let observed_evr = observation
                    .evr
                    .clone()
                    .unwrap_or_else(|| observation.version.clone());
                let arch_ok = observation.arch.as_deref() == Some(pin.arch.as_str());
                if observed_evr != pin.evr || !arch_ok {
                    let observed_build = match observation
                        .arch
                        .as_deref()
                        .filter(|arch| !arch.trim().is_empty())
                    {
                        Some(arch) => format!("{observed_evr} ({arch})"),
                        None => format!("{observed_evr} (architecture unavailable from rpmdb)"),
                    };
                    return Err(CliError::Runtime {
                        command: command.to_string(),
                        reason: format!(
                            "the interrupted {} of '{target}' pinned '{}', but package '{package}' is installed as {observed_build}; refusing to record a different version — reconcile manually (e.g. `dnf install {}`) then re-run `anolisa repair {target}`",
                            journal.operation, pin.artifact, pin.artifact
                        ),
                    });
                }
            }
            ensure_recovery_write_authorized(
                &store,
                target,
                package,
                recovery.pm,
                recovery.record_action,
                command,
            )?;
            if let Some(owner) = store_claim_owner(&store, target, package)
                && owner.name != target
            {
                return Err(CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "pending operation for component '{target}' (package '{package}') conflicts with existing state component '{}'; refusing to overwrite either owner",
                        owner.name
                    ),
                });
            }
            let operation_id = journal.operation_id.clone();
            commit_recovered_delegated(
                &mut store,
                state_path,
                target,
                Some(package),
                Some(&observation),
                scope,
                &journal.started_at,
                &operation_id,
                recovery.pm,
                recovery.record_action,
                command,
            )?;
            journal
                .finish(TransactionOutcomeStatus::Ok)
                .map_err(|err| journal_finish_error(&journal, command, err))?;
            append_repair_log(
                layout,
                ctx,
                target,
                command,
                &operation_id,
                now,
                LogStatus::Ok,
                format!(
                    "recovered interrupted {} of component {target} (package {package}) with {}",
                    journal.operation,
                    delegated_record_action_label(recovery.record_action),
                ),
            );
            Ok(Recovery::Recovered(Box::new(application::applied(
                command,
                application::RepairSubject {
                    component: target.to_string(),
                    package: Some(package.to_string()),
                    from_version: None,
                    to_version: Some(
                        observation
                            .evr
                            .clone()
                            .unwrap_or_else(|| observation.version.clone()),
                    ),
                },
                application::RepairAction::RecoveredJournal,
                Vec::new(),
                operation_id,
                Vec::new(),
                None,
            ))))
        }
        NativeProbe::MultipleVersions { .. } => Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "package '{package}' has multiple installed versions; cannot recover the pending operation for '{target}' unambiguously — resolve the duplicate install (e.g. `dnf remove` the stale version) and re-run `anolisa repair {target}`"
            ),
        }),
        NativeProbe::Absent | NativeProbe::NotProbed => {
            // The package is gone: the interrupted operation cannot be
            // completed from here. Terminate the journal so it stops
            // blocking, and hand the retry to the user.
            let operation = journal.operation.clone();
            journal
                .finish(TransactionOutcomeStatus::Failed)
                .map_err(|err| journal_finish_error(&journal, command, err))?;
            Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "the interrupted {operation} of '{target}' left package '{package}' absent; its journal is now Failed and no longer blocks operations — retry `anolisa {operation} {target}`, or run `anolisa forget {target}` if the record should be dropped"
                ),
            })
        }
    }
}

/// Read the explicit per-subject contract, or conservatively decode a
/// journal written before that additive field existed.
fn delegated_recovery_context(
    journal: &Transaction,
    store: &StateStore,
    target: &str,
    command: &str,
) -> Result<Option<DelegatedRecoveryContext>, CliError> {
    if let Some(context) = &journal.delegated_recovery {
        if journal.subject.as_deref() != Some(target) {
            return Err(unsafe_recovery_contract_error(
                journal,
                command,
                "the explicit delegated recovery contract has no exact subject",
            ));
        }
        if journal.steps.is_empty() {
            return Err(unsafe_recovery_contract_error(
                journal,
                command,
                "the explicit delegated recovery contract has no journal steps",
            ));
        }
        if context.record_action != DelegatedRecordAction::Drop
            && context
                .package
                .as_deref()
                .is_none_or(|package| package.trim().is_empty())
        {
            return Err(unsafe_recovery_contract_error(
                journal,
                command,
                "the explicit delegated recovery contract has no package",
            ));
        }
        if journal.steps.iter().any(|step| {
            record_action_from_journal_action(&step.action).is_some()
                && delegated_action_from_journal_step(step).is_none()
        }) {
            return Err(unsafe_recovery_contract_error(
                journal,
                command,
                "the explicit record transition uses a non-delegated phase or target",
            ));
        }
        if journal
            .steps
            .iter()
            .filter_map(delegated_action_from_journal_step)
            .any(|action| action != context.record_action)
        {
            return Err(unsafe_recovery_contract_error(
                journal,
                command,
                "the explicit record transition conflicts with the journal steps",
            ));
        }
        if let Some(package) = context.package.as_deref() {
            // The observe step re-observes the bare package; the native
            // transaction step names the pinned artifact (NEVRA) when the
            // operation was version-pinned, otherwise the bare package.
            //
            // A pinned operation is never a merged batch, so both its native
            // and observe steps must resolve to *exactly* one target — the
            // pinned artifact and the bare package respectively. A journal
            // whose pinned native step also lists another package is malformed
            // and must not recover. Only an unpinned batch native step keeps
            // membership semantics (a shared transaction names the whole
            // batch, and this subject need only appear among them).
            let contains = |step: &anolisa_core::transaction::TransactionStep, needle: &str| {
                step.target
                    .split(',')
                    .map(str::trim)
                    .any(|candidate| candidate == needle)
            };
            let resolves_exactly = |step: &anolisa_core::transaction::TransactionStep,
                                    expected: &str| {
                let mut targets = step
                    .target
                    .split(',')
                    .map(str::trim)
                    .filter(|target| !target.is_empty());
                targets.next() == Some(expected) && targets.next().is_none()
            };
            let conflict = journal.steps.iter().any(|step| {
                if step.phase == anolisa_core::executor::PHASE_NATIVE_TXN {
                    match &context.pinned {
                        Some(pin) => !resolves_exactly(step, &pin.artifact),
                        None => !contains(step, package),
                    }
                } else if step.phase == anolisa_core::executor::PHASE_OBSERVE {
                    !resolves_exactly(step, package)
                } else {
                    false
                }
            });
            if conflict {
                return Err(unsafe_recovery_contract_error(
                    journal,
                    command,
                    "the explicit package identity conflicts with the journal steps",
                ));
            }
        }
        return Ok(Some(context.clone()));
    }

    if journal.steps.iter().any(|step| {
        record_action_from_journal_action(&step.action).is_some()
            && delegated_action_from_journal_step(step).is_none()
            && !is_owned_drop_record_step(step)
    }) {
        return Err(unsafe_recovery_contract_error(
            journal,
            command,
            "the legacy record transition uses an unknown phase or target",
        ));
    }

    let mut action: Option<DelegatedRecordAction> = None;
    for candidate in journal
        .steps
        .iter()
        .filter_map(delegated_action_from_journal_step)
    {
        if action.is_some_and(|existing| existing != candidate) {
            return Err(unsafe_recovery_contract_error(
                journal,
                command,
                "the legacy journal contains conflicting record transitions",
            ));
        }
        action = Some(candidate);
    }

    let native_packages: BTreeSet<String> = journal
        .steps
        .iter()
        .filter(|step| step.phase == anolisa_core::executor::PHASE_NATIVE_TXN)
        .flat_map(|step| step.target.split(','))
        .map(str::trim)
        .filter(|package| !package.is_empty())
        .map(str::to_string)
        .collect();
    let observed_packages: BTreeSet<String> = journal
        .steps
        .iter()
        .filter(|step| step.phase == anolisa_core::executor::PHASE_OBSERVE)
        .flat_map(|step| step.target.split(','))
        .map(str::trim)
        .filter(|package| !package.is_empty())
        .map(str::to_string)
        .collect();
    if observed_packages.len() > 1 {
        return Err(unsafe_recovery_contract_error(
            journal,
            command,
            &format!(
                "the legacy journal observes multiple packages ({}) but has no per-subject mapping",
                observed_packages.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    let observed_package = observed_packages.into_iter().next();
    if let Some(observed) = observed_package.as_deref()
        && !native_packages.is_empty()
        && !native_packages.contains(observed)
    {
        return Err(unsafe_recovery_contract_error(
            journal,
            command,
            "the legacy observe step conflicts with its native transaction",
        ));
    }
    if observed_package.is_none() && native_packages.len() > 1 {
        return Err(unsafe_recovery_contract_error(
            journal,
            command,
            &format!(
                "the legacy journal names multiple packages ({}) but has no per-subject mapping",
                native_packages.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }

    // An old owned journal has neither a delegated phase nor a delegated
    // record transition. Do not infer from its operation verb alone.
    if native_packages.is_empty() && observed_package.is_none() && action.is_none() {
        return Ok(None);
    }
    let action = action
        .or(match journal.operation.as_str() {
            "install" => Some(DelegatedRecordAction::WriteManaged),
            "adopt" => Some(DelegatedRecordAction::WriteAdopted),
            "update" | "repair" => Some(DelegatedRecordAction::Refresh),
            "uninstall" => Some(DelegatedRecordAction::Drop),
            _ => None,
        })
        .ok_or_else(|| {
            unsafe_recovery_contract_error(
                journal,
                command,
                "the legacy journal does not identify its intended record transition",
            )
        })?;

    let package = observed_package
        .or_else(|| native_packages.into_iter().next())
        .or_else(|| {
            store
                .find(ObjectKind::Component, target)
                .and_then(|record| match &record.binding {
                    ProviderBinding::Delegated { package, .. } => {
                        package.resolved_name().map(str::to_string)
                    }
                    ProviderBinding::Owned { .. } => None,
                })
        });
    if package.is_none() && action != DelegatedRecordAction::Drop {
        return Err(unsafe_recovery_contract_error(
            journal,
            command,
            "the legacy journal has no unique package identity",
        ));
    }
    Ok(Some(DelegatedRecoveryContext {
        pm: NativePm::Rpm,
        package,
        record_action: action,
        pinned: None,
    }))
}

fn delegated_action_from_journal_step(
    step: &anolisa_core::transaction::TransactionStep,
) -> Option<DelegatedRecordAction> {
    if step.phase != anolisa_core::executor::PHASE_RECORD || step.target != "state" {
        return None;
    }
    record_action_from_journal_action(&step.action)
}

fn record_action_from_journal_action(action: &str) -> Option<DelegatedRecordAction> {
    match action {
        "write-delegated-managed" => Some(DelegatedRecordAction::WriteManaged),
        "write-delegated-adopted" => Some(DelegatedRecordAction::WriteAdopted),
        "write-delegated-observed" => Some(DelegatedRecordAction::WriteObserved),
        "refresh-observation" => Some(DelegatedRecordAction::Refresh),
        "drop-record" => Some(DelegatedRecordAction::Drop),
        _ => None,
    }
}

fn is_owned_drop_record_step(step: &anolisa_core::transaction::TransactionStep) -> bool {
    step.phase == anolisa_core::owned_executor::PHASE_RECORD
        && step.target == "state"
        && step.action == "drop-record"
}

fn unsafe_recovery_contract_error(journal: &Transaction, command: &str, detail: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "cannot recover pending journal {} safely: {detail}; the journal was left pending — inspect it and cross-check installed.toml before changing recovery state",
            journal.journal_path.display()
        ),
    }
}

fn ensure_recovery_write_authorized(
    store: &StateStore,
    target: &str,
    package: &str,
    pm: NativePm,
    action: DelegatedRecordAction,
    command: &str,
) -> Result<(), CliError> {
    let Some(existing) = store.find(ObjectKind::Component, target) else {
        return Ok(());
    };
    let ProviderBinding::Delegated {
        pm: existing_pm,
        package: existing_package,
        relation,
        ..
    } = &existing.binding
    else {
        return Err(recovery_record_conflict(target, package, command));
    };
    let identity_matches = *existing_pm == pm
        && existing_package
            .resolved_name()
            .is_none_or(|existing| existing == package);
    let relation_allows = match action {
        DelegatedRecordAction::WriteManaged => {
            matches!(relation, ManagementRelation::Managed { .. })
        }
        DelegatedRecordAction::WriteAdopted => matches!(
            relation,
            ManagementRelation::Observed | ManagementRelation::Adopted { .. }
        ),
        DelegatedRecordAction::WriteObserved => {
            matches!(relation, ManagementRelation::Observed)
        }
        DelegatedRecordAction::Refresh => true,
        DelegatedRecordAction::Drop => true,
    };
    if identity_matches && relation_allows {
        Ok(())
    } else {
        Err(recovery_record_conflict(target, package, command))
    }
}

fn recovery_record_conflict(target: &str, package: &str, command: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "state for '{target}' no longer matches the pending recovery contract for package '{package}'; refusing to overwrite its current authority — inspect the pending journal and installed.toml"
        ),
    }
}

fn authorize_recovery_drop(
    store: &StateStore,
    target: &str,
    recovery: &DelegatedRecoveryContext,
    command: &str,
) -> Result<bool, CliError> {
    let Some(existing) = store.find(ObjectKind::Component, target) else {
        if quarantined_record(store, target).is_some() && recovery.package.is_some() {
            return Err(recovery_record_conflict(
                target,
                recovery.package.as_deref().unwrap_or("<missing>"),
                command,
            ));
        }
        return Ok(false);
    };
    let package = recovery
        .package
        .as_deref()
        .ok_or_else(|| recovery_record_conflict(target, "<missing>", command))?;
    let ProviderBinding::Delegated {
        pm,
        package: recorded_package,
        relation,
        ..
    } = &existing.binding
    else {
        return Err(recovery_record_conflict(target, package, command));
    };
    if *pm != recovery.pm || recorded_package.resolved_name() != Some(package) {
        return Err(recovery_record_conflict(target, package, command));
    }
    Ok(matches!(relation, ManagementRelation::Managed { .. }))
}

#[expect(clippy::too_many_arguments)]
fn recover_delegated_drop(
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    scope: InstallationScope,
    now: &str,
    provider: &DelegatedProvider<'_>,
    command: &str,
    store: &mut StateStore,
    mut journal: Transaction,
    recovery: DelegatedRecoveryContext,
) -> Result<Recovery, CliError> {
    let managed = authorize_recovery_drop(store, target, &recovery, command)?;
    let package = recovery.package.as_deref();
    let ran_native_remove = journal.steps.iter().any(|step| {
        step.phase == anolisa_core::executor::PHASE_NATIVE_TXN && step.action == "remove"
    });
    if let Some(package) = package {
        match provider
            .observe(package, now)
            .map_err(|err| probe_error(err, command, target))?
        {
            NativeProbe::Present { .. } if ran_native_remove => {
                journal
                    .finish(TransactionOutcomeStatus::Failed)
                    .map_err(|err| journal_finish_error(&journal, command, err))?;
                return Err(CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "the interrupted uninstall of '{target}' left package '{package}' present; its journal is now Failed and state was preserved — re-run `anolisa uninstall {target}`"
                    ),
                });
            }
            NativeProbe::Present { .. } if managed => {
                return Err(unsafe_recovery_contract_error(
                    &journal,
                    command,
                    "a managed record is still backed by a present package but the journal has no native removal",
                ));
            }
            NativeProbe::MultipleVersions { .. } => {
                return Err(CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "package '{package}' has multiple installed versions; cannot settle the pending uninstall for '{target}' unambiguously"
                    ),
                });
            }
            NativeProbe::Present { .. } | NativeProbe::Absent | NativeProbe::NotProbed => {}
        }
    } else if ran_native_remove {
        return Err(unsafe_recovery_contract_error(
            &journal,
            command,
            "a native removal has no package identity",
        ));
    }

    let operation_id = journal.operation_id.clone();
    commit_recovered_delegated(
        store,
        state_path,
        target,
        package,
        None,
        scope,
        &journal.started_at,
        &operation_id,
        recovery.pm,
        DelegatedRecordAction::Drop,
        command,
    )?;
    journal
        .finish(TransactionOutcomeStatus::Ok)
        .map_err(|err| journal_finish_error(&journal, command, err))?;
    append_repair_log(
        layout,
        ctx,
        target,
        command,
        &operation_id,
        now,
        LogStatus::Ok,
        format!("completed interrupted record removal for component {target}"),
    );
    Ok(Recovery::Recovered(Box::new(application::applied(
        command,
        application::RepairSubject {
            component: target.to_string(),
            package: package.map(str::to_string),
            from_version: None,
            to_version: None,
        },
        application::RepairAction::RecoveredJournal,
        Vec::new(),
        operation_id,
        Vec::new(),
        None,
    ))))
}

fn delegated_record_action_label(action: DelegatedRecordAction) -> &'static str {
    match action {
        DelegatedRecordAction::WriteManaged => "managed record write",
        DelegatedRecordAction::WriteAdopted => "adopted record write",
        DelegatedRecordAction::WriteObserved => "observed record write",
        DelegatedRecordAction::Refresh => "observation refresh",
        DelegatedRecordAction::Drop => "record removal",
    }
}

/// Complete a legacy pending RPM install claim against the v5 store:
/// re-observe, and either commit the managed record (package present) or
/// terminate the journal (package absent) so installation can be retried.
#[expect(clippy::too_many_arguments)]
fn recover_legacy_rpm_install(
    mut pending: PendingRpmInstall,
    ctx: &CliContext,
    layout: &FsLayout,
    store: &mut StateStore,
    state_path: &Path,
    scope: InstallationScope,
    now: &str,
    provider: &DelegatedProvider<'_>,
    command: &str,
) -> Result<Recovery, CliError> {
    // The claim's component/package pair must not collide with an existing
    // record of a different owner.
    if let Some(owner) = store_claim_owner(store, &pending.component, &pending.package)
        && owner.name != pending.component
    {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "pending RPM install for component '{}' (package '{}') conflicts with existing state component '{}'; refusing to overwrite either owner",
                pending.component, pending.package, owner.name
            ),
        });
    }

    match provider
        .observe(&pending.package, now)
        .map_err(|err| probe_error(err, command, &pending.component))?
    {
        NativeProbe::Present { observation, .. } => {
            let operation_id = pending.transaction.operation_id.clone();
            let started_at = pending.transaction.started_at.clone();
            commit_recovered_managed(
                store,
                state_path,
                &pending.component,
                &pending.package,
                &observation,
                scope,
                &started_at,
                &operation_id,
                command,
            )?;
            let mut warnings = Vec::new();
            if let Err(err) = pending
                .mark_install_done(command)
                .and_then(|()| pending.mark_state_done(command))
                .and_then(|()| pending.finish_ok(command))
            {
                warnings.push(format!(
                    "recovered state was saved, but the RPM recovery journal could not be finalised: {}",
                    err.reason()
                ));
            }
            warnings.extend(snapshot_datadir_contract(
                layout,
                &pending.component,
                command,
                ctx.packaged_data_probe(),
            ));
            render_warnings(&warnings, &Palette::new(ctx.no_color));
            append_repair_log(
                layout,
                ctx,
                &pending.component,
                command,
                &operation_id,
                &started_at,
                LogStatus::Ok,
                format!(
                    "recovered pending RPM package {} ({}) as rpm-managed for component {}",
                    pending.package,
                    observation
                        .evr
                        .clone()
                        .unwrap_or_else(|| observation.version.clone()),
                    pending.component
                ),
            );
            Ok(Recovery::Recovered(Box::new(application::applied(
                command,
                application::RepairSubject {
                    component: pending.component.clone(),
                    package: Some(pending.package.clone()),
                    from_version: None,
                    to_version: Some(
                        observation
                            .evr
                            .clone()
                            .unwrap_or_else(|| observation.version.clone()),
                    ),
                },
                application::RepairAction::RecoveredPendingInstall,
                Vec::new(),
                operation_id,
                Vec::new(),
                None,
            ))))
        }
        NativeProbe::MultipleVersions { .. } => Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "pending RPM package '{}' has multiple installed versions; cannot recover the install for '{}' unambiguously — resolve the duplicate install and re-run `anolisa repair {}`",
                pending.package, pending.component, pending.component
            ),
        }),
        NativeProbe::Absent | NativeProbe::NotProbed => {
            let reason = format!("RPM package '{}' is not installed", pending.package);
            pending.finish_failed(pending.state_step, &reason, command)?;
            Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "pending RPM install for component '{}' was terminated because package '{}' is not installed; its journal is now Failed and no longer participates in recovery, and installed.toml was left unchanged — retry `anolisa install {}`",
                    pending.component, pending.package, pending.component
                ),
            })
        }
    }
}

/// Apply the record transition declared before the delegated side effect.
#[expect(clippy::too_many_arguments)]
fn commit_recovered_delegated(
    store: &mut StateStore,
    state_path: &Path,
    component: &str,
    package: Option<&str>,
    observation: Option<&Observation>,
    scope: InstallationScope,
    started_at: &str,
    operation_id: &str,
    pm: NativePm,
    action: DelegatedRecordAction,
    command: &str,
) -> Result<(), CliError> {
    let write = match action {
        DelegatedRecordAction::WriteManaged => Some(RecordWrite::DelegatedManaged),
        DelegatedRecordAction::WriteAdopted => Some(RecordWrite::DelegatedAdopted),
        DelegatedRecordAction::WriteObserved => Some(RecordWrite::DelegatedObserved),
        DelegatedRecordAction::Refresh => Some(RecordWrite::RefreshObservation),
        DelegatedRecordAction::Drop => None,
    };
    if write.is_some() && package.is_none() {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "cannot apply recovered {} for '{component}' without a package identity",
                delegated_record_action_label(action)
            ),
        });
    }

    if !store
        .operations
        .iter()
        .any(|operation| operation.id == operation_id)
    {
        store.operations.push(OperationRecord {
            id: operation_id.to_string(),
            command: command.to_string(),
            status: "ok".to_string(),
            started_at: started_at.to_string(),
            finished_at: Some(now_iso8601()),
            parent_operation_id: None,
        });
    }
    let context = RecordContext {
        kind: ObjectKind::Component,
        name: component.to_string(),
        scope,
        now: started_at.to_string(),
        operation_id: Some(operation_id.to_string()),
        delegated: package.map(|package| DelegatedIdentity {
            pm,
            package: package.to_string(),
        }),
        owned_artifact: None,
    };
    let result = {
        let mut sink = StoreRecordSink::new(store, state_path, context);
        match write {
            Some(write) => sink.write_record(write, observation),
            None => sink.drop_record(),
        }
    };
    result.map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "failed to apply recovered {} for '{component}': {err}",
            delegated_record_action_label(action)
        ),
    })
}

/// Commit a recovered legacy delegated install as managed.
#[expect(clippy::too_many_arguments)]
fn commit_recovered_managed(
    store: &mut StateStore,
    state_path: &Path,
    component: &str,
    package: &str,
    observation: &Observation,
    scope: InstallationScope,
    started_at: &str,
    operation_id: &str,
    command: &str,
) -> Result<(), CliError> {
    commit_recovered_delegated(
        store,
        state_path,
        component,
        Some(package),
        Some(observation),
        scope,
        started_at,
        operation_id,
        NativePm::Rpm,
        DelegatedRecordAction::WriteManaged,
        command,
    )
}

/// The record of a different owner that already claims this component name
/// or package identity, if any.
fn store_claim_owner<'a>(
    store: &'a StateStore,
    component: &str,
    package: &str,
) -> Option<&'a Installation> {
    store.installations.iter().find(|installation| {
        installation.kind == ObjectKind::Component
            && (installation.name == component
                || installation.name == package
                || matches!(
                    &installation.binding,
                    ProviderBinding::Delegated { package: recorded, .. }
                        if recorded.resolved_name() == Some(package)
                ))
    })
}

/// The quarantined record for `target`, if any.
fn quarantined_record<'a>(store: &'a StateStore, target: &str) -> Option<&'a QuarantinedObject> {
    store
        .quarantined
        .iter()
        .find(|q| q.record.kind == ObjectKind::Component && q.record.name == target)
}

/// Whether the store, as re-read under the install lock, still authorizes
/// the planned delegated repair. A quarantine restore (R5) needs the
/// quarantined record to survive unclaimed; a refresh/reinstall (R3/R4)
/// needs the record to still be delegated at the same package identity (an
/// unresolved identity is the legacy backfill the refresh pins), and a plan
/// that transacts additionally needs management consent.
fn delegated_repair_authorized(
    store: &StateStore,
    target: &str,
    package: &str,
    steps: &[Step],
) -> bool {
    let restores_quarantined = steps
        .iter()
        .any(|s| matches!(s, Step::WriteRecord(RecordWrite::DelegatedObserved)));
    if restores_quarantined {
        return store.find(ObjectKind::Component, target).is_none()
            && quarantined_record(store, target).is_some();
    }
    let needs_management = steps
        .iter()
        .any(|s| matches!(s, Step::NativeTransaction { .. }));
    match store
        .find(ObjectKind::Component, target)
        .map(|r| &r.binding)
    {
        Some(ProviderBinding::Delegated {
            relation,
            package: recorded,
            ..
        }) => {
            let identity_ok = match recorded.resolved_name() {
                Some(name) => name == package,
                None => true,
            };
            let relation_ok =
                !needs_management || matches!(relation, ManagementRelation::Managed { .. });
            identity_ok && relation_ok
        }
        _ => false,
    }
}

/// Resolve the RPM package name for a legacy delegated record that carries
/// no resolved identity, via the shared adopt candidate chain. repo.toml and
/// the component index are best-effort inputs: a load failure just drops
/// that precedence tier.
fn resolve_repair_package(
    component: &str,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    command: &str,
) -> Result<String, CliError> {
    let layout = common::resolve_layout(ctx);
    let repo_config =
        common::load_repo_config(ctx, &layout, COMMAND, RepoPersistPolicy::BestEffort).ok();
    let rpm_backend = repo_config.as_ref().and_then(|c| c.backends.get("rpm"));
    let env = anolisa_env::EnvService::detect();
    let component_index = repo_config
        .as_ref()
        .and_then(|cfg| load_optional_component_index(&layout, &env, cfg));

    let candidates = match rpm_package_candidates_with_index(
        None,
        rpm_backend,
        component_index.as_ref(),
        query,
        component,
    ) {
        Ok(candidates) => candidates,
        Err(PackageQueryError::CommandMissing { command: bin }) => {
            return Err(rpm_tooling_missing_error(command, &bin, component));
        }
        Err(err) => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!("rpm query failed: {err}"),
            });
        }
    };
    if candidates.len() >= 2 {
        return Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "multiple candidate RPMs for component '{component}': {}; cannot repair unambiguously — reinstall to pin one, or fix the component index / package metadata",
                candidates
                    .iter()
                    .map(|target| target.package.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    candidates
        .into_iter()
        .next()
        .map(|target| target.package)
        .ok_or_else(|| CliError::Runtime {
            command: command.to_string(),
            reason: format!("could not resolve an RPM package name for component '{component}'"),
        })
}

/// Human-facing name of what a repair plan does, for previews and payloads.
fn plan_action(steps: &[Step]) -> application::RepairAction {
    if matches!(steps, [Step::RecoverJournal]) {
        return application::RepairAction::RecoverJournal;
    }
    if matches!(steps, [Step::WriteRecord(RecordWrite::Owned)]) {
        return application::RepairAction::RestoreOwnedRecord;
    }
    if steps.iter().any(|s| matches!(s, Step::PlaceFiles)) {
        return application::RepairAction::ReplayOwnedFiles;
    }
    if steps
        .iter()
        .any(|s| matches!(s, Step::NativeTransaction { .. }))
    {
        return application::RepairAction::ReinstallPackage;
    }
    if steps
        .iter()
        .any(|s| matches!(s, Step::WriteRecord(RecordWrite::DelegatedObserved)))
    {
        return application::RepairAction::RestoreObservedRecord;
    }
    application::RepairAction::RefreshObservation
}

/// Map a planning refusal to an actionable CLI error. The planner names the
/// way out; this mapping only renders it.
fn plan_error_to_cli(err: PlanError, target: &str, command: &str) -> CliError {
    let command = command.to_string();
    match err {
        PlanError::NotInstalled => CliError::NotInstalled {
            command,
            reason: format!(
                "component '{target}' is not installed — nothing to repair (run `anolisa status` to see what is installed)"
            ),
        },
        PlanError::RecordUnrecoverable => CliError::InvalidArgument {
            command,
            reason: format!(
                "the quarantined record for '{target}' matches neither the native package database nor its recorded files; run `anolisa forget {target}` to drop it, then reinstall if needed"
            ),
        },
        PlanError::TrackedButAbsent => CliError::InvalidArgument {
            command,
            reason: format!(
                "component '{target}' is tracked but its package is gone and ANOLISA has no authority to reinstall it; run `anolisa forget {target}` to drop the record, or `anolisa install {target}` to install it fresh"
            ),
        },
        PlanError::AmbiguousPackage => CliError::Runtime {
            command,
            reason: format!(
                "the package backing '{target}' has multiple installed versions; cannot reconcile unambiguously — resolve the duplicate install (e.g. `dnf remove` the stale version) and re-run `anolisa repair {target}`"
            ),
        },
        PlanError::PackageUnresolved => CliError::Runtime {
            command,
            reason: format!(
                "the record for '{target}' has no resolved package name and none could be derived; reinstall to pin one, or run `anolisa forget {target}`"
            ),
        },
        other => CliError::InvalidArgument {
            command,
            reason: format!("cannot repair '{target}': {other:?}"),
        },
    }
}

/// Map an owned-executor failure to a CLI error that reports honestly what
/// happened to the host: cleanly restored, partially restored, or untouched.
fn owned_error_to_cli(
    err: OwnedExecutionError,
    target: &str,
    scope: InstallationScope,
    command: &str,
) -> CliError {
    let repair = common::scoped_component_command(scope, "repair", target);
    let reason = match err {
        OwnedExecutionError::StepFailed {
            step,
            source,
            rolled_back,
            rollback_warnings,
            ..
        } => {
            let at = step_label(&step);
            if !rolled_back {
                format!("repair of '{target}' failed at '{at}': {source}; the host was not changed")
            } else if rollback_warnings.is_empty() {
                format!(
                    "repair of '{target}' failed at '{at}': {source}; the previous files were restored"
                )
            } else {
                format!(
                    "repair of '{target}' failed at '{at}': {source}; restoring the previous files reported problems ({}) — run `{repair}` again",
                    rollback_warnings.join("; ")
                )
            }
        }
        OwnedExecutionError::RecoveryUncertain { detail, .. } => {
            format!("repair of '{target}' failed: {detail}; run `{repair}` again")
        }
        other => format!("repair of '{target}' failed: {other}"),
    };
    CliError::Runtime {
        command: command.to_string(),
        reason,
    }
}

/// Human-facing label for a plan step (preview rendering).
fn step_label(step: &Step) -> String {
    match step {
        Step::NativeTransaction {
            action, packages, ..
        } => format!("dnf {} {}", action.verb(), packages.join(" ")),
        Step::Observe { packages } => format!("observe {}", packages.join(" ")),
        Step::WriteRecord(write) => format!("record: {}", write.label()),
        Step::DropRecord => "record: drop".to_string(),
        Step::RecoverJournal => "recover pending journal".to_string(),
        Step::DownloadVerify => "download and verify artifact".to_string(),
        Step::BackupFiles => "back up current files".to_string(),
        Step::PlaceFiles => "place files".to_string(),
        Step::SetCapabilities => "apply file capabilities".to_string(),
        Step::RestartServices => "restart services".to_string(),
        Step::RemoveOwnedFiles => "remove owned files".to_string(),
        other => format!("{other:?}"),
    }
}

/// Actionable "rpm/dnf tooling missing" error: a delegated or
/// package-quarantined component cannot be reconciled without the native
/// package manager.
fn rpm_tooling_missing_error(command: &str, bin: &str, target: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "cannot repair '{target}': {bin} not found on PATH — install rpm/dnf and retry"
        ),
    }
}

/// Map a probe failure during journal recovery; the journal stays pending.
fn probe_error(err: ProviderError, command: &str, target: &str) -> CliError {
    match err {
        ProviderError::Query(PackageQueryError::CommandMissing { command: bin }) => {
            rpm_tooling_missing_error(command, &bin, target)
        }
        other => CliError::Runtime {
            command: command.to_string(),
            reason: format!("native probe failed: {other}; the recovery journal was preserved"),
        },
    }
}

/// A journal that could not be settled: report where it lives so the
/// operator can inspect it — it may remain live (InFlight or Partial).
fn journal_finish_error(
    journal: &Transaction,
    command: &str,
    err: anolisa_core::transaction::TransactionError,
) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "recovery journal operation '{}' at '{}' could not be updated: {err}; it may remain live (InFlight or Partial)",
            journal.operation_id,
            journal.journal_path.display()
        ),
    }
}

/// Best-effort central-log record for a committed repair.
#[expect(clippy::too_many_arguments)]
fn append_repair_log(
    layout: &FsLayout,
    ctx: &CliContext,
    component: &str,
    command: &str,
    operation_id: &str,
    started_at: &str,
    status: LogStatus,
    message: String,
) {
    let log = CentralLog::open(layout.central_log.clone());
    let record = LogRecord {
        kind: LogKind::Operation,
        operation_id: Some(operation_id.to_string()),
        command: command.to_string(),
        source: "anolisa-cli".to_string(),
        component: Some(component.to_string()),
        severity: if matches!(status, LogStatus::Ok) {
            Severity::Info
        } else {
            Severity::Warn
        },
        message,
        actor: "cli".to_string(),
        install_mode: Some(ctx.install_mode.as_str().to_string()),
        started_at: started_at.to_string(),
        finished_at: Some(now_iso8601()),
        status: Some(status),
        objects: vec![component.to_string()],
        backup_ids: Vec::new(),
        warnings: Vec::new(),
        details: serde_json::Value::Null,
    };
    if let Err(err) = log.append(&record) {
        eprintln!("warning: failed to write central log: {err}");
    }
}

/// Render the repair result (or its dry-run preview).
fn render_application_outcome(
    ctx: &CliContext,
    application_outcome: application::ApplicationOutcome,
) -> Result<(), CliError> {
    let payload = match application_outcome {
        application::ApplicationOutcome::NoOp {
            subject,
            manifest_reconciliation,
        } => RepairResultPayload {
            component: subject.component,
            package: subject.package,
            action: application::RepairAction::NothingToRepair
                .as_str()
                .to_string(),
            from_version: subject.from_version,
            to_version: subject.to_version,
            dry_run: ctx.dry_run,
            plan: Vec::new(),
            operation_id: None,
            manifest_reconciliation,
        },
        application::ApplicationOutcome::Preview {
            subject,
            action,
            steps,
            manifest_reconciliation,
        } => RepairResultPayload {
            component: subject.component,
            package: subject.package,
            action: action.as_str().to_string(),
            from_version: subject.from_version,
            to_version: subject.to_version,
            dry_run: true,
            plan: steps.iter().map(step_label).collect(),
            operation_id: None,
            manifest_reconciliation,
        },
        application::ApplicationOutcome::Applied {
            command,
            subject,
            action,
            steps,
            outcome,
            manifest_reconciliation,
        } => {
            for warning in outcome.warnings() {
                eprintln!("warning: {warning}");
            }
            match outcome.status() {
                anolisa_core::execution::CommandOutcomeStatus::Completed => {}
                anolisa_core::execution::CommandOutcomeStatus::Partial { reason }
                | anolisa_core::execution::CommandOutcomeStatus::Failed { reason } => {
                    return Err(CliError::Runtime {
                        command,
                        reason: reason.clone(),
                    });
                }
            }
            RepairResultPayload {
                component: subject.component,
                package: subject.package,
                action: action.as_str().to_string(),
                from_version: subject.from_version,
                to_version: subject.to_version,
                dry_run: false,
                plan: steps.iter().map(step_label).collect(),
                operation_id: outcome.operation_id().map(str::to_string),
                manifest_reconciliation,
            }
        }
    };
    render_result(ctx, &payload)
}

/// Render the repair result (or its dry-run preview).
fn render_result(ctx: &CliContext, payload: &RepairResultPayload) -> Result<(), CliError> {
    if ctx.json {
        return render_json(COMMAND, payload);
    }
    if ctx.quiet {
        return Ok(());
    }
    let color = Palette::new(ctx.no_color);
    if payload.dry_run {
        println!(
            "{} {} {}",
            color.command("repair"),
            payload.component,
            color.muted("(dry-run — nothing repaired)"),
        );
        println!("{} {}", color.label("would:"), payload.action);
        for label in &payload.plan {
            println!("  - {label}");
        }
        if let Some(reason) = payload.manifest_reconciliation {
            println!(
                "{} would refresh the component manifest snapshot ({reason})",
                color.label("also:"),
            );
        }
        return Ok(());
    }
    if payload.action == "nothing-to-repair" {
        if payload.manifest_reconciliation.is_some() {
            println!(
                "{} {} record is healthy; refreshed the component manifest snapshot",
                color.ok("✓"),
                payload.component
            );
        } else {
            println!(
                "{} {} has nothing to repair",
                color.ok("✓"),
                payload.component
            );
        }
        return Ok(());
    }
    match (&payload.from_version, &payload.to_version) {
        (Some(from), Some(to)) if from != to => println!(
            "{} {} ({}) {} → {}",
            color.ok("✓ repaired"),
            payload.component,
            payload.action,
            from,
            to,
        ),
        (_, Some(to)) => println!(
            "{} {} ({}) {}",
            color.ok("✓ repaired"),
            payload.component,
            payload.action,
            to,
        ),
        _ => println!(
            "{} {} ({})",
            color.ok("✓ repaired"),
            payload.component,
            payload.action,
        ),
    }
    if let Some(id) = &payload.operation_id {
        println!("{} {}", color.label("operation_id:"), color.id(id));
    }
    Ok(())
}

/// Render any accumulated warnings to stderr, one per line.
fn render_warnings(warnings: &[String], color: &Palette) {
    for w in warnings {
        eprintln!("{} {w}", color.warn("warning:"));
    }
}

/// RFC3339 UTC timestamp, seconds precision (matches the install path).
fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::path::PathBuf;

    use anolisa_core::domain::LifecycleStatus;
    use anolisa_core::executor::{PHASE_NATIVE_TXN, PHASE_OBSERVE};
    use anolisa_core::owned_executor::PHASE_FILES;
    use anolisa_core::state::{
        FileOwner, InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectStatus,
        OwnedFile, OwnedFileKind, Ownership, RpmMetadata,
    };
    use anolisa_core::transaction::{DelegatedPinnedArtifact, TransactionStep};
    use anolisa_platform::pkg_query::{PackageInfo, PackageVersion};

    use crate::context::InstallMode;

    fn no_op_application_outcome() -> application::ApplicationOutcome {
        application::ApplicationOutcome::NoOp {
            subject: application::RepairSubject {
                component: "cosh".to_string(),
                package: None,
                from_version: None,
                to_version: None,
            },
            manifest_reconciliation: None,
        }
    }

    #[test]
    fn failed_application_outcome_uses_its_terminal_reason() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let outcome = application::ApplicationOutcome::Applied {
            command: "repair cosh".to_string(),
            subject: application::RepairSubject {
                component: "cosh".to_string(),
                package: Some("cosh".to_string()),
                from_version: Some("2.6.0".to_string()),
                to_version: Some("2.7.0".to_string()),
            },
            action: application::RepairAction::RefreshObservation,
            steps: Vec::new(),
            outcome: anolisa_core::execution::CommandOutcome::new(
                anolisa_core::execution::CommandOutcomeStatus::Failed {
                    reason: "native package transaction failed".to_string(),
                },
                None,
                Vec::new(),
                vec!["non-terminal diagnostic".to_string()],
            ),
            manifest_reconciliation: None,
        };

        let err = render_application_outcome(&ctx, outcome)
            .expect_err("a failed outcome must use the error path");

        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.reason(), "native package transaction failed");
    }

    #[test]
    fn locked_pending_reenters_recovery_after_releasing_the_lock() {
        let retries = Cell::new(0);

        continue_after_locked_repair(
            LockedRepairExecution::RecoveryAppeared,
            true,
            "cosh",
            "repair cosh",
            || {
                retries.set(retries.get() + 1);
                Ok(no_op_application_outcome())
            },
        )
        .expect("new recovery chain should be replanned once");

        assert_eq!(retries.get(), 1);
    }

    #[test]
    fn second_locked_pending_requires_another_repair_invocation() {
        let retries = Cell::new(0);

        let err = continue_after_locked_repair(
            LockedRepairExecution::RecoveryAppeared,
            false,
            "cosh",
            "repair cosh",
            || {
                retries.set(retries.get() + 1);
                Ok(no_op_application_outcome())
            },
        )
        .expect_err("one invocation must not consume two recovery chains");

        assert_eq!(retries.get(), 0);
        assert!(err.reason().contains("repair cosh"));
    }

    /// Configurable in-memory rpm backend. Repair may query (R3/R5), install
    /// (R4), or neither (owned paths); every other transaction verb is a
    /// routing bug.
    struct FakeRpm {
        package: String,
        installed: RefCell<Option<PackageInfo>>,
        /// PackageInfo the rpmdb holds after a successful `dnf install`.
        installs_to: Option<PackageInfo>,
        install_succeeds: bool,
        multi_version: bool,
        command_missing: bool,
        /// Component name this package declares an `anolisa-component(...)`
        /// provide for (the legacy package-name resolution tier).
        provides_component: Option<String>,
        install_calls: Cell<usize>,
    }

    impl FakeRpm {
        fn new(package: &str, installed: Option<PackageInfo>) -> Self {
            Self {
                package: package.to_string(),
                installed: RefCell::new(installed),
                installs_to: None,
                install_succeeds: true,
                multi_version: false,
                command_missing: false,
                provides_component: None,
                install_calls: Cell::new(0),
            }
        }
        fn providing_component(mut self, component: &str) -> Self {
            self.provides_component = Some(component.to_string());
            self
        }
        fn installing_to(mut self, info: PackageInfo) -> Self {
            self.installs_to = Some(info);
            self
        }
        fn multi_version(mut self) -> Self {
            self.multi_version = true;
            self
        }
        fn command_missing(mut self) -> Self {
            self.command_missing = true;
            self
        }
    }

    impl PackageQuery for FakeRpm {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            if self.command_missing {
                return Err(PackageQueryError::CommandMissing {
                    command: "rpm".to_string(),
                });
            }
            if package != self.package {
                return Ok(None);
            }
            if self.multi_version {
                return Err(PackageQueryError::UnexpectedOutput {
                    command: "rpm".to_string(),
                    detail: "2 installed versions".to_string(),
                });
            }
            Ok(self.installed.borrow().clone())
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            if self.command_missing {
                return Err(PackageQueryError::CommandMissing {
                    command: "dnf".to_string(),
                });
            }
            Ok(Vec::new())
        }

        fn what_provides_installed(
            &self,
            capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            if self.command_missing {
                return Err(PackageQueryError::CommandMissing {
                    command: "rpm".to_string(),
                });
            }
            match &self.provides_component {
                Some(component)
                    if capability == format!("anolisa-component({component})")
                        && self.installed.borrow().is_some() =>
                {
                    Ok(vec![self.package.clone()])
                }
                _ => Ok(Vec::new()),
            }
        }
    }

    impl PackageTransaction for FakeRpm {
        fn check_install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("this path must not preflight an install")
        }

        fn install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            let &[package] = packages else {
                panic!("expected exactly one package, got {packages:?}");
            };
            self.install_calls.set(self.install_calls.get() + 1);
            assert_eq!(package, self.package, "install targeted the wrong package");
            if !self.install_succeeds {
                return Err(PackageTransactionError::TransactionFailed {
                    command: "dnf".to_string(),
                    operation: "install".to_string(),
                    code: Some(1),
                    stderr: "repo unreachable".to_string(),
                });
            }
            *self.installed.borrow_mut() = self.installs_to.clone();
            Ok(())
        }
        fn update(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("repair must not delegate a dnf update");
        }
        fn reinstall(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("repair must not delegate a dnf reinstall");
        }
        fn remove(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("repair must not delegate a dnf remove");
        }
    }

    fn pkg_info(name: &str, version: &str, release: Option<&str>, arch: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: PackageVersion {
                epoch: None,
                version: version.to_string(),
                release: release.map(str::to_string),
            },
            arch: arch.to_string(),
            origin: None,
        }
    }

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

    /// Legacy (v4) RPM-backed component object; the store migrates it on
    /// load, so these tests double as migration coverage.
    fn rpm_object(
        component: &str,
        package: &str,
        evr: &str,
        ownership: Ownership,
        adopted: bool,
    ) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: component.to_string(),
            version: evr.to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some("rpm".to_string()),
            ownership: Some(ownership),
            rpm_metadata: Some(RpmMetadata {
                package_name: package.to_string(),
                evr: Some(evr.to_string()),
                arch: Some("x86_64".to_string()),
                source_repo: Some("@System".to_string()),
            }),
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: Some("op-prior".to_string()),
            managed: !matches!(ownership, Ownership::RpmObserved),
            adopted,
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

    /// Legacy (v4) raw-managed component object (migrates to Owned).
    fn raw_object(component: &str, version: &str, files: Vec<OwnedFile>) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: component.to_string(),
            version: version.to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: Some("https://example.com/x".to_string()),
            raw_package: None,
            install_backend: Some("raw".to_string()),
            ownership: Some(Ownership::RawManaged),
            rpm_metadata: None,
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: None,
            managed: true,
            adopted: false,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files,
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        }
    }

    /// Legacy object with an unknown backend: quarantined on migration
    /// (rule R4f), the raw material for the R5/R6 exits.
    fn unknown_backend_object(component: &str, files: Vec<OwnedFile>) -> InstalledObject {
        InstalledObject {
            install_backend: Some("flatpak".to_string()),
            ownership: None,
            distribution_source: None,
            managed: false,
            ..raw_object(component, "0.9.0", files)
        }
    }

    fn anolisa_file(path: PathBuf) -> OwnedFile {
        OwnedFile {
            path,
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }
    }

    /// Seed `installed.toml` with v4 objects for `ctx`'s scope.
    fn seed(ctx: &CliContext, objects: Vec<InstalledObject>) {
        let layout = common::resolve_layout(ctx);
        fs::create_dir_all(&layout.state_dir).expect("mkdir state");
        let mut state = InstalledState {
            install_mode: match ctx.install_mode {
                InstallMode::System => StateInstallMode::System,
                InstallMode::User => StateInstallMode::User,
            },
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

    /// Load the (migrated) v5 store.
    fn load_store(ctx: &CliContext) -> StateStore {
        let layout = common::resolve_layout(ctx);
        StateStore::load(&layout.state_dir.join("installed.toml"), 0).expect("load state")
    }

    fn find_component(ctx: &CliContext, name: &str) -> Installation {
        load_store(ctx)
            .find(ObjectKind::Component, name)
            .cloned()
            .expect("component record present")
    }

    fn observed_evr(record: &Installation) -> Option<String> {
        match &record.binding {
            ProviderBinding::Delegated { last_observed, .. } => last_observed
                .as_ref()
                .map(|o| o.evr.clone().unwrap_or_else(|| o.version.clone())),
            _ => None,
        }
    }

    // ── R7 / plan refusals ────────────────────────────────────────────

    #[test]
    fn absent_component_is_not_installed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let fake = FakeRpm::new("cosh", None);

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false).unwrap_err();

        assert_eq!(err.code(), "NOT_INSTALLED");
        assert!(err.reason().contains("not installed"), "got: {err}");
    }

    #[test]
    fn adopted_component_with_absent_package_points_at_forget() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "cosh",
                "2.6.0-1.al4",
                Ownership::RpmObserved,
                true,
            )],
        );
        let fake = FakeRpm::new("cosh", None);

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, true).unwrap_err();

        assert!(err.reason().contains("forget"), "got: {err}");
        assert_eq!(fake.install_calls.get(), 0);
    }

    #[test]
    fn multi_version_rpmdb_is_ambiguous() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "cosh",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let fake = FakeRpm::new(
            "cosh",
            Some(pkg_info("cosh", "2.6.0", Some("1.al4"), "x86_64")),
        )
        .multi_version();

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, true).unwrap_err();

        assert!(
            err.reason().contains("multiple installed versions"),
            "got: {err}"
        );
    }

    #[test]
    fn missing_tooling_is_fatal_for_a_delegated_record() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "cosh",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let fake = FakeRpm::new("cosh", None).command_missing();

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, true).unwrap_err();

        assert!(err.reason().contains("not found on PATH"), "got: {err}");
    }

    // ── R3: refresh a delegated observation ──────────────────────────

    #[test]
    fn managed_drift_refreshes_the_observation() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "cosh",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        // dnf update ran outside ANOLISA: rpmdb is ahead of the record.
        let fake = FakeRpm::new(
            "cosh",
            Some(pkg_info("cosh", "3.0.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("cosh", &ctx, &fake, &fake, false).expect("repair ok");

        let record = find_component(&ctx, "cosh");
        assert_eq!(observed_evr(&record).as_deref(), Some("3.0.0-1.al4"));
        assert!(matches!(
            &record.binding,
            ProviderBinding::Delegated {
                relation: ManagementRelation::Managed { .. },
                ..
            }
        ));
        let store = load_store(&ctx);
        assert_eq!(store.operations.len(), 1);
        assert!(store.operations[0].command.starts_with("repair"));
        assert_eq!(fake.install_calls.get(), 0, "R3 never transacts");
    }

    #[test]
    fn healthy_record_with_stale_manifest_snapshot_reconciles_it() {
        use anolisa_core::adapter::contract::read_snapshot_provenance;

        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let component = "manifest-sync-test";
        let package = "manifest-sync-test-rpm";
        seed(
            &ctx,
            vec![rpm_object(
                component,
                package,
                "2.0.0-1.al8",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let package_datadir = layout.package_datadir().expect("package datadir");
        let source = FsLayout::component_contract_path(&package_datadir, component);
        fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
        fs::write(&source, "framework = \"new\"\n").expect("write source contract");
        let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
        fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("mkdir snapshot");
        fs::write(&snapshot, "framework = \"old\"\n").expect("write stale snapshot");
        // rpmdb matches the record: the R3 refresh is a state no-change, but
        // the drifted snapshot still has to be reconciled.
        let fake = FakeRpm::new(
            package,
            Some(pkg_info(package, "2.0.0", Some("1.al8"), "x86_64")),
        );

        repair_with_deps(component, &ctx, &fake, &fake, false).expect("repair manifest drift");

        assert_eq!(
            fs::read_to_string(&snapshot).expect("read refreshed snapshot"),
            "framework = \"new\"\n"
        );
        let provenance = read_snapshot_provenance(&snapshot).expect("refreshed provenance");
        assert_eq!(provenance.source_path, source);
        let store = load_store(&ctx);
        assert_eq!(store.operations.len(), 1);
        assert_eq!(store.operations[0].status, "ok");
        assert_eq!(fake.install_calls.get(), 0, "manifest sync never transacts");
    }

    #[test]
    fn manifest_refresh_failure_keeps_the_operation_partial() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let component = "manifest-write-failure";
        let package = "manifest-write-failure-rpm";
        seed(
            &ctx,
            vec![rpm_object(
                component,
                package,
                "2.0.0-1.al8",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let package_datadir = layout.package_datadir().expect("package datadir");
        let source = FsLayout::component_contract_path(&package_datadir, component);
        fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
        fs::write(&source, "framework = \"new\"\n").expect("write source contract");
        // A directory where the snapshot file belongs blocks the refresh.
        let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
        fs::create_dir_all(&snapshot).expect("create blocking snapshot directory");
        let marker = snapshot.join("keep");
        fs::write(&marker, "unchanged").expect("write snapshot marker");
        let fake = FakeRpm::new(
            package,
            Some(pkg_info(package, "2.0.0", Some("1.al8"), "x86_64")),
        );

        let err = repair_with_deps(component, &ctx, &fake, &fake, false)
            .expect_err("manifest refresh must fail");

        let CliError::Runtime { reason, .. } = err else {
            panic!("expected runtime error");
        };
        assert!(
            reason.contains("manifest reconciliation did not complete"),
            "{reason}"
        );
        assert_eq!(
            fs::read_to_string(&marker).expect("read unchanged marker"),
            "unchanged"
        );
        let store = load_store(&ctx);
        assert_eq!(store.operations.len(), 1);
        assert_eq!(
            store.operations[0].status, "partial",
            "a failed refresh must leave the conservative durable status"
        );
    }

    #[test]
    fn dry_run_previews_manifest_reconciliation_without_writes() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        let component = "manifest-dry-run";
        let package = "manifest-dry-run-rpm";
        seed(
            &ctx,
            vec![rpm_object(
                component,
                package,
                "2.0.0-1.al8",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let package_datadir = layout.package_datadir().expect("package datadir");
        let source = FsLayout::component_contract_path(&package_datadir, component);
        fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
        fs::write(&source, "framework = \"new\"\n").expect("write source contract");
        let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
        fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("mkdir snapshot");
        fs::write(&snapshot, "framework = \"old\"\n").expect("write stale snapshot");
        let fake = FakeRpm::new(
            package,
            Some(pkg_info(package, "2.0.0", Some("1.al8"), "x86_64")),
        );

        repair_with_deps(component, &ctx, &fake, &fake, false).expect("dry-run previews");

        assert_eq!(
            fs::read_to_string(&snapshot).expect("read unchanged snapshot"),
            "framework = \"old\"\n",
            "dry-run must not refresh the snapshot"
        );
        let store = load_store(&ctx);
        assert!(store.operations.is_empty(), "dry-run records no operation");
    }

    #[test]
    fn manifest_drift_is_judged_against_the_package_datadir_only() {
        use anolisa_core::adapter::contract::{
            ContractProvenance, ContractSourceKind, write_snapshot_provenance,
        };

        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let component = "manifest-root-priority";
        let package = "manifest-root-priority-rpm";
        seed(
            &ctx,
            vec![rpm_object(
                component,
                package,
                "2.0.0-1.al8",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let package_datadir = layout.package_datadir().expect("package datadir");
        let source = FsLayout::component_contract_path(&package_datadir, component);
        fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
        fs::write(&source, "framework = \"same\"\n").expect("write package contract");
        // A stale local copy in the plain datadir must not count as drift.
        let local = FsLayout::component_contract_path(&layout.datadir, component);
        fs::create_dir_all(local.parent().expect("local parent")).expect("mkdir local");
        fs::write(&local, "framework = \"stale-local\"\n").expect("write local copy");
        let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
        fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("mkdir snapshot");
        fs::write(&snapshot, "framework = \"same\"\n").expect("write matching snapshot");
        write_snapshot_provenance(
            &snapshot,
            &ContractProvenance {
                schema_version: 1,
                source_kind: ContractSourceKind::Datadir,
                source_path: source.clone(),
                datadir_root: package_datadir.clone(),
            },
        )
        .expect("write matching provenance");
        let fake = FakeRpm::new(
            package,
            Some(pkg_info(package, "2.0.0", Some("1.al8"), "x86_64")),
        );

        repair_with_deps(component, &ctx, &fake, &fake, false).expect("repair ok");

        // A misjudged drift against the local copy would overwrite the
        // snapshot with the stale local content; the package-datadir verdict
        // leaves it untouched.
        assert_eq!(
            fs::read_to_string(&snapshot).expect("read unchanged snapshot"),
            "framework = \"same\"\n"
        );
        let store = load_store(&ctx);
        assert_eq!(store.operations.len(), 1, "idempotent R3 refresh");
        assert_eq!(
            store.operations[0].status, "ok",
            "no manifest drift means no partial phase"
        );
    }

    #[test]
    fn local_manifest_is_ignored_when_the_package_contract_is_missing() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let component = "manifest-missing-package-contract";
        let package = "manifest-missing-package-contract-rpm";
        seed(
            &ctx,
            vec![rpm_object(
                component,
                package,
                "2.0.0-1.al8",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        // Only a local datadir copy exists; the package publishes no contract.
        let local = FsLayout::component_contract_path(&layout.datadir, component);
        fs::create_dir_all(local.parent().expect("local parent")).expect("mkdir local");
        fs::write(&local, "framework = \"local-only\"\n").expect("write local copy");
        let fake = FakeRpm::new(
            package,
            Some(pkg_info(package, "2.0.0", Some("1.al8"), "x86_64")),
        );

        repair_with_deps(component, &ctx, &fake, &fake, false).expect("repair ok");

        // An absent package contract is not drift: the local datadir copy
        // must be ignored, so no snapshot gets seeded and no partial phase
        // is entered.
        let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
        assert!(
            !snapshot.exists(),
            "an absent package contract must not seed a snapshot"
        );
        let store = load_store(&ctx);
        assert_eq!(store.operations.len(), 1, "idempotent R3 refresh");
        assert_eq!(store.operations[0].status, "ok");
    }

    #[test]
    fn repair_after_external_update_drops_the_previous_repository() {
        for (version, arch, expected) in [
            ("2.6.0", "x86_64", Some("prior-repo")),
            ("2.7.0", "x86_64", None),
            ("2.6.0", "aarch64", None),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
            let mut obj = rpm_object("cosh", "cosh", "2.6.0-1.al4", Ownership::RpmObserved, false);
            obj.rpm_metadata.as_mut().unwrap().source_repo = Some("prior-repo".into());
            seed(&ctx, vec![obj]);
            let fake = FakeRpm::new("cosh", Some(pkg_info("cosh", version, Some("1.al4"), arch)));
            repair_with_deps("cosh", &ctx, &fake, &fake, false).unwrap();
            let record = find_component(&ctx, "cosh");
            let ProviderBinding::Delegated {
                last_observed: Some(observed),
                ..
            } = record.binding
            else {
                panic!("missing observation")
            };
            assert_eq!(observed.source_repo.as_deref(), expected);
            assert_eq!(fake.install_calls.get(), 0);
        }
    }

    #[test]
    fn observed_component_refreshes_without_adoption() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let mut obj = rpm_object("cosh", "cosh", "2.6.0-1.al4", Ownership::RpmObserved, false);
        obj.managed = false;
        seed(&ctx, vec![obj]);
        let fake = FakeRpm::new(
            "cosh",
            Some(pkg_info("cosh", "2.7.0", Some("1.al4"), "x86_64")),
        );

        // No root either: an observation refresh runs no transaction.
        repair_with_deps("cosh", &ctx, &fake, &fake, false).expect("repair ok");

        let record = find_component(&ctx, "cosh");
        assert_eq!(observed_evr(&record).as_deref(), Some("2.7.0-1.al4"));
        assert!(
            matches!(
                &record.binding,
                ProviderBinding::Delegated {
                    relation: ManagementRelation::Observed,
                    ..
                }
            ),
            "repair must not upgrade the management relation"
        );
    }

    #[test]
    fn legacy_record_without_package_name_is_backfilled() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        // A legacy managed row whose rpm metadata never captured the name:
        // migrates to PackageIdentity::Unresolved.
        let mut obj = rpm_object("cosh", "", "2.6.0-1.al4", Ownership::RpmManaged, false);
        obj.rpm_metadata = Some(RpmMetadata {
            package_name: String::new(),
            evr: Some("2.6.0-1.al4".to_string()),
            arch: Some("x86_64".to_string()),
            source_repo: None,
        });
        seed(&ctx, vec![obj]);
        {
            let record = find_component(&ctx, "cosh");
            match &record.binding {
                ProviderBinding::Delegated { package, .. } => {
                    assert_eq!(package.resolved_name(), None, "seed must be unresolved");
                }
                other => panic!("expected delegated seed, got {other:?}"),
            }
        }
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.6.0", Some("1.al4"), "x86_64")),
        )
        .providing_component("cosh");

        repair_with_deps("cosh", &ctx, &fake, &fake, false).expect("repair ok");

        let record = find_component(&ctx, "cosh");
        match &record.binding {
            ProviderBinding::Delegated { package, .. } => {
                assert_eq!(
                    package.resolved_name(),
                    Some("copilot-shell"),
                    "name was backfilled"
                );
            }
            other => panic!("expected delegated record, got {other:?}"),
        }
        assert_eq!(observed_evr(&record).as_deref(), Some("2.6.0-1.al4"));
    }

    #[test]
    fn dry_run_previews_without_side_effects() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "cosh",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let before = fs::read_to_string(&state_path).expect("read state");
        let fake = FakeRpm::new(
            "cosh",
            Some(pkg_info("cosh", "3.0.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("cosh", &ctx, &fake, &fake, false).expect("dry-run ok");

        assert_eq!(
            fs::read_to_string(&state_path).expect("re-read state"),
            before,
            "dry-run must not touch state"
        );
        assert_eq!(fake.install_calls.get(), 0);
    }

    // ── R4: reinstall an externally removed managed package ──────────

    #[test]
    fn managed_absent_package_is_reinstalled_with_root() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "cosh",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let fake = FakeRpm::new("cosh", None).installing_to(pkg_info(
            "cosh",
            "2.7.0",
            Some("1.al4"),
            "x86_64",
        ));

        repair_with_deps("cosh", &ctx, &fake, &fake, true).expect("repair ok");

        assert_eq!(fake.install_calls.get(), 1);
        let record = find_component(&ctx, "cosh");
        assert_eq!(observed_evr(&record).as_deref(), Some("2.7.0-1.al4"));
        assert!(matches!(
            &record.binding,
            ProviderBinding::Delegated {
                relation: ManagementRelation::Managed { .. },
                ..
            }
        ));
    }

    #[test]
    fn managed_absent_package_requires_root() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "cosh",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let fake = FakeRpm::new("cosh", None).installing_to(pkg_info(
            "cosh",
            "2.7.0",
            Some("1.al4"),
            "x86_64",
        ));

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false).unwrap_err();

        assert!(err.reason().contains("requires root"), "got: {err}");
        assert_eq!(fake.install_calls.get(), 0);
    }

    // ── R5/R6: quarantine exits ───────────────────────────────────────

    #[test]
    fn quarantined_record_with_native_presence_restores_as_observed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, vec![unknown_backend_object("cosh", Vec::new())]);
        assert_eq!(load_store(&ctx).quarantined.len(), 1, "seed is quarantined");
        // The native db knows a package by the component's name: the native
        // authority wins the rebuild.
        let fake = FakeRpm::new(
            "cosh",
            Some(pkg_info("cosh", "2.7.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("cosh", &ctx, &fake, &fake, false).expect("repair ok");

        let record = find_component(&ctx, "cosh");
        assert!(matches!(
            &record.binding,
            ProviderBinding::Delegated {
                relation: ManagementRelation::Observed,
                ..
            }
        ));
        assert_eq!(observed_evr(&record).as_deref(), Some("2.7.0-1.al4"));
        assert!(
            load_store(&ctx).quarantined.is_empty(),
            "the restore consumes the quarantined record"
        );
    }

    #[test]
    fn quarantined_record_with_intact_files_restores_as_owned() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        let file_path = layout.bin_dir.join("legacy-tool");
        fs::write(&file_path, b"payload").expect("write file");
        seed(
            &ctx,
            vec![unknown_backend_object(
                "legacy-tool",
                vec![anolisa_file(file_path.clone())],
            )],
        );
        // Not an rpm package: the file-based exit is the only way back.
        let fake = FakeRpm::new("unrelated", None);

        repair_with_deps("legacy-tool", &ctx, &fake, &fake, false).expect("repair ok");

        let record = find_component(&ctx, "legacy-tool");
        match &record.binding {
            ProviderBinding::Owned { artifact } => {
                assert_eq!(artifact.version, "0.9.0");
                assert_eq!(artifact.files.len(), 1);
                assert_eq!(artifact.files[0].path, file_path);
            }
            other => panic!("expected owned record, got {other:?}"),
        }
        assert_eq!(record.status, LifecycleStatus::Installed);
        assert!(record.last_operation_id.is_some());
        assert!(load_store(&ctx).quarantined.is_empty());
        let store = load_store(&ctx);
        assert_eq!(store.operations.len(), 1);
    }

    #[test]
    fn quarantined_restore_survives_missing_tooling() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        let file_path = layout.bin_dir.join("legacy-tool");
        fs::write(&file_path, b"payload").expect("write file");
        seed(
            &ctx,
            vec![unknown_backend_object(
                "legacy-tool",
                vec![anolisa_file(file_path)],
            )],
        );
        // No rpm/dnf on this host at all: a package-less quarantined record
        // degrades to the file-based exit instead of failing.
        let fake = FakeRpm::new("unrelated", None).command_missing();

        repair_with_deps("legacy-tool", &ctx, &fake, &fake, false).expect("repair ok");

        assert!(matches!(
            find_component(&ctx, "legacy-tool").binding,
            ProviderBinding::Owned { .. }
        ));
    }

    #[test]
    fn unrecoverable_quarantined_record_points_at_forget() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        // The recorded file does not exist: no native presence, no file
        // evidence — nothing to rebuild from.
        seed(
            &ctx,
            vec![unknown_backend_object(
                "legacy-tool",
                vec![anolisa_file(layout.bin_dir.join("gone"))],
            )],
        );
        let fake = FakeRpm::new("unrelated", None);

        let err = repair_with_deps("legacy-tool", &ctx, &fake, &fake, false).unwrap_err();

        assert!(err.reason().contains("forget"), "got: {err}");
        assert_eq!(
            load_store(&ctx).quarantined.len(),
            1,
            "refusal must not consume the record"
        );
    }

    // ── R2 / NoOp: owned records ─────────────────────────────────────

    #[test]
    fn healthy_owned_record_is_nothing_to_repair() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        let file_path = layout.bin_dir.join("skillfs");
        fs::write(&file_path, b"payload").expect("write file");
        seed(
            &ctx,
            vec![raw_object(
                "skillfs",
                "1.0.0",
                vec![anolisa_file(file_path)],
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let before = fs::read_to_string(&state_path).expect("read state");
        let fake = FakeRpm::new("unrelated", None);

        repair_with_deps("skillfs", &ctx, &fake, &fake, false).expect("repair ok");

        assert_eq!(
            fs::read_to_string(&state_path).expect("re-read state"),
            before,
            "a no-op repair must not rewrite state"
        );
    }

    /// Fixture for the R2 replay tests: a local `file://` repo publishing
    /// `skillfs 1.0.0`, a repo.toml pointing at it, and a v5 owned record
    /// whose file is damaged on disk.
    fn seed_damaged_owned(tmp: &Path, ctx: &CliContext) -> (PathBuf, PathBuf) {
        use crate::commands::tier1::install::tests::write_local_repo_component;

        let layout = common::resolve_layout(ctx);
        let repo_root = tmp.join("repo");
        let base_url = write_local_repo_component(
            &repo_root,
            "skillfs",
            "1.0.0",
            &[ctx.install_mode.as_str()],
        );
        fs::create_dir_all(&layout.etc_dir).expect("etc dir");
        fs::write(
            layout.etc_dir.join("repo.toml"),
            format!(
                "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"{base_url}\"\n"
            ),
        )
        .expect("write repo.toml");

        // The record claims the file, but it is gone from disk: damage the
        // integrity probe reports as Some(false), routing into R2.
        let binary = layout.bin_dir.join("skillfs");
        let mut obj = raw_object("skillfs", "1.0.0", vec![anolisa_file(binary.clone())]);
        obj.raw_package = Some("skillfs".to_string());
        seed(ctx, vec![obj]);
        let state_path = layout.state_dir.join("installed.toml");
        (state_path, binary)
    }

    #[test]
    fn damaged_owned_record_replays_the_recorded_version() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let (state_path, binary) = seed_damaged_owned(tmp.path(), &ctx);
        let fake = FakeRpm::new("unrelated", None);

        repair_with_deps("skillfs", &ctx, &fake, &fake, false).expect("repair ok");

        // The artifact's payload was re-placed on disk.
        let placed = fs::read_to_string(&binary).expect("read replayed binary");
        assert!(placed.contains("echo skillfs"), "got: {placed}");

        // The record was rewritten from this run's execution state.
        let store = StateStore::load(&state_path, 0).expect("reload");
        let record = store
            .find(ObjectKind::Component, "skillfs")
            .expect("record");
        match &record.binding {
            ProviderBinding::Owned { artifact } => {
                assert_eq!(artifact.version, "1.0.0", "replay never upgrades");
                assert!(artifact.files.iter().any(|f| f.path == binary));
            }
            other => panic!("expected owned record, got {other:?}"),
        }
        assert_eq!(store.operations.len(), 1);
        assert!(store.operations[0].command.starts_with("repair"));
    }

    #[test]
    #[cfg(unix)]
    fn mode_only_damage_replays_and_restores_declared_mode() {
        use std::os::unix::fs::PermissionsExt;

        use crate::commands::tier1::install::tests::{
            component_manifest_toml, write_local_repo_component,
        };

        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        let repo_root = tmp.path().join("repo");
        let base_url = write_local_repo_component(&repo_root, "skillfs", "1.0.0", &["system"]);
        fs::create_dir_all(&layout.etc_dir).expect("etc dir");
        fs::write(
            layout.etc_dir.join("repo.toml"),
            format!(
                "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"{base_url}\"\n"
            ),
        )
        .expect("write repo.toml");

        let binary = layout.bin_dir.join("skillfs");
        fs::create_dir_all(&layout.bin_dir).expect("bin dir");
        let payload = b"#!/bin/sh\necho skillfs\n";
        fs::write(&binary, payload).expect("write intact binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).expect("damage mode");
        let mut owned = anolisa_file(binary.clone());
        owned.sha256 = Some({
            use sha2::{Digest, Sha256};
            Sha256::digest(payload)
                .iter()
                .fold(String::new(), |mut digest, byte| {
                    use std::fmt::Write;
                    let _ = write!(digest, "{byte:02x}");
                    digest
                })
        });
        let mut object = raw_object("skillfs", "1.0.0", vec![owned]);
        object.raw_package = Some("skillfs".to_string());
        seed(&ctx, vec![object]);

        let manifest_path = common::installed_component_manifest_path(&layout, "skillfs", COMMAND)
            .expect("manifest path");
        fs::create_dir_all(manifest_path.parent().expect("manifest parent")).expect("manifest dir");
        fs::write(
            &manifest_path,
            component_manifest_toml("skillfs", "1.0.0", &["system"]),
        )
        .expect("saved installed manifest");
        let fake = FakeRpm::new("unrelated", None);

        repair_with_deps("skillfs", &ctx, &fake, &fake, false).expect("repair mode");

        assert_eq!(
            fs::metadata(&binary)
                .expect("binary metadata")
                .permissions()
                .mode()
                & 0o7777,
            0o755
        );
        let state_path = layout.state_dir.join("installed.toml");
        let store = StateStore::load(&state_path, 0).expect("reload");
        let ProviderBinding::Owned { artifact } = &store
            .find(ObjectKind::Component, "skillfs")
            .expect("record")
            .binding
        else {
            panic!("expected owned record");
        };
        let binary_row = artifact
            .files
            .iter()
            .find(|file| file.path == binary)
            .expect("binary row");
        assert_eq!(binary_row.mode.as_deref(), Some("0755"));
    }

    #[test]
    fn damaged_user_owned_record_replays_without_native_privileges() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::User, false);
        let (state_path, binary) = seed_damaged_owned(tmp.path(), &ctx);
        let fake = FakeRpm::new("unrelated", None);

        repair_with_deps("skillfs", &ctx, &fake, &fake, false).expect("user repair ok");

        assert!(binary.is_file(), "owned payload should be restored");
        let store = StateStore::load(&state_path, 0).expect("reload");
        let record = store
            .find(ObjectKind::Component, "skillfs")
            .expect("record");
        assert!(matches!(record.scope, InstallationScope::User { .. }));
        assert!(matches!(record.binding, ProviderBinding::Owned { .. }));
    }

    #[test]
    fn owned_replay_failure_reports_the_failed_step() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let (state_path, _binary) = seed_damaged_owned(tmp.path(), &ctx);
        // The index still lists 1.0.0 but the artifact bytes are gone.
        fs::remove_file(tmp.path().join("repo").join("v1").join("skillfs.tar.gz"))
            .expect("remove artifact");
        let fake = FakeRpm::new("unrelated", None);

        let err = repair_with_deps("skillfs", &ctx, &fake, &fake, false).unwrap_err();

        assert!(
            err.reason().contains("download and verify artifact"),
            "got: {err}"
        );
        let store = StateStore::load(&state_path, 0).expect("reload");
        assert!(
            store.operations.is_empty(),
            "no ok history for a failed repair"
        );
    }

    // ── R1: journal recovery ─────────────────────────────────────────

    #[test]
    fn legacy_pending_install_recovers_when_the_package_is_present() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let pending =
            rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin pending install");
        let journal_path = pending.transaction.journal_path.clone();
        let operation_id = pending.transaction.operation_id.clone();
        drop(pending);
        // dnf install completed before the crash; the record commit did not.
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.7.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("cosh", &ctx, &fake, &fake, false).expect("repair ok");

        let record = find_component(&ctx, "cosh");
        match &record.binding {
            ProviderBinding::Delegated {
                relation: ManagementRelation::Managed { .. },
                package,
                ..
            } => {
                assert_eq!(package.resolved_name(), Some("copilot-shell"));
            }
            other => panic!("expected managed delegated record, got {other:?}"),
        }
        assert_eq!(observed_evr(&record).as_deref(), Some("2.7.0-1.al4"));
        let journal = Transaction::load_journal(&journal_path).expect("reload journal");
        assert_eq!(journal.status, TransactionOutcomeStatus::Ok);
        let store = load_store(&ctx);
        assert!(
            store
                .operations
                .iter()
                .any(|op| op.id == operation_id && op.status == "ok"),
            "the recovered install's operation id enters history"
        );
    }

    #[test]
    fn subjectless_legacy_install_is_not_recovered_for_an_unrelated_target() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let pending =
            rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin pending install");
        let journal_path = pending.transaction.journal_path.clone();
        drop(pending);
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.7.0", Some("1.al4"), "x86_64")),
        );

        let result = repair_with_deps("skillfs", &ctx, &fake, &fake, false);

        result.expect_err("an unrelated target must not own the legacy journal");
        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none()
        );
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::InFlight,
        );
    }

    #[test]
    fn malformed_legacy_journal_stays_pending() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let state_before = fs::read(&state_path).expect("read state");
        let mut pending =
            rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin pending install");
        pending.transaction.steps.reverse();
        let journal_path = pending.transaction.journal_path.clone();
        fs::write(
            &journal_path,
            toml::to_string_pretty(&pending.transaction).expect("serialize journal"),
        )
        .expect("rewrite journal");
        drop(pending);
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.7.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("ambiguous legacy recovery must fail closed");

        assert!(err.reason().contains("automatic recovery is unsafe"));
        let journal = Transaction::load_journal(&journal_path).expect("reload journal");
        assert_eq!(journal.status, TransactionOutcomeStatus::InFlight);
        assert_eq!(fs::read(&state_path).expect("re-read state"), state_before);
        assert_eq!(fake.install_calls.get(), 0);
    }

    #[test]
    fn subject_only_legacy_hybrid_stays_pending_without_replanning() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let state_before = fs::read(&state_path).expect("read state");
        let mut pending =
            rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin pending install");
        pending.transaction.subject = Some("cosh".to_string());
        let journal_path = pending.transaction.journal_path.clone();
        fs::write(
            &journal_path,
            toml::to_string_pretty(&pending.transaction).expect("serialize hybrid journal"),
        )
        .expect("rewrite hybrid journal");
        drop(pending);
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.7.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("subject-only legacy hybrid must fail closed");

        assert!(err.reason().contains("automatic recovery is unsafe"));
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::InFlight
        );
        assert_eq!(fs::read(&state_path).expect("re-read state"), state_before);
        assert_eq!(fake.install_calls.get(), 0);
    }

    #[test]
    fn legacy_pending_install_terminates_when_the_package_is_absent() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let pending =
            rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin pending install");
        let journal_path = pending.transaction.journal_path.clone();
        drop(pending);
        let fake = FakeRpm::new("copilot-shell", None);

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false).unwrap_err();

        assert!(err.reason().contains("install cosh"), "got: {err}");
        let journal = Transaction::load_journal(&journal_path).expect("reload journal");
        assert_eq!(journal.status, TransactionOutcomeStatus::Failed);
        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none(),
            "no record is fabricated for an absent package"
        );
    }

    #[test]
    fn legacy_pending_install_refuses_to_overwrite_a_different_owner() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        // "other" already tracks the same package.
        seed(
            &ctx,
            vec![rpm_object(
                "other",
                "copilot-shell",
                "1.0.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let pending =
            rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin pending install");
        drop(pending);
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.7.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false).unwrap_err();

        assert!(err.reason().contains("conflicts"), "got: {err}");
        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none()
        );
    }

    /// A pipeline journal interrupted mid-operation: subject + a
    /// native-transaction step, never finished.
    fn write_pipeline_journal(
        layout: &FsLayout,
        operation: &str,
        subject: &str,
        phase: &str,
        target: &str,
    ) -> PathBuf {
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(layout);
        let mut journal =
            Transaction::begin_with_subject(operation, Some(subject), state_path, &journal_dir)
                .expect("begin journal");
        journal
            .record_steps([TransactionStep::planned(phase, target, "native", None)])
            .expect("record steps");
        let path = journal.journal_path.clone();
        drop(journal);
        path
    }

    fn write_explicit_delegated_journal(
        layout: &FsLayout,
        operation: &str,
        subject: &str,
        package: &str,
        record_action: DelegatedRecordAction,
        shared_native_target: Option<&str>,
    ) -> PathBuf {
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(layout);
        let mut journal =
            Transaction::begin_with_subject(operation, Some(subject), state_path, &journal_dir)
                .expect("begin journal");
        let record_step = match record_action {
            DelegatedRecordAction::WriteManaged => "write-delegated-managed",
            DelegatedRecordAction::WriteAdopted => "write-delegated-adopted",
            DelegatedRecordAction::WriteObserved => "write-delegated-observed",
            DelegatedRecordAction::Refresh => "refresh-observation",
            DelegatedRecordAction::Drop => "drop-record",
        };
        let native_action = match operation {
            "uninstall" => "remove",
            "update" => "update",
            _ => "install",
        };
        let mut steps = shared_native_target
            .map(|target| TransactionStep::planned(PHASE_NATIVE_TXN, target, native_action, None))
            .into_iter()
            .collect::<Vec<_>>();
        steps.push(TransactionStep::planned(
            anolisa_core::executor::PHASE_RECORD,
            "state",
            record_step,
            None,
        ));
        journal
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some(package.to_string()),
                    record_action,
                    pinned: None,
                },
                steps,
            )
            .expect("record recovery contract");
        let path = journal.journal_path.clone();
        drop(journal);
        path
    }

    /// A version-pinned install journal interrupted after dnf committed but
    /// before the record: NEVRA native step, bare observe step, and a pinned
    /// recovery contract — exactly what the executor persists.
    fn write_pinned_install_journal(
        layout: &FsLayout,
        subject: &str,
        package: &str,
        artifact: &str,
        evr: &str,
        arch: &str,
    ) -> PathBuf {
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(layout);
        let mut journal =
            Transaction::begin_with_subject("install", Some(subject), state_path, &journal_dir)
                .expect("begin journal");
        journal
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some(package.to_string()),
                    record_action: DelegatedRecordAction::WriteManaged,
                    pinned: Some(DelegatedPinnedArtifact {
                        artifact: artifact.to_string(),
                        evr: evr.to_string(),
                        arch: arch.to_string(),
                    }),
                },
                [
                    TransactionStep::planned(PHASE_NATIVE_TXN, artifact, "install", None),
                    TransactionStep::planned(PHASE_OBSERVE, package, "observe", None),
                    TransactionStep::planned(
                        anolisa_core::executor::PHASE_RECORD,
                        "state",
                        "write-delegated-managed",
                        None,
                    ),
                ],
            )
            .expect("record pinned recovery contract");
        let path = journal.journal_path.clone();
        drop(journal);
        path
    }

    #[test]
    fn pinned_install_journal_recovers_when_the_installed_evr_matches() {
        // Crash after dnf committed the pinned artifact: repair re-observes the
        // bare package, confirms the EVR/arch matches the durable pin, and
        // commits the managed record for the exact version.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        let journal_path = write_pinned_install_journal(
            &layout,
            "cosh",
            "copilot-shell",
            "copilot-shell-2.6.0-1.al4.x86_64",
            "2.6.0-1.al4",
            "x86_64",
        );
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.6.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect("a matching pinned install must recover");

        let store = load_store(&ctx);
        let record = store
            .find(ObjectKind::Component, "cosh")
            .expect("record committed");
        match &record.binding {
            ProviderBinding::Delegated {
                package,
                last_observed,
                ..
            } => {
                assert_eq!(package.resolved_name(), Some("copilot-shell"));
                assert_eq!(
                    last_observed.as_ref().and_then(|o| o.evr.as_deref()),
                    Some("2.6.0-1.al4")
                );
            }
            other => panic!("expected a delegated binding, got {other:?}"),
        }
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Ok
        );
    }

    #[test]
    fn pinned_install_journal_refuses_recovery_on_evr_mismatch() {
        // Crash left a different EVR installed than the pin resolved. Recovery
        // must refuse to record the wrong version and leave the journal pending
        // for manual reconciliation — never fall back to what is present.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        let journal_path = write_pinned_install_journal(
            &layout,
            "cosh",
            "copilot-shell",
            "copilot-shell-2.6.0-1.al4.x86_64",
            "2.6.0-1.al4",
            "x86_64",
        );
        // rpmdb reports 2.7.0, not the pinned 2.6.0.
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.7.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("a mismatched installed EVR must not be recovered");
        assert!(
            err.reason().contains("2.7.0-1.al4"),
            "got: {}",
            err.reason()
        );
        assert!(
            err.reason().contains("copilot-shell-2.6.0-1.al4.x86_64"),
            "got: {}",
            err.reason()
        );
        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none(),
            "the wrong version must not be recorded"
        );
        // The journal stays pending so the user can reconcile and retry.
        assert!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .is_pending(),
            "a refused recovery must leave the journal pending"
        );
    }

    #[test]
    fn pinned_install_journal_reports_when_rpmdb_arch_is_unavailable() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        let journal_path = write_pinned_install_journal(
            &layout,
            "cosh",
            "copilot-shell",
            "copilot-shell-2.6.0-1.al4.x86_64",
            "2.6.0-1.al4",
            "x86_64",
        );
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.6.0", Some("1.al4"), "")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("a missing rpmdb architecture must not satisfy the pin");
        assert!(
            err.reason().contains("architecture unavailable from rpmdb"),
            "got: {}",
            err.reason()
        );
        assert!(
            !err.reason().contains("unknown-arch"),
            "a sentinel must not look like an rpm architecture: {}",
            err.reason()
        );
        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none()
        );
        assert!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .is_pending()
        );
    }

    #[test]
    fn pinned_journal_with_a_mixed_native_target_is_not_recovered() {
        // A pinned native step must resolve to exactly the artifact. A journal
        // whose native target smuggles in another package alongside the NEVRA
        // is malformed and must stay pending, not recover.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal =
            Transaction::begin_with_subject("install", Some("cosh"), state_path, &journal_dir)
                .expect("begin journal");
        journal
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some("copilot-shell".to_string()),
                    record_action: DelegatedRecordAction::WriteManaged,
                    pinned: Some(DelegatedPinnedArtifact {
                        artifact: "copilot-shell-2.6.0-1.al4.x86_64".to_string(),
                        evr: "2.6.0-1.al4".to_string(),
                        arch: "x86_64".to_string(),
                    }),
                },
                [
                    TransactionStep::planned(
                        PHASE_NATIVE_TXN,
                        "copilot-shell-2.6.0-1.al4.x86_64,unrelated-package",
                        "install",
                        None,
                    ),
                    TransactionStep::planned(PHASE_OBSERVE, "copilot-shell", "observe", None),
                    TransactionStep::planned(
                        anolisa_core::executor::PHASE_RECORD,
                        "state",
                        "write-delegated-managed",
                        None,
                    ),
                ],
            )
            .expect("record malformed pinned contract");
        let journal_path = journal.journal_path.clone();
        drop(journal);
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.6.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("a mixed pinned native target must not recover");
        assert!(
            err.reason().contains("cannot recover"),
            "got: {}",
            err.reason()
        );
        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none()
        );
        assert!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .is_pending()
        );
    }

    #[test]
    fn unpinned_journal_with_a_mixed_observe_target_is_not_recovered() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal =
            Transaction::begin_with_subject("install", Some("cosh"), state_path, &journal_dir)
                .expect("begin journal");
        journal
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some("copilot-shell".to_string()),
                    record_action: DelegatedRecordAction::WriteManaged,
                    pinned: None,
                },
                [
                    TransactionStep::planned(PHASE_NATIVE_TXN, "copilot-shell", "install", None),
                    TransactionStep::planned(
                        PHASE_OBSERVE,
                        "copilot-shell,foreign-package",
                        "observe",
                        None,
                    ),
                    TransactionStep::planned(
                        anolisa_core::executor::PHASE_RECORD,
                        "state",
                        "write-delegated-managed",
                        None,
                    ),
                ],
            )
            .expect("record malformed unpinned contract");
        let journal_path = journal.journal_path.clone();
        drop(journal);
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.6.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("a mixed observe target must not recover");

        assert!(
            err.reason().contains("cannot recover"),
            "got: {}",
            err.reason()
        );
        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none(),
            "an unsafe recovery contract must not create a record"
        );
        assert!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .is_pending(),
            "an unsafe recovery contract must remain pending"
        );
    }

    #[test]
    fn delegated_drop_rejects_a_package_that_does_not_bind_the_record() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let state_before = fs::read(&state_path).expect("read state");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal = Transaction::begin_with_subject(
            "uninstall",
            Some("cosh"),
            state_path.clone(),
            &journal_dir,
        )
        .expect("begin journal");
        journal.delegated_recovery = Some(DelegatedRecoveryContext {
            pm: NativePm::Rpm,
            package: Some("unrelated".to_string()),
            record_action: DelegatedRecordAction::Drop,
            pinned: None,
        });
        journal
            .record_step(TransactionStep::planned(
                anolisa_core::executor::PHASE_RECORD,
                "state",
                "drop-record",
                None,
            ))
            .expect("persist crafted drop intent");
        let journal_path = journal.journal_path.clone();
        drop(journal);
        let fake = FakeRpm::new("unrelated", None);

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("mismatched drop contract must fail closed");

        assert!(err.reason().contains("no longer matches"), "got: {err}");
        assert_eq!(fs::read(&state_path).expect("re-read state"), state_before);
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::InFlight
        );
    }

    #[test]
    fn explicit_delegated_drop_requires_an_exact_subject() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let state_before = fs::read(&state_path).expect("read state");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal = Transaction::begin("uninstall", state_path.clone(), &journal_dir)
            .expect("begin subjectless journal");
        journal.delegated_recovery = Some(DelegatedRecoveryContext {
            pm: NativePm::Rpm,
            package: Some("copilot-shell".to_string()),
            record_action: DelegatedRecordAction::Drop,
            pinned: None,
        });
        journal
            .record_step(TransactionStep::planned(
                anolisa_core::executor::PHASE_RECORD,
                "state",
                "drop-record",
                None,
            ))
            .expect("persist subjectless drop intent");
        let journal_path = journal.journal_path.clone();
        drop(journal);
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.6.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("explicit recovery without a subject must fail closed");

        assert!(err.reason().contains("subject"), "got: {err}");
        assert_eq!(fs::read(&state_path).expect("re-read state"), state_before);
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::InFlight
        );
    }

    #[test]
    fn explicit_delegated_drop_rejects_an_owned_record_phase() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        let file_path = layout.bin_dir.join("skillfs");
        fs::write(&file_path, b"payload").expect("write file");
        seed(
            &ctx,
            vec![raw_object(
                "skillfs",
                "1.0.0",
                vec![anolisa_file(file_path)],
            )],
        );
        let state_path = layout.state_dir.join("installed.toml");
        let state_before = fs::read(&state_path).expect("read state");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal = Transaction::begin_with_subject(
            "uninstall",
            Some("skillfs"),
            state_path.clone(),
            &journal_dir,
        )
        .expect("begin journal");
        journal
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some("skillfs".to_string()),
                    record_action: DelegatedRecordAction::Drop,
                    pinned: None,
                },
                [TransactionStep::planned(
                    anolisa_core::owned_executor::PHASE_RECORD,
                    "state",
                    "drop-record",
                    None,
                )],
            )
            .expect("persist malformed recovery contract");
        let journal_path = journal.journal_path.clone();
        drop(journal);
        let fake = FakeRpm::new("skillfs", None);

        let err = repair_with_deps("skillfs", &ctx, &fake, &fake, false)
            .expect_err("mixed recovery family must fail closed");

        assert!(err.reason().contains("non-delegated phase"), "got: {err}");
        assert_eq!(fs::read(&state_path).expect("re-read state"), state_before);
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::InFlight,
        );
    }

    #[test]
    fn context_only_managed_drop_keeps_state_when_the_package_is_present() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let state_before = fs::read(&state_path).expect("read state");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal = Transaction::begin_with_subject(
            "uninstall",
            Some("cosh"),
            state_path.clone(),
            &journal_dir,
        )
        .expect("begin journal");
        journal.delegated_recovery = Some(DelegatedRecoveryContext {
            pm: NativePm::Rpm,
            package: Some("copilot-shell".to_string()),
            record_action: DelegatedRecordAction::Drop,
            pinned: None,
        });
        fs::write(
            &journal.journal_path,
            toml::to_string_pretty(&journal).expect("serialize context-only journal"),
        )
        .expect("persist context-only journal");
        let journal_path = journal.journal_path.clone();
        drop(journal);
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.6.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect_err("context-only managed drop must fail closed");

        assert!(
            err.reason()
                .contains("recovery context has no operation steps"),
            "got: {err}"
        );
        assert_eq!(fs::read(&state_path).expect("re-read state"), state_before);
        assert_eq!(
            toml::from_str::<Transaction>(
                &fs::read_to_string(&journal_path).expect("read malformed journal")
            )
            .expect("parse journal without structural validation")
            .status,
            TransactionOutcomeStatus::InFlight
        );
    }

    #[test]
    fn adopted_record_only_drop_still_succeeds_with_a_present_package() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "2.6.0-1.al4",
                Ownership::RpmObserved,
                true,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let journal_path = write_explicit_delegated_journal(
            &layout,
            "uninstall",
            "cosh",
            "copilot-shell",
            DelegatedRecordAction::Drop,
            None,
        );
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.6.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect("record-only adopted drop remains recoverable");

        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none()
        );
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Ok
        );
    }

    #[test]
    fn managed_record_only_drop_succeeds_when_the_package_is_absent() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let journal_path = write_explicit_delegated_journal(
            &layout,
            "uninstall",
            "cosh",
            "copilot-shell",
            DelegatedRecordAction::Drop,
            None,
        );
        let fake = FakeRpm::new("copilot-shell", None);

        repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect("an absent managed package permits the record drop");

        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none()
        );
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Ok
        );
    }

    #[test]
    fn managed_drop_finishes_after_the_native_remove_landed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let journal_path = write_explicit_delegated_journal(
            &layout,
            "uninstall",
            "cosh",
            "copilot-shell",
            DelegatedRecordAction::Drop,
            Some("copilot-shell"),
        );
        let fake = FakeRpm::new("copilot-shell", None);

        repair_with_deps("cosh", &ctx, &fake, &fake, false)
            .expect("a landed native removal permits the record drop");

        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "cosh")
                .is_none()
        );
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Ok
        );
    }

    #[test]
    fn interrupted_delegated_update_settles_the_journal_and_replans() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "cosh",
                "2.6.0-1.al4",
                Ownership::RpmManaged,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let journal_path =
            write_pipeline_journal(&layout, "update", "cosh", PHASE_NATIVE_TXN, "cosh");
        // The interrupted dnf update actually committed 2.7.0.
        let fake = FakeRpm::new(
            "cosh",
            Some(pkg_info("cosh", "2.7.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("cosh", &ctx, &fake, &fake, false).expect("repair ok");

        let journal = Transaction::load_journal(&journal_path).expect("reload journal");
        assert_eq!(journal.status, TransactionOutcomeStatus::Ok);
        // The replan absorbed the post-update observation (R3).
        let record = find_component(&ctx, "cosh");
        assert_eq!(observed_evr(&record).as_deref(), Some("2.7.0-1.al4"));
    }

    #[test]
    fn interrupted_delegated_install_without_a_record_is_adopted_as_managed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let journal_path =
            write_pipeline_journal(&layout, "install", "newcomp", PHASE_NATIVE_TXN, "newpkg");
        let fake = FakeRpm::new(
            "newpkg",
            Some(pkg_info("newpkg", "1.0.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("newcomp", &ctx, &fake, &fake, false).expect("repair ok");

        let record = find_component(&ctx, "newcomp");
        match &record.binding {
            ProviderBinding::Delegated {
                relation: ManagementRelation::Managed { .. },
                package,
                ..
            } => assert_eq!(package.resolved_name(), Some("newpkg")),
            other => panic!("expected managed delegated record, got {other:?}"),
        }
        let journal = Transaction::load_journal(&journal_path).expect("reload journal");
        assert_eq!(journal.status, TransactionOutcomeStatus::Ok);
    }

    #[test]
    fn explicit_batch_recovery_uses_the_subject_package() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let journal_path = write_explicit_delegated_journal(
            &layout,
            "install",
            "b",
            "pkg-b",
            DelegatedRecordAction::WriteManaged,
            Some("pkg-a,pkg-b"),
        );
        let fake = FakeRpm::new(
            "pkg-b",
            Some(pkg_info("pkg-b", "1.0.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("b", &ctx, &fake, &fake, false).expect("repair b");

        let record = find_component(&ctx, "b");
        match &record.binding {
            ProviderBinding::Delegated {
                package, relation, ..
            } => {
                assert_eq!(package.resolved_name(), Some("pkg-b"));
                assert!(matches!(relation, ManagementRelation::Managed { .. }));
            }
            other => panic!("expected delegated record, got {other:?}"),
        }
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Ok
        );
    }

    #[test]
    fn legacy_batch_journal_without_subject_mapping_stays_pending() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let journal_path =
            write_pipeline_journal(&layout, "install", "b", PHASE_NATIVE_TXN, "pkg-a,pkg-b");
        let fake = FakeRpm::new(
            "pkg-b",
            Some(pkg_info("pkg-b", "1.0.0", Some("1.al4"), "x86_64")),
        );

        let err = repair_with_deps("b", &ctx, &fake, &fake, false).unwrap_err();

        assert!(err.reason().contains("multiple packages"), "got: {err}");
        let journal = Transaction::load_journal(&journal_path).expect("reload journal");
        assert_eq!(journal.status, TransactionOutcomeStatus::InFlight);
        assert!(load_store(&ctx).installations.is_empty());
    }

    #[test]
    fn legacy_batch_journal_uses_a_unique_subject_observation() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let journal_path =
            write_pipeline_journal(&layout, "install", "b", PHASE_NATIVE_TXN, "pkg-a,pkg-b");
        let mut journal = Transaction::load_journal(&journal_path).expect("load journal");
        journal
            .record_step(TransactionStep::planned(
                anolisa_core::executor::PHASE_OBSERVE,
                "pkg-b",
                "observe",
                None,
            ))
            .expect("record subject observation");
        let fake = FakeRpm::new(
            "pkg-b",
            Some(pkg_info("pkg-b", "1.0.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("b", &ctx, &fake, &fake, false).expect("repair b");

        let record = find_component(&ctx, "b");
        assert!(matches!(
            &record.binding,
            ProviderBinding::Delegated {
                package,
                relation: ManagementRelation::Managed { .. },
                ..
            } if package.resolved_name() == Some("pkg-b")
        ));
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Ok
        );
    }

    #[test]
    fn interrupted_adopt_preserves_the_adopted_relation() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &ctx,
            vec![rpm_object(
                "cosh",
                "copilot-shell",
                "2.6.0-1.al4",
                Ownership::RpmObserved,
                false,
            )],
        );
        let layout = common::resolve_layout(&ctx);
        let journal_path = write_explicit_delegated_journal(
            &layout,
            "adopt",
            "cosh",
            "copilot-shell",
            DelegatedRecordAction::WriteAdopted,
            None,
        );
        let fake = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.7.0", Some("1.al4"), "x86_64")),
        );

        repair_with_deps("cosh", &ctx, &fake, &fake, false).expect("repair adopt");

        let record = find_component(&ctx, "cosh");
        assert!(matches!(
            record.binding,
            ProviderBinding::Delegated {
                relation: ManagementRelation::Adopted { .. },
                ..
            }
        ));
        assert_eq!(observed_evr(&record).as_deref(), Some("2.7.0-1.al4"));
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Ok
        );
    }

    #[test]
    fn interrupted_delegated_operation_with_absent_package_terminates() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let journal_path =
            write_pipeline_journal(&layout, "install", "newcomp", PHASE_NATIVE_TXN, "newpkg");
        let fake = FakeRpm::new("newpkg", None);

        let err = repair_with_deps("newcomp", &ctx, &fake, &fake, false).unwrap_err();

        assert!(err.reason().contains("install newcomp"), "got: {err}");
        let journal = Transaction::load_journal(&journal_path).expect("reload journal");
        assert_eq!(journal.status, TransactionOutcomeStatus::Failed);
    }

    #[test]
    fn interrupted_owned_operation_terminates_and_replans() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        let file_path = layout.bin_dir.join("skillfs");
        fs::write(&file_path, b"payload").expect("write file");
        seed(
            &ctx,
            vec![raw_object(
                "skillfs",
                "1.0.0",
                vec![anolisa_file(file_path)],
            )],
        );
        let journal_path =
            write_pipeline_journal(&layout, "update", "skillfs", PHASE_FILES, "skillfs");
        let fake = FakeRpm::new("unrelated", None);

        // The files are healthy, so the replan converges on nothing-to-do.
        repair_with_deps("skillfs", &ctx, &fake, &fake, false).expect("repair ok");

        let journal = Transaction::load_journal(&journal_path).expect("reload journal");
        assert_eq!(
            journal.status,
            TransactionOutcomeStatus::Failed,
            "an owned journal's compensation state died with the process; it is terminated, not replayed"
        );
    }

    #[test]
    fn fresh_owned_install_without_recovery_context_stays_pending() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let journal_path =
            write_pipeline_journal(&layout, "install", "skillfs", PHASE_FILES, "owned-files");
        let fake = FakeRpm::new("unrelated", None);

        let result = repair_with_deps("skillfs", &ctx, &fake, &fake, false);

        result.expect_err("unknown fresh-owned side effects must remain pending");
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::InFlight,
        );
        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "skillfs")
                .is_none()
        );
    }

    #[test]
    fn fresh_owned_install_with_matching_committed_record_is_settled() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal =
            Transaction::begin_with_subject("install", Some("skillfs"), state_path, &journal_dir)
                .expect("begin journal");
        journal
            .record_steps([TransactionStep::planned(
                PHASE_FILES,
                "owned-files",
                "place-files",
                None,
            )])
            .expect("record step");
        let journal_path = journal.journal_path.clone();
        let operation_id = journal.operation_id.clone();
        drop(journal);

        let mut object = raw_object("skillfs", "1.0.0", Vec::new());
        object.last_operation_id = Some(operation_id);
        seed(&ctx, vec![object]);
        let fake = FakeRpm::new("unrelated", None);

        repair_with_deps("skillfs", &ctx, &fake, &fake, false)
            .expect("matching committed record settles the journal");

        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Ok,
        );
        assert_eq!(
            load_store(&ctx)
                .find(ObjectKind::Component, "skillfs")
                .expect("record")
                .status,
            LifecycleStatus::Installed,
        );
    }

    #[test]
    fn interrupted_owned_uninstall_drop_terminates_and_replans() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        let file_path = layout.bin_dir.join("skillfs");
        fs::write(&file_path, b"payload").expect("write file");
        seed(
            &ctx,
            vec![raw_object(
                "skillfs",
                "1.0.0",
                vec![anolisa_file(file_path)],
            )],
        );
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal =
            Transaction::begin_with_subject("uninstall", Some("skillfs"), state_path, &journal_dir)
                .expect("begin journal");
        journal
            .record_steps([TransactionStep::planned(
                anolisa_core::owned_executor::PHASE_RECORD,
                "state",
                "drop-record",
                None,
            )])
            .expect("record owned drop");
        let journal_path = journal.journal_path.clone();
        drop(journal);
        let fake = FakeRpm::new("unrelated", None);

        repair_with_deps("skillfs", &ctx, &fake, &fake, false).expect("repair ok");

        assert!(
            load_store(&ctx)
                .find(ObjectKind::Component, "skillfs")
                .is_some()
        );
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Failed,
        );
    }

    #[test]
    fn interrupted_owned_uninstall_after_record_drop_clears_the_journal() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(&ctx, Vec::new());
        let layout = common::resolve_layout(&ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(&layout);
        let mut journal =
            Transaction::begin_with_subject("uninstall", Some("skillfs"), state_path, &journal_dir)
                .expect("begin journal");
        journal
            .record_steps([TransactionStep::planned(
                anolisa_core::owned_executor::PHASE_RECORD,
                "state",
                "drop-record",
                None,
            )])
            .expect("record owned drop");
        let journal_path = journal.journal_path.clone();
        drop(journal);
        let fake = FakeRpm::new("unrelated", None);

        let err = repair_with_deps("skillfs", &ctx, &fake, &fake, false)
            .expect_err("completed uninstall has no record left to repair");

        assert!(err.reason().contains("not installed"), "got: {err}");
        assert_eq!(
            Transaction::load_journal(&journal_path)
                .expect("reload journal")
                .status,
            TransactionOutcomeStatus::Failed,
        );
    }

    #[test]
    fn journal_recovery_replans_only_once() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let layout = common::resolve_layout(&ctx);
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        let file_path = layout.bin_dir.join("skillfs");
        fs::write(&file_path, b"payload").expect("write file");
        seed(
            &ctx,
            vec![raw_object(
                "skillfs",
                "1.0.0",
                vec![anolisa_file(file_path)],
            )],
        );
        // Two independent pending journals for the same subject.
        let first = write_pipeline_journal(&layout, "update", "skillfs", PHASE_FILES, "skillfs");
        let second = write_pipeline_journal(&layout, "update", "skillfs", PHASE_FILES, "skillfs");
        let fake = FakeRpm::new("unrelated", None);

        let err = repair_with_deps("skillfs", &ctx, &fake, &fake, false).unwrap_err();

        assert!(err.reason().contains("still pending"), "got: {err}");
        // Exactly one of the two was consumed.
        let settled = [&first, &second]
            .iter()
            .filter(|path| {
                Transaction::load_journal(path).expect("reload").status
                    == TransactionOutcomeStatus::Failed
            })
            .count();
        assert_eq!(settled, 1);
    }

    // ── lock-window authority ─────────────────────────────────────────

    #[test]
    fn delegated_authority_is_recomputed_from_the_locked_read() {
        use anolisa_core::domain::{Observation, PackageIdentity};

        fn delegated(
            name: &str,
            package: PackageIdentity,
            relation: ManagementRelation,
        ) -> Installation {
            Installation {
                kind: ObjectKind::Component,
                name: name.to_string(),
                scope: InstallationScope::System,
                binding: ProviderBinding::Delegated {
                    pm: NativePm::Rpm,
                    package,
                    relation,
                    last_observed: Some(Observation {
                        version: "1.0.0".to_string(),
                        evr: None,
                        arch: None,
                        source_repo: None,
                        observed_at: "2026-07-16T00:00:00Z".to_string(),
                    }),
                },
                status: LifecycleStatus::Installed,
                installed_at: "2026-07-16T00:00:00Z".to_string(),
                last_operation_id: None,
                subscription_scope: Default::default(),
                enabled_features: Vec::new(),
                health: Vec::new(),
            }
        }

        let refresh = [Step::Observe {
            packages: vec!["cosh".to_string()],
        }];
        let reinstall = [Step::NativeTransaction {
            pm: NativePm::Rpm,
            action: anolisa_core::planner::NativeAction::Install,
            packages: vec!["cosh".to_string()],
        }];
        let restore = [Step::WriteRecord(RecordWrite::DelegatedObserved)];
        let managed = ManagementRelation::Managed {
            since: "2026-07-16T00:00:00Z".to_string(),
        };

        let mut store = StateStore::empty();
        store.upsert(delegated(
            "cosh",
            PackageIdentity::Resolved {
                name: "cosh".to_string(),
            },
            managed.clone(),
        ));
        assert!(delegated_repair_authorized(
            &store, "cosh", "cosh", &refresh
        ));
        assert!(delegated_repair_authorized(
            &store, "cosh", "cosh", &reinstall
        ));
        // The record was re-pointed at another package in the lock window.
        assert!(!delegated_repair_authorized(
            &store, "cosh", "other", &refresh
        ));

        // Consent was withdrawn: refresh survives, a transaction does not.
        let mut store = StateStore::empty();
        store.upsert(delegated(
            "cosh",
            PackageIdentity::Resolved {
                name: "cosh".to_string(),
            },
            ManagementRelation::Observed,
        ));
        assert!(delegated_repair_authorized(
            &store, "cosh", "cosh", &refresh
        ));
        assert!(!delegated_repair_authorized(
            &store, "cosh", "cosh", &reinstall
        ));

        // An unresolved identity is the legacy backfill the refresh pins.
        let mut store = StateStore::empty();
        store.upsert(delegated(
            "cosh",
            PackageIdentity::Unresolved {
                component_hint: "cosh".to_string(),
            },
            managed,
        ));
        assert!(delegated_repair_authorized(
            &store, "cosh", "cosh", &refresh
        ));

        // R5: the quarantined record must survive unclaimed.
        let store = StateStore::empty();
        assert!(!delegated_repair_authorized(
            &store, "cosh", "cosh", &restore
        ));
    }
    #[test]
    fn raw_effect_repair_hydrates_legacy_capabilities_through_factory() {
        use crate::commands::tier1::install::tests::component_manifest_toml;
        use std::os::unix::fs::PermissionsExt;

        for supported in [false, true] {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let c = ctx(tmp.path().join("home"), InstallMode::User, false);
            let (state_path, binary) = seed_damaged_owned(tmp.path(), &c);
            let layout = common::resolve_layout(&c);
            fs::create_dir_all(binary.parent().expect("parent")).expect("bindir");
            fs::write(&binary, b"healthy binary\n").expect("binary");
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("mode");
            let manifest_path =
                common::installed_component_manifest_path(&layout, "skillfs", "repair")
                    .expect("manifest path");
            fs::create_dir_all(manifest_path.parent().expect("parent")).expect("manifest dir");
            fs::write(
                &manifest_path,
                format!(
                    "{}\n[[component.capabilities]]\npath = \"{{bindir}}/skillfs\"\ncaps = [\"cap_net_bind_service\"]\noptional = false\n",
                    component_manifest_toml("skillfs", "1.0.0", &["user"])
                ),
            )
            .expect("legacy manifest");
            let before = fs::read(&state_path).expect("state");
            let rpm = FakeRpm::new("unused", None);
            let mut effects = crate::test_support::RawEffectRecorder::default();
            effects.supported = supported;
            let outcome = effects
                .with(|f| {
                    application::run_with_dependencies(
                        application::ApplicationRequest {
                            component: "skillfs",
                            intent: anolisa_core::execution::ExecutionIntent::Plan,
                        },
                        &c,
                        &rpm,
                        &rpm,
                        false,
                        f,
                    )
                })
                .expect("repair preview");
            assert_eq!(effects.calls(), ["capability factory user"]);
            assert!(effects.capability_contents().is_empty());
            if supported {
                assert!(matches!(
                    outcome,
                    application::ApplicationOutcome::Preview { .. }
                ));
            } else {
                assert!(matches!(
                    outcome,
                    application::ApplicationOutcome::NoOp { .. }
                ));
            }
            assert_eq!(fs::read(&state_path).expect("unchanged state"), before);
            assert!(!layout.lock_file.exists());
            assert_eq!(
                fs::read(&binary).expect("unchanged binary"),
                b"healthy binary\n"
            );
        }
    }

    #[test]
    fn raw_effect_repair_replay_uses_injected_service_and_capability() {
        use crate::commands::tier1::install::tests::{
            build_tar_gz, component_manifest_toml, replace_repo_artifact,
        };
        for fail in [None, Some(anolisa_core::ServiceOp::Restart)] {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
            let (state_path, binary) = seed_damaged_owned(tmp.path(), &c);
            let manifest = format!(
                "{}\n[[component.services]]\nunit = \"skillfs.service\"\nenable = true\nstart = true\n\n[[component.capabilities]]\npath = \"{{bindir}}/skillfs\"\ncaps = [\"CAP_BPF\"]\noptional = true\n",
                component_manifest_toml("skillfs", "1.0.0", &["system"])
            );
            replace_repo_artifact(
                &tmp.path().join("repo"),
                "skillfs",
                &build_tar_gz(&[
                    (".anolisa/component.toml", manifest.as_bytes()),
                    ("bin/skillfs", b"repaired binary\n"),
                ]),
            );
            let rpm = FakeRpm::new("unused", None);
            let mut effects = crate::test_support::RawEffectRecorder::default();
            effects.fail_service = fail;
            effects
                .with(|f| {
                    application::run_with_dependencies(
                        application::ApplicationRequest {
                            component: "skillfs",
                            intent: anolisa_core::execution::ExecutionIntent::Plan,
                        },
                        &c,
                        &rpm,
                        &rpm,
                        false,
                        f,
                    )
                })
                .expect("repair preview");
            assert_eq!(effects.calls(), ["capability factory system"]);
            assert!(effects.capability_contents().is_empty());
            assert!(!binary.exists());
            let outcome = effects
                .with(|f| {
                    application::run_with_dependencies(
                        application::ApplicationRequest {
                            component: "skillfs",
                            intent: execution_intent(&c),
                        },
                        &c,
                        &rpm,
                        &rpm,
                        false,
                        f,
                    )
                })
                .expect("repair succeeds with non-terminal service warnings");
            assert_eq!(
                effects.calls(),
                vec![
                    "capability factory system".to_string(),
                    "capability factory system".to_string(),
                    "capability factory system".to_string(),
                    format!("apply {} CAP_BPF", binary.display()),
                    "service factory system System".to_string(),
                    "DaemonReload System ".to_string(),
                    "Enable System skillfs.service".to_string(),
                    "Restart System skillfs.service".to_string(),
                ]
            );
            assert_eq!(fs::read(&binary).expect("binary"), b"repaired binary\n");
            assert!(
                StateStore::load(&state_path, 0)
                    .expect("state")
                    .find(ObjectKind::Component, "skillfs")
                    .is_some()
            );
            render_application_outcome(&c, outcome).expect("existing renderer");
        }
    }
}
