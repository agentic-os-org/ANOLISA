//! Adapter receipt schema (`AdapterClaim`) and its security-boundary
//! [`ClaimResource`] model.
//!
//! A receipt is **pure data**: it records what a framework driver took
//! over on behalf of one component, so [`status`](super::manager) and
//! [`disable`](super::manager) can run later without re-reading the
//! resource directory and without trusting any executable instruction
//! from disk. Receipts never carry executable argv, script paths, or reverse
//! commands. [`AdapterNotice::command`](crate::manifest::AdapterNotice::command)
//! is the sole command-like string: an inert display hint that must never be
//! parsed into argv or executed. Framework CLI invocations are constructed by
//! built-in drivers, not read back from receipts.
//!
//! Every value that `status`/`disable` would interpret as a path, a
//! symlink, or a framework-registry entry must live in [`ClaimResource`],
//! the closed set the Manager re-validates before handing the claim to a
//! driver. The framework-specific [`DriverPayload`] may only hold typed
//! data the driver needs to *understand* the receipt; it is never a path
//! safety boundary and must reference paths by [`ClaimResource::id`]
//! rather than duplicating them.
//!
//! Wire format note: the enums here are **externally tagged** (serde
//! default, no `#[serde(flatten)]`). `toml` 0.8 mis-serializes
//! internally-tagged enums combined with `flatten`; externally-tagged
//! variants round-trip cleanly as long as scalar fields are declared
//! before nested tables/arrays. The round-trip is pinned by the
//! `adapter_claim_toml_round_trip` test.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::path_safety::{PathBoundaryError, canonicalize_nearest_existing, validate_owned_path};
use anolisa_platform::fs_layout::FsLayout;

/// Schema version for the generic claim shape and [`ClaimResource`].
/// Persisted in every receipt so a future on-disk migration can branch.
pub const CLAIM_SCHEMA_VERSION: u32 = 2;

/// Schema version for [`DriverPayload`]. Bumped independently of
/// [`CLAIM_SCHEMA_VERSION`] when a driver's typed payload changes shape.
///
/// 4 adds [`OpenClawClaim::displaced_plugins`], whose entries carry
/// [`DisplacedPluginRef::applied`]. Both are defaulted on the wire, so a
/// version-3 receipt still parses and a version-4 receipt written before
/// `applied` existed still reads as applied; the bump records that a payload
/// written from version 4 on may carry them.
///
/// **Write-time version, never a read gate.** This is the number every driver
/// stamps into the receipt it is writing *now*; it says nothing about which
/// fields an older receipt on disk could have carried. A driver that needs to
/// know whether a receipt already had one of its fields must gate on a
/// driver-local constant naming the version that introduced it — see
/// `QODER_INSTALL_CONFIRMED_MIN_SCHEMA` in [`super::qoder`]. Gating a read on
/// this shared constant instead would revoke that field's meaning from every
/// existing receipt the moment an unrelated driver's payload changed shape.
pub const DRIVER_SCHEMA_VERSION: u32 = 4;

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

/// [`DisplacedPluginRef::applied`] defaults to applied, which is the compatible
/// reading for a receipt written before the field existed — see its doc for why
/// that is also the safer of the two defaults.
fn displacement_applied_default() -> bool {
    true
}

/// A single adapter receipt: "the current user's `component` has, through
/// `framework`'s driver, taken over the framework-side state described by
/// `resources`".
///
/// Persisted in the user-level `installed.toml` as `[[adapter_claims]]`,
/// alongside `[[objects]]`. Scalar fields are declared first so the TOML
/// serializer emits them before the `resources` array and the
/// `driver_payload` table (TOML requires scalars to precede sub-tables
/// within a table).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterClaim {
    /// Generic claim + [`ClaimResource`] schema version
    /// ([`CLAIM_SCHEMA_VERSION`] at write time).
    pub claim_schema: u32,
    /// ANOLISA component this receipt belongs to.
    pub component: String,
    /// Framework name; must resolve to a built-in driver.
    pub framework: String,
    /// Framework-native plugin id, when the framework has one. Sanitized
    /// before it ever enters an argv (see [`validate_plugin_id`]). The
    /// authoritative copy for CLI use lives in the
    /// [`ClaimResourceKind::FrameworkPlugin`] resource; this top-level
    /// field is a convenience for listing/scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Adapter type declared at enable time. Persisted so status/disable can
    /// preserve skill-only semantics without trusting the current manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_type: Option<String>,
    /// RFC3339 UTC timestamp when enable last wrote this receipt.
    pub enabled_at: String,
    /// Resource directory read at enable time. Kept for status display and
    /// upgrade detection; `disable` must NOT depend on it still existing.
    pub resource_root: PathBuf,
    /// Legacy whole-tree digest read only to migrate pre-v2 receipts. New
    /// receipts never write it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_digest: Option<String>,
    /// Package-manager-owned adapter inputs captured at enable time. The root
    /// is the resolved source root and every file path is relative to it, so
    /// a source-root migration changes the revision even when bytes do not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<AdapterSourceRevision>,
    /// Files ANOLISA explicitly materialized into framework-owned resources.
    /// Runtime additions under those resources are intentionally absent and
    /// therefore ignored by status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materialized_files: Vec<MaterializedFile>,
    /// [`DriverPayload`] schema version ([`DRIVER_SCHEMA_VERSION`] at write
    /// time).
    pub driver_schema: u32,
    /// Lifecycle status of the receipt itself.
    pub status: ClaimStatus,
    /// Static, display-only notices declared in the component manifest at
    /// enable time. Persisted so `disable` can show `post_disable` notices
    /// from the receipt alone, without depending on the manifest still
    /// being present (same rationale as `adapter_type`). Inert text: never
    /// shell-expanded, template-substituted, or executed. Declared after
    /// the scalar fields so TOML emits it among the sub-tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<crate::manifest::AdapterNotice>,
    /// Manager-validatable resource declarations — the receipt's security
    /// boundary. Re-validated before every `status`/`disable`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ClaimResource>,
    /// Framework-specific typed payload. Closed enum, no free-form map.
    pub driver_payload: DriverPayload,
}

impl AdapterClaim {
    /// Whether this receipt represents a skill-only adapter bundle.
    pub fn is_skill_bundle(&self) -> bool {
        self.adapter_type.as_deref() == Some("skill_bundle")
    }

    /// Find a resource by its stable `id`.
    pub fn resource(&self, id: &str) -> Option<&ClaimResource> {
        self.resources.iter().find(|r| r.id == id)
    }

    /// Re-validate every [`ClaimResource`] against the current layout and
    /// the driver's static external roots, plus any embedded `plugin_id`.
    ///
    /// The Manager calls this before writing a receipt, after reading one
    /// back, and before handing the claim to a driver's `status`/`disable`
    /// — so a forged `installed.toml` cannot widen ANOLISA's authority to
    /// an arbitrary path or smuggle a shell metacharacter into an argv.
    ///
    /// # Errors
    ///
    /// Returns the first [`ClaimValidationError`] encountered: an owned
    /// path outside ANOLISA roots, an external path outside every
    /// `allowed_external_roots` entry, a traversal/symlink escape, or an
    /// invalid plugin id.
    pub fn validate(
        &self,
        layout: &FsLayout,
        allowed_external_roots: &[PathBuf],
    ) -> Result<(), ClaimValidationError> {
        self.validate_with_owned_roots(layout, allowed_external_roots, &[])
    }

    /// Like [`Self::validate`], but additionally trusts `extra_owned_roots`
    /// as ANOLISA-owned locations for symlink *targets*.
    ///
    /// A symlink target (e.g. Codex's plugin symlink pointing back at the
    /// resource bundle) is normally validated against the primary layout's
    /// owned roots. When the bundle is resolved from a packaged datadir that
    /// differs from `layout.datadir` (registered via
    /// [`push_primary_datadir_root`](super::manager::AdapterManager::push_primary_datadir_root)),
    /// that target lives outside the layout roots yet is still legitimately
    /// ANOLISA-owned. The Manager passes its trusted datadir roots here so
    /// such targets validate.
    ///
    /// `extra_owned_roots` MUST come from Manager configuration (visible
    /// datadir roots), never from receipt fields such as `resource_root` —
    /// otherwise a forged receipt could authorize its own symlink target.
    ///
    /// # Errors
    ///
    /// Returns the first [`ClaimValidationError`] encountered: an owned
    /// path outside ANOLISA roots, an external path outside every
    /// `allowed_external_roots` entry, a traversal/symlink escape, or an
    /// invalid plugin id.
    pub fn validate_with_owned_roots(
        &self,
        layout: &FsLayout,
        allowed_external_roots: &[PathBuf],
        extra_owned_roots: &[PathBuf],
    ) -> Result<(), ClaimValidationError> {
        self.validate_with_trust(layout, allowed_external_roots, extra_owned_roots, &[])
    }

    /// Like [`Self::validate_with_owned_roots`], with one more allowance:
    /// a symlink target that is byte-for-byte **equal** to an entry of
    /// `exact_symlink_targets` validates even though it is under none of
    /// the owned roots.
    ///
    /// This carries the enable-time anchor (see
    /// `StateStore::find_adapter_trust_root`): after an RPM update moves a
    /// contract's external resource root, the prior receipt's target is no
    /// longer derivable from the current contract, yet status/disable must
    /// still be able to report and clean it up, and re-enable must migrate
    /// it. Exact equality is deliberate — an entry here authorizes one
    /// path, never a subtree, so a forged anchor (e.g. `/etc`) cannot
    /// widen validation to paths beneath it (e.g. `/etc/cron.d/evil`).
    /// Relative entries never match: anchors are recorded absolute.
    ///
    /// # Errors
    ///
    /// Returns the first [`ClaimValidationError`] encountered: an owned
    /// path outside ANOLISA roots, an external path outside every
    /// `allowed_external_roots` entry, a traversal/symlink escape, or an
    /// invalid plugin id.
    pub fn validate_with_trust(
        &self,
        layout: &FsLayout,
        allowed_external_roots: &[PathBuf],
        extra_owned_roots: &[PathBuf],
        exact_symlink_targets: &[PathBuf],
    ) -> Result<(), ClaimValidationError> {
        if let Some(pid) = &self.plugin_id {
            if self.framework == "dsh" {
                validate_dsh_package_name(pid)?;
            } else {
                validate_plugin_id(pid)?;
            }
        }
        for resource in &self.resources {
            resource.validate_with_owned_roots(
                layout,
                allowed_external_roots,
                extra_owned_roots,
                exact_symlink_targets,
            )?;
            match &resource.kind {
                ClaimResourceKind::FrameworkPlugin { framework, .. }
                | ClaimResourceKind::FrameworkMarketplace { framework, .. }
                | ClaimResourceKind::FrameworkConfig { framework, .. }
                    if framework != &self.framework =>
                {
                    return Err(ClaimValidationError::FrameworkMismatch {
                        id: resource.id.clone(),
                        resource_framework: framework.clone(),
                        claim_framework: self.framework.clone(),
                    });
                }
                _ => {}
            }
        }
        validate_integrity_entries(self)?;
        Ok(())
    }

