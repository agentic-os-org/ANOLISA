//! Cross-consumer conformance tests for request-scoped component snapshots.

use anolisa_core::domain::{
    Installation, InstallationScope, LifecycleStatus, ManagementRelation, NativePm, Observation,
    OwnedArtifact, PackageIdentity, ProviderBinding,
};
use anolisa_core::state_store::StateStore;
use anolisa_core::{
    FileOwner, IntegrityStatus, NativePackageProvenance, NativePackageSnapshot, ObjectKind,
    OwnedFile, OwnedFileKind, OwnedFilesProvenance, OwnedFilesVerdict, ProbeEvidence,
    StateRootScope, StateSnapshot, SubscriptionScope,
};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::{PackageInfo, PackageQuery, PackageQueryError, PackageVersion};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::{component_observation, doctor, status, update};
use crate::commands::state_view::{ScopedInstalledObject, ScopedStateRoot, StateScope, StateView};

const OBSERVED_AT: &str = "2026-09-02T00:00:00Z";
const COMPONENT: &str = "tokenless";
const PACKAGE: &str = "tokenless-runtime";
const RECORDED_EVR: &str = "1.0.0-1.al4";

#[derive(Clone, Copy)]
enum InstalledReply {
    Matching,
    Drifted,
    Absent,
    Unavailable,
    Unexpected,
}

struct ScriptedQuery(InstalledReply);

impl PackageQuery for ScriptedQuery {
    fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
        assert_eq!(package, PACKAGE);
        match self.0 {
            InstalledReply::Matching => Ok(Some(package_info(package, "1.0.0"))),
            InstalledReply::Drifted => Ok(Some(package_info(package, "1.1.0"))),
            InstalledReply::Absent => Ok(None),
            InstalledReply::Unavailable => Err(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            }),
            InstalledReply::Unexpected => Err(PackageQueryError::UnexpectedOutput {
                command: "rpm".to_string(),
                detail: "multiple installed rows".to_string(),
            }),
        }
    }

    fn query_available(&self, package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
        assert_eq!(package, PACKAGE);
        Ok(Vec::new())
    }
}

fn package_info(package: &str, version: &str) -> PackageInfo {
    PackageInfo {
        name: package.to_string(),
        version: PackageVersion {
            epoch: None,
            version: version.to_string(),
            release: Some("1.al4".to_string()),
        },
        arch: "x86_64".to_string(),
        origin: Some("@System".to_string()),
    }
}

fn delegated_installation(
    name: &str,
    scope: InstallationScope,
    recorded_evr: &str,
) -> Installation {
    Installation {
        kind: ObjectKind::Component,
        name: name.to_string(),
        scope,
        binding: ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package: PackageIdentity::Resolved {
                name: PACKAGE.to_string(),
            },
            relation: ManagementRelation::Managed {
                since: "2026-08-01T00:00:00Z".to_string(),
            },
            last_observed: Some(Observation {
                version: recorded_evr
                    .split_once('-')
                    .map_or(recorded_evr, |(version, _)| version)
                    .to_string(),
                evr: Some(recorded_evr.to_string()),
                arch: Some("x86_64".to_string()),
                source_repo: Some("@System".to_string()),
                observed_at: "2026-08-01T00:00:00Z".to_string(),
            }),
        },
        status: LifecycleStatus::Installed,
        installed_at: "2026-08-01T00:00:00Z".to_string(),
        last_operation_id: Some("op-1".to_string()),
        subscription_scope: SubscriptionScope::None,
        enabled_features: Vec::new(),
        health: Vec::new(),
    }
}

fn owned_installation(name: &str, scope: InstallationScope, files: Vec<OwnedFile>) -> Installation {
    Installation {
        kind: ObjectKind::Component,
        name: name.to_string(),
        scope,
        binding: ProviderBinding::Owned {
            artifact: OwnedArtifact {
                version: "1.0.0".to_string(),
                distribution_source: None,
                raw_package: None,
                manifest_digest: None,
                files,
                services: Vec::new(),
                external_modified_files: Vec::new(),
                provisioned_packages: Vec::new(),
            },
        },
        status: LifecycleStatus::Installed,
        installed_at: "2026-08-01T00:00:00Z".to_string(),
        last_operation_id: Some("op-1".to_string()),
        subscription_scope: SubscriptionScope::None,
        enabled_features: Vec::new(),
        health: Vec::new(),
    }
}

