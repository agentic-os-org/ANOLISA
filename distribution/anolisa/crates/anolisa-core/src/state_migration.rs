//! One-shot migration from legacy state objects (schema ≤ 4) to the
//! authority-centric domain model.
//!
//! Implements an ordered decision table — first match wins. The
//! function is pure and total: no I/O, and every legacy record maps to
//! exactly one of active / quarantined / dropped. Ambiguity that needs the
//! real world (does the package exist? what is its name?) is *not* resolved
//! here; it is deferred to planning-time re-observation via
//! [`PackageIdentity::Unresolved`].
//!
//! Safety direction: never grant more authority than the evidence supports.
//! The legacy `effective_ownership()` fallback defaulted unrecognized
//! records to `RawManaged` — maximum authority. These rules invert that
//! default: conflicting RPM evidence degrades to `Delegated + Observed`
//! (planning re-observation self-corrects a wrong guess), and records with
//! no evidence are quarantined, never assumed `Owned`.

use serde::{Deserialize, Serialize};

use crate::domain::{
    Installation, InstallationScope, LifecycleStatus, ManagementRelation, NativePm, Observation,
    OwnedArtifact, PackageIdentity, ProviderBinding,
};
use crate::state::{
    FileOwner, InstalledObject, ObjectKind, ObjectStatus, Ownership, is_legacy_rpm_backend,
};

/// Rule identifiers of the migration decision table, recorded so every
/// migrated record carries an auditable reason.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationRule {
    /// Legacy capability object → dropped.
    R0,
    /// Explicit RawManaged contradicted by RPM evidence → Delegated+Observed.
    R1a,
    /// Explicit RawManaged, no RPM evidence → Owned.
    R1b,
    /// Explicit RpmManaged contradicted by raw evidence → Delegated+Observed.
    R2a,
    /// Explicit RpmManaged with package name → Delegated+Managed.
    R2b,
    /// Explicit RpmManaged without package name → Delegated+Managed, unresolved.
    R2c,
    /// Explicit RpmObserved with adoption marker → Delegated+Adopted.
    R3a,
    /// Explicit RpmObserved without adoption marker → Delegated+Observed.
    R3b,
    /// Pre-v3 RPM backend, adopted → Delegated+Adopted.
    R4a,
    /// Pre-v3 RPM backend, unmanaged → Delegated+Observed.
    R4b,
    /// Pre-v3 RPM backend, managed → Delegated+Managed.
    R4c,
    /// Pre-v3 non-RPM backend but RPM metadata present → Delegated+Observed.
    R4d,
    /// Pre-v3 raw backend or raw-looking source → Owned.
    R4e,
    /// Pre-v3 unknown backend string → quarantined.
    R4f,
    /// Pre-v3 no backend, ANOLISA-owned file list as evidence → Owned.
    R4g,
    /// Pre-v3, no evidence at all → quarantined.
    R4h,
}

impl MigrationRule {
    /// Stable label for audit output.
    pub fn label(self) -> &'static str {
        match self {
            Self::R0 => "r0-dropped-capability",
            Self::R1a => "r1a-raw-claim-with-rpm-evidence",
            Self::R1b => "r1b-owned-explicit",
            Self::R2a => "r2a-rpm-claim-with-raw-evidence",
            Self::R2b => "r2b-managed-explicit",
            Self::R2c => "r2c-managed-unresolved-package",
            Self::R3a => "r3a-adopted-explicit",
            Self::R3b => "r3b-observed-explicit",
            Self::R4a => "r4a-legacy-rpm-adopted",
            Self::R4b => "r4b-legacy-rpm-unmanaged",
            Self::R4c => "r4c-legacy-rpm-managed",
            Self::R4d => "r4d-legacy-rpm-metadata-only",
            Self::R4e => "r4e-legacy-raw",
            Self::R4f => "r4f-unknown-backend",
            Self::R4g => "r4g-legacy-owned-files",
            Self::R4h => "r4h-no-evidence",
        }
    }
}

/// Why a record could not be safely classified. Quarantined records keep
/// their original content, are surfaced by `status` as needs-attention, and
/// never trigger file deletion or native removal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum QuarantineReason {
    /// `install_backend` names a backend this version cannot interpret.
    UnknownBackend { backend: String },
    /// No provenance evidence: no ownership, no backend, no source, no
    /// package metadata, no verifiable file list.
    NoEvidence,
}

/// A legacy record preserved verbatim because migration refused to guess.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuarantinedObject {
    /// Original legacy record, unmodified — zero information loss.
    pub record: InstalledObject,
    /// Why classification was refused.
    pub reason: QuarantineReason,
}

