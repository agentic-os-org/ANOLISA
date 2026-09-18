//! Shared component observations used by lifecycle consumers.
//!
//! A snapshot records which probes were requested separately from the evidence
//! they produced, so unavailable observations cannot be confused with absence.

use std::collections::BTreeSet;
use std::path::PathBuf;

use thiserror::Error;

use crate::adapter::claim::ClaimStatus;
use crate::domain::{Installation, InstallationScope, NativePm, Observation};
use crate::health::CheckOutcome;
use crate::integrity::IntegrityStatus;
use crate::state::ObjectKind;
use crate::state_migration::QuarantineReason;

/// A component fact that may not have been requested or observable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeEvidence<T, P> {
    /// The snapshot request did not include this probe.
    NotRequested,
    /// The probe was attempted, but its source could not provide a result.
    Unavailable {
        /// Source consulted by the failed probe.
        provenance: P,
        /// Developer-facing explanation of the failure.
        reason: String,
    },
    /// The source was consulted and confirmed that the fact does not exist.
    Absent {
        /// Source that established absence.
        provenance: P,
    },
    /// The source was consulted and returned a value.
    Present {
        /// Source that produced the value.
        provenance: P,
        /// Observed value.
        value: T,
    },
}

impl<T, P> ProbeEvidence<T, P> {
    fn is_not_requested(&self) -> bool {
        matches!(self, Self::NotRequested)
    }
}

/// Probe kinds supported by the component snapshot contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SnapshotProbe {
    /// Read the ANOLISA installation state.
    State,
    /// Verify the files owned by the active or quarantined component record.
    OwnedFiles,
    /// Query the native package authority. Valid only in system scope.
    NativePackage,
    /// Execute the installed manifest's read-only health check.
    ManifestHealth,
    /// Observe adapter declarations, resources, receipts, and runtime support.
    Adapters,
    /// Inspect the transaction journal directory.
    PendingJournal,
}

/// Explicit set of observations needed by a lifecycle consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentSnapshotRequest {
    component: String,
    scope: InstallationScope,
    probes: BTreeSet<SnapshotProbe>,
}

impl ComponentSnapshotRequest {
    /// Creates a request for a resolved component and installation scope.
    pub fn new(
        component: impl Into<String>,
        scope: InstallationScope,
        probes: impl IntoIterator<Item = SnapshotProbe>,
    ) -> Self {
        Self {
            component: component.into(),
            scope,
            probes: probes.into_iter().collect(),
        }
    }

    /// Returns the resolved component name.
    pub fn component(&self) -> &str {
        &self.component
    }

    /// Returns the installation scope being observed.
    pub fn scope(&self) -> InstallationScope {
        self.scope
    }

    /// Returns whether the request includes a probe.
    pub fn requests(&self, probe: SnapshotProbe) -> bool {
        self.probes.contains(&probe)
    }

    /// Returns the requested probes in deterministic order.
    pub fn probes(&self) -> impl Iterator<Item = SnapshotProbe> + '_ {
        self.probes.iter().copied()
    }
}

/// Source of an ANOLISA installation-state observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateProvenance {
    /// State file that was consulted.
    pub path: PathBuf,
}

/// Multi-root visibility attached to an installation-state observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateVisibilitySnapshot {
    /// State root that supplied this record.
    pub root_scope: StateRootScope,
    /// Whether this record wins scope precedence for its component identity.
    pub active: bool,
    /// Whether the current invocation may mutate the record's state root.
    pub mutable_by_current_invocation: bool,
    /// Higher-precedence scope hiding this record, when shadowed.
    pub shadowed_by: Option<StateRootScope>,
}

/// Scope label used by multi-root read-only views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateRootScope {
    /// Per-user state root.
    User,
    /// Host-wide state root.
    System,
}

/// Source of a native package observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePackageProvenance {
    /// Native authority that was queried.
    pub manager: NativePm,
    /// Package name used for the query.
    pub package: String,
}

/// Source of an owned-file integrity observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedFilesProvenance {
    /// State file whose owned-file contract selected the paths to verify.
    pub state_path: PathBuf,
}

/// Source of an installed-manifest health observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestHealthProvenance {
    /// Installed manifest snapshot that declared the health check.
    pub path: PathBuf,
}