fn state_root(
    scope: StateScope,
    layout: FsLayout,
    writable: bool,
    installations: Vec<Installation>,
) -> ScopedStateRoot {
    let state_path = layout.state_dir.join("installed.toml");
    let mut state = StateStore::empty();
    state.installations = installations;
    ScopedStateRoot {
        scope,
        layout,
        state_path,
        writable,
        state,
    }
}

fn state_view(roots: Vec<ScopedStateRoot>) -> StateView {
    let writable = roots
        .iter()
        .find(|root| root.writable)
        .expect("fixture requires a writable state root")
        .clone();
    StateView {
        writable,
        visible_roots: roots,
        unavailable_roots: Vec::new(),
        warnings: Vec::new(),
    }
}

fn record_for_scope(view: &StateView, scope: StateScope) -> ScopedInstalledObject<'_> {
    view.visible_components()
        .into_iter()
        .find(|record| record.scope() == scope)
        .expect("fixture record for scope")
}

fn assert_shared_state_and_native(
    status_snapshot: &anolisa_core::ComponentSnapshot,
    doctor_snapshot: &anolisa_core::ComponentSnapshot,
    update_snapshot: &anolisa_core::ComponentSnapshot,
) {
    assert_eq!(status_snapshot.state(), doctor_snapshot.state());
    assert_eq!(status_snapshot.state(), update_snapshot.state());
    assert_eq!(
        status_snapshot.state_visibility(),
        doctor_snapshot.state_visibility()
    );
    assert_eq!(
        status_snapshot.state_visibility(),
        update_snapshot.state_visibility()
    );
    assert_eq!(
        status_snapshot.native_package(),
        doctor_snapshot.native_package()
    );
    assert_eq!(
        status_snapshot.native_package(),
        update_snapshot.native_package()
    );
}

#[test]
fn matching_system_record_projects_consistent_base_facts() {
    let temp = tempdir().expect("tempdir");
    let layout = FsLayout::system(Some(temp.path().join("system")));
    let view = state_view(vec![state_root(
        StateScope::System,
        layout.clone(),
        true,
        vec![delegated_installation(
            COMPONENT,
            InstallationScope::System,
            RECORDED_EVR,
        )],
    )]);
    let record = record_for_scope(&view, StateScope::System);
    let query = ScriptedQuery(InstalledReply::Matching);

    let status_snapshot = status::snapshot_for_conformance(&record, Some(&query), OBSERVED_AT)
        .expect("status snapshot");
    let doctor_snapshot =
        doctor::snapshot_for_conformance(&record, &query, OBSERVED_AT).expect("doctor snapshot");
    let (update_snapshot, update_record) =
        update::check::checked_component_for_conformance(&record, &query, OBSERVED_AT)
            .expect("update-check result");
    assert_shared_state_and_native(&status_snapshot, &doctor_snapshot, &update_snapshot);
    assert_eq!(status_snapshot.owned_files(), doctor_snapshot.owned_files());

    match update_snapshot.native_package() {
        ProbeEvidence::Present { provenance, .. } => {
            assert_eq!(provenance.manager, NativePm::Rpm);
            assert_eq!(provenance.package, PACKAGE);
        }
        evidence => panic!("expected present native package evidence, got {evidence:?}"),
    }

    let status_record = status::projection_for_conformance(&status_snapshot, OBSERVED_AT)
        .expect("status projection");
    let doctor_record =
        doctor::projection_for_conformance(&doctor_snapshot, &layout, &query, OBSERVED_AT)
            .expect("doctor projection");
    assert_eq!(status_record.name, doctor_record.name);
    assert_eq!(status_record.name, update_record.component);
    assert_eq!(update_record.package.as_deref(), Some(PACKAGE));
    assert_eq!(status_record.status, doctor_record.state_status.unwrap());
    assert_eq!(status_record.scope, doctor_record.scope);
    assert_eq!(status_record.active, doctor_record.active);
    assert_eq!(
        status_record.mutable_by_current_invocation,
        doctor_record.mutable_by_current_invocation
    );
    assert_eq!(status_record.shadowed_by, doctor_record.shadowed_by);
    assert_eq!(status_record.state_path, doctor_record.state_path);
    assert_eq!(status_record.version, doctor_record.version);
    assert_eq!(status_record.version, update_record.installed);
    assert_eq!(update_record.action, update::check::ACTION_NOOP);
    assert_eq!(update_record.error, None);
}

