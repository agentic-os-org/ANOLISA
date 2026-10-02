//! Compensating executor for the owned step family.
//!
//! Interprets the owned subset of the planner's [`Step`] vocabulary —
//! download, provisioning, hooks, file placement, capabilities, services,
//! backup/removal, record writes — against an [`OwnedOps`] port, journaling
//! every step. The failure semantics are the opposite of the delegated
//! executor's on purpose: ANOLISA is the authority over owned artifacts, so
//! a failed step *unwinds* the side effects already committed (remove placed
//! files, stop activated services, restore the backup) instead of leaving
//! them for a re-observe.
//!
//! The executor owns the compensation policy — which steps register an undo,
//! in which order undos run, and what terminal journal status each failure
//! shape earns. The port only supplies the side effects and their undo
//! primitives; it holds whatever working state they need (placed files,
//! activated units, backup location) internally.
//!
//! Delegated-family steps ([`Step::NativeTransaction`], [`Step::Observe`])
//! and [`Step::RecoverJournal`] are rejected up front as
//! [`OwnedExecutionError::UnsupportedStep`].

use thiserror::Error;

use crate::planner::{HookKind, RecordWrite, Step};
use crate::transaction::{
    Transaction, TransactionError, TransactionOutcomeStatus, TransactionStep,
};

/// Journal phase label for artifact download and digest verification.
pub const PHASE_DOWNLOAD: &str = "owned-download";
/// Journal phase label for runtime-dependency provisioning.
pub const PHASE_PROVISION: &str = "owned-provision";
/// Journal phase label for contract hooks.
pub const PHASE_HOOK: &str = "owned-hook";
/// Journal phase label for file backup/placement/removal and capabilities.
pub const PHASE_FILES: &str = "owned-files";
/// Journal phase label for service activation changes.
pub const PHASE_SERVICES: &str = "owned-services";
/// Journal phase label for state-record commits.
pub const PHASE_RECORD: &str = "owned-record";

/// A step's successful result: side effect done, plus non-fatal findings to
/// surface to the user (failed optional capability, unit that refused to
/// start, …).
#[derive(Debug, Default)]
pub struct StepSuccess {
    /// Non-fatal findings collected while running the step.
    pub warnings: Vec<String>,
}

impl StepSuccess {
    /// A clean success with nothing to report.
    pub fn clean() -> Self {
        Self::default()
    }

    /// A success that carries warnings.
    pub fn with_warnings(warnings: Vec<String>) -> Self {
        Self { warnings }
    }
}

/// Hard failure of one owned step. Soft failures are [`StepSuccess`]
/// warnings; an `Err` always triggers compensation.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct OwnedOpError(pub String);

