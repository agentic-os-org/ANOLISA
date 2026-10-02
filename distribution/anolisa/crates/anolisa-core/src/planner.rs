//! Lifecycle planner: pure decision tables from intent and observed facts
//! to an executable plan.
//!
//! The lifecycle protocol is
//! intent → observe → plan → execute; this module is the *plan* stage only.
//! It performs no I/O: everything the decision needs — the state record,
//! the re-observed native facts, pending-journal presence — arrives as
//! [`Facts`], gathered by the observe stage. The executor re-validates the
//! facts under lock before running the returned steps.
//!
//! Global rules implemented here:
//! 1. A pending journal fails planning for every intent except `Repair`,
//!    which consumes it (R1).
//! 2. Delegated facts are assumed re-observed; the planner treats
//!    [`NativeProbe::NotProbed`] on a delegated path as a caller bug.
//! 3. Implicit behaviors are gone: every branch either maps to a step
//!    sequence from the tables or to a typed [`PlanError`] naming the way
//!    out.

use crate::domain::{
    Installation, InstallationScope, ManagementRelation, NativePm, Observation, ProviderBinding,
};
use crate::state_migration::QuarantineReason;

/// User intent, one per lifecycle verb. `status` is a read-only projection
/// and never plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Fresh install of a component in the effective scope.
    Install(InstallRequest),
    /// Explicitly adopt a pre-existing native package (system scope only).
    Adopt,
    /// Move to a newer version.
    Update(UpdateRequest),
    /// Make reality and record agree again.
    Repair,
    /// Remove the installation.
    Uninstall(UninstallRequest),
}

/// Install parameters resolved by the observe stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRequest {
    /// Distribution the resolver selected for this component and scope.
    pub target: ProviderTarget,
    /// Explicit `--version` request, if any.
    pub requested_version: Option<String>,
}

/// Which provider family an install resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTarget {
    /// Raw artifact ANOLISA will own.
    Owned { version: String },
    /// Native package transaction ANOLISA will request.
    Delegated {
        pm: NativePm,
        /// Bare package identity: the name used for rpmdb observation and the
        /// persisted delegated record. Never carries version/arch decoration.
        package: String,
        /// Exact native-manager transaction spec (an RPM NEVRA) when a
        /// `--version` pin resolved to a concrete repository candidate. `None`
        /// installs the repository default. Kept separate from `package` so a
        /// pin reaches only the [`Step::NativeTransaction`] — the observation,
        /// the `DelegatedIdentity`, and persisted state stay on the bare name.
        artifact: Option<String>,
    },
}

/// Update parameters resolved by the observe stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRequest {
    /// Resolution of the owned-artifact update target. `None` for delegated
    /// records (the native solver picks the target) — required when the
    /// record is `Owned`.
    pub owned_resolution: Option<OwnedUpdateResolution>,
}

/// Resolved owned-update target and its relation to the installed version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedUpdateResolution {
    /// Version the resolver selected.
    pub to_version: String,
    /// How it compares to the installed version.
    pub relation: VersionRelation,
}

/// Comparison of a resolved version against the installed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    /// Strictly newer — update proceeds.
    Newer,
    /// Identical — nothing to do.
    Same,
    /// Older — downgrades are refused (U4).
    Older,
    /// Not comparable (non-semver) — refused (U4).
    Indeterminate,
}

/// Uninstall parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallRequest {
    /// `--remove-system-package`: per-invocation authority to remove an
    /// adopted/observed native package.
    pub remove_system_package: bool,
    /// How the command was invoked. The remove flag above is only honored
    /// on single named invocations.
    pub invocation: InvocationForm,
}

/// Whether the intent targets one named component or comes from a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationForm {
    /// `anolisa <verb> <component>` naming exactly this component.
    SingleNamed,
    /// Part of a multi-component plan (`--all`, upgrade, …).
    Batch,
}

/// Everything the planner may consult. Gathered by the observe stage;
/// pure data, already re-observed where a native authority is involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// Effective installation scope.
    pub scope: InstallationScope,
    /// What the state store says about this component in this scope.
    pub record: RecordFacts,
    /// What the native package database says (re-observed this run).
    pub native: NativeProbe,
    /// A pending operation journal exists for this component.
    pub pending_journal: bool,
    /// Enabled adapters currently claiming this component (uninstall guard).
    pub active_adapter_claims: Vec<String>,
    /// Integrity probe over the record's file list: `Some(true)` verified,
    /// `Some(false)` missing/corrupt, `None` not probed. Drives repair R2
    /// and the quarantine exit R6.
    pub owned_files_verified: Option<bool>,
}

/// State-store facts for one (component, scope).
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(clippy::large_enum_variant, reason = "facts are built once per plan")]
pub enum RecordFacts {
    /// No record.
    Absent,
    /// A migrated, active installation.
    Active(Installation),
    /// A quarantined record: inert until repair/forget.
    Quarantined(QuarantineReason),
}

/// Re-observed native package facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeProbe {
    /// No probe ran (owned-only path or user scope).
    NotProbed,
    /// The package is not in the native database.
    Absent,
    /// Exactly one installed version.
    Present {
        /// Resolved native package name.
        package: String,
        /// Fresh observation.
        observation: Observation,
    },
    /// More than one installed version (kernel-style multi-install).
    MultipleVersions {
        /// Resolved native package name.
        package: String,
    },
}

impl NativeProbe {
    fn package(&self) -> Option<&str> {
        match self {
            Self::Present { package, .. } | Self::MultipleVersions { package } => Some(package),
            Self::NotProbed | Self::Absent => None,
        }
    }

    fn is_present(&self) -> bool {
        matches!(self, Self::Present { .. } | Self::MultipleVersions { .. })
    }
}