/// Sources consulted by an adapter observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterProvenance {
    /// Visible state roots whose receipts and component records were scanned.
    pub state_paths: Vec<PathBuf>,
}

/// Source of a pending-journal observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalProvenance {
    /// Transaction journal directory that was inspected.
    pub directory: PathBuf,
}

/// Installation-state values returned by the state probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateSnapshot {
    /// A valid installation is present in state.
    Active(Box<Installation>),
    /// A preserved legacy record cannot safely participate in lifecycle actions.
    Quarantined(QuarantineReason),
}

/// Native package values returned by the native authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativePackageSnapshot {
    /// Exactly one installed package version was observed.
    Installed(Observation),
    /// More than one installed version makes the package observation ambiguous.
    MultipleVersions,
    /// The package authority returned output that could not identify one version.
    UnexpectedOutput {
        /// Backend detail retained for drift diagnostics.
        detail: String,
    },
}

/// Aggregate integrity of the files declared by an owned component record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnedFilesVerdict {
    /// Every verifiable owned path satisfies its recorded contract.
    Verified,
    /// At least one owned path has a decisive integrity failure.
    Drifted,
    /// At least one path could not be checked within the probe budget.
    Inconclusive,
}

/// One owned path and the result of its read-only integrity probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedFileObservation {
    /// Path declared by the component's ownership contract.
    pub path: PathBuf,
    /// Integrity result observed for the path.
    pub status: IntegrityStatus,
}

/// Integrity observations for all files declared by an owned component record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedFilesSnapshot {
    /// Aggregate verdict used by lifecycle planning.
    pub verdict: OwnedFilesVerdict,
    /// Per-path observations retained for read-only consumers.
    pub files: Vec<OwnedFileObservation>,
}

/// Result of an installed manifest's structured health check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestHealthSnapshot {
    /// Typed outcome tree returned by the shared health engine.
    pub outcome: CheckOutcome,
}

/// Availability of the component source behind an enabled adapter receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterSourceSnapshot {
    /// The component and adapter resource are still visible.
    Available,
    /// The receipt remains but its component or adapter resource is missing.
    Missing,
}

/// Stable adapter observation consumed independently of manager internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterObservation {
    /// Component that owns the adapter.
    pub component: String,
    /// Framework targeted by the adapter.
    pub framework: String,
    /// Whether an installed component manifest declares the adapter.
    pub declared: bool,
    /// Adapter resource directory when one is visible.
    pub resource_root: Option<PathBuf>,
    /// Whether a built-in driver exists for the framework.
    pub driver_available: bool,
    /// Whether the framework was detected on the host.
    pub framework_detected: bool,
    /// Adapter type declared by the installed manifest, when available.
    pub adapter_type: Option<String>,
    /// Whether an adapter receipt exists in state.
    pub enabled: bool,
    /// Lifecycle status of the receipt, when one exists.
    pub claim_status: Option<ClaimStatus>,
    /// Availability of the source behind an enabled receipt.
    pub source_status: Option<AdapterSourceSnapshot>,
    /// Explanation for unavailable adapter source evidence.
    pub source_reason: Option<String>,
}

/// Pending transaction selected for the requested component.
///
/// A collector may inspect a multi-entry journal inventory, but each snapshot
/// carries the single entry that determines this component's lifecycle decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingJournalSnapshot {
    /// Path to the selected pending journal file.
    pub path: PathBuf,
}

/// Consistent component observations for one explicit request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentSnapshot {
    request: ComponentSnapshotRequest,
    state: ProbeEvidence<StateSnapshot, StateProvenance>,
    state_visibility: Option<StateVisibilitySnapshot>,
    owned_files: ProbeEvidence<OwnedFilesSnapshot, OwnedFilesProvenance>,
    native_package: ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
    manifest_health: ProbeEvidence<ManifestHealthSnapshot, ManifestHealthProvenance>,
    adapters: ProbeEvidence<Vec<AdapterObservation>, AdapterProvenance>,
    pending_journal: ProbeEvidence<PendingJournalSnapshot, JournalProvenance>,
}