/// Result of migrating a single legacy object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// Cleanly mapped into the domain model.
    Active(Installation),
    /// Preserved but inert until `repair` or `forget` resolves it.
    Quarantined(QuarantinedObject),
    /// Legacy capability object dropped at the state migration boundary.
    DroppedLegacyCapability,
}

/// Outcome plus the decision-table rule that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationResult {
    /// Rule that fired (audit trail).
    pub rule: MigrationRule,
    /// What the record became.
    pub outcome: MigrationOutcome,
}

/// Aggregate result of migrating every object in a legacy state file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StateMigration {
    /// Cleanly migrated installations.
    pub active: Vec<Installation>,
    /// Records preserved verbatim pending `repair`/`forget`.
    pub quarantined: Vec<QuarantinedObject>,
    /// Names of dropped legacy capability objects.
    pub dropped_capabilities: Vec<String>,
    /// Per-object audit trail: (object name, rule that fired).
    pub audit: Vec<(String, MigrationRule)>,
}

/// Migrate every legacy object for one scope. Pure aggregation over
/// [`migrate_object`]; ordering is preserved.
pub fn migrate_state(objects: &[InstalledObject], scope: InstallationScope) -> StateMigration {
    let mut out = StateMigration::default();
    for object in objects {
        let result = migrate_object(object, scope);
        out.audit.push((object.name.clone(), result.rule));
        match result.outcome {
            MigrationOutcome::Active(installation) => out.active.push(installation),
            MigrationOutcome::Quarantined(q) => out.quarantined.push(q),
            MigrationOutcome::DroppedLegacyCapability => {
                out.dropped_capabilities.push(object.name.clone());
            }
        }
    }
    out
}

/// Migrate one legacy object into the domain model.
///
/// Ordered rules, first match wins — the order is load-bearing. The
/// exhaustive test below verifies,
/// for every field combination, that the fired rule's guard holds and no
/// earlier rule's guard does.
pub fn migrate_object(legacy: &InstalledObject, scope: InstallationScope) -> MigrationResult {
    // R0: preserve compatibility with old files without retaining obsolete objects.
    if matches!(legacy.kind, ObjectKind::Capability) {
        return MigrationResult {
            rule: MigrationRule::R0,
            outcome: MigrationOutcome::DroppedLegacyCapability,
        };
    }

    let backend = legacy.install_backend.as_deref();
    let rpm_backend = is_legacy_rpm_backend(backend);
    let raw_backend = matches!(backend, Some("raw"));
    let raw_source = is_raw_distribution_source(legacy.distribution_source.as_deref());
    let has_rpm_metadata = legacy.rpm_metadata.is_some();

    let (rule, outcome) = match legacy.ownership {
        Some(Ownership::RawManaged) => {
            if has_rpm_metadata || rpm_backend {
                // R1a: an Owned claim carrying RPM evidence is contradictory.
                // RPM evidence wins and authority degrades to Observed: a
                // wrong guess self-corrects at the next re-observation,
                // whereas a wrong Owned claim deletes files ANOLISA does not
                // own. The owned-file claim is intentionally dropped — those
                // files will never be deleted by ANOLISA.
                (
                    MigrationRule::R1a,
                    delegated(legacy, scope, ManagementRelation::Observed),
                )
            } else {
                // R1b: every v3+ write sets ownership explicitly — strong
                // evidence, map losslessly.
                (MigrationRule::R1b, owned(legacy, scope))
            }
        }
        Some(Ownership::RpmManaged) => {
            if raw_backend || raw_source {
                // R2a: a Managed claim carrying raw evidence is
                // contradictory; do not grant native-removal authority.
                (
                    MigrationRule::R2a,
                    delegated(legacy, scope, ManagementRelation::Observed),
                )
            } else {
                let relation = ManagementRelation::Managed {
                    since: legacy.installed_at.clone(),
                };
                let rule = if resolved_package_name(legacy).is_some() {
                    MigrationRule::R2b
                } else {
                    MigrationRule::R2c
                };
                (rule, delegated(legacy, scope, relation))
            }
        }
        Some(Ownership::RpmObserved) => {
            // The legacy variant conflated adopted and merely-observed;
            // split on the explicit markers (rules R3a/R3b).
            if legacy.adopted || legacy.status == ObjectStatus::Adopted {
                let relation = ManagementRelation::Adopted {
                    since: legacy.installed_at.clone(),
                };
                (MigrationRule::R3a, delegated(legacy, scope, relation))
            } else {
                (
                    MigrationRule::R3b,
                    delegated(legacy, scope, ManagementRelation::Observed),
                )
            }
        }
        None => {
            if rpm_backend {
                if legacy.adopted {
                    let relation = ManagementRelation::Adopted {
                        since: legacy.installed_at.clone(),
                    };
                    (MigrationRule::R4a, delegated(legacy, scope, relation))
                } else if !legacy.managed {
                    (
                        MigrationRule::R4b,
                        delegated(legacy, scope, ManagementRelation::Observed),
                    )
                } else {
                    let relation = ManagementRelation::Managed {
                        since: legacy.installed_at.clone(),
                    };
                    (MigrationRule::R4c, delegated(legacy, scope, relation))
                }
            } else if has_rpm_metadata {
                // R4d: RPM evidence beats any raw-looking source or backend.
                (
                    MigrationRule::R4d,
                    delegated(legacy, scope, ManagementRelation::Observed),
                )
            } else if raw_backend || (backend.is_none() && raw_source) {
                // R4e: a raw-looking source only counts as Owned evidence
                // when no foreign backend string claims otherwise.
                (MigrationRule::R4e, owned(legacy, scope))
            } else if let Some(other) = backend {
                (
                    MigrationRule::R4f,
                    MigrationOutcome::Quarantined(QuarantinedObject {
                        record: legacy.clone(),
                        reason: QuarantineReason::UnknownBackend {
                            backend: other.to_string(),
                        },
                    }),
                )
            } else if !legacy.files.is_empty()
                && legacy.files.iter().all(|f| f.owner == FileOwner::Anolisa)
            {
                // R4g: an ANOLISA-owned file list is only ever written by a
                // raw install, and uninstall re-verifies existence and
                // digests before deleting — acceptable Owned evidence.
                (MigrationRule::R4g, owned(legacy, scope))
            } else {
                (
                    MigrationRule::R4h,
                    MigrationOutcome::Quarantined(QuarantinedObject {
                        record: legacy.clone(),
                        reason: QuarantineReason::NoEvidence,
                    }),
                )
            }
        }
    };

    MigrationResult { rule, outcome }
}