    /// True when validating this receipt actually depends on external
    /// symlink-target trust: some [`ClaimResourceKind::Symlink`] resource
    /// has a *target* that does not validate as ANOLISA-owned (primary
    /// layout roots plus `trusted_owned_roots`).
    ///
    /// This is the Manager's anchor-persistence criterion. A receipt whose
    /// symlink targets all re-validate from the static boundary on every
    /// run — or one with no symlink resources at all (every driver but
    /// Codex today) — never reads an anchor back, so persisting one would
    /// only bump the state schema for nothing. The check reuses the same
    /// owned-target validation as [`ClaimResource::validate_with_owned_roots`],
    /// so the persistence condition cannot drift from the consumption
    /// condition.
    pub fn requires_external_symlink_trust(
        &self,
        layout: &FsLayout,
        trusted_owned_roots: &[PathBuf],
    ) -> bool {
        self.resources.iter().any(|res| match &res.kind {
            ClaimResourceKind::Symlink { target, .. } => {
                validate_owned_symlink_target(layout, target, trusted_owned_roots).is_err()
            }
            _ => false,
        })
    }
}

/// Follow-up reason shared by adapter status and component update actions.
pub const SOURCE_REVISION_CHANGED_REASON: &str = "adapter source revision changed since enable";

/// Package-managed file type persisted in source and materialized revisions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ManagedFileKind {
    /// Regular file verified by SHA-256.
    File,
    /// Symbolic link verified by its literal target.
    Symlink,
}

/// One package-managed input relative to an adapter source root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManagedSourceFile {
    /// Relative path below [`AdapterSourceRevision::source_root`].
    pub relative_path: PathBuf,
    /// File or symbolic link.
    pub kind: ManagedFileKind,
    /// Lowercase SHA-256 for regular files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Literal link target for symbolic links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<PathBuf>,
}

/// Canonical adapter input revision captured from package-manager ownership.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterSourceRevision {
    /// Canonical source root selected from the installed component contract.
    pub source_root: PathBuf,
    /// Sorted package-managed inputs below the source root.
    pub files: Vec<ManagedSourceFile>,
    /// Additional package-managed source roots ANOLISA explicitly copies.
    /// Resource ids keep multiple declared copies distinct without assigning
    /// semantic file roles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materialized_sources: Vec<MaterializedSourceRevision>,
}

/// One explicit-copy source included in an adapter input revision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MaterializedSourceRevision {
    /// Destination [`ClaimResource::id`] used to identify the copy mapping.
    pub resource_id: String,
    /// Canonical package-managed source root copied into that resource.
    pub source_root: PathBuf,
    /// Sorted package-managed inputs below the source root.
    pub files: Vec<ManagedSourceFile>,
}

/// One expected file below a receipt-declared materialized resource.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MaterializedFile {
    /// [`ClaimResource::id`] of the copied destination root.
    pub resource_id: String,
    /// Relative path below that resource.
    pub relative_path: PathBuf,
    /// File or symbolic link.
    pub kind: ManagedFileKind,
    /// Lowercase SHA-256 for regular files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Literal link target for symbolic links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<PathBuf>,
}

/// Lifecycle status of a receipt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// Adapter is enabled and the receipt is authoritative.
    Enabled,
    /// A prior `disable` could not fully clean up; the receipt is kept so
    /// the cleanup can be retried.
    CleanupFailed,
}

/// One entry in a receipt's `resources` list — the unit the Manager
/// validates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimResource {
    /// Stable id referenced from [`DriverPayload`] and condition reports.
    pub id: String,
    /// Human-facing role, e.g. `openclaw_state_dir`.
    pub purpose: String,
    /// The typed, validatable resource.
    pub kind: ClaimResourceKind,
}

impl ClaimResource {
    /// Validate this resource against ANOLISA-owned roots (for owned
    /// paths) or the driver's static external roots (for external paths),
    /// and sanitize any embedded plugin id.
    ///
    /// # Errors
    ///
    /// See [`AdapterClaim::validate`].
    pub fn validate(
        &self,
        layout: &FsLayout,
        allowed_external_roots: &[PathBuf],
    ) -> Result<(), ClaimValidationError> {
        self.validate_with_owned_roots(layout, allowed_external_roots, &[], &[])
    }

    /// Like [`Self::validate`], but trusts `extra_owned_roots` as additional
    /// ANOLISA-owned locations for a symlink *target*, and
    /// `exact_symlink_targets` as byte-for-byte target allowances. See
    /// [`AdapterClaim::validate_with_trust`] for the trust contract.
    ///
    /// # Errors
    ///
    /// See [`AdapterClaim::validate`].
    pub fn validate_with_owned_roots(
        &self,
        layout: &FsLayout,
        allowed_external_roots: &[PathBuf],
        extra_owned_roots: &[PathBuf],
        exact_symlink_targets: &[PathBuf],
    ) -> Result<(), ClaimValidationError> {
        match &self.kind {
            ClaimResourceKind::OwnedPath { path } => {
                validate_owned_path(layout, path).map_err(|source| {
                    ClaimValidationError::OwnedPath {
                        id: self.id.clone(),
                        source,
                    }
                })
            }
            ClaimResourceKind::ExternalPath { path } => {
                validate_external_path(path, allowed_external_roots).map_err(|source| {
                    ClaimValidationError::ExternalPath {
                        id: self.id.clone(),
                        source,
                    }
                })
            }
            ClaimResourceKind::Symlink { link, target } => {
                // The `link` is a per-user framework path, validated against
                // the driver's static external roots so disable removes only
                // ANOLISA's own entry. We validate the link *location*
                // without resolving the link itself: canonicalizing the link
                // would follow it to its (owned) target and wrongly reject
                // an in-boundary link. The `target` must be an
                // ANOLISA-owned path (validated against the *trusted layout*
                // roots plus any Manager-supplied trusted datadir roots,
                // never the receipt-derived external roots): a forged receipt
                // must not be able to point a claimed symlink at, say,
                // `/etc` and have it validate. Owned-path validation is
                // independent of the receipt, closing the self-authorization
                // hole.
                validate_external_link_location(link, allowed_external_roots).map_err(
                    |source| ClaimValidationError::ExternalPath {
                        id: self.id.clone(),
                        source,
                    },
                )?;
                // Anchor allowance: equality only, absolute only — an
                // anchored path never authorizes anything beneath it. See
                // [`AdapterClaim::validate_with_trust`].
                if target.is_absolute()
                    && exact_symlink_targets
                        .iter()
                        .any(|allowed| allowed == target)
                {
                    return Ok(());
                }
                validate_owned_symlink_target(layout, target, extra_owned_roots).map_err(|source| {
                    ClaimValidationError::OwnedPath {
                        id: self.id.clone(),
                        source,
                    }
                })
            }
            ClaimResourceKind::FrameworkPlugin {
                framework,
                plugin_id,
            } if framework == "dsh" => validate_dsh_package_name(plugin_id),
            ClaimResourceKind::FrameworkPlugin { plugin_id, .. } => validate_plugin_id(plugin_id),
            ClaimResourceKind::FrameworkMarketplace { marketplace, .. } => {
                validate_marketplace_name(marketplace).map_err(|_| {
                    ClaimValidationError::MarketplaceName {
                        id: self.id.clone(),
                        marketplace: marketplace.clone(),
                    }
                })
            }
            ClaimResourceKind::FrameworkConfig { key, .. } => {
                if key.is_empty() {
                    return Err(ClaimValidationError::ConfigKey {
                        id: self.id.clone(),
                        reason: "config key must not be empty".to_string(),
                    });
                }
                validate_config_key(key).map_err(|_| ClaimValidationError::ConfigKey {
                    id: self.id.clone(),
                    reason: format!("config key '{key}' contains unsafe characters"),
                })
            }
        }
    }
}

/// Confirmation state for a framework configuration mutation.
///
/// `Pending` is durable write-ahead intent: the command has not yet produced
/// a confirmed success, so the host may or may not contain the requested
/// value. Existing receipts omit this field and therefore deserialize as
/// `Applied`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigApplyState {
    /// The framework command completed successfully.
    #[default]
    Applied,
    /// The mutation is about to run or its outcome is uncertain.
    Pending,
}

impl ConfigApplyState {
    fn is_applied(&self) -> bool {
        *self == Self::Applied
    }
}

/// The closed set of resource kinds a receipt may declare.
///
/// Additional kinds (`Tree`, `JsonKeys`) are introduced when their first
/// driver lands — adding a variant here is a deliberate, reviewed
/// extension of the security boundary, never an open map. `Symlink` and
/// `FrameworkMarketplace` landed with the Codex/Claude Code drivers.
///
/// Externally tagged with snake_case variant keys (`owned_path`,
/// `external_path`, `framework_plugin`, `symlink`, `framework_marketplace`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimResourceKind {
    /// A path inside an ANOLISA-owned root; validated by
    /// [`validate_owned_path`].
    OwnedPath {
        /// Absolute owned path.
        path: PathBuf,
    },
    /// A path in a framework/user directory. Validated against the
    /// driver's static `allowed_external_roots` only — the receipt does
    /// **not** get to declare its own allowed root (that would let a
    /// forged receipt authorize itself).
    ExternalPath {
        /// Absolute external path.
        path: PathBuf,
    },
    /// A symlink ANOLISA created and took over. The `link` location is
    /// validated against the driver's static external roots, while the
    /// `target` must live under ANOLISA-owned roots (including trusted
    /// datadir roots supplied by the Manager for packaged bundles).
    Symlink {
        /// Absolute path of the link ANOLISA created.
        link: PathBuf,
        /// Absolute ANOLISA-owned path the link points at.
        target: PathBuf,
    },
    /// A record in a framework's plugin registry. `plugin_id` is
    /// whitelist-sanitized before it enters any argv.
    FrameworkPlugin {
        /// Framework that owns the registry (e.g. `openclaw`).
        framework: String,
        /// Native plugin id.
        plugin_id: String,
    },
    /// A source registered in a framework's marketplace (e.g. Codex,
    /// Claude Code). `marketplace` is whitelist-sanitized before it enters
    /// any argv.
    FrameworkMarketplace {
        /// Framework that owns the marketplace (e.g. `codex`).
        framework: String,
        /// Marketplace name ANOLISA registered.
        marketplace: String,
    },
    /// A framework configuration key ANOLISA attempted to apply.
    FrameworkConfig {
        /// Framework that owns the config (e.g. `openclaw`).
        framework: String,
        /// Config key path.
        key: String,
        /// Whether the framework confirmed the mutation. Applied is omitted
        /// on the wire to preserve compatibility with existing receipts.
        #[serde(default, skip_serializing_if = "ConfigApplyState::is_applied")]
        state: ConfigApplyState,
    },
}