/// Evidence bundle supplied to [`ComponentSnapshot::from_observations`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentSnapshotObservations {
    /// Installation-state evidence.
    pub state: ProbeEvidence<StateSnapshot, StateProvenance>,
    /// Optional multi-root visibility for active state.
    pub state_visibility: Option<StateVisibilitySnapshot>,
    /// Owned-file integrity evidence.
    pub owned_files: ProbeEvidence<OwnedFilesSnapshot, OwnedFilesProvenance>,
    /// Native package evidence.
    pub native_package: ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
    /// Installed-manifest health evidence.
    pub manifest_health: ProbeEvidence<ManifestHealthSnapshot, ManifestHealthProvenance>,
    /// Adapter evidence.
    pub adapters: ProbeEvidence<Vec<AdapterObservation>, AdapterProvenance>,
    /// Pending transaction-journal evidence.
    pub pending_journal: ProbeEvidence<PendingJournalSnapshot, JournalProvenance>,
}

impl ComponentSnapshot {
    /// Builds a snapshot that may include owned-file integrity evidence.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotContractError`] when a probe is unsupported in the
    /// requested scope, evidence disagrees with the request, or active state
    /// belongs to a different component identity.
    pub fn from_parts_with_owned_files(
        request: ComponentSnapshotRequest,
        state: ProbeEvidence<StateSnapshot, StateProvenance>,
        owned_files: ProbeEvidence<OwnedFilesSnapshot, OwnedFilesProvenance>,
        native_package: ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
        pending_journal: ProbeEvidence<PendingJournalSnapshot, JournalProvenance>,
    ) -> Result<Self, SnapshotContractError> {
        Self::from_observations(
            request,
            ComponentSnapshotObservations {
                state,
                state_visibility: None,
                owned_files,
                native_package,
                manifest_health: ProbeEvidence::NotRequested,
                adapters: ProbeEvidence::NotRequested,
                pending_journal,
            },
        )
    }

    /// Builds a snapshot with the observations needed by read-only consumers.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotContractError`] when evidence disagrees with the
    /// request or visibility metadata is attached without active state.
    pub fn from_observations(
        request: ComponentSnapshotRequest,
        observations: ComponentSnapshotObservations,
    ) -> Result<Self, SnapshotContractError> {
        let ComponentSnapshotObservations {
            state,
            state_visibility,
            owned_files,
            native_package,
            manifest_health,
            adapters,
            pending_journal,
        } = observations;
        if matches!(request.scope, InstallationScope::User { .. })
            && request.requests(SnapshotProbe::NativePackage)
        {
            return Err(SnapshotContractError::UnsupportedProbeScope {
                probe: SnapshotProbe::NativePackage,
                scope: request.scope,
            });
        }
        validate_evidence(&request, SnapshotProbe::State, state.is_not_requested())?;
        validate_state_target(&request, &state)?;
        if let Some(visibility) = &state_visibility {
            let expected = state_root_scope(request.scope);
            if visibility.root_scope != expected {
                return Err(SnapshotContractError::StateRootScopeMismatch {
                    expected,
                    actual: visibility.root_scope,
                });
            }
        }
        if state_visibility.is_some()
            && !matches!(
                &state,
                ProbeEvidence::Present {
                    value: StateSnapshot::Active(_),
                    ..
                }
            )
        {
            return Err(SnapshotContractError::StateVisibilityWithoutActiveState);
        }
        validate_evidence(
            &request,
            SnapshotProbe::OwnedFiles,
            owned_files.is_not_requested(),
        )?;
        validate_evidence(
            &request,
            SnapshotProbe::NativePackage,
            native_package.is_not_requested(),
        )?;
        validate_evidence(
            &request,
            SnapshotProbe::ManifestHealth,
            manifest_health.is_not_requested(),
        )?;
        validate_evidence(
            &request,
            SnapshotProbe::Adapters,
            adapters.is_not_requested(),
        )?;
        validate_evidence(
            &request,
            SnapshotProbe::PendingJournal,
            pending_journal.is_not_requested(),
        )?;

        Ok(Self {
            request,
            state,
            state_visibility,
            owned_files,
            native_package,
            manifest_health,
            adapters,
            pending_journal,
        })
    }

    /// Returns the request that defines this snapshot.
    pub fn request(&self) -> &ComponentSnapshotRequest {
        &self.request
    }

