//! Manifest v2 schema.
//!
//! This module hosts the canonical typed representation of the TOML manifests
//! shipped under `src/anolisa/manifests/`. The single top-level shape is
//! `ComponentManifest` — a concrete component (runtime or osbase substrate).
//!
//! All deserialization is *tolerant*: missing optional fields default and we
//! accept both the new canonical TOML layout (per `templates/*.toml`) and the
//! current bundled fixture layout. Unknown keys are silently ignored so that
//! schema growth in either direction does not break existing artifacts.

use crate::distribution::ArtifactType;
use crate::health::CheckSpec;
use crate::hooks::HookPhase;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Default schema version applied when the TOML omits it.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Canonical `render` value for layout-placeholder content rendering
/// (see [`InstallFileSpec::render`]). Single source of the wire spelling
/// so contract authors, the install resolver, and docs cannot drift.
pub const RENDER_ANOLISA_PATHS_V1: &str = "anolisa-paths-v1";

// ---------------------------------------------------------------------------
// ComponentManifest
// ---------------------------------------------------------------------------

/// Canonical runtime / osbase component manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "ComponentManifestRaw")]
pub struct ComponentManifest {
    /// Schema version after tolerant parsing.
    pub schema_version: u32,
    /// Component identity and layer metadata.
    pub component: ComponentMeta,
    /// `[component.contract]` schema/version envelope (minimal schema).
    pub contract: ContractSpec,
    /// `[component.artifact]` artifact shape description (minimal schema).
    pub artifact: ArtifactSpec,
    /// Source tree or release source declaration.
    pub source: SourceSpec,
    /// Structured selector list. Each entry says "for this install_mode + OS
    /// family + arch (+optional libc/pkg_base), prefer these artifact types
    /// in order". The resolver feeds `preferred_artifact_types` into
    /// `ResolveQuery` as the tiebreaker.
    pub distribution_selectors: Vec<DistributionSelector>,
    /// Build backend declaration for source builds.
    pub build: BuildSpec,
    /// Files, services, and capabilities installed by this component.
    pub install: InstallSpec,
    /// `[backends]` — backend-specific packaging metadata. Empty for components
    /// that only ship raw artifacts; the RPM backend reads
    /// [`ManifestBackends::rpm`] to resolve the package name during adopt.
    #[serde(default, skip_serializing_if = "ManifestBackends::is_empty")]
    pub backends: ManifestBackends,
    /// Component-level host requirements.
    pub env_requirements: EnvRequirements,
    /// Structured dependency lists. `build`, `runtime`, and `components`
    /// stay separate so downstream consumers can reason about each kind
    /// (e.g. resolver only follows `components`, doctor only checks
    /// `runtime`).
    pub dependencies: DependenciesSpec,
    /// Structured `[[component.dependencies]]` entries the raw backend
    /// preflights before laying files. Distinct from the flat
    /// [`dependencies`](Self::dependencies): each entry carries a
    /// [`DependencyKind`] and probe/remediation metadata so the resolver can
    /// fail fast on a missing runtime dependency instead of letting a service
    /// silently die after install. Empty for components with no declared
    /// runtime dependencies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_deps: Vec<RuntimeDependency>,
    /// Feature toggles exposed by this component manifest.
    pub features: Vec<FeatureSpec>,
    /// `[[adapters]]` declarations preserved verbatim. The component schema
    /// itself does not install these — the Adapter layer (scan + framework
    /// detect + safe install/remove) consumes them — but the manifest keeps
    /// every parsed field so that tooling need not re-read the TOML.
    pub adapters: Vec<AdapterSpec>,
    /// Minimal-schema `[component.health_check]`. `None` falls back to a
    /// synthesized `binary_version` over the first executable layout file —
    /// see [`ComponentManifest::health_spec`].
    pub health_check: Option<CheckSpec>,
}

/// Structured distribution selector, surfaced for downstream consumers
/// (resolver, planner, doctor).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DistributionSelector {
    /// Optional install-mode selector.
    #[serde(default)]
    pub install_mode: Option<String>,
    /// Accepted OS names.
    #[serde(default)]
    pub os: Vec<String>,
    /// Accepted CPU architectures.
    #[serde(default)]
    pub arch: Vec<String>,
    /// Optional libc selector.
    #[serde(default)]
    pub libc: Option<String>,
    /// Optional package-base selector such as `rpm` or `deb`.
    #[serde(default)]
    pub pkg_base: Option<String>,
    /// Artifact-type preference used only after target filtering.
    #[serde(default)]
    pub preferred_artifact_types: Vec<ArtifactType>,
}

/// `[backends]` — backend-specific packaging metadata.
///
/// Orthogonal to [`DistributionSelector::pkg_base`] (which says *which package
/// format a distribution uses*): this says *what the component is named under a
/// given backend*. Only populated where a backend needs an explicit name; an
/// absent table leaves package-name resolution to the lower mapping tiers
/// (repo.toml `package_map` / RPM provides / the bare component name).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestBackends {
    /// RPM backend packaging info; `None` falls through to the lower tiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<RpmBackendSpec>,
}

impl ManifestBackends {
    /// Whether no backend metadata is present (used by `skip_serializing_if` so
    /// raw-only manifests round-trip without an empty `[backends]` table).
    pub fn is_empty(&self) -> bool {
        self.rpm.is_none()
    }
}

/// `[backends.rpm]` — RPM-backend packaging info for a component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpmBackendSpec {
    /// RPM package name for this component (e.g. `copilot-shell`),
    /// the highest-precedence package-name source after a CLI `--package`
    /// override.
    pub package: String,
}

/// Component identity and placement metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentMeta {
    /// Stable component name.
    pub name: String,
    /// Component version expected by planners.
    pub version: String,
    /// Architecture layer label (`runtime`, `osbase`, ...).
    pub layer: String,
    /// Optional domain label for catalog grouping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Human-facing component name (minimal schema `display_name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Owning team/maintainer (minimal schema `owner`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// SPDX license identifier (minimal schema `license`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Upstream repository URL (minimal schema `repository`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Component names that cannot coexist with this component.
    ///
    /// The raw backend checks this list against the installed state before
    /// laying files. If any listed component is already installed, the
    /// install is refused with a diagnostic pointing at the conflict. The
    /// RPM backend ignores this field — rpm `Conflicts:` handles it natively.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
}

/// `[component.contract]` — schema/version compatibility envelope (minimal
/// schema). Both fields are optional during the tokenless-first migration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractSpec {
    /// Component-manifest schema version, e.g. `"1.0"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    /// Minimum ANOLISA CLI version that can install this component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_anolisa_version: Option<String>,
}

/// `[component.artifact]` — single-artifact shape description (minimal schema).
/// Complements [`DistributionSelector`], which selects among multiple targets.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactSpec {
    /// Artifact form: `binary` | `archive` | `script-only` | `mixed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    /// Archive format when `artifact_type = "archive"`, e.g. `"tar.gz"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_format: Option<String>,
    /// Filename template, e.g. `"{name}-{version}-{os}-{arch}.tar.gz"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub naming_pattern: Option<String>,
}

/// Source declaration for source-build capable components.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceSpec {
    /// Source kind (`git`, `path`, `archive`, ...).
    pub kind: String,
    /// Local source path, when the source is already staged.
    pub path: Option<String>,
    /// Remote source URL, when source must be fetched.
    pub url: Option<String>,
}

/// Build backend declaration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildSpec {
    /// Backend name such as `cargo`.
    pub backend: String,
    /// Expected output paths or target names.
    pub outputs: Vec<String>,
}

/// Install contract emitted by a component manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstallSpec {
    /// Supported install modes.
    pub modes: Vec<String>,
    /// Files copied or extracted during install.
    pub files: Vec<InstallFileSpec>,
    /// Service units managed by lifecycle operations (enable/start on
    /// install, stop/disable on uninstall).
    pub services: Vec<ServiceSpec>,
    /// Linux capability assignments requested for installed files.
    pub capabilities: Vec<InstallCapabilitySpec>,
    /// Lifecycle hook scripts run around install/uninstall phases.
    pub hooks: Vec<ManifestHookSpec>,
}

/// Role of an installed file (minimal-schema `type` key).
///
/// Drives default permissions (an [`FileKind::Executable`] without an explicit
/// mode installs 0755) and default health-check synthesis — the first
/// executable layout file becomes a `binary_version` probe when no
/// `[component.health_check]` is declared. Legacy `[install]` files carry no
/// `type` and default to [`FileKind::Data`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Opaque data file (default).
    #[default]
    Data,
    /// Executable binary.
    Executable,
    /// Configuration file (purge-scoped on uninstall).
    Config,
    /// Shared library.
    Library,
    /// Symbolic link created at install time. `source` is the link's
    /// referent (a layout-template path like `{libexecdir}/tokenless/rtk`,
    /// expanded at resolve time — NOT an archive member), `target`/`dest`
    /// is where the link is created. `mode` is ignored: symlink
    /// permissions are meaningless on Linux.
    Symlink,
}

/// One file mapping in an install contract.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallFileSpec {
    /// Source path inside an artifact or source tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Destination path after layout placeholder expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
    /// Unix file mode requested by the manifest, e.g. `"0755"` or `"0644"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// File role from the minimal-schema `type` key. Defaults to
    /// [`FileKind::Data`]; legacy `[install]` files have no `type`.
    #[serde(default, skip_serializing_if = "is_default_file_kind")]
    pub kind: FileKind,
    /// Content-rendering mode requested by the contract, e.g.
    /// [`RENDER_ANOLISA_PATHS_V1`]. Declares that the installer MUST
    /// substitute layout placeholders (`{bindir}`, `{datadir}`, …) inside
    /// the file *content* against the final [`FsLayout`] before placing it
    /// — relocatable systemd unit templates rely on this. The value is
    /// versioned so the vocabulary can evolve without ambiguity; installers
    /// reject values they do not implement. `None` copies bytes verbatim.
    ///
    /// [`FsLayout`]: anolisa_platform::fs_layout::FsLayout
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<String>,
}

/// Skip serializing the default [`FileKind`] so round-tripped legacy manifests
/// stay byte-stable.
fn is_default_file_kind(kind: &FileKind) -> bool {
    *kind == FileKind::Data
}

impl InstallFileSpec {
    /// Destination if present, otherwise legacy single-path source.
    pub fn install_path(&self) -> Option<&str> {
        self.dest.as_deref().or(self.source.as_deref())
    }

    /// Human-readable mapping for plans and warnings.
    pub fn display(&self) -> String {
        match (self.source.as_deref(), self.dest.as_deref()) {
            (Some(source), Some(dest)) => format!("{source} -> {dest}"),
            (Some(source), None) => source.to_string(),
            (None, Some(dest)) => dest.to_string(),
            (None, None) => "<empty>".to_string(),
        }
    }
}

/// Manager scope for a declared service unit.
///
/// `system` units live under `{unitdir}` and are driven by `systemctl`
/// (needs root); `user` units live under `{userunitdir}` and are driven by
/// `systemctl --user` (per-user, no root). System services are only
/// actionable in system install mode.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceScope {
    /// System-level unit (`systemctl`, needs root). Default.
    #[default]
    System,
    /// User-level unit (`systemctl --user`, per-user, no root).
    User,
}

impl ServiceScope {
    /// systemd manager namespace recorded as `ServiceRef.manager` and shown
    /// in diagnostics: `systemd` for system units, `systemd-user` for user
    /// units (driven by `systemctl --user`). Single source of truth so the
    /// persisted label can never disagree with a unit's scope.
    pub fn manager_label(self) -> &'static str {
        match self {
            Self::System => "systemd",
            Self::User => "systemd-user",
        }
    }
}

/// systemd unit lifecycle declaration from `[[component.services]]`.
///
/// Drives `enable`/`start` on install and `stop`/`disable` on uninstall.
/// The unit *file* is an ordinary layout entry; this spec references it by
/// name and declares how its lifecycle is managed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Native unit name, e.g. `agentsight.service`, or a template unit
    /// `anolisa-memory@.service` instantiated via [`instance`](Self::instance).
    pub unit: String,
    /// Manager scope. Defaults to [`ServiceScope::System`].
    #[serde(default)]
    pub scope: ServiceScope,
    /// `systemctl enable` on install for boot persistence. Defaults true.
    #[serde(default = "default_true")]
    pub enable: bool,
    /// Ensure the unit is active on install, restarting it when already
    /// running so upgrades pick up the new binary. Defaults true.
    #[serde(default = "default_true")]
    pub start: bool,
    /// Instance token for template (`@.service`) units — e.g. `%u` expands to
    /// the caller's username (`anolisa-memory@<user>`). `None` for plain units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

impl ServiceSpec {
    /// Build a spec from a legacy `[install] services = [...]` unit name:
    /// system scope, enable + start, no template instance.
    fn from_legacy_unit(unit: String) -> Self {
        Self {
            unit,
            scope: ServiceScope::System,
            enable: true,
            start: true,
            instance: None,
        }
    }

