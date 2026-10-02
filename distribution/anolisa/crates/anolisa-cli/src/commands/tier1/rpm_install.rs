//! Durable intent and recovery helpers for fresh delegated RPM installs.

use std::path::{Path, PathBuf};

use anolisa_core::domain::NativePm;
#[cfg(test)]
use anolisa_core::facts::JournalEvidence;
use anolisa_core::facts::{JournalInventory, is_legacy_rpm_install_journal};
#[cfg(test)]
use anolisa_core::state::OperationRecord;
use anolisa_core::transaction::{
    Transaction, TransactionError, TransactionOutcomeStatus, TransactionStep, TransactionStepStatus,
};
use anolisa_platform::fs_layout::FsLayout;

use crate::response::CliError;

const INSTALL_PHASE: &str = "rpm-install";
const INSTALL_ACTION: &str = "dnf-install";
const STATE_PHASE: &str = "rpm-state";
const STATE_ACTION: &str = "commit-rpm-managed";

/// Validated pending intent for a fresh RPM install.
#[derive(Debug, Clone)]
pub(crate) struct PendingRpmInstall {
    pub(crate) transaction: Transaction,
    pub(crate) component: String,
    pub(crate) package: String,
    pub(crate) install_step: usize,
    pub(crate) state_step: usize,
}

impl PendingRpmInstall {
    pub(crate) fn mark_install_done(&mut self, command: &str) -> Result<(), CliError> {
        if self.transaction.steps[self.install_step].status != TransactionStepStatus::Done {
            self.transaction
                .mark_done(self.install_step)
                .map_err(|err| journal_error(command, "record completed RPM install", err))?;
        }
        Ok(())
    }

    pub(crate) fn mark_state_done(&mut self, command: &str) -> Result<(), CliError> {
        if self.transaction.steps[self.state_step].status != TransactionStepStatus::Done {
            self.transaction
                .mark_done(self.state_step)
                .map_err(|err| journal_error(command, "record committed RPM state", err))?;
        }
        Ok(())
    }

    pub(crate) fn finish_ok(&mut self, command: &str) -> Result<(), CliError> {
        self.transaction
            .finish(TransactionOutcomeStatus::Ok)
            .map_err(|err| journal_error(command, "finish RPM install journal", err))
    }

    pub(crate) fn finish_failed(
        &mut self,
        failed_step: usize,
        reason: &str,
        command: &str,
    ) -> Result<(), CliError> {
        self.transaction
            .mark_failed(failed_step, reason)
            .map_err(|err| journal_error(command, "record failed RPM install", err))?;
        self.transaction
            .finish(TransactionOutcomeStatus::Failed)
            .map_err(|err| journal_error(command, "finish failed RPM install", err))
    }
}

pub(crate) fn journal_dir(layout: &FsLayout) -> PathBuf {
    layout.state_dir.join("journal")
}

// Test-only since the delegated install moved to the planner pipeline's
// subject journals: tests use this to fabricate the legacy two-step journal
// shape that repair's R1 recovery still consumes from disk.
#[cfg(test)]
pub(crate) fn begin_fresh_install(
    layout: &FsLayout,
    component: &str,
    package: &str,
    command: &str,
) -> Result<PendingRpmInstall, CliError> {
    let state_path = layout.state_dir.join("installed.toml");
    let mut transaction = Transaction::begin("install", state_path, &journal_dir(layout))
        .map_err(|err| journal_error(command, "create pending RPM install", err))?;
    // Component and package together define the recovery claim. Persist both
    // steps in one revision so a crash cannot expose a half-formed contract
    // that neither repair nor a later install can interpret safely.
    if let Err(err) = transaction.record_steps([
        TransactionStep::planned(INSTALL_PHASE, package, INSTALL_ACTION, None),
        TransactionStep::planned(STATE_PHASE, component, STATE_ACTION, None),
    ]) {
        let _ = transaction.finish(TransactionOutcomeStatus::Failed);
        return Err(journal_error(
            command,
            "record pending RPM install steps",
            err,
        ));
    }
    Ok(PendingRpmInstall {
        transaction,
        component: component.to_string(),
        package: package.to_string(),
        install_step: 0,
        state_step: 1,
    })
}

fn has_legacy_install_marker(step: &TransactionStep) -> bool {
    step.phase == INSTALL_PHASE || step.action == INSTALL_ACTION
}

fn has_legacy_state_marker(step: &TransactionStep) -> bool {
    step.phase == STATE_PHASE || step.action == STATE_ACTION
}

