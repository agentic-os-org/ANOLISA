//! Application orchestration for `anolisa forget`.

use chrono::{SecondsFormat, Utc};

use anolisa_core::central_log::{CentralLog, LogKind, LogRecord, LogStatus, Severity};
use anolisa_core::domain::ProviderBinding;
use anolisa_core::execution::{CommandOutcome, CommandOutcomeStatus, ExecutionIntent};
use anolisa_core::facts::{JournalEvidence, pending_journal_for};
use anolisa_core::lock::InstallLock;
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_store::StateStore;
use anolisa_platform::privilege;

use crate::commands::common;
use crate::commands::tier1::rpm_install;
use crate::context::CliContext;
use crate::response::CliError;

use super::COMMAND;

/// Typed input for one forget request.
pub(super) struct ForgetRequest<'a> {
    pub(super) component: &'a str,
    pub(super) intent: ExecutionIntent,
}

/// Resolved component facts carried to the command renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ForgetSubject {
    pub(super) component: String,
    pub(super) provenance: &'static str,
    pub(super) install_mode: String,
}

/// Durable state change made by an applied forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ForgetChange {
    /// The component record and its local manifest snapshots were dropped.
    StateRecordDropped,
}

/// Typed application result consumed by the compatibility renderer.
#[derive(Debug)]
pub(super) enum ForgetApplicationOutcome {
    /// Plan-only result; no install lock or persistent mutation was performed.
    Preview { subject: ForgetSubject },
    /// Applied result with durable operation evidence.
    Applied {
        subject: ForgetSubject,
        outcome: CommandOutcome<ForgetChange>,
    },
}

/// Resolve, validate, and optionally apply one forget request.
pub(super) fn run(
    request: ForgetRequest<'_>,
    ctx: &CliContext,
) -> Result<ForgetApplicationOutcome, CliError> {
    let command = format!("{COMMAND} {}", request.component);
    let layout = common::resolve_layout(ctx);
    let (resolved, view) = common::resolve_mutation_target(request.component, ctx, &command)?;
    let store = view.writable.state;
    let target = resolved.as_str();

    let journal_dir = rpm_install::journal_dir(&layout);
    ensure_no_pending_journal(
        JournalEvidence::new(&journal_dir, &store.operations),
        target,
        &command,
    )?;

    // Forget also resolves quarantined records: it is the documented exit for
    // legacy state the migration refused to classify and repair cannot recover.
    let provenance = record_provenance(&store, target).ok_or_else(|| CliError::NotInstalled {
        command: command.clone(),
        reason: format!(
            "component '{target}' is not installed — nothing to forget (run `anolisa status` to see what is tracked)"
        ),
    })?;

    // A successful preview must prove the same adapter precondition that apply
    // re-checks under the lock; otherwise it would advertise work that cannot run.
    ensure_no_adapter_claims(&store, target, &command)?;

    if matches!(request.intent, ExecutionIntent::Plan) {
        return Ok(ForgetApplicationOutcome::Preview {
            subject: ForgetSubject {
                component: resolved,
                provenance,
                install_mode: ctx.install_mode.as_str().to_string(),
            },
        });
    }

    let (outcome, provenance) = persist_forget(ctx, target, &command)?;
    Ok(ForgetApplicationOutcome::Applied {
        subject: ForgetSubject {
            component: resolved,
            provenance,
            install_mode: ctx.install_mode.as_str().to_string(),
        },
        outcome,
    })
}