    /// Returns the installation-state evidence.
    pub fn state(&self) -> &ProbeEvidence<StateSnapshot, StateProvenance> {
        &self.state
    }

    /// Returns multi-root visibility attached to the state observation.
    pub fn state_visibility(&self) -> Option<&StateVisibilitySnapshot> {
        self.state_visibility.as_ref()
    }

    /// Returns the owned-file integrity evidence.
    pub fn owned_files(&self) -> &ProbeEvidence<OwnedFilesSnapshot, OwnedFilesProvenance> {
        &self.owned_files
    }

    /// Returns the native package evidence.
    pub fn native_package(&self) -> &ProbeEvidence<NativePackageSnapshot, NativePackageProvenance> {
        &self.native_package
    }

    /// Returns installed-manifest health evidence.
    pub fn manifest_health(
        &self,
    ) -> &ProbeEvidence<ManifestHealthSnapshot, ManifestHealthProvenance> {
        &self.manifest_health
    }

    /// Returns adapter evidence.
    pub fn adapters(&self) -> &ProbeEvidence<Vec<AdapterObservation>, AdapterProvenance> {
        &self.adapters
    }

    /// Returns the pending-journal evidence.
    pub fn pending_journal(&self) -> &ProbeEvidence<PendingJournalSnapshot, JournalProvenance> {
        &self.pending_journal
    }
}

/// Contract violation between a snapshot request and its evidence.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SnapshotContractError {
    /// A requested probe was represented as not requested.
    #[error("requested probe {probe:?} has no evidence")]
    MissingRequestedEvidence {
        /// Probe whose evidence is missing.
        probe: SnapshotProbe,
    },
    /// An unrequested probe unexpectedly contains evidence.
    #[error("unrequested probe {probe:?} contains evidence")]
    UnexpectedUnrequestedEvidence {
        /// Probe that unexpectedly contains evidence.
        probe: SnapshotProbe,
    },
    /// A probe cannot produce facts for the requested installation scope.
    #[error("probe {probe:?} is not supported for scope {scope:?}")]
    UnsupportedProbeScope {
        /// Probe that is unavailable in the requested scope.
        probe: SnapshotProbe,
        /// Installation scope that cannot use the probe.
        scope: InstallationScope,
    },
    /// Active state belongs to a different component identity.
    #[error(
        "active state target mismatch: expected Component {expected_component:?} in \
         {expected_scope:?}, got {actual_kind:?} {actual_name:?} in {actual_scope:?}"
    )]
    ActiveStateTargetMismatch {
        /// Component name requested by the snapshot.
        expected_component: String,
        /// Installation scope requested by the snapshot.
        expected_scope: InstallationScope,
        /// Object kind carried by the active state.
        actual_kind: ObjectKind,
        /// Object name carried by the active state.
        actual_name: String,
        /// Installation scope carried by the active state.
        actual_scope: InstallationScope,
    },
    /// Multi-root visibility was attached without an active installation.
    #[error("state visibility requires active installation evidence")]
    StateVisibilityWithoutActiveState,
    /// Visibility metadata names a different root scope than the request.
    #[error("state root scope mismatch: expected {expected:?}, got {actual:?}")]
    StateRootScopeMismatch {
        /// Scope implied by the snapshot request.
        expected: StateRootScope,
        /// Scope carried by the visibility metadata.
        actual: StateRootScope,
    },
}

fn state_root_scope(scope: InstallationScope) -> StateRootScope {
    match scope {
        InstallationScope::System => StateRootScope::System,
        InstallationScope::User { .. } => StateRootScope::User,
    }
}

fn validate_evidence(
    request: &ComponentSnapshotRequest,
    probe: SnapshotProbe,
    not_requested: bool,
) -> Result<(), SnapshotContractError> {
    match (request.requests(probe), not_requested) {
        (true, true) => Err(SnapshotContractError::MissingRequestedEvidence { probe }),
        (false, false) => Err(SnapshotContractError::UnexpectedUnrequestedEvidence { probe }),
        _ => Ok(()),
    }
}