    /// Whether this declaration covers `unit`: an exact name match, or a
    /// template declaration (`foo@.service`) covering its instantiated
    /// units (`foo@alice.service`).
    pub fn covers_unit(&self, unit: &str) -> bool {
        if self.unit == unit {
            return true;
        }
        if let Some(prefix) = self.unit.strip_suffix("@.service") {
            return unit
                .strip_prefix(&format!("{prefix}@"))
                .map(|rest| rest.ends_with(".service") && rest.len() > ".service".len())
                .unwrap_or(false);
        }
        false
    }
}

/// Scope declared for `unit` by the `[[component.services]]` declarations;
/// a unit with no covering declaration probes as system scope, matching the
/// serde default on [`ServiceSpec::scope`].
pub fn declared_unit_scope(services: &[ServiceSpec], unit: &str) -> ServiceScope {
    services
        .iter()
        .find(|decl| decl.covers_unit(unit))
        .map(|decl| decl.scope)
        .unwrap_or_default()
}

/// Serde default for boolean fields whose absent value is `true`.
fn default_true() -> bool {
    true
}

/// Linux capability assignment for an installed file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallCapabilitySpec {
    /// Path receiving capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Capability names, e.g. `CAP_BPF`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caps: Vec<String>,
    /// Best-effort flag. When true, a `setcap` failure (no xattr support,
    /// non-root, unsupported filesystem) degrades to a warning instead of
    /// aborting the install. Defaults to false — a strict capability whose
    /// failure must abort. Skipped on serialize when false so legacy
    /// manifests round-trip byte-stable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

impl InstallCapabilitySpec {
    /// Human-readable capability assignment for plans and warnings.
    pub fn display(&self) -> String {
        match (self.path.as_deref(), self.caps.is_empty()) {
            (Some(path), false) => format!("{path}: {}", self.caps.join("+")),
            (Some(path), true) => path.to_string(),
            (None, false) => self.caps.join("+"),
            (None, true) => "<empty>".to_string(),
        }
    }
}

/// Lifecycle hook declaration from `[[component.hooks]]`.
///
/// Distinct from the executor input [`HookSpec`](crate::hooks::HookSpec):
/// this is the *contract-level declaration*. `script` is an unexpanded
/// layout-template path (e.g. `{datadir}/hooks/ws-ckpt/pre-uninstall.sh`),
/// not a resolved absolute path, and the owning component comes from the
/// surrounding manifest. The executor input is derived from this at install
/// time (placeholder expansion + path-safety validation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestHookSpec {
    /// Lifecycle phase that triggers the script.
    pub phase: HookPhase,
    /// Layout-template path to the script, shipped in the package payload as
    /// an ordinary layout file.
    pub script: String,
    /// Maximum wall-clock seconds before the script is killed. Defaults to 30.
    #[serde(default = "default_hook_timeout")]
    pub timeout_secs: u32,
    /// Failure handling. When false (default), a failure warns and the verb
    /// continues; when true, it aborts and the installer rolls back. ws-ckpt's
    /// pre-uninstall recover stays false to match the RPM `recover || warn`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}

/// Serde default for [`ManifestHookSpec::timeout_secs`] — matches the
/// executor's alpha 30-second default.
fn default_hook_timeout() -> u32 {
    30
}

/// Optional feature declared by a component manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSpec {
    /// Stable feature name.
    pub name: String,
    /// Human-readable summary.
    pub description: String,
    /// Whether this feature is enabled by default.
    pub default: bool,
}

/// Structured dependency lists, preserving the original `[dependencies]`
/// kind for each entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependenciesSpec {
    /// Build-time dependencies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build: Vec<String>,
    /// Runtime dependencies checked by doctor/status flows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime: Vec<String>,
    /// Component dependencies followed by planners.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

impl DependenciesSpec {
    /// True when every kind list is empty.
    pub fn is_empty(&self) -> bool {
        self.build.is_empty() && self.runtime.is_empty() && self.components.is_empty()
    }
}

/// Resolution bucket for a [`RuntimeDependency`]. The kind decides what the
/// preflight does when the dependency is missing — the strategy is dispatched
/// by bucket, never hard-coded per dependency name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyKind {
    /// A language interpreter/runtime (e.g. Node). Vendored into the
    /// component's isolated prefix; never touches the user's global install.
    LanguageRuntime,
    /// A system package installed by the host package manager (e.g.
    /// `btrfs-progs`). Shared infrastructure: the preflight reports the install
    /// command but never removes it on uninstall.
    SystemPackage,
    /// A kernel/platform capability that no package manager can install (e.g.
    /// in-kernel btrfs support, eBPF BTF / `CAP_BPF`). Check-only; a miss is
    /// fatal with a kernel/capability requirement.
    PlatformCapability,
}

impl DependencyKind {
    /// Wire/display spelling (kebab-case), matching the serde representation.
    /// Shared by the resolver and CLI so labels never drift from the schema.
    pub fn as_str(self) -> &'static str {
        match self {
            DependencyKind::LanguageRuntime => "language-runtime",
            DependencyKind::SystemPackage => "system-package",
            DependencyKind::PlatformCapability => "platform-capability",
        }
    }
}

/// Per-package-format names for a [`DependencyKind::SystemPackage`].
///
/// The same logical dependency is often packaged under different names across
/// distributions; this maps the package name by format (`rpm` → dnf, `deb` →
/// apt). An absent entry for the host's format falls back to the dependency's
/// [`name`](RuntimeDependency::name).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageNames {
    /// Package name on `rpm`-based hosts (dnf).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<String>,
    /// Package name on `deb`-based hosts (apt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deb: Option<String>,
}

impl PackageNames {
    /// Whether no package-name mapping is present (used by `skip_serializing_if`
    /// so dependencies without an explicit map round-trip cleanly).
    pub fn is_empty(&self) -> bool {
        self.rpm.is_none() && self.deb.is_none()
    }
}

/// One structured `[[component.dependencies]]` entry from the artifact
/// contract.
///
/// Drives the raw-backend preflight ([`crate::resolver`]): each entry is probed
/// before any filesystem mutation, and a miss is reported with a
/// kind-appropriate remediation. The RPM backend ignores these — dnf resolves
/// `Requires` instead — so the same dependency is never resolved twice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeDependency {
    /// Logical dependency identifier (e.g. `btrfs-progs`, `node`).
    pub name: String,
    /// Resolution bucket; see [`DependencyKind`].
    pub kind: DependencyKind,
    /// Optional version constraint (e.g. `>=20`). Presence-first: an
    /// unparseable constraint or probed version does not fail the preflight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Command whose success means the dependency is satisfied (e.g.
    /// `btrfs version`). Absent system packages fall back to a native
    /// package-manager query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
    /// Distribution-source identifier for a [`DependencyKind::LanguageRuntime`]
    /// (surfaced in the manual-install hint while vendoring is unimplemented).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Per-format package names for a [`DependencyKind::SystemPackage`].
    #[serde(default, skip_serializing_if = "PackageNames::is_empty")]
    pub packages: PackageNames,
    /// Built-in capability check identifier for a
    /// [`DependencyKind::PlatformCapability`] (e.g. `btrfs`, `btf`, `cap_bpf`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// Minimum kernel version for a [`DependencyKind::PlatformCapability`]
    /// (e.g. `5.4`). The one hard version gate in the preflight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_kernel: Option<String>,
}

/// One `[[adapters]]` entry. We keep every field the loader can parse so
/// later tooling (adapter registry, doctor, build planner) does not have
/// to re-read the TOML. `detect` is captured as a free-form map because
/// each framework defines its own probe shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AdapterSpec {
    /// Adapter display or manifest name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-facing adapter label for CLI output and documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Framework this adapter targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    /// Adapter kind (`first-party`, `third-party`, `protocol`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Adapter type within the framework (`plugin`, `skill_bundle`,
    /// `extension`, `service`, ...). The adapter manager gates on this
    /// value: only `"plugin"`, `"skill_bundle"`, or absent/`None`
    /// (defaulting to plugin) is supported; all other values are
    /// rejected at enable time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_type: Option<String>,
    /// Trust level (`first-party`, `third-party`, `protocol`). Separate
    /// from `kind` so both dimensions remain usable independently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    /// Framework-native plugin identifier, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Source path inside the component artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Destination path after layout placeholder expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
    /// `[adapters.backends]` — backend-specific resource-root overrides.
    /// A unified raw/RPM contract keeps [`dest`](Self::dest) as the raw
    /// install target while RPM payloads live at a package-owned path;
    /// scan/enable select the root by the component's install provenance.
    #[serde(default, skip_serializing_if = "AdapterBackendsSpec::is_empty")]
    pub backends: AdapterBackendsSpec,
    /// Bundle description: how the driver should read the adapter's
    /// resource directory.
    #[serde(default, skip_serializing_if = "AdapterBundleSpec::is_empty")]
    pub bundle: AdapterBundleSpec,
    /// Compatibility metadata: driver schema and framework version
    /// constraints.
    #[serde(default, skip_serializing_if = "AdapterCompatSpec::is_empty")]
    pub compat: AdapterCompatSpec,
    /// Framework-specific detection hints preserved as TOML values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub detect: BTreeMap<String, toml::Value>,
    /// Skills this adapter delivers into the framework.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_skills",
        serialize_with = "serialize_skills"
    )]
    pub skills: Vec<AdapterSkillSpec>,
    /// Post-install config key/value pairs the driver should apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<AdapterConfigSetSpec>,
    /// Static, display-only operator notices shown after `adapter enable`
    /// or `adapter disable` succeeds. Inert text: never expanded,
    /// substituted, or executed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<AdapterNotice>,
    /// Semver constraint on the target framework version, e.g. `">=1.2"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework_version_req: Option<String>,
    /// OpenClaw-specific adapter configuration, when this adapter targets
    /// the `openclaw` framework.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openclaw: Option<OpenClawAdapterSpec>,
    /// Hermes-specific adapter configuration, when this adapter targets
    /// the `hermes` framework.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes: Option<HermesAdapterSpec>,
}

/// Bundle description for an adapter resource directory. The driver uses
/// `entry` as a hint to locate the framework-native manifest inside the
/// bundle; it is NOT an executable path.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AdapterBundleSpec {
    /// Bundle schema version, for future evolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<u32>,
    /// Entry-point file inside the bundle directory (e.g.
    /// `"plugin.json"`). The driver reads this file to understand the
    /// bundle; it is never executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

impl AdapterBundleSpec {
    fn is_empty(&self) -> bool {
        self.schema.is_none() && self.entry.is_none()
    }
}

/// `[adapters.backends]` — backend-specific adapter resource metadata for
/// unified raw/RPM contracts. Only the RPM backend is defined today; the
/// table shape leaves room for other native backends.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AdapterBackendsSpec {
    /// `[adapters.backends.rpm]` — resource metadata for RPM-installed
    /// components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<AdapterRpmBackendSpec>,
}

impl AdapterBackendsSpec {
    /// True when no backend declares any metadata; the whole `[adapters.
    /// backends]` table is then omitted on serialization so round-tripped
    /// contracts without it stay byte-stable.
    pub fn is_empty(&self) -> bool {
        self.rpm.as_ref().is_none_or(|r| r.resource_root.is_none())
    }
}

/// `[adapters.backends.rpm]` — adapter resource metadata consumed when the
/// owning component was installed via the RPM backend.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AdapterRpmBackendSpec {
    /// Read-only resource root holding the RPM-installed adapter bundle.
    /// May use layout placeholders (`{datadir}`, …, plus `{component}`) or
    /// be an absolute package-owned path such as
    /// `/opt/agent-sec/openclaw-plugin/`. Scan/enable read the bundle from
    /// here instead of expanding [`AdapterSpec::dest`]; ANOLISA never
    /// writes under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_root: Option<String>,
}

/// Compatibility metadata for an adapter entry. Lets packaging and the
/// driver gate on schema evolution and framework version constraints.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AdapterCompatSpec {
    /// Driver-side schema version this adapter targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_schema: Option<u32>,
    /// Semver constraint on the target framework, e.g. `">=0.1.0"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework_version: Option<String>,
}

impl AdapterCompatSpec {
    fn is_empty(&self) -> bool {
        self.driver_schema.is_none() && self.framework_version.is_none()
    }
}

/// A post-install config key/value pair that the driver should apply to
/// the framework's configuration. The driver interprets `key` as a
/// framework-specific config path; `value` is the TOML value to set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdapterConfigSetSpec {
    /// Framework-specific config key path.
    pub key: String,
    /// Value to set.
    pub value: toml::Value,
    /// Optional semver-style constraint on the target framework version,
    /// e.g. `">=2026.4.24"`. When present, the driver applies this config
    /// entry only if the detected framework version satisfies the
    /// constraint; otherwise the entry is skipped and left out of the
    /// receipt. Absent means "always apply", preserving backward
    /// compatibility with older manifests. Skipped on serialize when absent
    /// so those manifests round-trip byte-stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework_version: Option<String>,
}