/// Remove a component record and snapshots under the authoritative lock.
pub(super) fn persist_forget(
    ctx: &CliContext,
    component: &str,
    command: &str,
) -> Result<(CommandOutcome<ForgetChange>, &'static str), CliError> {
    let layout = common::resolve_layout(ctx);
    let state_path = layout.state_dir.join("installed.toml");
    let _lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(&state_path, privilege::effective_uid(), &layout)
        .map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to load installed state: {err}"),
    })?;

    // A surviving journal could later recreate the record removed here.
    let journal_dir = rpm_install::journal_dir(&layout);
    ensure_no_pending_journal(
        JournalEvidence::new(&journal_dir, &store.operations),
        component,
        command,
    )?;

    // Re-check after locking so a concurrent adapter enable cannot be orphaned.
    ensure_no_adapter_claims(&store, component, command)?;

    // Report the authority observed under the lock, not the preview read.
    let provenance = record_provenance(&store, component).ok_or_else(|| CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "component '{component}' disappeared from state during forget; nothing removed"
        ),
    })?;
    let legacy_manifest_dir = store
        .find(ObjectKind::Component, component)
        .map(|installation| {
            common::legacy_component_manifest_dir_for_installation(&layout, installation, command)
        })
        .transpose()?
        .flatten();
    store.remove(ObjectKind::Component, component);
    remove_component_manifest_snapshot(&layout, component, command)?;
    if let Some(dir) = legacy_manifest_dir {
        remove_manifest_snapshot_dir(&dir, command)?;
    }

    let now = now_iso8601();
    let lock_ts = Utc::now();
    let operation_id = format!(
        "op-forget-{}-{}",
        lock_ts.format("%Y%m%d%H%M%S"),
        lock_ts.timestamp_subsec_nanos()
    );
    store.operations.push(OperationRecord {
        id: operation_id.clone(),
        command: command.to_string(),
        status: "ok".to_string(),
        started_at: now.clone(),
        finished_at: Some(now.clone()),
        parent_operation_id: None,
    });
    store.save(&state_path).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to save state: {err}"),
    })?;

    let log = CentralLog::open(layout.central_log);
    let record = LogRecord {
        kind: LogKind::Operation,
        operation_id: Some(operation_id.clone()),
        command: command.to_string(),
        source: "anolisa-cli".to_string(),
        component: Some(component.to_string()),
        severity: Severity::Info,
        message: format!(
            "forgot ANOLISA state for component {component}; no package operation performed"
        ),
        actor: "cli".to_string(),
        install_mode: Some(ctx.install_mode.as_str().to_string()),
        started_at: now.clone(),
        finished_at: Some(now),
        status: Some(LogStatus::Ok),
        objects: vec![component.to_string()],
        backup_ids: Vec::new(),
        warnings: Vec::new(),
        details: serde_json::Value::Null,
    };
    let warnings = log
        .append(&record)
        .err()
        .map(|err| format!("failed to write central log: {err}"))
        .into_iter()
        .collect();

    Ok((
        CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            Some(operation_id),
            vec![ForgetChange::StateRecordDropped],
            warnings,
        ),
        provenance,
    ))
}

/// Provenance label for an active or quarantined component record.
fn record_provenance(store: &StateStore, component: &str) -> Option<&'static str> {
    if let Some(installation) = store.find(ObjectKind::Component, component) {
        return Some(match &installation.binding {
            ProviderBinding::Owned { .. } => "owned",
            ProviderBinding::Delegated { relation, .. } => relation.label(),
        });
    }
    store
        .quarantined
        .iter()
        .any(|entry| entry.record.kind == ObjectKind::Component && entry.record.name == component)
        .then_some("quarantined")
}

/// Refuse to drop a component that still has enabled adapter receipts.
fn ensure_no_adapter_claims(
    store: &StateStore,
    target: &str,
    command: &str,
) -> Result<(), CliError> {
    let mut frameworks: Vec<&str> = store
        .adapter_claims
        .iter()
        .filter(|claim| claim.component == target)
        .map(|claim| claim.framework.as_str())
        .collect();
    if frameworks.is_empty() {
        return Ok(());
    }
    frameworks.sort_unstable();
    frameworks.dedup();
    Err(CliError::InvalidArgument {
        command: command.to_string(),
        reason: format!(
            "'{target}' has enabled adapters ({}); run `anolisa adapter disable {target}` for each framework before forgetting",
            frameworks.join(", ")
        ),
    })
}

fn ensure_no_pending_journal(
    evidence: JournalEvidence<'_>,
    component: &str,
    command: &str,
) -> Result<(), CliError> {
    let pending = pending_journal_for(evidence, component).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to inspect operation journals: {err}"),
    })?;
    if let Some(path) = pending {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "component '{component}' has a pending operation journal at {}; run `anolisa repair {component}` before forgetting its state",
                path.display()
            ),
        });
    }
    Ok(())
}

fn remove_component_manifest_snapshot(
    layout: &anolisa_platform::fs_layout::FsLayout,
    component: &str,
    command: &str,
) -> Result<(), CliError> {
    let dir = common::installed_component_manifest_dir(layout, component, command)?;
    remove_manifest_snapshot_dir(&dir, command)
}

fn remove_manifest_snapshot_dir(dir: &std::path::Path, command: &str) -> Result<(), CliError> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "failed to remove component manifest snapshot at {}: {err}",
                dir.display()
            ),
        }),
    }
}

fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