/// Whether one journal claims `component` as its recorded state identity:
/// an explicit subject attribution or a legacy `rpm-state` step.
fn journal_claims(entry: &anolisa_core::facts::JournalEntry, component: &str) -> bool {
    entry.transaction().subject.as_deref() == Some(component)
        || entry
            .transaction()
            .steps
            .iter()
            .any(|step| has_legacy_state_marker(step) && step.target == component)
}

/// Whether any journal on disk claims `component` as its recorded state
/// identity.
///
/// Mutation identity resolution treats such a claim as exact local state, so
/// an interrupted (or still-uncleared) operation stays addressable under its
/// recorded name even when the component index cannot validate it
/// (issue #2630). Deliberately not gated on the pending flag — a terminal
/// journal that repair has yet to clear still names the component.
pub(crate) fn journal_claims_component(
    inventory: &anolisa_core::facts::JournalInventory,
    component: &str,
) -> bool {
    inventory
        .entries()
        .iter()
        .any(|entry| journal_claims(entry, component))
}

/// [`journal_claims_component`] restricted to effectively-pending journals.
///
/// Install uses this narrower claim: only a pending operation needs to reach
/// its recovery guidance, while a terminal journal left behind by a finished
/// operation must not keep authorizing an identity the component index no
/// longer recognizes.
pub(crate) fn pending_journal_claims_component(
    inventory: &anolisa_core::facts::JournalInventory,
    component: &str,
) -> bool {
    inventory
        .entries()
        .iter()
        .any(|entry| entry.is_effectively_pending() && journal_claims(entry, component))
}

/// Find one live RPM claim matching a component or package alias.
///
/// `operations` is the operation history the claims are checked against; the
/// v4 `InstalledState` and the v5 `StateStore` carry the same record shape,
/// so callers on either model pass their history slice.
#[cfg(test)]
pub(crate) fn find_pending_claim(
    layout: &FsLayout,
    operations: &[OperationRecord],
    claims: &[&str],
    command: &str,
) -> Result<Option<PendingRpmInstall>, CliError> {
    let dir = journal_dir(layout);
    let evidence = JournalEvidence::new(&dir, operations);
    let inventory = JournalInventory::load(evidence).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: err.to_string(),
    })?;
    find_pending_claim_in_inventory(layout, claims, command, &inventory)
}

/// Find one legacy two-step RPM claim in an already validated journal
/// inventory.
pub(crate) fn find_pending_claim_in_inventory(
    layout: &FsLayout,
    claims: &[&str],
    command: &str,
    inventory: &JournalInventory,
) -> Result<Option<PendingRpmInstall>, CliError> {
    let mut matches = Vec::new();
    for entry in inventory.entries() {
        if !entry.is_effectively_pending() {
            continue;
        }
        let Some(pending) =
            parse_pending(entry.transaction().clone(), entry.path(), layout, command)?
        else {
            continue;
        };
        if claims.is_empty()
            || claims
                .iter()
                .any(|claim| *claim == pending.component || *claim == pending.package)
        {
            matches.push(pending);
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => {
            let journals = matches
                .iter()
                .map(|pending| {
                    format!(
                        "{} (component '{}', package '{}', path {})",
                        pending.transaction.operation_id,
                        pending.component,
                        pending.package,
                        pending.transaction.journal_path.display()
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            let target = if claims.is_empty() {
                "the state root".to_string()
            } else {
                format!("'{}'", claims.join("', '"))
            };
            Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "multiple pending RPM installs match {target}: {journals}; refusing to choose an owner automatically — verify each package in rpmdb and inspect the listed journals before removing any recovery marker"
                ),
            })
        }
    }
}

/// Interpret one effectively pending journal as a legacy RPM install claim.
///
/// Callers must first classify the journal through [`JournalInventory`] with
/// same-root operation history. Journals from another pipeline return
/// `Ok(None)`; a live journal carrying legacy markers but an unsafe shape
/// fails closed.
pub(crate) fn parse_pending(
    transaction: Transaction,
    path: &Path,
    layout: &FsLayout,
    command: &str,
) -> Result<Option<PendingRpmInstall>, CliError> {
    let install_steps = transaction
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| has_legacy_install_marker(step))
        .collect::<Vec<_>>();
    let state_steps = transaction
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| has_legacy_state_marker(step))
        .collect::<Vec<_>>();
    // `Transaction::begin` persists an empty revision before the initial step
    // batch. An interruption in that window is known to precede dnf, so the
    // empty journal owns nothing and is safe to ignore.
    if install_steps.is_empty() && state_steps.is_empty() {
        return Ok(None);
    }
    if !transaction.is_pending() {
        return Ok(None);
    }
    if transaction.delegated_recovery.is_some() {
        return Ok(None);
    }
    if transaction.subject.is_some()
        || !is_legacy_rpm_install_journal(&transaction)
        || install_steps.len() != 1
        || state_steps.len() != 1
        || install_steps[0].1.target.trim().is_empty()
        || !valid_component_name(state_steps[0].1.target.trim())
    {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "malformed live RPM recovery journal {} (operation '{}'); automatic recovery is unsafe — cross-check this operation in installed.toml and verify the package in rpmdb before removing or editing the recovery marker",
                path.display(),
                transaction.operation_id
            ),
        });
    }
    let expected_state = layout.state_dir.join("installed.toml");
    if transaction.state_path != expected_state || transaction.journal_path != path {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "pending RPM journal {} references an unexpected state or journal path",
                path.display()
            ),
        });
    }

    let component = state_steps[0].1.target.trim().to_string();
    let package = install_steps[0].1.target.trim().to_string();
    let install_step = install_steps[0].0;
    let state_step = state_steps[0].0;
    drop(install_steps);
    drop(state_steps);

    Ok(Some(PendingRpmInstall {
        component,
        package,
        install_step,
        state_step,
        transaction,
    }))
}