/// A static, display-only operator notice attached to an adapter. The
/// adapter manager surfaces matching notices after `adapter enable`
/// ([`NoticeWhen::PostEnable`]) or `adapter disable`
/// ([`NoticeWhen::PostDisable`]) succeeds.
///
/// Notices are inert text: `text` and `command` are never shell-expanded,
/// template-substituted, or executed. Structured output preserves their
/// values, while human output escapes control characters. Required framework
/// configuration is NOT a notice — it stays a structured
/// [`AdapterConfigSetSpec`] entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterNotice {
    /// When the notice is displayed.
    pub when: NoticeWhen,
    /// Notice severity. Defaults to [`NoticeLevel::Info`] when absent.
    #[serde(default, skip_serializing_if = "is_default_level")]
    pub level: NoticeLevel,
    /// The notice body. Required; human output escapes control characters.
    pub text: String,
    /// Optional display-only command hint (e.g. a follow-up the operator
    /// may run). Never parsed or executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Lifecycle point at which an [`AdapterNotice`] is displayed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeWhen {
    /// Show after `adapter enable` succeeds.
    PostEnable,
    /// Show after `adapter disable` succeeds.
    PostDisable,
}

/// Severity of an [`AdapterNotice`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    /// Informational notice.
    #[default]
    Info,
    /// Warning the operator should pay attention to.
    Warning,
}

fn is_default_level(level: &NoticeLevel) -> bool {
    matches!(level, NoticeLevel::Info)
}

/// One skill entry in an `[[adapters]]` or framework-specific section.
///
/// Two TOML forms are accepted (via `deserialize_skills`):
///
/// * **String array** (legacy): `skills = ["code-scanner", "prompt-scanner"]`
///   — normalised to `AdapterSkillSpec { name, source: None }`.
/// * **Table array** (recommended):
///   ```toml
///   [[adapters.openclaw.skills]]
///   name   = "code-scanner"
///   source = "{datadir}/skills/code-scanner/"
///   ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdapterSkillSpec {
    /// Skill name.
    pub name: String,
    /// Source directory for the skill bundle. When absent, the driver
    /// falls back to `<resource_root>/skills/<name>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Deserialise `skills` from either a string array or a table array.
fn deserialize_skills<'de, D>(deserializer: D) -> Result<Vec<AdapterSkillSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Item {
        Simple(String),
        Full(AdapterSkillSpec),
    }

    let items: Vec<Item> = Vec::deserialize(deserializer)?;
    Ok(items
        .into_iter()
        .map(|item| match item {
            Item::Simple(name) => AdapterSkillSpec { name, source: None },
            Item::Full(spec) => spec,
        })
        .collect())
}

/// Serialise `skills` as a string array when all sources are `None`,
/// preserving backward-compatible TOML output.
fn serialize_skills<S>(skills: &[AdapterSkillSpec], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    if skills.iter().all(|s| s.source.is_none()) {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        names.serialize(serializer)
    } else {
        skills.serialize(serializer)
    }
}

/// One framework plugin an OpenClaw adapter displaces while it is enabled.
///
/// Some frameworks ship a bundled plugin whose tool names collide with the
/// adapter's own, and the framework's tool registry resolves that collision by
/// silently dropping the later plugin's tool (`plugin tool name conflict`). The
/// bundled plugin stays loaded even after this adapter takes the framework's
/// exclusive slot for it, so installing the adapter alone does not release the
/// names. Declaring the bundled plugin here makes `adapter enable` disable it
/// and `adapter disable` hand it back.
///
/// This is the built-in driver's contract only. A bundle's own `install.sh` /
/// `uninstall.sh` script entry point does not read this declaration and performs
/// no hand-off, so a script install has to release the colliding names itself.
///
/// This is publishing metadata, not an execution hook: the built-in framework
/// driver owns every command it runs, exactly as it does for
/// `[[adapters.openclaw.config]]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplacedPluginSpec {
    /// Framework-native plugin id to disable while this adapter is enabled.
    /// Whitelist-validated before it ever reaches an argv.
    pub id: String,
    /// Exclusive framework slot the displaced plugin re-takes when it is
    /// enabled again — the key suffix under `plugins.slots`, e.g. `"memory"`
    /// for `plugins.slots.memory`. When present, `adapter disable` skips the
    /// restore while that slot belongs to some *third* plugin or has been
    /// explicitly closed, so an operator who moved the slot elsewhere after
    /// enabling — or turned it off outright — keeps that choice; restoring
    /// would re-run the framework's exclusive slot selection and undo it.
    ///
    /// Absent does **not** make the restore unconditional — it only removes the
    /// slot from the set of things consulted. The framework driver still skips a
    /// restore the host has made impossible or pointless, e.g. because the plugin
    /// has left its inventory or a policy key keeps it off. Declare `slot` when
    /// the displaced plugin re-takes an exclusive slot, and omit it when it does
    /// not; neither choice promises the plugin will be re-enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
}

/// OpenClaw-specific adapter configuration. When present on an
/// `[[adapters]]` entry whose `framework = "openclaw"`, the driver uses
/// these fields instead of the generic adapter-level ones.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OpenClawAdapterSpec {
    /// OpenClaw-specific bundle description (overrides generic
    /// `[adapters.bundle]` when present).
    #[serde(default, skip_serializing_if = "AdapterBundleSpec::is_empty")]
    pub bundle: AdapterBundleSpec,
    /// Skills delivered into OpenClaw's skill directory.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_skills",
        serialize_with = "serialize_skills"
    )]
    pub skills: Vec<AdapterSkillSpec>,
    /// Post-install config key/value pairs for OpenClaw.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<AdapterConfigSetSpec>,
    /// Static, display-only operator notices for the OpenClaw adapter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<AdapterNotice>,
    /// Framework plugins this adapter displaces while it is enabled. The
    /// OpenClaw driver disables each one after its own plugin is verified
    /// loaded, records the transition in the receipt, and reverses exactly
    /// what it recorded on disable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub displaces: Vec<DisplacedPluginSpec>,
}

impl OpenClawAdapterSpec {
    /// Whether no framework-specific data is present.
    pub fn is_empty(&self) -> bool {
        self.bundle.is_empty()
            && self.skills.is_empty()
            && self.config.is_empty()
            && self.notices.is_empty()
            && self.displaces.is_empty()
    }
}

/// Hermes-specific adapter configuration. When present on an
/// `[[adapters]]` entry whose `framework = "hermes"`, the driver uses
/// these fields instead of the generic adapter-level ones.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HermesAdapterSpec {
    /// Hermes-specific bundle description.
    #[serde(default, skip_serializing_if = "AdapterBundleSpec::is_empty")]
    pub bundle: AdapterBundleSpec,
    /// Skills delivered into Hermes's skill directory.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_skills",
        serialize_with = "serialize_skills"
    )]
    pub skills: Vec<AdapterSkillSpec>,
    /// Static, display-only operator notices for the Hermes adapter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<AdapterNotice>,
}

impl HermesAdapterSpec {
    /// Whether no framework-specific data is present.
    pub fn is_empty(&self) -> bool {
        self.bundle.is_empty() && self.skills.is_empty() && self.notices.is_empty()
    }
}

#[derive(Deserialize)]
struct ComponentManifestRaw {
    #[serde(default = "current_schema_version", alias = "manifest_version")]
    schema_version: u32,
    component: ComponentMetaRaw,
    #[serde(default)]
    source: Option<SourceRaw>,
    #[serde(default)]
    distribution: Option<DistributionRaw>,
    #[serde(default)]
    build: Option<BuildRaw>,
    #[serde(default)]
    install: Option<InstallRaw>,
    // `[backends]` deserializes directly into the typed shape: the structs are
    // simple and tolerant (`default` table, optional `rpm` sub-table), so no
    // separate Raw mirror is warranted.
    #[serde(default)]
    backends: ManifestBackends,
    #[serde(default, alias = "env_requirements")]
    environment: EnvRequirementsRaw,
    #[serde(default)]
    dependencies: DependenciesRaw,
    #[serde(default)]
    features: Vec<FeatureRaw>,
    #[serde(default)]
    adapters: Vec<AdapterRaw>,
}

#[derive(Deserialize)]
struct ComponentMetaRaw {
    name: String,
    version: String,
    #[serde(default = "default_runtime_layer")]
    layer: String,
    #[serde(default)]
    domain: Option<String>,
    // Minimal-schema additions on the `[component]` table.
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    /// Component-level mutual exclusion list. Names listed here cannot
    /// coexist with this component when installed via the raw backend.
    #[serde(default)]
    conflicts: Vec<String>,
    // Minimal-schema nested sub-tables (`[component.contract]`, etc.). When
    // present they take precedence over the legacy top-level sections.
    #[serde(default)]
    contract: Option<ContractRaw>,
    #[serde(default)]
    platform: Option<EnvRequirementsRaw>,
    #[serde(default)]
    artifact: Option<ArtifactRaw>,
    #[serde(default)]
    layout: Option<LayoutRaw>,
    // `[[component.services]]` — systemd unit lifecycle declarations, a
    // sibling of `[component.layout]`. ServiceSpec is simple and tolerant
    // (every field but `unit` defaults), so it deserializes directly without
    // a separate Raw mirror — same rationale as `[backends]`.
    #[serde(default)]
    services: Vec<ServiceSpec>,
    // `[[component.capabilities]]` — install-related setcap declarations,
    // a sibling of `[component.layout]` in the minimal schema (the legacy
    // schema declares them under `[install.capabilities]` instead).
    #[serde(default)]
    capabilities: Vec<InstallCapabilityRaw>,
    // `[[component.hooks]]` — lifecycle hook declarations, minimal-schema only
    // (legacy `[install]` predates hooks). ManifestHookSpec deserializes
    // directly: phase + script are required, the rest defaults.
    #[serde(default)]
    hooks: Vec<ManifestHookSpec>,
    // `[component.health_check]` — minimal-schema structured check. Parses
    // directly into the internally-tagged `CheckSpec` (`type = "..."`).
    #[serde(default)]
    health_check: Option<CheckSpec>,
    // `[[component.dependencies]]` — structured runtime dependency entries, a
    // sibling of `[component.layout]`. Nested under `[component]`, so it never
    // collides with the top-level `[dependencies]` parsed into
    // `ComponentManifestRaw::dependencies`. RuntimeDependency deserializes
    // directly: `name` + `kind` are required, the rest defaults.
    #[serde(default)]
    dependencies: Vec<RuntimeDependency>,
}

#[derive(Deserialize, Default)]
struct ContractRaw {
    #[serde(default)]
    schema_version: Option<String>,
    #[serde(default)]
    min_anolisa_version: Option<String>,
}

#[derive(Deserialize, Default)]
struct ArtifactRaw {
    #[serde(default, rename = "type")]
    artifact_type: Option<String>,
    #[serde(default)]
    archive_format: Option<String>,
    #[serde(default)]
    naming_pattern: Option<String>,
}

/// `[component.layout]` — minimal-schema install layout. Maps onto the same
/// internal [`InstallSpec`] as the legacy `[install]` section.
#[derive(Deserialize, Default)]
struct LayoutRaw {
    #[serde(default)]
    modes: Vec<String>,
    #[serde(default)]
    files: Vec<LayoutFileRaw>,
}

#[derive(Deserialize, Default)]
struct LayoutFileRaw {
    #[serde(default)]
    source: Option<String>,
    /// Minimal schema uses `target`; `dest` tolerated for symmetry.
    #[serde(default, alias = "dest")]
    target: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    /// Minimal-schema `type` → [`FileKind`]. Absent defaults to `Data`.
    #[serde(default, rename = "type")]
    kind: FileKind,
    /// Content-rendering mode (see [`InstallFileSpec::render`]).
    #[serde(default)]
    render: Option<String>,
}

#[derive(Deserialize, Default)]
struct SourceRaw {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    upstream: Option<String>,
}

#[derive(Deserialize, Default)]
struct DistributionRaw {
    #[serde(default)]
    selectors: Vec<DistributionSelectorRaw>,
}

#[derive(Deserialize, Default)]
struct DistributionSelectorRaw {
    #[serde(default)]
    install_mode: Option<String>,
    #[serde(default)]
    os: Vec<String>,
    #[serde(default)]
    arch: Vec<String>,
    #[serde(default)]
    libc: Option<String>,
    #[serde(default)]
    pkg_base: Option<String>,
    #[serde(default)]
    preferred_artifact_types: Vec<ArtifactType>,
}

impl From<DistributionSelectorRaw> for DistributionSelector {
    fn from(raw: DistributionSelectorRaw) -> Self {
        Self {
            install_mode: raw.install_mode,
            os: raw.os,
            arch: raw.arch,
            libc: raw.libc,
            pkg_base: raw.pkg_base,
            preferred_artifact_types: raw.preferred_artifact_types,
        }
    }
}

#[derive(Deserialize, Default)]
struct BuildRaw {
    #[serde(default, alias = "backend")]
    system: Option<String>,
    #[serde(default, alias = "outputs")]
    targets: Vec<String>,
    #[serde(default)]
    outputs_named: Vec<BuildOutputRaw>,
}

#[derive(Deserialize)]
struct BuildOutputRaw {
    name: String,
}

#[derive(Deserialize, Default)]
struct InstallRaw {
    #[serde(default)]
    modes: Vec<String>,
    #[serde(default)]
    files: Vec<InstallFileRaw>,
    #[serde(default)]
    services: Vec<String>,
    #[serde(default)]
    capabilities: Vec<InstallCapabilityRaw>,
}