/// One executable step. The two provider families share this vocabulary but
/// never each other's compensation semantics: owned steps roll back via
/// journal compensation, `NativeTransaction` is forward-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Fetch and verify the owned artifact for the recorded/target version.
    DownloadVerify,
    /// Install system packages the component's runtime depends on.
    ProvisionRuntimeDeps,
    /// Run a contract hook.
    RunHook(HookKind),
    /// Place owned files on disk.
    PlaceFiles,
    /// Apply file capabilities.
    SetCapabilities,
    /// Enable and start recorded services.
    EnableServices,
    /// Restart recorded services (updates/reinstalls load the new binary).
    RestartServices,
    /// Stop recorded services before teardown.
    StopServices,
    /// Back up owned files before a destructive owned step.
    BackupFiles,
    /// Remove ANOLISA-owned files (existence and digest re-checked first).
    RemoveOwnedFiles,
    /// Request one native package-manager transaction. Forward-only: on
    /// failure the recovery is re-observation, never an automatic undo.
    NativeTransaction {
        /// Which native manager.
        pm: NativePm,
        /// Transaction verb.
        action: NativeAction,
        /// Packages in this transaction.
        packages: Vec<String>,
    },
    /// Re-read the native authority and refresh the observation cache.
    Observe {
        /// Packages to observe.
        packages: Vec<String>,
    },
    /// Persist the installation record.
    WriteRecord(RecordWrite),
    /// Remove the installation record.
    DropRecord,
    /// Consume a pending operation journal (repair R1).
    RecoverJournal,
}

/// Contract hook classes the planner schedules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    /// Before files are placed.
    PreInstall,
    /// After files are placed.
    PostInstall,
    /// Before owned teardown.
    PreUninstall,
    /// After owned teardown.
    PostUninstall,
}

/// Native transaction verbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAction {
    /// `dnf install`-equivalent.
    Install,
    /// `dnf update`-equivalent.
    Update,
    /// `dnf reinstall`-equivalent.
    Reinstall,
    /// `dnf remove`-equivalent.
    Remove,
}

impl NativeAction {
    /// Stable verb label (`install`, `update`, `reinstall`, `remove`) used
    /// in journals and diagnostics.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Reinstall => "reinstall",
            Self::Remove => "remove",
        }
    }
}

/// What a `WriteRecord` step persists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordWrite {
    /// New or replayed owned installation.
    Owned,
    /// Delegated installation ANOLISA installed (managed).
    DelegatedManaged,
    /// Delegated installation the user adopted.
    DelegatedAdopted,
    /// Delegated installation merely observed (quarantine exit R5).
    DelegatedObserved,
    /// Absorb a fresh observation into an existing delegated record.
    RefreshObservation,
}

impl RecordWrite {
    /// Stable label used in journals and diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Owned => "write-owned",
            Self::DelegatedManaged => "write-delegated-managed",
            Self::DelegatedAdopted => "write-delegated-adopted",
            Self::DelegatedObserved => "write-delegated-observed",
            Self::RefreshObservation => "refresh-observation",
        }
    }
}

/// Planner output: either an ordered step sequence or an explicit no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Facts already satisfy the intent.
    NoOp {
        /// Why nothing needs to run.
        reason: NoOpReason,
    },
    /// Steps to execute in order.
    Execute {
        /// Ordered steps.
        steps: Vec<Step>,
        /// Non-fatal findings to surface alongside execution.
        notes: Vec<PlanNote>,
    },
}

impl Plan {
    fn execute(steps: Vec<Step>) -> Self {
        Self::Execute {
            steps,
            notes: Vec::new(),
        }
    }
}

/// Reasons a plan is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoOpReason {
    /// Owned record already at the requested state (I4).
    AlreadyInstalled,
    /// Delegated record already tracks the present package (I8).
    AlreadyTracked,
    /// Already adopted (A7).
    AlreadyAdopted,
    /// Owned update resolved to the installed version (U2).
    AlreadyLatest,
    /// Repair found record and reality in agreement.
    NothingToRepair,
}

/// Non-fatal findings attached to a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanNote {
    /// The native package was already removed externally; only the record
    /// is dropped (X3).
    PackageAlreadyAbsent,
}

/// Typed planning failures. Each names the way out — the CLI renders the
/// suggestion, the planner never improvises a fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// A pending operation journal exists → run `repair` first.
    PendingOperation,
    /// Install found an unmanaged system package → `adopt` (I3).
    AlreadyPresentOnSystem,
    /// Install with a version argument over an existing owned record →
    /// `update --version` (I5).
    UseUpdate,
    /// The component is already managed through the native backend (I6, A5).
    AlreadyManaged,
    /// A managed package was removed externally → `repair` or `forget` (I7, U7, RI5).
    ExternallyRemoved,
    /// A tracked (adopted/observed) package is gone → `forget` (I9).
    TrackedButAbsent,
    /// The record is only observed; native transactions require adoption
    /// first (U6, RI4).
    NotAdopted,
    /// No record exists → `install` (U1, RI1, R7, X6).
    NotInstalled,
    /// Owned update resolved to an older version (U4).
    RefuseDowngrade,
    /// Owned update target is not comparable (U4).
    IndeterminateVersion,
    /// Requested provider/package conflicts with recorded provenance (I11).
    ProvenanceConflict,
    /// The record is quarantined → `repair` or `forget` (I10, U8, RI6, X5).
    NeedsAttention,
    /// A quarantined record matches neither the native db nor its file
    /// list → `forget` (R6 failure arm).
    RecordUnrecoverable,
    /// Delegated targets need system scope.
    DelegatedRequiresSystemScope,
    /// Adopt is a system-scope verb.
    AdoptRequiresSystemScope,
    /// Nothing to adopt: the package is not installed → `install` (A2).
    NothingToAdopt,
    /// Multiple installed versions or ambiguous package match (A3).
    AmbiguousPackage,
    /// Enabled adapters still claim this component → `adapter disable` (X guard).
    AdapterClaimsActive,
    /// `--remove-system-package` is only honored on single named
    /// invocations.
    RemoveSystemPackageRequiresSingleTarget,
    /// The record's package identity is unresolved; `repair` resolves it
    /// before native transactions can be planned.
    PackageUnresolved,
    /// Caller bug: an owned update was planned without a resolution.
    MissingUpdateResolution,
    /// Caller bug: a delegated path was planned without a native probe.
    MissingNativeProbe,
}