/// Side-effect port for the owned step family.
///
/// One method per planner step plus the undo primitives compensation needs.
/// Implementations keep their own working state across calls (resolved
/// artifact, placed files, activated units, backup set) — the executor only
/// sequences them. Undo methods are best-effort: they return warnings
/// instead of failing, because they already run on a failure path.
pub trait OwnedOps {
    /// Fetch the artifact and verify its digest ([`Step::DownloadVerify`]).
    /// Must be side-effect free outside the download cache.
    fn download_verify(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Install missing runtime dependencies ([`Step::ProvisionRuntimeDeps`]).
    /// Provisioned system packages are intentionally never rolled back.
    fn provision_runtime_deps(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Run the contract hooks for `kind` ([`Step::RunHook`]). A strict hook
    /// failure is an `Err`; non-strict failures degrade to warnings.
    fn run_hook(&mut self, kind: HookKind) -> Result<StepSuccess, OwnedOpError>;

    /// Back up owned files before a destructive step ([`Step::BackupFiles`]).
    fn backup_files(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Place owned files on disk ([`Step::PlaceFiles`]).
    fn place_files(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Apply declared file capabilities ([`Step::SetCapabilities`]). A
    /// required capability failure is an `Err`; optional ones are warnings.
    fn set_capabilities(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Enable and start recorded services ([`Step::EnableServices`]).
    /// Activation trouble is conventionally a warning, not an `Err` — files
    /// are installed and an operator can fix a unit out of band.
    fn enable_services(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Restart recorded services ([`Step::RestartServices`]).
    fn restart_services(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Stop recorded services before teardown ([`Step::StopServices`]).
    fn stop_services(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Remove ANOLISA-owned files ([`Step::RemoveOwnedFiles`]).
    fn remove_owned_files(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Persist the installation record ([`Step::WriteRecord`]).
    ///
    /// Lives on the port — not on a separate sink — because an owned record
    /// is built from this run's execution state (the placed files and their
    /// digests, the services actually activated), which only the port holds.
    fn write_record(&mut self, write: RecordWrite) -> Result<StepSuccess, OwnedOpError>;

    /// Remove the installation record ([`Step::DropRecord`]).
    fn drop_record(&mut self) -> Result<StepSuccess, OwnedOpError>;

    /// Undo a completed [`place_files`](Self::place_files): remove what this
    /// run placed. Best-effort; returns cleanup warnings.
    fn undo_place_files(&mut self) -> Vec<String>;

    /// Undo a completed [`enable_services`](Self::enable_services): stop and
    /// disable the units this run activated. Best-effort.
    fn undo_enable_services(&mut self) -> Vec<String>;

    /// Restore the file backup taken by [`backup_files`](Self::backup_files).
    /// Best-effort; runs after `undo_place_files` so the restored tree is
    /// not overwritten by leftovers.
    fn restore_backup(&mut self) -> Vec<String>;
}

/// What execution left behind on success.
#[derive(Debug, Default)]
pub struct OwnedExecutionOutcome {
    /// Warnings aggregated across all steps, in execution order.
    pub warnings: Vec<String>,
}

/// How execution of an owned plan failed.
#[derive(Debug, Error)]
pub enum OwnedExecutionError {
    /// The plan contains a step this executor does not interpret (delegated
    /// family or journal recovery). Rejected before any side effect runs.
    #[error("owned executor cannot run step {step:?}")]
    UnsupportedStep {
        /// The offending step.
        step: Step,
    },
    /// A step failed and the side effects committed so far were compensated
    /// (or there were none). `rolled_back` says whether compensation ran;
    /// `rollback_warnings` carries best-effort cleanup trouble.
    #[error("step {step:?} failed: {source}")]
    StepFailed {
        /// The step that failed.
        step: Step,
        /// The port's failure.
        source: OwnedOpError,
        /// Whether any compensation ran for prior side effects.
        rolled_back: bool,
        /// Best-effort cleanup trouble from the undo primitives.
        rollback_warnings: Vec<String>,
        /// Warnings aggregated from the steps that succeeded before the
        /// failure — still worth surfacing.
        warnings: Vec<String>,
    },
    /// The journal itself could not be persisted.
    #[error(transparent)]
    Journal(#[from] TransactionError),
    /// Journal persistence failed after execution had begun. Compensation is
    /// still attempted, and every diagnostic from that attempt is retained.
    #[error("{detail}")]
    RecoveryUncertain {
        /// Complete diagnostic rendered by CLI callers.
        detail: String,
        /// The journal failure that entered this recovery path.
        #[source]
        journal_source: TransactionError,
        /// Whether at least one registered compensation ran.
        rolled_back: bool,
        /// Best-effort cleanup trouble from undo primitives.
        rollback_warnings: Vec<String>,
        /// A second journal failure while recording the compensation result.
        recovery_journal_error: Option<TransactionError>,
    },
}

/// Compensation registered by a completed step, replayed in reverse order.
#[derive(Debug, Clone, Copy)]
enum Compensation {
    /// Remove the files placed this run (from [`Step::PlaceFiles`]).
    UndoPlaceFiles,
    /// Stop/disable the units activated this run
    /// (from [`Step::EnableServices`]).
    UndoEnableServices,
    /// Put the backup back (from [`Step::BackupFiles`]); ordered after
    /// `UndoPlaceFiles` by the reverse replay, so the restored tree wins.
    RestoreBackup,
    RestoreState,
}

/// Execute the owned steps of a plan in order, journaling each one and
/// unwinding committed side effects when a step fails.
///
/// `journal` must already be begun by the caller; the executor appends its
/// steps after whatever the journal already holds. On failure the journal
/// finishes as:
///
/// - `RolledBack` — compensation ran and reported no trouble;
/// - `Partial` — compensation ran with warnings, or an uncompensatable side
///   effect (files removed without a backup, services stopped) was already
///   committed;
/// - `Failed` — nothing had touched the host yet.
pub fn execute_owned_steps(
    steps: &[Step],
    ops: &mut dyn OwnedOps,
    journal: &mut Transaction,
) -> Result<OwnedExecutionOutcome, OwnedExecutionError> {
    // Reject foreign steps before any side effect or journal write.
    if let Some(step) = steps.iter().find(|step| !is_owned_step(step)) {
        return Err(OwnedExecutionError::UnsupportedStep { step: step.clone() });
    }

    let base = journal.steps.len();
    journal.record_steps(steps.iter().map(journal_step))?;

    let mut warnings: Vec<String> = Vec::new();
    // (journal idx, undo) per compensatable completed step, unwound in
    // reverse on failure.
    let mut compensations: Vec<(usize, Compensation)> = Vec::new();
    // Set once a completed step's side effect cannot be undone (removed
    // files without a backup, stopped services): a later failure can then
    // never claim a clean rollback.
    let mut irreversible_side_effect = false;

    for (offset, step) in steps.iter().enumerate() {
        let idx = base + offset;
        let result: Result<StepSuccess, OwnedOpError> = match step {
            Step::DownloadVerify => ops.download_verify(),
            Step::ProvisionRuntimeDeps => ops.provision_runtime_deps(),
            Step::RunHook(kind) => ops.run_hook(*kind),
            Step::BackupFiles => ops.backup_files(),
            Step::PlaceFiles => ops.place_files(),
            Step::SetCapabilities => ops.set_capabilities(),
            Step::EnableServices => ops.enable_services(),
            Step::RestartServices => ops.restart_services(),
            Step::StopServices => ops.stop_services(),
            Step::RemoveOwnedFiles => ops.remove_owned_files(),
            Step::WriteRecord(write) => ops.write_record(*write),
            Step::DropRecord => ops.drop_record(),
            // Unreachable: the pre-validation pass rejected foreign steps.
            other => {
                return Err(OwnedExecutionError::UnsupportedStep {
                    step: other.clone(),
                });
            }
        };

        match result {
            Ok(success) => {
                warnings.extend(success.warnings);
                register_compensation(step, idx, &mut compensations, &mut irreversible_side_effect);
                if let Err(source) = journal.mark_done(idx) {
                    let report = compensate(ops, journal, &compensations, irreversible_side_effect);
                    return Err(recovery_uncertain(
                        format!("persisting completion of {step:?} failed"),
                        source,
                        report,
                    ));
                }
            }
            Err(source) => {
                if let Err(journal_error) = journal.mark_failed(idx, &source.to_string()) {
                    let report = compensate(ops, journal, &compensations, irreversible_side_effect);
                    return Err(recovery_uncertain(
                        format!(
                            "step {step:?} failed ({source}) and its failure could not be persisted"
                        ),
                        journal_error,
                        report,
                    ));
                }
                let mut report = compensate(ops, journal, &compensations, irreversible_side_effect);
                if let Some(journal_error) = report.journal_error.take() {
                    return Err(recovery_uncertain(
                        format!(
                            "step {step:?} failed ({source}) and its compensation outcome could not be persisted"
                        ),
                        journal_error,
                        report,
                    ));
                }
                return Err(OwnedExecutionError::StepFailed {
                    step: step.clone(),
                    source,
                    rolled_back: report.rolled_back,
                    rollback_warnings: report.warnings,
                    warnings,
                });
            }
        }
    }

    journal.finish(TransactionOutcomeStatus::Ok)?;
    Ok(OwnedExecutionOutcome { warnings })
}

fn register_compensation(
    step: &Step,
    idx: usize,
    compensations: &mut Vec<(usize, Compensation)>,
    irreversible_side_effect: &mut bool,
) {
    match step {
        Step::PlaceFiles => compensations.push((idx, Compensation::UndoPlaceFiles)),
        Step::EnableServices => compensations.push((idx, Compensation::UndoEnableServices)),
        Step::BackupFiles => compensations.push((idx, Compensation::RestoreBackup)),
        Step::WriteRecord(_) | Step::DropRecord => {
            compensations.push((idx, Compensation::RestoreState));
        }
        Step::RemoveOwnedFiles => {
            let has_backup = compensations
                .iter()
                .any(|(_, compensation)| matches!(compensation, Compensation::RestoreBackup));
            if !has_backup {
                *irreversible_side_effect = true;
            }
        }
        Step::StopServices | Step::RestartServices => {
            *irreversible_side_effect = true;
        }
        _ => {}
    }
}

struct CompensationReport {
    rolled_back: bool,
    warnings: Vec<String>,
    journal_error: Option<TransactionError>,
}

fn compensate(
    ops: &mut dyn OwnedOps,
    journal: &mut Transaction,
    compensations: &[(usize, Compensation)],
    irreversible_side_effect: bool,
) -> CompensationReport {
    if compensations.is_empty() {
        // Nothing to unwind. If an uncompensatable side effect already
        // committed, the host no longer matches the record: Partial. If the
        // failure struck before anything touched the host: a clean Failed.
        let journal_error = journal
            .finish(if irreversible_side_effect {
                TransactionOutcomeStatus::Partial
            } else {
                TransactionOutcomeStatus::Failed
            })
            .err();
        return CompensationReport {
            rolled_back: false,
            warnings: Vec::new(),
            journal_error,
        };
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut first_journal_error: Option<TransactionError> = None;
    for (idx, compensation) in compensations.iter().rev() {
        let undo_warnings = match compensation {
            Compensation::UndoPlaceFiles => ops.undo_place_files(),
            Compensation::UndoEnableServices => ops.undo_enable_services(),
            Compensation::RestoreBackup => ops.restore_backup(),
            Compensation::RestoreState => journal
                .restore_state()
                .err()
                .map(|err| vec![err.to_string()])
                .unwrap_or_default(),
        };
        warnings.extend(undo_warnings);
        if let Err(err) = journal.mark_rolled_back(*idx)
            && first_journal_error.is_none()
        {
            first_journal_error = Some(err);
        }
    }

    // Cleanup trouble or an uncompensatable side effect means the unwind
    // cannot claim the host is back to its pre-plan state.
    let status = if warnings.is_empty() && !irreversible_side_effect {
        TransactionOutcomeStatus::RolledBack
    } else {
        TransactionOutcomeStatus::Partial
    };
    if let Err(err) = journal.finish(status)
        && first_journal_error.is_none()
    {
        first_journal_error = Some(err);
    }
    CompensationReport {
        rolled_back: true,
        warnings,
        journal_error: first_journal_error,
    }
}

fn recovery_uncertain(
    cause: String,
    journal_source: TransactionError,
    report: CompensationReport,
) -> OwnedExecutionError {
    let cleanup = if report.rolled_back && report.warnings.is_empty() {
        "host compensation completed".to_string()
    } else if report.rolled_back {
        format!(
            "host compensation reported problems ({})",
            report.warnings.join("; ")
        )
    } else {
        "no compensatable host changes were registered".to_string()
    };
    let recovery_journal_detail = report
        .journal_error
        .as_ref()
        .map(|err| format!("; recording the compensation outcome also failed ({err})"))
        .unwrap_or_default();
    let detail = format!(
        "{cause}: {journal_source}; {cleanup}{recovery_journal_detail}; host recovery is uncertain"
    );
    OwnedExecutionError::RecoveryUncertain {
        detail,
        journal_source,
        rolled_back: report.rolled_back,
        rollback_warnings: report.warnings,
        recovery_journal_error: report.journal_error,
    }
}

/// Whether this executor interprets `step`.
fn is_owned_step(step: &Step) -> bool {
    matches!(
        step,
        Step::DownloadVerify
            | Step::ProvisionRuntimeDeps
            | Step::RunHook(_)
            | Step::BackupFiles
            | Step::PlaceFiles
            | Step::SetCapabilities
            | Step::EnableServices
            | Step::RestartServices
            | Step::StopServices
            | Step::RemoveOwnedFiles
            | Step::WriteRecord(_)
            | Step::DropRecord
    )
}

/// Journal entry for an owned step, initialised to `Planned`.
fn journal_step(step: &Step) -> TransactionStep {
    let (phase, target, action) = match step {
        Step::DownloadVerify => (PHASE_DOWNLOAD, "artifact", "download-verify"),
        Step::ProvisionRuntimeDeps => (PHASE_PROVISION, "runtime-deps", "provision"),
        Step::RunHook(kind) => (
            PHASE_HOOK,
            "hooks",
            match kind {
                HookKind::PreInstall => "pre-install",
                HookKind::PostInstall => "post-install",
                HookKind::PreUninstall => "pre-uninstall",
                HookKind::PostUninstall => "post-uninstall",
            },
        ),
        Step::BackupFiles => (PHASE_FILES, "owned-files", "backup"),
        Step::PlaceFiles => (PHASE_FILES, "owned-files", "place"),
        Step::SetCapabilities => (PHASE_FILES, "owned-files", "set-capabilities"),
        Step::EnableServices => (PHASE_SERVICES, "services", "enable"),
        Step::RestartServices => (PHASE_SERVICES, "services", "restart"),
        Step::StopServices => (PHASE_SERVICES, "services", "stop"),
        Step::RemoveOwnedFiles => (PHASE_FILES, "owned-files", "remove"),
        Step::WriteRecord(write) => (PHASE_RECORD, "state", write.label()),
        Step::DropRecord => (PHASE_RECORD, "state", "drop-record"),
        // Foreign steps never reach journaling; keep the label honest.
        _ => ("unsupported", "unsupported", "unsupported"),
    };
    TransactionStep::planned(phase, target, action, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NativePm;
    use crate::planner::{NativeAction, RecordWrite};
    use crate::transaction::TransactionStepStatus;

    /// Scripted port: records the call sequence, fails on demand, and
    /// reports undo invocations.
    #[derive(Default)]
    struct FakeOps {
        calls: Vec<String>,
        fail_on: Option<&'static str>,
        warn_on: Option<&'static str>,
        undo_warnings: Vec<String>,
        break_journal_on: Option<&'static str>,
        journal_path: Option<std::path::PathBuf>,
    }

    impl FakeOps {
        fn failing(step: &'static str) -> Self {
            Self {
                fail_on: Some(step),
                ..Self::default()
            }
        }

        fn run(&mut self, name: &str) -> Result<StepSuccess, OwnedOpError> {
            self.calls.push(name.to_string());
            if self.break_journal_on == Some(name)
                && let Some(path) = &self.journal_path
            {
                std::fs::remove_file(path).expect("remove journal");
                std::fs::create_dir(path).expect("block journal persistence");
            }
            if self.fail_on == Some(name) {
                return Err(OwnedOpError(format!("{name} exploded")));
            }
            if self.warn_on == Some(name) {
                return Ok(StepSuccess::with_warnings(vec![format!("{name} warned")]));
            }
            Ok(StepSuccess::clean())
        }

        fn undo(&mut self, name: &'static str) -> Vec<String> {
            self.calls.push(name.to_string());
            self.undo_warnings.clone()
        }
    }

    impl OwnedOps for FakeOps {
        fn download_verify(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("download_verify")
        }
        fn provision_runtime_deps(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("provision_runtime_deps")
        }
        fn run_hook(&mut self, kind: HookKind) -> Result<StepSuccess, OwnedOpError> {
            match kind {
                HookKind::PreInstall => self.run("hook_pre_install"),
                HookKind::PostInstall => self.run("hook_post_install"),
                HookKind::PreUninstall => self.run("hook_pre_uninstall"),
                HookKind::PostUninstall => self.run("hook_post_uninstall"),
            }
        }
        fn backup_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("backup_files")
        }
        fn place_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("place_files")
        }
        fn set_capabilities(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("set_capabilities")
        }
        fn enable_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("enable_services")
        }
        fn restart_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("restart_services")
        }
        fn stop_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("stop_services")
        }
        fn remove_owned_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("remove_owned_files")
        }
        fn write_record(&mut self, write: RecordWrite) -> Result<StepSuccess, OwnedOpError> {
            self.run(&format!("write_record:{}", write.label()))
        }
        fn drop_record(&mut self) -> Result<StepSuccess, OwnedOpError> {
            self.run("drop_record")
        }
        fn undo_place_files(&mut self) -> Vec<String> {
            self.undo("undo_place_files")
        }
        fn undo_enable_services(&mut self) -> Vec<String> {
            self.undo("undo_enable_services")
        }
        fn restore_backup(&mut self) -> Vec<String> {
            self.undo("restore_backup")
        }
    }

    fn journal(dir: &std::path::Path) -> Transaction {
        Transaction::begin("install", dir.join("installed.toml"), dir).expect("begin journal")
    }

    /// I1: fresh owned install.
    fn install_steps() -> Vec<Step> {
        vec![
            Step::DownloadVerify,
            Step::ProvisionRuntimeDeps,
            Step::RunHook(HookKind::PreInstall),
            Step::PlaceFiles,
            Step::SetCapabilities,
            Step::RunHook(HookKind::PostInstall),
            Step::EnableServices,
            Step::WriteRecord(RecordWrite::Owned),
        ]
    }

    /// U3/RI2/R2: replay at the recorded/target version.
    fn replay_steps() -> Vec<Step> {
        vec![
            Step::BackupFiles,
            Step::DownloadVerify,
            Step::RemoveOwnedFiles,
            Step::PlaceFiles,
            Step::SetCapabilities,
            Step::RestartServices,
            Step::WriteRecord(RecordWrite::Owned),
        ]
    }

    /// X1: owned uninstall.
    fn uninstall_steps() -> Vec<Step> {
        vec![
            Step::RunHook(HookKind::PreUninstall),
            Step::StopServices,
            Step::RemoveOwnedFiles,
            Step::RunHook(HookKind::PostUninstall),
            Step::DropRecord,
        ]
    }

    #[test]
    fn owned_install_happy_path_runs_all_steps_in_order() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps::default();
        let mut journal = journal(tmp.path());

        let outcome =
            execute_owned_steps(&install_steps(), &mut ops, &mut journal).expect("execution ok");

        assert_eq!(
            ops.calls,
            vec![
                "download_verify",
                "provision_runtime_deps",
                "hook_pre_install",
                "place_files",
                "set_capabilities",
                "hook_post_install",
                "enable_services",
                "write_record:write-owned",
            ]
        );
        assert!(outcome.warnings.is_empty());
        assert_eq!(journal.status, TransactionOutcomeStatus::Ok);
        assert_eq!(journal.steps.len(), 8);
        assert!(
            journal
                .steps
                .iter()
                .all(|s| s.status == TransactionStepStatus::Done)
        );
    }

    #[test]
    fn step_warnings_aggregate_into_the_outcome() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps {
            warn_on: Some("enable_services"),
            ..FakeOps::default()
        };
        let mut journal = journal(tmp.path());

        let outcome =
            execute_owned_steps(&install_steps(), &mut ops, &mut journal).expect("execution ok");

        assert_eq!(outcome.warnings, vec!["enable_services warned".to_string()]);
        assert_eq!(journal.status, TransactionOutcomeStatus::Ok);
    }

    #[test]
    fn capability_failure_unwinds_placed_files() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps::failing("set_capabilities");
        let mut journal = journal(tmp.path());

        let err = execute_owned_steps(&install_steps(), &mut ops, &mut journal).unwrap_err();

        match err {
            OwnedExecutionError::StepFailed {
                step,
                rolled_back,
                rollback_warnings,
                ..
            } => {
                assert_eq!(step, Step::SetCapabilities);
                assert!(rolled_back);
                assert!(rollback_warnings.is_empty());
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
        assert_eq!(
            ops.calls.last().map(String::as_str),
            Some("undo_place_files")
        );
        // Services were never enabled, so no service undo runs — and the
        // record commit was never reached.
        assert!(!ops.calls.iter().any(|c| c == "undo_enable_services"));
        assert!(!ops.calls.iter().any(|c| c.starts_with("write_record")));
        assert_eq!(journal.status, TransactionOutcomeStatus::RolledBack);
        // PlaceFiles carries the rolled-back marker; SetCapabilities failed.
        assert_eq!(journal.steps[3].status, TransactionStepStatus::RolledBack);
        assert_eq!(journal.steps[4].status, TransactionStepStatus::Failed);
        // EnableServices and WriteRecord never ran.
        assert_eq!(journal.steps[6].status, TransactionStepStatus::Planned);
    }

    #[test]
    fn record_commit_failure_unwinds_services_then_files() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps::failing("write_record:write-owned");
        let mut journal = journal(tmp.path());

        let err = execute_owned_steps(&install_steps(), &mut ops, &mut journal).unwrap_err();

        assert!(matches!(
            err,
            OwnedExecutionError::StepFailed {
                step: Step::WriteRecord(RecordWrite::Owned),
                rolled_back: true,
                ..
            }
        ));
        // Reverse order: services activated last are undone first.
        let undo_calls: Vec<&str> = ops
            .calls
            .iter()
            .filter(|c| c.starts_with("undo") || c.as_str() == "restore_backup")
            .map(String::as_str)
            .collect();
        assert_eq!(undo_calls, vec!["undo_enable_services", "undo_place_files"]);
        assert_eq!(journal.status, TransactionOutcomeStatus::RolledBack);
    }

    #[test]
    fn mark_done_failure_unwinds_the_completed_step_and_prior_effects() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut journal = journal(tmp.path());
        let mut ops = FakeOps {
            break_journal_on: Some("enable_services"),
            journal_path: Some(journal.journal_path.clone()),
            undo_warnings: vec!["service cleanup remained uncertain".to_string()],
            ..FakeOps::default()
        };

        let err = execute_owned_steps(
            &[Step::PlaceFiles, Step::EnableServices],
            &mut ops,
            &mut journal,
        )
        .expect_err("journal persistence must fail");

        let detail = err.to_string();
        assert!(
            detail.contains("service cleanup remained uncertain"),
            "{detail}"
        );
        assert!(detail.contains("journal"), "{detail}");
        assert_eq!(
            ops.calls,
            vec![
                "place_files",
                "enable_services",
                "undo_enable_services",
                "undo_place_files",
            ]
        );
    }

    #[test]
    fn mark_failed_persistence_failure_still_unwinds_prior_effects() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut journal = journal(tmp.path());
        let mut ops = FakeOps {
            fail_on: Some("set_capabilities"),
            break_journal_on: Some("set_capabilities"),
            journal_path: Some(journal.journal_path.clone()),
            ..FakeOps::default()
        };

        let err = execute_owned_steps(
            &[Step::PlaceFiles, Step::SetCapabilities],
            &mut ops,
            &mut journal,
        )
        .expect_err("failed-step journaling must fail");

        assert!(matches!(err, OwnedExecutionError::RecoveryUncertain { .. }));
        assert!(err.to_string().contains("SetCapabilities"));
        assert_eq!(
            ops.calls,
            vec!["place_files", "set_capabilities", "undo_place_files"]
        );
    }

    #[test]
    fn replay_failure_after_removal_restores_the_backup_last() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps::failing("set_capabilities");
        let mut journal = journal(tmp.path());

        let err = execute_owned_steps(&replay_steps(), &mut ops, &mut journal).unwrap_err();

        assert!(matches!(err, OwnedExecutionError::StepFailed { .. }));
        // New files come off first, then the backup goes back, so the
        // restored tree is not clobbered by this run's leftovers.
        let undo_calls: Vec<&str> = ops
            .calls
            .iter()
            .filter(|c| c.starts_with("undo") || c.as_str() == "restore_backup")
            .map(String::as_str)
            .collect();
        assert_eq!(undo_calls, vec!["undo_place_files", "restore_backup"]);
        assert_eq!(journal.status, TransactionOutcomeStatus::RolledBack);
    }

    #[test]
    fn pre_side_effect_failure_is_a_clean_failed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        // The pre-install hook fails: provisioning ran (retained by policy)
        // but no files or services were touched — nothing to unwind.
        let mut ops = FakeOps::failing("hook_pre_install");
        let mut journal = journal(tmp.path());

        let err = execute_owned_steps(&install_steps(), &mut ops, &mut journal).unwrap_err();

        match err {
            OwnedExecutionError::StepFailed {
                rolled_back,
                rollback_warnings,
                ..
            } => {
                assert!(!rolled_back);
                assert!(rollback_warnings.is_empty());
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
        assert!(!ops.calls.iter().any(|c| c.starts_with("undo")));
        assert_eq!(journal.status, TransactionOutcomeStatus::Failed);
    }

    #[test]
    fn owned_uninstall_happy_path() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps::default();
        let mut journal = journal(tmp.path());

        execute_owned_steps(&uninstall_steps(), &mut ops, &mut journal).expect("execution ok");

        assert_eq!(
            ops.calls,
            vec![
                "hook_pre_uninstall",
                "stop_services",
                "remove_owned_files",
                "hook_post_uninstall",
                "drop_record",
            ]
        );
        assert_eq!(journal.status, TransactionOutcomeStatus::Ok);
    }

    #[test]
    fn uninstall_removal_failure_is_partial_not_rolled_back() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        // X1 has no BackupFiles step: services are already stopped and
        // there is nothing to restore — the honest status is Partial.
        let mut ops = FakeOps::failing("remove_owned_files");
        let mut journal = journal(tmp.path());

        let err = execute_owned_steps(&uninstall_steps(), &mut ops, &mut journal).unwrap_err();

        match err {
            OwnedExecutionError::StepFailed { rolled_back, .. } => assert!(!rolled_back),
            other => panic!("expected StepFailed, got {other:?}"),
        }
        assert!(!ops.calls.iter().any(|c| c.starts_with("undo")));
        assert_eq!(journal.status, TransactionOutcomeStatus::Partial);
    }