/// Framework-specific typed payload. Closed enum — there is no runtime
/// custom-type escape hatch. The variant key doubles as the
/// `driver_payload_kind` discriminator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DriverPayload {
    /// OpenClaw driver payload.
    #[serde(rename = "openclaw")]
    OpenClaw(OpenClawClaim),
    /// Hermes driver payload.
    #[serde(rename = "hermes")]
    Hermes(HermesClaim),
    /// Cosh (copilot-shell) driver payload.
    #[serde(rename = "cosh")]
    Cosh(CoshClaim),
    /// Codex driver payload.
    #[serde(rename = "codex")]
    Codex(CodexClaim),
    /// Claude Code driver payload.
    #[serde(rename = "claude_code")]
    ClaudeCode(ClaudeCodeClaim),
    /// Qoder (qodercli) driver payload.
    #[serde(rename = "qoder")]
    Qoder(QoderClaim),
    /// Qwen Code driver payload.
    #[serde(rename = "qwencode")]
    QwenCode(QwenCodeClaim),
    /// QwenPaw driver payload.
    #[serde(rename = "qwenpaw")]
    QwenPaw(QwenPawClaim),
    /// DeepSeek Harness (`dsh`) native plugin payload.
    #[serde(rename = "dsh")]
    Dsh(DshClaim),
}

/// OpenClaw driver payload. Holds only [`ClaimResource::id`] references —
/// never the paths themselves — so the validated `resources` list stays
/// the single source of truth for path data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenClawClaim {
    /// Resource id of the OpenClaw state/home directory
    /// ([`ClaimResourceKind::ExternalPath`]).
    pub state_dir_resource: String,
    /// Resource id of the registered plugin
    /// ([`ClaimResourceKind::FrameworkPlugin`]).
    pub plugin_resource: String,
    /// Resource ids of delivered skill directories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skill_resources: Vec<String>,
    /// Resource ids of applied config key/value pairs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_resources: Vec<String>,
    /// Framework plugins this enable displaced, each an id reference plus the
    /// exclusive slot the plugin re-takes when restored. Empty on receipts
    /// written before [`DRIVER_SCHEMA_VERSION`] 4 and for adapters that
    /// displace nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub displaced_plugins: Vec<DisplacedPluginRef>,
}

/// One displaced framework plugin recorded in an [`OpenClawClaim`].
///
/// Holds a [`ClaimResource::id`] reference — never a path — so the validated
/// `resources` list stays the single source of truth for what was taken over.
/// The resource it points at is a
/// [`ClaimResourceKind::FrameworkPlugin`] whose `plugin_id` is the plugin the
/// driver disabled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplacedPluginRef {
    /// Resource id of the displaced plugin
    /// ([`ClaimResourceKind::FrameworkPlugin`]).
    pub resource: String,
    /// Exclusive framework slot the plugin re-takes when it is enabled again,
    /// as the key suffix under `plugins.slots` (e.g. `memory`).
    ///
    /// `None` does **not** make the restore unconditional; it only means there is
    /// no slot to consult. The driver still skips a restore the host has made
    /// pointless or impossible — the plugin has left its inventory, or a policy
    /// key keeps it off — and reports that as released rather than failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// Whether the framework command that performs this hand-off has been issued.
    ///
    /// A driver records a displacement while it is still preparing the enable,
    /// and the receipt is persisted before anything mutates — deliberately, so a
    /// crash cannot leave a framework mutation with no receipt behind it. But the
    /// command itself may only run much later, after the adapter's own plugin is
    /// installed and verified, so an enable that fails in between leaves a
    /// `cleanup_failed` receipt describing a hand-off that never happened.
    ///
    /// Acting on that record is worse than not having it: a later disable would
    /// restore a plugin this adapter never displaced, undoing a disable the
    /// operator did themselves in the meantime and leaving nothing behind to show
    /// why. So the driver sets this once the command has been issued, and every
    /// consumer that would act on the ownership — the restore branch, its dry-run
    /// preview, and `status` — checks it first.
    ///
    /// Absent means applied. Besides being the compatible reading for an older
    /// receipt, it is the safer default: the alternative silently drops ownership
    /// a host may be relying on, and a stranded host with nothing behind the slot
    /// costs more than one unwanted restore.
    #[serde(
        default = "displacement_applied_default",
        skip_serializing_if = "is_true"
    )]
    pub applied: bool,
}

/// Hermes driver payload. Holds only [`ClaimResource::id`] references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HermesClaim {
    /// Resource id of the Hermes home directory
    /// ([`ClaimResourceKind::ExternalPath`]).
    pub home_resource: String,
    /// Resource id of the installed plugin directory
    /// ([`ClaimResourceKind::ExternalPath`]).
    pub plugin_resource: String,
    /// Resource ids of delivered skill directories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skill_resources: Vec<String>,
}

/// Cosh (copilot-shell) driver payload. Holds only [`ClaimResource::id`]
/// references. Cosh is extension-based: ANOLISA drops an auto-discovered
/// extension tree into the user's cosh home and takes over only that
/// directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoshClaim {
    /// Resource id of the delivered extension directory
    /// ([`ClaimResourceKind::ExternalPath`]).
    pub extension_dir_resource: String,
}

/// Codex driver payload. Holds only [`ClaimResource::id`] references. Codex
/// requires a local marketplace layout (a directory plus a symlink to the
/// resource root) before a plugin can be added from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexClaim {
    /// Resource id of the marketplace root directory ANOLISA created
    /// ([`ClaimResourceKind::ExternalPath`]).
    pub marketplace_dir_resource: String,
    /// Resource id of the plugin symlink under the marketplace root
    /// ([`ClaimResourceKind::Symlink`]).
    pub symlink_resource: String,
    /// Resource id of the registered marketplace
    /// ([`ClaimResourceKind::FrameworkMarketplace`]).
    pub marketplace_resource: String,
    /// Resource id of the installed plugin
    /// ([`ClaimResourceKind::FrameworkPlugin`]).
    pub plugin_resource: String,
}

/// Claude Code driver payload. Holds only [`ClaimResource::id`] references.
/// Claude Code owns its own registry and settings; ANOLISA only registers a
/// marketplace pointing at the shared resource root and installs the plugin
/// — it never writes `~/.claude/settings.json` directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaudeCodeClaim {
    /// Resource id of the registered marketplace
    /// ([`ClaimResourceKind::FrameworkMarketplace`]).
    pub marketplace_resource: String,
    /// Resource id of the installed plugin
    /// ([`ClaimResourceKind::FrameworkPlugin`]).
    pub plugin_resource: String,
}

/// One Qoder hook entry ANOLISA manages in `settings.json`.
///
/// The `entry` is the fully-resolved JSON object written under
/// `settings.hooks.<event>[]` at enable time. Persisting it in the receipt
/// makes status/disable independent of `resource_root` still existing and
/// keeps cleanup ownership tied to validated receipt data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QoderManagedHook {
    /// Qoder hook event key, e.g. `PreToolUse`.
    pub event: String,
    /// Full hook entry written to `settings.json`.
    pub entry: Value,
}

/// Qoder driver payload.
///
/// Native receipts reference only the framework-managed plugin. Legacy
/// receipts also retain the exact settings resource and hook specs that
/// ANOLISA owns so status and cleanup remain backward compatible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QoderClaim {
    /// Resource id of the installed plugin
    /// ([`ClaimResourceKind::FrameworkPlugin`]).
    pub plugin_resource: String,
    /// Legacy resource id of the user's `settings.json` ANOLISA edits in place
    /// ([`ClaimResourceKind::ExternalPath`]). Native receipts omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_resource: Option<String>,
    /// Whether the native plugin registration existed before ANOLISA enable.
    /// Such a plugin is retained on disable because ANOLISA cannot claim
    /// ownership of framework state it did not create.
    #[serde(default, skip_serializing_if = "is_false")]
    pub plugin_preexisting: bool,
    /// Whether ANOLISA confirmed a successful native plugin installation.
    /// Native enable persists this transition immediately after qodercli
    /// succeeds, so a retained write-ahead receipt never infers ownership
    /// from an install attempt that may not have created the registration.
    #[serde(default, skip_serializing_if = "is_false")]
    pub plugin_install_confirmed: bool,
    /// Hook names ANOLISA merged into `settings.json` at enable time.
    /// These names are metadata for human/debug visibility; lifecycle logic
    /// uses [`Self::managed_hook_specs`]. `disable` prunes only exact hook
    /// entries from that list, so a forged or user-defined `tokenless-*` name
    /// cannot delete a user hook.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed_hooks: Vec<String>,
    /// Full hook entries ANOLISA owns in `settings.json`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed_hook_specs: Vec<QoderManagedHook>,
}

/// Qwen Code driver payload. Qwen owns extension artifacts and activation
/// state through its CLI; the receipt references the exact native extension
/// entry and plugin identity ANOLISA verifies before enabling or uninstalling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QwenCodeClaim {
    /// Resource id of the Qwen-managed extension entry
    /// ([`ClaimResourceKind::ExternalPath`]).
    pub extension_dir_resource: String,
    /// Resource id of the installed extension
    /// ([`ClaimResourceKind::FrameworkPlugin`]).
    pub plugin_resource: String,
}

/// QwenPaw driver payload. QwenPaw copies, validates and hot-loads the
/// plugin through its own CLI; the receipt references the working directory
/// and the installed plugin directory the CLI populated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QwenPawClaim {
    /// Resource id of the QwenPaw working directory
    /// ([`ClaimResourceKind::ExternalPath`]).
    pub home_resource: String,
    /// Resource id of the installed plugin directory
    /// ([`ClaimResourceKind::ExternalPath`]).
    pub plugin_resource: String,
}