#[test]
fn native_probe_categories_and_provenance_match_across_consumers() {
    let temp = tempdir().expect("tempdir");
    let layout = FsLayout::system(Some(temp.path().join("system")));
    let view = state_view(vec![state_root(
        StateScope::System,
        layout.clone(),
        true,
        vec![delegated_installation(
            COMPONENT,
            InstallationScope::System,
            RECORDED_EVR,
        )],
    )]);
    let record = record_for_scope(&view, StateScope::System);

    for reply in [
        InstalledReply::Drifted,
        InstalledReply::Absent,
        InstalledReply::Unavailable,
        InstalledReply::Unexpected,
    ] {
        let query = ScriptedQuery(reply);
        let status_snapshot = status::snapshot_for_conformance(&record, Some(&query), OBSERVED_AT)
            .expect("status snapshot");
        let doctor_snapshot = doctor::snapshot_for_conformance(&record, &query, OBSERVED_AT)
            .expect("doctor snapshot");
        let (update_snapshot, update_record) =
            update::check::checked_component_for_conformance(&record, &query, OBSERVED_AT)
                .expect("update-check result");
        assert_shared_state_and_native(&status_snapshot, &doctor_snapshot, &update_snapshot);

        let status_record = status::projection_for_conformance(&status_snapshot, OBSERVED_AT)
            .expect("status projection");
        let doctor_record =
            doctor::projection_for_conformance(&doctor_snapshot, &layout, &query, OBSERVED_AT)
                .expect("doctor projection");
        match reply {
            InstalledReply::Drifted => {
                assert_eq!(status_record.status, "drifted");
                assert!(
                    doctor_record
                        .finding_codes
                        .contains(&"rpm_drifted".to_string())
                );
                assert_eq!(update_record.installed.as_deref(), Some("1.1.0-1.al4"));
                assert_eq!(update_record.action, update::check::ACTION_NOOP);
            }
            InstalledReply::Absent => {
                assert_eq!(status_record.status, "missing");
                assert!(
                    doctor_record
                        .finding_codes
                        .contains(&"rpm_missing".to_string())
                );
                assert_eq!(update_record.action, update::check::ACTION_ERROR);
                assert!(
                    update_record
                        .error
                        .as_deref()
                        .is_some_and(|reason| reason.contains("not present in rpmdb"))
                );
            }
            InstalledReply::Unavailable => {
                assert_eq!(status_record.status, "installed");
                assert!(
                    !doctor_record
                        .finding_codes
                        .iter()
                        .any(|code| code.starts_with("rpm_"))
                );
                assert_eq!(update_record.action, update::check::ACTION_ERROR);
                assert_eq!(
                    update_record.error.as_deref(),
                    Some("rpm/dnf not found; cannot query the installed version")
                );
            }
            InstalledReply::Unexpected => {
                assert_eq!(status_record.status, "drifted");
                assert!(
                    doctor_record
                        .finding_codes
                        .contains(&"rpm_drifted".to_string())
                );
                assert_eq!(update_record.action, update::check::ACTION_ERROR);
                assert_eq!(
                    update_record.error.as_deref(),
                    Some("rpmdb reports multiple installed versions for this package")
                );
            }
            InstalledReply::Matching => unreachable!("matching case has its own conformance test"),
        }
    }
}