/// Raw-source inference shared with the CLI's
/// `infer_backend_from_distribution_source`: only URL-shaped sources that
/// the raw backend actually fetches count as raw evidence.
fn is_raw_distribution_source(source: Option<&str>) -> bool {
    source.is_some_and(|s| {
        s.starts_with("http://") || s.starts_with("https://") || s.starts_with("file://")
    })
}

/// Non-empty recorded package name, if any.
fn resolved_package_name(legacy: &InstalledObject) -> Option<String> {
    legacy
        .rpm_metadata
        .as_ref()
        .map(|m| m.package_name.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// Legacy status → narrowed lifecycle status. `Adopted` collapses to
/// `Installed`: adoption is carried by the management relation now.
fn migrate_status(status: ObjectStatus) -> LifecycleStatus {
    match status {
        ObjectStatus::Installed | ObjectStatus::Adopted => LifecycleStatus::Installed,
        ObjectStatus::Partial => LifecycleStatus::Partial,
        ObjectStatus::Disabled => LifecycleStatus::Disabled,
        ObjectStatus::Failed => LifecycleStatus::Failed,
    }
}

/// Build an `Owned` installation from a legacy record.
fn owned(legacy: &InstalledObject, scope: InstallationScope) -> MigrationOutcome {
    MigrationOutcome::Active(owned_installation(legacy, scope))
}

/// Map a legacy record's fields onto an `Owned` installation, verbatim.
///
/// Public because repair's quarantine-restore exit performs the same
/// mapping: a quarantined legacy record whose owned files verify intact is
/// rebuilt into an active owned record through exactly this translation.
pub fn owned_installation(legacy: &InstalledObject, scope: InstallationScope) -> Installation {
    Installation {
        kind: legacy.kind,
        name: legacy.name.clone(),
        scope,
        binding: ProviderBinding::Owned {
            artifact: OwnedArtifact {
                version: legacy.version.clone(),
                distribution_source: legacy.distribution_source.clone(),
                raw_package: legacy.raw_package.clone(),
                manifest_digest: legacy.manifest_digest.clone(),
                files: legacy.files.clone(),
                services: legacy.services.clone(),
                external_modified_files: legacy.external_modified_files.clone(),
                provisioned_packages: legacy.provisioned_packages.clone(),
            },
        },
        status: migrate_status(legacy.status),
        installed_at: legacy.installed_at.clone(),
        last_operation_id: legacy.last_operation_id.clone(),
        subscription_scope: legacy.subscription_scope,
        enabled_features: legacy.enabled_features.clone(),
        health: legacy.health.clone(),
    }
}

/// Build a `Delegated` installation from a legacy record. The observation
/// snapshot is stamped with the legacy `installed_at` — stale by
/// construction, so the first planning pass always re-observes.
fn delegated(
    legacy: &InstalledObject,
    scope: InstallationScope,
    relation: ManagementRelation,
) -> MigrationOutcome {
    let package = match resolved_package_name(legacy) {
        Some(name) => PackageIdentity::Resolved { name },
        None => PackageIdentity::Unresolved {
            component_hint: legacy.name.clone(),
        },
    };
    let meta = legacy.rpm_metadata.as_ref();
    let last_observed = Some(Observation {
        version: legacy.version.clone(),
        evr: meta.and_then(|m| m.evr.clone()),
        arch: meta.and_then(|m| m.arch.clone()),
        source_repo: meta.and_then(|m| m.source_repo.clone()),
        observed_at: legacy.installed_at.clone(),
    });
    MigrationOutcome::Active(Installation {
        kind: legacy.kind,
        name: legacy.name.clone(),
        scope,
        binding: ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package,
            relation,
            last_observed,
        },
        status: migrate_status(legacy.status),
        installed_at: legacy.installed_at.clone(),
        last_operation_id: legacy.last_operation_id.clone(),
        subscription_scope: legacy.subscription_scope,
        enabled_features: legacy.enabled_features.clone(),
        health: legacy.health.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{OwnedFile, OwnedFileKind, RpmMetadata, SubscriptionScope};
    use std::path::PathBuf;

    const SCOPE: InstallationScope = InstallationScope::System;

    fn base_object() -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: "copilot-shell".to_string(),
            version: "1.2.3".to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: None,
            ownership: None,
            rpm_metadata: None,
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            last_operation_id: None,
            managed: true,
            adopted: false,
            subscription_scope: SubscriptionScope::None,
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: Vec::new(),
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        }
    }

    fn anolisa_file() -> OwnedFile {
        OwnedFile {
            path: PathBuf::from("/usr/bin/copilot-shell"),
            owner: FileOwner::Anolisa,
            sha256: Some("aa".repeat(32)),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }
    }

    fn external_file() -> OwnedFile {
        OwnedFile {
            path: PathBuf::from("/etc/third-party.conf"),
            owner: FileOwner::External,
            sha256: None,
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }
    }

    fn named_meta() -> RpmMetadata {
        RpmMetadata {
            package_name: "copilot-shell".to_string(),
            evr: Some("1:1.2.3-4".to_string()),
            arch: Some("x86_64".to_string()),
            source_repo: Some("@System".to_string()),
        }
    }

    fn unnamed_meta() -> RpmMetadata {
        RpmMetadata {
            package_name: "  ".to_string(),
            evr: None,
            arch: None,
            source_repo: None,
        }
    }

    fn expect_active(result: MigrationResult) -> Installation {
        match result.outcome {
            MigrationOutcome::Active(i) => i,
            other => panic!("expected active installation, got {other:?}"),
        }
    }

    fn expect_quarantined(result: MigrationResult) -> QuarantinedObject {
        match result.outcome {
            MigrationOutcome::Quarantined(q) => q,
            other => panic!("expected quarantined record, got {other:?}"),
        }
    }

    // ---- per-rule exact tests ------------------------------------------

    #[test]
    fn r0_drops_legacy_capability() {
        let mut o = base_object();
        o.kind = ObjectKind::Capability;
        let r = migrate_object(&o, SCOPE);
        assert_eq!(r.rule, MigrationRule::R0);
        assert_eq!(r.outcome, MigrationOutcome::DroppedLegacyCapability);
    }

    #[test]
    fn r1a_raw_claim_with_rpm_metadata_degrades_to_observed() {
        let mut o = base_object();
        o.ownership = Some(Ownership::RawManaged);
        o.rpm_metadata = Some(named_meta());
        o.files = vec![anolisa_file()];
        let r = migrate_object(&o, SCOPE);
        assert_eq!(r.rule, MigrationRule::R1a);
        let i = expect_active(r);
        match i.binding {
            ProviderBinding::Delegated {
                relation, package, ..
            } => {
                assert_eq!(relation, ManagementRelation::Observed);
                assert_eq!(package.resolved_name(), Some("copilot-shell"));
            }
            other => panic!("expected delegated binding, got {other:?}"),
        }
    }

    #[test]
    fn r1b_explicit_raw_maps_owned_losslessly() {
        let mut o = base_object();
        o.ownership = Some(Ownership::RawManaged);
        o.install_backend = Some("raw".to_string());
        o.distribution_source = Some("https://repo.example/pkg.tar.gz".to_string());
        o.raw_package = Some("copilot-shell-x86_64".to_string());
        o.manifest_digest = Some("digest".to_string());
        o.files = vec![anolisa_file()];
        let r = migrate_object(&o, SCOPE);
        assert_eq!(r.rule, MigrationRule::R1b);
        let i = expect_active(r);
        match i.binding {
            ProviderBinding::Owned { artifact } => {
                assert_eq!(artifact.version, "1.2.3");
                assert_eq!(
                    artifact.raw_package.as_deref(),
                    Some("copilot-shell-x86_64")
                );
                assert_eq!(artifact.files.len(), 1);
            }
            other => panic!("expected owned binding, got {other:?}"),
        }
    }

    #[test]
    fn r2a_managed_claim_with_raw_source_degrades_to_observed() {
        let mut o = base_object();
        o.ownership = Some(Ownership::RpmManaged);
        o.rpm_metadata = Some(named_meta());
        o.distribution_source = Some("https://repo.example/pkg.tar.gz".to_string());
        let r = migrate_object(&o, SCOPE);
        assert_eq!(r.rule, MigrationRule::R2a);
        let i = expect_active(r);
        assert!(
            !i.binding.owns_removal(),
            "contradictory record must not own removal"
        );
    }

    #[test]
    fn r2b_explicit_managed_keeps_authority_and_observation() {
        let mut o = base_object();
        o.ownership = Some(Ownership::RpmManaged);
        o.install_backend = Some("rpm".to_string());
        o.rpm_metadata = Some(named_meta());
        let r = migrate_object(&o, SCOPE);
        assert_eq!(r.rule, MigrationRule::R2b);
        let i = expect_active(r);
        assert!(i.binding.owns_removal());
        match i.binding {
            ProviderBinding::Delegated {
                last_observed: Some(obs),
                relation,
                ..
            } => {
                assert_eq!(obs.evr.as_deref(), Some("1:1.2.3-4"));
                assert_eq!(obs.observed_at, "2026-01-01T00:00:00Z");
                assert_eq!(
                    relation,
                    ManagementRelation::Managed {
                        since: "2026-01-01T00:00:00Z".to_string()
                    }
                );
            }
            other => panic!("expected delegated binding with observation, got {other:?}"),
        }
    }

    #[test]
    fn r2c_managed_without_package_name_is_unresolved() {
        let mut o = base_object();
        o.ownership = Some(Ownership::RpmManaged);
        o.rpm_metadata = Some(unnamed_meta());
        let r = migrate_object(&o, SCOPE);
        assert_eq!(r.rule, MigrationRule::R2c);
        let i = expect_active(r);
        match i.binding {
            ProviderBinding::Delegated { package, .. } => {
                assert_eq!(
                    package,
                    PackageIdentity::Unresolved {
                        component_hint: "copilot-shell".to_string()
                    }
                );
            }
            other => panic!("expected delegated binding, got {other:?}"),
        }
    }

    #[test]
    fn r3_splits_conflated_observed_variant_on_adoption_markers() {
        // adopted bool set → Adopted.
        let mut adopted = base_object();
        adopted.ownership = Some(Ownership::RpmObserved);
        adopted.rpm_metadata = Some(named_meta());
        adopted.adopted = true;
        let r = migrate_object(&adopted, SCOPE);
        assert_eq!(r.rule, MigrationRule::R3a);

        // status Adopted alone also counts as adoption evidence.
        let mut status_adopted = base_object();
        status_adopted.ownership = Some(Ownership::RpmObserved);
        status_adopted.status = ObjectStatus::Adopted;
        let r = migrate_object(&status_adopted, SCOPE);
        assert_eq!(r.rule, MigrationRule::R3a);
        let i = expect_active(r);
        assert_eq!(
            i.status,
            LifecycleStatus::Installed,
            "Adopted status must collapse"
        );

        // neither marker → merely observed.
        let mut observed = base_object();
        observed.ownership = Some(Ownership::RpmObserved);
        let r = migrate_object(&observed, SCOPE);
        assert_eq!(r.rule, MigrationRule::R3b);
    }

    #[test]
    fn r4_legacy_rpm_backend_splits_on_markers() {
        let mut adopted = base_object();
        adopted.install_backend = Some("yum".to_string());
        adopted.adopted = true;
        assert_eq!(migrate_object(&adopted, SCOPE).rule, MigrationRule::R4a);

        let mut unmanaged = base_object();
        unmanaged.install_backend = Some("rpm".to_string());
        unmanaged.managed = false;
        assert_eq!(migrate_object(&unmanaged, SCOPE).rule, MigrationRule::R4b);

        let mut managed = base_object();
        managed.install_backend = Some("rpm".to_string());
        let r = migrate_object(&managed, SCOPE);
        assert_eq!(r.rule, MigrationRule::R4c);
        assert!(expect_active(r).binding.owns_removal());
    }

    #[test]
    fn r4d_rpm_metadata_beats_raw_looking_source() {
        let mut o = base_object();
        o.rpm_metadata = Some(named_meta());
        o.distribution_source = Some("https://repo.example/pkg.tar.gz".to_string());
        let r = migrate_object(&o, SCOPE);
        assert_eq!(r.rule, MigrationRule::R4d);
        assert!(!expect_active(r).binding.owns_removal());
    }

    #[test]
    fn r4e_raw_source_counts_only_without_foreign_backend() {
        let mut plain = base_object();
        plain.distribution_source = Some("https://repo.example/pkg.tar.gz".to_string());
        assert_eq!(migrate_object(&plain, SCOPE).rule, MigrationRule::R4e);

        let mut raw_backend = base_object();
        raw_backend.install_backend = Some("raw".to_string());
        assert_eq!(migrate_object(&raw_backend, SCOPE).rule, MigrationRule::R4e);

        // A foreign backend string wins over the raw-looking source: unknown
        // semantics quarantine instead of guessing Owned.
        let mut foreign = base_object();
        foreign.install_backend = Some("brew".to_string());
        foreign.distribution_source = Some("https://repo.example/pkg.tar.gz".to_string());
        let r = migrate_object(&foreign, SCOPE);
        assert_eq!(r.rule, MigrationRule::R4f);
        let q = expect_quarantined(r);
        assert_eq!(
            q.reason,
            QuarantineReason::UnknownBackend {
                backend: "brew".to_string()
            }
        );
    }

    #[test]
    fn r4g_owned_file_list_is_owned_evidence_but_external_files_are_not() {
        let mut owned_files = base_object();
        owned_files.files = vec![anolisa_file()];
        assert_eq!(migrate_object(&owned_files, SCOPE).rule, MigrationRule::R4g);

        let mut mixed = base_object();
        mixed.files = vec![anolisa_file(), external_file()];
        let r = migrate_object(&mixed, SCOPE);
        assert_eq!(r.rule, MigrationRule::R4h);
        assert_eq!(expect_quarantined(r).reason, QuarantineReason::NoEvidence);
    }

    #[test]
    fn r4h_no_evidence_quarantines_and_preserves_record() {
        let o = base_object();
        let r = migrate_object(&o, SCOPE);
        assert_eq!(r.rule, MigrationRule::R4h);
        let q = expect_quarantined(r);
        assert_eq!(q.record, o, "quarantine must preserve the record verbatim");
    }

    #[test]
    fn migrate_state_partitions_and_audits() {
        let mut capability = base_object();
        capability.kind = ObjectKind::Capability;
        capability.name = "old-capability".to_string();
        let mut owned = base_object();
        owned.name = "raw-component".to_string();
        owned.ownership = Some(Ownership::RawManaged);
        let orphan = base_object();

        let migration = migrate_state(&[capability, owned, orphan], SCOPE);
        assert_eq!(migration.active.len(), 1);
        assert_eq!(migration.quarantined.len(), 1);
        assert_eq!(
            migration.dropped_capabilities,
            vec!["old-capability".to_string()]
        );
        assert_eq!(migration.audit.len(), 3);
        assert_eq!(
            migration.audit[1],
            ("raw-component".to_string(), MigrationRule::R1b)
        );
    }

    #[test]
    fn scope_is_carried_through() {
        let mut o = base_object();
        o.ownership = Some(Ownership::RawManaged);
        let user_scope = InstallationScope::User { uid: 1000 };
        let i = expect_active(migrate_object(&o, user_scope));
        assert_eq!(i.scope, user_scope);
    }

    #[test]
    fn adapter_and_osbase_kinds_pass_through() {
        for kind in [ObjectKind::Adapter, ObjectKind::Osbase] {
            let mut o = base_object();
            o.kind = kind;
            o.ownership = Some(Ownership::RawManaged);
            let i = expect_active(migrate_object(&o, SCOPE));
            assert_eq!(i.kind, kind);
        }
    }

    // ---- exhaustive decision-table pinning ------------------------------

    /// Independent restatement of the ordered rule guards. The exhaustive
    /// test cross-checks the
    /// implementation against this spec for every field combination.
    fn spec_rule(o: &InstalledObject) -> MigrationRule {
        use MigrationRule::*;
        let backend = o.install_backend.as_deref();
        let rpm = matches!(backend, Some("rpm" | "yum"));
        let raw_b = matches!(backend, Some("raw"));
        let raw_src = o.distribution_source.as_deref().is_some_and(|s| {
            s.starts_with("http://") || s.starts_with("https://") || s.starts_with("file://")
        });
        let meta = o.rpm_metadata.is_some();
        if matches!(o.kind, ObjectKind::Capability) {
            return R0;
        }
        match o.ownership {
            Some(Ownership::RawManaged) if meta || rpm => R1a,
            Some(Ownership::RawManaged) => R1b,
            Some(Ownership::RpmManaged) if raw_b || raw_src => R2a,
            Some(Ownership::RpmManaged)
                if o.rpm_metadata
                    .as_ref()
                    .is_some_and(|m| !m.package_name.trim().is_empty()) =>
            {
                R2b
            }
            Some(Ownership::RpmManaged) => R2c,
            Some(Ownership::RpmObserved) if o.adopted || o.status == ObjectStatus::Adopted => R3a,
            Some(Ownership::RpmObserved) => R3b,
            None if rpm && o.adopted => R4a,
            None if rpm && !o.managed => R4b,
            None if rpm => R4c,
            None if meta => R4d,
            None if raw_b || (backend.is_none() && raw_src) => R4e,
            None if backend.is_some() => R4f,
            None if !o.files.is_empty()
                && o.files.iter().all(|f| f.owner == FileOwner::Anolisa) =>
            {
                R4g
            }
            None => R4h,
        }
    }

    /// Shape every rule must produce.
    fn assert_shape(o: &InstalledObject, result: &MigrationResult) {
        use MigrationRule::*;
        match (result.rule, &result.outcome) {
            (R0, MigrationOutcome::DroppedLegacyCapability) => {}
            (R1b | R4e | R4g, MigrationOutcome::Active(i)) => {
                assert!(matches!(i.binding, ProviderBinding::Owned { .. }), "{o:?}");
            }
            (R1a | R2a | R3b | R4b | R4d, MigrationOutcome::Active(i)) => match &i.binding {
                ProviderBinding::Delegated {
                    relation: ManagementRelation::Observed,
                    ..
                } => {}
                other => panic!("rule {:?} must yield Observed, got {other:?}", result.rule),
            },
            (R2b | R2c | R4c, MigrationOutcome::Active(i)) => match &i.binding {
                ProviderBinding::Delegated {
                    relation: ManagementRelation::Managed { .. },
                    ..
                } => {}
                other => panic!("rule {:?} must yield Managed, got {other:?}", result.rule),
            },
            (R3a | R4a, MigrationOutcome::Active(i)) => match &i.binding {
                ProviderBinding::Delegated {
                    relation: ManagementRelation::Adopted { .. },
                    ..
                } => {}
                other => panic!("rule {:?} must yield Adopted, got {other:?}", result.rule),
            },
            (R4f, MigrationOutcome::Quarantined(q)) => {
                assert!(
                    matches!(q.reason, QuarantineReason::UnknownBackend { .. }),
                    "{o:?}"
                );
            }
            (R4h, MigrationOutcome::Quarantined(q)) => {
                assert_eq!(q.reason, QuarantineReason::NoEvidence, "{o:?}");
            }
            (rule, outcome) => panic!("rule {rule:?} with unexpected outcome {outcome:?}"),
        }
    }

    /// Safety invariants that hold for every input, independent of which
    /// rule fired.
    fn assert_safety(o: &InstalledObject, result: &MigrationResult) {
        if let MigrationOutcome::Active(i) = &result.outcome {
            match &i.binding {
                ProviderBinding::Owned { .. } => {
                    // RPM evidence must never yield Owned.
                    assert!(
                        o.rpm_metadata.is_none(),
                        "owned despite rpm metadata: {o:?}"
                    );
                    assert!(
                        !is_legacy_rpm_backend(o.install_backend.as_deref()),
                        "owned despite rpm backend: {o:?}"
                    );
                }
                ProviderBinding::Delegated {
                    relation,
                    package,
                    last_observed,
                    ..
                } => {
                    // Native-removal authority requires explicit managed
                    // evidence — never granted on contradiction fallbacks.
                    if matches!(relation, ManagementRelation::Managed { .. }) {
                        let explicit = o.ownership == Some(Ownership::RpmManaged);
                        let legacy_managed = o.ownership.is_none()
                            && is_legacy_rpm_backend(o.install_backend.as_deref())
                            && o.managed
                            && !o.adopted;
                        assert!(explicit || legacy_managed, "unearned Managed: {o:?}");
                    }
                    // Package identity resolves iff a non-empty name exists.
                    let has_name = o
                        .rpm_metadata
                        .as_ref()
                        .is_some_and(|m| !m.package_name.trim().is_empty());
                    assert_eq!(package.resolved_name().is_some(), has_name, "{o:?}");
                    // Migrated observations always exist and are stamped
                    // with the legacy install time.
                    let obs = last_observed.as_ref().expect("migrated observation");
                    assert_eq!(obs.version, o.version);
                    assert_eq!(obs.observed_at, o.installed_at);
                }
            }
            // Status narrows: Adopted collapses to Installed, others map 1:1.
            let expected_status = match o.status {
                ObjectStatus::Installed | ObjectStatus::Adopted => LifecycleStatus::Installed,
                ObjectStatus::Partial => LifecycleStatus::Partial,
                ObjectStatus::Disabled => LifecycleStatus::Disabled,
                ObjectStatus::Failed => LifecycleStatus::Failed,
            };
            assert_eq!(i.status, expected_status, "{o:?}");
        }
        if let MigrationOutcome::Quarantined(q) = &result.outcome {
            assert_eq!(&q.record, o, "quarantine must preserve the record verbatim");
        }
    }

    #[test]
    fn exhaustive_combinations_match_spec_table() {
        let ownerships = [
            None,
            Some(Ownership::RawManaged),
            Some(Ownership::RpmManaged),
            Some(Ownership::RpmObserved),
        ];
        let backends = [None, Some("raw"), Some("rpm"), Some("yum"), Some("brew")];
        let metas = [None, Some(unnamed_meta()), Some(named_meta())];
        let sources = [
            None,
            Some("https://repo.example/pkg.tar.gz"),
            Some("oci://registry.example/pkg"),
        ];
        let file_sets: [Vec<OwnedFile>; 3] = [
            Vec::new(),
            vec![anolisa_file()],
            vec![anolisa_file(), external_file()],
        ];
        let statuses = [ObjectStatus::Installed, ObjectStatus::Adopted];

        let mut checked = 0usize;
        for ownership in ownerships {
            for managed in [true, false] {
                for adopted in [true, false] {
                    for backend in backends {
                        for meta in &metas {
                            for source in sources {
                                for files in &file_sets {
                                    for status in statuses {
                                        let mut o = base_object();
                                        o.ownership = ownership;
                                        o.managed = managed;
                                        o.adopted = adopted;
                                        o.install_backend = backend.map(str::to_string);
                                        o.rpm_metadata = meta.clone();
                                        o.distribution_source = source.map(str::to_string);
                                        o.files = files.clone();
                                        o.status = status;

                                        let result = migrate_object(&o, SCOPE);
                                        assert_eq!(
                                            result.rule,
                                            spec_rule(&o),
                                            "rule mismatch for {o:?}"
                                        );
                                        assert_shape(&o, &result);
                                        assert_safety(&o, &result);
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 4 * 2 * 2 * 5 * 3 * 3 * 3 * 2);
    }

    #[test]
    fn new_model_round_trips_through_toml() {
        let mut o = base_object();
        o.ownership = Some(Ownership::RpmManaged);
        o.rpm_metadata = Some(named_meta());
        let installation = expect_active(migrate_object(&o, SCOPE));
        let encoded = toml::to_string(&installation).expect("serialize installation");
        let decoded: Installation = toml::from_str(&encoded).expect("deserialize installation");
        assert_eq!(decoded, installation);

        let mut raw = base_object();
        raw.ownership = Some(Ownership::RawManaged);
        raw.files = vec![anolisa_file()];
        let installation = expect_active(migrate_object(&raw, SCOPE));
        let encoded = toml::to_string(&installation).expect("serialize owned installation");
        let decoded: Installation = toml::from_str(&encoded).expect("deserialize owned");
        assert_eq!(decoded, installation);
    }
}
