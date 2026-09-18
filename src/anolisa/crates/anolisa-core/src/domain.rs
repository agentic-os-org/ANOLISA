//! Authority-centric domain model for installations.
//!
//! The core axis is who
//! owns the installation fact: [`ProviderBinding::Owned`] — ANOLISA is the
//! authority and the state record *is* the fact; [`ProviderBinding::Delegated`]
//! — a native package manager is the authority and the state record is a
//! management claim plus a non-authoritative observation cache.
//!
//! These types intentionally do **not** deserialize the legacy
//! `installed.toml` object layout (schema ≤ 4). Legacy records enter the
//! domain exclusively through [`crate::state_migration::migrate_object`], so
//! the historical field combinations are interpreted in exactly one place.

use serde::{Deserialize, Serialize};

use crate::state::{
    ExternalModifiedFile, HealthEntry, ObjectKind, OwnedFile, ServiceRef, SubscriptionScope,
};

/// Scope an installation belongs to. Claim uniqueness and provider conflicts
/// are judged per scope: the same component may have one installation in
/// `System` and one in `User` (shadow policy applies between them).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallationScope {
    /// System-wide FHS scope.
    System,
    /// Per-user scope, keyed by uid.
    User { uid: u32 },
}

/// Native package managers ANOLISA can delegate to. Open set: adding a
/// variant adds an adapter, never new lifecycle semantics — all variants
/// share the `Delegated` authority class.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativePm {
    /// rpm/dnf family (rpmdb is the authority).
    Rpm,
}

/// Identity of the native package backing a delegated installation.
///
/// `Unresolved` exists for records migrated from pre-v3 state, which never
/// stored a package name. The first planning pass resolves it through the
/// alias index and re-observes; resolution failure routes to `repair`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "resolution")]
pub enum PackageIdentity {
    /// Package name is known and usable for native-db queries.
    Resolved { name: String },
    /// Package name must be derived from the component before first use.
    Unresolved { component_hint: String },
}

impl PackageIdentity {
    /// Package name when already resolved.
    pub fn resolved_name(&self) -> Option<&str> {
        match self {
            Self::Resolved { name } => Some(name),
            Self::Unresolved { .. } => None,
        }
    }
}

/// Management relation between ANOLISA and a native package. Only exists on
/// the `Delegated` branch: an owned artifact has no adoption concept.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ManagementRelation {
    /// ANOLISA installed the package through a native transaction.
    /// Default uninstall delegates removal to the native manager.
    Managed { since: String },
    /// Pre-existing package the user explicitly adopted. Management consent
    /// exists; removal authority stays with the user (record-only uninstall
    /// unless `--remove-system-package`).
    Adopted { since: String },
    /// Known to exist, no management consent. ANOLISA never runs native
    /// transactions for it — adoption must come first.
    Observed,
}

impl ManagementRelation {
    /// Stable wire label (`managed`, `adopted`, `observed`).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Managed { .. } => "managed",
            Self::Adopted { .. } => "adopted",
            Self::Observed => "observed",
        }
    }
}

/// One observation of the native authority. A non-authoritative cache by
/// type: planning re-observes before acting; this snapshot only serves
/// offline display (`status` without the native db available).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Observation {
    /// Human-facing version at observation time.
    pub version: String,
    /// Full EVR from the native db, when it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evr: Option<String>,
    /// Package architecture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Repository or label that supplied the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    /// RFC3339 UTC timestamp of the observation. Migrated records carry the
    /// legacy `installed_at` here and must be treated as stale.
    pub observed_at: String,
}

/// Artifact fully owned by ANOLISA (raw distribution). The state record is
/// authoritative: these files may be verified, removed, and rolled back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedArtifact {
    /// Installed version.
    pub version: String,
    /// Distribution entry URL that supplied the bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution_source: Option<String>,
    /// Raw package this component resolved to (preserves `--package`
    /// overrides across updates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_package: Option<String>,
    /// Digest of the manifest used for install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
    /// Files ANOLISA owns and may remove on uninstall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<OwnedFile>,
    /// Service units installed by this artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceRef>,
    /// Third-party files touched under explicit backup contracts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_modified_files: Vec<ExternalModifiedFile>,
    /// System packages auto-installed by the provisioner. Never auto-removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provisioned_packages: Vec<String>,
}

/// Who owns the installation fact. This is the closed, two-member axis that
/// determines state validity, failure semantics, and reconciliation — the
/// provider list (raw, rpm, deb, …) is the open axis layered on top.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "authority")]
pub enum ProviderBinding {
    /// ANOLISA is the authority; the record is the fact.
    Owned { artifact: OwnedArtifact },
    /// A native package manager is the authority; the record is a claim
    /// plus an observation cache.
    Delegated {
        pm: NativePm,
        package: PackageIdentity,
        relation: ManagementRelation,
        /// Last successful observation; `None` means never observed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_observed: Option<Observation>,
    },
}

impl ProviderBinding {
    /// Whether default uninstall may remove backing files or packages.
    pub fn owns_removal(&self) -> bool {
        match self {
            Self::Owned { .. } => true,
            Self::Delegated { relation, .. } => {
                matches!(relation, ManagementRelation::Managed { .. })
            }
        }
    }

    /// Whether the installation fact lives in a native package database.
    pub fn is_delegated(&self) -> bool {
        matches!(self, Self::Delegated { .. })
    }

    /// Best-known version: authoritative for `Owned`, last observation for
    /// `Delegated` (stale by definition — planning re-observes).
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Owned { artifact } => Some(&artifact.version),
            Self::Delegated { last_observed, .. } => {
                last_observed.as_ref().map(|o| o.version.as_str())
            }
        }
    }
}

/// Lifecycle health of an installation. Narrowed from the legacy
/// `ObjectStatus`: "adopted" is a management relation, not a status, so it
/// no longer appears here.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    /// Fully installed and active.
    Installed,
    /// Partially installed or degraded dependency.
    Partial,
    /// Present but intentionally inactive.
    Disabled,
    /// Last mutating operation failed or health checks found a hard error.
    Failed,
}

/// Aggregate root: one component installed in one scope. At most one
/// installation exists per (kind, name, scope).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Installation {
    /// Object vocabulary shared with the legacy state (component, adapter,
    /// osbase). `Capability` never reaches the domain — migration drops it.
    pub kind: ObjectKind,
    /// Stable object name from the manifest/catalog.
    pub name: String,
    /// Scope this installation belongs to.
    pub scope: InstallationScope,
    /// Who owns the installation fact.
    pub binding: ProviderBinding,
    /// Lifecycle health.
    pub status: LifecycleStatus,
    /// RFC3339 UTC timestamp when this object entered state.
    pub installed_at: String,
    /// Last operation that changed this object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_operation_id: Option<String>,
    /// Subscription entitlement attached to this object.
    #[serde(default)]
    pub subscription_scope: SubscriptionScope,
    /// Enabled feature names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_features: Vec<String>,
    /// Cached health results from the last status/probe pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub health: Vec<HealthEntry>,
}