#[test]
fn status_and_doctor_preserve_scope_visibility_and_state_provenance() {
    let temp = tempdir().expect("tempdir");
    let user_layout = FsLayout::user(temp.path().join("home"));
    let system_layout = FsLayout::system(Some(temp.path().join("system")));
    let view = state_view(vec![
        state_root(
            StateScope::User,
            user_layout.clone(),
            true,
            vec![delegated_installation(
                COMPONENT,
                InstallationScope::User { uid: 1000 },
                "1.0.1-1.al4",
            )],
        ),
        state_root(
            StateScope::System,
            system_layout.clone(),
            false,
            vec![delegated_installation(
                COMPONENT,
                InstallationScope::System,
                RECORDED_EVR,
            )],
        ),
    ]);
    let query = ScriptedQuery(InstalledReply::Matching);

    for (scope, layout, installation_scope, root_scope, active, mutable, shadowed_by, version) in [
        (
            StateScope::User,
            &user_layout,
            InstallationScope::User { uid: 1000 },
            StateRootScope::User,
            true,
            true,
            None,
            "1.0.1-1.al4",
        ),
        (
            StateScope::System,
            &system_layout,
            InstallationScope::System,
            StateRootScope::System,
            false,
            false,
            Some(StateRootScope::User),
            RECORDED_EVR,
        ),
    ] {
        let record = record_for_scope(&view, scope);
        let expected_state_path = layout.state_dir.join("installed.toml");
        let status_snapshot = status::snapshot_for_conformance(&record, Some(&query), OBSERVED_AT)
            .expect("status snapshot");
        let doctor_snapshot = doctor::snapshot_for_conformance(&record, &query, OBSERVED_AT)
            .expect("doctor snapshot");
        assert_eq!(status_snapshot.state(), doctor_snapshot.state());
        assert_eq!(
            status_snapshot.state_visibility(),
            doctor_snapshot.state_visibility()
        );
        assert_eq!(
            status_snapshot.native_package(),
            doctor_snapshot.native_package()
        );
        assert_eq!(status_snapshot.request().scope(), installation_scope);
        let visibility = status_snapshot
            .state_visibility()
            .expect("visible fixture record");
        assert_eq!(visibility.root_scope, root_scope);
        assert_eq!(visibility.active, active);
        assert_eq!(visibility.mutable_by_current_invocation, mutable);
        assert_eq!(visibility.shadowed_by, shadowed_by);
        match status_snapshot.state() {
            ProbeEvidence::Present {
                provenance,
                value: StateSnapshot::Active(installation),
            } => {
                assert_eq!(provenance.path, expected_state_path);
                assert_eq!(installation.name, COMPONENT);
                assert_eq!(installation.scope, installation_scope);
            }
            evidence => panic!("expected active state evidence, got {evidence:?}"),
        }

        let status_record = status::projection_for_conformance(&status_snapshot, OBSERVED_AT)
            .expect("status projection");
        let doctor_record =
            doctor::projection_for_conformance(&doctor_snapshot, layout, &query, OBSERVED_AT)
                .expect("doctor projection");
        let expected_state_path = expected_state_path.display().to_string();
        assert_eq!(status_record.scope, scope.label());
        assert_eq!(status_record.active, active);
        assert_eq!(status_record.mutable_by_current_invocation, mutable);
        assert_eq!(
            status_record.shadowed_by.as_deref(),
            shadowed_by.map(|_| "user")
        );
        assert_eq!(
            status_record.state_path.as_deref(),
            Some(expected_state_path.as_str())
        );
        assert_eq!(status_record.version.as_deref(), Some(version));
        assert_eq!(doctor_record.scope, scope.label());
        assert_eq!(doctor_record.active, active);
        assert_eq!(doctor_record.mutable_by_current_invocation, mutable);
        assert_eq!(
            doctor_record.shadowed_by.as_deref(),
            shadowed_by.map(|_| "user")
        );
        assert_eq!(
            doctor_record.state_path.as_deref(),
            Some(expected_state_path.as_str())
        );
        assert_eq!(doctor_record.state_status.as_deref(), Some("installed"));
        assert_eq!(doctor_record.version.as_deref(), Some(version));
        assert_eq!(status_record.scope, doctor_record.scope);
        assert_eq!(status_record.active, doctor_record.active);
        assert_eq!(
            status_record.mutable_by_current_invocation,
            doctor_record.mutable_by_current_invocation
        );
        assert_eq!(status_record.shadowed_by, doctor_record.shadowed_by);
        assert_eq!(status_record.state_path, doctor_record.state_path);
        assert_eq!(status_record.version, doctor_record.version);
        assert_eq!(status_record.status, doctor_record.state_status.unwrap());
    }

    let user = record_for_scope(&view, StateScope::User);
    let user_snapshot = status::snapshot_for_conformance(&user, Some(&query), OBSERVED_AT)
        .expect("user status snapshot");
    assert!(matches!(
        user_snapshot.native_package(),
        ProbeEvidence::NotRequested
    ));
    let user_record = status::projection_for_conformance(&user_snapshot, OBSERVED_AT)
        .expect("user status projection");
    assert_ne!(user_record.status, "missing");
}