#[derive(Deserialize, Default)]
struct InstallFileRaw {
    #[serde(default)]
    dest: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    /// Optional `type` so legacy `[install]` manifests can declare
    /// symlinks; absent defaults to `Data`, keeping old files byte-stable.
    #[serde(default, rename = "type")]
    kind: FileKind,
    /// Content-rendering mode (see [`InstallFileSpec::render`]). Parsed on
    /// the legacy path too — both because serialized manifests round-trip
    /// through `[install]`, and because silently dropping a declared
    /// render would install literal placeholders (the failure this field
    /// exists to prevent).
    #[serde(default)]
    render: Option<String>,
}

#[derive(Deserialize, Default)]
struct InstallCapabilityRaw {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    caps: Vec<String>,
    #[serde(default)]
    optional: bool,
}

#[derive(Deserialize, Default)]
struct DependenciesRaw {
    #[serde(default)]
    build: Vec<String>,
    #[serde(default)]
    runtime: Vec<String>,
    #[serde(default)]
    components: Vec<String>,
}

#[derive(Deserialize)]
struct FeatureRaw {
    name: String,
    #[serde(default, alias = "label")]
    description: String,
    #[serde(default)]
    default: bool,
}

#[derive(Deserialize, Default)]
struct AdapterRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    framework: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    adapter_type: Option<String>,
    #[serde(default)]
    trust: Option<String>,
    #[serde(default)]
    plugin_id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    dest: Option<String>,
    // `[adapters.backends]` deserializes directly into the typed shape —
    // simple and tolerant (`default` table, optional `rpm` sub-table) —
    // same rationale as the top-level `[backends]`.
    #[serde(default)]
    backends: AdapterBackendsSpec,
    #[serde(default)]
    bundle: AdapterBundleRaw,
    #[serde(default)]
    compat: AdapterCompatRaw,
    #[serde(default)]
    detect: BTreeMap<String, toml::Value>,
    #[serde(default, deserialize_with = "deserialize_skills")]
    skills: Vec<AdapterSkillSpec>,
    #[serde(default)]
    config: Vec<AdapterConfigSetSpec>,
    #[serde(default)]
    notices: Vec<AdapterNotice>,
    #[serde(default)]
    framework_version_req: Option<String>,
    #[serde(default)]
    openclaw: Option<OpenClawAdapterSpec>,
    #[serde(default)]
    hermes: Option<HermesAdapterSpec>,
}

#[derive(Deserialize, Default)]
struct AdapterBundleRaw {
    #[serde(default)]
    schema: Option<u32>,
    #[serde(default)]
    entry: Option<String>,
}

#[derive(Deserialize, Default)]
struct AdapterCompatRaw {
    #[serde(default)]
    driver_schema: Option<u32>,
    #[serde(default)]
    framework_version: Option<String>,
}

impl From<ComponentManifestRaw> for ComponentManifest {
    fn from(raw: ComponentManifestRaw) -> Self {
        // Destructure the `[component]` table once so the nested minimal-schema
        // sub-tables (contract/platform/artifact/layout) can be consumed
        // alongside the identity fields without partial-move friction.
        let ComponentMetaRaw {
            name,
            version,
            layer,
            domain,
            display_name,
            owner,
            license,
            repository,
            conflicts,
            contract: contract_raw,
            platform: platform_raw,
            artifact: artifact_raw,
            layout: layout_raw,
            services: component_services,
            capabilities: component_capabilities,
            hooks: component_hooks,
            health_check,
            dependencies: component_runtime_deps,
        } = raw.component;

        let component = ComponentMeta {
            name,
            version,
            layer,
            domain,
            display_name,
            owner,
            license,
            repository,
            conflicts,
        };

        let contract = contract_raw
            .map(|c| ContractSpec {
                schema_version: c.schema_version,
                min_anolisa_version: c.min_anolisa_version,
            })
            .unwrap_or_default();

        let artifact = artifact_raw
            .map(|a| ArtifactSpec {
                artifact_type: a.artifact_type,
                archive_format: a.archive_format,
                naming_pattern: a.naming_pattern,
            })
            .unwrap_or_default();

        let source = raw.source.map(source_from_raw).unwrap_or_default();

        let distribution_selectors = raw
            .distribution
            .map(|d| {
                d.selectors
                    .into_iter()
                    .map(DistributionSelector::from)
                    .collect()
            })
            .unwrap_or_default();

        let build = raw
            .build
            .map(|b| {
                let mut outputs = b.targets;
                outputs.extend(b.outputs_named.into_iter().map(|o| o.name));
                BuildSpec {
                    backend: b.system.unwrap_or_default(),
                    outputs,
                }
            })
            .unwrap_or_default();

        // Prefer the minimal-schema `[component.layout]`; fall back to the
        // legacy top-level `[install]` for not-yet-migrated manifests. The
        // minimal `target` key maps onto the internal `dest`. Services,
        // capabilities, and hooks are `[component.*]` siblings of
        // `[component.layout]`, parsed only on this minimal path; legacy
        // `[install]` carries services/capabilities inline and predates hooks.
        // Legacy files may carry `type` (e.g. symlinks); absent defaults `Data`.
        let install = if let Some(layout) = layout_raw {
            let files = layout
                .files
                .into_iter()
                .map(|f| InstallFileSpec {
                    source: f.source,
                    dest: f.target,
                    mode: f.mode,
                    kind: f.kind,
                    render: f.render,
                })
                .filter(|f| f.install_path().is_some())
                .collect();
            let capabilities = capabilities_from_raw(component_capabilities);
            let services = component_services
                .into_iter()
                .filter(|s| !s.unit.is_empty())
                .collect();
            let hooks = component_hooks
                .into_iter()
                .filter(|h| !h.script.is_empty())
                .collect();
            InstallSpec {
                modes: layout.modes,
                files,
                services,
                capabilities,
                hooks,
            }
        } else {
            raw.install
                .map(|i| {
                    let files = i
                        .files
                        .into_iter()
                        .map(|f| InstallFileSpec {
                            source: f.source,
                            dest: f.dest,
                            mode: f.mode,
                            kind: f.kind,
                            render: f.render,
                        })
                        .filter(|f| f.install_path().is_some())
                        .collect();
                    let capabilities = capabilities_from_raw(i.capabilities);
                    let services = i
                        .services
                        .into_iter()
                        .filter(|u| !u.is_empty())
                        .map(ServiceSpec::from_legacy_unit)
                        .collect();
                    InstallSpec {
                        modes: i.modes,
                        files,
                        services,
                        capabilities,
                        // Hooks are minimal-schema only; legacy `[install]`
                        // predates them.
                        hooks: Vec::new(),
                    }
                })
                .unwrap_or_default()
        };

        let dependencies = DependenciesSpec {
            build: raw.dependencies.build,
            runtime: raw.dependencies.runtime,
            components: raw.dependencies.components,
        };

        // Drop nameless entries, mirroring the empty-unit/empty-script filters
        // applied to services and hooks above.
        let runtime_deps = component_runtime_deps
            .into_iter()
            .filter(|d| !d.name.is_empty())
            .collect();

        let features = raw
            .features
            .into_iter()
            .map(|f| FeatureSpec {
                name: f.name,
                description: f.description,
                default: f.default,
            })
            .collect();

        let adapters: Vec<AdapterSpec> = raw
            .adapters
            .into_iter()
            .map(|a| AdapterSpec {
                name: a.name,
                display_name: a.display_name,
                framework: a.framework,
                kind: a.kind,
                adapter_type: a.adapter_type,
                trust: a.trust,
                plugin_id: a.plugin_id,
                source: a.source,
                dest: a.dest,
                backends: a.backends,
                bundle: AdapterBundleSpec {
                    schema: a.bundle.schema,
                    entry: a.bundle.entry,
                },
                compat: AdapterCompatSpec {
                    driver_schema: a.compat.driver_schema,
                    framework_version: a.compat.framework_version,
                },
                detect: a.detect,
                skills: a.skills,
                config: a.config,
                notices: a.notices,
                framework_version_req: a.framework_version_req,
                openclaw: a.openclaw,
                hermes: a.hermes,
            })
            .collect();

        // Prefer the minimal-schema `[component.platform]`; fall back to the
        // legacy `[environment]` / `requires_env`.
        let env_requirements = platform_raw
            .map(EnvRequirements::from)
            .unwrap_or_else(|| raw.environment.into());

        Self {
            schema_version: raw.schema_version,
            component,
            contract,
            artifact,
            source,
            distribution_selectors,
            build,
            install,
            backends: raw.backends,
            env_requirements,
            dependencies,
            runtime_deps,
            features,
            adapters,
            health_check,
        }
    }
}

/// Map raw capability entries to specs, dropping fully-empty rows. Shared by
/// the minimal-schema (`[[component.capabilities]]`) and legacy
/// (`[install.capabilities]`) parse paths so the two cannot drift.
fn capabilities_from_raw(raw: Vec<InstallCapabilityRaw>) -> Vec<InstallCapabilitySpec> {
    raw.into_iter()
        .map(|c| InstallCapabilitySpec {
            path: c.path,
            caps: c.caps,
            optional: c.optional,
        })
        .filter(|c| c.path.is_some() || !c.caps.is_empty())
        .collect()
}

fn source_from_raw(raw: SourceRaw) -> SourceSpec {
    let kind = raw
        .kind
        .or(raw.upstream)
        .unwrap_or_else(|| "workspace".to_string());
    SourceSpec {
        kind,
        path: raw.path,
        url: raw.url,
    }
}

// ---------------------------------------------------------------------------
// EnvRequirements
// ---------------------------------------------------------------------------

/// Host requirements normalized from the component TOML styles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(from = "EnvRequirementsRaw")]
pub struct EnvRequirements {
    /// Accepted OS names.
    pub os: Vec<String>,
    /// Accepted CPU architectures.
    pub arch: Vec<String>,
    /// Accepted libc families.
    pub libc: Vec<String>,
    /// Minimum kernel version.
    pub kernel_min: Option<String>,
    /// Whether BTF debug data must be available.
    pub btf: Option<bool>,
    /// Whether the process/host must expose CAP_BPF.
    pub cap_bpf: Option<bool>,
    /// Accepted package bases.
    pub pkg_base: Vec<String>,
}

#[derive(Deserialize, Default)]
struct EnvRequirementsRaw {
    // Bare keys, also accepted via `[component.platform]`.
    #[serde(default)]
    os: Option<StringOrList>,
    #[serde(default)]
    arch: Option<StringOrList>,
    #[serde(default)]
    libc: Option<StringOrList>,
    #[serde(default, alias = "min_kernel")]
    kernel: Option<String>,
    #[serde(default)]
    pkg_base: Option<StringOrList>,

    // Component-style keys.
    #[serde(default)]
    requires_os: Option<StringOrList>,
    #[serde(default)]
    requires_arch: Option<StringOrList>,
    #[serde(default)]
    requires_libc: Option<StringOrList>,
    #[serde(default)]
    requires_kernel: Option<String>,
    #[serde(default)]
    requires_pkg_base: Option<StringOrList>,

    // Either prefix accepts the free-form map.
    #[serde(default)]
    requires_env: BTreeMap<String, toml::Value>,

    #[serde(default)]
    btf: Option<bool>,
    #[serde(default)]
    cap_bpf: Option<bool>,
}

impl From<EnvRequirementsRaw> for EnvRequirements {
    fn from(r: EnvRequirementsRaw) -> Self {
        let merge = |a: Option<StringOrList>, b: Option<StringOrList>| -> Vec<String> {
            a.or(b).map(|v| v.into_vec()).unwrap_or_default()
        };
        let btf = r
            .btf
            .or_else(|| lookup_bool(&r.requires_env, "btf_available"))
            .or_else(|| lookup_bool(&r.requires_env, "btf"));
        let cap_bpf = r
            .cap_bpf
            .or_else(|| lookup_cap_bpf(r.requires_env.get("linux_capabilities")))
            .or_else(|| lookup_cap_bpf(r.requires_env.get("capability")));
        Self {
            os: merge(r.os, r.requires_os),
            arch: merge(r.arch, r.requires_arch),
            libc: merge(r.libc, r.requires_libc),
            kernel_min: r.kernel.or(r.requires_kernel),
            btf,
            cap_bpf,
            pkg_base: merge(r.pkg_base, r.requires_pkg_base),
        }
    }
}

fn lookup_bool(map: &BTreeMap<String, toml::Value>, key: &str) -> Option<bool> {
    match map.get(key)? {
        toml::Value::Boolean(b) => Some(*b),
        toml::Value::String(s) => match s.as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn lookup_cap_bpf(value: Option<&toml::Value>) -> Option<bool> {
    let v = value?;
    match v {
        toml::Value::String(s) => Some(s.eq_ignore_ascii_case("CAP_BPF")),
        toml::Value::Array(items) => Some(items.iter().any(|item| match item {
            toml::Value::String(s) => s.eq_ignore_ascii_case("CAP_BPF"),
            _ => false,
        })),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum StringOrList {
    One(String),
    Many(Vec<String>),
}

impl StringOrList {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(v) => v,
        }
    }
}

impl<'de> Deserialize<'de> for StringOrList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            One(String),
            Many(Vec<String>),
        }
        Ok(match Helper::deserialize(deserializer)? {
            Helper::One(s) => Self::One(s),
            Helper::Many(v) => Self::Many(v),
        })
    }
}

