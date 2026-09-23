//! Shared read-only component evidence for status and diagnostics.
//!
//! This module owns snapshot assembly and source-level classification only;
//! command-specific records, findings, fix suggestions, and rendering stay
//! with their consumers.

use anolisa_core::domain::{Installation, InstallationScope, NativePm, Observation};
use anolisa_core::{
    AdapterObservation, AdapterProvenance, ComponentSnapshot, ComponentSnapshotObservations,
    ComponentSnapshotRequest, ManifestHealthProvenance, ManifestHealthSnapshot,
    NativePackageProvenance, NativePackageSnapshot, OwnedFilesProvenance, OwnedFilesSnapshot,
    PendingJournalSnapshot, ProbeEvidence, SnapshotContractError, SnapshotProbe, StateProvenance,
    StateRootScope, StateSnapshot, StateVisibilitySnapshot,
};
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError, PackageVersion};
use thiserror::Error;

use crate::commands::common;
use crate::commands::state_view::{ScopedInstalledObject, StateScope, StateView};

/// Selects which visible state records a read-only consumer observes.
#[derive(Clone, Copy)]
pub(crate) enum AggregateSelection {
    /// Select only the effective record for each component name.
    ActiveOnly,
    /// Select every readable scope record, including shadowed records.
    AllVisible,
}

/// Failure while assembling a component snapshot from read-only evidence.
#[derive(Debug, Error)]
pub(crate) enum ComponentObservationError {
    /// Collected evidence violated the shared snapshot contract.
    #[error("component snapshot contract failed for {component:?}: {source}")]
    Contract {
        /// Component whose request and evidence disagreed.
        component: String,
        /// Underlying request/evidence contract violation.
        #[source]
        source: SnapshotContractError,
    },
}

/// Live native-package drift relative to the recorded component state.
pub(crate) enum RpmDrift {
    /// The native authority disagrees with the recorded package version.
    Drifted { reason: String },
    /// The recorded native package is absent.
    Missing,
}

/// Native-package evidence plus the backend-native version from the same query.
pub(crate) struct NativePackageObservation {
    pub(crate) evidence: ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
    pub(crate) installed_version: Option<PackageVersion>,
    pub(crate) query_error: Option<PackageQueryError>,
}

/// Apply the read-only state normalization shared by status and doctor.
pub(crate) fn normalize_view_states(view: &mut StateView) {
    for root in &mut view.visible_roots {
        common::migrate_v3_symlinks(&mut root.state, &root.layout);
        common::hydrate_owned_file_contracts(&mut root.state, &root.layout);
    }
    if let Some(root) = view.visible_roots.iter().find(|root| root.writable) {
        view.writable = root.clone();
    }
}

/// Select visible component records without projecting command output.
pub(crate) fn select_visible_components<'a>(
    view: &'a StateView,
    name: Option<&str>,
    aggregate_selection: AggregateSelection,
) -> Vec<ScopedInstalledObject<'a>> {
    let visible_components = view.visible_components();
    match name {
        None => visible_components
            .into_iter()
            .filter(|record| {
                record.active || matches!(aggregate_selection, AggregateSelection::AllVisible)
            })
            .collect(),
        Some(target) => visible_components
            .into_iter()
            .filter(|record| record.object.name == target)
            .collect(),
    }
}

/// Assemble one installed component snapshot from consumer-selected probes.
pub(crate) fn snapshot_from_record(
    record: &ScopedInstalledObject<'_>,
    owned_files: ProbeEvidence<OwnedFilesSnapshot, OwnedFilesProvenance>,
    native_package: ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
    manifest_health: ProbeEvidence<ManifestHealthSnapshot, ManifestHealthProvenance>,
    adapters: ProbeEvidence<Vec<AdapterObservation>, AdapterProvenance>,
    pending_journal: ProbeEvidence<PendingJournalSnapshot, anolisa_core::JournalProvenance>,
) -> Result<ComponentSnapshot, ComponentObservationError> {
    let state = ProbeEvidence::Present {
        provenance: StateProvenance {
            path: record.root.state_path.clone(),
        },
        value: StateSnapshot::Active(Box::new(record.object.clone())),
    };
    let state_visibility = StateVisibilitySnapshot {
        root_scope: snapshot_scope(record.scope()),
        active: record.active,
        mutable_by_current_invocation: record.mutable_by_current_invocation,
        shadowed_by: record.shadowed_by.map(snapshot_scope),
    };
    let mut probes = vec![SnapshotProbe::State];
    push_requested_probe(&mut probes, SnapshotProbe::OwnedFiles, &owned_files);
    push_requested_probe(&mut probes, SnapshotProbe::NativePackage, &native_package);
    push_requested_probe(&mut probes, SnapshotProbe::ManifestHealth, &manifest_health);
    push_requested_probe(&mut probes, SnapshotProbe::Adapters, &adapters);
    push_requested_probe(&mut probes, SnapshotProbe::PendingJournal, &pending_journal);
    let request =
        ComponentSnapshotRequest::new(record.object.name.clone(), record.object.scope, probes);

    ComponentSnapshot::from_observations(
        request,
        ComponentSnapshotObservations {
            state,
            state_visibility: Some(state_visibility),
            owned_files,
            native_package,
            manifest_health,
            adapters,
            pending_journal,
        },
    )
    .map_err(|source| ComponentObservationError::Contract {
        component: record.object.name.clone(),
        source,
    })
}