fn valid_component_name(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.contains('/')
        && !component.contains('\\')
}

/// One live pending fresh RPM install claim that reserves a package.
#[derive(Debug)]
pub(crate) struct PendingRpmPackageClaim {
    /// Component the pending operation installs the package for.
    pub(crate) component: String,
    /// Package name the pending operation reserves.
    pub(crate) package: String,
    /// Journal that reserves the package.
    pub(crate) journal_path: PathBuf,
}

/// Find the first live pending fresh RPM install journal reserving any of
/// `packages`.
///
/// Modern subject journals carry the reserved package in their delegated
/// recovery context; journals written before that field existed are decoded
/// through [`parse_pending`], inheriting its fail-closed verdict for
/// ambiguous live markers. Settled journals and non-install operations
/// reserve nothing here.
pub(crate) fn find_pending_rpm_claim_for_packages(
    inventory: &JournalInventory,
    layout: &FsLayout,
    packages: &[&str],
    command: &str,
) -> Result<Option<PendingRpmPackageClaim>, CliError> {
    for entry in inventory.entries() {
        if !entry.is_effectively_pending() || entry.transaction().operation != "install" {
            continue;
        }
        let transaction = entry.transaction();
        if let Some(recovery) = &transaction.delegated_recovery {
            let Some(package) = recovery.package.as_deref() else {
                continue;
            };
            if recovery.pm != NativePm::Rpm || !packages.contains(&package) {
                continue;
            }
            // A delegated journal without a subject cannot be routed to
            // `repair <component>`; fail closed instead of guessing an owner.
            let Some(component) = transaction.subject.as_deref() else {
                return Err(CliError::Runtime {
                    command: command.to_string(),
                    reason: format!(
                        "system package '{package}' is reserved by the pending RPM install journal {}, which has no component subject; inspect the journal and settle it before retrying",
                        entry.path().display()
                    ),
                });
            };
            return Ok(Some(PendingRpmPackageClaim {
                component: component.to_string(),
                package: package.to_string(),
                journal_path: entry.path().to_path_buf(),
            }));
        }
        let Some(pending) = parse_pending(transaction.clone(), entry.path(), layout, command)?
        else {
            continue;
        };
        if packages.contains(&pending.package.as_str()) {
            return Ok(Some(PendingRpmPackageClaim {
                component: pending.component,
                package: pending.package,
                journal_path: entry.path().to_path_buf(),
            }));
        }
    }
    Ok(None)
}