fn current_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}
fn default_runtime_layer() -> String {
    "runtime".to_string()
}

// ---------------------------------------------------------------------------
// File-loading entry points
// ---------------------------------------------------------------------------

impl ComponentManifest {
    /// Load a component manifest from TOML on disk.
    pub fn from_file(path: &Path) -> Result<Self, ManifestError> {
        let content = read_to_string(path)?;
        toml::from_str(&content)
            .map_err(|e| ManifestError::Parse(path.display().to_string(), e.to_string()))
    }

    /// Parse a component manifest from TOML text.
    pub fn from_toml_str(s: &str) -> Result<Self, ManifestError> {
        toml::from_str(s).map_err(|e| ManifestError::Parse("<string>".into(), e.to_string()))
    }

    /// Declared RPM package name from `[backends.rpm].package`, if any.
    ///
    /// This is the highest-precedence package-name source after a CLI
    /// `--package` override during RPM adopt; `None` falls through to the
    /// repo.toml `package_map` / provides / default-naming tiers.
    pub fn rpm_package(&self) -> Option<&str> {
        self.backends.rpm.as_ref().map(|s| s.package.as_str())
    }

    /// Health check to run after install: the declared
    /// `[component.health_check]`, or a synthesized `binary_version` over the
    /// first [`FileKind::Executable`] layout file. Returns `None` when neither
    /// is available (no declared check and no executable to probe).
    ///
    /// The synthesized probe targets the file's install destination
    /// (post-placeholder template), so the engine expands `{bindir}` against
    /// the active layout — matching how the file itself was installed.
    pub fn health_spec(&self) -> Option<CheckSpec> {
        if let Some(spec) = &self.health_check {
            return Some(spec.clone());
        }
        self.install
            .files
            .iter()
            .find(|f| f.kind == FileKind::Executable)
            .and_then(|f| f.install_path())
            .map(|target| CheckSpec::BinaryVersion {
                binary: target.to_string(),
                expect_pattern: None,
                timeout_secs: None,
            })
    }
}

fn read_to_string(path: &Path) -> Result<String, ManifestError> {
    std::fs::read_to_string(path).map_err(|e| ManifestError::Io(path.display().to_string(), e))
}