/// Observe the owned-file contract selected by one state record.
pub(crate) fn owned_files_evidence(
    record: &ScopedInstalledObject<'_>,
) -> ProbeEvidence<OwnedFilesSnapshot, OwnedFilesProvenance> {
    match anolisa_core::facts::observe_owned_files(record.object, &record.root.layout) {
        Some(value) => ProbeEvidence::Present {
            provenance: OwnedFilesProvenance {
                state_path: record.root.state_path.clone(),
            },
            value,
        },
        None => ProbeEvidence::Absent {
            provenance: OwnedFilesProvenance {
                state_path: record.root.state_path.clone(),
            },
        },
    }
}

/// Assemble state-absent evidence for a named component query.
pub(crate) fn absent_snapshot(
    component: &str,
    scope: InstallationScope,
    state_path: std::path::PathBuf,
) -> Result<ComponentSnapshot, ComponentObservationError> {
    let request = ComponentSnapshotRequest::new(component, scope, [SnapshotProbe::State]);
    ComponentSnapshot::from_observations(
        request,
        ComponentSnapshotObservations {
            state: ProbeEvidence::Absent {
                provenance: StateProvenance { path: state_path },
            },
            state_visibility: None,
            owned_files: ProbeEvidence::NotRequested,
            native_package: ProbeEvidence::NotRequested,
            manifest_health: ProbeEvidence::NotRequested,
            adapters: ProbeEvidence::NotRequested,
            pending_journal: ProbeEvidence::NotRequested,
        },
    )
    .map_err(|source| ComponentObservationError::Contract {
        component: component.to_string(),
        source,
    })
}

fn push_requested_probe<T, P>(
    probes: &mut Vec<SnapshotProbe>,
    probe: SnapshotProbe,
    evidence: &ProbeEvidence<T, P>,
) {
    if !matches!(evidence, ProbeEvidence::NotRequested) {
        probes.push(probe);
    }
}

fn snapshot_scope(scope: StateScope) -> StateRootScope {
    match scope {
        StateScope::User => StateRootScope::User,
        StateScope::System => StateRootScope::System,
    }
}

/// Query one resolved native package while preserving source failure detail.
pub(crate) fn native_package_evidence(
    manager: NativePm,
    package: &str,
    query: &dyn PackageQuery,
    observed_at: &str,
) -> ProbeEvidence<NativePackageSnapshot, NativePackageProvenance> {
    observe_native_package(manager, package, query, observed_at).evidence
}

/// Query one native package once and retain its structured version for ordering.
pub(crate) fn observe_native_package(
    manager: NativePm,
    package: &str,
    query: &dyn PackageQuery,
    observed_at: &str,
) -> NativePackageObservation {
    let provenance = NativePackageProvenance {
        manager,
        package: package.to_string(),
    };
    match query.query_installed(package) {
        Ok(Some(info)) => NativePackageObservation {
            installed_version: Some(info.version.clone()),
            query_error: None,
            evidence: ProbeEvidence::Present {
                provenance,
                value: NativePackageSnapshot::Installed(Observation {
                    version: info.version.version.clone(),
                    evr: Some(info.version.to_string()),
                    arch: Some(info.arch),
                    source_repo: info.origin,
                    observed_at: observed_at.to_string(),
                }),
            },
        },
        Ok(None) => NativePackageObservation {
            evidence: ProbeEvidence::Absent { provenance },
            installed_version: None,
            query_error: None,
        },
        Err(PackageQueryError::UnexpectedOutput { detail, .. }) => NativePackageObservation {
            evidence: ProbeEvidence::Present {
                provenance,
                value: NativePackageSnapshot::UnexpectedOutput { detail },
            },
            installed_version: None,
            query_error: None,
        },
        Err(error) => {
            let reason = error.to_string();
            NativePackageObservation {
                evidence: ProbeEvidence::Unavailable { provenance, reason },
                installed_version: None,
                query_error: Some(error),
            }
        }
    }
}