fn validate_state_target(
    request: &ComponentSnapshotRequest,
    state: &ProbeEvidence<StateSnapshot, StateProvenance>,
) -> Result<(), SnapshotContractError> {
    let ProbeEvidence::Present {
        value: StateSnapshot::Active(installation),
        ..
    } = state
    else {
        return Ok(());
    };

    if installation.kind == ObjectKind::Component
        && installation.name == request.component
        && installation.scope == request.scope
    {
        return Ok(());
    }

    Err(SnapshotContractError::ActiveStateTargetMismatch {
        expected_component: request.component.clone(),
        expected_scope: request.scope,
        actual_kind: installation.kind,
        actual_name: installation.name.clone(),
        actual_scope: installation.scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LifecycleStatus, ManagementRelation, PackageIdentity, ProviderBinding};
    use crate::health::CheckStatus;
    use crate::state::SubscriptionScope;
    use crate::state_migration::QuarantineReason;

    fn active_installation() -> Installation {
        Installation {
            kind: ObjectKind::Component,
            name: "tokenless".to_string(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved {
                    name: "tokenless".to_string(),
                },
                relation: ManagementRelation::Observed,
                last_observed: None,
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-08-19T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: SubscriptionScope::None,
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    fn state_source() -> StateProvenance {
        StateProvenance {
            path: PathBuf::from("/var/lib/anolisa/state.json"),
        }
    }

    fn native_source() -> NativePackageProvenance {
        NativePackageProvenance {
            manager: NativePm::Rpm,
            package: "tokenless".to_string(),
        }
    }

    fn journal_source() -> JournalProvenance {
        JournalProvenance {
            directory: PathBuf::from("/var/lib/anolisa/transactions"),
        }
    }

    fn all_probes_request() -> ComponentSnapshotRequest {
        ComponentSnapshotRequest::new(
            "tokenless",
            InstallationScope::System,
            [
                SnapshotProbe::State,
                SnapshotProbe::NativePackage,
                SnapshotProbe::PendingJournal,
            ],
        )
    }

    #[test]
    fn request_deduplicates_probes_in_deterministic_order() {
        let request = ComponentSnapshotRequest::new(
            "tokenless",
            InstallationScope::System,
            [
                SnapshotProbe::PendingJournal,
                SnapshotProbe::State,
                SnapshotProbe::State,
            ],
        );

        assert_eq!(request.component(), "tokenless");
        assert_eq!(request.scope(), InstallationScope::System);
        assert_eq!(
            request.probes().collect::<Vec<_>>(),
            vec![SnapshotProbe::State, SnapshotProbe::PendingJournal]
        );
    }

    #[test]
    fn snapshot_preserves_typed_values_and_provenance() {
        let installation = active_installation();
        let observation = Observation {
            version: "0.7.7".to_string(),
            evr: Some("0:0.7.7-1".to_string()),
            arch: Some("x86_64".to_string()),
            source_repo: Some("anolisa".to_string()),
            observed_at: "2026-08-19T00:00:00Z".to_string(),
        };
        let snapshot = ComponentSnapshot::from_parts_with_owned_files(
            all_probes_request(),
            ProbeEvidence::Present {
                provenance: state_source(),
                value: StateSnapshot::Active(Box::new(installation.clone())),
            },
            ProbeEvidence::NotRequested,
            ProbeEvidence::Present {
                provenance: native_source(),
                value: NativePackageSnapshot::Installed(observation.clone()),
            },
            ProbeEvidence::Present {
                provenance: journal_source(),
                value: PendingJournalSnapshot {
                    path: PathBuf::from("/var/lib/anolisa/transactions/pending.json"),
                },
            },
        )
        .unwrap();

        assert_eq!(
            snapshot.state(),
            &ProbeEvidence::Present {
                provenance: state_source(),
                value: StateSnapshot::Active(Box::new(installation)),
            }
        );
        assert_eq!(
            snapshot.native_package(),
            &ProbeEvidence::Present {
                provenance: native_source(),
                value: NativePackageSnapshot::Installed(observation),
            }
        );
        assert_eq!(
            snapshot.pending_journal(),
            &ProbeEvidence::Present {
                provenance: journal_source(),
                value: PendingJournalSnapshot {
                    path: PathBuf::from("/var/lib/anolisa/transactions/pending.json"),
                },
            }
        );
    }

    #[test]
    fn snapshot_distinguishes_unavailable_absent_and_not_requested() {
        let request = ComponentSnapshotRequest::new(
            "tokenless",
            InstallationScope::System,
            [SnapshotProbe::State, SnapshotProbe::NativePackage],
        );
        let snapshot = ComponentSnapshot::from_parts_with_owned_files(
            request,
            ProbeEvidence::Unavailable {
                provenance: state_source(),
                reason: "permission denied".to_string(),
            },
            ProbeEvidence::NotRequested,
            ProbeEvidence::Absent {
                provenance: native_source(),
            },
            ProbeEvidence::NotRequested,
        )
        .unwrap();

        assert!(matches!(
            snapshot.state(),
            ProbeEvidence::Unavailable { reason, .. } if reason == "permission denied"
        ));
        assert!(matches!(
            snapshot.native_package(),
            ProbeEvidence::Absent { .. }
        ));
        assert_eq!(snapshot.pending_journal(), &ProbeEvidence::NotRequested);
    }

    #[test]
    fn snapshot_rejects_missing_requested_evidence() {
        let request = ComponentSnapshotRequest::new(
            "tokenless",
            InstallationScope::System,
            [SnapshotProbe::State],
        );

        let error = ComponentSnapshot::from_parts_with_owned_files(
            request,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
        )
        .unwrap_err();

        assert_eq!(
            error,
            SnapshotContractError::MissingRequestedEvidence {
                probe: SnapshotProbe::State,
            }
        );
    }

    #[test]
    fn snapshot_rejects_unrequested_evidence() {
        let request = ComponentSnapshotRequest::new(
            "tokenless",
            InstallationScope::System,
            [SnapshotProbe::State],
        );

        let error = ComponentSnapshot::from_parts_with_owned_files(
            request,
            ProbeEvidence::Absent {
                provenance: state_source(),
            },
            ProbeEvidence::NotRequested,
            ProbeEvidence::Absent {
                provenance: native_source(),
            },
            ProbeEvidence::NotRequested,
        )
        .unwrap_err();

        assert_eq!(
            error,
            SnapshotContractError::UnexpectedUnrequestedEvidence {
                probe: SnapshotProbe::NativePackage,
            }
        );
    }

    #[test]
    fn snapshot_rejects_active_state_for_a_different_target() {
        let mismatches = [
            (
                ObjectKind::Adapter,
                "tokenless".to_string(),
                InstallationScope::System,
            ),
            (
                ObjectKind::Component,
                "cosh".to_string(),
                InstallationScope::System,
            ),
            (
                ObjectKind::Component,
                "tokenless".to_string(),
                InstallationScope::User { uid: 1000 },
            ),
        ];

        for (kind, name, scope) in mismatches {
            let mut installation = active_installation();
            installation.kind = kind;
            installation.name.clone_from(&name);
            installation.scope = scope;

            let error = ComponentSnapshot::from_parts_with_owned_files(
                ComponentSnapshotRequest::new(
                    "tokenless",
                    InstallationScope::System,
                    [SnapshotProbe::State],
                ),
                ProbeEvidence::Present {
                    provenance: state_source(),
                    value: StateSnapshot::Active(Box::new(installation)),
                },
                ProbeEvidence::NotRequested,
                ProbeEvidence::NotRequested,
                ProbeEvidence::NotRequested,
            )
            .unwrap_err();

            assert_eq!(
                error,
                SnapshotContractError::ActiveStateTargetMismatch {
                    expected_component: "tokenless".to_string(),
                    expected_scope: InstallationScope::System,
                    actual_kind: kind,
                    actual_name: name,
                    actual_scope: scope,
                }
            );
        }
    }

    #[test]
    fn snapshot_rejects_native_package_probe_in_user_scope() {
        let error = ComponentSnapshot::from_parts_with_owned_files(
            ComponentSnapshotRequest::new(
                "tokenless",
                InstallationScope::User { uid: 1000 },
                [SnapshotProbe::NativePackage],
            ),
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
            ProbeEvidence::Absent {
                provenance: native_source(),
            },
            ProbeEvidence::NotRequested,
        )
        .unwrap_err();

        assert_eq!(
            error,
            SnapshotContractError::UnsupportedProbeScope {
                probe: SnapshotProbe::NativePackage,
                scope: InstallationScope::User { uid: 1000 },
            }
        );
    }

    #[test]
    fn state_and_native_variants_cover_ambiguous_observations() {
        let quarantined = StateSnapshot::Quarantined(QuarantineReason::NoEvidence);
        let multiple_versions = NativePackageSnapshot::MultipleVersions;

        assert!(matches!(
            quarantined,
            StateSnapshot::Quarantined(QuarantineReason::NoEvidence)
        ));
        assert_eq!(multiple_versions, NativePackageSnapshot::MultipleVersions);
    }

    #[test]
    fn read_only_observations_preserve_visibility_health_and_adapters() {
        let visibility = StateVisibilitySnapshot {
            root_scope: StateRootScope::System,
            active: false,
            mutable_by_current_invocation: false,
            shadowed_by: Some(StateRootScope::User),
        };
        let health = ManifestHealthSnapshot {
            outcome: CheckOutcome {
                spec_label: "systemd_active service=tokenless.service".to_string(),
                status: CheckStatus::Ok,
                detail: None,
                children: Vec::new(),
            },
        };
        let adapter = AdapterObservation {
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            declared: true,
            resource_root: Some(PathBuf::from(
                "/usr/share/anolisa/adapters/tokenless/openclaw",
            )),
            driver_available: true,
            framework_detected: true,
            adapter_type: Some("plugin".to_string()),
            enabled: true,
            claim_status: Some(ClaimStatus::Enabled),
            source_status: Some(AdapterSourceSnapshot::Available),
            source_reason: None,
        };
        let manifest_source = ManifestHealthProvenance {
            path: PathBuf::from("/var/lib/anolisa/component-manifests/tokenless/component.toml"),
        };
        let adapter_source = AdapterProvenance {
            state_paths: vec![PathBuf::from("/var/lib/anolisa/installed.toml")],
        };
        let snapshot = ComponentSnapshot::from_observations(
            ComponentSnapshotRequest::new(
                "tokenless",
                InstallationScope::System,
                [
                    SnapshotProbe::State,
                    SnapshotProbe::ManifestHealth,
                    SnapshotProbe::Adapters,
                ],
            ),
            ComponentSnapshotObservations {
                state: ProbeEvidence::Present {
                    provenance: state_source(),
                    value: StateSnapshot::Active(Box::new(active_installation())),
                },
                state_visibility: Some(visibility.clone()),
                owned_files: ProbeEvidence::NotRequested,
                native_package: ProbeEvidence::NotRequested,
                manifest_health: ProbeEvidence::Present {
                    provenance: manifest_source.clone(),
                    value: health.clone(),
                },
                adapters: ProbeEvidence::Present {
                    provenance: adapter_source.clone(),
                    value: vec![adapter.clone()],
                },
                pending_journal: ProbeEvidence::NotRequested,
            },
        )
        .expect("read-only observations");

        assert_eq!(snapshot.state_visibility(), Some(&visibility));
        assert_eq!(
            snapshot.manifest_health(),
            &ProbeEvidence::Present {
                provenance: manifest_source,
                value: health,
            }
        );
        assert_eq!(
            snapshot.adapters(),
            &ProbeEvidence::Present {
                provenance: adapter_source,
                value: vec![adapter],
            }
        );
    }

    #[test]
    fn snapshot_rejects_visibility_without_active_state() {
        let error = ComponentSnapshot::from_observations(
            ComponentSnapshotRequest::new(
                "tokenless",
                InstallationScope::System,
                [SnapshotProbe::State],
            ),
            ComponentSnapshotObservations {
                state: ProbeEvidence::Absent {
                    provenance: state_source(),
                },
                state_visibility: Some(StateVisibilitySnapshot {
                    root_scope: StateRootScope::System,
                    active: true,
                    mutable_by_current_invocation: true,
                    shadowed_by: None,
                }),
                owned_files: ProbeEvidence::NotRequested,
                native_package: ProbeEvidence::NotRequested,
                manifest_health: ProbeEvidence::NotRequested,
                adapters: ProbeEvidence::NotRequested,
                pending_journal: ProbeEvidence::NotRequested,
            },
        )
        .expect_err("visibility needs active state");

        assert_eq!(
            error,
            SnapshotContractError::StateVisibilityWithoutActiveState
        );
    }
}