/// Plan an intent against observed facts. Pure function: the full lifecycle
/// decision tables, first match wins within each table.
pub fn plan(intent: &Intent, facts: &Facts) -> Result<Plan, PlanError> {
    // Global rule: pending journals block everything except repair, which
    // consumes them (R1).
    if facts.pending_journal && !matches!(intent, Intent::Repair) {
        return Err(PlanError::PendingOperation);
    }
    match intent {
        Intent::Install(req) => plan_install(req, facts),
        Intent::Adopt => plan_adopt(facts),
        Intent::Update(req) => plan_update(req, facts),
        Intent::Repair => plan_repair(facts),
        Intent::Uninstall(req) => plan_uninstall(req, facts),
    }
}

/// Install decision table (rows I1–I11).
fn plan_install(req: &InstallRequest, facts: &Facts) -> Result<Plan, PlanError> {
    if matches!(req.target, ProviderTarget::Delegated { .. })
        && matches!(facts.scope, InstallationScope::User { .. })
    {
        return Err(PlanError::DelegatedRequiresSystemScope);
    }
    match &facts.record {
        RecordFacts::Quarantined(_) => Err(PlanError::NeedsAttention), // I10
        RecordFacts::Active(record) => {
            // I11: provenance stays sticky; explicit overrides that
            // contradict the record fail before any state-specific arm.
            if target_conflicts_with_record(&req.target, record) {
                return Err(PlanError::ProvenanceConflict);
            }
            match &record.binding {
                ProviderBinding::Owned { artifact } => match &req.requested_version {
                    // I5: a different version through install is an update.
                    Some(v) if *v != artifact.version => Err(PlanError::UseUpdate),
                    // I4: install is idempotent for the recorded version.
                    _ => Ok(Plan::NoOp {
                        reason: NoOpReason::AlreadyInstalled,
                    }),
                },
                ProviderBinding::Delegated { relation, .. } => {
                    let present = probed_presence(&facts.native)?;
                    match (relation, present) {
                        (ManagementRelation::Managed { .. }, true) => {
                            Err(PlanError::AlreadyManaged) // I6
                        }
                        (ManagementRelation::Managed { .. }, false) => {
                            Err(PlanError::ExternallyRemoved) // I7
                        }
                        (_, true) => Ok(Plan::NoOp {
                            reason: NoOpReason::AlreadyTracked,
                        }), // I8
                        (_, false) => Err(PlanError::TrackedButAbsent), // I9
                    }
                }
            }
        }
        RecordFacts::Absent => {
            // I3: an unmanaged system package is never silently
            // adopted, whatever the resolved target family.
            if matches!(facts.scope, InstallationScope::System) && facts.native.is_present() {
                return Err(PlanError::AlreadyPresentOnSystem);
            }
            match &req.target {
                ProviderTarget::Owned { .. } => Ok(Plan::execute(vec![
                    // I1
                    Step::DownloadVerify,
                    Step::ProvisionRuntimeDeps,
                    Step::RunHook(HookKind::PreInstall),
                    Step::PlaceFiles,
                    Step::SetCapabilities,
                    Step::RunHook(HookKind::PostInstall),
                    Step::EnableServices,
                    Step::WriteRecord(RecordWrite::Owned),
                ])),
                ProviderTarget::Delegated {
                    pm,
                    package,
                    artifact,
                } => Ok(Plan::execute(vec![
                    // I2: the native transaction targets the pinned artifact
                    // (exact NEVRA) when a version was resolved, otherwise the
                    // bare package (repository default). Observation and the
                    // record always use the bare package identity.
                    Step::NativeTransaction {
                        pm: *pm,
                        action: NativeAction::Install,
                        packages: vec![artifact.clone().unwrap_or_else(|| package.clone())],
                    },
                    Step::Observe {
                        packages: vec![package.clone()],
                    },
                    Step::WriteRecord(RecordWrite::DelegatedManaged),
                ])),
            }
        }
    }
}

/// Adopt decision table (rows A1–A7).
fn plan_adopt(facts: &Facts) -> Result<Plan, PlanError> {
    if !matches!(facts.scope, InstallationScope::System) {
        return Err(PlanError::AdoptRequiresSystemScope);
    }
    match &facts.record {
        RecordFacts::Quarantined(_) => Err(PlanError::NeedsAttention),
        RecordFacts::Active(record) => match &record.binding {
            ProviderBinding::Owned { .. } => Err(PlanError::ProvenanceConflict), // A4
            ProviderBinding::Delegated { relation, .. } => {
                let present = probed_presence(&facts.native)?;
                match (relation, present) {
                    (ManagementRelation::Managed { .. }, _) => Err(PlanError::AlreadyManaged), // A5
                    (ManagementRelation::Adopted { .. }, true) => {
                        Ok(Plan::NoOp {
                            reason: NoOpReason::AlreadyAdopted,
                        }) // A7
                    }
                    (ManagementRelation::Observed, true) => Ok(Plan::execute(vec![
                        // A6: upgrade the management consent in place.
                        Step::WriteRecord(RecordWrite::DelegatedAdopted),
                    ])),
                    (_, false) => Err(PlanError::TrackedButAbsent),
                }
            }
        },
        RecordFacts::Absent => match &facts.native {
            NativeProbe::Present { package, .. } => Ok(Plan::execute(vec![
                // A1
                Step::Observe {
                    packages: vec![package.clone()],
                },
                Step::WriteRecord(RecordWrite::DelegatedAdopted),
            ])),
            NativeProbe::Absent => Err(PlanError::NothingToAdopt), // A2
            NativeProbe::MultipleVersions { .. } => Err(PlanError::AmbiguousPackage), // A3
            NativeProbe::NotProbed => Err(PlanError::MissingNativeProbe),
        },
    }
}