/// Helper used by [`Catalog`] when scanning layer directories.
pub(crate) fn manifest_paths(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if !dir.exists() {
        return files;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("toml") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Errors raised while loading manifests.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Manifest file could not be read.
    #[error("cannot read manifest '{0}': {1}")]
    Io(String, std::io::Error),
    /// Manifest TOML could not be parsed.
    #[error("cannot parse manifest '{0}': {1}")]
    Parse(String, String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_names(names: &[&str]) -> Vec<AdapterSkillSpec> {
        names
            .iter()
            .map(|n| AdapterSkillSpec {
                name: n.to_string(),
                source: None,
            })
            .collect()
    }

    #[test]
    fn component_manifest_parses_existing_fixture() {
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"
            layer = "runtime"
            domain = "observability"

            [build]
            system = "cargo"
            targets = ["agentsight"]

            [install]
            modes = ["system"]
            services = ["agentsight.service"]

            [[install.files]]
            source = "target/release/agentsight"
            dest = "{bindir}/agentsight"

            [environment]
            requires_os = "linux"
            requires_arch = ["x86_64"]
            requires_kernel = ">=5.8"

            [environment.requires_env]
            btf_available = "true"
            capability = "CAP_BPF"

            [dependencies]
            build = ["rust>=1.91"]
            runtime = ["kernel-headers"]

            [[features]]
            name = "token_counting"
            label = "LLM Token metering"
            default = true
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.component.name, "agentsight");
        assert_eq!(m.component.domain.as_deref(), Some("observability"));
        assert_eq!(m.build.backend, "cargo");
        assert_eq!(m.install.modes, vec!["system"]);
        assert_eq!(m.install.files.len(), 1);
        assert_eq!(
            m.install.files[0].source.as_deref(),
            Some("target/release/agentsight")
        );
        assert_eq!(
            m.install.files[0].dest.as_deref(),
            Some("{bindir}/agentsight")
        );
        assert_eq!(m.env_requirements.kernel_min.as_deref(), Some(">=5.8"));
        assert_eq!(m.env_requirements.btf, Some(true));
        assert_eq!(m.env_requirements.cap_bpf, Some(true));
        assert_eq!(m.features.len(), 1);
        assert_eq!(m.features[0].description, "LLM Token metering");
        assert_eq!(m.dependencies.build, vec!["rust>=1.91"]);
        assert_eq!(m.dependencies.runtime, vec!["kernel-headers"]);
        assert!(m.dependencies.components.is_empty());
        // No `[backends]` table → empty, and the rpm-package accessor is None.
        assert!(m.backends.is_empty());
        assert_eq!(m.rpm_package(), None);
    }

    #[test]
    fn component_manifest_parses_rpm_backend_package() {
        let toml_text = r#"
            [component]
            name = "copilot-shell"
            version = "0.1.0"
            layer = "runtime"

            [backends.rpm]
            package = "copilot-shell"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert!(!m.backends.is_empty());
        assert_eq!(m.rpm_package(), Some("copilot-shell"));
    }

    #[test]
    fn component_manifest_rpm_backend_round_trips() {
        // `[backends]` must survive a serialize→deserialize cycle, and a
        // manifest without it must serialize *without* an empty table (the
        // `skip_serializing_if` contract that keeps raw-only manifests clean).
        let with_rpm = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "copilot-shell"
            version = "0.1.0"
            layer = "runtime"

            [backends.rpm]
            package = "copilot-shell"
        "#,
        )
        .expect("parse");
        let dumped = toml::to_string(&with_rpm).expect("serialize");
        assert!(
            dumped.contains("copilot-shell"),
            "rpm package must round-trip: {dumped}"
        );

        let without = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "agentsight"
            version = "0.2.0"
            layer = "runtime"
        "#,
        )
        .expect("parse");
        let dumped = toml::to_string(&without).expect("serialize");
        assert!(
            !dumped.contains("[backends]"),
            "empty backends must be skipped on serialize: {dumped}"
        );
    }

    #[test]
    fn component_manifest_parses_minimal_schema() {
        // Minimal schema (phase1-2-dev §2.1): namespaced [component.*] sections.
        let toml_text = r#"
            [component]
            name = "tokenless"
            version = "0.5.0"
            display_name = "Tokenless"
            owner = "tokenless-team"
            license = "MIT"
            repository = "https://github.com/alibaba/anolisa"

            [component.contract]
            schema_version = "1.0"
            min_anolisa_version = "0.2.0"

            [component.platform]
            os = ["linux"]
            arch = ["x86_64", "aarch64"]
            min_kernel = "5.4"

            [component.artifact]
            type = "archive"
            archive_format = "tar.gz"
            naming_pattern = "{name}-{version}-{os}-{arch}.tar.gz"

            [component.layout]
            modes = ["user", "system"]

            [[component.layout.files]]
            source = "bin/tokenless"
            target = "{bindir}/tokenless"
            mode = "0755"
            type = "executable"

            [component.health_check]
            type = "binary_version"
            binary = "{bindir}/tokenless"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse minimal");
        // Identity + new [component] metadata.
        assert_eq!(m.component.name, "tokenless");
        assert_eq!(m.component.display_name.as_deref(), Some("Tokenless"));
        assert_eq!(m.component.owner.as_deref(), Some("tokenless-team"));
        assert_eq!(m.component.license.as_deref(), Some("MIT"));
        // [component.contract] / [component.artifact].
        assert_eq!(m.contract.schema_version.as_deref(), Some("1.0"));
        assert_eq!(m.contract.min_anolisa_version.as_deref(), Some("0.2.0"));
        assert_eq!(m.artifact.artifact_type.as_deref(), Some("archive"));
        assert_eq!(m.artifact.archive_format.as_deref(), Some("tar.gz"));
        // [component.platform] → env_requirements (min_kernel → kernel_min).
        assert_eq!(m.env_requirements.os, vec!["linux"]);
        assert_eq!(m.env_requirements.arch, vec!["x86_64", "aarch64"]);
        assert_eq!(m.env_requirements.kernel_min.as_deref(), Some("5.4"));
        // [component.layout] → install (minimal `target` mapped to dest).
        assert_eq!(m.install.modes, vec!["user", "system"]);
        assert_eq!(m.install.files.len(), 1);
        assert_eq!(m.install.files[0].source.as_deref(), Some("bin/tokenless"));
        assert_eq!(
            m.install.files[0].dest.as_deref(),
            Some("{bindir}/tokenless")
        );
    }

    #[test]
    fn minimal_layout_takes_precedence_over_legacy_install() {
        // When both are present, [component.layout] wins (migration guard).
        let toml_text = r#"
            [component]
            name = "dual"
            version = "1.0.0"

            [component.layout]
            [[component.layout.files]]
            source = "bin/new"
            target = "{bindir}/new"

            [install]
            modes = ["system"]
            [[install.files]]
            source = "bin/old"
            dest = "{bindir}/old"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.install.files.len(), 1);
        assert_eq!(m.install.files[0].source.as_deref(), Some("bin/new"));
    }

    #[test]
    fn install_files_preserve_source_dest_and_fallbacks() {
        let toml_text = r#"
            [component]
            name = "tool"
            version = "1.0.0"

            [install]
            modes = ["user"]

            [[install.files]]
            source = "target/release/tool"
            dest = "{bindir}/tool"

            [[install.files]]
            source = "{datadir}/source-only"

            [[install.files]]
            dest = "{etcdir}/dest-only"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");

        assert_eq!(m.install.files.len(), 3);
        assert_eq!(
            m.install.files[0].source.as_deref(),
            Some("target/release/tool")
        );
        assert_eq!(m.install.files[0].dest.as_deref(), Some("{bindir}/tool"));
        assert_eq!(m.install.files[0].install_path(), Some("{bindir}/tool"));
        assert_eq!(
            m.install.files[1].source.as_deref(),
            Some("{datadir}/source-only")
        );
        assert_eq!(m.install.files[1].dest, None);
        assert_eq!(
            m.install.files[1].install_path(),
            Some("{datadir}/source-only")
        );
        assert_eq!(m.install.files[2].source, None);
        assert_eq!(
            m.install.files[2].dest.as_deref(),
            Some("{etcdir}/dest-only")
        );
        assert_eq!(
            m.install.files[2].install_path(),
            Some("{etcdir}/dest-only")
        );
    }

    #[test]
    fn install_capabilities_preserve_path_and_caps() {
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"

            [install]
            modes = ["system"]

            [[install.capabilities]]
            path = "{bindir}/agentsight"
            caps = ["cap_bpf", "cap_perfmon"]
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");

        assert_eq!(m.install.capabilities.len(), 1);
        assert_eq!(
            m.install.capabilities[0].path.as_deref(),
            Some("{bindir}/agentsight")
        );
        assert_eq!(
            m.install.capabilities[0].caps,
            vec!["cap_bpf", "cap_perfmon"]
        );
    }

    #[test]
    fn install_capability_optional_parses_and_defaults_false() {
        // `optional = true` marks a setcap as best-effort (warn on failure
        // rather than aborting the install); absent, it defaults to a strict
        // capability whose failure must abort.
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"

            [install]
            modes = ["system"]

            [[install.capabilities]]
            path = "{bindir}/agentsight"
            caps = ["cap_bpf", "cap_perfmon"]
            optional = true

            [[install.capabilities]]
            path = "{bindir}/strict-tool"
            caps = ["cap_net_admin"]
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");

        assert_eq!(m.install.capabilities.len(), 2);
        assert!(m.install.capabilities[0].optional);
        assert!(!m.install.capabilities[1].optional);
    }

    #[test]
    fn minimal_schema_parses_component_capabilities() {
        // `[[component.capabilities]]` lives in the [component] namespace,
        // a sibling of [component.layout]; the layout branch must read it
        // (rather than dropping it on the floor as it did before).
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"

            [component.layout]
            modes = ["system"]

            [[component.layout.files]]
            source = "bin/agentsight"
            target = "{bindir}/agentsight"
            type = "executable"

            [[component.capabilities]]
            path = "{bindir}/agentsight"
            caps = ["cap_bpf", "cap_perfmon"]
            optional = true
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse minimal caps");

        assert_eq!(m.install.capabilities.len(), 1);
        assert_eq!(
            m.install.capabilities[0].path.as_deref(),
            Some("{bindir}/agentsight")
        );
        assert_eq!(
            m.install.capabilities[0].caps,
            vec!["cap_bpf", "cap_perfmon"]
        );
        assert!(m.install.capabilities[0].optional);
    }

    #[test]
    fn minimal_schema_parses_component_services_with_defaults() {
        // `[[component.services]]` parses into ServiceSpec. scope defaults to
        // system; enable/start default to true; instance is template-only.
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"

            [component.layout]
            modes = ["system"]

            [[component.layout.files]]
            source = "bin/agentsight"
            target = "{bindir}/agentsight"
            type = "executable"

            [[component.services]]
            unit = "agentsight.service"

            [[component.services]]
            unit = "anolisa-memory@.service"
            scope = "user"
            enable = false
            start = false
            instance = "%u"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse services");

        assert_eq!(m.install.services.len(), 2);
        // First service: every optional field defaulted.
        assert_eq!(m.install.services[0].unit, "agentsight.service");
        assert_eq!(m.install.services[0].scope, ServiceScope::System);
        assert!(m.install.services[0].enable);
        assert!(m.install.services[0].start);
        assert_eq!(m.install.services[0].instance, None);
        // Second service: explicit user-scope opt-in template unit.
        assert_eq!(m.install.services[1].unit, "anolisa-memory@.service");
        assert_eq!(m.install.services[1].scope, ServiceScope::User);
        assert!(!m.install.services[1].enable);
        assert!(!m.install.services[1].start);
        assert_eq!(m.install.services[1].instance.as_deref(), Some("%u"));
    }

    #[test]
    fn service_declarations_cover_exact_and_template_units() {
        let plain = ServiceSpec::from_legacy_unit("agentsight.service".to_string());
        assert!(plain.covers_unit("agentsight.service"));
        assert!(!plain.covers_unit("agentsight-user.service"));

        let template = ServiceSpec {
            unit: "anolisa-memory@.service".to_string(),
            scope: ServiceScope::User,
            enable: true,
            start: true,
            instance: Some("%u".to_string()),
        };
        assert!(template.covers_unit("anolisa-memory@alice.service"));
        // Exact match on the template's own literal name still counts.
        assert!(template.covers_unit("anolisa-memory@.service"));
        assert!(!template.covers_unit("anolisa-memory@.service.d"));
    }

    #[test]
    fn declared_unit_scope_defaults_to_system_without_a_covering_decl() {
        let declared = vec![ServiceSpec {
            unit: "anolisa-memory@.service".to_string(),
            scope: ServiceScope::User,
            enable: true,
            start: true,
            instance: Some("%u".to_string()),
        }];
        assert_eq!(
            declared_unit_scope(&declared, "anolisa-memory@alice.service"),
            ServiceScope::User
        );
        assert_eq!(
            declared_unit_scope(&declared, "agentsight.service"),
            ServiceScope::System
        );
        assert_eq!(
            declared_unit_scope(&[], "anything.service"),
            ServiceScope::System
        );
    }

    #[test]
    fn legacy_install_services_map_to_specs_with_defaults() {
        // Legacy `[install] services = [...]` is a bare unit-name list; each
        // name becomes a system-scoped ServiceSpec with enable/start true.
        let toml_text = r#"
            [component]
            name = "ws-ckpt"
            version = "0.1.0"

            [install]
            modes = ["system"]
            services = ["ws-ckpt.service"]
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse legacy services");

        assert_eq!(m.install.services.len(), 1);
        assert_eq!(m.install.services[0].unit, "ws-ckpt.service");
        assert_eq!(m.install.services[0].scope, ServiceScope::System);
        assert!(m.install.services[0].enable);
        assert!(m.install.services[0].start);
    }

    #[test]
    fn minimal_schema_parses_component_hooks_with_defaults() {
        // `[[component.hooks]]` declares lifecycle scripts. strict defaults to
        // false (warn-and-continue) and timeout_secs to 30; phase reuses the
        // executor's HookPhase vocabulary. `script` stays an unexpanded
        // layout-template path — expansion happens at install time.
        let toml_text = r#"
            [component]
            name = "ws-ckpt"
            version = "0.1.0"

            [component.layout]
            modes = ["system"]

            [[component.layout.files]]
            source = "bin/ws-ckpt"
            target = "{bindir}/ws-ckpt"
            type = "executable"

            [[component.hooks]]
            phase = "pre_uninstall"
            script = "{datadir}/hooks/ws-ckpt/pre-uninstall.sh"

            [[component.hooks]]
            phase = "post_uninstall"
            script = "{datadir}/hooks/ws-ckpt/post-uninstall.sh"
            timeout_secs = 120
            strict = true
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse hooks");

        assert_eq!(m.install.hooks.len(), 2);
        // First hook: defaulted timeout + non-strict.
        assert_eq!(m.install.hooks[0].phase, HookPhase::PreUninstall);
        assert_eq!(
            m.install.hooks[0].script,
            "{datadir}/hooks/ws-ckpt/pre-uninstall.sh"
        );
        assert_eq!(m.install.hooks[0].timeout_secs, 30);
        assert!(!m.install.hooks[0].strict);
        // Second hook: explicit timeout + strict abort.
        assert_eq!(m.install.hooks[1].phase, HookPhase::PostUninstall);
        assert_eq!(m.install.hooks[1].timeout_secs, 120);
        assert!(m.install.hooks[1].strict);
    }

    #[test]
    fn dependencies_preserve_build_runtime_components_kind() {
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"

            [dependencies]
            build = ["rust>=1.91", "clang>=14"]
            runtime = ["kernel-headers"]
            components = ["sec-core"]
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.dependencies.build, vec!["rust>=1.91", "clang>=14"]);
        assert_eq!(m.dependencies.runtime, vec!["kernel-headers"]);
        assert_eq!(m.dependencies.components, vec!["sec-core"]);
    }

    #[test]
    fn runtime_deps_parse_system_package_with_packages_map() {
        let toml_text = r#"
            [component]
            name = "ws-ckpt"
            version = "0.3.3"

            [[component.dependencies]]
            name = "btrfs-progs"
            kind = "system-package"
            probe = "btrfs version"
            packages = { rpm = "btrfs-progs", deb = "btrfs-progs" }
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.runtime_deps.len(), 1);
        let d = &m.runtime_deps[0];
        assert_eq!(d.name, "btrfs-progs");
        assert_eq!(d.kind, DependencyKind::SystemPackage);
        assert_eq!(d.probe.as_deref(), Some("btrfs version"));
        assert_eq!(d.packages.rpm.as_deref(), Some("btrfs-progs"));
        assert_eq!(d.packages.deb.as_deref(), Some("btrfs-progs"));
    }

    #[test]
    fn runtime_deps_parse_platform_capability_check_and_min_kernel() {
        let toml_text = r#"
            [component]
            name = "ws-ckpt"
            version = "0.3.3"

            [[component.dependencies]]
            name = "btrfs"
            kind = "platform-capability"
            check = "btrfs"
            min_kernel = "5.4"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.runtime_deps.len(), 1);
        let d = &m.runtime_deps[0];
        assert_eq!(d.kind, DependencyKind::PlatformCapability);
        assert_eq!(d.check.as_deref(), Some("btrfs"));
        assert_eq!(d.min_kernel.as_deref(), Some("5.4"));
        assert!(d.packages.is_empty());
    }

    #[test]
    fn runtime_deps_parse_language_runtime_with_version_and_source() {
        let toml_text = r#"
            [component]
            name = "cosh"
            version = "1.0.0"

            [[component.dependencies]]
            name = "node"
            kind = "language-runtime"
            version = ">=20"
            probe = "node --version"
            source = "nodejs-official"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let d = &m.runtime_deps[0];
        assert_eq!(d.kind, DependencyKind::LanguageRuntime);
        assert_eq!(d.version.as_deref(), Some(">=20"));
        assert_eq!(d.source.as_deref(), Some("nodejs-official"));
    }

    #[test]
    fn runtime_deps_kind_kebab_case_roundtrip() {
        // The wire spelling is kebab-case; serializing back must preserve it so
        // a contract round-trips byte-stably.
        let dep = RuntimeDependency {
            name: "node".into(),
            kind: DependencyKind::LanguageRuntime,
            version: None,
            probe: None,
            source: None,
            packages: PackageNames::default(),
            check: None,
            min_kernel: None,
        };
        let text = toml::to_string(&dep).expect("serialize");
        assert!(
            text.contains("kind = \"language-runtime\""),
            "kebab-case kind expected, got: {text}"
        );
    }

    #[test]
    fn runtime_deps_distinct_from_flat_dependencies() {
        // The structured `[[component.dependencies]]` and the flat top-level
        // `[dependencies]` are independent fields at different TOML paths; a
        // manifest may carry both without either shadowing the other.
        let toml_text = r#"
            [component]
            name = "ws-ckpt"
            version = "0.3.3"

            [dependencies]
            runtime = ["legacy-flat"]

            [[component.dependencies]]
            name = "btrfs-progs"
            kind = "system-package"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.dependencies.runtime, vec!["legacy-flat"]);
        assert_eq!(m.runtime_deps.len(), 1);
        assert_eq!(m.runtime_deps[0].name, "btrfs-progs");
    }

    #[test]
    fn runtime_deps_empty_name_entry_dropped() {
        let toml_text = r#"
            [component]
            name = "ws-ckpt"
            version = "0.3.3"

            [[component.dependencies]]
            name = ""
            kind = "system-package"

            [[component.dependencies]]
            name = "btrfs-progs"
            kind = "system-package"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.runtime_deps.len(), 1);
        assert_eq!(m.runtime_deps[0].name, "btrfs-progs");
    }

    #[test]
    fn adapters_preserve_framework_kind_plugin_source_dest_detect() {
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"

            [[adapters]]
            framework = "openclaw"
            kind = "third-party"
            plugin_id = "agentsight-openclaw"
            source = "adapters/agentsight/openclaw"
            dest = "{datadir}/adapters/{component}/openclaw/"

            [adapters.detect]
            config_path = "~/.openclaw/config.toml"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.adapters.len(), 1);
        let a = &m.adapters[0];
        assert_eq!(a.framework.as_deref(), Some("openclaw"));
        assert_eq!(a.kind.as_deref(), Some("third-party"));
        assert_eq!(a.plugin_id.as_deref(), Some("agentsight-openclaw"));
        assert_eq!(a.source.as_deref(), Some("adapters/agentsight/openclaw"));
        assert_eq!(
            a.dest.as_deref(),
            Some("{datadir}/adapters/{component}/openclaw/")
        );
        assert_eq!(
            a.detect.get("config_path").and_then(|v| v.as_str()),
            Some("~/.openclaw/config.toml")
        );
    }

    #[test]
    fn layout_file_kind_parses_and_defaults_to_data() {
        let toml_text = r#"
            [component]
            name = "tokenless"
            version = "0.5.0"

            [component.layout]
            [[component.layout.files]]
            source = "bin/tokenless"
            target = "{bindir}/tokenless"
            type = "executable"

            [[component.layout.files]]
            source = "share/data.bin"
            target = "{sharedir}/tokenless/data.bin"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.install.files.len(), 2);
        assert_eq!(m.install.files[0].kind, FileKind::Executable);
        // No `type` key → defaults to Data.
        assert_eq!(m.install.files[1].kind, FileKind::Data);
    }

    /// `type = "symlink"` parses in both schemas: minimal layout files and
    /// legacy `[[install.files]]` (catalog manifests).
    #[test]
    fn symlink_file_kind_parses_in_both_schemas() {
        let minimal = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "tokenless"
            version = "0.5.0"

            [component.layout]
            [[component.layout.files]]
            source = "{libexecdir}/tokenless/rtk"
            target = "{bindir}/rtk"
            type = "symlink"
        "#,
        )
        .expect("parse minimal");
        assert_eq!(minimal.install.files[0].kind, FileKind::Symlink);

        let legacy = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "tokenless"
            version = "0.5.0"

            [install]
            modes = ["user"]

            [[install.files]]
            source = "{libexecdir}/tokenless/rtk"
            dest = "{bindir}/rtk"
            type = "symlink"
        "#,
        )
        .expect("parse legacy");
        assert_eq!(legacy.install.files[0].kind, FileKind::Symlink);
    }

    #[test]
    fn layout_files_target_aliases_dest() {
        // `target` (minimal) and `dest` (legacy) land in the same internal
        // field, so both spellings install to the same place.
        let m = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "t"
            version = "1.0.0"
            [component.layout]
            [[component.layout.files]]
            source = "bin/t"
            dest = "{bindir}/t"
        "#,
        )
        .expect("parse");
        assert_eq!(m.install.files[0].dest.as_deref(), Some("{bindir}/t"));
    }

    #[test]
    fn layout_files_render_parses_and_round_trips() {
        // Minimal-schema `render` reaches InstallFileSpec verbatim and
        // survives serialization, so a snapshotted contract keeps the
        // rendering request; entries without it stay `None` and omit the
        // key on round-trip.
        let m = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "sec-core"
            version = "0.9.0"
            [component.layout]
            [[component.layout.files]]
            source = "share/agent-sec-core.service.in"
            target = "{userunitdir}/agent-sec-core.service"
            render = "anolisa-paths-v1"
            [[component.layout.files]]
            source = "bin/agent-sec-cli"
            target = "{bindir}/agent-sec-cli"
        "#,
        )
        .expect("parse");
        assert_eq!(
            m.install.files[0].render.as_deref(),
            Some(RENDER_ANOLISA_PATHS_V1)
        );
        assert_eq!(m.install.files[1].render, None);

        let serialized = toml::to_string_pretty(&m).expect("serialize");
        let m2 = ComponentManifest::from_toml_str(&serialized).expect("re-parse");
        assert_eq!(
            m.install.files, m2.install.files,
            "round-trip must preserve render"
        );
    }

    #[test]
    fn legacy_install_files_carry_render_too() {
        // The legacy `[install]` path must not silently drop a declared
        // render — that would install literal placeholders — and serialized
        // manifests round-trip through this shape.
        let m = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "t"
            version = "1.0.0"
            [install]
            modes = ["user"]
            [[install.files]]
            source = "share/unit.in"
            dest = "{userunitdir}/unit.service"
            render = "anolisa-paths-v1"
        "#,
        )
        .expect("parse");
        assert_eq!(
            m.install.files[0].render.as_deref(),
            Some(RENDER_ANOLISA_PATHS_V1)
        );
    }

    #[test]
    fn health_spec_uses_declared_check_when_present() {
        let m = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "tokenless"
            version = "0.5.0"
            [component.health_check]
            type = "binary_version"
            binary = "{bindir}/tokenless"
        "#,
        )
        .expect("parse");
        match m.health_spec() {
            Some(CheckSpec::BinaryVersion { binary, .. }) => {
                assert_eq!(binary, "{bindir}/tokenless");
            }
            other => panic!("expected declared binary_version, got {other:?}"),
        }
    }

    #[test]
    fn health_spec_synthesizes_binary_version_from_first_executable() {
        let m = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "tokenless"
            version = "0.5.0"
            [component.layout]
            [[component.layout.files]]
            source = "share/x"
            target = "{sharedir}/x"
            type = "data"
            [[component.layout.files]]
            source = "bin/tokenless"
            target = "{bindir}/tokenless"
            type = "executable"
        "#,
        )
        .expect("parse");
        match m.health_spec() {
            Some(CheckSpec::BinaryVersion { binary, .. }) => {
                assert_eq!(binary, "{bindir}/tokenless", "first executable wins");
            }
            other => panic!("expected synthesized binary_version, got {other:?}"),
        }
    }

    #[test]
    fn health_spec_is_none_without_check_or_executable() {
        let m = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "t"
            version = "1.0.0"
            [component.layout]
            [[component.layout.files]]
            source = "share/x"
            target = "{sharedir}/x"
            type = "data"
        "#,
        )
        .expect("parse");
        assert!(m.health_spec().is_none());
    }

    /// The retired `[[health_checks]]` vocabulary must not break parsing:
    /// manifests written against the legacy schema still load, the legacy
    /// entries are simply ignored (structured health comes only from
    /// `[component.health_check]`).
    #[test]
    fn legacy_health_checks_sections_are_ignored() {
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"

            [[health_checks]]
            name = "binary"
            kind = "command"
            command = "{bindir}/agentsight --help"

            [[health_checks]]
            name = "service"
            kind = "systemd"
            unit = "agentsight.service"
            optional = true
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("legacy manifest still parses");
        assert_eq!(m.component.name, "agentsight");
        assert!(
            m.health_spec().is_none(),
            "legacy checks must not synthesize a structured spec"
        );
    }

    #[test]
    fn adapter_new_fields_parse_full_example() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"

            [[adapters]]
            framework = "openclaw"
            name = "sec-core-openclaw"
            display_name = "Sec Core for OpenClaw"
            adapter_type = "plugin"
            trust = "first-party"
            plugin_id = "sec-core"
            source = "adapters/openclaw"
            dest = "{datadir}/adapters/{component}/openclaw/"

            [adapters.bundle]
            schema = 1
            entry = "plugin.json"

            [adapters.compat]
            driver_schema = 1
            framework_version = ">=0.1.0"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.adapters.len(), 1);
        let a = &m.adapters[0];
        assert_eq!(a.name.as_deref(), Some("sec-core-openclaw"));
        assert_eq!(a.display_name.as_deref(), Some("Sec Core for OpenClaw"));
        assert_eq!(a.framework.as_deref(), Some("openclaw"));
        assert_eq!(a.adapter_type.as_deref(), Some("plugin"));
        assert_eq!(a.trust.as_deref(), Some("first-party"));
        assert_eq!(a.plugin_id.as_deref(), Some("sec-core"));
        assert_eq!(a.source.as_deref(), Some("adapters/openclaw"));
        assert_eq!(
            a.dest.as_deref(),
            Some("{datadir}/adapters/{component}/openclaw/")
        );
        assert_eq!(a.bundle.schema, Some(1));
        assert_eq!(a.bundle.entry.as_deref(), Some("plugin.json"));
        assert_eq!(a.compat.driver_schema, Some(1));
        assert_eq!(a.compat.framework_version.as_deref(), Some(">=0.1.0"));
    }

    #[test]
    fn adapter_new_fields_round_trip() {
        let toml_text = r#"
            [component]
            name = "roundtrip"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"
            name = "rt-adapter"
            display_name = "RT Adapter"
            adapter_type = "extension"
            trust = "third-party"
            plugin_id = "rt"
            source = "adapters/openclaw"
            dest = "{datadir}/adapters/{component}/openclaw/"

            [adapters.bundle]
            schema = 2
            entry = "manifest.json"

            [adapters.compat]
            driver_schema = 3
            framework_version = ">=1.0.0"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        let m2 = ComponentManifest::from_toml_str(&serialized).expect("re-parse");
        assert_eq!(
            m.adapters, m2.adapters,
            "round-trip must preserve all adapter fields"
        );
    }

    #[test]
    fn adapter_minimal_fields_still_parse() {
        let toml_text = r#"
            [component]
            name = "minimal"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"
            source = "adapters/openclaw"
            dest = "{datadir}/adapters/{component}/openclaw/"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.adapters.len(), 1);
        let a = &m.adapters[0];
        assert_eq!(a.framework.as_deref(), Some("openclaw"));
        assert!(a.display_name.is_none());
        assert!(a.adapter_type.is_none());
        assert!(a.trust.is_none());
        assert!(a.bundle.is_empty());
        assert!(a.compat.is_empty());
    }

    #[test]
    fn adapter_empty_adapters_array_still_parses() {
        let toml_text = r#"
            [component]
            name = "no-adapters"
            version = "1.0.0"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert!(m.adapters.is_empty());
    }

    #[test]
    fn adapter_new_fields_default_to_none_when_absent() {
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"

            [[adapters]]
            framework = "openclaw"
            kind = "third-party"
            plugin_id = "agentsight-openclaw"
            source = "adapters/agentsight/openclaw"
            dest = "{datadir}/adapters/{component}/openclaw/"

            [adapters.detect]
            config_path = "~/.openclaw/config.toml"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let a = &m.adapters[0];
        assert!(a.display_name.is_none());
        assert!(a.adapter_type.is_none());
        assert!(a.trust.is_none());
        assert!(a.bundle.schema.is_none());
        assert!(a.bundle.entry.is_none());
        assert!(a.compat.driver_schema.is_none());
        assert!(a.compat.framework_version.is_none());
        // Existing fields preserved.
        assert_eq!(a.kind.as_deref(), Some("third-party"));
        assert_eq!(
            a.detect.get("config_path").and_then(|v| v.as_str()),
            Some("~/.openclaw/config.toml")
        );
    }

    #[test]
    fn adapter_bundle_and_compat_skip_serializing_when_empty() {
        let toml_text = r#"
            [component]
            name = "skiptest"
            version = "1.0.0"

            [[adapters]]
            framework = "cosh"
            source = "adapters/cosh"
            dest = "{datadir}/adapters/{component}/cosh/"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        assert!(
            !serialized.contains("[bundle]"),
            "empty bundle must be skipped in serialization"
        );
        assert!(
            !serialized.contains("[compat]"),
            "empty compat must be skipped in serialization"
        );
        assert!(
            !serialized.contains("[adapters.backends]"),
            "empty backends must be skipped in serialization"
        );
    }

    #[test]
    fn adapter_backends_rpm_resource_root_parses_and_round_trips() {
        // `[adapters.backends.rpm]` declares the read-only resource root an
        // RPM install provides; it must parse alongside the raw `dest` and
        // survive round-trip so snapshotted contracts keep both roots.
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.9.0"

            [[adapters]]
            framework = "openclaw"
            source = "adapters/sec-core/openclaw"
            dest = "{datadir}/adapters/{component}/openclaw/"

            [adapters.backends.rpm]
            resource_root = "/opt/agent-sec/openclaw-plugin/"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let a = &m.adapters[0];
        assert_eq!(
            a.dest.as_deref(),
            Some("{datadir}/adapters/{component}/openclaw/")
        );
        assert_eq!(
            a.backends
                .rpm
                .as_ref()
                .and_then(|rpm| rpm.resource_root.as_deref()),
            Some("/opt/agent-sec/openclaw-plugin/")
        );

        let serialized = toml::to_string_pretty(&m).expect("serialize");
        let m2 = ComponentManifest::from_toml_str(&serialized).expect("re-parse");
        assert_eq!(
            m.adapters, m2.adapters,
            "round-trip must preserve backend resource roots"
        );
    }

    #[test]
    fn component_domain_distinguishes_unset_from_explicit_empty() {
        let unset = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "unset-domain"
            version = "1.0.0"
        "#,
        )
        .expect("parse unset");
        assert_eq!(unset.component.domain, None);

        let explicit_empty = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "empty-domain"
            version = "1.0.0"
            domain = ""
        "#,
        )
        .expect("parse explicit empty");
        assert_eq!(explicit_empty.component.domain.as_deref(), Some(""));
    }

    #[test]
    fn adapter_skills_and_config_parse() {
        let toml_text = r#"
            [component]
            name = "agent-sec-core"
            version = "2.1.0"

            [[adapters]]
            framework = "openclaw"
            adapter_type = "plugin"
            plugin_id = "agent-sec"
            skills = ["sec-audit", "cred-scan"]
            framework_version_req = ">=1.2"

            [adapters.bundle]
            entry = "openclaw.plugin.json"

            [[adapters.config]]
            key = "plugins.entries.agent-sec.hooks.allowConversationAccess"
            value = true
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.adapters.len(), 1);
        let a = &m.adapters[0];
        assert_eq!(a.skills, skill_names(&["sec-audit", "cred-scan"]));
        assert_eq!(a.framework_version_req.as_deref(), Some(">=1.2"));
        assert_eq!(a.config.len(), 1);
        assert_eq!(
            a.config[0].key,
            "plugins.entries.agent-sec.hooks.allowConversationAccess"
        );
        assert_eq!(a.config[0].value, toml::Value::Boolean(true));
    }

    #[test]
    fn adapter_openclaw_specific_section_parses() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"

            [[adapters]]
            framework = "openclaw"
            plugin_id = "sec-core"

            [adapters.openclaw]
            skills = ["sec-audit"]

            [adapters.openclaw.bundle]
            entry = "openclaw.plugin.json"

            [[adapters.openclaw.config]]
            key = "plugins.entries.sec-core.enabled"
            value = true
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let a = &m.adapters[0];
        let oc = a.openclaw.as_ref().expect("openclaw section");
        assert_eq!(oc.skills, skill_names(&["sec-audit"]));
        assert_eq!(oc.bundle.entry.as_deref(), Some("openclaw.plugin.json"));
        assert_eq!(oc.config.len(), 1);
        assert_eq!(oc.config[0].key, "plugins.entries.sec-core.enabled");
    }

    #[test]
    fn adapter_hermes_specific_section_parses() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"

            [[adapters]]
            framework = "hermes"
            skills = ["sec-audit"]

            [adapters.hermes]
            skills = ["sec-audit"]

            [adapters.hermes.bundle]
            entry = "hermes.manifest.yaml"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let a = &m.adapters[0];
        let h = a.hermes.as_ref().expect("hermes section");
        assert_eq!(h.skills, skill_names(&["sec-audit"]));
        assert_eq!(h.bundle.entry.as_deref(), Some("hermes.manifest.yaml"));
    }

    #[test]
    fn adapter_skills_config_round_trip() {
        let toml_text = r#"
            [component]
            name = "roundtrip"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"
            plugin_id = "rt"
            skills = ["skill-a", "skill-b"]
            framework_version_req = ">=1.0"

            [[adapters.config]]
            key = "some.key"
            value = "hello"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        let m2 = ComponentManifest::from_toml_str(&serialized).expect("re-parse");
        assert_eq!(m.adapters[0].skills, m2.adapters[0].skills);
        assert_eq!(m.adapters[0].config, m2.adapters[0].config);
        assert_eq!(
            m.adapters[0].framework_version_req,
            m2.adapters[0].framework_version_req
        );
    }

    #[test]
    fn adapter_config_framework_version_parses_and_round_trips() {
        // The issue's suggested component shape: a per-config framework
        // version condition alongside the adapter-level compat requirement.
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"

            [[adapters]]
            framework = "openclaw"
            plugin_id = "agent-sec"

            [adapters.compat]
            framework_version = ">=2026.4.14"

            [[adapters.openclaw.config]]
            key = "plugins.entries.agent-sec.hooks.allowConversationAccess"
            value = true
            framework_version = ">=2026.4.24"

            [[adapters.openclaw.config]]
            key = "plugins.entries.agent-sec.enabled"
            value = true
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let a = &m.adapters[0];
        assert_eq!(a.compat.framework_version.as_deref(), Some(">=2026.4.14"));
        let oc = a.openclaw.as_ref().expect("openclaw section");
        assert_eq!(oc.config.len(), 2);
        assert_eq!(
            oc.config[0].framework_version.as_deref(),
            Some(">=2026.4.24")
        );
        // A config entry with no condition means "always apply".
        assert!(oc.config[1].framework_version.is_none());

        // The condition must survive a serialize→parse round-trip, and an
        // absent condition must not be emitted (old manifests stay stable).
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        let m2 = ComponentManifest::from_toml_str(&serialized).expect("re-parse");
        assert_eq!(m.adapters, m2.adapters);
    }

    #[test]
    fn adapter_config_without_framework_version_skips_serializing_field() {
        let toml_text = r#"
            [component]
            name = "skiptest"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.config]]
            key = "some.key"
            value = "hello"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        assert!(
            !serialized.contains("framework_version"),
            "absent per-config framework_version must be skipped: {serialized}"
        );
    }

    #[test]
    fn adapter_empty_skills_config_skip_serializing() {
        let toml_text = r#"
            [component]
            name = "skiptest"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        assert!(
            !serialized.contains("skills"),
            "empty skills must be skipped: {serialized}"
        );
        assert!(
            !serialized.contains("config"),
            "empty config must be skipped: {serialized}"
        );
        assert!(
            !serialized.contains("[openclaw]"),
            "absent openclaw section must be skipped: {serialized}"
        );
        assert!(
            !serialized.contains("[hermes]"),
            "absent hermes section must be skipped: {serialized}"
        );
    }

    #[test]
    fn adapter_notices_parse_generic() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"

            [[adapters]]
            framework = "openclaw"
            plugin_id = "sec-core"

            [[adapters.notices]]
            when = "post_enable"
            level = "info"
            text = "Restart the framework to load the plugin."
            command = "openclaw restart"

            [[adapters.notices]]
            when = "post_disable"
            level = "warning"
            text = "Cached tokens remain until the framework restarts."
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let a = &m.adapters[0];
        assert_eq!(a.notices.len(), 2);
        assert_eq!(a.notices[0].when, NoticeWhen::PostEnable);
        assert_eq!(a.notices[0].level, NoticeLevel::Info);
        assert_eq!(
            a.notices[0].text,
            "Restart the framework to load the plugin."
        );
        assert_eq!(a.notices[0].command.as_deref(), Some("openclaw restart"));
        assert_eq!(a.notices[1].when, NoticeWhen::PostDisable);
        assert_eq!(a.notices[1].level, NoticeLevel::Warning);
        assert!(a.notices[1].command.is_none());
    }

    #[test]
    fn adapter_notice_level_defaults_to_info() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.notices]]
            when = "post_enable"
            text = "No level declared."
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(m.adapters[0].notices[0].level, NoticeLevel::Info);
    }

    #[test]
    fn adapter_notices_parse_framework_specific_sections() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.openclaw.notices]]
            when = "post_enable"
            text = "OpenClaw-specific notice."

            [[adapters]]
            framework = "hermes"

            [[adapters.hermes.notices]]
            when = "post_disable"
            level = "warning"
            text = "Hermes-specific notice."
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let oc = m.adapters[0].openclaw.as_ref().expect("openclaw section");
        assert_eq!(oc.notices.len(), 1);
        assert_eq!(oc.notices[0].when, NoticeWhen::PostEnable);
        assert_eq!(oc.notices[0].text, "OpenClaw-specific notice.");
        let h = m.adapters[1].hermes.as_ref().expect("hermes section");
        assert_eq!(h.notices.len(), 1);
        assert_eq!(h.notices[0].when, NoticeWhen::PostDisable);
        assert_eq!(h.notices[0].level, NoticeLevel::Warning);
    }

    #[test]
    fn adapter_notices_round_trip() {
        let toml_text = r#"
            [component]
            name = "roundtrip"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.notices]]
            when = "post_enable"
            level = "warning"
            text = "Keep this verbatim: {datadir} $HOME `id`"
            command = "echo {datadir}/$HOME"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        let m2 = ComponentManifest::from_toml_str(&serialized).expect("re-parse");
        assert_eq!(
            m.adapters, m2.adapters,
            "round-trip must preserve notices verbatim"
        );
    }

    #[test]
    fn adapter_notices_text_is_verbatim_and_inert() {
        // Placeholders and shell metacharacters must survive parsing
        // unchanged: notices are display-only and never expanded/executed.
        let toml_text = r#"
            [component]
            name = "inert"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.notices]]
            when = "post_enable"
            text = "run {datadir}/bin/tool; rm -rf $(whoami)"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        assert_eq!(
            m.adapters[0].notices[0].text,
            "run {datadir}/bin/tool; rm -rf $(whoami)"
        );
    }

    #[test]
    fn adapter_notices_skip_serializing_when_empty() {
        let toml_text = r#"
            [component]
            name = "skiptest"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        assert!(
            !serialized.contains("notices"),
            "absent notices must be skipped: {serialized}"
        );
    }

    #[test]
    fn adapter_notice_default_level_skips_serializing_field() {
        let toml_text = r#"
            [component]
            name = "skiptest"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.notices]]
            when = "post_enable"
            text = "info-level notice"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        assert!(
            !serialized.contains("level ="),
            "default info level must be skipped: {serialized}"
        );
    }

    #[test]
    fn adapter_notice_unknown_when_rejected() {
        let toml_text = r#"
            [component]
            name = "bad"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.notices]]
            when = "post_install"
            text = "unsupported trigger"
        "#;
        assert!(
            ComponentManifest::from_toml_str(toml_text).is_err(),
            "unknown `when` must fail closed"
        );
    }

    #[test]
    fn adapter_notice_unknown_level_rejected() {
        let toml_text = r#"
            [component]
            name = "bad"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.notices]]
            when = "post_enable"
            level = "critical"
            text = "unsupported level"
        "#;
        assert!(
            ComponentManifest::from_toml_str(toml_text).is_err(),
            "unknown `level` must fail closed"
        );
    }

    #[test]
    fn adapter_notice_missing_text_rejected() {
        let toml_text = r#"
            [component]
            name = "bad"
            version = "1.0.0"

            [[adapters]]
            framework = "openclaw"

            [[adapters.notices]]
            when = "post_enable"
        "#;
        assert!(
            ComponentManifest::from_toml_str(toml_text).is_err(),
            "missing required `text` must fail"
        );
    }

    #[test]
    fn component_md_example_parses() {
        let toml_text = r#"
            schema_version = 1

            [component]
            name         = "agent-sec-core"
            version      = "2.1.0"
            display_name = "Agent Security Core"
            owner        = "security-team"
            license      = "Apache-2.0"
            repository   = "https://github.com/example/agent-sec-core"
            layer        = "runtime"
            domain       = "security"

            [component.contract]
            schema_version      = "1"
            min_anolisa_version = "0.6.0"

            [[adapters]]
            name         = "agent-sec-openclaw"
            display_name = "Agent Sec (OpenClaw)"
            framework    = "openclaw"
            kind         = "first-party"
            adapter_type = "plugin"
            plugin_id    = "agent-sec"
            skills       = ["sec-audit", "cred-scan"]
            framework_version_req = ">=1.2"

            [adapters.bundle]
            entry = "openclaw.plugin.json"

            [[adapters.config]]
            key   = "plugins.entries.agent-sec.hooks.allowConversationAccess"
            value = true

            [[adapters]]
            name         = "agent-sec-hermes"
            display_name = "Agent Sec (hermes)"
            framework    = "hermes"
            kind         = "first-party"
            skills       = ["sec-audit"]
            framework_version_req = ">=0.4"

            [adapters.bundle]
            entry = "hermes.manifest.yaml"

            [[adapters.config]]
            key   = "security.conversation_access"
            value = true
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse component.md example");
        assert_eq!(m.adapters.len(), 2);

        let oc = &m.adapters[0];
        assert_eq!(oc.framework.as_deref(), Some("openclaw"));
        assert_eq!(oc.plugin_id.as_deref(), Some("agent-sec"));
        assert_eq!(oc.skills, skill_names(&["sec-audit", "cred-scan"]));
        assert_eq!(oc.bundle.entry.as_deref(), Some("openclaw.plugin.json"));
        assert_eq!(oc.config.len(), 1);

        let hm = &m.adapters[1];
        assert_eq!(hm.framework.as_deref(), Some("hermes"));
        assert_eq!(hm.skills, skill_names(&["sec-audit"]));
        assert_eq!(hm.bundle.entry.as_deref(), Some("hermes.manifest.yaml"));
        assert_eq!(hm.config.len(), 1);
    }

    #[test]
    fn existing_manifests_still_parse_after_schema_extension() {
        let toml_text = r#"
            [component]
            name = "agentsight"
            version = "0.2.0"
            layer = "runtime"

            [[adapters]]
            framework = "openclaw"
            kind = "third-party"
            plugin_id = "agentsight-openclaw"
            source = "adapters/agentsight/openclaw"
            dest = "{datadir}/adapters/{component}/openclaw/"

            [adapters.detect]
            config_path = "~/.openclaw/config.toml"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let a = &m.adapters[0];
        assert!(a.skills.is_empty());
        assert!(a.config.is_empty());
        assert!(a.framework_version_req.is_none());
        assert!(a.openclaw.is_none());
        assert!(a.hermes.is_none());
        assert_eq!(a.plugin_id.as_deref(), Some("agentsight-openclaw"));
    }

    // -- AdapterSkillSpec dual-format ------------------------------------------

    #[test]
    fn adapter_skills_table_array_parses() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"
            layer = "runtime"

            [[adapters]]
            framework = "openclaw"

            [[adapters.openclaw.skills]]
            name = "code-scanner"
            source = "{datadir}/skills/code-scanner/"

            [[adapters.openclaw.skills]]
            name = "prompt-scanner"
            source = "{datadir}/skills/prompt-scanner/"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let oc = m.adapters[0].openclaw.as_ref().expect("openclaw section");
        assert_eq!(oc.skills.len(), 2);
        assert_eq!(oc.skills[0].name, "code-scanner");
        assert_eq!(
            oc.skills[0].source.as_deref(),
            Some("{datadir}/skills/code-scanner/")
        );
        assert_eq!(oc.skills[1].name, "prompt-scanner");
    }

    #[test]
    fn adapter_skills_string_array_still_parses() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"
            layer = "runtime"

            [[adapters]]
            framework = "openclaw"

            [adapters.openclaw]
            skills = ["code-scanner", "prompt-scanner"]
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let oc = m.adapters[0].openclaw.as_ref().expect("openclaw section");
        assert_eq!(oc.skills, skill_names(&["code-scanner", "prompt-scanner"]));
        assert!(
            oc.skills.iter().all(|s| s.source.is_none()),
            "string-form skills must have no source"
        );
    }

    #[test]
    fn adapter_skills_table_array_hermes_parses() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"
            layer = "runtime"

            [[adapters]]
            framework = "hermes"

            [[adapters.hermes.skills]]
            name = "code-scanner"
            source = "{datadir}/skills/code-scanner/"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let h = m.adapters[0].hermes.as_ref().expect("hermes section");
        assert_eq!(h.skills.len(), 1);
        assert_eq!(h.skills[0].name, "code-scanner");
        assert_eq!(
            h.skills[0].source.as_deref(),
            Some("{datadir}/skills/code-scanner/")
        );
    }

    #[test]
    fn adapter_skills_generic_table_array_parses() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"
            layer = "runtime"

            [[adapters]]
            framework = "openclaw"

            [[adapters.skills]]
            name = "code-scanner"
            source = "{datadir}/skills/code-scanner/"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let a = &m.adapters[0];
        assert_eq!(a.skills.len(), 1);
        assert_eq!(a.skills[0].name, "code-scanner");
        assert_eq!(
            a.skills[0].source.as_deref(),
            Some("{datadir}/skills/code-scanner/")
        );
    }

    #[test]
    fn adapter_skills_string_array_round_trips() {
        let toml_text = r#"
            [component]
            name = "roundtrip"
            version = "1.0.0"
            layer = "runtime"

            [[adapters]]
            framework = "openclaw"
            skills = ["a", "b"]
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        // The serializer uses a multi-line format; just check the values
        // appear and no `source` or `name` key is emitted (i.e. it
        // serialized as a string array, not a table array).
        assert!(
            !serialized.contains("source ="),
            "string-form skills must not produce 'source =' keys: {serialized}"
        );
        let m2 = ComponentManifest::from_toml_str(&serialized).expect("re-parse");
        assert_eq!(m.adapters[0].skills, m2.adapters[0].skills);
    }

    #[test]
    fn adapter_skills_table_array_serialises_with_source() {
        let toml_text = r#"
            [component]
            name = "sec-core"
            version = "0.1.0"
            layer = "runtime"

            [[adapters]]
            framework = "openclaw"

            [[adapters.skills]]
            name = "code-scanner"
            source = "{datadir}/skills/code-scanner/"
        "#;
        let m = ComponentManifest::from_toml_str(toml_text).expect("parse");
        let serialized = toml::to_string_pretty(&m).expect("serialize");
        assert!(
            serialized.contains("code-scanner"),
            "must contain skill name: {serialized}"
        );
        let m2 = ComponentManifest::from_toml_str(&serialized).expect("re-parse");
        assert_eq!(m.adapters[0].skills, m2.adapters[0].skills);
    }
}