/// Return the resolved package identity and recorded EVR for drift checks.
pub(crate) fn drift_probe_identity(installation: &Installation) -> Option<(&str, Option<&str>)> {
    match &installation.binding {
        anolisa_core::domain::ProviderBinding::Delegated {
            package,
            last_observed,
            ..
        } => package
            .resolved_name()
            .map(|name| (name, last_observed.as_ref().and_then(|o| o.evr.as_deref()))),
        anolisa_core::domain::ProviderBinding::Owned { .. } => None,
    }
}

/// Version displayed by read-only component consumers.
pub(crate) fn record_version(installation: &Installation) -> Option<String> {
    match &installation.binding {
        anolisa_core::domain::ProviderBinding::Owned { artifact } => Some(artifact.version.clone()),
        anolisa_core::domain::ProviderBinding::Delegated { last_observed, .. } => {
            last_observed.as_ref().map(|observation| {
                observation
                    .evr
                    .clone()
                    .unwrap_or_else(|| observation.version.clone())
            })
        }
    }
}

/// Classify native package evidence relative to the recorded EVR.
pub(crate) fn rpm_drift_from_evidence(
    evidence: &ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
    recorded_evr: Option<&str>,
) -> Option<RpmDrift> {
    match evidence {
        ProbeEvidence::Absent { .. } => Some(RpmDrift::Missing),
        ProbeEvidence::Present {
            provenance,
            value: NativePackageSnapshot::Installed(observation),
        } => match (recorded_evr, observation.evr.as_deref()) {
            (Some(recorded), Some(live)) if recorded != live => Some(RpmDrift::Drifted {
                reason: format!(
                    "rpmdb reports {live} for package {} but ANOLISA state records {recorded}",
                    provenance.package
                ),
            }),
            _ => None,
        },
        ProbeEvidence::Present {
            provenance,
            value: NativePackageSnapshot::MultipleVersions,
        } => Some(RpmDrift::Drifted {
            reason: format!(
                "rpmdb reports multiple installed versions for package {}",
                provenance.package
            ),
        }),
        ProbeEvidence::Present {
            provenance,
            value: NativePackageSnapshot::UnexpectedOutput { detail },
        } => Some(RpmDrift::Drifted {
            reason: format!(
                "rpmdb returned unexpected output for package {}: {detail}",
                provenance.package
            ),
        }),
        ProbeEvidence::NotRequested | ProbeEvidence::Unavailable { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anolisa_core::domain::{
        LifecycleStatus, ManagementRelation, OwnedArtifact, PackageIdentity, ProviderBinding,
    };
    use anolisa_core::state::{FileOwner, OwnedFile, OwnedFileKind};
    use anolisa_core::state_store::StateStore;
    use anolisa_core::{IntegrityStatus, ObjectKind, SubscriptionScope};
    use anolisa_platform::fs_layout::FsLayout;
    use anolisa_platform::pkg_query::{PackageInfo, PackageVersion};

    fn installation(scope: InstallationScope) -> Installation {
        Installation {
            kind: ObjectKind::Component,
            name: "tokenless".to_string(),
            scope,
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved {
                    name: "tokenless".to_string(),
                },
                relation: ManagementRelation::Observed,
                last_observed: None,
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-08-25T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: SubscriptionScope::None,
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    #[test]
    fn selection_preserves_active_and_shadowed_records() {
        let mut user = StateStore::empty();
        user.installations
            .push(installation(InstallationScope::User { uid: 1000 }));
        let mut system = StateStore::empty();
        system
            .installations
            .push(installation(InstallationScope::System));
        let user_root = crate::commands::state_view::ScopedStateRoot {
            scope: StateScope::User,
            layout: FsLayout::user_with_overrides(
                "/tmp/home".into(),
                None,
                None,
                Some("/tmp/user-state".into()),
                None,
                None,
            ),
            state_path: "/tmp/user-state/installed.toml".into(),
            writable: true,
            state: user,
        };
        let system_root = crate::commands::state_view::ScopedStateRoot {
            scope: StateScope::System,
            layout: FsLayout::system(Some("/tmp/system".into())),
            state_path: "/tmp/system-state/installed.toml".into(),
            writable: false,
            state: system,
        };
        let view = StateView {
            writable: user_root.clone(),
            visible_roots: vec![user_root, system_root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        };

        let active = select_visible_components(&view, None, AggregateSelection::ActiveOnly);
        assert_eq!(active.len(), 1);
        assert!(active[0].active);
        let named =
            select_visible_components(&view, Some("tokenless"), AggregateSelection::ActiveOnly);
        assert_eq!(named.len(), 2);
        assert!(named[0].active);
        assert_eq!(named[1].shadowed_by, Some(StateScope::User));
    }

    #[test]
    fn absent_snapshot_requests_only_state() {
        let snapshot = absent_snapshot(
            "ghost",
            InstallationScope::System,
            "/var/lib/anolisa/installed.toml".into(),
        )
        .expect("absent snapshot");

        assert!(matches!(snapshot.state(), ProbeEvidence::Absent { .. }));
        assert_eq!(snapshot.owned_files(), &ProbeEvidence::NotRequested);
        assert_eq!(snapshot.native_package(), &ProbeEvidence::NotRequested);
    }

    #[test]
    fn snapshot_preserves_scope_visibility_and_owned_file_evidence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let binary = layout.bin_dir.join("tokenless");
        std::fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary dir");
        std::fs::write(&binary, b"tokenless").expect("binary fixture");
        let mut store = StateStore::empty();
        store.installations.push(Installation {
            kind: ObjectKind::Component,
            name: "tokenless".to_string(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Owned {
                artifact: OwnedArtifact {
                    version: "1.0.0".to_string(),
                    distribution_source: None,
                    raw_package: None,
                    manifest_digest: None,
                    files: vec![OwnedFile {
                        path: binary.clone(),
                        owner: FileOwner::Anolisa,
                        sha256: None,
                        kind: OwnedFileKind::File,
                        referent: None,
                        mode: None,
                        capabilities: Vec::new(),
                    }],
                    services: Vec::new(),
                    external_modified_files: Vec::new(),
                    provisioned_packages: Vec::new(),
                },
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-08-25T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: SubscriptionScope::None,
            enabled_features: Vec::new(),
            health: Vec::new(),
        });
        let root = crate::commands::state_view::ScopedStateRoot {
            scope: StateScope::System,
            layout,
            state_path: tmp.path().join("state/installed.toml"),
            writable: true,
            state: store,
        };
        let view = StateView {
            writable: root.clone(),
            visible_roots: vec![root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let selected = select_visible_components(&view, None, AggregateSelection::ActiveOnly);
        let owned_files = owned_files_evidence(&selected[0]);
        let snapshot = snapshot_from_record(
            &selected[0],
            owned_files,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
        )
        .expect("component snapshot");

        let visibility = snapshot.state_visibility().expect("state visibility");
        assert_eq!(visibility.root_scope, StateRootScope::System);
        assert!(visibility.active);
        assert!(visibility.mutable_by_current_invocation);
        assert!(matches!(
            snapshot.owned_files(),
            ProbeEvidence::Present { value, .. }
                if value.files.len() == 1
                    && value.files[0].path == binary
                    && value.files[0].status == IntegrityStatus::Unverified
        ));

        let state_only = snapshot_from_record(
            &selected[0],
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
            ProbeEvidence::NotRequested,
        )
        .expect("state-only component snapshot");
        assert_eq!(state_only.owned_files(), &ProbeEvidence::NotRequested);
        assert!(!state_only.request().requests(SnapshotProbe::OwnedFiles));
    }

    #[derive(Clone, Copy)]
    enum InstalledReply {
        Present,
        Absent,
        Unavailable,
        Unexpected,
    }

    struct ScriptedQuery(InstalledReply);

    impl PackageQuery for ScriptedQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            match self.0 {
                InstalledReply::Present => Ok(Some(PackageInfo {
                    name: package.to_string(),
                    version: PackageVersion {
                        epoch: None,
                        version: "1.2.3".to_string(),
                        release: Some("1.al4".to_string()),
                    },
                    arch: "x86_64".to_string(),
                    origin: Some("@System".to_string()),
                })),
                InstalledReply::Absent => Ok(None),
                InstalledReply::Unavailable => Err(PackageQueryError::CommandMissing {
                    command: "rpm".to_string(),
                }),
                InstalledReply::Unexpected => Err(PackageQueryError::UnexpectedOutput {
                    command: "rpm".to_string(),
                    detail: "malformed row".to_string(),
                }),
            }
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn native_rpm_query_failure_is_unavailable_not_absent() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use anolisa_platform::command::{CommandOutput, CommandRunner};
        use anolisa_platform::rpm_query::RpmPackageQuery;

        struct Runner(Rc<RefCell<Option<CommandOutput>>>);

        impl CommandRunner for Runner {
            fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
                assert_eq!(program, "rpm");
                assert_eq!(
                    args,
                    [
                        "-q",
                        "--qf",
                        "%{NAME}|%{EPOCH}|%{VERSION}|%{RELEASE}|%{ARCH}\n",
                        "tokenless"
                    ]
                );
                Ok(self.0.borrow_mut().take().expect("exactly one RPM query"))
            }
        }

        for stderr in ["", "error: cannot open Packages database in /dev/null\n"] {
            let reply = Rc::new(RefCell::new(Some(CommandOutput {
                code: Some(1),
                stdout: "package tokenless is not installed\n".into(),
                stderr: stderr.into(),
            })));
            let query = RpmPackageQuery::with_runner(Runner(Rc::clone(&reply)));
            let observation =
                observe_native_package(NativePm::Rpm, "tokenless", &query, "2026-09-11T00:00:00Z");
            assert!(reply.borrow().is_none());
            assert!(observation.installed_version.is_none());
            if stderr.is_empty() {
                assert!(observation.query_error.is_none());
                assert!(
                    matches!(observation.evidence, ProbeEvidence::Absent { provenance }
                    if provenance.manager == NativePm::Rpm && provenance.package == "tokenless")
                );
            } else {
                let expected_error = PackageQueryError::QueryFailed {
                    command: "rpm".into(),
                    code: Some(1),
                    stderr: stderr.into(),
                };
                assert!(
                    matches!(observation.evidence, ProbeEvidence::Unavailable { provenance, reason }
                    if provenance.manager == NativePm::Rpm && provenance.package == "tokenless"
                        && reason == expected_error.to_string())
                );
                assert!(matches!(observation.query_error,
                    Some(PackageQueryError::QueryFailed { command, code: Some(1), stderr: actual })
                        if command == "rpm" && actual == stderr));
            }
        }
    }

    #[test]
    fn native_package_evidence_preserves_query_outcomes() {
        let present = observe_native_package(
            NativePm::Rpm,
            "tokenless",
            &ScriptedQuery(InstalledReply::Present),
            "2026-08-25T00:00:00Z",
        );
        assert_eq!(
            present.installed_version.as_ref().map(ToString::to_string),
            Some("1.2.3-1.al4".to_string())
        );
        assert!(matches!(
            present.evidence,
            ProbeEvidence::Present {
                value: NativePackageSnapshot::Installed(Observation { evr: Some(evr), .. }),
                ..
            } if evr == "1.2.3-1.al4"
        ));

        let absent = native_package_evidence(
            NativePm::Rpm,
            "tokenless",
            &ScriptedQuery(InstalledReply::Absent),
            "2026-08-25T00:00:00Z",
        );
        assert!(matches!(absent, ProbeEvidence::Absent { .. }));

        let unavailable = observe_native_package(
            NativePm::Rpm,
            "tokenless",
            &ScriptedQuery(InstalledReply::Unavailable),
            "2026-08-25T00:00:00Z",
        );
        assert!(matches!(
            unavailable.query_error,
            Some(PackageQueryError::CommandMissing { command }) if command == "rpm"
        ));
        assert!(matches!(
            unavailable.evidence,
            ProbeEvidence::Unavailable { reason, .. } if reason == "command not found: rpm"
        ));

        let unexpected = native_package_evidence(
            NativePm::Rpm,
            "tokenless",
            &ScriptedQuery(InstalledReply::Unexpected),
            "2026-08-25T00:00:00Z",
        );
        assert!(matches!(
            unexpected,
            ProbeEvidence::Present {
                value: NativePackageSnapshot::UnexpectedOutput { detail },
                ..
            } if detail == "malformed row"
        ));
    }

    #[test]
    fn rpm_drift_classifies_multiple_and_unexpected_output() {
        let provenance = NativePackageProvenance {
            manager: NativePm::Rpm,
            package: "tokenless".to_string(),
        };
        let multiple = ProbeEvidence::Present {
            provenance: provenance.clone(),
            value: NativePackageSnapshot::MultipleVersions,
        };
        assert!(matches!(
            rpm_drift_from_evidence(&multiple, Some("1.2.3-1.al4")),
            Some(RpmDrift::Drifted { reason }) if reason.contains("multiple installed versions")
        ));

        let unexpected = ProbeEvidence::Present {
            provenance,
            value: NativePackageSnapshot::UnexpectedOutput {
                detail: "malformed row".to_string(),
            },
        };
        assert!(matches!(
            rpm_drift_from_evidence(&unexpected, Some("1.2.3-1.al4")),
            Some(RpmDrift::Drifted { reason }) if reason.contains("malformed row")
        ));
    }
}