/// DeepSeek Harness native-plugin receipt. A single ANOLISA receipt owns the
/// same package across every explicitly selected dsh profile; each profile
/// keeps its own validated framework-plugin resource reference so disable can
/// release exactly the registrations that enable created.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DshClaim {
    /// Package name read from the bundle's `package.json`.
    pub package_name: String,
    /// Resource id of the enable-time dsh home
    /// ([`ClaimResourceKind::ExternalPath`]). Persisting the resolved root
    /// keeps later lifecycle operations independent of process environment
    /// and working-directory drift.
    pub home_resource: String,
    /// Profiles in which ANOLISA registered the package.
    pub profiles: Vec<DshProfileClaim>,
}

/// One profile entry in a [`DshClaim`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DshProfileClaim {
    /// dsh profile identifier passed to the native plugin CLI.
    pub name: String,
    /// Resource id of the profile's registered package.
    pub plugin_resource: String,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Reasons a receipt's resources or plugin id fail validation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ClaimValidationError {
    /// An [`ClaimResourceKind::OwnedPath`] is outside ANOLISA-owned roots.
    #[error("owned-path resource '{id}' failed boundary check: {source}")]
    OwnedPath {
        /// Offending resource id.
        id: String,
        /// Underlying boundary error.
        #[source]
        source: PathBoundaryError,
    },
    /// An [`ClaimResourceKind::ExternalPath`] is outside every allowed
    /// external root, or contains a traversal/symlink escape.
    #[error("external-path resource '{id}' failed boundary check: {source}")]
    ExternalPath {
        /// Offending resource id.
        id: String,
        /// Underlying boundary error.
        #[source]
        source: ExternalPathError,
    },
    /// A `plugin_id` is empty or contains characters outside the
    /// argv-safe whitelist.
    #[error("invalid plugin id '{plugin_id}': {reason}")]
    PluginId {
        /// The rejected id.
        plugin_id: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A config key in a [`ClaimResourceKind::FrameworkConfig`] resource
    /// is empty or contains unsafe characters.
    #[error("invalid config key in resource '{id}': {reason}")]
    ConfigKey {
        /// Offending resource id.
        id: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A `marketplace` name in a [`ClaimResourceKind::FrameworkMarketplace`]
    /// resource is empty or contains characters outside the argv-safe
    /// whitelist.
    #[error("invalid marketplace name '{marketplace}' in resource '{id}'")]
    MarketplaceName {
        /// Offending resource id.
        id: String,
        /// The rejected marketplace name.
        marketplace: String,
    },
    /// A resource declares a framework that differs from the claim's.
    #[error(
        "resource '{id}' declares framework '{resource_framework}' but claim targets '{claim_framework}'"
    )]
    FrameworkMismatch {
        /// Offending resource id.
        id: String,
        /// Framework in the resource.
        resource_framework: String,
        /// Framework in the claim.
        claim_framework: String,
    },
    /// A persisted source/materialized file entry is malformed or references
    /// a non-path claim resource.
    #[error("invalid adapter integrity record: {reason}")]
    IntegrityRecord {
        /// Validation failure detail.
        reason: String,
    },
}

fn validate_integrity_entries(claim: &AdapterClaim) -> Result<(), ClaimValidationError> {
    let fail = |reason: String| Err(ClaimValidationError::IntegrityRecord { reason });
    if let Some(revision) = &claim.source_revision {
        if !revision.source_root.is_absolute() {
            return fail("source root is not absolute".into());
        }
        if revision.files.is_empty() {
            return fail("source revision has no managed files".into());
        }
        if revision
            .files
            .windows(2)
            .any(|pair| pair[0].relative_path >= pair[1].relative_path)
        {
            return fail("source revision files are not strictly sorted".into());
        }
        for file in &revision.files {
            validate_integrity_file(
                &file.relative_path,
                file.kind,
                file.sha256.as_deref(),
                file.symlink_target.as_deref(),
            )?;
        }
        if revision.materialized_sources.windows(2).any(|pair| {
            (&pair[0].resource_id, &pair[0].source_root)
                >= (&pair[1].resource_id, &pair[1].source_root)
        }) {
            return fail("materialized source revisions are not strictly sorted".into());
        }
        for source in &revision.materialized_sources {
            if source.resource_id.is_empty() {
                return fail("materialized source revision has an empty resource id".into());
            }
            let Some(resource) = claim.resource(&source.resource_id) else {
                return fail(format!(
                    "materialized source references unknown resource '{}'",
                    source.resource_id
                ));
            };
            if !matches!(
                resource.kind,
                ClaimResourceKind::OwnedPath { .. } | ClaimResourceKind::ExternalPath { .. }
            ) {
                return fail(format!(
                    "materialized source resource '{}' is not a filesystem root",
                    source.resource_id
                ));
            }
            if !source.source_root.is_absolute() {
                return fail(format!(
                    "materialized source '{}' root is not absolute",
                    source.resource_id
                ));
            }
            if source.files.is_empty() {
                return fail(format!(
                    "materialized source '{}' has no managed files",
                    source.resource_id
                ));
            }
            if source
                .files
                .windows(2)
                .any(|pair| pair[0].relative_path >= pair[1].relative_path)
            {
                return fail(format!(
                    "materialized source '{}' files are not strictly sorted",
                    source.resource_id
                ));
            }
            for file in &source.files {
                validate_integrity_file(
                    &file.relative_path,
                    file.kind,
                    file.sha256.as_deref(),
                    file.symlink_target.as_deref(),
                )?;
            }
        }
    }
    if claim.materialized_files.windows(2).any(|pair| {
        (
            pair[0].resource_id.as_str(),
            pair[0].relative_path.as_path(),
        ) >= (
            pair[1].resource_id.as_str(),
            pair[1].relative_path.as_path(),
        )
    }) {
        return fail("materialized files are not strictly sorted".into());
    }
    for file in &claim.materialized_files {
        let Some(resource) = claim.resource(&file.resource_id) else {
            return fail(format!(
                "materialized file references unknown resource '{}'",
                file.resource_id
            ));
        };
        if !matches!(
            resource.kind,
            ClaimResourceKind::OwnedPath { .. } | ClaimResourceKind::ExternalPath { .. }
        ) {
            return fail(format!(
                "materialized resource '{}' is not a filesystem root",
                file.resource_id
            ));
        }
        validate_integrity_file(
            &file.relative_path,
            file.kind,
            file.sha256.as_deref(),
            file.symlink_target.as_deref(),
        )?;
    }
    Ok(())
}