/// Update decision table (rows U1–U8).
fn plan_update(req: &UpdateRequest, facts: &Facts) -> Result<Plan, PlanError> {
    match &facts.record {
        RecordFacts::Absent => Err(PlanError::NotInstalled), // U1
        RecordFacts::Quarantined(_) => Err(PlanError::NeedsAttention), // U8
        RecordFacts::Active(record) => match &record.binding {
            ProviderBinding::Owned { .. } => {
                let resolution = req
                    .owned_resolution
                    .as_ref()
                    .ok_or(PlanError::MissingUpdateResolution)?;
                match resolution.relation {
                    VersionRelation::Same => Ok(Plan::NoOp {
                        reason: NoOpReason::AlreadyLatest,
                    }), // U2
                    VersionRelation::Newer => Ok(Plan::execute(vec![
                        // U3
                        Step::BackupFiles,
                        Step::DownloadVerify,
                        Step::RemoveOwnedFiles,
                        Step::PlaceFiles,
                        Step::SetCapabilities,
                        Step::RestartServices,
                        Step::WriteRecord(RecordWrite::Owned),
                    ])),
                    VersionRelation::Older => Err(PlanError::RefuseDowngrade), // U4
                    VersionRelation::Indeterminate => Err(PlanError::IndeterminateVersion), // U4
                }
            }
            // U6: no management consent → no native transaction.
            ProviderBinding::Delegated {
                relation: ManagementRelation::Observed,
                ..
            } => Err(PlanError::NotAdopted),
            ProviderBinding::Delegated { .. } => {
                if !probed_presence(&facts.native)? {
                    return Err(PlanError::ExternallyRemoved); // U7
                }
                let package = record_package(record, facts)?;
                Ok(Plan::execute(vec![
                    // U5
                    Step::NativeTransaction {
                        pm: NativePm::Rpm,
                        action: NativeAction::Update,
                        packages: vec![package.clone()],
                    },
                    Step::Observe {
                        packages: vec![package],
                    },
                    Step::WriteRecord(RecordWrite::RefreshObservation),
                ]))
            }
        },
    }
}

/// Repair decision table (rows R1–R7).
fn plan_repair(facts: &Facts) -> Result<Plan, PlanError> {
    // R1: repair is the journal's consumer.
    if facts.pending_journal {
        return Ok(Plan::execute(vec![Step::RecoverJournal]));
    }
    match &facts.record {
        RecordFacts::Absent => Err(PlanError::NotInstalled), // R7
        RecordFacts::Quarantined(_) => {
            // A multi-version probe is as ambiguous on the quarantine exit
            // as on the active rows (R3): rebuilding an observation from an
            // arbitrary EVR would launder the ambiguity into the record.
            if matches!(facts.native, NativeProbe::MultipleVersions { .. }) {
                return Err(PlanError::AmbiguousPackage);
            }
            if let NativeProbe::Present { package, .. } = &facts.native {
                return Ok(Plan::execute(vec![
                    // R5: rebuild from the native authority.
                    Step::Observe {
                        packages: vec![package.clone()],
                    },
                    Step::WriteRecord(RecordWrite::DelegatedObserved),
                ]));
            }
            if facts.owned_files_verified == Some(true) {
                // R6: the original file list still checks out.
                return Ok(Plan::execute(vec![Step::WriteRecord(RecordWrite::Owned)]));
            }
            Err(PlanError::RecordUnrecoverable) // R6 failure arm
        }
        RecordFacts::Active(record) => match &record.binding {
            ProviderBinding::Owned { .. } => match facts.owned_files_verified {
                Some(false) => Ok(Plan::execute(owned_replay_steps())), // R2
                _ => Ok(Plan::NoOp {
                    reason: NoOpReason::NothingToRepair,
                }),
            },
            ProviderBinding::Delegated { relation, .. } => match &facts.native {
                NativeProbe::Present { package, .. } => Ok(Plan::execute(vec![
                    // R3: absorb the fresh observation (drift or not).
                    Step::Observe {
                        packages: vec![package.clone()],
                    },
                    Step::WriteRecord(RecordWrite::RefreshObservation),
                ])),
                NativeProbe::MultipleVersions { .. } => Err(PlanError::AmbiguousPackage),
                NativeProbe::Absent => match relation {
                    ManagementRelation::Managed { .. } => {
                        let package = record_package(record, facts)?;
                        Ok(Plan::execute(vec![
                            // R4: managed grants reinstall authority.
                            Step::NativeTransaction {
                                pm: NativePm::Rpm,
                                action: NativeAction::Install,
                                packages: vec![package.clone()],
                            },
                            Step::Observe {
                                packages: vec![package],
                            },
                            Step::WriteRecord(RecordWrite::RefreshObservation),
                        ]))
                    }
                    // Adopted/observed grant no reinstall authority.
                    _ => Err(PlanError::TrackedButAbsent),
                },
                NativeProbe::NotProbed => Err(PlanError::MissingNativeProbe),
            },
        },
    }
}

/// Uninstall decision table (rows X1–X6).
fn plan_uninstall(req: &UninstallRequest, facts: &Facts) -> Result<Plan, PlanError> {
    // Adapter claims block teardown before any table arm (X guard).
    if !facts.active_adapter_claims.is_empty() {
        return Err(PlanError::AdapterClaimsActive);
    }
    // The flag is per-invocation authority; batches dilute it.
    if req.remove_system_package && matches!(req.invocation, InvocationForm::Batch) {
        return Err(PlanError::RemoveSystemPackageRequiresSingleTarget);
    }
    match &facts.record {
        RecordFacts::Absent => Err(PlanError::NotInstalled), // X6
        RecordFacts::Quarantined(_) => {
            if req.remove_system_package {
                // X5: a quarantined package identity is unverified —
                // never hand it to a native remove.
                return Err(PlanError::NeedsAttention);
            }
            Ok(Plan::execute(vec![Step::DropRecord]))
        }
        RecordFacts::Active(record) => match &record.binding {
            ProviderBinding::Owned { .. } => Ok(Plan::execute(vec![
                // X1
                Step::RunHook(HookKind::PreUninstall),
                Step::StopServices,
                Step::RemoveOwnedFiles,
                Step::RunHook(HookKind::PostUninstall),
                Step::DropRecord,
            ])),
            ProviderBinding::Delegated { relation, .. } => {
                let removes = match relation {
                    ManagementRelation::Managed { .. } => true, // X2/X3
                    _ => req.remove_system_package,             // X4
                };
                if !removes {
                    // X4 default: a record-only drop consults no native
                    // authority, so it must not demand a probe (the native
                    // tooling may be the very thing that is missing).
                    return Ok(Plan::execute(vec![Step::DropRecord]));
                }
                let present = probed_presence(&facts.native)?;
                if !present {
                    // X3: already gone externally — record-only, flagged.
                    return Ok(Plan::Execute {
                        steps: vec![Step::DropRecord],
                        notes: vec![PlanNote::PackageAlreadyAbsent],
                    });
                }
                let package = record_package(record, facts)?;
                Ok(Plan::execute(vec![
                    Step::NativeTransaction {
                        pm: NativePm::Rpm,
                        action: NativeAction::Remove,
                        packages: vec![package],
                    },
                    Step::DropRecord,
                ]))
            }
        },
    }
}