pub(crate) fn journal_error(command: &str, action: &str, err: TransactionError) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to {action}: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anolisa_core::domain::NativePm;
    use anolisa_core::transaction::{DelegatedRecordAction, DelegatedRecoveryContext};
    use std::fs;
    use tempfile::tempdir;

    fn layout() -> (tempfile::TempDir, FsLayout) {
        let tmp = tempdir().expect("tmpdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        (tmp, layout)
    }

    #[test]
    fn claim_lookup_matches_component_and_package_alias() {
        let (_tmp, layout) = layout();
        let pending = begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
            .expect("begin journal");

        for claim in ["cosh", "copilot-shell"] {
            let found = find_pending_claim(&layout, &[], &[claim], "test")
                .expect("find claim")
                .expect("pending claim");
            assert_eq!(
                found.transaction.operation_id,
                pending.transaction.operation_id
            );
        }
    }

    #[test]
    fn claim_lookup_rejects_multiple_matching_journals() {
        let (_tmp, layout) = layout();
        let first = begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
            .expect("first journal");
        let second = begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
            .expect("second journal");

        let err = find_pending_claim(&layout, &[], &["cosh"], "test")
            .expect_err("ambiguous claim must fail");
        assert!(err.reason().contains("multiple pending RPM installs"));
        assert!(err.reason().contains(&first.transaction.operation_id));
        assert!(err.reason().contains(&second.transaction.operation_id));
        assert!(err.reason().contains("verify each package in rpmdb"));
    }

    #[test]
    fn committed_operation_makes_malformed_stale_journal_ignorable() {
        let (_tmp, layout) = layout();
        let mut pending = begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
            .expect("begin journal");
        pending.transaction.steps.pop();
        fs::write(
            &pending.transaction.journal_path,
            toml::to_string_pretty(&pending.transaction).expect("serialize journal"),
        )
        .expect("rewrite journal");
        let operations = vec![OperationRecord {
            id: pending.transaction.operation_id,
            command: "install cosh".to_string(),
            status: "ok".to_string(),
            started_at: "2026-07-14T00:00:00Z".to_string(),
            finished_at: Some("2026-07-14T00:00:01Z".to_string()),
            parent_operation_id: None,
        }];

        assert!(
            find_pending_claim(&layout, &operations, &["cosh"], "test")
                .expect("scan stale journal")
                .is_none()
        );
    }

    #[test]
    fn malformed_live_journal_reports_safe_inspection_steps() {
        let (_tmp, layout) = layout();
        let mut pending = begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
            .expect("begin journal");
        pending.transaction.steps.pop();
        fs::write(
            &pending.transaction.journal_path,
            toml::to_string_pretty(&pending.transaction).expect("serialize journal"),
        )
        .expect("rewrite journal");

        let err = find_pending_claim(&layout, &[], &["cosh"], "test")
            .expect_err("live malformed journal must fail closed");
        assert!(err.reason().contains(&pending.transaction.operation_id));
        assert!(err.reason().contains("installed.toml"));
        assert!(err.reason().contains("rpmdb"));
        assert!(err.reason().contains("before removing or editing"));
    }

    #[test]
    fn ambiguous_legacy_shapes_remain_recovery_errors() {
        for shape in ["reversed", "duplicate", "partial-marker", "foreign-step"] {
            let (_tmp, layout) = layout();
            let mut pending = begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin journal");
            if shape == "reversed" {
                pending.transaction.steps.reverse();
            } else if shape == "duplicate" {
                let duplicate = pending.transaction.steps[0].clone();
                pending.transaction.steps[1] = duplicate;
            } else if shape == "partial-marker" {
                pending.transaction.steps[0].action = "other-action".to_string();
            } else {
                assert_eq!(shape, "foreign-step");
                pending.transaction.steps.push(TransactionStep::planned(
                    "other-phase",
                    "cosh",
                    "other-action",
                    None,
                ));
            }
            fs::write(
                &pending.transaction.journal_path,
                toml::to_string_pretty(&pending.transaction).expect("serialize journal"),
            )
            .expect("rewrite journal");

            let err = find_pending_claim(&layout, &[], &["cosh"], "test")
                .expect_err("ambiguous legacy journal must fail closed");

            assert!(err.reason().contains(&pending.transaction.operation_id));
            assert!(err.reason().contains("automatic recovery is unsafe"));
        }
    }

    #[test]
    fn explicit_delegated_context_is_not_a_legacy_rpm_claim() {
        let (_tmp, layout) = layout();
        let state_path = layout.state_dir.join("installed.toml");
        let mut transaction = Transaction::begin_with_subject(
            "install",
            Some("cosh"),
            state_path,
            &journal_dir(&layout),
        )
        .expect("begin subjected journal");
        transaction
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some("copilot-shell".to_string()),
                    record_action: DelegatedRecordAction::WriteManaged,
                    pinned: None,
                },
                [
                    TransactionStep::planned(INSTALL_PHASE, "copilot-shell", INSTALL_ACTION, None),
                    TransactionStep::planned(STATE_PHASE, "cosh", STATE_ACTION, None),
                ],
            )
            .expect("record hybrid steps");
        drop(transaction);

        let pending =
            find_pending_claim(&layout, &[], &["cosh"], "test").expect("scan hybrid journal");

        assert!(pending.is_none());
    }
}