fn validate_integrity_file(
    path: &Path,
    kind: ManagedFileKind,
    sha256: Option<&str>,
    symlink_target: Option<&Path>,
) -> Result<(), ClaimValidationError> {
    use std::path::Component;
    let fail = |reason: String| Err(ClaimValidationError::IntegrityRecord { reason });
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return fail(format!(
            "invalid relative managed path '{}'",
            path.display()
        ));
    }
    match kind {
        ManagedFileKind::File => {
            let Some(digest) = sha256 else {
                return fail(format!("managed file '{}' has no SHA-256", path.display()));
            };
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || symlink_target.is_some()
            {
                return fail(format!(
                    "managed file '{}' has invalid digest/type metadata",
                    path.display()
                ));
            }
        }
        ManagedFileKind::Symlink => {
            if sha256.is_some()
                || symlink_target.is_none()
                || symlink_target.is_some_and(|target| target.as_os_str().is_empty())
            {
                return fail(format!(
                    "managed symlink '{}' has invalid target/type metadata",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// Reasons an external path is rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExternalPathError {
    /// Path contains a `.` or `..` segment.
    #[error("path '{path}' contains a '.' or '..' segment")]
    Traversal {
        /// Rejected path.
        path: PathBuf,
    },
    /// Path is not under any allowed external root (lexically or after
    /// canonicalizing the deepest existing ancestor).
    #[error("path '{path}' is not under any allowed external root for this driver")]
    OutsideAllowedRoots {
        /// Rejected path.
        path: PathBuf,
    },
}

/// Validate an external path: reject traversal, require containment under
/// one of `allowed_roots` both lexically and after canonicalizing the
/// deepest existing ancestor (defeats a symlinked ancestor that escapes
/// the root). Mirrors [`validate_owned_path`] but against driver-declared
/// roots instead of the layout's owned roots.
///
/// # Errors
///
/// [`ExternalPathError::Traversal`] for `.`/`..` segments;
/// [`ExternalPathError::OutsideAllowedRoots`] when no allowed root
/// contains the path.
pub fn validate_external_path(
    path: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(), ExternalPathError> {
    use std::path::Component;
    for component in path.components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(ExternalPathError::Traversal {
                path: path.to_path_buf(),
            });
        }
    }
    if !allowed_roots.iter().any(|root| path.starts_with(root)) {
        return Err(ExternalPathError::OutsideAllowedRoots {
            path: path.to_path_buf(),
        });
    }
    if let Some(canonical) = canonicalize_nearest_existing(path) {
        let canonical_roots: Vec<PathBuf> = allowed_roots
            .iter()
            .filter_map(|r| canonicalize_nearest_existing(r))
            .collect();
        if !canonical_roots.is_empty() && !canonical_roots.iter().any(|r| canonical.starts_with(r))
        {
            return Err(ExternalPathError::OutsideAllowedRoots {
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// Validate the *location* of a symlink `link` against `allowed_roots`
/// without following the link itself.
///
/// [`validate_external_path`] canonicalizes the whole path, which for an
/// existing symlink resolves through it to the target — the wrong thing for
/// a claimed link that legitimately points at an ANOLISA-owned path outside
/// the external roots. Instead this rejects traversal, requires the link to
/// live lexically under an allowed root, and canonicalizes only the link's
/// **parent** (catching a symlinked ancestor that escapes the boundary)
/// while leaving the final link component unresolved.
///
/// # Errors
///
/// [`ExternalPathError::Traversal`] for `.`/`..` segments;
/// [`ExternalPathError::OutsideAllowedRoots`] when the link (or its
/// canonicalized parent) is not under any allowed root.
pub fn validate_external_link_location(
    link: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(), ExternalPathError> {
    use std::path::Component;
    for component in link.components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(ExternalPathError::Traversal {
                path: link.to_path_buf(),
            });
        }
    }
    if !allowed_roots.iter().any(|root| link.starts_with(root)) {
        return Err(ExternalPathError::OutsideAllowedRoots {
            path: link.to_path_buf(),
        });
    }
    if let Some(parent) = link.parent()
        && let Some(canonical_parent) = canonicalize_nearest_existing(parent)
    {
        let canonical_roots: Vec<PathBuf> = allowed_roots
            .iter()
            .filter_map(|r| canonicalize_nearest_existing(r))
            .collect();
        if !canonical_roots.is_empty()
            && !canonical_roots
                .iter()
                .any(|r| canonical_parent.starts_with(r))
        {
            return Err(ExternalPathError::OutsideAllowedRoots {
                path: link.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// Validate a symlink `target` as an ANOLISA-owned path.
///
/// A target is accepted when it lives under one of the primary layout's
/// owned roots ([`validate_owned_path`]) **or** under one of
/// `extra_owned_roots` — trusted datadir roots supplied by the Manager for
/// bundles resolved from a packaged datadir that differs from
/// `layout.datadir`. `extra_owned_roots` must be Manager configuration, not
/// receipt data, so a forged target (e.g. `/etc`) that is under neither is
/// rejected.
///
/// # Errors
///
/// The [`PathBoundaryError`] from the layout check when the target is under
/// neither the layout roots nor any extra trusted root.
fn validate_owned_symlink_target(
    layout: &FsLayout,
    target: &Path,
    extra_owned_roots: &[PathBuf],
) -> Result<(), PathBoundaryError> {
    match validate_owned_path(layout, target) {
        Ok(()) => Ok(()),
        Err(owned_err) => {
            if !extra_owned_roots.is_empty()
                && validate_external_path(target, extra_owned_roots).is_ok()
            {
                Ok(())
            } else {
                Err(owned_err)
            }
        }
    }
}

/// Reject a plugin id unless it is a non-empty string of argv-safe
/// characters (`[A-Za-z0-9._-]`) that is neither `.`/`..` nor leading
/// with `-` (which an argv parser could mistake for a flag).
///
/// # Errors
///
/// [`ClaimValidationError::PluginId`] with a specific reason.
pub fn validate_plugin_id(plugin_id: &str) -> Result<(), ClaimValidationError> {
    let reject = |reason: &str| {
        Err(ClaimValidationError::PluginId {
            plugin_id: plugin_id.to_string(),
            reason: reason.to_string(),
        })
    };
    if plugin_id.is_empty() {
        return reject("must not be empty");
    }
    if plugin_id == "." || plugin_id == ".." {
        return reject("must not be '.' or '..'");
    }
    if plugin_id.starts_with('-') {
        return reject("must not start with '-' (would be parsed as a flag)");
    }
    if let Some(bad) = plugin_id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(ClaimValidationError::PluginId {
            plugin_id: plugin_id.to_string(),
            reason: format!("contains disallowed character '{bad}'"),
        });
    }
    Ok(())
}

/// Validate an npm-compatible dsh package name before it enters a CLI argv.
/// Scoped names (`@scope/name`) are accepted because native dsh bundles use
/// package-manager names, while traversal, flags, empty segments, and shell
/// metacharacters remain rejected.
pub fn validate_dsh_package_name(package_name: &str) -> Result<(), ClaimValidationError> {
    let reject = |reason: &str| {
        Err(ClaimValidationError::PluginId {
            plugin_id: package_name.to_string(),
            reason: reason.to_string(),
        })
    };
    if package_name.is_empty() {
        return reject("must not be empty");
    }
    if package_name.starts_with('-') || package_name == "." || package_name == ".." {
        return reject("must not be '.'/'..' or start with '-'");
    }
    let mut segments = package_name.split('/');
    let first = segments.next().unwrap_or_default();
    let scoped = first.starts_with('@');
    if scoped {
        if first.len() <= 1 || segments.clone().count() != 1 {
            return reject("scoped names must have exactly one non-empty package segment");
        }
        if first[1..].starts_with('.') || first[1..].starts_with('-') {
            return reject("scope must not start with '.' or '-'");
        }
        if !first[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return reject("scope contains disallowed characters");
        }
    } else if segments.clone().next().is_some() {
        return reject("unscoped names must not contain '/'");
    }
    let name = segments.next().unwrap_or(first);
    if name.is_empty() || name == "." || name == ".." || name.starts_with('-') {
        return reject("package segment is empty, traversal, or starts with '-'");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return reject("contains disallowed character");
    }
    Ok(())
}

/// Reject a marketplace name unless it is a non-empty string of argv-safe
/// characters (`[A-Za-z0-9._-]`) that is neither `.`/`..` nor leading with
/// `-`. Codex/Claude Code marketplace names are passed to the framework
/// CLI (`marketplace add/remove`) and combined into a `plugin@marketplace`
/// argument, so the same whitelist as [`validate_plugin_id`] applies.
///
/// # Errors
///
/// [`ClaimValidationError::PluginId`] with a specific reason (reused so the
/// argv-safety whitelist stays defined in one place).
pub fn validate_marketplace_name(marketplace: &str) -> Result<(), ClaimValidationError> {
    validate_plugin_id(marketplace)
}

/// Reject a skill name that is empty, `.`/`..`, starts with `-`, or
/// contains characters outside `[A-Za-z0-9._-]`. Same whitelist as
/// [`validate_plugin_id`] — a skill name becomes a directory name under
/// the framework's skill root, so it must be path-component-safe.
pub fn validate_skill_name(name: &str) -> Result<(), super::AdapterError> {
    let reject = |reason: String| {
        Err(super::AdapterError::InvalidAdapterInput {
            component: String::new(),
            framework: String::new(),
            reason: format!("invalid skill name '{name}': {reason}"),
        })
    };
    if name.is_empty() {
        return reject("must not be empty".to_string());
    }
    if name == "." || name == ".." {
        return reject("must not be '.' or '..'".to_string());
    }
    if name.starts_with('-') {
        return reject("must not start with '-'".to_string());
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return reject(format!("contains disallowed character '{bad}'"));
    }
    Ok(())
}

/// Reject a config key that is empty or contains shell metacharacters.
/// Allowed: printable ASCII minus `` ` `` `$` `;` `|` `&` `(` `)` `{`
/// `}` `[` `]` `<` `>` `\` `!` `#` `~`. This prevents injection when
/// the key is passed as a CLI argument to `config set`.
pub fn validate_config_key(key: &str) -> Result<(), super::AdapterError> {
    let reject = |reason: String| {
        Err(super::AdapterError::InvalidAdapterInput {
            component: String::new(),
            framework: String::new(),
            reason: format!("invalid config key '{key}': {reason}"),
        })
    };
    if key.is_empty() {
        return reject("must not be empty".to_string());
    }
    const BANNED: &[char] = &[
        '`', '$', ';', '|', '&', '(', ')', '{', '}', '[', ']', '<', '>', '\\', '!', '#', '~', '\'',
        '"', ' ', '\t', '\n', '\r',
    ];
    if let Some(bad) = key.chars().find(|c| BANNED.contains(c) || !c.is_ascii()) {
        return reject(format!("contains disallowed character '{bad}'"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_claim() -> AdapterClaim {
        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: None,
            enabled_at: "2026-06-12T10:30:45Z".to_string(),
            resource_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/openclaw"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "openclaw_state_dir".to_string(),
                    purpose: "openclaw_state_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.openclaw"),
                    },
                },
                ClaimResource {
                    id: "openclaw_plugin".to_string(),
                    purpose: "openclaw_plugin".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: "openclaw".to_string(),
                        plugin_id: "tokenless".to_string(),
                    },
                },
            ],
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "openclaw_state_dir".to_string(),
                plugin_resource: "openclaw_plugin".to_string(),
                skill_resources: Vec::new(),
                config_resources: Vec::new(),

                displaced_plugins: Vec::new(),
            }),
        }
    }

    /// The receipt must round-trip through TOML losslessly. This is the
    /// pin against the `toml` 0.8 enum-serialization footgun: if a future
    /// edit reaches for `#[serde(flatten)]` or an internally-tagged enum,
    /// this test fails.
    #[test]
    fn adapter_claim_toml_round_trip() {
        // Wrap in a table so the array-of-tables nesting matches how the
        // claim is stored inside `InstalledState`.
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }
        let wrapper = Wrapper {
            adapter_claims: vec![sample_claim()],
        };
        let text = toml::to_string_pretty(&wrapper).expect("serialize to TOML");
        let parsed: Wrapper = toml::from_str(&text).expect("parse from TOML");
        assert_eq!(wrapper, parsed, "round-trip mismatch; TOML was:\n{text}");
    }

    #[test]
    fn adapter_claim_json_round_trip() {
        let claim = sample_claim();
        let json = serde_json::to_string(&claim).expect("serialize JSON");
        let parsed: AdapterClaim = serde_json::from_str(&json).expect("parse JSON");
        assert_eq!(claim, parsed);
    }

    #[test]
    fn managed_integrity_schema_round_trips_through_toml() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }

        let file = ManagedSourceFile {
            relative_path: PathBuf::from("SKILL.md"),
            kind: ManagedFileKind::File,
            sha256: Some("a".repeat(64)),
            symlink_target: None,
        };
        let mut claim = sample_claim();
        claim.source_revision = Some(AdapterSourceRevision {
            source_root: claim.resource_root.clone(),
            files: vec![ManagedSourceFile {
                relative_path: PathBuf::from("plugin.json"),
                ..file.clone()
            }],
            materialized_sources: vec![MaterializedSourceRevision {
                resource_id: "openclaw_state_dir".into(),
                source_root: PathBuf::from("/usr/local/share/anolisa/skills/sec-audit"),
                files: vec![file.clone()],
            }],
        });
        claim.materialized_files = vec![MaterializedFile {
            resource_id: "openclaw_state_dir".into(),
            relative_path: file.relative_path,
            kind: file.kind,
            sha256: file.sha256,
            symlink_target: file.symlink_target,
        }];
        let wrapper = Wrapper {
            adapter_claims: vec![claim],
        };
        let text = toml::to_string_pretty(&wrapper).expect("serialize integrity receipt");
        let parsed: Wrapper = toml::from_str(&text).expect("parse integrity receipt");
        assert_eq!(wrapper, parsed, "round-trip mismatch; TOML was:\n{text}");
    }

    #[test]
    fn legacy_bundle_digest_round_trips_but_new_claim_omits_it() {
        let new_claim = serde_json::to_value(sample_claim()).expect("new claim JSON");
        assert!(new_claim.get("bundle_digest").is_none());

        let mut value = serde_json::to_value(sample_claim()).expect("claim JSON value");
        value.as_object_mut().expect("claim object").insert(
            "bundle_digest".to_string(),
            serde_json::Value::String("sha256:legacy".to_string()),
        );
        let parsed: AdapterClaim = serde_json::from_value(value).expect("legacy claim");
        assert_eq!(parsed.bundle_digest.as_deref(), Some("sha256:legacy"));
        let written = serde_json::to_value(parsed).expect("legacy claim JSON");
        assert_eq!(
            written
                .get("bundle_digest")
                .and_then(serde_json::Value::as_str),
            Some("sha256:legacy")
        );
    }

    #[test]
    fn integrity_records_require_normalized_digest_and_nonempty_symlink_target() {
        let mut claim = sample_claim();
        claim.source_revision = Some(AdapterSourceRevision {
            source_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/openclaw"),
            files: vec![ManagedSourceFile {
                relative_path: PathBuf::from("plugin.json"),
                kind: ManagedFileKind::File,
                sha256: Some("A".repeat(64)),
                symlink_target: None,
            }],
            materialized_sources: Vec::new(),
        });
        assert!(validate_integrity_entries(&claim).is_err());

        claim.source_revision = Some(AdapterSourceRevision {
            source_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/openclaw"),
            files: vec![ManagedSourceFile {
                relative_path: PathBuf::from("plugin-link"),
                kind: ManagedFileKind::Symlink,
                sha256: None,
                symlink_target: Some(PathBuf::new()),
            }],
            materialized_sources: Vec::new(),
        });
        assert!(validate_integrity_entries(&claim).is_err());
    }

    #[test]
    fn validate_plugin_id_accepts_safe_ids() {
        validate_plugin_id("tokenless").expect("plain");
        validate_plugin_id("ws-ckpt").expect("dash");
        validate_plugin_id("a.b_c-1").expect("mixed");
    }

    #[test]
    fn validate_dsh_package_name_accepts_scoped_name() {
        validate_dsh_package_name("@anolisa/dsh-tokenless").expect("scoped package");
        validate_dsh_package_name("dsh-tokenless").expect("unscoped package");
    }

    #[test]
    fn validate_dsh_package_name_rejects_traversal_and_shell_text() {
        for value in [
            "",
            "@anolisa",
            "@anolisa/",
            "@anolisa/a/b",
            "../escape",
            "a b",
        ] {
            assert!(validate_dsh_package_name(value).is_err(), "{value}");
        }
    }

    #[test]
    fn validate_plugin_id_rejects_unsafe_ids() {
        assert!(validate_plugin_id("").is_err(), "empty");
        assert!(validate_plugin_id("..").is_err(), "dotdot");
        assert!(validate_plugin_id("-rf").is_err(), "leading dash");
        assert!(validate_plugin_id("a/b").is_err(), "slash");
        assert!(validate_plugin_id("a b").is_err(), "space");
        assert!(validate_plugin_id("a;b").is_err(), "semicolon");
        assert!(validate_plugin_id("a$b").is_err(), "dollar");
    }

    #[test]
    fn validate_external_path_rejects_traversal() {
        let roots = vec![PathBuf::from("/home/alice/.openclaw")];
        let err = validate_external_path(Path::new("/home/alice/.openclaw/../.ssh"), &roots)
            .expect_err("must reject");
        assert!(matches!(err, ExternalPathError::Traversal { .. }));
    }

    #[test]
    fn validate_external_path_rejects_outside_root() {
        let roots = vec![PathBuf::from("/home/alice/.openclaw")];
        let err =
            validate_external_path(Path::new("/etc/passwd"), &roots).expect_err("must reject");
        assert!(matches!(err, ExternalPathError::OutsideAllowedRoots { .. }));
    }

    #[test]
    fn validate_external_path_accepts_under_root() {
        let roots = vec![PathBuf::from("/home/alice/.openclaw")];
        validate_external_path(
            Path::new("/home/alice/.openclaw/extensions/tokenless"),
            &roots,
        )
        .expect("under root must pass");
    }

    /// A forged receipt pointing an "external" path at `/etc` must be
    /// rejected by the full claim validation, using the driver's allowed
    /// roots — not any root the receipt names for itself.
    #[test]
    fn forged_external_path_rejected_by_claim_validate() {
        let layout = FsLayout::system(None);
        let allowed = vec![PathBuf::from("/home/alice/.openclaw")];
        let mut claim = sample_claim();
        claim.resources[0].kind = ClaimResourceKind::ExternalPath {
            path: PathBuf::from("/etc/cron.d/evil"),
        };
        let err = claim.validate(&layout, &allowed).expect_err("must reject");
        assert!(matches!(err, ClaimValidationError::ExternalPath { .. }));
    }

    fn sample_hermes_claim() -> AdapterClaim {
        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "agent-sec".to_string(),
            framework: "hermes".to_string(),
            plugin_id: Some("agent-sec".to_string()),
            adapter_type: None,
            enabled_at: "2026-06-22T10:30:45Z".to_string(),
            resource_root: PathBuf::from("/usr/local/share/anolisa/adapters/agent-sec/hermes"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "hermes_home".to_string(),
                    purpose: "hermes_home".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.hermes"),
                    },
                },
                ClaimResource {
                    id: "hermes_plugin".to_string(),
                    purpose: "hermes_plugin_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.hermes/plugins/agent-sec"),
                    },
                },
            ],
            driver_payload: DriverPayload::Hermes(HermesClaim {
                home_resource: "hermes_home".to_string(),
                plugin_resource: "hermes_plugin".to_string(),
                skill_resources: Vec::new(),
            }),
        }
    }

    #[test]
    fn hermes_claim_toml_round_trip() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }
        let wrapper = Wrapper {
            adapter_claims: vec![sample_hermes_claim()],
        };
        let text = toml::to_string_pretty(&wrapper).expect("serialize Hermes to TOML");
        let parsed: Wrapper = toml::from_str(&text).expect("parse Hermes from TOML");
        assert_eq!(wrapper, parsed, "Hermes round-trip mismatch; TOML:\n{text}");
    }

    #[test]
    fn hermes_claim_json_round_trip() {
        let claim = sample_hermes_claim();
        let json = serde_json::to_string(&claim).expect("serialize Hermes JSON");
        let parsed: AdapterClaim = serde_json::from_str(&json).expect("parse Hermes JSON");
        assert_eq!(claim, parsed);
    }

    fn sample_qwenpaw_claim() -> AdapterClaim {
        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "qwenpaw".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-09-04T10:30:45Z".to_string(),
            resource_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/qwenpaw"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "qwenpaw_home".to_string(),
                    purpose: "qwenpaw_home".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.qwenpaw"),
                    },
                },
                ClaimResource {
                    id: "qwenpaw_plugin".to_string(),
                    purpose: "qwenpaw_plugin_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.qwenpaw/plugins/tokenless"),
                    },
                },
            ],
            driver_payload: DriverPayload::QwenPaw(QwenPawClaim {
                home_resource: "qwenpaw_home".to_string(),
                plugin_resource: "qwenpaw_plugin".to_string(),
            }),
        }
    }

    #[test]
    fn qwenpaw_claim_round_trips_and_validates() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }
        let claim = sample_qwenpaw_claim();
        let wrapper = Wrapper {
            adapter_claims: vec![claim.clone()],
        };
        let text = toml::to_string_pretty(&wrapper).expect("serialize QwenPaw to TOML");
        let parsed: Wrapper = toml::from_str(&text).expect("parse QwenPaw from TOML");
        assert_eq!(
            wrapper, parsed,
            "QwenPaw round-trip mismatch; TOML:\n{text}"
        );
        let json = serde_json::to_string(&claim).expect("serialize QwenPaw JSON");
        let parsed: AdapterClaim = serde_json::from_str(&json).expect("parse QwenPaw JSON");
        assert_eq!(claim, parsed);
        let layout = FsLayout::system(None);
        claim
            .validate(&layout, &[PathBuf::from("/home/alice/.qwenpaw")])
            .expect("QwenPaw claim validates under its working directory");
    }

    #[test]
    fn framework_config_resource_validates() {
        let layout = FsLayout::system(None);
        let allowed = vec![PathBuf::from("/home/alice/.openclaw")];
        let resource = ClaimResource {
            id: "config_touch".to_string(),
            purpose: "openclaw_config".to_string(),
            kind: ClaimResourceKind::FrameworkConfig {
                framework: "openclaw".to_string(),
                key: "plugins.entries.sec.enabled".to_string(),
                state: ConfigApplyState::Applied,
            },
        };
        resource
            .validate(&layout, &allowed)
            .expect("config resource should pass");
    }

    #[test]
    fn framework_config_state_is_backward_compatible_and_round_trips_pending() {
        let applied = ClaimResource {
            id: "config_applied".to_string(),
            purpose: "openclaw_config".to_string(),
            kind: ClaimResourceKind::FrameworkConfig {
                framework: "openclaw".to_string(),
                key: "applied.key".to_string(),
                state: ConfigApplyState::Applied,
            },
        };
        let applied_json = serde_json::to_string(&applied).expect("serialize applied");
        assert!(
            !applied_json.contains("\"state\""),
            "default applied state must keep the existing wire shape"
        );
        let parsed: ClaimResource =
            serde_json::from_str(&applied_json).expect("parse implicit applied");
        assert_eq!(parsed, applied);

        let pending = ClaimResource {
            id: "config_pending".to_string(),
            purpose: "openclaw_config".to_string(),
            kind: ClaimResourceKind::FrameworkConfig {
                framework: "openclaw".to_string(),
                key: "pending.key".to_string(),
                state: ConfigApplyState::Pending,
            },
        };
        let pending_json = serde_json::to_string(&pending).expect("serialize pending");
        assert!(pending_json.contains("\"state\":\"pending\""));
        let parsed: ClaimResource =
            serde_json::from_str(&pending_json).expect("parse explicit pending");
        assert_eq!(parsed, pending);
    }

    #[test]
    fn openclaw_claim_with_skills_and_config_round_trips() {
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "sec-core".to_string(),
            framework: "openclaw".to_string(),
            plugin_id: Some("sec-core".to_string()),
            adapter_type: None,
            enabled_at: "2026-06-22T12:00:00Z".to_string(),
            resource_root: PathBuf::from("/data/adapters/sec-core/openclaw"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "state_dir".to_string(),
                    purpose: "openclaw_state_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.openclaw"),
                    },
                },
                ClaimResource {
                    id: "plugin".to_string(),
                    purpose: "openclaw_plugin".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: "openclaw".to_string(),
                        plugin_id: "sec-core".to_string(),
                    },
                },
                ClaimResource {
                    id: "skill_sec_audit".to_string(),
                    purpose: "openclaw_skill".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.openclaw/skills/sec-audit"),
                    },
                },
                ClaimResource {
                    id: "config_enabled".to_string(),
                    purpose: "openclaw_config".to_string(),
                    kind: ClaimResourceKind::FrameworkConfig {
                        framework: "openclaw".to_string(),
                        key: "plugins.entries.sec-core.enabled".to_string(),
                        state: ConfigApplyState::Applied,
                    },
                },
            ],
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "state_dir".to_string(),
                plugin_resource: "plugin".to_string(),
                skill_resources: vec!["skill_sec_audit".to_string()],
                config_resources: vec!["config_enabled".to_string()],

                displaced_plugins: Vec::new(),
            }),
        };
        let json = serde_json::to_string(&claim).expect("serialize");
        let parsed: AdapterClaim = serde_json::from_str(&json).expect("parse");
        assert_eq!(claim, parsed);
    }

    #[test]
    fn validate_skill_name_accepts_safe_names() {
        validate_skill_name("sec-audit").expect("dash");
        validate_skill_name("cred_scan").expect("underscore");
        validate_skill_name("skill.v2").expect("dot");
        validate_skill_name("a1").expect("short");
    }

    #[test]
    fn validate_skill_name_rejects_unsafe_names() {
        assert!(validate_skill_name("").is_err(), "empty");
        assert!(validate_skill_name("..").is_err(), "dotdot");
        assert!(validate_skill_name(".").is_err(), "dot");
        assert!(validate_skill_name("-rf").is_err(), "leading dash");
        assert!(validate_skill_name("a/b").is_err(), "slash");
        assert!(validate_skill_name("a b").is_err(), "space");
        assert!(validate_skill_name("../x").is_err(), "traversal");
    }

    #[test]
    fn validate_config_key_accepts_safe_keys() {
        validate_config_key("plugins.entries.sec.enabled").expect("dotted path");
        validate_config_key("foo.bar_baz-1").expect("mixed");
    }

    #[test]
    fn validate_config_key_rejects_unsafe_keys() {
        assert!(validate_config_key("").is_err(), "empty");
        assert!(validate_config_key("a;b").is_err(), "semicolon");
        assert!(validate_config_key("a$b").is_err(), "dollar");
        assert!(validate_config_key("a`b").is_err(), "backtick");
        assert!(validate_config_key("a b").is_err(), "space");
        assert!(validate_config_key("a|b").is_err(), "pipe");
    }

    fn sample_codex_claim() -> AdapterClaim {
        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "codex".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-07-04T10:30:45Z".to_string(),
            resource_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/codex"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "codex_marketplace_dir".to_string(),
                    purpose: "codex_marketplace_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.local/share/anolisa/codex-marketplace"),
                    },
                },
                ClaimResource {
                    id: "codex_symlink".to_string(),
                    purpose: "codex_plugin_symlink".to_string(),
                    kind: ClaimResourceKind::Symlink {
                        link: PathBuf::from(
                            "/home/alice/.local/share/anolisa/codex-marketplace/tokenless",
                        ),
                        target: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/codex"),
                    },
                },
                ClaimResource {
                    id: "codex_marketplace".to_string(),
                    purpose: "codex_marketplace".to_string(),
                    kind: ClaimResourceKind::FrameworkMarketplace {
                        framework: "codex".to_string(),
                        marketplace: "anolisa-tokenless".to_string(),
                    },
                },
                ClaimResource {
                    id: "codex_plugin".to_string(),
                    purpose: "codex_plugin".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: "codex".to_string(),
                        plugin_id: "tokenless".to_string(),
                    },
                },
            ],
            driver_payload: DriverPayload::Codex(CodexClaim {
                marketplace_dir_resource: "codex_marketplace_dir".to_string(),
                symlink_resource: "codex_symlink".to_string(),
                marketplace_resource: "codex_marketplace".to_string(),
                plugin_resource: "codex_plugin".to_string(),
            }),
        }
    }

    #[test]
    fn codex_claim_toml_and_json_round_trip() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }
        let wrapper = Wrapper {
            adapter_claims: vec![sample_codex_claim()],
        };
        let text = toml::to_string_pretty(&wrapper).expect("serialize Codex to TOML");
        let parsed: Wrapper = toml::from_str(&text).expect("parse Codex from TOML");
        assert_eq!(wrapper, parsed, "Codex round-trip mismatch; TOML:\n{text}");

        let claim = sample_codex_claim();
        let json = serde_json::to_string(&claim).expect("serialize Codex JSON");
        let back: AdapterClaim = serde_json::from_str(&json).expect("parse Codex JSON");
        assert_eq!(claim, back);
    }

    #[test]
    fn codex_claim_validates_under_allowed_roots() {
        let layout = FsLayout::system(None);
        let allowed = vec![
            PathBuf::from("/home/alice/.local/share/anolisa"),
            PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/codex"),
        ];
        sample_codex_claim()
            .validate(&layout, &allowed)
            .expect("codex claim under allowed roots must pass");
    }

    #[test]
    fn cosh_claim_round_trips_and_validates() {
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "cosh".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: Some("extension".to_string()),
            enabled_at: "2026-07-04T10:30:45Z".to_string(),
            resource_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/common"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![ClaimResource {
                id: "cosh_extension_dir".to_string(),
                purpose: "cosh_extension_dir".to_string(),
                kind: ClaimResourceKind::ExternalPath {
                    path: PathBuf::from("/home/alice/.copilot-shell/extensions/tokenless"),
                },
            }],
            driver_payload: DriverPayload::Cosh(CoshClaim {
                extension_dir_resource: "cosh_extension_dir".to_string(),
            }),
        };
        let json = serde_json::to_string(&claim).expect("serialize Cosh JSON");
        let back: AdapterClaim = serde_json::from_str(&json).expect("parse Cosh JSON");
        assert_eq!(claim, back);

        let layout = FsLayout::system(None);
        let allowed = vec![PathBuf::from("/home/alice/.copilot-shell")];
        claim
            .validate(&layout, &allowed)
            .expect("cosh claim under allowed roots must pass");
    }

    #[test]
    fn claude_code_claim_round_trips() {
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "claude-code".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-07-04T10:30:45Z".to_string(),
            resource_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/claude-code"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "cc_marketplace".to_string(),
                    purpose: "claude_code_marketplace".to_string(),
                    kind: ClaimResourceKind::FrameworkMarketplace {
                        framework: "claude-code".to_string(),
                        marketplace: "anolisa".to_string(),
                    },
                },
                ClaimResource {
                    id: "cc_plugin".to_string(),
                    purpose: "claude_code_plugin".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: "claude-code".to_string(),
                        plugin_id: "tokenless".to_string(),
                    },
                },
            ],
            driver_payload: DriverPayload::ClaudeCode(ClaudeCodeClaim {
                marketplace_resource: "cc_marketplace".to_string(),
                plugin_resource: "cc_plugin".to_string(),
            }),
        };
        let json = serde_json::to_string(&claim).expect("serialize Claude Code JSON");
        let back: AdapterClaim = serde_json::from_str(&json).expect("parse Claude Code JSON");
        assert_eq!(claim, back);
    }

    fn sample_qoder_claim() -> AdapterClaim {
        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "qoder".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-07-08T10:30:45Z".to_string(),
            resource_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/qoder"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: 1,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "qoder_plugin".to_string(),
                    purpose: "qoder_plugin".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: "qoder".to_string(),
                        plugin_id: "tokenless".to_string(),
                    },
                },
                ClaimResource {
                    id: "qoder_settings".to_string(),
                    purpose: "qoder_settings".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.qoder/settings.json"),
                    },
                },
            ],
            driver_payload: DriverPayload::Qoder(QoderClaim {
                plugin_resource: "qoder_plugin".to_string(),
                settings_resource: Some("qoder_settings".to_string()),
                plugin_preexisting: false,
                plugin_install_confirmed: false,
                managed_hooks: vec!["tokenless-rewrite".to_string()],
                managed_hook_specs: vec![QoderManagedHook {
                    event: "PreToolUse".to_string(),
                    entry: serde_json::json!({
                        "hooks": [{
                            "type": "command",
                            "name": "tokenless-rewrite",
                            "command": "python3 /opt/anolisa/rewrite.py"
                        }]
                    }),
                }],
            }),
        }
    }

    #[test]
    fn qoder_claim_toml_and_json_round_trip() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }
        let wrapper = Wrapper {
            adapter_claims: vec![sample_qoder_claim()],
        };
        let text = toml::to_string_pretty(&wrapper).expect("serialize Qoder to TOML");
        assert!(
            text.contains("settings_resource = \"qoder_settings\""),
            "legacy receipt keeps its string settings reference: {text}"
        );
        let parsed: Wrapper = toml::from_str(&text).expect("parse Qoder from TOML");
        assert_eq!(wrapper, parsed, "Qoder round-trip mismatch; TOML:\n{text}");

        let claim = sample_qoder_claim();
        let json = serde_json::to_string(&claim).expect("serialize Qoder JSON");
        let back: AdapterClaim = serde_json::from_str(&json).expect("parse Qoder JSON");
        assert_eq!(claim, back);
    }

    #[test]
    fn native_qoder_claim_omits_settings_resource() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }

        let mut claim = sample_qoder_claim();
        claim
            .resources
            .retain(|resource| resource.id == "qoder_plugin");
        let DriverPayload::Qoder(payload) = &mut claim.driver_payload else {
            unreachable!("sample is a Qoder claim")
        };
        payload.settings_resource = None;
        payload.plugin_preexisting = false;
        payload.plugin_install_confirmed = true;
        payload.managed_hooks.clear();
        payload.managed_hook_specs.clear();
        claim.driver_schema = DRIVER_SCHEMA_VERSION;
        let wrapper = Wrapper {
            adapter_claims: vec![claim],
        };

        let text = toml::to_string_pretty(&wrapper).expect("serialize native Qoder receipt");
        assert!(!text.contains("settings_resource"), "native TOML: {text}");
        assert!(
            text.contains("plugin_install_confirmed = true"),
            "native TOML: {text}"
        );
        assert_eq!(wrapper.adapter_claims[0].driver_schema, 4);
        let parsed: Wrapper = toml::from_str(&text).expect("parse native Qoder receipt");
        assert_eq!(wrapper, parsed);
    }

    #[test]
    fn native_qoder_v2_receipt_defaults_install_confirmation_to_false() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }

        let mut claim = sample_qoder_claim();
        claim
            .resources
            .retain(|resource| resource.id == "qoder_plugin");
        let DriverPayload::Qoder(payload) = &mut claim.driver_payload else {
            unreachable!("sample is a Qoder claim")
        };
        payload.settings_resource = None;
        payload.plugin_preexisting = false;
        payload.plugin_install_confirmed = false;
        payload.managed_hooks.clear();
        payload.managed_hook_specs.clear();
        claim.driver_schema = 2;
        let text = toml::to_string_pretty(&Wrapper {
            adapter_claims: vec![claim],
        })
        .expect("serialize v2 Native Qoder receipt");
        assert!(
            !text.contains("plugin_install_confirmed"),
            "v2 TOML: {text}"
        );

        let parsed: Wrapper = toml::from_str(&text).expect("parse v2 Native Qoder receipt");
        let DriverPayload::Qoder(payload) = &parsed.adapter_claims[0].driver_payload else {
            unreachable!("fixture is a Qoder claim")
        };
        assert!(!payload.plugin_install_confirmed);
    }

    #[test]
    fn qoder_claim_validates_under_allowed_roots() {
        let layout = FsLayout::system(None);
        let allowed = vec![PathBuf::from("/home/alice/.qoder")];
        sample_qoder_claim()
            .validate(&layout, &allowed)
            .expect("qoder claim under allowed roots must pass");
    }

    #[test]
    fn qwencode_claim_round_trips_and_validates() {
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "qwencode".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: Some("extension".to_string()),
            enabled_at: "2026-07-16T10:30:45Z".to_string(),
            resource_root: PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/qwencode"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "qwencode_extension_dir".to_string(),
                    purpose: "qwencode_extension_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/alice/.qwen/extensions/tokenless"),
                    },
                },
                ClaimResource {
                    id: "qwencode_plugin".to_string(),
                    purpose: "qwencode_plugin".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: "qwencode".to_string(),
                        plugin_id: "tokenless".to_string(),
                    },
                },
            ],
            driver_payload: DriverPayload::QwenCode(QwenCodeClaim {
                extension_dir_resource: "qwencode_extension_dir".to_string(),
                plugin_resource: "qwencode_plugin".to_string(),
            }),
        };

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            adapter_claims: Vec<AdapterClaim>,
        }
        let wrapper = Wrapper {
            adapter_claims: vec![claim.clone()],
        };
        let text = toml::to_string_pretty(&wrapper).expect("serialize Qwen Code to TOML");
        let parsed: Wrapper = toml::from_str(&text).expect("parse Qwen Code from TOML");
        assert_eq!(
            wrapper, parsed,
            "Qwen Code round-trip mismatch; TOML:\n{text}"
        );

        let json = serde_json::to_string(&claim).expect("serialize Qwen Code JSON");
        let back: AdapterClaim = serde_json::from_str(&json).expect("parse Qwen Code JSON");
        assert_eq!(claim, back);

        claim
            .validate(
                &FsLayout::system(None),
                &[PathBuf::from("/home/alice/.qwen")],
            )
            .expect("Qwen Code claim under allowed root must pass");
    }

    /// A forged qoder receipt pointing its settings resource at `~/.ssh` or
    /// `/etc` must be rejected: the settings path is an external resource
    /// validated against the driver's allowed roots, not a root the receipt
    /// names for itself.
    #[test]
    fn qoder_forged_settings_path_rejected() {
        let layout = FsLayout::system(None);
        let allowed = vec![PathBuf::from("/home/alice/.qoder")];
        for forged in ["/home/alice/.ssh/authorized_keys", "/etc/cron.d/evil"] {
            let mut claim = sample_qoder_claim();
            for res in &mut claim.resources {
                if res.id == "qoder_settings" {
                    res.kind = ClaimResourceKind::ExternalPath {
                        path: PathBuf::from(forged),
                    };
                }
            }
            let err = claim
                .validate(&layout, &allowed)
                .expect_err("forged settings path must be rejected");
            assert!(
                matches!(err, ClaimValidationError::ExternalPath { .. }),
                "got {err:?} for {forged}"
            );
        }
    }

    #[test]
    fn forged_symlink_target_outside_roots_rejected() {
        let layout = FsLayout::system(None);
        let allowed = vec![
            PathBuf::from("/home/alice/.local/share/anolisa"),
            PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/codex"),
        ];
        let mut claim = sample_codex_claim();
        // Repoint the symlink target at /etc — outside every owned root.
        for res in &mut claim.resources {
            if let ClaimResourceKind::Symlink { target, .. } = &mut res.kind {
                *target = PathBuf::from("/etc/cron.d/evil");
            }
        }
        let err = claim.validate(&layout, &allowed).expect_err("must reject");
        // The target is validated as an ANOLISA-owned path, so a non-owned
        // target is an OwnedPath boundary violation.
        assert!(
            matches!(err, ClaimValidationError::OwnedPath { .. }),
            "got {err:?}"
        );
    }

    /// The anchor-persistence criterion: only a symlink *target* outside
    /// the static owned boundary requires external trust. Receipts whose
    /// targets validate as owned — or without symlink resources at all
    /// (every driver but Codex) — never do, so the Manager persists no
    /// anchor for them.
    #[test]
    fn requires_external_symlink_trust_only_for_out_of_boundary_targets() {
        let layout = FsLayout::system(None);
        // The sample target lives under the primary layout's owned roots.
        assert!(!sample_codex_claim().requires_external_symlink_trust(&layout, &[]));

        let mut claim = sample_codex_claim();
        for res in &mut claim.resources {
            if let ClaimResourceKind::Symlink { target, .. } = &mut res.kind {
                *target = PathBuf::from("/opt/vendor/plugin");
            }
        }
        assert!(claim.requires_external_symlink_trust(&layout, &[]));
        // A Manager-trusted extra owned root covering the target lifts it.
        assert!(!claim.requires_external_symlink_trust(&layout, &[PathBuf::from("/opt/vendor")]));

        // No symlink resources left: external trust is never required.
        claim
            .resources
            .retain(|r| !matches!(r.kind, ClaimResourceKind::Symlink { .. }));
        assert!(!claim.requires_external_symlink_trust(&layout, &[]));
    }

    /// A forged receipt cannot self-authorize a symlink target by also
    /// forging its own `resource_root`: the target is validated against the
    /// trusted layout, not against anything the receipt names.
    #[test]
    fn forged_symlink_target_not_authorized_by_forged_resource_root() {
        let layout = FsLayout::system(None);
        let allowed = vec![PathBuf::from("/home/alice/.local/share/anolisa")];
        let mut claim = sample_codex_claim();
        claim.resource_root = PathBuf::from("/etc");
        for res in &mut claim.resources {
            if let ClaimResourceKind::Symlink { target, .. } = &mut res.kind {
                *target = PathBuf::from("/etc/cron.d/evil");
            }
        }
        let err = claim.validate(&layout, &allowed).expect_err("must reject");
        assert!(
            matches!(err, ClaimValidationError::OwnedPath { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn marketplace_framework_mismatch_rejected() {
        let layout = FsLayout::system(None);
        let allowed = vec![
            PathBuf::from("/home/alice/.local/share/anolisa"),
            PathBuf::from("/usr/local/share/anolisa/adapters/tokenless/codex"),
        ];
        let mut claim = sample_codex_claim();
        for res in &mut claim.resources {
            if let ClaimResourceKind::FrameworkMarketplace { framework, .. } = &mut res.kind {
                *framework = "claude-code".to_string();
            }
        }
        let err = claim.validate(&layout, &allowed).expect_err("must reject");
        assert!(
            matches!(err, ClaimValidationError::FrameworkMismatch { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn validate_marketplace_name_rejects_unsafe() {
        validate_marketplace_name("anolisa").expect("plain");
        validate_marketplace_name("anolisa-tokenless").expect("dash");
        assert!(validate_marketplace_name("").is_err(), "empty");
        assert!(validate_marketplace_name("a b").is_err(), "space");
        assert!(validate_marketplace_name("a@b").is_err(), "at-sign");
        assert!(validate_marketplace_name("-x").is_err(), "leading dash");
    }

    #[test]
    fn framework_mismatch_rejected_by_claim_validate() {
        let layout = FsLayout::system(None);
        let allowed = vec![PathBuf::from("/home/alice/.openclaw")];
        let mut claim = sample_claim();
        claim.resources.push(ClaimResource {
            id: "wrong_framework".to_string(),
            purpose: "test".to_string(),
            kind: ClaimResourceKind::FrameworkPlugin {
                framework: "hermes".to_string(),
                plugin_id: "tokenless".to_string(),
            },
        });
        let err = claim.validate(&layout, &allowed).expect_err("must reject");
        assert!(
            matches!(err, ClaimValidationError::FrameworkMismatch { .. }),
            "got {err:?}"
        );
    }

    /// [`DisplacedPluginRef::applied`] is defaulted in both directions, and both
    /// halves carry weight: an entry written before the field existed must read as
    /// applied, or an upgrade would silently drop ownership a host is relying on;
    /// and an applied entry must serialize *without* the field, or every receipt on
    /// disk would change shape for a value that means "as before".
    #[test]
    fn displaced_plugin_ref_defaults_to_applied_and_omits_it_when_true() {
        let legacy: DisplacedPluginRef =
            serde_json::from_str(r#"{"resource":"openclaw_displaced_plugin_memory-core"}"#)
                .expect("a receipt written before `applied` existed still parses");
        assert!(
            legacy.applied,
            "absent must read as applied: a stranded host costs more than one \
             unwanted restore"
        );
        assert_eq!(legacy.slot, None);

        let applied = DisplacedPluginRef {
            resource: "openclaw_displaced_plugin_memory-core".to_string(),
            slot: Some("memory".to_string()),
            applied: true,
        };
        let json = serde_json::to_string(&applied).expect("serialize applied");
        assert!(
            !json.contains("applied"),
            "an applied entry must serialize exactly as it did before the field: {json}"
        );
        let back: DisplacedPluginRef = serde_json::from_str(&json).expect("parse applied");
        assert_eq!(applied, back);

        let pending = DisplacedPluginRef {
            applied: false,
            ..applied.clone()
        };
        let json = serde_json::to_string(&pending).expect("serialize pending");
        assert!(
            json.contains("\"applied\":false"),
            "an unapplied entry is the only one that has to say so: {json}"
        );
        let back: DisplacedPluginRef = serde_json::from_str(&json).expect("parse pending");
        assert!(!back.applied);

        // Receipts are TOML on disk, so the same defaults have to hold there.
        let toml_text = toml::to_string(&applied).expect("serialize applied TOML");
        assert!(
            !toml_text.contains("applied"),
            "TOML must omit it too: {toml_text}"
        );
        let back: DisplacedPluginRef = toml::from_str(&toml_text).expect("parse applied TOML");
        assert!(back.applied);
        let legacy_toml: DisplacedPluginRef =
            toml::from_str("resource = \"openclaw_displaced_plugin_memory_core\"\n")
                .expect("a version-4 receipt without the field still parses from TOML");
        assert!(legacy_toml.applied);
    }
}