/// Steps replaying an owned installation at its recorded version (repair R2).
fn owned_replay_steps() -> Vec<Step> {
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

/// Presence according to a probe that must have run (delegated paths).
fn probed_presence(native: &NativeProbe) -> Result<bool, PlanError> {
    match native {
        NativeProbe::NotProbed => Err(PlanError::MissingNativeProbe),
        NativeProbe::Absent => Ok(false),
        NativeProbe::Present { .. } | NativeProbe::MultipleVersions { .. } => Ok(true),
    }
}

/// Package name for a native transaction against an existing record: the
/// record's resolved identity, falling back to the probe's resolution.
/// Unresolved identities cannot be transacted — repair resolves them.
fn record_package(record: &Installation, facts: &Facts) -> Result<String, PlanError> {
    if let ProviderBinding::Delegated { package, .. } = &record.binding
        && let Some(name) = package.resolved_name()
    {
        return Ok(name.to_string());
    }
    facts
        .native
        .package()
        .map(str::to_string)
        .ok_or(PlanError::PackageUnresolved)
}

/// True when an explicit install target contradicts recorded provenance
/// (I11): family switch, or a different delegated package.
fn target_conflicts_with_record(target: &ProviderTarget, record: &Installation) -> bool {
    match (&record.binding, target) {
        (ProviderBinding::Owned { .. }, ProviderTarget::Owned { .. }) => false,
        (
            ProviderBinding::Delegated { package, .. },
            ProviderTarget::Delegated {
                package: requested, ..
            },
        ) => package
            .resolved_name()
            .is_some_and(|name| name != requested),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LifecycleStatus, OwnedArtifact, PackageIdentity};
    use crate::state::{ObjectKind, SubscriptionScope};

    const PKG: &str = "copilot-shell";

    fn observation() -> Observation {
        Observation {
            version: "1.2.3".to_string(),
            evr: Some("1:1.2.3-4".to_string()),
            arch: Some("x86_64".to_string()),
            source_repo: Some("@System".to_string()),
            observed_at: "2026-07-16T00:00:00Z".to_string(),
        }
    }

    fn installation(binding: ProviderBinding) -> Installation {
        Installation {
            kind: ObjectKind::Component,
            name: PKG.to_string(),
            scope: InstallationScope::System,
            binding,
            status: LifecycleStatus::Installed,
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: SubscriptionScope::None,
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    fn owned_record() -> Installation {
        installation(ProviderBinding::Owned {
            artifact: OwnedArtifact {
                version: "1.2.3".to_string(),
                distribution_source: None,
                raw_package: None,
                manifest_digest: None,
                files: Vec::new(),
                services: Vec::new(),
                external_modified_files: Vec::new(),
                provisioned_packages: Vec::new(),
            },
        })
    }

    fn delegated_record(relation: ManagementRelation) -> Installation {
        installation(ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package: PackageIdentity::Resolved {
                name: PKG.to_string(),
            },
            relation,
            last_observed: Some(observation()),
        })
    }

    fn managed() -> ManagementRelation {
        ManagementRelation::Managed {
            since: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn adopted() -> ManagementRelation {
        ManagementRelation::Adopted {
            since: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn facts() -> Facts {
        Facts {
            scope: InstallationScope::System,
            record: RecordFacts::Absent,
            native: NativeProbe::Absent,
            pending_journal: false,
            active_adapter_claims: Vec::new(),
            owned_files_verified: None,
        }
    }

    fn present() -> NativeProbe {
        NativeProbe::Present {
            package: PKG.to_string(),
            observation: observation(),
        }
    }

    fn owned_target() -> ProviderTarget {
        ProviderTarget::Owned {
            version: "1.2.3".to_string(),
        }
    }

    fn delegated_target() -> ProviderTarget {
        ProviderTarget::Delegated {
            pm: NativePm::Rpm,
            package: PKG.to_string(),
            artifact: None,
        }
    }

    fn delegated_pinned_target(artifact: &str) -> ProviderTarget {
        ProviderTarget::Delegated {
            pm: NativePm::Rpm,
            package: PKG.to_string(),
            artifact: Some(artifact.to_string()),
        }
    }

    fn install(target: ProviderTarget) -> Intent {
        Intent::Install(InstallRequest {
            target,
            requested_version: None,
        })
    }

    fn update() -> Intent {
        Intent::Update(UpdateRequest {
            owned_resolution: None,
        })
    }

    fn update_resolved(relation: VersionRelation) -> Intent {
        Intent::Update(UpdateRequest {
            owned_resolution: Some(OwnedUpdateResolution {
                to_version: "2.0.0".to_string(),
                relation,
            }),
        })
    }

    fn uninstall(remove_system_package: bool) -> Intent {
        Intent::Uninstall(UninstallRequest {
            remove_system_package,
            invocation: InvocationForm::SingleNamed,
        })
    }

    fn native_txn(action: NativeAction) -> Step {
        Step::NativeTransaction {
            pm: NativePm::Rpm,
            action,
            packages: vec![PKG.to_string()],
        }
    }

    fn observe() -> Step {
        Step::Observe {
            packages: vec![PKG.to_string()],
        }
    }

    fn expect_steps(result: Result<Plan, PlanError>) -> Vec<Step> {
        match result {
            Ok(Plan::Execute { steps, .. }) => steps,
            other => panic!("expected executable plan, got {other:?}"),
        }
    }

    // ---- global preflight -----------------------------------------

    #[test]
    fn pending_journal_blocks_every_intent_except_repair() {
        let mut f = facts();
        f.pending_journal = true;
        f.record = RecordFacts::Active(delegated_record(managed()));
        f.native = present();
        let intents = [
            install(delegated_target()),
            Intent::Adopt,
            update(),
            uninstall(false),
        ];
        for intent in intents {
            assert_eq!(
                plan(&intent, &f),
                Err(PlanError::PendingOperation),
                "intent {intent:?} must be blocked by a pending journal"
            );
        }
        // Repair consumes the journal instead (R1).
        assert_eq!(
            plan(&Intent::Repair, &f),
            Ok(Plan::execute(vec![Step::RecoverJournal]))
        );
    }

    // ---- install ---------------------------------------------------

    #[test]
    fn i1_fresh_owned_install() {
        let f = facts();
        let steps = expect_steps(plan(&install(owned_target()), &f));
        assert_eq!(
            steps,
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
        );
    }

    #[test]
    fn i2_fresh_delegated_install() {
        let f = facts();
        let steps = expect_steps(plan(&install(delegated_target()), &f));
        assert_eq!(
            steps,
            vec![
                native_txn(NativeAction::Install),
                observe(),
                Step::WriteRecord(RecordWrite::DelegatedManaged),
            ]
        );
    }

    #[test]
    fn i2_pinned_delegated_install_targets_artifact_but_observes_bare_package() {
        // A version pin renders the native transaction to the exact NEVRA
        // while observation stays on the bare package identity.
        let f = facts();
        let artifact = format!("{PKG}-1.2.3-4.al8.x86_64");
        let steps = expect_steps(plan(&install(delegated_pinned_target(&artifact)), &f));
        assert_eq!(
            steps,
            vec![
                Step::NativeTransaction {
                    pm: NativePm::Rpm,
                    action: NativeAction::Install,
                    packages: vec![artifact.clone()],
                },
                observe(),
                Step::WriteRecord(RecordWrite::DelegatedManaged),
            ]
        );
    }

    #[test]
    fn i3_existing_system_package_is_never_silently_adopted() {
        let mut f = facts();
        f.native = present();
        for target in [owned_target(), delegated_target()] {
            assert_eq!(
                plan(&install(target), &f),
                Err(PlanError::AlreadyPresentOnSystem)
            );
        }
    }

    #[test]
    fn i4_i5_install_over_owned_record_is_idempotent_or_redirects() {
        let mut f = facts();
        f.record = RecordFacts::Active(owned_record());
        assert_eq!(
            plan(&install(owned_target()), &f),
            Ok(Plan::NoOp {
                reason: NoOpReason::AlreadyInstalled
            })
        );
        // Same explicit version → still a no-op.
        let same = Intent::Install(InstallRequest {
            target: owned_target(),
            requested_version: Some("1.2.3".to_string()),
        });
        assert_eq!(
            plan(&same, &f),
            Ok(Plan::NoOp {
                reason: NoOpReason::AlreadyInstalled
            })
        );
        // Different version → update's job, fails at planning (I5).
        let other = Intent::Install(InstallRequest {
            target: owned_target(),
            requested_version: Some("2.0.0".to_string()),
        });
        assert_eq!(plan(&other, &f), Err(PlanError::UseUpdate));
    }

    #[test]
    fn i6_i7_managed_record_errors_by_presence() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(managed()));
        f.native = present();
        assert_eq!(
            plan(&install(delegated_target()), &f),
            Err(PlanError::AlreadyManaged)
        );
        f.native = NativeProbe::Absent;
        assert_eq!(
            plan(&install(delegated_target()), &f),
            Err(PlanError::ExternallyRemoved)
        );
    }

    #[test]
    fn i8_i9_tracked_record_noop_or_forget() {
        for relation in [adopted(), ManagementRelation::Observed] {
            let mut f = facts();
            f.record = RecordFacts::Active(delegated_record(relation));
            f.native = present();
            assert_eq!(
                plan(&install(delegated_target()), &f),
                Ok(Plan::NoOp {
                    reason: NoOpReason::AlreadyTracked
                })
            );
            f.native = NativeProbe::Absent;
            assert_eq!(
                plan(&install(delegated_target()), &f),
                Err(PlanError::TrackedButAbsent)
            );
        }
    }

    #[test]
    fn i10_quarantined_blocks_install() {
        let mut f = facts();
        f.record = RecordFacts::Quarantined(QuarantineReason::NoEvidence);
        assert_eq!(
            plan(&install(owned_target()), &f),
            Err(PlanError::NeedsAttention)
        );
    }

    #[test]
    fn i11_provenance_conflicts_fail_before_state_arms() {
        // Family switch: owned record, delegated target.
        let mut f = facts();
        f.record = RecordFacts::Active(owned_record());
        f.native = present();
        assert_eq!(
            plan(&install(delegated_target()), &f),
            Err(PlanError::ProvenanceConflict)
        );
        // Package repoint on a delegated record.
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(managed()));
        f.native = present();
        let repointed = Intent::Install(InstallRequest {
            target: ProviderTarget::Delegated {
                pm: NativePm::Rpm,
                package: "other".to_string(),
                artifact: None,
            },
            requested_version: None,
        });
        assert_eq!(plan(&repointed, &f), Err(PlanError::ProvenanceConflict));
    }

    #[test]
    fn user_scope_rejects_delegated_target_and_skips_rpm_probe() {
        let mut f = facts();
        f.scope = InstallationScope::User { uid: 1000 };
        f.native = NativeProbe::NotProbed;
        assert_eq!(
            plan(&install(delegated_target()), &f),
            Err(PlanError::DelegatedRequiresSystemScope)
        );
        // Owned install proceeds without any native probe in user scope.
        let steps = expect_steps(plan(&install(owned_target()), &f));
        assert_eq!(steps.last(), Some(&Step::WriteRecord(RecordWrite::Owned)));
    }

    // ---- adopt -------------------------------------------------------

    #[test]
    fn a1_adopt_unique_package() {
        let mut f = facts();
        f.native = present();
        let steps = expect_steps(plan(&Intent::Adopt, &f));
        assert_eq!(
            steps,
            vec![observe(), Step::WriteRecord(RecordWrite::DelegatedAdopted)]
        );
    }

    #[test]
    fn a2_a3_absent_or_ambiguous() {
        let f = facts();
        assert_eq!(plan(&Intent::Adopt, &f), Err(PlanError::NothingToAdopt));
        let mut f = facts();
        f.native = NativeProbe::MultipleVersions {
            package: PKG.to_string(),
        };
        assert_eq!(plan(&Intent::Adopt, &f), Err(PlanError::AmbiguousPackage));
    }

    #[test]
    fn a4_a5_wrong_provenance_refuses_adopt() {
        let mut f = facts();
        f.record = RecordFacts::Active(owned_record());
        assert_eq!(plan(&Intent::Adopt, &f), Err(PlanError::ProvenanceConflict));
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(managed()));
        f.native = present();
        assert_eq!(plan(&Intent::Adopt, &f), Err(PlanError::AlreadyManaged));
    }

    #[test]
    fn a6_a7_observed_upgrades_adopted_is_idempotent() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(ManagementRelation::Observed));
        f.native = present();
        assert_eq!(
            expect_steps(plan(&Intent::Adopt, &f)),
            vec![Step::WriteRecord(RecordWrite::DelegatedAdopted)]
        );
        f.record = RecordFacts::Active(delegated_record(adopted()));
        assert_eq!(
            plan(&Intent::Adopt, &f),
            Ok(Plan::NoOp {
                reason: NoOpReason::AlreadyAdopted
            })
        );
    }

    #[test]
    fn adopt_requires_system_scope() {
        let mut f = facts();
        f.scope = InstallationScope::User { uid: 1000 };
        f.native = present();
        assert_eq!(
            plan(&Intent::Adopt, &f),
            Err(PlanError::AdoptRequiresSystemScope)
        );
    }

    // ---- update -------------------------------------------------------

    #[test]
    fn u1_u8_update_terminal_records() {
        let f = facts();
        assert_eq!(plan(&update(), &f), Err(PlanError::NotInstalled));
        let mut f = facts();
        f.record = RecordFacts::Quarantined(QuarantineReason::NoEvidence);
        assert_eq!(plan(&update(), &f), Err(PlanError::NeedsAttention));
    }

    #[test]
    fn u2_u3_u4_owned_update_gates_on_version_relation() {
        let mut f = facts();
        f.record = RecordFacts::Active(owned_record());
        assert_eq!(
            plan(&update_resolved(VersionRelation::Same), &f),
            Ok(Plan::NoOp {
                reason: NoOpReason::AlreadyLatest
            })
        );
        assert_eq!(
            expect_steps(plan(&update_resolved(VersionRelation::Newer), &f)),
            vec![
                Step::BackupFiles,
                Step::DownloadVerify,
                Step::RemoveOwnedFiles,
                Step::PlaceFiles,
                Step::SetCapabilities,
                Step::RestartServices,
                Step::WriteRecord(RecordWrite::Owned),
            ]
        );
        assert_eq!(
            plan(&update_resolved(VersionRelation::Older), &f),
            Err(PlanError::RefuseDowngrade)
        );
        assert_eq!(
            plan(&update_resolved(VersionRelation::Indeterminate), &f),
            Err(PlanError::IndeterminateVersion)
        );
        // Missing resolution on an owned record is a caller bug.
        assert_eq!(plan(&update(), &f), Err(PlanError::MissingUpdateResolution));
    }

    #[test]
    fn u5_delegated_update_for_managed_and_adopted() {
        for relation in [managed(), adopted()] {
            let mut f = facts();
            f.record = RecordFacts::Active(delegated_record(relation));
            f.native = present();
            assert_eq!(
                expect_steps(plan(&update(), &f)),
                vec![
                    native_txn(NativeAction::Update),
                    observe(),
                    Step::WriteRecord(RecordWrite::RefreshObservation),
                ]
            );
        }
    }

    #[test]
    fn u6_observed_requires_adoption_first() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(ManagementRelation::Observed));
        f.native = present();
        assert_eq!(plan(&update(), &f), Err(PlanError::NotAdopted));
    }

    #[test]
    fn u7_externally_removed_package() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(managed()));
        f.native = NativeProbe::Absent;
        assert_eq!(plan(&update(), &f), Err(PlanError::ExternallyRemoved));
    }

    // ---- repair -------------------------------------------------------

    #[test]
    fn r2_owned_repair_replays_only_when_broken() {
        let mut f = facts();
        f.record = RecordFacts::Active(owned_record());
        f.owned_files_verified = Some(false);
        assert_eq!(
            expect_steps(plan(&Intent::Repair, &f)),
            owned_replay_steps()
        );
        f.owned_files_verified = Some(true);
        assert_eq!(
            plan(&Intent::Repair, &f),
            Ok(Plan::NoOp {
                reason: NoOpReason::NothingToRepair
            })
        );
    }

    #[test]
    fn r3_delegated_repair_absorbs_observation() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(ManagementRelation::Observed));
        f.native = present();
        assert_eq!(
            expect_steps(plan(&Intent::Repair, &f)),
            vec![
                observe(),
                Step::WriteRecord(RecordWrite::RefreshObservation),
            ]
        );
    }

    #[test]
    fn r4_only_managed_reinstalls_missing_packages() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(managed()));
        f.native = NativeProbe::Absent;
        assert_eq!(
            expect_steps(plan(&Intent::Repair, &f)),
            vec![
                native_txn(NativeAction::Install),
                observe(),
                Step::WriteRecord(RecordWrite::RefreshObservation),
            ]
        );
        for relation in [adopted(), ManagementRelation::Observed] {
            let mut f = facts();
            f.record = RecordFacts::Active(delegated_record(relation));
            f.native = NativeProbe::Absent;
            assert_eq!(plan(&Intent::Repair, &f), Err(PlanError::TrackedButAbsent));
        }
    }

    #[test]
    fn r5_r6_quarantine_exits() {
        // R5: the native db still knows the package → rebuild as observed.
        let mut f = facts();
        f.record = RecordFacts::Quarantined(QuarantineReason::NoEvidence);
        f.native = present();
        assert_eq!(
            expect_steps(plan(&Intent::Repair, &f)),
            vec![observe(), Step::WriteRecord(RecordWrite::DelegatedObserved),]
        );
        // R6: no native trace but the file list verifies → rebuild owned.
        let mut f = facts();
        f.record = RecordFacts::Quarantined(QuarantineReason::NoEvidence);
        f.owned_files_verified = Some(true);
        assert_eq!(
            expect_steps(plan(&Intent::Repair, &f)),
            vec![Step::WriteRecord(RecordWrite::Owned)]
        );
        // Neither → unrecoverable, suggest forget.
        let mut f = facts();
        f.record = RecordFacts::Quarantined(QuarantineReason::NoEvidence);
        assert_eq!(
            plan(&Intent::Repair, &f),
            Err(PlanError::RecordUnrecoverable)
        );
        // An ambiguous probe refuses the rebuild, matching the active rows:
        // never seed an observation from an arbitrary EVR.
        let mut f = facts();
        f.record = RecordFacts::Quarantined(QuarantineReason::NoEvidence);
        f.native = NativeProbe::MultipleVersions {
            package: PKG.to_string(),
        };
        assert_eq!(plan(&Intent::Repair, &f), Err(PlanError::AmbiguousPackage));
    }

    #[test]
    fn r7_repair_without_record() {
        let f = facts();
        assert_eq!(plan(&Intent::Repair, &f), Err(PlanError::NotInstalled));
    }

    // ---- uninstall ----------------------------------------------------

    #[test]
    fn x1_owned_teardown() {
        let mut f = facts();
        f.record = RecordFacts::Active(owned_record());
        assert_eq!(
            expect_steps(plan(&uninstall(false), &f)),
            vec![
                Step::RunHook(HookKind::PreUninstall),
                Step::StopServices,
                Step::RemoveOwnedFiles,
                Step::RunHook(HookKind::PostUninstall),
                Step::DropRecord,
            ]
        );
    }

    #[test]
    fn x2_x3_managed_removal_by_presence() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(managed()));
        f.native = present();
        assert_eq!(
            expect_steps(plan(&uninstall(false), &f)),
            vec![native_txn(NativeAction::Remove), Step::DropRecord,]
        );
        f.native = NativeProbe::Absent;
        assert_eq!(
            plan(&uninstall(false), &f),
            Ok(Plan::Execute {
                steps: vec![Step::DropRecord],
                notes: vec![PlanNote::PackageAlreadyAbsent],
            })
        );
    }

    #[test]
    fn x4_tracked_records_drop_record_unless_flagged() {
        for relation in [adopted(), ManagementRelation::Observed] {
            let mut f = facts();
            f.record = RecordFacts::Active(delegated_record(relation));
            f.native = present();
            assert_eq!(
                expect_steps(plan(&uninstall(false), &f)),
                vec![Step::DropRecord]
            );
            assert_eq!(
                expect_steps(plan(&uninstall(true), &f)),
                vec![native_txn(NativeAction::Remove), Step::DropRecord,]
            );
        }
    }

    #[test]
    fn x4_flag_is_single_invocation_only() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(adopted()));
        f.native = present();
        let batched = Intent::Uninstall(UninstallRequest {
            remove_system_package: true,
            invocation: InvocationForm::Batch,
        });
        assert_eq!(
            plan(&batched, &f),
            Err(PlanError::RemoveSystemPackageRequiresSingleTarget)
        );
    }

    #[test]
    fn x5_quarantined_never_reaches_a_native_remove() {
        let mut f = facts();
        f.record = RecordFacts::Quarantined(QuarantineReason::NoEvidence);
        assert_eq!(
            expect_steps(plan(&uninstall(false), &f)),
            vec![Step::DropRecord]
        );
        assert_eq!(plan(&uninstall(true), &f), Err(PlanError::NeedsAttention));
    }

    #[test]
    fn x6_uninstall_without_record() {
        let f = facts();
        assert_eq!(plan(&uninstall(false), &f), Err(PlanError::NotInstalled));
    }

    #[test]
    fn adapter_claims_block_uninstall() {
        let mut f = facts();
        f.record = RecordFacts::Active(owned_record());
        f.active_adapter_claims = vec!["openclaw".to_string()];
        assert_eq!(
            plan(&uninstall(false), &f),
            Err(PlanError::AdapterClaimsActive)
        );
    }

    // ---- cross-cutting safety ----------------------------------------------

    #[test]
    fn unresolved_package_identity_never_reaches_a_transaction() {
        let record = installation(ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package: PackageIdentity::Unresolved {
                component_hint: PKG.to_string(),
            },
            relation: managed(),
            last_observed: None,
        });
        // Native probe also failed to resolve → no transaction possible.
        let mut f = facts();
        f.record = RecordFacts::Active(record.clone());
        f.native = NativeProbe::Absent;
        assert_eq!(plan(&Intent::Repair, &f), Err(PlanError::PackageUnresolved));
        // The probe resolved a name → transactions may proceed with it.
        let mut f = facts();
        f.record = RecordFacts::Active(record);
        f.native = present();
        assert_eq!(
            expect_steps(plan(&update(), &f)),
            vec![
                native_txn(NativeAction::Update),
                observe(),
                Step::WriteRecord(RecordWrite::RefreshObservation),
            ]
        );
    }

    #[test]
    fn delegated_paths_require_a_probe() {
        let mut f = facts();
        f.record = RecordFacts::Active(delegated_record(managed()));
        f.native = NativeProbe::NotProbed;
        for intent in [install(delegated_target()), update(), Intent::Repair] {
            assert_eq!(
                plan(&intent, &f),
                Err(PlanError::MissingNativeProbe),
                "intent {intent:?} must demand a native probe"
            );
        }
    }
}