    #[test]
    fn uninstall_drop_record_failure_is_partial() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        // Files are gone (irreversibly — X1 takes no backup); the record
        // stayed. Partial tells repair exactly that story.
        let mut ops = FakeOps::failing("drop_record");
        let mut journal = journal(tmp.path());

        let err = execute_owned_steps(&uninstall_steps(), &mut ops, &mut journal).unwrap_err();

        assert!(matches!(
            err,
            OwnedExecutionError::StepFailed {
                step: Step::DropRecord,
                rolled_back: false,
                ..
            }
        ));
        assert_eq!(journal.status, TransactionOutcomeStatus::Partial);
    }

    #[test]
    fn cleanup_trouble_downgrades_rollback_to_partial() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps {
            fail_on: Some("set_capabilities"),
            undo_warnings: vec!["could not remove /usr/bin/cosh".to_string()],
            ..FakeOps::default()
        };
        let mut journal = journal(tmp.path());

        let err = execute_owned_steps(&install_steps(), &mut ops, &mut journal).unwrap_err();

        match err {
            OwnedExecutionError::StepFailed {
                rolled_back,
                rollback_warnings,
                ..
            } => {
                assert!(rolled_back);
                assert_eq!(
                    rollback_warnings,
                    vec!["could not remove /usr/bin/cosh".to_string()]
                );
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
        assert_eq!(journal.status, TransactionOutcomeStatus::Partial);
    }

    #[test]
    fn quarantine_exit_write_record_only_plan_succeeds() {
        // R6: the whole plan is a single owned record write.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps::default();
        let mut journal = journal(tmp.path());

        execute_owned_steps(
            &[Step::WriteRecord(RecordWrite::Owned)],
            &mut ops,
            &mut journal,
        )
        .expect("execution ok");

        assert_eq!(ops.calls, vec!["write_record:write-owned"]);
        assert_eq!(journal.status, TransactionOutcomeStatus::Ok);
    }

    #[test]
    fn delegated_steps_are_rejected_before_any_side_effect() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps::default();
        let mut journal = journal(tmp.path());

        let steps = vec![
            Step::PlaceFiles,
            Step::NativeTransaction {
                pm: NativePm::Rpm,
                action: NativeAction::Install,
                packages: vec!["cosh".to_string()],
            },
        ];
        let err = execute_owned_steps(&steps, &mut ops, &mut journal).unwrap_err();

        assert!(matches!(
            err,
            OwnedExecutionError::UnsupportedStep {
                step: Step::NativeTransaction { .. }
            }
        ));
        assert!(ops.calls.is_empty());
        assert!(journal.steps.is_empty());
        assert_eq!(journal.status, TransactionOutcomeStatus::InFlight);
    }

    #[test]
    fn recover_journal_is_not_this_executors_job() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mut ops = FakeOps::default();
        let mut journal = journal(tmp.path());

        let err = execute_owned_steps(&[Step::RecoverJournal], &mut ops, &mut journal).unwrap_err();

        assert!(matches!(
            err,
            OwnedExecutionError::UnsupportedStep {
                step: Step::RecoverJournal
            }
        ));
    }
}