#[test]
fn owned_file_evidence_matches_without_forcing_update_check_to_request_it() {
    let actual_digest = format!("{:x}", Sha256::digest(b"fixture payload"));
    for digest_matches in [true, false] {
        let temp = tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(temp.path().join("system")));
        std::fs::create_dir_all(&layout.bin_dir).expect("create bin dir");
        let binary = layout.bin_dir.join("tokenless");
        std::fs::write(&binary, b"fixture payload").expect("write owned file");
        let expected_digest = if digest_matches {
            actual_digest.clone()
        } else {
            "0".repeat(64)
        };
        let installation = owned_installation(
            COMPONENT,
            InstallationScope::System,
            vec![OwnedFile {
                path: binary,
                owner: FileOwner::Anolisa,
                sha256: Some(expected_digest.clone()),
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            }],
        );
        let view = state_view(vec![state_root(
            StateScope::System,
            layout.clone(),
            true,
            vec![installation],
        )]);
        let record = record_for_scope(&view, StateScope::System);
        let query = ScriptedQuery(InstalledReply::Absent);

        let status_snapshot = status::snapshot_for_conformance(&record, Some(&query), OBSERVED_AT)
            .expect("status snapshot");
        let doctor_snapshot = doctor::snapshot_for_conformance(&record, &query, OBSERVED_AT)
            .expect("doctor snapshot");
        let (update_snapshot, _) =
            update::check::checked_component_for_conformance(&record, &query, OBSERVED_AT)
                .expect("update-check result");
        assert_eq!(status_snapshot.owned_files(), doctor_snapshot.owned_files());
        assert!(matches!(
            update_snapshot.owned_files(),
            ProbeEvidence::NotRequested
        ));
        assert!(matches!(
            update_snapshot.native_package(),
            ProbeEvidence::NotRequested
        ));

        let status_record = status::projection_for_conformance(&status_snapshot, OBSERVED_AT)
            .expect("status projection");
        let doctor_record =
            doctor::projection_for_conformance(&doctor_snapshot, &layout, &query, OBSERVED_AT)
                .expect("doctor projection");
        let ProbeEvidence::Present { value, .. } = status_snapshot.owned_files() else {
            panic!("expected owned-file evidence");
        };
        assert_eq!(value.files.len(), 1);
        if digest_matches {
            assert_eq!(value.verdict, OwnedFilesVerdict::Verified);
            assert_eq!(value.files[0].status, IntegrityStatus::Ok);
            assert_eq!(status_record.status, "installed");
            assert_eq!(doctor_record.status, "ok");
        } else {
            assert_eq!(value.verdict, OwnedFilesVerdict::Drifted);
            match &value.files[0].status {
                IntegrityStatus::ShaMismatch { expected, actual } => {
                    assert_eq!(expected, &expected_digest);
                    assert_eq!(actual, &actual_digest);
                }
                status => panic!("expected sha mismatch, got {status:?}"),
            }
            assert_eq!(status_record.status, "failed");
            assert_eq!(doctor_record.status, "failed");
        }
    }
}

#[test]
fn unavailable_owned_files_and_multiple_native_versions_remain_typed() {
    let temp = tempdir().expect("tempdir");
    let layout = FsLayout::system(Some(temp.path().join("system")));
    let view = state_view(vec![state_root(
        StateScope::System,
        layout.clone(),
        true,
        vec![delegated_installation(
            COMPONENT,
            InstallationScope::System,
            RECORDED_EVR,
        )],
    )]);
    let record = record_for_scope(&view, StateScope::System);
    let state_path = record.root.state_path.clone();
    let native_provenance = NativePackageProvenance {
        manager: NativePm::Rpm,
        package: PACKAGE.to_string(),
    };
    let native_package = ProbeEvidence::Present {
        provenance: native_provenance,
        value: NativePackageSnapshot::MultipleVersions,
    };
    let snapshot = component_observation::snapshot_from_record(
        &record,
        ProbeEvidence::Unavailable {
            provenance: OwnedFilesProvenance { state_path },
            reason: "integrity source unavailable".to_string(),
        },
        native_package.clone(),
        ProbeEvidence::NotRequested,
        ProbeEvidence::NotRequested,
        ProbeEvidence::NotRequested,
    )
    .expect("shared snapshot");
    let update_snapshot =
        update::check::snapshot_with_native_for_conformance(&record, native_package)
            .expect("update-check snapshot");

    assert!(matches!(
        snapshot.owned_files(),
        ProbeEvidence::Unavailable { reason, .. }
            if reason == "integrity source unavailable"
    ));
    assert_eq!(snapshot.native_package(), update_snapshot.native_package());

    let query = ScriptedQuery(InstalledReply::Matching);
    let status_record =
        status::projection_for_conformance(&snapshot, OBSERVED_AT).expect("status projection");
    let doctor_record = doctor::projection_for_conformance(&snapshot, &layout, &query, OBSERVED_AT)
        .expect("doctor projection");
    assert_eq!(status_record.status, "drifted");
    assert!(
        doctor_record
            .finding_codes
            .contains(&"rpm_drifted".to_string())
    );
    assert!(
        !doctor_record
            .finding_codes
            .iter()
            .any(|code| code == "rpm_missing")
    );
}
