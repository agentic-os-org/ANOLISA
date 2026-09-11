//! Adapter manager: the trusted orchestrator that owns the
//! dangerous-resource boundary.
//!
//! The Manager is the only thing that takes the install lock, reads and
//! writes adapter receipts in `installed.toml`, re-validates every
//! [`ClaimResource`](super::claim::ClaimResource) against a driver's static
//! external roots, runs framework CLIs through a single controlled
//! [`AdapterOps`] implementation, and records to the central log. Drivers
//! own framework *semantics*; the Manager owns *trust and IO*. A driver
//! never spawns a process, deletes a path, or persists state on its own.
//!
//! Resource discovery has two modes, tried in order:
//!
//! 1. **Contract-driven** — when the installed component manifest declares
//!    an `[[adapters]]` entry with a `dest` field, that template is expanded
//!    against each visible datadir root. The first root whose expanded path
//!    exists as a directory wins. When `dest` is declared but no directory
//!    exists, enable fails with an explicit error and scan shows the adapter
//!    as declared but absent — convention discovery is **not** used as a
//!    silent fallback.
//!
//! 2. **Convention** — `{datadir}/adapters/<component>/<framework>/`.
//!    Multiple datadir roots may be searched (e.g. the user datadir
//!    preferred over the system one); the first root that contains the
//!    directory wins.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anolisa_platform::fs_layout::{FsLayout, InstallMode};
use anolisa_platform::pkg_files::PackageFileQuery;
use anolisa_platform::rpm_query::RpmPackageQuery;

use super::AdapterError;
use super::claim::{
    AdapterClaim, AdapterSourceRevision, ClaimResourceKind, ClaimStatus, DriverPayload,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterOps, AdapterStatusReport,
    AdapterSummary, CliOutput, ConditionStatus, DisableReport, DriverCtx, DriverPlan,
    EnableProgress, FrameworkCommand, FrameworkRpcSession, HostEnv,
};
use super::managed_files::{
    ManagedInventory, ManagedMatch, cleanup_replaced_materialized_files,
    inventory_for_installation, materialized_files, plan_replaced_materialized_files,
    source_revision, verify_managed_bundle, verify_materialized_bundle,
};
use super::registry::DriverRegistry;
use crate::central_log::{CentralLog, LogKind, LogRecord, LogStatus, Severity};
use crate::domain::{Installation, LifecycleStatus, NativePm, ProviderBinding};
use crate::lock::InstallLock;
use crate::manifest::ComponentManifest;
use crate::state::ObjectKind;
use crate::state_store::StateStore;

/// Per-CLI-call producer name recorded in the central log.
const LOG_SOURCE: &str = "anolisa-cli";

/// Cap on captured stdout/stderr per framework CLI invocation (bytes).
/// Output beyond this is drained (so the child never blocks on a full
/// pipe) but discarded before logging.
const OUTPUT_CAP: usize = 64 * 1024;
/// Cap on stdout for framework commands that return structured JSON.
///
/// Inventory commands need substantially more than diagnostic commands:
/// Qoder reports every plugin and its resources in one document. Four MiB
/// covers thousands of ordinary entries while retaining a finite memory
/// bound. Stderr remains subject to [`OUTPUT_CAP`].
const JSON_OUTPUT_CAP: usize = 4 * 1024 * 1024;
/// Outcome of [`AdapterManager::enable`].
#[derive(Debug, Clone)]
pub enum EnableOutcome {
    /// `--dry-run`: what enable *would* do, no state mutated.
    Planned {
        /// The driver's plan.
        plan: DriverPlan,
        /// Static `post_enable` notices a real enable would display.
        /// Preview only — nothing was executed.
        notices: Vec<crate::manifest::AdapterNotice>,
    },
    /// Enable ran; the persisted receipt. The declared notices are carried
    /// in [`AdapterClaim::notices`]; the `post_enable` subset is what a
    /// caller displays after success.
    Enabled(Box<AdapterClaim>),
}

/// Typed knobs for [`AdapterManager::enable_with_options`].
///
/// A distinct struct (rather than extra positional parameters) keeps the
/// default-safe [`AdapterManager::enable`] wrapper stable for its many
/// callers while letting a new authorization be threaded to the driver as
/// typed data — never an environment variable or free-form map.
#[derive(Debug, Clone, Default)]
pub struct EnableOptions {
    /// The caller explicitly authorized an unsafe plugin install
    /// (`--allow-unsafe-plugin-install`). Only honored for the OpenClaw
    /// plugin adapter; rejected for any other framework or a `skill_bundle`
    /// adapter. Even when set, the driver adds the framework's unsafe flag
    /// only if the host's install help exposes it.
    pub allow_unsafe_plugin_install: bool,
    /// Explicit profiles for profile-scoped framework adapters such as dsh.
    /// An empty list means no profiles were selected; profile-scoped drivers
    /// reject that input rather than silently mutating an implicit profile.
    pub profiles: Vec<String>,
}

/// Outcome of [`AdapterManager::disable`].
#[derive(Debug, Clone)]
pub struct DisableOutcome {
    /// Component the disable targeted.
    pub component: String,
    /// Resolved framework, when one was determined (`None` only for the
    /// "component has no enabled adapters" no-op).
    pub framework: Option<String>,
    /// The driver's cleanup report (or the dry-run plan).
    pub report: DisableReport,
    /// True when the receipt was removed; false when it was kept and
    /// marked `cleanup_failed` for retry. Always `false` under dry-run.
    pub claim_removed: bool,
    /// True when the operation was a dry-run (no state mutated).
    pub dry_run: bool,
    /// Static `post_disable` notices from the receipt. Populated when a
    /// disable succeeds, or as a preview under dry-run; empty for the
    /// no-op and degraded (cleanup-incomplete) outcomes. Display-only;
    /// the text is inert and never executed.
    pub notices: Vec<crate::manifest::AdapterNotice>,
}

/// One row of [`AdapterManager::scan`].
#[derive(Debug, Clone)]
pub struct ScanEntry {
    /// Component the adapter belongs to.
    pub component: String,
    /// Framework the adapter targets.
    pub framework: String,
    /// Whether the installed component manifest declares this adapter.
    pub declared: bool,
    /// Resource directory, when present under a visible datadir root.
    pub resource_root: Option<PathBuf>,
    /// Whether a built-in driver exists for `framework`.
    pub driver_available: bool,
    /// Whether the framework was detected on the host (best-effort;
    /// `false` when no driver is available to probe).
    pub framework_detected: bool,
    /// The `adapter_type` declared in the component manifest for this
    /// adapter entry, when the manifest was readable (`None` when the
    /// entry came from resource-directory discovery only).
    pub adapter_type: Option<String>,
    /// Whether a receipt for `(component, framework)` exists in state.
    pub enabled: bool,
    /// Lifecycle status of the receipt, when one exists.
    pub claim_status: Option<ClaimStatus>,
    /// Availability of the component/adapter source behind an enabled
    /// receipt. `None` means this row has no receipt, so source health is not
    /// a persisted-state concern.
    pub source_status: Option<AdapterSourceStatus>,
    /// Human-readable reason for [`Self::source_status`], present when source
    /// health is missing or otherwise needs operator explanation.
    pub source_reason: Option<String>,
}

/// Source availability for an adapter receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterSourceStatus {
    /// The component is still installed in a visible scope and its adapter
    /// resource root can be resolved.
    Available,
    /// The receipt remains, but the visible component source or adapter
    /// resource root is gone.
    Missing,
}

impl AdapterSourceStatus {
    /// Stable wire/human label for scan JSON and table output.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone)]
struct SourceProbe {
    status: AdapterSourceStatus,
    resource_root: Option<PathBuf>,
    reason: Option<String>,
    revision: Result<super::claim::AdapterSourceRevision, String>,
}

/// Full result of [`AdapterManager::scan`].
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    /// Adapter entries from manifest declarations and/or resource
    /// directories, sorted by `(component, framework)`.
    pub entries: Vec<ScanEntry>,
    /// Non-fatal manifest/state issues encountered while scanning fallback
    /// roots.
    pub warnings: Vec<String>,
}

/// One row of [`AdapterManager::status`].
#[derive(Debug, Clone)]
pub struct StatusEntry {
    /// Component the receipt belongs to.
    pub component: String,
    /// Framework the receipt targets.
    pub framework: String,
    /// The driver's status report for this receipt.
    pub report: AdapterStatusReport,
}

/// Full result of [`AdapterManager::status`].
#[derive(Debug, Clone, Default)]
pub struct StatusReport {
    /// Per-receipt status entries.
    pub entries: Vec<StatusEntry>,
}

/// Declaration state of `[adapters.backends.rpm].resource_root` for one
/// framework entry — the single source of truth for the three-valued
/// semantics (undeclared / declared-but-blank / usable). Every consumer
/// matches on it, so absent and blank can never be conflated again (a
/// blank root is a contract defect: enable rejects it and scan must not
/// fall back to the raw `dest`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RpmRootDecl {
    /// Key not declared: RPM provenance falls back to the raw `dest`.
    Absent,
    /// Declared but blank after trimming — fail closed everywhere.
    Blank,
    /// Declared with a placeholder outside the RPM vocabulary
    /// (`{datadir}`, `{component}`) — fail closed everywhere. Any other
    /// layout placeholder (`{bindir}`, `{libexecdir}`, …) expands
    /// against the *consuming* manager's layout: a user-mode manager
    /// consuming a system RPM contract would resolve it under the user
    /// prefix, misreporting the package payload as missing or — worse —
    /// selecting a caller-writable bundle in place of the package-owned
    /// one.
    Unsupported {
        /// The declared template (trimmed).
        template: String,
        /// The first offending placeholder (without braces).
        placeholder: String,
    },
    /// Declared with a usable template (trimmed, non-empty).
    Declared(String),
}

impl RpmRootDecl {
    /// Classify a raw declaration value (as read from the manifest).
    fn from_raw(raw: Option<&str>) -> Self {
        match raw {
            None => Self::Absent,
            Some(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    Self::Blank
                } else if let Some(placeholder) = unsupported_rpm_placeholder(trimmed) {
                    Self::Unsupported {
                        template: trimmed.to_string(),
                        placeholder,
                    }
                } else {
                    Self::Declared(trimmed.to_string())
                }
            }
        }
    }

    /// The usable template, when one is declared.
    fn declared(&self) -> Option<&str> {
        match self {
            Self::Declared(root) => Some(root),
            Self::Absent | Self::Blank | Self::Unsupported { .. } => None,
        }
    }
}

/// First `{placeholder}` in `template` outside the vocabulary allowed for
/// `[adapters.backends.rpm].resource_root`. An RPM payload path is
/// scope-independent by definition: absolute (the package-owned path) or
/// `{datadir}`-rooted (expanded against the datadir roots of the scope
/// that owns the contract, plus `{component}`). Every other layout
/// placeholder expands against the consuming manager's layout — the
/// wrong scope whenever the contract is consumed cross-scope. Token
/// scanning mirrors [`expand_layout_placeholders`](super::expand_layout_placeholders)
/// (unterminated braces are left for the expander to handle).
fn unsupported_rpm_placeholder(template: &str) -> Option<String> {
    let mut search_from = 0;
    while let Some(rel_open) = template[search_from..].find('{') {
        let open = search_from + rel_open;
        let Some(rel_close) = template[open..].find('}') else {
            break;
        };
        let key = &template[open + 1..open + rel_close];
        if key != "datadir" && key != "component" {
            return Some(key.to_string());
        }
        search_from = open + rel_close + 1;
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AdapterDecl {
    component: String,
    framework: String,
    /// The `adapter_type` from the manifest entry, if present.
    adapter_type: Option<String>,
    /// Raw `dest` template from the manifest entry, before placeholder
    /// expansion. Used by `scan` and `enable` to resolve the
    /// contract-driven resource root.
    dest: Option<String>,
    /// `[adapters.backends.rpm].resource_root` declaration state.
    /// Authoritative over [`dest`](Self::dest) when the component has RPM
    /// provenance — a unified contract keeps `dest` for raw installs
    /// while the RPM payload lives at a package-owned path.
    rpm_root: RpmRootDecl,
    /// Whether the component's installed record is RPM-delegated in the
    /// visible root that owns this declaration.
    rpm_provenance: bool,
    /// Bundle entry filename declared in the contract, if any. Scan uses
    /// it (like enable) to tell a real bundle directory from a stale
    /// leftover skeleton.
    bundle_entry: Option<String>,
    /// Datadir roots from the [`VisibleRoot`] where this declaration's
    /// component contract was resolved. `dest` expansion is scoped to
    /// only these roots.
    scoped_datadir_roots: Vec<PathBuf>,
}

impl AdapterDecl {
    /// Contract resource-root template effective for this declaration's
    /// install provenance: an RPM-installed component reads its bundle
    /// from the declared RPM resource root, everything else from `dest`.
    fn effective_root_template(&self) -> Option<&str> {
        if self.rpm_provenance
            && let Some(root) = self.rpm_root.declared()
        {
            return Some(root);
        }
        self.dest.as_deref()
    }

    /// The RPM backend root this provenance depends on is declared but
    /// unusable — blank, or using a placeholder outside the RPM
    /// vocabulary — the same contract defect enable rejects. Scan must
    /// report the resource as unavailable rather than fall back to
    /// `dest`.
    fn rpm_root_defective(&self) -> bool {
        self.rpm_provenance
            && matches!(
                self.rpm_root,
                RpmRootDecl::Blank | RpmRootDecl::Unsupported { .. }
            )
    }
}

/// Trust decision for the external resources of one `(component, framework)`:
/// the roots symlink targets may resolve under, plus
/// whether the two-source condition — RPM provenance recorded in state
/// **and** a contract-declared `[adapters.backends.rpm].resource_root`
/// — currently grants external-root trust. This is the single decision
/// point shared by every claim-validation site (enable prior-receipt,
/// pre-persist, apply, disable, status) and by the anchor lifecycle, so
/// no call site can fall out of sync with the rule. Derived from the
/// on-disk contract and state provenance — never from the receipt under
/// validation — so a forged receipt cannot authorize its own target.
struct ExternalRootTrust {
    /// Roots a receipt's symlink targets may resolve under. Derived from
    /// the contract only — the state anchor is deliberately not a member:
    /// roots authorize subtrees, and a state-resident value must never
    /// authorize more than itself.
    target_roots: Vec<PathBuf>,
    /// Enable-time anchor from state, honoured as an *exact-equality*
    /// symlink-target allowance (see
    /// [`AdapterClaim::validate_with_trust`]): it keeps the receipt of a
    /// since-moved or since-removed external root reportable and
    /// cleanable, while a forged anchor authorizes nothing beneath it and
    /// no write outside anolisa's own layout.
    anchor: Option<PathBuf>,
    /// The two-source condition held. The enable-time anchor is
    /// (re)written on enable exactly when this is true: a forged state
    /// entry alone must not become durable.
    anchor_eligible: bool,
}

impl ExternalRootTrust {
    /// The anchor as an exact-target allowance slice for
    /// [`AdapterClaim::validate_with_trust`].
    fn exact_targets(&self) -> &[PathBuf] {
        self.anchor.as_slice()
    }

    /// Restore a Manager-written dsh home anchor as an allowed external
    /// root. Unlike ordinary receipt data, this value was captured only
    /// after the enable-time `DSH_HOME` boundary validated, so environment
    /// drift cannot redirect later reads or cleanup commands.
    fn extend_allowed_roots(&self, framework: &str, roots: &mut Vec<PathBuf>) {
        if framework == "dsh"
            && let Some(anchor) = &self.anchor
            && !roots.contains(anchor)
        {
            roots.push(anchor.clone());
        }
    }

    /// Persist or clear the enable-time anchor under the same
    /// eligibility that governs anchor consumption — by construction the
    /// write condition and the read condition can never diverge. The
    /// anchor keeps a prior receipt validatable after an RPM update
    /// moves the contract root, so re-enable can migrate it instead of
    /// wedging on `OwnedPath`.
    ///
    /// An anchor is worth remembering only when the just-validated claim
    /// actually depends on external symlink-target trust
    /// ([`AdapterClaim::requires_external_symlink_trust`]): a receipt
    /// whose targets all re-validate from the static boundary on every
    /// run — or one with no symlink resources at all (every driver but
    /// Codex today) — never reads its anchor back, and a redundant
    /// anchor would bump the state schema to v6 for nothing, locking
    /// released 0.2.16 CLIs out of all state commands on a path that
    /// never needed trust migration.
    fn sync_anchor(
        &self,
        state: &mut StateStore,
        layout: &FsLayout,
        claim: &AdapterClaim,
        trusted_owned_roots: &[PathBuf],
    ) {
        if claim.framework == "dsh" {
            if let Some(root) = dsh_home_anchor(claim) {
                state.upsert_adapter_trust_root(
                    &claim.component,
                    &claim.framework,
                    root.to_path_buf(),
                );
            } else {
                state.remove_adapter_trust_root(&claim.component, &claim.framework);
            }
            return;
        }
        if self.anchor_eligible
            && claim.requires_external_symlink_trust(layout, trusted_owned_roots)
        {
            state.upsert_adapter_trust_root(
                &claim.component,
                &claim.framework,
                claim.resource_root.clone(),
            );
        } else {
            state.remove_adapter_trust_root(&claim.component, &claim.framework);
        }
    }
}

/// Return the already-validated dsh home resource for anchor persistence.
/// The driver's claim validation establishes the exact payload/resource
/// relationship before [`ExternalRootTrust::sync_anchor`] is called.
fn dsh_home_anchor(claim: &AdapterClaim) -> Option<&Path> {
    let DriverPayload::Dsh(payload) = &claim.driver_payload else {
        return None;
    };
    match &claim.resource(&payload.home_resource)?.kind {
        ClaimResourceKind::ExternalPath { path } => Some(path),
        _ => None,
    }
}

/// A state root paired with the datadir roots it may use for component
/// contract resolution. Contract lookup for a component found in this
/// state root searches only the paired datadir roots — not datadirs
/// from other visible roots — so a user-scope component cannot
/// silently fall back to a system-scope contract.
#[derive(Debug, Clone)]
pub struct VisibleRoot {
    /// State directory containing `installed.toml`.
    pub state_dir: PathBuf,
    /// Datadir roots searched for component contracts when a component
    /// is found in this state root. Searched in order; first match wins.
    pub contract_datadir_roots: Vec<PathBuf>,
}

/// Trusted orchestrator for adapter enable/disable/status/scan.
pub struct AdapterManager {
    layout: FsLayout,
    registry: DriverRegistry,
    state_path: PathBuf,
    /// Paired visible roots, in preference order. Each pairs a state root
    /// with its contract-visible datadir roots. Receipts are always
    /// written only to [`Self::state_path`] (the primary root's state).
    visible_roots: Vec<VisibleRoot>,
    /// All datadir roots (across all visible roots, deduped), used for
    /// resource-directory discovery (`adapters/<component>/<framework>/`).
    /// Resource discovery is scope-independent: a user-mode enable may
    /// use adapter resources from a system-installed package.
    all_datadir_roots: Vec<PathBuf>,
    /// Warnings discovered by the caller while deriving visible roots. Scan
    /// returns these with manager-local manifest/state warnings so read-only
    /// adapter surfaces do not lose StateView visibility diagnostics.
    visibility_warnings: Vec<String>,
    user_home: Option<PathBuf>,
    /// Identity recorded as the central-log actor.
    actor: String,
    /// Read-only native package file inventory. Kept separate from lifecycle
    /// version queries so adapter status can be tested without rpmdb.
    package_files: Box<dyn PackageFileQuery>,
}

impl AdapterManager {
    /// Build a manager for the given layout. The primary visible root
    /// pairs `layout.state_dir` with `layout.datadir`. Use
    /// [`Self::push_visible_root`] to add fallback roots (e.g. system
    /// roots when running in user mode).
    pub fn new(layout: FsLayout, user_home: Option<PathBuf>, actor: String) -> Self {
        let state_path = layout.state_dir.join("installed.toml");
        let primary = VisibleRoot {
            state_dir: layout.state_dir.clone(),
            contract_datadir_roots: vec![layout.datadir.clone()],
        };
        let all_datadir_roots = vec![layout.datadir.clone()];
        Self {
            layout,
            registry: DriverRegistry::builtin(),
            state_path,
            visible_roots: vec![primary],
            all_datadir_roots,
            visibility_warnings: Vec::new(),
            user_home,
            actor,
            package_files: Box::new(RpmPackageQuery::system()),
        }
    }

    /// Replace the read-only native package file query (primarily for tests).
    pub fn set_package_file_query(&mut self, query: Box<dyn PackageFileQuery>) {
        self.package_files = query;
    }

    /// Replace the visible root set. Receipts still read/write only the
    /// manager's primary state path; this controls read-only source discovery.
    pub fn set_visible_roots(&mut self, roots: Vec<VisibleRoot>) {
        if roots.is_empty() {
            return;
        }
        self.visible_roots.clear();
        self.all_datadir_roots.clear();
        for root in roots {
            self.push_visible_root(root);
        }
    }

    /// Preserve a non-fatal root-visibility warning for the next scan report.
    pub fn push_visibility_warning(&mut self, warning: String) {
        self.visibility_warnings.push(warning);
    }

    /// Add a visible root with explicit contract-scope datadir pairing.
    ///
    /// The state root is appended to the search order (lower priority
    /// than roots registered earlier). Its `contract_datadir_roots` are
    /// only used for contract resolution when a component is found in
    /// this state root — they are not mixed into other roots' contract
    /// scope.
    ///
    /// All datadir roots are also added to the global resource-discovery
    /// set (for `adapters/<component>/<framework>/` lookups), since
    /// adapter resource directories are scope-independent.
    pub fn push_visible_root(&mut self, root: VisibleRoot) {
        if self
            .visible_roots
            .iter()
            .any(|r| r.state_dir == root.state_dir)
        {
            return;
        }
        for dd in &root.contract_datadir_roots {
            if !self.all_datadir_roots.contains(dd) {
                self.all_datadir_roots.push(dd.clone());
            }
        }
        self.visible_roots.push(root);
    }

    /// Add a datadir root to the primary visible root's contract scope
    /// and to the global resource-discovery set. Use this when the
    /// system-mode packaged datadir differs from `layout.datadir`
    /// (e.g. exe-sibling `/usr/share/anolisa/` vs install prefix
    /// `/usr/local/share/anolisa/`).
    pub fn push_primary_datadir_root(&mut self, root: PathBuf) {
        if let Some(primary) = self.visible_roots.first_mut()
            && !primary.contract_datadir_roots.contains(&root)
        {
            primary.contract_datadir_roots.push(root.clone());
        }
        if !self.all_datadir_roots.contains(&root) {
            self.all_datadir_roots.push(root);
        }
    }

    /// Built-in driver registry, for callers that want to introspect
    /// supported frameworks.
    pub fn registry(&self) -> &DriverRegistry {
        &self.registry
    }

    // -- scan ---------------------------------------------------------------

    /// Load the primary root's installed state (v5, migrating legacy files
    /// at the boundary). Scope resolution follows the invoking uid, the
    /// same convention the CLI uses.
    fn load_state(&self) -> Result<StateStore, crate::state::StateError> {
        StateStore::load_for_layout(
            &self.state_path,
            anolisa_platform::privilege::effective_uid(),
            &self.layout,
        )
    }

    fn load_state_at(path: &Path) -> Result<StateStore, crate::state::StateError> {
        StateStore::load(path, anolisa_platform::privilege::effective_uid())
    }

    /// Discover adapter declarations from visible installed component
    /// manifests, merge them with resource directories under the datadir
    /// roots, then annotate each row with driver availability, framework
    /// detection, and receipt state. Read-only.
    ///
    /// # Errors
    ///
    /// [`AdapterError::State`] if the state file cannot be read.
    pub fn scan(&self) -> Result<ScanReport, AdapterError> {
        let state = self.load_state()?;
        let mut entries: BTreeMap<(String, String), ScanEntry> = BTreeMap::new();
        for (component, framework, _first_dir) in self.discover_all() {
            let (driver_available, framework_detected) = self.driver_scan_facts(&framework);
            let claim = state.find_adapter_claim(&component, &framework);
            // discover_all() only supplies the (component, framework) key:
            // its first-wins path may be a stale skeleton shadowing a real
            // bundle in a later root. Re-select across all datadir roots so
            // only a directory holding a real bundle is surfaced; a
            // key with no valid bundle anywhere shows resource absent.
            let resource_root = self
                .discover_valid_convention_root(&component, &framework, None)
                .map(|(path, _)| path);
            entries.insert(
                (component.clone(), framework.clone()),
                ScanEntry {
                    component,
                    framework,
                    declared: false,
                    resource_root,
                    driver_available,
                    framework_detected,
                    adapter_type: None,
                    enabled: claim.is_some(),
                    claim_status: claim.map(|c| c.status),
                    source_status: None,
                    source_reason: None,
                },
            );
        }

        let (declarations, declaration_warnings) = self.load_visible_adapter_declarations(&state);
        for declaration in declarations {
            let key = (declaration.component.clone(), declaration.framework.clone());
            if let Some(entry) = entries.get_mut(&key) {
                entry.declared = true;
                entry.adapter_type = declaration.adapter_type.clone();
                // When the contract declares a custom resource root for
                // this provenance (raw `dest`, or the RPM backend root for
                // an RPM-installed component), the contract-resolved path
                // is authoritative — override the convention-discovered
                // root (which may point elsewhere). If the contract
                // directory does not exist, show resource_root = None
                // (declared yes / resource absent).
                if declaration.rpm_root_defective() {
                    // Blank RPM root: enable will reject this contract, so
                    // a stale raw `dest` bundle must not be reported as a
                    // usable resource (declared yes / resource absent).
                    entry.resource_root = None;
                } else if declaration.effective_root_template().is_some() {
                    entry.resource_root = declaration.effective_root_template().and_then(|dest| {
                        self.resolve_declared_scan_root(
                            &declaration.component,
                            &declaration.framework,
                            dest,
                            declaration.bundle_entry.as_deref(),
                            &declaration.scoped_datadir_roots,
                        )
                    });
                } else {
                    // Convention adapter: always re-resolve with the
                    // contract-declared bundle entry. The native-default
                    // probe above may have accepted a root that lacks the
                    // declared entry, which enable and the source probe
                    // would reject — the declared entry is authoritative.
                    entry.resource_root = self
                        .discover_valid_convention_root(
                            &declaration.component,
                            &declaration.framework,
                            declaration.bundle_entry.as_deref(),
                        )
                        .map(|(path, _)| path);
                }
                continue;
            }

            // Not found in directory discovery — resolve from the
            // provenance-effective contract root, if declared.
            let resource_root = if declaration.rpm_root_defective() {
                None
            } else {
                declaration.effective_root_template().and_then(|dest| {
                    self.resolve_declared_scan_root(
                        &declaration.component,
                        &declaration.framework,
                        dest,
                        declaration.bundle_entry.as_deref(),
                        &declaration.scoped_datadir_roots,
                    )
                })
            };

            let (driver_available, framework_detected) =
                self.driver_scan_facts(&declaration.framework);
            let claim = state.find_adapter_claim(&declaration.component, &declaration.framework);
            entries.insert(
                key,
                ScanEntry {
                    component: declaration.component,
                    framework: declaration.framework,
                    declared: true,
                    resource_root,
                    driver_available,
                    framework_detected,
                    adapter_type: declaration.adapter_type,
                    enabled: claim.is_some(),
                    claim_status: claim.map(|c| c.status),
                    source_status: None,
                    source_reason: None,
                },
            );
        }

        for claim in &state.adapter_claims {
            let source = self.source_probe(&claim.component, &claim.framework, &state);
            let (driver_available, framework_detected) = self.driver_scan_facts(&claim.framework);
            let key = (claim.component.clone(), claim.framework.clone());
            let entry = entries.entry(key).or_insert_with(|| ScanEntry {
                component: claim.component.clone(),
                framework: claim.framework.clone(),
                declared: false,
                resource_root: source.resource_root.clone(),
                driver_available,
                framework_detected,
                adapter_type: claim.adapter_type.clone(),
                enabled: false,
                claim_status: None,
                source_status: None,
                source_reason: None,
            });
            entry.enabled = true;
            entry.claim_status = Some(claim.status);
            if entry.adapter_type.is_none() {
                entry.adapter_type = claim.adapter_type.clone();
            }
            if entry.resource_root.is_none() {
                entry.resource_root = source.resource_root.clone();
            }
            entry.source_status = Some(source.status);
            entry.source_reason = source.reason;
        }

        // A directory name alone cannot establish an unknown framework. Keep
        // unknown rows only when a contract or receipt identifies them, so
        // shared trees such as `adapters/tokenless/common` are not adapters.
        entries.retain(|_, entry| entry.declared || entry.enabled || entry.driver_available);

        let mut warnings = self.visibility_warnings.clone();
        warnings.extend(declaration_warnings);
        Ok(ScanReport {
            entries: entries.into_values().collect(),
            warnings,
        })
    }

    // -- enable -------------------------------------------------------------

    /// Enable `component`'s adapter for `framework` (resolved automatically
    /// when `None` and exactly one framework is present). When `dry_run`,
    /// returns the plan without mutating any state.
    ///
    /// Apply takes the install lock before loading state; dry-run remains
    /// read-only and does not create or acquire the lock file.
    ///
    /// # Errors
    ///
    /// [`AdapterError::ComponentNotInstalled`], [`AdapterError::AdapterNotDeclared`],
    /// [`AdapterError::AdapterManifest`], [`AdapterError::MissingAdapterManifest`],
    /// [`AdapterError::UnknownFramework`],
    /// [`AdapterError::AmbiguousFramework`], [`AdapterError::UnsupportedAdapterType`],
    /// [`AdapterError::ResourceRootNotFound`],
    /// [`AdapterError::FrameworkNotDetected`], [`AdapterError::BundleInvalid`],
    /// [`AdapterError::FrameworkCli`], [`AdapterError::ClaimValidation`],
    /// [`AdapterError::ReenableCleanupIncomplete`], or state/lock/log errors.
    pub fn enable(
        &self,
        component: &str,
        framework: Option<&str>,
        dry_run: bool,
    ) -> Result<EnableOutcome, AdapterError> {
        self.enable_with_options(component, framework, dry_run, EnableOptions::default())
    }

    /// Enable with explicit typed [`EnableOptions`]. [`Self::enable`] is the
    /// default-safe wrapper over this; callers that need to pass an explicit
    /// safety-bypass authorization use this form.
    ///
    /// # Errors
    ///
    /// Same as [`Self::enable`], plus [`AdapterError::UnsafeInstallNotApplicable`]
    /// when an unsafe-install authorization is passed for a framework or
    /// adapter type that does not support it.
    pub fn enable_with_options(
        &self,
        component: &str,
        framework: Option<&str>,
        dry_run: bool,
        options: EnableOptions,
    ) -> Result<EnableOutcome, AdapterError> {
        let _lock = if dry_run {
            None
        } else {
            Some(InstallLock::acquire(&self.layout.lock_file)?)
        };
        let mut state = self.load_state()?;

        let (manifest, scoped_datadir_roots, contract_datadir_root, rpm_provenance) =
            self.load_visible_component_manifest(component, &state)?;
        let framework = self.resolve_framework(component, framework, &manifest)?;

        // Fail-closed in two steps. First reject a value no driver
        // implements at all (`service`, typos, …).
        let adapter_type = declared_adapter_type(&manifest, &framework);
        if let Some(ref at) = adapter_type
            && !is_supported_adapter_type(at)
        {
            return Err(AdapterError::UnsupportedAdapterType {
                component: component.to_string(),
                framework: framework.clone(),
                adapter_type: at.clone(),
            });
        }
        // Then reject an implemented type used with a framework that does
        // not support it (e.g. `openclaw` + `extension`, `cosh` + `plugin`),
        // so the wrong driver code path can never run.
        validate_adapter_type_for_framework(component, &framework, adapter_type.as_deref())?;

        // An unsafe-install authorization is only meaningful for the OpenClaw
        // plugin adapter. Reject it for any other framework, and for a
        // skill_bundle adapter (which never installs a plugin), before doing
        // any work — the authorization must never silently no-op.
        if options.allow_unsafe_plugin_install
            && (framework != "openclaw" || adapter_type.as_deref() == Some("skill_bundle"))
        {
            return Err(AdapterError::UnsafeInstallNotApplicable {
                component: component.to_string(),
                framework: framework.clone(),
                adapter_type: adapter_type.clone(),
            });
        }
        if !options.profiles.is_empty() && framework != "dsh" {
            return Err(AdapterError::InvalidAdapterInput {
                component: component.to_string(),
                framework: framework.clone(),
                reason: "--profile is only valid for the dsh framework".to_string(),
            });
        }

        let declared_plugin_id = declared_plugin_id(&manifest, &framework);
        let skill_specs = declared_skills(&manifest, &framework);
        let config = declared_config(&manifest, &framework);
        let framework_version_req = declared_framework_version_req(&manifest, &framework);
        let bundle_entry = declared_bundle_entry(&manifest, &framework);
        let displaces = declared_displaces(&manifest, &framework);
        let all_notices = declared_all_notices(&manifest, &framework);
        if adapter_type.as_deref() == Some("skill_bundle") && !config.is_empty() {
            return Err(AdapterError::InvalidAdapterInput {
                component: component.to_string(),
                framework: framework.clone(),
                reason: "skill_bundle adapters do not support framework config entries".to_string(),
            });
        }
        // A skill bundle installs no plugin, so it can displace nothing: the
        // declaration would silently no-op. Reject it rather than accept a
        // contract that promises a hand-off it cannot perform.
        if adapter_type.as_deref() == Some("skill_bundle") && !displaces.is_empty() {
            return Err(AdapterError::InvalidAdapterInput {
                component: component.to_string(),
                framework: framework.clone(),
                reason: "skill_bundle adapters do not support displaced framework plugins"
                    .to_string(),
            });
        }

        for skill in &skill_specs {
            super::claim::validate_skill_name(&skill.name).map_err(|mut err| {
                if let AdapterError::InvalidAdapterInput {
                    component: ref mut c,
                    framework: ref mut f,
                    ..
                } = err
                {
                    *c = component.to_string();
                    *f = framework.clone();
                }
                err
            })?;
        }
        for cfg in &config {
            super::claim::validate_config_key(&cfg.key).map_err(|mut err| {
                if let AdapterError::InvalidAdapterInput {
                    component: ref mut c,
                    framework: ref mut f,
                    ..
                } = err
                {
                    *c = component.to_string();
                    *f = framework.clone();
                }
                err
            })?;
        }
        // Displaced plugin ids reach `plugins disable`/`plugins enable` argv,
        // and a declared slot becomes the `plugins.slots.<slot>` config key
        // read back on disable — so both go through the same whitelist
        // validation as the values they resemble, here rather than in the
        // driver, so a bad contract fails before any host probing.
        //
        // Uniqueness is part of that, and only this loop can check it: each entry
        // is well-formed on its own, and nothing downstream sees two of them at
        // once until the receipt is already on disk. A repeated id would build two
        // resources with the same id, and the receipt would pass the generic claim
        // validation, get persisted, and get its own plugin installed before
        // `claim_displaced_plugins` rejected it — leaving a `cleanup_failed`
        // receipt that neither status nor disable can consume. A repeated slot is
        // worse and stays silent: the slot is exclusive, so on disable the first
        // restore makes that plugin the slot's owner and the guard then reads it as
        // a *third* owner for the second one, stepping aside and removing the
        // receipt while the second plugin stays disabled by this adapter.
        let mut declared_displaced_ids: BTreeSet<&str> = BTreeSet::new();
        let mut declared_displaced_slots: BTreeSet<&str> = BTreeSet::new();
        for displaced in &displaces {
            let invalid = |reason: String| AdapterError::InvalidAdapterInput {
                component: component.to_string(),
                framework: framework.clone(),
                reason,
            };
            super::claim::validate_plugin_id(&displaced.id)
                .map_err(|err| invalid(format!("displaced plugin id: {err}")))?;
            if displaced.id == declared_plugin_id.as_deref().unwrap_or_default() {
                return Err(invalid(
                    "displaced plugin id must differ from the adapter's own plugin id".to_string(),
                ));
            }
            if !declared_displaced_ids.insert(displaced.id.as_str()) {
                return Err(invalid(format!(
                    "displaced plugin id '{}' is declared more than once; one adapter cannot \
                     own the same transition twice",
                    displaced.id
                )));
            }
            if let Some(slot) = displaced.slot.as_deref() {
                // `validate_config_key` only rejects an empty *whole key*, and
                // `plugins.slots.` is not empty — so an empty suffix would slip
                // through and quietly degrade this entry to a restore with no slot
                // guard. Omitting `slot` is how a contract says there is no
                // exclusive slot to guard; spelling it empty is a mistake and is
                // rejected as one.
                if slot.is_empty() {
                    return Err(invalid(format!(
                        "displaced plugin '{}' declares an empty slot; omit `slot` when there \
                         is no exclusive slot to guard rather than naming `plugins.slots.`",
                        displaced.id
                    )));
                }
                super::claim::validate_config_key(&format!("plugins.slots.{slot}"))
                    .map_err(|err| invalid(format!("displaced plugin slot '{slot}': {err}")))?;
                if !declared_displaced_slots.insert(slot) {
                    return Err(invalid(format!(
                        "displaced plugin slot '{slot}' is declared by more than one plugin; \
                         the slot is exclusive, so disable could not hand both back"
                    )));
                }
            }
        }

        let driver =
            self.registry
                .get(&framework)
                .ok_or_else(|| AdapterError::UnknownFramework {
                    framework: framework.clone(),
                })?;

        let (resource_root, effective_datadir) = self.resolve_resource_root(
            component,
            &framework,
            &manifest,
            &scoped_datadir_roots,
            contract_datadir_root.as_deref(),
            rpm_provenance,
        )?;
        // Receipt symlink targets must validate against roots derived from
        // the on-disk contract and state provenance — never from the receipt
        // itself. See [`ExternalRootTrust`] for the rule; every validation
        // site below and the anchor lifecycle share this one decision.
        let trust = self.external_root_trust(
            component,
            &manifest,
            &framework,
            &scoped_datadir_roots,
            rpm_provenance,
            &state,
        );
        let skills = resolve_skill_sources(
            skill_specs,
            &self.layout,
            &effective_datadir,
            component,
            &framework,
            &resource_root,
        )?;

        let label = format!("adapter enable {component} {framework}");
        // Two-phase ManagerOps: first build a read-only ops (no allowed
        // roots) to construct the DriverCtx needed for
        // allowed_external_roots; then rebuild with the computed roots
        // for the mutable phase.
        let probe_ops = ManagerOps::new(
            self.central_log(),
            self.actor.clone(),
            install_mode_str(self.layout.mode).to_string(),
            component.to_string(),
            label.clone(),
            vec![resource_root.clone()],
        )
        .with_invocation_logging(!dry_run);
        let probe_ctx = DriverCtx {
            component: component.to_string(),
            framework: framework.clone(),
            layout: &self.layout,
            resource_root: resource_root.clone(),
            user_home: self.user_home.clone(),
            declared_plugin_id: declared_plugin_id.clone(),
            requested_profiles: options.profiles.clone(),
            adapter_type: adapter_type.clone(),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            declared_displaces: Vec::new(),
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run,
            ops: &probe_ops,
        };
        let mut allowed_roots = driver.allowed_external_roots(&probe_ctx);
        trust.extend_allowed_roots(&framework, &mut allowed_roots);
        allowed_roots.push(resource_root.clone());
        // Skill sources that live outside the resource root (e.g.
        // `{datadir}/skills/<name>/`) must also be readable by the
        // Manager's controlled IO.
        for skill in &skills {
            if let Some(ref src) = skill.source
                && !allowed_roots.iter().any(|r| src.starts_with(r))
            {
                allowed_roots.push(src.clone());
            }
        }
        drop(probe_ctx);
        drop(probe_ops);

        let ops = ManagerOps::new(
            self.central_log(),
            self.actor.clone(),
            install_mode_str(self.layout.mode).to_string(),
            component.to_string(),
            label.clone(),
            allowed_roots,
        )
        .with_invocation_logging(!dry_run);
        let ctx = DriverCtx {
            component: component.to_string(),
            framework: framework.clone(),
            layout: &self.layout,
            resource_root: resource_root.clone(),
            user_home: self.user_home.clone(),
            declared_plugin_id,
            requested_profiles: options.profiles.clone(),
            adapter_type,
            declared_skills: skills,
            declared_config: config,
            declared_bundle_entry: bundle_entry,
            declared_displaces: displaces,
            framework_version_req,
            allow_unsafe_plugin_install: options.allow_unsafe_plugin_install,
            dry_run,
            ops: &ops,
        };

        if dry_run {
            let bundle = driver.read_bundle(&ctx)?;
            validate_displacements_not_self(&bundle, &ctx)?;
            // Validate the receipt a dry-run would replace *before* handing it to
            // `plan_enable`: a driver that carries facts across a re-enable plans
            // from that receipt, and a forged one must not shape the plan any more
            // than it may shape the real re-enable below.
            let prior = state.find_adapter_claim(component, &framework);
            if let Some(prior) = prior {
                let mut claim_allowed_roots = driver.allowed_external_roots(&ctx);
                trust.extend_allowed_roots(&framework, &mut claim_allowed_roots);
                prior.validate_with_trust(
                    &self.layout,
                    &claim_allowed_roots,
                    &trust.target_roots,
                    trust.exact_targets(),
                )?;
            }
            let mut plan = driver.plan_enable(&bundle, prior, &ctx)?;
            if let Some(prior) = prior {
                let mappings = driver.materialized_mappings(
                    &resource_root,
                    ctx.adapter_type.as_deref(),
                    &ctx.declared_skills,
                );
                let next_files = if mappings.is_empty() {
                    Vec::new()
                } else {
                    let inventory = self.managed_inventory(component, &state, &framework)?;
                    materialized_files(&inventory, &mappings).map_err(|reason| {
                        AdapterError::InvalidAdapterInput {
                            component: component.to_string(),
                            framework: framework.clone(),
                            reason,
                        }
                    })?
                };
                let next_roots = driver.materialized_destination_roots(&bundle, &ctx)?;
                // Same order the real re-enable runs them in — prune, then the
                // driver's release. A preview that lists them the other way round
                // describes a transaction the operation does not perform, which is
                // the divergence the rest of this driver's previews are built to
                // avoid.
                let mut cleanup_actions =
                    plan_replaced_materialized_files(prior, &next_files, &next_roots)?;
                cleanup_actions.extend(driver.plan_reenable_cleanup(prior, &ctx)?);
                plan.actions.splice(0..0, cleanup_actions);
            }
            let notices = declared_notices(
                &manifest,
                &framework,
                crate::manifest::NoticeWhen::PostEnable,
            );
            return Ok(EnableOutcome::Planned { plan, notices });
        }

        // enable mutates framework state, so the framework must be usable.
        let detect = driver.detect(&HostEnv {
            user_home: self.user_home.clone(),
        });
        if !detect.detected {
            return Err(AdapterError::FrameworkNotDetected {
                framework: framework.clone(),
                reason: detect.reason,
            });
        }

        // Preserve the driver's existing read-only input validation and error
        // ordering. The authoritative integrity gate runs below after all
        // pure preparation, immediately before replacement cleanup or apply
        // can mutate framework state.
        let bundle = driver.read_bundle(&ctx)?;
        validate_displacements_not_self(&bundle, &ctx)?;
        let mut claim_allowed_roots = driver.allowed_external_roots(&ctx);
        trust.extend_allowed_roots(&framework, &mut claim_allowed_roots);
        // Validate the receipt this enable would replace *before* handing it to
        // `prepare_enable`, mirroring the dry-run branch above: a driver that
        // inherits ownership across a re-enable decides there what it may later
        // re-confirm, and a forged prior must not shape the real enable any more
        // than it may shape the plan.
        let mut prior = state.find_adapter_claim(component, &framework).cloned();
        if let Some(prior) = &prior {
            // A forged prior receipt must not gain authority merely because a
            // driver preserves facts from it during re-enable.
            prior.validate_with_trust(
                &self.layout,
                &claim_allowed_roots,
                &trust.target_roots,
                trust.exact_targets(),
            )?;
        }
        let (mut claim, prepared) = driver.prepare_enable(&bundle, prior.as_ref(), &ctx)?;
        claim.bundle_digest = None;
        // Persist the manifest's static notices in the receipt so a later
        // disable can show `post_disable` notices from the receipt alone.
        // Inert text — never expanded or executed.
        claim.notices = all_notices;
        if let Some(prior) = &prior {
            driver.preserve_reenable_facts(prior, &mut claim, &ctx)?;
        }
        let mappings = driver.materialized_mappings(
            &resource_root,
            ctx.adapter_type.as_deref(),
            &ctx.declared_skills,
        );
        let managed_inventory = self.managed_inventory(component, &state, &framework)?;
        let revision =
            source_revision(&managed_inventory, &resource_root, &mappings).map_err(|reason| {
                AdapterError::InvalidAdapterInput {
                    component: component.to_string(),
                    framework: framework.clone(),
                    reason,
                }
            })?;
        claim.source_revision = Some(revision.clone());
        if !mappings.is_empty() {
            claim.materialized_files =
                materialized_files(&managed_inventory, &mappings).map_err(|reason| {
                    AdapterError::InvalidAdapterInput {
                        component: component.to_string(),
                        framework: framework.clone(),
                        reason,
                    }
                })?;
        }
        // Defense in depth: the driver must not emit a claim that points
        // outside its own declared roots. Reject before persisting.
        claim.validate_with_trust(
            &self.layout,
            &claim_allowed_roots,
            &trust.target_roots,
            trust.exact_targets(),
        )?;
        driver.validate_prepared_enable(&claim)?;
        match verify_managed_bundle(&revision) {
            ManagedMatch::Matched => {}
            ManagedMatch::Changed(reason) | ManagedMatch::Unknown(reason) => {
                return Err(AdapterError::InvalidAdapterInput {
                    component: component.to_string(),
                    framework: framework.clone(),
                    reason,
                });
            }
        }

        if let Some(prior) = &mut prior {
            // Prune stale materialized output *before* the driver releases
            // anything on the host, so no fallible step sits between that release
            // and the receipt swap below.
            //
            // The ordering is pinned from both ends. The driver's cleanup must run
            // while the prior receipt is still the durable one, because a restore
            // that fails has to stay retryable from it — swapping first would
            // strand a plugin nobody owns any more. And once a restore has
            // *succeeded*, the swap must be the very next durable action, because
            // the prior receipt is then the only record of an ownership that no
            // longer exists: keep it and a working adapter reports a tool-name
            // collision it does not have, and a later disable re-enables a plugin
            // the operator closed themselves.
            //
            // This prune used to sit between the two, and it is not exotic — it
            // removes a stale materialized *directory* with a non-recursive
            // `remove_dir`, so a single runtime-created file inside it is enough
            // to fail the re-enable after `plugins enable` has already run. The
            // two steps are independent: this one only touches ANOLISA's own
            // materialized output under the datadir, the driver's only runs
            // framework CLI verbs. So moving it changes nothing about what either
            // does, only what a failure of each leaves behind — a prune failure
            // now happens with the host untouched, and a restore failure still
            // happens with the prune done and the prior receipt intact, as before.
            //
            // What remains is a `state.save` failure right after a successful
            // restore. That window is not closable from here: recording the
            // release *before* performing it would strand the host if the process
            // died in between, which this codebase consistently ranks as the worse
            // error. It is the same residual window `apply_displacements` accepts
            // around its own write-ahead mark.
            cleanup_replaced_materialized_files(prior, &claim, &ops).map_err(|err| {
                AdapterError::ReenableCleanupIncomplete {
                    component: component.to_string(),
                    framework: framework.clone(),
                    reason: format!("failed to prune stale materialized output: {err}"),
                }
            })?;
            // Do not overwrite the only durable ownership record until the
            // driver has released resources the replacement cannot describe.
            // A failed cleanup leaves the validated prior receipt untouched,
            // so disable or a later re-enable can retry safely.
            let pristine_prior = prior.clone();
            let report = match driver.cleanup_replaced_claim(prior, &claim, &ctx) {
                Ok(report) => report,
                Err(err) => {
                    // An error is not the same as "the driver got nowhere": it
                    // can arrive from the middle of its restore loop, after
                    // earlier entries were handed back on the host and struck
                    // from `prior`. Propagating it bare would drop exactly those
                    // mutations and leave the durable receipt claiming ownership
                    // that no longer exists, which the retry then performs a
                    // second time. So record what the driver released before
                    // failing — and only what it released: an error before its
                    // first mutation must leave the prior receipt byte-for-byte
                    // as it was.
                    if *prior != pristine_prior {
                        state.upsert_adapter_claim(prior.clone());
                        if let Err(save_err) = state.save(&self.state_path) {
                            self.log_operation(
                                &label,
                                component,
                                LogStatus::Partial,
                                "re-enable cleanup failed; released-ownership update not persisted",
                                Some(format!(
                                    "cleanup failed ({err}); failed to record the ownership the \
                                     driver released: {save_err}"
                                )),
                            );
                        }
                    }
                    return Err(err);
                }
            };
            if !report.cleanup_complete {
                // The driver may already have handed some of this receipt's
                // ownership back on the host before cleanup failed, and it says so
                // the only way it can — by striking those entries from `prior`.
                // Persist that before failing, or the receipt kept for the retry
                // still claims a transition that has been undone, and the retry
                // performs it a second time over whatever the operator did in
                // between. Nothing else about the prior receipt is touched, so the
                // retry stays as safe as it was.
                state.upsert_adapter_claim(prior.clone());
                if let Err(save_err) = state.save(&self.state_path) {
                    self.log_operation(
                        &label,
                        component,
                        LogStatus::Partial,
                        "re-enable cleanup incomplete; released-ownership update not persisted",
                        Some(format!(
                            "cleanup incomplete; failed to record the ownership the driver \
                             released: {save_err}"
                        )),
                    );
                }
                let reason = if report.messages.is_empty() {
                    "driver reported incomplete cleanup without details".to_string()
                } else {
                    report.messages.join("; ")
                };
                return Err(AdapterError::ReenableCleanupIncomplete {
                    component: component.to_string(),
                    framework: framework.clone(),
                    reason,
                });
            }
        }

        state.upsert_adapter_claim(claim.clone());
        // Anchor lifecycle shares the trust decision above: it is recorded
        // exactly when the validated claim depends on an externally-targeted
        // symlink, anything else clears a stale anchor.
        trust.sync_anchor(&mut state, &self.layout, &claim, &self.all_datadir_roots);
        state.save(&self.state_path)?;
        let apply_result = {
            let mut progress = ManagerEnableProgress {
                state: &mut state,
                state_path: &self.state_path,
                layout: &self.layout,
                allowed_external_roots: &claim_allowed_roots,
                extra_owned_roots: &trust.target_roots,
                exact_symlink_targets: trust.exact_targets(),
            };
            driver.apply_enable(&mut claim, &prepared, &ctx, &mut progress)
        };
        if let Err(err) = apply_result {
            claim.status = ClaimStatus::CleanupFailed;
            state.upsert_adapter_claim(claim.clone());
            if let Err(save_err) = state.save(&self.state_path) {
                self.log_operation(
                    &label,
                    component,
                    LogStatus::Partial,
                    "adapter enable failed; receipt status update failed",
                    Some(format!(
                        "enable error: {err}; failed to mark receipt cleanup_failed: {save_err}"
                    )),
                );
            } else {
                self.log_operation(
                    &label,
                    component,
                    LogStatus::Failed,
                    "adapter enable failed; receipt kept for cleanup retry",
                    Some(err.to_string()),
                );
            }
            return Err(err);
        }
        claim.validate_with_trust(
            &self.layout,
            &claim_allowed_roots,
            &trust.target_roots,
            trust.exact_targets(),
        )?;
        state.upsert_adapter_claim(claim.clone());
        state.save(&self.state_path)?;
        self.log_operation(&label, component, LogStatus::Ok, "adapter enabled", None);

        Ok(EnableOutcome::Enabled(Box::new(claim)))
    }

    // -- disable ------------------------------------------------------------

    /// Disable `component`'s adapter for `framework` (resolved from existing
    /// receipts when `None`). Idempotent: disabling something with no
    /// receipt is a successful no-op.
    ///
    /// When `dry_run`, resolves the receipt and validates it, then returns a
    /// descriptive plan without mutating framework state, adapter receipts,
    /// or `installed.toml`.
    ///
    /// Apply takes the install lock before loading state; dry-run remains
    /// read-only and does not create or acquire the lock file.
    ///
    /// # Errors
    ///
    /// [`AdapterError::AmbiguousFramework`] when `framework` is omitted and
    /// the component has receipts for more than one; [`AdapterError::UnknownFramework`],
    /// [`AdapterError::ClaimValidation`], [`AdapterError::FrameworkCli`], or
    /// state/lock/log errors.
    pub fn disable(
        &self,
        component: &str,
        framework: Option<&str>,
        dry_run: bool,
    ) -> Result<DisableOutcome, AdapterError> {
        let _lock = if dry_run {
            None
        } else {
            Some(InstallLock::acquire(&self.layout.lock_file)?)
        };
        let mut state = self.load_state()?;

        let framework = match framework {
            Some(f) => f.to_string(),
            None => {
                let claimed: Vec<String> = state
                    .adapter_claims_for_component(component)
                    .iter()
                    .map(|c| c.framework.clone())
                    .collect();
                match claimed.len() {
                    0 => {
                        return Ok(DisableOutcome {
                            component: component.to_string(),
                            framework: None,
                            report: DisableReport {
                                cleanup_complete: true,
                                messages: vec![format!(
                                    "component '{component}' has no enabled adapters"
                                )],
                            },
                            claim_removed: false,
                            dry_run,
                            notices: Vec::new(),
                        });
                    }
                    1 => claimed[0].clone(),
                    _ => {
                        return Err(AdapterError::AmbiguousFramework {
                            component: component.to_string(),
                            frameworks: claimed,
                        });
                    }
                }
            }
        };

        let mut claim = match state.find_adapter_claim(component, &framework) {
            Some(c) => c.clone(),
            None => {
                // Idempotent: nothing to disable.
                return Ok(DisableOutcome {
                    component: component.to_string(),
                    framework: Some(framework.clone()),
                    report: DisableReport {
                        cleanup_complete: true,
                        messages: vec![format!(
                            "no receipt for '{component}/{framework}'; nothing to disable"
                        )],
                    },
                    claim_removed: false,
                    dry_run,
                    notices: Vec::new(),
                });
            }
        };

        let driver =
            self.registry
                .get(&framework)
                .ok_or_else(|| AdapterError::UnknownFramework {
                    framework: framework.clone(),
                })?;

        // resource_root may be gone after an uninstall of the bundle; that
        // is fine — disable must not depend on it. Fall back to the
        // receipt's recorded root for context only.
        let resource_root = self
            .discover_resource_root(component, &framework)
            .map(|(path, _)| path)
            .unwrap_or_else(|| claim.resource_root.clone());
        let trust = self.external_root_trust_from_state(component, &framework, &state);

        let label = format!("adapter disable {component} {framework}");
        let probe_ops = ManagerOps::new(
            self.central_log(),
            self.actor.clone(),
            install_mode_str(self.layout.mode).to_string(),
            component.to_string(),
            label.clone(),
            vec![resource_root.clone()],
        )
        .with_invocation_logging(!dry_run);
        let probe_ctx = DriverCtx {
            component: component.to_string(),
            framework: framework.clone(),
            layout: &self.layout,
            resource_root: resource_root.clone(),
            user_home: self.user_home.clone(),
            declared_plugin_id: None,
            requested_profiles: Vec::new(),
            adapter_type: claim.adapter_type.clone(),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            declared_displaces: Vec::new(),
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run,
            ops: &probe_ops,
        };
        let mut allowed_roots = driver.allowed_external_roots(&probe_ctx);
        trust.extend_allowed_roots(&framework, &mut allowed_roots);
        allowed_roots.push(resource_root.clone());
        drop(probe_ctx);
        drop(probe_ops);

        let ops = ManagerOps::new(
            self.central_log(),
            self.actor.clone(),
            install_mode_str(self.layout.mode).to_string(),
            component.to_string(),
            label.clone(),
            allowed_roots,
        )
        .with_invocation_logging(!dry_run);
        let ctx = DriverCtx {
            component: component.to_string(),
            framework: framework.clone(),
            layout: &self.layout,
            resource_root,
            user_home: self.user_home.clone(),
            declared_plugin_id: None,
            requested_profiles: Vec::new(),
            adapter_type: claim.adapter_type.clone(),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            declared_displaces: Vec::new(),
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run,
            ops: &ops,
        };

        // Re-validate the receipt before acting on it (forged-state guard).
        let mut claim_allowed_roots = driver.allowed_external_roots(&ctx);
        trust.extend_allowed_roots(&framework, &mut claim_allowed_roots);
        claim.validate_with_trust(
            &self.layout,
            &claim_allowed_roots,
            &trust.target_roots,
            trust.exact_targets(),
        )?;

        // The generic pass above checks paths and trust, not the driver's own
        // payload cross-references. Ask the driver before branching, so a receipt
        // it cannot consume is rejected identically by `disable --dry-run` and by
        // the real disable — a plan that succeeds for a receipt the operation
        // will refuse misreports whether the disable is available at all.
        driver.validate_claim(&claim)?;

        if dry_run {
            // The driver describes its own restores: only it knows which of the
            // claimed resources it would hand back, and which host state would
            // veto that. `None` keeps the generic wording for a driver that
            // describes nothing.
            let restores = driver.plan_disable_restores(&claim, &ctx)?;
            let report = plan_disable_report(
                &claim,
                if restores.is_empty() {
                    None
                } else {
                    Some(&restores)
                },
            );
            let notices = post_disable_notices(&claim);
            return Ok(DisableOutcome {
                component: component.to_string(),
                framework: Some(framework),
                report,
                claim_removed: false,
                dry_run: true,
                notices,
            });
        }

        // Mutable because a driver whose cleanup can partially fail strikes the
        // ownership it has already handed back out of the receipt as it goes; the
        // `else` branch below persists that mutated claim, which is what keeps a
        // kept-for-retry receipt describing the host as it actually is.
        //
        // The error path needs the same persistence, and used to skip it: a bare
        // `?` returns before either branch runs, so a driver that errored out
        // *after* handing ownership back — a CLI that stops being spawnable
        // halfway through the restore loop, say — left those releases in memory
        // only and the receipt on disk still naming them. The retry then re-enables
        // a plugin it already gave back, over whatever the operator did since.
        let pristine = claim.clone();
        let report = match driver.disable(&mut claim, &ctx) {
            Ok(report) => report,
            Err(err) => {
                // Only when the driver actually changed something: an error
                // before its first mutation must leave the receipt exactly as it
                // was, status included, so a disable that got nowhere keeps
                // behaving as if this branch did not exist.
                if claim != pristine {
                    claim.status = ClaimStatus::CleanupFailed;
                    state.upsert_adapter_claim(claim);
                    if let Err(save_err) = state.save(&self.state_path) {
                        self.log_operation(
                            &label,
                            component,
                            LogStatus::Partial,
                            "adapter disable failed; released-ownership update not persisted",
                            Some(format!(
                                "disable failed ({err}); failed to record the ownership the \
                                 driver released: {save_err}"
                            )),
                        );
                    } else {
                        self.log_operation(
                            &label,
                            component,
                            LogStatus::Failed,
                            "adapter disable failed; released ownership recorded, receipt kept",
                            Some(err.to_string()),
                        );
                    }
                }
                return Err(err);
            }
        };
        let claim_removed = report.cleanup_complete;
        // Extract before the branch below moves `claim` into the kept receipt.
        let disable_notices = post_disable_notices(&claim);
        if claim_removed {
            state.remove_adapter_claim(component, &framework);
            // The anchor exists to validate this receipt; drop it with it.
            state.remove_adapter_trust_root(component, &framework);
            self.log_operation(&label, component, LogStatus::Ok, "adapter disabled", None);
        } else {
            // Keep the receipt so cleanup can be retried; mark it failed.
            let mut kept = claim;
            kept.status = ClaimStatus::CleanupFailed;
            state.upsert_adapter_claim(kept);
            self.log_operation(
                &label,
                component,
                LogStatus::Failed,
                "adapter cleanup incomplete; receipt kept",
                Some(report.messages.join("; ")),
            );
        }
        state.save(&self.state_path)?;

        Ok(DisableOutcome {
            component: component.to_string(),
            framework: Some(framework),
            report,
            claim_removed,
            dry_run: false,
            // Notices are shown only on a successful disable; a degraded
            // (cleanup-incomplete) disable shows the retry path instead.
            notices: if claim_removed {
                disable_notices
            } else {
                Vec::new()
            },
        })
    }

    // -- status -------------------------------------------------------------

    /// Report status for every receipt, or only those of `component` when
    /// given. Read-only; never mutates state.
    ///
    /// # Errors
    ///
    /// [`AdapterError::ClaimValidation`] if a stored receipt fails
    /// re-validation, or state errors. A missing driver or undetectable
    /// framework is reported in the per-entry conditions, not as an error.
    pub fn status(&self, component: Option<&str>) -> Result<StatusReport, AdapterError> {
        let state = self.load_state()?;
        let mut entries = Vec::new();

        for claim in &state.adapter_claims {
            if let Some(c) = component
                && claim.component != c
            {
                continue;
            }
            let framework = claim.framework.clone();
            let source = self.source_probe(&claim.component, &claim.framework, &state);
            let driver = match self.registry.get(&framework) {
                Some(d) => d,
                None => {
                    // No driver: cannot verify. Surface an unverified report
                    // rather than skipping the receipt silently.
                    entries.push(StatusEntry {
                        component: claim.component.clone(),
                        framework,
                        report: with_managed_conditions(
                            unverified_report("no built-in driver for framework"),
                            claim,
                            &source,
                            false,
                        ),
                    });
                    continue;
                }
            };

            let resource_root = source
                .resource_root
                .clone()
                .or_else(|| {
                    self.discover_resource_root(&claim.component, &framework)
                        .map(|(path, _)| path)
                })
                .unwrap_or_else(|| claim.resource_root.clone());
            let trust = self.external_root_trust_from_state(&claim.component, &framework, &state);
            let label = format!("adapter status {} {framework}", claim.component);
            // Two-phase ops mirroring enable/disable: probe to learn the
            // driver's external roots, then rebuild so a driver that verifies
            // read-only state through the controlled IO boundary (e.g. Qoder
            // reading ~/.qoder/settings.json) is not confined to the resource
            // root. status remains read-only by trait contract.
            let probe_ops = ManagerOps::new(
                self.central_log(),
                self.actor.clone(),
                install_mode_str(self.layout.mode).to_string(),
                claim.component.clone(),
                label.clone(),
                vec![resource_root.clone()],
            );
            let probe_ctx = DriverCtx {
                component: claim.component.clone(),
                framework: framework.clone(),
                layout: &self.layout,
                resource_root: resource_root.clone(),
                user_home: self.user_home.clone(),
                declared_plugin_id: None,
                requested_profiles: Vec::new(),
                adapter_type: claim.adapter_type.clone(),
                declared_skills: Vec::new(),
                declared_config: Vec::new(),
                declared_bundle_entry: None,
                declared_displaces: Vec::new(),
                framework_version_req: None,
                allow_unsafe_plugin_install: false,
                dry_run: false,
                ops: &probe_ops,
            };
            let mut allowed_roots = driver.allowed_external_roots(&probe_ctx);
            trust.extend_allowed_roots(&framework, &mut allowed_roots);
            allowed_roots.push(resource_root.clone());
            drop(probe_ctx);
            drop(probe_ops);

            let ops = ManagerOps::new(
                self.central_log(),
                self.actor.clone(),
                install_mode_str(self.layout.mode).to_string(),
                claim.component.clone(),
                label,
                allowed_roots,
            );
            let ctx = DriverCtx {
                component: claim.component.clone(),
                framework: framework.clone(),
                layout: &self.layout,
                resource_root,
                user_home: self.user_home.clone(),
                declared_plugin_id: None,
                requested_profiles: Vec::new(),
                adapter_type: claim.adapter_type.clone(),
                declared_skills: Vec::new(),
                declared_config: Vec::new(),
                declared_bundle_entry: None,
                declared_displaces: Vec::new(),
                framework_version_req: None,
                allow_unsafe_plugin_install: false,
                dry_run: false,
                ops: &ops,
            };

            let mut claim_allowed_roots = driver.allowed_external_roots(&ctx);
            trust.extend_allowed_roots(&framework, &mut claim_allowed_roots);
            claim.validate_with_trust(
                &self.layout,
                &claim_allowed_roots,
                &trust.target_roots,
                trust.exact_targets(),
            )?;
            let report = with_managed_conditions(
                driver.status(claim, &ctx)?,
                claim,
                &source,
                driver.materialized_verification_applicable(claim),
            );
            entries.push(StatusEntry {
                component: claim.component.clone(),
                framework,
                report,
            });
        }

        Ok(StatusReport { entries })
    }

    /// Compare one receipt with the same authoritative source revision used by
    /// [`Self::status`]. Component update uses this to avoid a second drift
    /// definition.
    pub fn source_revision_match(
        &self,
        claim: &AdapterClaim,
        current_state: &StateStore,
    ) -> ManagedMatch {
        let source = self.source_probe(&claim.component, &claim.framework, current_state);
        match source.revision {
            Ok(current) => super::managed_files::compare_source_revision(claim, &current),
            Err(reason) => ManagedMatch::Unknown(reason),
        }
    }

    /// Capture every declared adapter's authoritative source revision.
    ///
    /// Missing package metadata is represented as `None` for that framework
    /// so update reporting cannot mistake an unverifiable source for an
    /// unchanged one.
    pub fn source_revision_snapshot(
        &self,
        component: &str,
        current_state: &StateStore,
    ) -> BTreeMap<String, Option<AdapterSourceRevision>> {
        let Ok((manifest, _, _, _)) =
            self.load_visible_component_manifest(component, current_state)
        else {
            return BTreeMap::new();
        };
        declared_frameworks(&manifest)
            .into_iter()
            .map(|framework| {
                let revision = self
                    .source_probe(component, &framework, current_state)
                    .revision
                    .ok();
                (framework, revision)
            })
            .collect()
    }

    // -- discovery helpers --------------------------------------------------

    fn driver_scan_facts(&self, framework: &str) -> (bool, bool) {
        let driver = self.registry.get(framework);
        let driver_available = driver.is_some();
        let framework_detected = driver
            .map(|d| {
                d.detect(&HostEnv {
                    user_home: self.user_home.clone(),
                })
                .detected
            })
            .unwrap_or(false);
        (driver_available, framework_detected)
    }

    fn source_probe(
        &self,
        component: &str,
        framework: &str,
        current_state: &StateStore,
    ) -> SourceProbe {
        let (vr, rpm) = match self.find_component_visible_root(component, current_state) {
            Ok(Some((vr, rpm_provenance))) => (vr, rpm_provenance),
            Ok(None) => {
                return source_missing(format!(
                    "no visible installed component '{component}' supplies this adapter"
                ));
            }
            Err(err) => {
                return source_missing(format!(
                    "failed to inspect visible component source for '{component}': {err}"
                ));
            }
        };

        let resolved = match super::contract::resolve_component_contract_with_source(
            component,
            std::slice::from_ref(&vr.state_dir),
            &vr.contract_datadir_roots,
        ) {
            Ok(resolved) => resolved,
            Err(err) => {
                return source_missing(format!(
                    "component contract unavailable for '{component}': {err}"
                ));
            }
        };
        let manifest = resolved.manifest;
        if manifest.component.name != component {
            return source_missing(format!(
                "component contract declares '{}', expected '{component}'",
                manifest.component.name
            ));
        }
        if !declared_frameworks(&manifest)
            .iter()
            .any(|fw| fw == framework)
        {
            return source_missing(format!(
                "component '{component}' no longer declares adapter framework '{framework}'"
            ));
        }

        let contract_datadir_root = contract_datadir_root_from_source(
            component,
            &resolved.path,
            &vr.contract_datadir_roots,
        );
        match self.resolve_resource_root(
            component,
            framework,
            &manifest,
            &vr.contract_datadir_roots,
            contract_datadir_root.as_deref(),
            rpm,
        ) {
            // resolve prefers a root with a valid bundle, so a winner that
            // still fails the bundle check means every candidate is a stale
            // leftover (e.g. the empty skeleton an uninstalled scope left
            // behind). Report Missing rather than letting a hollow
            // directory masquerade as a live source.
            Ok((resource_root, effective_datadir))
                if self.bundle_root_valid(
                    framework,
                    declared_bundle_entry(&manifest, framework).as_deref(),
                    &resource_root,
                ) =>
            {
                let revision = (|| {
                    let mappings = if let Some(driver) = self.registry.get(framework) {
                        let skills = resolve_skill_sources(
                            declared_skills(&manifest, framework),
                            &self.layout,
                            &effective_datadir,
                            component,
                            framework,
                            &resource_root,
                        )
                        .map_err(|err| format!("adapter skill sources are invalid: {err}"))?;
                        driver.materialized_mappings(
                            &resource_root,
                            declared_adapter_type(&manifest, framework).as_deref(),
                            &skills,
                        )
                    } else {
                        Vec::new()
                    };
                    self.current_source_revision(
                        component,
                        current_state,
                        &resource_root,
                        &mappings,
                    )
                })();
                SourceProbe {
                    status: AdapterSourceStatus::Available,
                    resource_root: Some(resource_root.clone()),
                    reason: None,
                    revision,
                }
            }
            Ok((resource_root, _)) => source_missing(format!(
                "adapter source for '{component}/{framework}' at {} is not a valid bundle (bundle marker missing)",
                resource_root.display()
            )),
            Err(err) => source_missing(format!(
                "adapter source unavailable for '{component}/{framework}': {err}"
            )),
        }
    }

    /// Resolve the framework for an operation from the installed manifest:
    /// use the explicit one when declared, else the single declared
    /// framework, else error.
    fn resolve_framework(
        &self,
        component: &str,
        framework: Option<&str>,
        manifest: &ComponentManifest,
    ) -> Result<String, AdapterError> {
        let declared = declared_frameworks(manifest);
        if let Some(f) = framework {
            if declared.iter().any(|decl| decl == f) {
                return Ok(f.to_string());
            }
            return Err(AdapterError::AdapterNotDeclared {
                component: component.to_string(),
                framework: f.to_string(),
            });
        }
        match declared.len() {
            0 => Err(AdapterError::AdapterNotDeclared {
                component: component.to_string(),
                framework: "<any>".to_string(),
            }),
            1 => Ok(declared[0].clone()),
            _ => Err(AdapterError::AmbiguousFramework {
                component: component.to_string(),
                frameworks: declared,
            }),
        }
    }

    /// Load the component contract for an installed component and return
    /// the matched visible root's contract datadir roots plus the datadir
    /// root that actually supplied the contract, when the winning contract
    /// came from a datadir path rather than a state snapshot.
    ///
    /// The component must be recorded as installed in a visible state root.
    /// Once that gate passes, the contract is resolved using only the
    /// matched visible root's paired state and datadir roots — a user-scope
    /// component never falls back to a system-scope contract.
    ///
    /// The returned datadir roots should be used to scope layout placeholder
    /// expansion for `dest` fields in the manifest.
    fn load_visible_component_manifest(
        &self,
        component: &str,
        current_state: &StateStore,
    ) -> Result<(ComponentManifest, Vec<PathBuf>, Option<PathBuf>, bool), AdapterError> {
        let (vr, rpm_provenance) = self
            .find_component_visible_root(component, current_state)?
            .ok_or_else(|| AdapterError::ComponentNotInstalled {
                component: component.to_string(),
            })?;

        let resolved = super::contract::resolve_component_contract_with_source(
            component,
            std::slice::from_ref(&vr.state_dir),
            &vr.contract_datadir_roots,
        )
        .map_err(|err| map_contract_error(component, err))?;
        let contract_datadir_root = contract_datadir_root_from_source(
            component,
            &resolved.path,
            &vr.contract_datadir_roots,
        );
        let manifest = resolved.manifest;

        if manifest.component.name != component {
            return Err(AdapterError::AdapterManifest {
                component: component.to_string(),
                path: PathBuf::new(),
                reason: format!("manifest declares component '{}'", manifest.component.name),
            });
        }
        Ok((
            manifest,
            vr.contract_datadir_roots.clone(),
            contract_datadir_root,
            rpm_provenance,
        ))
    }

    /// First visible root whose installed state contains `component` in
    /// an adapter-visible status ([`LifecycleStatus::Installed`]).
    /// Returns the full
    /// [`VisibleRoot`] so callers can scope contract resolution to the
    /// paired datadir roots, along with whether the record has RPM
    /// provenance (delegated to the native RPM manager) — resource-root
    /// resolution selects the backend-specific contract root from it.
    fn find_component_visible_root(
        &self,
        component: &str,
        current_state: &StateStore,
    ) -> Result<Option<(&VisibleRoot, bool)>, AdapterError> {
        for vr in &self.visible_roots {
            let found = if vr.state_dir == self.layout.state_dir {
                current_state
                    .find(ObjectKind::Component, component)
                    .filter(|i| is_adapter_visible(i))
                    .map(rpm_provenance)
            } else {
                let state_path = vr.state_dir.join("installed.toml");
                Self::load_state_at(&state_path)?
                    .find(ObjectKind::Component, component)
                    .filter(|i| is_adapter_visible(i))
                    .map(rpm_provenance)
            };
            if let Some(rpm) = found {
                return Ok(Some((vr, rpm)));
            }
        }
        Ok(None)
    }

    fn find_component_installation(
        &self,
        component: &str,
        current_state: &StateStore,
    ) -> Result<Option<Installation>, AdapterError> {
        for vr in &self.visible_roots {
            let found = if vr.state_dir == self.layout.state_dir {
                current_state
                    .find(ObjectKind::Component, component)
                    .filter(|installation| is_adapter_visible(installation))
                    .cloned()
            } else {
                let state_path = vr.state_dir.join("installed.toml");
                Self::load_state_at(&state_path)?
                    .find(ObjectKind::Component, component)
                    .filter(|installation| is_adapter_visible(installation))
                    .cloned()
            };
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    fn managed_inventory(
        &self,
        component: &str,
        current_state: &StateStore,
        framework: &str,
    ) -> Result<ManagedInventory, AdapterError> {
        let installation = self
            .find_component_installation(component, current_state)?
            .ok_or_else(|| AdapterError::ComponentNotInstalled {
                component: component.to_string(),
            })?;
        inventory_for_installation(&installation, self.package_files.as_ref()).map_err(|reason| {
            AdapterError::InvalidAdapterInput {
                component: component.to_string(),
                framework: framework.to_string(),
                reason,
            }
        })
    }

    fn current_source_revision(
        &self,
        component: &str,
        current_state: &StateStore,
        resource_root: &Path,
        mappings: &[super::managed_files::MaterializedMapping],
    ) -> Result<super::claim::AdapterSourceRevision, String> {
        let installation = self
            .find_component_installation(component, current_state)
            .map_err(|err| format!("installed component state unavailable: {err}"))?
            .ok_or_else(|| format!("component '{component}' is not installed"))?;
        let inventory = inventory_for_installation(&installation, self.package_files.as_ref())?;
        source_revision(&inventory, resource_root, mappings)
    }

    /// Adapter declarations from component contracts visible to the
    /// manager. Uses the same scope-paired contract resolution as `enable`
    /// so scan and enable agree.
    ///
    /// When a component appears in multiple visible roots (e.g. user and
    /// system), only the first (highest-priority) root owns the
    /// resolution — its paired state snapshot and datadir roots are
    /// searched. A lower-priority root's contract is never used as a
    /// fallback.
    fn load_visible_adapter_declarations(
        &self,
        current_state: &StateStore,
    ) -> (Vec<AdapterDecl>, Vec<String>) {
        let mut declarations = BTreeSet::new();
        // Map component name → the VisibleRoot where it was first seen,
        // plus the RPM provenance of its installed record there.
        let mut component_vr: BTreeMap<String, (&VisibleRoot, bool)> = BTreeMap::new();
        let mut warnings = Vec::new();

        for vr in &self.visible_roots {
            let state_path = vr.state_dir.join("installed.toml");
            let state = if vr.state_dir == self.layout.state_dir {
                current_state.clone()
            } else {
                match Self::load_state_at(&state_path) {
                    Ok(state) => state,
                    Err(err) => {
                        warnings.push(format!(
                            "failed to load installed state at {}: {err}",
                            state_path.display()
                        ));
                        continue;
                    }
                }
            };

            for object in state
                .installations
                .iter()
                .filter(|object| object.kind == ObjectKind::Component)
                .filter(|object| is_adapter_visible(object))
            {
                component_vr
                    .entry(object.name.clone())
                    .or_insert((vr, rpm_provenance(object)));
            }
        }

        for (component, (vr, rpm)) in &component_vr {
            let resolved = match super::contract::resolve_component_contract_with_source(
                component,
                std::slice::from_ref(&vr.state_dir),
                &vr.contract_datadir_roots,
            ) {
                Ok(r) => r,
                Err(super::contract::ContractError::Unavailable { .. }) => {
                    let other_scope_exists = self.visible_roots.iter().any(|other| {
                        other.state_dir != vr.state_dir
                            && super::contract::resolve_component_contract(
                                component,
                                std::slice::from_ref(&other.state_dir),
                                &other.contract_datadir_roots,
                            )
                            .is_ok()
                    });
                    if other_scope_exists {
                        warnings.push(format!(
                            "installed component '{component}' has no component contract in its scope; a contract exists in another scope but was not used because the component is scoped to {}", vr.state_dir.display()
                        ));
                    } else {
                        warnings.push(format!(
                            "installed component '{component}' has no component contract; adapter declarations unavailable"
                        ));
                    }
                    continue;
                }
                Err(err) => {
                    warnings.push(format!(
                        "failed to read component contract for '{component}': {err}"
                    ));
                    continue;
                }
            };
            let manifest = resolved.manifest;
            if manifest.component.name != component.as_str() {
                warnings.push(format!(
                    "component contract for '{component}' declares component '{}', expected '{component}'",
                    manifest.component.name,
                ));
                continue;
            }

            let contract_origin = contract_datadir_root_from_source(
                component,
                &resolved.path,
                &vr.contract_datadir_roots,
            );
            let scoped_roots =
                prioritize_datadir_root(&vr.contract_datadir_roots, contract_origin.as_deref());

            for adapter in &manifest.adapters {
                if let Some(framework) = adapter.framework.as_deref().map(str::trim)
                    && !framework.is_empty()
                {
                    declarations.insert(AdapterDecl {
                        component: component.clone(),
                        framework: framework.to_string(),
                        adapter_type: adapter.adapter_type.clone(),
                        dest: adapter
                            .dest
                            .as_deref()
                            .map(str::trim)
                            .filter(|d| !d.is_empty())
                            .map(str::to_string),
                        rpm_root: RpmRootDecl::from_raw(
                            adapter
                                .backends
                                .rpm
                                .as_ref()
                                .and_then(|rpm| rpm.resource_root.as_deref()),
                        ),
                        rpm_provenance: *rpm,
                        bundle_entry: declared_bundle_entry(&manifest, framework),
                        scoped_datadir_roots: scoped_roots.clone(),
                    });
                }
            }
        }

        (declarations.into_iter().collect(), warnings)
    }

    /// Expand a `dest` template from a component contract against a
    /// specific datadir root. The template may use layout placeholders
    /// (`{datadir}`, `{etcdir}`, …) and the extra variable `{component}`.
    ///
    /// The expansion must be absolute: layout placeholders always expand
    /// to absolute directories, so a relative result means the template
    /// was a bare relative path — probing it would resolve against the
    /// process CWD, letting whoever controls the working directory decide
    /// which bundle is read. Rejected before any filesystem access.
    fn expand_dest_template(
        &self,
        dest_template: &str,
        component: &str,
        datadir: &Path,
    ) -> Result<PathBuf, AdapterError> {
        let mut layout = self.layout.clone();
        layout.datadir = datadir.to_path_buf();
        let path =
            super::expand_layout_placeholders(dest_template, &layout, &[("component", component)])?;
        if !path.is_absolute() {
            return Err(AdapterError::RelativeTemplateExpansion {
                template: dest_template.to_string(),
                path,
            });
        }
        Ok(path)
    }

    /// Build the [`ExternalRootTrust`] decision from an already-loaded
    /// contract. The trusted set is the shared datadir roots, extended —
    /// only under the two-source condition — with the contract's expanded
    /// RPM root and the receipt's enable-time anchor.
    fn external_root_trust(
        &self,
        component: &str,
        manifest: &ComponentManifest,
        framework: &str,
        scoped_datadir_roots: &[PathBuf],
        rpm_provenance: bool,
        state: &StateStore,
    ) -> ExternalRootTrust {
        let mut target_roots = self.all_datadir_roots.clone();
        // The anchor is read regardless of the two-source outcome: as an
        // exact-equality allowance it is what keeps a stale external-root
        // receipt reportable and cleanable after the RPM (or the whole
        // contract) went away — while `anchor_eligible` below still gates
        // whether enable may persist one.
        let anchor = state
            .find_adapter_trust_root(component, framework)
            .map(Path::to_path_buf);
        let template = match (rpm_provenance, rpm_root_decl(manifest, framework)) {
            (true, RpmRootDecl::Declared(template)) => template,
            // No RPM provenance, or no usable declared root (absent,
            // blank, or an unsupported placeholder): validation stays
            // exactly as strict as before the unified contract, and
            // enable will clear any stale anchor.
            _ => {
                return ExternalRootTrust {
                    target_roots,
                    anchor,
                    anchor_eligible: false,
                };
            }
        };
        // `{datadir}` templates expand under already-trusted roots; absolute
        // RPM roots (the normal case, e.g. /opt/…) expand identically for
        // every datadir and dedupe to one extra entry.
        for datadir in scoped_datadir_roots
            .iter()
            .chain(std::iter::once(&self.layout.datadir))
        {
            if let Ok(path) = self.expand_dest_template(&template, component, datadir)
                && !target_roots.contains(&path)
            {
                target_roots.push(path);
            }
        }
        ExternalRootTrust {
            target_roots,
            anchor,
            anchor_eligible: true,
        }
    }

    /// [`Self::external_root_trust`] for receipt-only flows
    /// (disable/status) that have not already loaded the component
    /// manifest. When the contract is no longer visible (e.g. the
    /// component was uninstalled before disable), the roots fall back to
    /// the shared datadirs and only the exact-equality anchor allowance
    /// survives — enough to report and clean up the stale receipt, while
    /// a state file alone still cannot authorize any subtree or any
    /// write outside anolisa's layout.
    fn external_root_trust_from_state(
        &self,
        component: &str,
        framework: &str,
        state: &StateStore,
    ) -> ExternalRootTrust {
        match self.load_visible_component_manifest(component, state) {
            Ok((manifest, scoped_datadir_roots, _contract_datadir_root, rpm_provenance)) => self
                .external_root_trust(
                    component,
                    &manifest,
                    framework,
                    &scoped_datadir_roots,
                    rpm_provenance,
                    state,
                ),
            Err(_) => ExternalRootTrust {
                target_roots: self.all_datadir_roots.clone(),
                // The contract is gone, but the receipt may still point at
                // an anchored external root; surfacing the anchor as an
                // exact allowance is what lets status report it and
                // disable clean it up instead of wedging on
                // `ClaimValidation` forever.
                anchor: state
                    .find_adapter_trust_root(component, framework)
                    .map(Path::to_path_buf),
                anchor_eligible: false,
            },
        }
    }

    /// Resolve the adapter resource root for a component/framework using
    /// the contract `dest` field first, then the convention discovery
    /// path as fallback.
    ///
    /// `scoped_datadir_roots` are the datadir roots from the
    /// [`VisibleRoot`] that owns this component's contract. Only these
    /// roots are searched for contract-driven `dest` expansion — this
    /// prevents a user-scope component from silently discovering a
    /// system-scope resource (or vice-versa).
    ///
    /// Returns `(resource_root, effective_datadir)`. The
    /// `effective_datadir` is the datadir root whose `{datadir}`
    /// expansion produced the winning path — callers should use it for
    /// further placeholder expansion (skill sources) so `{datadir}`
    /// stays consistent across the component's scope.
    ///
    /// Contract-driven resolution (`dest` present):
    /// - Expands the `dest` template against each scoped datadir root.
    /// - Returns the first expanded path that exists as a directory.
    /// - When no expanded path exists, returns
    ///   [`AdapterError::ContractResourceRootNotFound`].
    ///
    /// Backend-aware selection: when `rpm_provenance` is true and the
    /// contract declares `[adapters.backends.rpm].resource_root`, that
    /// template replaces `dest` — an RPM payload lives at the
    /// package-owned path (possibly outside every datadir root, e.g.
    /// `/opt/…`), which the raw `dest` cannot describe. The declared RPM
    /// root is as authoritative as an explicit `dest`: a missing directory
    /// is [`AdapterError::ContractResourceRootNotFound`], never a silent
    /// fallback to the raw dest or convention discovery.
    ///
    /// Convention fallback (no effective template):
    /// - Searches `{datadir}/adapters/<component>/<framework>/` across
    ///   **all** datadir roots via [`Self::discover_resource_root`].
    ///   The `effective_datadir` is `self.layout.datadir` (the primary
    ///   root) since convention discovery is scope-independent.
    fn resolve_resource_root(
        &self,
        component: &str,
        framework: &str,
        manifest: &ComponentManifest,
        scoped_datadir_roots: &[PathBuf],
        contract_datadir_root: Option<&Path>,
        rpm_provenance: bool,
    ) -> Result<(PathBuf, PathBuf), AdapterError> {
        let dest_template = if rpm_provenance {
            match rpm_root_decl(manifest, framework) {
                // A declared-but-blank RPM root is a contract defect, not
                // an undeclared backend: silently selecting the raw `dest`
                // would point enable at a bundle the RPM never laid down.
                // Fail closed (mirrors the empty-`render` rejection in raw
                // install).
                RpmRootDecl::Blank => {
                    return Err(AdapterError::InvalidAdapterInput {
                        component: component.to_string(),
                        framework: framework.to_string(),
                        reason: "[adapters.backends.rpm].resource_root is declared but empty; \
                                 declare the RPM bundle path or remove the key"
                            .to_string(),
                    });
                }
                // A placeholder outside the RPM vocabulary would expand
                // against the consuming manager's layout — the wrong scope
                // whenever the contract is consumed cross-scope (e.g. a
                // user-mode manager consuming a system RPM contract). Fail
                // closed instead of probing a wrong-scope path.
                RpmRootDecl::Unsupported {
                    template,
                    placeholder,
                } => {
                    return Err(AdapterError::InvalidAdapterInput {
                        component: component.to_string(),
                        framework: framework.to_string(),
                        reason: format!(
                            "[adapters.backends.rpm].resource_root \"{template}\" uses \
                             placeholder '{{{placeholder}}}'; an RPM root must be an absolute \
                             path or a {{datadir}}-rooted template (plus {{component}}) — \
                             other layout placeholders would resolve against the consuming \
                             scope's layout, not the contract's"
                        ),
                    });
                }
                RpmRootDecl::Declared(template) => Some(template),
                RpmRootDecl::Absent => declared_dest(manifest, framework),
            }
        } else {
            declared_dest(manifest, framework)
        };
        let declared_entry = declared_bundle_entry(manifest, framework);
        match dest_template {
            Some(template) => {
                let dest_uses_datadir = template.contains("{datadir}");
                let ordered_roots = if dest_uses_datadir {
                    prioritize_datadir_root(scoped_datadir_roots, contract_datadir_root)
                } else {
                    scoped_datadir_roots.to_vec()
                };
                let effective_for = |datadir: &PathBuf| -> PathBuf {
                    if dest_uses_datadir {
                        datadir.clone()
                    } else {
                        contract_datadir_root
                            .map(Path::to_path_buf)
                            .or_else(|| self.manifest_datadir_root(component, scoped_datadir_roots))
                            .unwrap_or_else(|| datadir.clone())
                    }
                };
                // Prefer the first root holding a real bundle. A directory
                // that exists but fails the bundle check (e.g. the empty
                // skeleton a raw uninstall leaves in another scope) must
                // not shadow a valid bundle in a later root; it is kept
                // only as a fallback so enable still reaches the driver's
                // specific BundleInvalid error when no root has a valid
                // bundle at all.
                let mut existing_fallback = None;
                let mut last_expanded = None;
                for datadir in &ordered_roots {
                    match self.expand_dest_template(&template, component, datadir) {
                        Ok(path)
                            if self.bundle_root_valid(
                                framework,
                                declared_entry.as_deref(),
                                &path,
                            ) =>
                        {
                            let effective = effective_for(datadir);
                            return Ok((path, effective));
                        }
                        Ok(path) if path.is_dir() => {
                            if existing_fallback.is_none() {
                                existing_fallback = Some((path, datadir.clone()));
                            }
                        }
                        Ok(path) => {
                            last_expanded = Some((path, datadir.clone()));
                        }
                        Err(_) => continue,
                    }
                }
                if let Some((path, datadir)) = existing_fallback {
                    let effective = effective_for(&datadir);
                    return Ok((path, effective));
                }
                let path = match last_expanded {
                    Some((p, _)) => p,
                    None => {
                        // All expansions failed (unknown placeholder,
                        // relative result, …) — re-expand against the
                        // primary datadir so the caller sees the actual
                        // rejection instead of a generic not-found.
                        let primary = self.layout.datadir.clone();
                        self.expand_dest_template(&template, component, &primary)?
                    }
                };
                Err(AdapterError::ContractResourceRootNotFound {
                    component: component.to_string(),
                    framework: framework.to_string(),
                    path,
                })
            }
            None => {
                // Convention discovery, with the same valid-bundle
                // preference across all datadir roots.
                if let Some(found) = self.discover_valid_convention_root(
                    component,
                    framework,
                    declared_entry.as_deref(),
                ) {
                    return Ok(found);
                }
                let mut existing_fallback = None;
                for root in &self.all_datadir_roots {
                    let candidate = root.join("adapters").join(component).join(framework);
                    if existing_fallback.is_none() && candidate.is_dir() {
                        existing_fallback = Some((candidate, root.clone()));
                    }
                }
                existing_fallback.ok_or(AdapterError::ResourceRootNotFound {
                    component: component.to_string(),
                    framework: framework.to_string(),
                })
            }
        }
    }

    /// Resolve the contract-declared resource root for a declared adapter
    /// during scan. Returns `Some(path)` only when the expanded `dest`
    /// directory exists on disk **and** looks like a real bundle for the
    /// framework (see [`Self::bundle_root_valid`]); returns `None` when
    /// the template cannot be expanded, the directory is absent, or only
    /// a stale leftover skeleton exists.
    ///
    /// `scoped_datadir_roots` limits expansion to the visible root that
    /// owns the component's contract.
    fn resolve_declared_scan_root(
        &self,
        component: &str,
        framework: &str,
        dest_template: &str,
        declared_entry: Option<&str>,
        scoped_datadir_roots: &[PathBuf],
    ) -> Option<PathBuf> {
        for datadir in scoped_datadir_roots {
            if let Ok(path) = self.expand_dest_template(dest_template, component, datadir)
                && self.bundle_root_valid(framework, declared_entry, &path)
            {
                return Some(path);
            }
        }
        None
    }

    /// True when `dir` holds a plausible adapter bundle for `framework`
    /// rather than a stale or empty leftover (e.g. the bare directory
    /// skeleton an uninstalled scope leaves behind, which must not shadow
    /// the real source in another datadir root).
    ///
    /// Validity is owned by the driver ([`FrameworkDriver::probe_bundle`]
    /// mirrors its own `read_bundle` mandatory-file checks); a framework
    /// without a driver falls back to "non-empty directory", the only
    /// signal available. Fail-closed: an unreadable directory is not a
    /// bundle.
    fn bundle_root_valid(&self, framework: &str, declared_entry: Option<&str>, dir: &Path) -> bool {
        if !dir.is_dir() {
            return false;
        }
        match self.registry.get(framework) {
            Some(driver) => driver.probe_bundle(dir, declared_entry),
            None => dir
                .read_dir()
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false),
        }
    }

    /// First datadir root whose conventional
    /// `adapters/<component>/<framework>/` directory holds a valid bundle.
    /// Returns `(resource_root, datadir_root)` like
    /// [`Self::discover_resource_root`], but skips stale leftovers.
    fn discover_valid_convention_root(
        &self,
        component: &str,
        framework: &str,
        declared_entry: Option<&str>,
    ) -> Option<(PathBuf, PathBuf)> {
        for root in &self.all_datadir_roots {
            let candidate = root.join("adapters").join(component).join(framework);
            if self.bundle_root_valid(framework, declared_entry, &candidate) {
                return Some((candidate, root.clone()));
            }
        }
        None
    }

    /// First datadir root in `scoped_datadir_roots` that actually
    /// contains the component contract file on disk. Used to determine
    /// the authoritative `effective_datadir` when the adapter `dest` is
    /// an absolute path (not relative to `{datadir}`).
    fn manifest_datadir_root(
        &self,
        component: &str,
        scoped_datadir_roots: &[PathBuf],
    ) -> Option<PathBuf> {
        for root in scoped_datadir_roots {
            let contract = FsLayout::component_contract_path(root, component);
            if contract.is_file() {
                return Some(root.clone());
            }
        }
        None
    }

    /// First datadir root that contains
    /// `adapters/<component>/<framework>/` as a directory.
    ///
    /// Returns `(resource_path, datadir_root)` so callers know which
    /// datadir root the resource came from.
    fn discover_resource_root(
        &self,
        component: &str,
        framework: &str,
    ) -> Option<(PathBuf, PathBuf)> {
        for root in &self.all_datadir_roots {
            let candidate = root.join("adapters").join(component).join(framework);
            if candidate.is_dir() {
                return Some((candidate, root.clone()));
            }
        }
        None
    }

    /// Every `(component, framework, resource_root)` discoverable under the
    /// datadir roots, deduped on `(component, framework)` and sorted.
    fn discover_all(&self) -> Vec<(String, String, PathBuf)> {
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        let mut out: Vec<(String, String, PathBuf)> = Vec::new();
        for root in &self.all_datadir_roots {
            let adapters = root.join("adapters");
            let Ok(components) = adapters.read_dir() else {
                continue;
            };
            for comp_entry in components.flatten() {
                if !comp_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let component = comp_entry.file_name().to_string_lossy().into_owned();
                let Ok(frameworks) = comp_entry.path().read_dir() else {
                    continue;
                };
                for fw_entry in frameworks.flatten() {
                    if !fw_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let framework = fw_entry.file_name().to_string_lossy().into_owned();
                    if seen.insert((component.clone(), framework.clone())) {
                        out.push((component.clone(), framework, fw_entry.path()));
                    }
                }
            }
        }
        out.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
        out
    }

    // -- logging ------------------------------------------------------------

    fn central_log(&self) -> CentralLog {
        CentralLog::open(self.layout.central_log.clone())
    }

    /// Append one operation-summary record. Logging failures are
    /// swallowed: an audit-log hiccup must not fail an otherwise-successful
    /// adapter operation.
    fn log_operation(
        &self,
        command: &str,
        component: &str,
        status: LogStatus,
        message: &str,
        detail: Option<String>,
    ) {
        let severity = match status {
            LogStatus::Ok => Severity::Info,
            LogStatus::Partial => Severity::Warn,
            LogStatus::Failed | LogStatus::RolledBack => Severity::Error,
        };
        let now = now_iso8601();
        let record = LogRecord {
            kind: LogKind::Operation,
            operation_id: None,
            command: command.to_string(),
            source: LOG_SOURCE.to_string(),
            component: Some(component.to_string()),
            severity,
            message: message.to_string(),
            actor: self.actor.clone(),
            install_mode: Some(install_mode_str(self.layout.mode).to_string()),
            started_at: now.clone(),
            finished_at: Some(now),
            status: Some(status),
            objects: vec![component.to_string()],
            backup_ids: Vec::new(),
            warnings: detail.into_iter().collect(),
            details: serde_json::Value::Null,
        };
        let _ = self.central_log().append(&record);
    }
}

// ---------------------------------------------------------------------------
// Controlled IO
// ---------------------------------------------------------------------------

/// The Manager's [`AdapterOps`] implementation: spawns framework CLIs with
/// a timeout, captures and truncates their output, and records each
/// invocation in the central log. The argv is executed directly (no
/// shell), so receipt-derived data can never inject extra commands.
struct ManagerOps {
    log: CentralLog,
    actor: String,
    install_mode: String,
    component: String,
    /// Human-readable operation label for the log `command` field.
    label: String,
    /// Roots that `copy_tree` / `remove_tree` destinations must fall
    /// under. Populated from the driver's `allowed_external_roots` plus
    /// the resource root.
    allowed_roots: Vec<PathBuf>,
    /// Read-only previews may probe framework capabilities but must not
    /// persist those invocations as operation records.
    record_invocations: bool,
}

/// Persists incremental receipt facts while the Manager holds the enable
/// lock. Drivers never receive the state path or write installed state
/// directly.
struct ManagerEnableProgress<'a> {
    state: &'a mut StateStore,
    state_path: &'a Path,
    layout: &'a FsLayout,
    allowed_external_roots: &'a [PathBuf],
    extra_owned_roots: &'a [PathBuf],
    /// Exact-equality symlink-target allowances (the enable-time anchor);
    /// see [`AdapterClaim::validate_with_trust`].
    exact_symlink_targets: &'a [PathBuf],
}

impl EnableProgress for ManagerEnableProgress<'_> {
    fn persist_claim(&mut self, claim: &AdapterClaim) -> Result<(), AdapterError> {
        claim.validate_with_trust(
            self.layout,
            self.allowed_external_roots,
            self.extra_owned_roots,
            self.exact_symlink_targets,
        )?;
        self.state.upsert_adapter_claim(claim.clone());
        self.state.save(self.state_path)?;
        Ok(())
    }
}

impl ManagerOps {
    fn new(
        log: CentralLog,
        actor: String,
        install_mode: String,
        component: String,
        label: String,
        allowed_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            log,
            actor,
            install_mode,
            component,
            label,
            allowed_roots,
            record_invocations: true,
        }
    }

    fn with_invocation_logging(mut self, enabled: bool) -> Self {
        self.record_invocations = enabled;
        self
    }

    /// Record one framework CLI invocation. Best-effort; a log failure
    /// never propagates.
    fn record(&self, cmd: &FrameworkCommand, output: &CliOutput) {
        if !self.record_invocations {
            return;
        }
        let severity = if output.success() {
            Severity::Debug
        } else {
            Severity::Warn
        };
        let argv = std::iter::once(cmd.program.clone())
            .chain(cmd.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        let now = now_iso8601();
        let record = LogRecord {
            kind: LogKind::Operation,
            operation_id: None,
            command: self.label.clone(),
            source: LOG_SOURCE.to_string(),
            component: Some(self.component.clone()),
            severity,
            message: format!("framework cli: {argv}"),
            actor: self.actor.clone(),
            install_mode: Some(self.install_mode.clone()),
            started_at: now.clone(),
            finished_at: Some(now),
            status: Some(if output.success() {
                LogStatus::Ok
            } else {
                LogStatus::Failed
            }),
            objects: vec![self.component.clone()],
            backup_ids: Vec::new(),
            warnings: Vec::new(),
            details: serde_json::json!({
                "exit": output.status,
                "timed_out": output.timed_out,
            }),
        };
        let _ = self.log.append(&record);
    }
}

impl AdapterOps for ManagerOps {
    fn run_framework_cli(&self, cmd: FrameworkCommand) -> Result<CliOutput, AdapterError> {
        let output = run_capture(&cmd)?;
        self.record(&cmd, &output);
        Ok(output)
    }

    fn run_framework_cli_json(&self, cmd: FrameworkCommand) -> Result<CliOutput, AdapterError> {
        let output = run_capture_with_stdout_cap(&cmd, JSON_OUTPUT_CAP)?;
        self.record(&cmd, &output);
        Ok(output)
    }

    fn run_framework_rpc(&self, session: FrameworkRpcSession) -> Result<CliOutput, AdapterError> {
        let output = run_rpc_capture(&session, JSON_OUTPUT_CAP)?;
        self.record(&session.command, &output);
        Ok(output)
    }

    fn copy_tree(&self, src: &Path, dst: &Path) -> Result<(), AdapterError> {
        validate_ops_path(src, &self.allowed_roots)?;
        validate_ops_path(dst, &self.allowed_roots)?;
        reject_symlink(src)?;
        if !src.is_dir() {
            return Err(AdapterError::Io {
                path: src.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "source directory does not exist",
                ),
            });
        }
        std::fs::create_dir_all(dst).map_err(|source| AdapterError::Io {
            path: dst.to_path_buf(),
            source,
        })?;
        copy_dir_recursive(src, dst).map_err(|source| AdapterError::Io {
            path: dst.to_path_buf(),
            source,
        })
    }

    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), AdapterError> {
        validate_ops_path(src, &self.allowed_roots)?;
        validate_ops_path(dst, &self.allowed_roots)?;
        reject_symlink(src)?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AdapterError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::copy(src, dst).map_err(|source| AdapterError::Io {
            path: dst.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    fn remove_path(&self, path: &Path) -> Result<bool, AdapterError> {
        validate_ops_path(path, &self.allowed_roots)?;
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_dir() => {
                std::fs::remove_dir(path).map_err(|source| AdapterError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(true)
            }
            Ok(_) => {
                std::fs::remove_file(path).map_err(|source| AdapterError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(true)
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(AdapterError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn remove_tree(&self, path: &Path) -> Result<bool, AdapterError> {
        validate_ops_path(path, &self.allowed_roots)?;
        // Use symlink_metadata so a symlink at `path` is removed as a link
        // rather than followed (remove_dir_all refuses a symlink, and
        // following one could escape the allowed roots).
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                std::fs::remove_file(path).map_err(|source| AdapterError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(true)
            }
            Ok(_) => {
                std::fs::remove_dir_all(path).map_err(|source| AdapterError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(true)
            }
            // Only a genuinely-absent path is a no-op. Permission or other
            // IO errors must surface, not masquerade as "already removed"
            // (which would let a driver mark cleanup complete and drop the
            // receipt without having verified the target was gone).
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(AdapterError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), AdapterError> {
        validate_ops_path(path, &self.allowed_roots)?;
        // Refuse to write through an existing symlink at `path`: following it
        // could land the write outside the allowed roots (the same escape
        // `read_file`/`copy_file` guard against). A symlink swapped in after
        // this check is still defeated by the rename below, which replaces the
        // link atomically rather than writing through it.
        let target_meta = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(AdapterError::Io {
                    path: path.to_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "refusing to write through a symlink at the adapter target path",
                    ),
                });
            }
            Ok(meta) => Some(meta),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(AdapterError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let parent = path.parent().ok_or_else(|| AdapterError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "adapter target path has no parent directory",
            ),
        })?;
        std::fs::create_dir_all(parent).map_err(|source| AdapterError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        // Atomic write: stage a sibling temp file then rename over the target.
        // rename replaces a symlink target with our regular file instead of
        // following it, closing the read/modify/write TOCTOU window, and never
        // leaves a truncated file if the process dies mid-write.
        //
        // Each temp candidate is opened with O_EXCL (`create_new`), which
        // fails rather than following a symlink a hostile user may have
        // planted at the temp path — so the write can never escape through the
        // temp path either. Candidate names are unique (pid + sequence), so an
        // occupied name (unrelated file or planted symlink) is skipped for a
        // fresh one rather than deleted: write_file is generic ManagerOps and
        // must not remove a sibling it does not own.
        use std::io::Write;
        const MAX_TEMP_ATTEMPTS: u32 = 16;
        let mut opened: Option<(std::fs::File, PathBuf)> = None;
        let mut last_occupied: Option<PathBuf> = None;
        for _ in 0..MAX_TEMP_ATTEMPTS {
            let candidate = temp_sibling(path);
            match create_new_file(&candidate) {
                Ok(file) => {
                    opened = Some((file, candidate));
                    break;
                }
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                    last_occupied = Some(candidate);
                    continue;
                }
                Err(source) => {
                    return Err(AdapterError::Io {
                        path: candidate,
                        source,
                    });
                }
            }
        }
        let (mut file, tmp) = opened.ok_or_else(|| AdapterError::Io {
            path: last_occupied.unwrap_or_else(|| path.to_path_buf()),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not create a unique temp file for atomic write",
            ),
        })?;
        set_atomic_write_permissions(&file, target_meta.as_ref()).map_err(|source| {
            let _ = std::fs::remove_file(&tmp);
            AdapterError::Io {
                path: tmp.clone(),
                source,
            }
        })?;
        file.write_all(contents).map_err(|source| {
            let _ = std::fs::remove_file(&tmp);
            AdapterError::Io {
                path: tmp.clone(),
                source,
            }
        })?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|source| {
            let _ = std::fs::remove_file(&tmp);
            AdapterError::Io {
                path: path.to_path_buf(),
                source,
            }
        })
    }

    fn create_symlink(&self, link: &Path, target: &Path) -> Result<(), AdapterError> {
        validate_ops_path(link, &self.allowed_roots)?;
        validate_ops_path(target, &self.allowed_roots)?;
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AdapterError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // Replace an existing ANOLISA-created symlink (idempotent
        // re-enable), but refuse to clobber a real file/dir we did not
        // create.
        match std::fs::symlink_metadata(link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                std::fs::remove_file(link).map_err(|source| AdapterError::Io {
                    path: link.to_path_buf(),
                    source,
                })?;
            }
            Ok(_) => {
                return Err(AdapterError::Io {
                    path: link.to_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "refusing to replace a non-symlink at the adapter link path",
                    ),
                });
            }
            Err(_) => {}
        }
        symlink_file(target, link).map_err(|source| AdapterError::Io {
            path: link.to_path_buf(),
            source,
        })
    }

    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
        validate_ops_path(path, &self.allowed_roots)?;
        // Refuse to follow a symlink at `path`: reading through it could
        // escape the allowed roots (the same escape `copy_file` guards).
        reject_symlink(path)?;
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(AdapterError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

/// Create a symlink at `link` pointing to `target`. Unix-only; on other
/// platforms this returns an unsupported error so the boundary never
/// silently degrades.
#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink_file(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlink adapters are only supported on Unix",
    ))
}

/// Spawn `cmd` as a direct argv (no shell), enforce its timeout, and return
/// truncated output. The child's stdout/stderr are drained on separate
/// threads so a full pipe can never deadlock the wait loop.
fn run_capture(cmd: &FrameworkCommand) -> Result<CliOutput, AdapterError> {
    run_capture_with_stdout_cap(cmd, OUTPUT_CAP)
}

/// Run a framework CLI with a caller-selected bounded stdout capture.
/// Stderr stays on the ordinary diagnostic cap regardless of stdout policy.
fn run_capture_with_stdout_cap(
    cmd: &FrameworkCommand,
    stdout_cap: usize,
) -> Result<CliOutput, AdapterError> {
    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(if cmd.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for key in &cmd.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &cmd.env_set {
        command.env(key, value);
    }
    if !cmd.path_prepend.is_empty() {
        command.env("PATH", prepend_path(&cmd.path_prepend));
    }

    let mut child = crate::process::spawn_retry_etxtbsy(&mut command).map_err(|source| {
        AdapterError::FrameworkCli {
            program: cmd.program.clone(),
            reason: format!("failed to spawn: {source}"),
        }
    })?;

    let stdout_handle = child.stdout.take().map(|r| spawn_drain(r, stdout_cap));
    let stderr_handle = child.stderr.take().map(|r| spawn_drain(r, OUTPUT_CAP));
    let start = Instant::now();
    let mut stdin_handle = if let Some(input) = &cmd.stdin {
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            let _ = collect_drain(stdout_handle);
            let _ = collect_drain(stderr_handle);
            return Err(AdapterError::FrameworkCli {
                program: cmd.program.clone(),
                reason: "failed to open child stdin".to_string(),
            });
        };
        let input = input.clone();
        Some(thread::spawn(move || stdin.write_all(&input)))
    } else {
        None
    };

    let mut timed_out = false;
    let status = loop {
        if stdin_handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
            && let Err(reason) = collect_stdin_writer(stdin_handle.take())
        {
            let _ = child.kill();
            let _ = child.wait();
            let _ = collect_drain(stdout_handle);
            let _ = collect_drain(stderr_handle);
            return Err(AdapterError::FrameworkCli {
                program: cmd.program.clone(),
                reason: format!("failed to write child stdin: {reason}"),
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= cmd.timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = collect_stdin_writer(stdin_handle.take());
                    timed_out = true;
                    break None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = collect_stdin_writer(stdin_handle.take());
                let _ = collect_drain(stdout_handle);
                let _ = collect_drain(stderr_handle);
                return Err(AdapterError::FrameworkCli {
                    program: cmd.program.clone(),
                    reason: format!("failed to wait: {source}"),
                });
            }
        }
    };

    if let Err(reason) = collect_stdin_writer(stdin_handle) {
        let _ = child.kill();
        let _ = child.wait();
        let _ = collect_drain(stdout_handle);
        let _ = collect_drain(stderr_handle);
        return Err(AdapterError::FrameworkCli {
            program: cmd.program.clone(),
            reason: format!("failed to write child stdin: {reason}"),
        });
    }
    let stdout = collect_drain(stdout_handle);
    let stderr = collect_drain(stderr_handle);

    Ok(CliOutput {
        status: status.and_then(|s| s.code()),
        timed_out,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Drive a line-delimited JSON-RPC session, holding the server's stdin open
/// until `session.expected_responses` id-bearing replies have been read (or
/// the command timeout elapses), then closing it so the child exits.
///
/// stdout is read line-by-line on a worker thread rather than drained to EOF,
/// because the close decision depends on what has already been answered — a
/// server that only exits once its stdin closes would otherwise deadlock
/// against a drain-to-EOF reader.
fn run_rpc_capture(
    session: &FrameworkRpcSession,
    stdout_cap: usize,
) -> Result<CliOutput, AdapterError> {
    let cmd = &session.command;
    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for key in &cmd.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &cmd.env_set {
        command.env(key, value);
    }
    if !cmd.path_prepend.is_empty() {
        command.env("PATH", prepend_path(&cmd.path_prepend));
    }

    let mut child = crate::process::spawn_retry_etxtbsy(&mut command).map_err(|source| {
        AdapterError::FrameworkCli {
            program: cmd.program.clone(),
            reason: format!("failed to spawn: {source}"),
        }
    })?;

    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(AdapterError::FrameworkCli {
            program: cmd.program.clone(),
            reason: "failed to open child stdio pipes".to_string(),
        });
    };
    let stderr_handle = child.stderr.take().map(|r| spawn_drain(r, OUTPUT_CAP));

    // The writer thread owns stdin and only drops it (signalling EOF) once
    // `close_tx` is dropped by this thread.
    let (close_tx, close_rx) = std::sync::mpsc::channel::<()>();
    let payload: Vec<u8> = session
        .requests
        .iter()
        .flat_map(|line| {
            line.as_bytes()
                .iter()
                .copied()
                .chain(std::iter::once(b'\n'))
        })
        .collect();
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        let result = stdin.write_all(&payload).and_then(|()| stdin.flush());
        // Park until the reader is done; `recv` returns Err on sender drop.
        let _ = close_rx.recv();
        result
    });

    enum RpcStdoutEvent {
        Line(String),
        LimitExceeded,
        ReadFailed(String),
    }

    // A rendezvous channel prevents the reader from queuing output faster
    // than this thread can account for it.
    let (line_tx, line_rx) = std::sync::mpsc::sync_channel::<RpcStdoutEvent>(0);
    let reader = thread::spawn(move || {
        // Read at most one byte beyond the cap so overflow is detectable
        // without allocating an arbitrarily large unterminated JSONL line.
        let limit = (stdout_cap as u64).saturating_add(1);
        let mut reader = std::io::BufReader::new(stdout).take(limit);
        let mut total = 0usize;
        let mut drain_after_disconnect = false;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(read) => {
                    total = total.saturating_add(read);
                    if total > stdout_cap {
                        drain_after_disconnect =
                            line_tx.send(RpcStdoutEvent::LimitExceeded).is_err();
                        break;
                    }
                    if line.ends_with('\n') {
                        line.pop();
                        if line.ends_with('\r') {
                            line.pop();
                        }
                    }
                    if line_tx.send(RpcStdoutEvent::Line(line)).is_err() {
                        drain_after_disconnect = true;
                        break;
                    }
                }
                Err(source) => {
                    let _ = line_tx.send(RpcStdoutEvent::ReadFailed(source.to_string()));
                    break;
                }
            }
        }
        if drain_after_disconnect {
            let mut reader = reader.into_inner();
            let mut chunk = [0u8; 8192];
            while reader.read(&mut chunk).is_ok_and(|read| read != 0) {
                // Keep draining after the RPC exchange completes so the child
                // can flush and exit without retaining any additional output.
            }
        }
    });

    let start = Instant::now();
    let mut kept = String::new();
    let mut answered = 0usize;
    let mut timed_out = false;
    let mut stdout_failure = None;
    while answered < session.expected_responses {
        let Some(remaining) = cmd.timeout.checked_sub(start.elapsed()) else {
            timed_out = true;
            break;
        };
        match line_rx.recv_timeout(remaining) {
            Ok(RpcStdoutEvent::Line(line)) => {
                if kept.len().saturating_add(line.len()).saturating_add(1) > stdout_cap {
                    stdout_failure = Some(format!(
                        "app-server stdout exceeded the {stdout_cap}-byte limit"
                    ));
                    break;
                }
                if is_rpc_response(&line) {
                    answered += 1;
                }
                kept.push_str(&line);
                kept.push('\n');
            }
            Ok(RpcStdoutEvent::LimitExceeded) => {
                stdout_failure = Some(format!(
                    "app-server stdout exceeded the {stdout_cap}-byte limit"
                ));
                break;
            }
            Ok(RpcStdoutEvent::ReadFailed(reason)) => {
                stdout_failure = Some(format!("failed to read app-server stdout: {reason}"));
                break;
            }
            // The server closed stdout (or died) before answering.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                break;
            }
        }
    }

    // Unblock a reader waiting to publish another event. It will drain the
    // pipe without retaining output once all expected replies are complete.
    drop(line_rx);
    // Closing stdin lets a well-behaved server flush and exit. If the
    // exchange is incomplete, terminate the child before joining the writer:
    // it may be blocked in `write_all` on a full pipe that the server stopped
    // reading, and waiting for it first would defeat the session timeout.
    drop(close_tx);
    let incomplete = answered < session.expected_responses || stdout_failure.is_some();
    let mut status = if incomplete {
        match child.try_wait() {
            Ok(Some(status)) => Some(status),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                child.wait().ok()
            }
        }
    } else {
        None
    };
    let write_result = writer.join();
    if !incomplete {
        status = wait_bounded(&mut child, start, cmd.timeout);
        if status.is_none() {
            timed_out = true;
        }
    }
    let _ = reader.join();
    let mut stderr = String::from_utf8_lossy(&collect_drain(stderr_handle)).into_owned();

    // A failed stdin write is a symptom, not the diagnosis: a server that does
    // not implement the subcommand exits before the request lands, and EPIPE
    // would mask its exit code and stderr. Note it and let the caller judge
    // the exchange by the replies it did or did not get.
    let write_note = match write_result {
        Ok(Err(source)) => Some(format!("failed to write server stdin: {source}")),
        Err(_) => Some("stdin writer thread panicked".to_string()),
        Ok(Ok(())) => None,
    };
    if let Some(note) = write_note.filter(|_| answered < session.expected_responses) {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&note);
    }

    if let Some(reason) = stdout_failure {
        return Err(AdapterError::FrameworkCli {
            program: cmd.program.clone(),
            reason,
        });
    }

    Ok(CliOutput {
        status: status.and_then(|s| s.code()),
        timed_out,
        stdout: kept,
        stderr,
    })
}

/// Whether a server stdout line is a JSON-RPC *response* (carries an `id`)
/// rather than a notification. Only responses count toward the session's
/// expected reply count.
fn is_rpc_response(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| value.get("id").cloned())
        .is_some_and(|id| !id.is_null())
}

/// Reap `child` within what remains of `timeout` measured from `start`,
/// killing it on expiry. Returns `None` when the child had to be killed.
fn wait_bounded(
    child: &mut std::process::Child,
    start: Instant,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Build a `PATH` value with `prepend` dirs in front of the current one.
fn prepend_path(prepend: &[PathBuf]) -> std::ffi::OsString {
    prepend_path_with_existing(prepend, std::env::var_os("PATH"))
}

fn prepend_path_with_existing(
    prepend: &[PathBuf],
    existing: Option<std::ffi::OsString>,
) -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = prepend.to_vec();
    if let Some(existing) = existing {
        parts.extend(std::env::split_paths(&existing));
    }
    // join_paths only fails if a component contains the path separator,
    // which our dirs do not; fall back to the prepend dirs alone.
    std::env::join_paths(&parts)
        .unwrap_or_else(|_| std::env::join_paths(prepend).unwrap_or_default())
}

/// Open `path` for writing, failing if it already exists. Uses `O_EXCL`
/// semantics (`create_new`), which does not follow a symlink at `path` — the
/// key guard against a pre-planted symlink at the temp path.
fn create_new_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Match atomic-write temp permissions to the existing target mode, or use a
/// private default for a newly-created target.
#[cfg(unix)]
fn set_atomic_write_permissions(
    file: &std::fs::File,
    target_meta: Option<&std::fs::Metadata>,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = target_meta
        .map(|meta| meta.permissions().mode() & 0o7777)
        .unwrap_or(0o600);
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_atomic_write_permissions(
    _file: &std::fs::File,
    _target_meta: Option<&std::fs::Metadata>,
) -> std::io::Result<()> {
    Ok(())
}

/// Monotonic counter making atomic-write temp names unique within a process.
static WRITE_TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A fresh, unique sibling temp path for an atomic write in `path`'s
/// directory: `.<name>.anolisa-tmp.<pid>.<seq>`. Unique per call (pid +
/// process-monotonic sequence) so [`ManagerOps::write_file`] can create it
/// with `O_EXCL` and never collide with — nor need to delete — an unrelated
/// pre-existing file at a fixed name. Sibling placement keeps the final
/// `rename` within one filesystem.
fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "adapter".to_string());
    let pid = std::process::id();
    let seq = WRITE_TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_name = format!(".{name}.anolisa-tmp.{pid}.{seq}");
    match path.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}

/// Validate that `path` is under one of `allowed_roots` and contains no
/// traversal segments. Used by `copy_tree` / `remove_tree` to enforce the
/// Manager's IO boundary before any filesystem mutation.
fn validate_ops_path(path: &Path, allowed_roots: &[PathBuf]) -> Result<(), AdapterError> {
    use super::claim::validate_external_path;

    validate_external_path(path, allowed_roots).map_err(|source| {
        AdapterError::ClaimValidation(super::claim::ClaimValidationError::ExternalPath {
            id: format!("ops:{}", path.display()),
            source,
        })
    })
}

/// Reject a path that is a symlink. Used by `copy_file` and
/// `copy_dir_recursive` to prevent following a symlink that escapes the
/// allowed roots.
fn reject_symlink(path: &Path) -> Result<(), AdapterError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(AdapterError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "symlink rejected in adapter resource tree: {}",
                    path.display()
                ),
            ),
        }),
        _ => Ok(()),
    }
}

/// Recursively copy regular files and subdirectories from `src` into
/// `dst`. Symlinks are rejected — a symlink inside the resource tree
/// could point outside the allowed roots, bypassing the boundary check
/// on the top-level `src` path.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "symlink rejected in adapter resource tree: {}",
                    entry.path().display()
                ),
            ));
        }
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ft.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Drain a child pipe to EOF on its own thread, keeping at most `cap`
/// bytes. Reading to EOF (even past the cap) keeps the child from blocking
/// on a full pipe.
fn spawn_drain<R: Read + Send + 'static>(mut reader: R, cap: usize) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if kept.len() < cap {
                        let take = (cap - kept.len()).min(n);
                        kept.extend_from_slice(&chunk[..take]);
                    }
                }
                Err(_) => break,
            }
        }
        kept
    })
}

/// Join a drain thread, returning its captured bytes (empty on panic or
/// absent pipe).
fn collect_drain(handle: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

fn collect_stdin_writer(handle: Option<JoinHandle<std::io::Result<()>>>) -> Result<(), String> {
    match handle {
        None => Ok(()),
        Some(handle) => match handle.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(source)) => Err(source.to_string()),
            Err(_) => Err("writer thread panicked".to_string()),
        },
    }
}

/// ISO 8601 UTC timestamp, second precision.
fn now_iso8601() -> String {
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Stable string for the central log's `install_mode` field.
fn install_mode_str(mode: InstallMode) -> &'static str {
    match mode {
        InstallMode::System => "system",
        InstallMode::User => "user",
    }
}

/// Map a [`super::contract::ContractError`] to the existing [`AdapterError`]
/// family for backward-compatible CLI rendering.
fn map_contract_error(component: &str, err: super::contract::ContractError) -> AdapterError {
    match err {
        super::contract::ContractError::Unavailable { searched, .. } => {
            AdapterError::MissingAdapterManifest {
                component: component.to_string(),
                searched,
            }
        }
        super::contract::ContractError::ParseError { path, reason } => {
            AdapterError::AdapterManifest {
                component: component.to_string(),
                path,
                reason,
            }
        }
        super::contract::ContractError::Io { path, source } => AdapterError::AdapterManifest {
            component: component.to_string(),
            path,
            reason: source.to_string(),
        },
    }
}

fn declared_frameworks(manifest: &ComponentManifest) -> Vec<String> {
    let mut set = BTreeSet::new();
    for adapter in &manifest.adapters {
        if let Some(framework) = adapter.framework.as_deref().map(str::trim)
            && !framework.is_empty()
        {
            set.insert(framework.to_string());
        }
    }
    set.into_iter().collect()
}

/// Extract the `dest` from the first `[[adapters]]` entry whose
/// `framework` matches. Returns `None` when the field is absent or empty.
fn declared_dest(manifest: &ComponentManifest, framework: &str) -> Option<String> {
    manifest
        .adapters
        .iter()
        .find(|adapter| adapter.framework.as_deref().map(str::trim) == Some(framework))
        .and_then(|adapter| adapter.dest.as_deref())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
}

/// Classify the `[adapters.backends.rpm].resource_root` declaration of
/// the first `[[adapters]]` entry whose `framework` matches. See
/// [`RpmRootDecl`] for the three-valued semantics.
fn rpm_root_decl(manifest: &ComponentManifest, framework: &str) -> RpmRootDecl {
    RpmRootDecl::from_raw(
        manifest
            .adapters
            .iter()
            .find(|adapter| adapter.framework.as_deref().map(str::trim) == Some(framework))
            .and_then(|adapter| adapter.backends.rpm.as_ref())
            .and_then(|rpm| rpm.resource_root.as_deref()),
    )
}

/// Extract the `adapter_type` from the first `[[adapters]]` entry whose
/// `framework` matches. Returns `None` when the manifest omits the field
/// (which the caller treats as defaulting to `"plugin"`).
fn declared_adapter_type(manifest: &ComponentManifest, framework: &str) -> Option<String> {
    manifest
        .adapters
        .iter()
        .find(|adapter| adapter.framework.as_deref().map(str::trim) == Some(framework))
        .and_then(|adapter| adapter.adapter_type.as_deref())
        .map(str::trim)
        .filter(|at| !at.is_empty())
        .map(str::to_string)
}

fn is_supported_adapter_type(adapter_type: &str) -> bool {
    matches!(adapter_type, "plugin" | "skill_bundle" | "extension")
}

/// The adapter types each built-in framework accepts, in declaration order.
///
/// A framework whose list contains `"plugin"` also accepts an absent
/// `adapter_type` (the legacy default). A framework without `"plugin"` (e.g.
/// `cosh`, which is extension-only) requires an explicit `adapter_type`.
///
/// Returns `None` for frameworks with no built-in driver; enable resolves
/// that into the more specific [`AdapterError::UnknownFramework`] later, so
/// this gate stays silent for them.
fn allowed_adapter_types(framework: &str) -> Option<&'static [&'static str]> {
    match framework {
        // Plugin frameworks that also deliver skills.
        "openclaw" | "hermes" => Some(&["plugin", "skill_bundle"]),
        // Marketplace-plugin frameworks: plugin only.
        "codex" | "claude-code" => Some(&["plugin"]),
        // Qoder installs a directory-named plugin and activates it via
        // settings.json entries: plugin only (no extension / skill_bundle).
        "qoder" => Some(&["plugin"]),
        // dsh bundles are native plugins registered per explicit profile.
        "dsh" => Some(&["plugin"]),
        // QwenPaw installs a directory-named plugin through its own CLI.
        "qwenpaw" => Some(&["plugin"]),
        // Extension frameworks require an explicit type. Qwen Code delegates
        // artifact and activation mutations to its native CLI.
        "cosh" | "qwencode" => Some(&["extension"]),
        _ => None,
    }
}

/// Reject a framework/`adapter_type` pair the framework does not support,
/// even when the type is implemented by *some other* driver.
///
/// This is distinct from [`AdapterError::UnsupportedAdapterType`], which is
/// reserved for a value no driver implements at all (e.g. `service`). Here
/// the value is a real adapter type used with the wrong framework — for
/// example `openclaw` + `extension` (which would otherwise silently run the
/// plugin path) or `cosh` + `plugin` (which would mis-handle a filesystem
/// extension as a CLI plugin).
///
/// # Errors
///
/// [`AdapterError::InvalidAdapterInput`] when the declared (or defaulted)
/// type is not in the framework's allowed set.
fn validate_adapter_type_for_framework(
    component: &str,
    framework: &str,
    adapter_type: Option<&str>,
) -> Result<(), AdapterError> {
    let Some(allowed) = allowed_adapter_types(framework) else {
        // No built-in driver; let enable surface UnknownFramework instead.
        return Ok(());
    };
    let accepts_default = allowed.contains(&"plugin");
    let ok = match adapter_type {
        Some(at) => allowed.contains(&at),
        None => accepts_default,
    };
    if ok {
        return Ok(());
    }
    let declared = adapter_type.unwrap_or("<absent> (defaults to plugin)");
    Err(AdapterError::InvalidAdapterInput {
        component: component.to_string(),
        framework: framework.to_string(),
        reason: format!(
            "framework '{framework}' does not support adapter_type '{declared}'; it accepts: {}",
            allowed.join(", ")
        ),
    })
}

/// Whether a component status makes it visible to adapter scan/enable.
/// Both fully-installed and adopted components should be adapter-visible.
fn is_adapter_visible(installation: &Installation) -> bool {
    installation.status == LifecycleStatus::Installed
}

/// Whether an installed record is delegated to the native RPM manager
/// (managed, adopted, or observed alike): its payload was placed by the
/// RPM transaction, so adapter resource-root resolution must prefer the
/// contract's `[adapters.backends.rpm].resource_root` over the raw `dest`.
fn rpm_provenance(installation: &Installation) -> bool {
    matches!(
        installation.binding,
        ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            ..
        }
    )
}

/// Return the datadir root that supplied a resolved component contract.
///
/// Delegates to [`super::contract::infer_contract_datadir_root`] which
/// checks provenance first (written during install/adopt), then falls
/// back to content matching for snapshots created before provenance was
/// introduced.
fn contract_datadir_root_from_source(
    component: &str,
    contract_path: &Path,
    scoped_datadir_roots: &[PathBuf],
) -> Option<PathBuf> {
    super::contract::infer_contract_datadir_root(component, contract_path, scoped_datadir_roots)
}

fn declared_plugin_id(manifest: &ComponentManifest, framework: &str) -> Option<String> {
    manifest
        .adapters
        .iter()
        .find(|adapter| adapter.framework.as_deref().map(str::trim) == Some(framework))
        .and_then(|adapter| adapter.plugin_id.as_deref())
        .map(str::trim)
        .filter(|plugin_id| !plugin_id.is_empty())
        .map(str::to_string)
}

/// Extract declared skills for a framework, checking the framework-specific
/// section first (e.g. `adapters.openclaw.skills`) then falling back to
/// the generic `adapters.skills`.
fn declared_skills(
    manifest: &ComponentManifest,
    framework: &str,
) -> Vec<crate::manifest::AdapterSkillSpec> {
    let adapter = manifest
        .adapters
        .iter()
        .find(|a| a.framework.as_deref().map(str::trim) == Some(framework));
    let adapter = match adapter {
        Some(a) => a,
        None => return Vec::new(),
    };
    // Framework-specific section takes precedence.
    match framework {
        "openclaw" => {
            if let Some(ref oc) = adapter.openclaw
                && !oc.skills.is_empty()
            {
                return oc.skills.clone();
            }
        }
        "hermes" => {
            if let Some(ref h) = adapter.hermes
                && !h.skills.is_empty()
            {
                return h.skills.clone();
            }
        }
        _ => {}
    }
    adapter.skills.clone()
}

/// Resolve skill source paths from manifest specs.
///
/// `effective_datadir` is the datadir root that was used to resolve the
/// adapter resource root — `{datadir}` in skill source templates
/// expands to this value so skill sources stay in the same scope as the
/// adapter itself (important for user-mode enabling a system-adopted
/// component).
///
/// Resolved paths are validated against an IO boundary: they must fall
/// under `resource_root` or `effective_datadir`. A manifest cannot
/// self-authorise access to arbitrary filesystem paths.
///
/// For each declared skill:
/// - If `source` is present, expand layout placeholders (with
///   `{component}` as extra var and `{datadir}` set to
///   `effective_datadir`). A relative result is resolved against
///   `resource_root`.
/// - If `source` is absent, the driver will fall back to
///   `<resource_root>/skills/<name>/`.
fn resolve_skill_sources(
    specs: Vec<crate::manifest::AdapterSkillSpec>,
    layout: &FsLayout,
    effective_datadir: &Path,
    component: &str,
    framework: &str,
    resource_root: &Path,
) -> Result<Vec<super::driver::DeclaredSkill>, AdapterError> {
    let mut scoped_layout = layout.clone();
    scoped_layout.datadir = effective_datadir.to_path_buf();
    let allowed_roots = [resource_root.to_path_buf(), effective_datadir.to_path_buf()];
    specs
        .into_iter()
        .map(|spec| {
            let source = match spec.source {
                Some(ref template) => {
                    let expanded = super::expand_layout_placeholders(
                        template,
                        &scoped_layout,
                        &[("component", component)],
                    )?;
                    let resolved = if expanded.is_relative() {
                        resource_root.join(&expanded)
                    } else {
                        expanded
                    };
                    super::claim::validate_external_path(&resolved, &allowed_roots).map_err(
                        |_| AdapterError::InvalidAdapterInput {
                            component: component.to_string(),
                            framework: framework.to_string(),
                            reason: format!(
                                "skill '{}' source '{}' resolves to '{}' which is outside the allowed roots (resource_root or datadir)",
                                spec.name,
                                template,
                                resolved.display(),
                            ),
                        },
                    )?;
                    Some(resolved)
                }
                None => None,
            };
            Ok(super::driver::DeclaredSkill {
                name: spec.name,
                source,
            })
        })
        .collect()
}

/// Extract declared config entries for a framework, checking the
/// framework-specific section first then falling back to the generic one.
fn declared_config(
    manifest: &ComponentManifest,
    framework: &str,
) -> Vec<crate::manifest::AdapterConfigSetSpec> {
    let adapter = manifest
        .adapters
        .iter()
        .find(|a| a.framework.as_deref().map(str::trim) == Some(framework));
    let adapter = match adapter {
        Some(a) => a,
        None => return Vec::new(),
    };
    // Framework-specific section takes precedence.
    if framework == "openclaw"
        && let Some(ref oc) = adapter.openclaw
        && !oc.config.is_empty()
    {
        return oc.config.clone();
    }
    adapter.config.clone()
}

/// Extract every declared notice for a framework, checking the
/// framework-specific section first then falling back to the generic one.
///
/// Notices are inert, display-only text: they are returned verbatim and
/// never shell-expanded, template-substituted, or executed.
fn declared_all_notices(
    manifest: &ComponentManifest,
    framework: &str,
) -> Vec<crate::manifest::AdapterNotice> {
    let adapter = manifest
        .adapters
        .iter()
        .find(|a| a.framework.as_deref().map(str::trim) == Some(framework));
    let adapter = match adapter {
        Some(a) => a,
        None => return Vec::new(),
    };
    // Framework-specific section takes precedence.
    let notices = match framework {
        "openclaw" => adapter
            .openclaw
            .as_ref()
            .filter(|oc| !oc.notices.is_empty())
            .map_or(&adapter.notices, |oc| &oc.notices),
        "hermes" => adapter
            .hermes
            .as_ref()
            .filter(|h| !h.notices.is_empty())
            .map_or(&adapter.notices, |h| &h.notices),
        _ => &adapter.notices,
    };
    notices.clone()
}

/// The subset of a framework's declared notices matching `when`.
fn declared_notices(
    manifest: &ComponentManifest,
    framework: &str,
    when: crate::manifest::NoticeWhen,
) -> Vec<crate::manifest::AdapterNotice> {
    declared_all_notices(manifest, framework)
        .into_iter()
        .filter(|notice| notice.when == when)
        .collect()
}

/// Resolve the adapter-level framework version requirement for a framework.
///
/// `[adapters.compat].framework_version` is the primary source; the legacy
/// top-level `framework_version_req` is the compatibility entry used only
/// when `compat.framework_version` is absent. This precedence lets newer
/// manifests express the requirement in the structured `compat` table while
/// older manifests keep working unchanged.
fn declared_framework_version_req(manifest: &ComponentManifest, framework: &str) -> Option<String> {
    let adapter = manifest
        .adapters
        .iter()
        .find(|a| a.framework.as_deref().map(str::trim) == Some(framework))?;
    // Precedence only — no validity check here. `[adapters.compat]
    // .framework_version` is primary: when the field is *present* (even if
    // empty), the legacy `framework_version_req` is not consulted, so a
    // migrated manifest cannot accidentally fall back to a stale value. The
    // raw value (including a present-but-empty one) is passed through to the
    // driver, which alone decides validity — this keeps the framework-agnostic
    // Manager from imposing OpenClaw's version rules on other frameworks
    // (Hermes/Codex/Qoder/Cosh ignore the field entirely).
    if let Some(raw) = adapter.compat.framework_version.as_deref() {
        return Some(raw.to_string());
    }
    adapter.framework_version_req.as_deref().map(str::to_string)
}

/// Extract the bundle entry-point from the manifest, checking the
/// framework-specific section first then falling back to the generic
/// `[adapters.bundle].entry`.
fn declared_bundle_entry(manifest: &ComponentManifest, framework: &str) -> Option<String> {
    let adapter = manifest
        .adapters
        .iter()
        .find(|a| a.framework.as_deref().map(str::trim) == Some(framework))?;
    match framework {
        "openclaw" => {
            if let Some(ref oc) = adapter.openclaw
                && let Some(ref entry) = oc.bundle.entry
            {
                return Some(entry.clone());
            }
        }
        "hermes" => {
            if let Some(ref h) = adapter.hermes
                && let Some(ref entry) = h.bundle.entry
            {
                return Some(entry.clone());
            }
        }
        _ => {}
    }
    adapter.bundle.entry.clone()
}

/// Framework plugins an adapter declares as displaced while it is enabled.
///
/// Only the OpenClaw contract carries the declaration today
/// (`[adapters.openclaw].displaces`); every other framework returns an empty
/// list, so their drivers never see the field. The ids and slot keys are
/// validated by the caller before they reach a driver — the driver alone
/// decides what to do with them.
fn declared_displaces(
    manifest: &ComponentManifest,
    framework: &str,
) -> Vec<crate::manifest::DisplacedPluginSpec> {
    let Some(adapter) = manifest
        .adapters
        .iter()
        .find(|a| a.framework.as_deref().map(str::trim) == Some(framework))
    else {
        return Vec::new();
    };
    match framework {
        "openclaw" => adapter
            .openclaw
            .as_ref()
            .map(|oc| oc.displaces.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The `post_disable` notices persisted in a receipt, verbatim. Notices are
/// inert display text — never expanded, substituted, or executed.
fn post_disable_notices(claim: &AdapterClaim) -> Vec<crate::manifest::AdapterNotice> {
    claim
        .notices
        .iter()
        .filter(|notice| notice.when == crate::manifest::NoticeWhen::PostDisable)
        .cloned()
        .collect()
}

/// Reject a contract that displaces the adapter's own plugin, using the plugin
/// id the driver actually resolved from the bundle.
///
/// The declaration check in [`AdapterManager::enable_with_options`] can only
/// compare against the manifest's `plugin_id`, and that key is optional: when it
/// is omitted the real id comes from the bundle entry (`openclaw.plugin.json`) or
/// from the component name, neither of which is known until `read_bundle` runs.
/// Without this second gate such a contract would install and verify the
/// adapter's own plugin, then disable it, and still report success with an
/// `Enabled` receipt — an adapter that is enabled, verified, and unable to
/// answer a single tool call.
///
/// Runs before any mutation on both the dry-run and the real path, so a bad
/// contract is rejected identically whether or not the caller asked for a plan.
fn validate_displacements_not_self(
    bundle: &AdapterBundle,
    ctx: &DriverCtx,
) -> Result<(), AdapterError> {
    let Some(own_plugin_id) = bundle.plugin_id.as_deref() else {
        // A skill bundle has no plugin id of its own, and the Manager already
        // rejects a displacement declaration on one.
        return Ok(());
    };
    for displaced in &ctx.declared_displaces {
        if displaced.id == own_plugin_id {
            return Err(AdapterError::InvalidAdapterInput {
                component: ctx.component.clone(),
                framework: ctx.framework.clone(),
                reason: format!(
                    "displaced plugin id '{own_plugin_id}' must differ from the adapter's own \
                     plugin id resolved from the bundle"
                ),
            });
        }
    }
    Ok(())
}

/// Build a non-mutating [`DisableReport`] from a validated receipt,
/// describing what a real disable would do. Uses the driver payload's
/// resource IDs to determine which resources are actually cleaned up
/// (skill dirs, plugin dirs), excluding the framework home/state dir
/// which real disable never removes.
///
/// The returned `cleanup_complete` is always `true`: it signals that the
/// planned cleanup description is complete, not that real cleanup ran.
/// Callers must check [`DisableOutcome::dry_run`] to distinguish planned
/// output from actual framework cleanup.
fn plan_disable_report(claim: &AdapterClaim, restores: Option<&[String]>) -> DisableReport {
    use super::claim::{ClaimResourceKind, DriverPayload};
    let mut messages = Vec::new();

    // Resource ids that a real disable actually acts on. Empty plugin ids
    // (skill-bundle receipts carry no plugin resource) are excluded.
    // `cli_step` names the plugin resource id and verb for receipts whose
    // real disable first runs a framework CLI step (`hermes plugins disable
    // <id>`, `qwenpaw plugin uninstall <id>`) before removing the plugin
    // directory, so their dry-run plan needs the extra CLI line that
    // OpenClaw (registry-only) does not.
    let mut cleanup_ids: Vec<&str> = Vec::new();
    // Resource ids of framework plugins a real disable *re-enables* rather than
    // removes. Kept apart from `cleanup_ids` because the verb is the opposite:
    // the generic branch below would announce "unregister" for a plugin disable
    // actually hands back.
    let mut restore_ids: Vec<(&str, Option<&str>)> = Vec::new();
    let cli_step: Option<(&str, &str)> = match &claim.driver_payload {
        DriverPayload::OpenClaw(oc) => {
            cleanup_ids.extend(oc.skill_resources.iter().map(String::as_str));
            if !oc.plugin_resource.is_empty() {
                cleanup_ids.push(&oc.plugin_resource);
            }
            restore_ids.extend(
                oc.displaced_plugins
                    .iter()
                    .map(|d| (d.resource.as_str(), d.slot.as_deref())),
            );
            None
        }
        DriverPayload::Hermes(h) => {
            cleanup_ids.extend(h.skill_resources.iter().map(String::as_str));
            if h.plugin_resource.is_empty() {
                None
            } else {
                cleanup_ids.push(&h.plugin_resource);
                Some((h.plugin_resource.as_str(), "disable"))
            }
        }
        DriverPayload::Cosh(c) => {
            cleanup_ids.push(&c.extension_dir_resource);
            None
        }
        DriverPayload::Codex(c) => {
            // Real disable order: plugin remove, marketplace remove, then
            // remove the marketplace directory (which contains the symlink).
            cleanup_ids.push(&c.plugin_resource);
            cleanup_ids.push(&c.marketplace_resource);
            cleanup_ids.push(&c.symlink_resource);
            cleanup_ids.push(&c.marketplace_dir_resource);
            None
        }
        DriverPayload::ClaudeCode(c) => {
            cleanup_ids.push(&c.plugin_resource);
            cleanup_ids.push(&c.marketplace_resource);
            None
        }
        DriverPayload::Qoder(q) => {
            cleanup_ids.push(&q.plugin_resource);
            // settings.json is edited in place (ANOLISA-managed entries
            // pruned), never removed, so it is not a cleanup_id; describe the
            // prune explicitly rather than as a file removal.
            let entry = claim
                .plugin_id
                .as_deref()
                .map(|p| format!("{p}@local"))
                .unwrap_or_else(|| "<plugin>@local".to_string());
            messages.push(format!(
                "would prune ANOLISA-managed hooks and '{entry}' from ~/.qoder/settings.json"
            ));
            None
        }
        DriverPayload::QwenCode(q) => {
            cleanup_ids.push(&q.plugin_resource);
            messages
                .push("would remove the Qwen Code activation policy via the qwen CLI".to_string());
            None
        }
        DriverPayload::Dsh(dsh) => {
            for profile in &dsh.profiles {
                cleanup_ids.push(&profile.plugin_resource);
            }
            None
        }
        DriverPayload::QwenPaw(q) => {
            cleanup_ids.push(&q.plugin_resource);
            Some((q.plugin_resource.as_str(), "uninstall"))
        }
    };

    // Whether disable uninstalls (Claude Code / Qoder semantics) rather than
    // unregisters (registry-only). Purely cosmetic for the plan text.
    let plugin_verb = match claim.driver_payload {
        DriverPayload::ClaudeCode(_) | DriverPayload::Qoder(_) | DriverPayload::QwenCode(_) => {
            "uninstall"
        }
        DriverPayload::Dsh(_) => "remove",
        _ => "unregister",
    };

    for resource in &claim.resources {
        if restore_ids
            .iter()
            .any(|(id, _)| *id == resource.id.as_str())
        {
            // A resource disable hands *back* rather than removes. When the driver
            // described those restores itself, its lines are authoritative and this
            // generic wording is dropped: two descriptions of one mutation, one of
            // them composed here without the driver's veto logic, is exactly the
            // divergence the `plan_disable_restores` hook exists to remove.
            if restores.is_some() {
                continue;
            }
            let slot = restore_ids
                .iter()
                .find(|(id, _)| *id == resource.id.as_str())
                .and_then(|(_, slot)| *slot);
            // A real disable hands this one back through
            // `restore_displaced_plugins`, which is a host mutation the preview
            // must not hide — and it is conditional, so say what it depends on.
            if let ClaimResourceKind::FrameworkPlugin {
                framework,
                plugin_id,
            } = &resource.kind
            {
                // A fallback for a driver that records displaced resources but
                // does not implement `plan_disable_restores`. It can only describe
                // the slot veto, because which host state releases an ownership is
                // the driver's business — so it deliberately under-promises rather
                // than claim the restore is unconditional, which is what an earlier
                // version did for the slotless case. A driver with more vetoes must
                // describe its own restores through the hook; OpenClaw does, and
                // this branch is not reached for it.
                let slot_note = match slot {
                    Some(slot) => format!(
                        " unless plugins.slots.{slot} has been given to another plugin or \
                         explicitly closed by then, or the driver's own restore checks step \
                         aside"
                    ),
                    None => " unless the driver's own restore checks step aside".to_string(),
                };
                messages.push(format!(
                    "would re-enable {framework} plugin '{plugin_id}', which this adapter \
                     displaced{slot_note}"
                ));
            }
            continue;
        }
        if !cleanup_ids.contains(&resource.id.as_str()) {
            // Not a resource that disable actually touches (e.g. the
            // framework home/state directory).
            if let ClaimResourceKind::FrameworkConfig { key, .. } = &resource.kind {
                messages.push(format!("config key '{key}' left in place (not reversed)"));
            }
            continue;
        }
        match &resource.kind {
            ClaimResourceKind::FrameworkPlugin {
                framework,
                plugin_id,
            } => {
                messages.push(format!(
                    "would {plugin_verb} {framework} plugin '{plugin_id}'"
                ));
            }
            ClaimResourceKind::FrameworkMarketplace {
                framework,
                marketplace,
            } => {
                messages.push(format!(
                    "would remove {framework} marketplace '{marketplace}'"
                ));
            }
            ClaimResourceKind::Symlink { link, .. } => {
                messages.push(format!("would remove symlink {}", link.display()));
            }
            ClaimResourceKind::ExternalPath { path } => {
                // Hermes and QwenPaw store the plugin as a directory
                // (ExternalPath) but disable also runs a CLI step first —
                // surface it.
                if let Some((cli_resource, verb)) = cli_step
                    && cli_resource == resource.id
                    && let Some(plugin_id) = claim.plugin_id.as_deref()
                {
                    messages.push(format!(
                        "would {verb} {} plugin '{plugin_id}'",
                        claim.framework
                    ));
                }
                messages.push(format!("would remove {}", path.display()));
            }
            _ => {}
        }
    }
    if let Some(restores) = restores {
        messages.extend(restores.iter().cloned());
    }
    messages.push("would remove adapter receipt".to_string());

    DisableReport {
        cleanup_complete: true,
        messages,
    }
}

/// A status report for a receipt that cannot be verified at all (e.g. no
/// driver). Reports `Unknown` rather than faking a healthy/absent verdict.
fn unverified_report(reason: &str) -> AdapterStatusReport {
    AdapterStatusReport {
        summary: AdapterSummary::Unknown,
        conditions: vec![AdapterCondition {
            kind: AdapterConditionKind::VerificationSupported,
            status: ConditionStatus::False,
            reason: Some(reason.to_string()),
            resource: None,
        }],
    }
}

fn source_missing(reason: String) -> SourceProbe {
    SourceProbe {
        status: AdapterSourceStatus::Missing,
        resource_root: None,
        reason: Some(reason.clone()),
        revision: Err(reason),
    }
}

fn source_condition(source: &SourceProbe) -> AdapterCondition {
    AdapterCondition {
        kind: AdapterConditionKind::SourceAvailable,
        status: match source.status {
            AdapterSourceStatus::Available => ConditionStatus::True,
            AdapterSourceStatus::Missing => ConditionStatus::False,
        },
        reason: source.reason.clone(),
        resource: None,
    }
}

fn with_managed_conditions(
    mut report: AdapterStatusReport,
    claim: &AdapterClaim,
    source: &SourceProbe,
    materialized_applicable: bool,
) -> AdapterStatusReport {
    let (managed, revision) = match &source.revision {
        Ok(current) => (
            verify_managed_bundle(current),
            super::managed_files::compare_source_revision(claim, current),
        ),
        Err(reason) => (
            ManagedMatch::Unknown(reason.clone()),
            ManagedMatch::Unknown(reason.clone()),
        ),
    };
    let mut integrity = vec![
        managed_condition(AdapterConditionKind::ManagedBundleMatches, managed),
        managed_condition(AdapterConditionKind::SourceRevisionMatches, revision),
    ];
    if materialized_applicable {
        let materialized = if claim.materialized_files.is_empty() {
            ManagedMatch::Unknown(
                "receipt has no materialized file inventory; re-enable the adapter".into(),
            )
        } else {
            verify_materialized_bundle(claim)
        };
        integrity.push(managed_condition(
            AdapterConditionKind::MaterializedBundleMatches,
            materialized,
        ));
    }

    let has_false = integrity
        .iter()
        .any(|condition| condition.status == ConditionStatus::False);
    let has_unknown = integrity
        .iter()
        .any(|condition| condition.status == ConditionStatus::Unknown);
    if report.summary != AdapterSummary::CleanupFailed {
        if source.status == AdapterSourceStatus::Missing || has_false {
            report.summary = AdapterSummary::Degraded;
        } else if has_unknown && report.summary == AdapterSummary::Healthy {
            report.summary = AdapterSummary::Unknown;
        }
    }
    report.conditions.retain(|condition| {
        !matches!(
            condition.kind,
            AdapterConditionKind::ManagedBundleMatches
                | AdapterConditionKind::SourceRevisionMatches
                | AdapterConditionKind::MaterializedBundleMatches
        )
    });
    report.conditions.insert(0, source_condition(source));
    report.conditions.splice(1..1, integrity);
    report
}

fn managed_condition(kind: AdapterConditionKind, verdict: ManagedMatch) -> AdapterCondition {
    let (status, reason) = match verdict {
        ManagedMatch::Matched => (ConditionStatus::True, None),
        ManagedMatch::Changed(reason) => (ConditionStatus::False, Some(reason)),
        ManagedMatch::Unknown(reason) => (ConditionStatus::Unknown, Some(reason)),
    };
    AdapterCondition {
        kind,
        status,
        reason,
        resource: None,
    }
}

/// Reorder datadir roots so `preferred` is tried first, then the remaining
/// roots in their original order. No-op when `preferred` is `None` or
/// absent from `roots`.
fn prioritize_datadir_root(roots: &[PathBuf], preferred: Option<&Path>) -> Vec<PathBuf> {
    let Some(preferred) = preferred else {
        return roots.to_vec();
    };
    let mut out = Vec::with_capacity(roots.len());
    if roots.iter().any(|r| r.as_path() == preferred) {
        out.push(preferred.to_path_buf());
    }
    for r in roots {
        if r.as_path() != preferred {
            out.push(r.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::state::{InstalledState, ObjectStatus};

    /// Read back the state file the manager wrote (v5 on disk).
    fn load_written_state(path: &std::path::Path) -> StateStore {
        StateStore::load(path, anolisa_platform::privilege::effective_uid()).expect("load state")
    }

    fn test_user_layout(root: &std::path::Path) -> (FsLayout, PathBuf) {
        let home = root.join("home");
        let layout = FsLayout::user_with_overrides(
            home.clone(),
            Some(root.join("user_data_home")),
            None,
            Some(root.join("user_state_home")),
            None,
            None,
        );
        (layout, home)
    }

    #[test]
    fn dsh_home_anchor_survives_environment_root_drift() {
        use crate::adapter::claim::{
            CLAIM_SCHEMA_VERSION, ClaimResource, DRIVER_SCHEMA_VERSION, DshClaim, DshProfileClaim,
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let (layout, _) = test_user_layout(tmp.path());
        let enabled_home = tmp.path().join("first-dsh-home");
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "dsh".to_string(),
            plugin_id: Some("@anolisa/dsh-tokenless".to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-08-16T00:00:00Z".to_string(),
            resource_root: tmp.path().join("bundle"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "dsh_home".to_string(),
                    purpose: "dsh_home".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: enabled_home.clone(),
                    },
                },
                ClaimResource {
                    id: "dsh_plugin_0".to_string(),
                    purpose: "dsh_plugin_profile_web".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: "dsh".to_string(),
                        plugin_id: "@anolisa/dsh-tokenless".to_string(),
                    },
                },
            ],
            driver_payload: DriverPayload::Dsh(DshClaim {
                package_name: "@anolisa/dsh-tokenless".to_string(),
                home_resource: "dsh_home".to_string(),
                profiles: vec![DshProfileClaim {
                    name: "web".to_string(),
                    plugin_resource: "dsh_plugin_0".to_string(),
                }],
            }),
        };
        let mut state = StateStore::empty();
        let initial = ExternalRootTrust {
            target_roots: Vec::new(),
            anchor: None,
            anchor_eligible: false,
        };

        initial.sync_anchor(&mut state, &layout, &claim, &[]);

        let anchored = ExternalRootTrust {
            target_roots: Vec::new(),
            anchor: state
                .find_adapter_trust_root("tokenless", "dsh")
                .map(Path::to_path_buf),
            anchor_eligible: false,
        };
        let mut later_roots = vec![tmp.path().join("second-dsh-home")];
        anchored.extend_allowed_roots("dsh", &mut later_roots);
        assert_eq!(
            later_roots,
            [tmp.path().join("second-dsh-home"), enabled_home]
        );
    }

    /// The framework-agnostic Manager resolves the requirement by precedence
    /// only and never validates it — a present-but-empty value is passed
    /// through verbatim (the owning driver decides validity), and it never
    /// errors for any framework. This keeps the strict OpenClaw emptiness
    /// rule from leaking onto Hermes/Codex/Qoder/Cosh (Issue non-goal).
    #[test]
    fn declared_framework_version_req_is_precedence_only() {
        let compat_wins = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "c"
            version = "1.0.0"
            [[adapters]]
            framework = "openclaw"
            framework_version_req = ">=legacy"
            [adapters.compat]
            framework_version = ">=2026.4.14"
        "#,
        )
        .expect("parse");
        assert_eq!(
            declared_framework_version_req(&compat_wins, "openclaw").as_deref(),
            Some(">=2026.4.14"),
            "compat is primary; legacy is not consulted when compat is present"
        );

        // A present-but-empty compat value is returned as-is (not collapsed to
        // None, not an error) — the driver validates it.
        let empty_compat = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "c"
            version = "1.0.0"
            [[adapters]]
            framework = "hermes"
            [adapters.compat]
            framework_version = ""
        "#,
        )
        .expect("parse");
        assert_eq!(
            declared_framework_version_req(&empty_compat, "hermes").as_deref(),
            Some(""),
            "a non-OpenClaw framework's empty requirement passes through unchanged, never erroring"
        );

        // Legacy field is the fallback only when compat is absent.
        let legacy_only = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "c"
            version = "1.0.0"
            [[adapters]]
            framework = "openclaw"
            framework_version_req = ">=1.2"
        "#,
        )
        .expect("parse");
        assert_eq!(
            declared_framework_version_req(&legacy_only, "openclaw").as_deref(),
            Some(">=1.2")
        );

        // No field at all → None.
        let none = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "c"
            version = "1.0.0"
            [[adapters]]
            framework = "openclaw"
        "#,
        )
        .expect("parse");
        assert_eq!(declared_framework_version_req(&none, "openclaw"), None);
    }

    #[test]
    fn prepend_path_puts_dirs_in_front() {
        let joined = prepend_path_with_existing(
            &[PathBuf::from("/opt/a"), PathBuf::from("/opt/b")],
            Some(std::ffi::OsString::from("/usr/bin:/bin")),
        );
        let dirs: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(dirs[0], PathBuf::from("/opt/a"));
        assert_eq!(dirs[1], PathBuf::from("/opt/b"));
        assert!(dirs.contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn run_capture_captures_stdout_and_exit() {
        let cmd = FrameworkCommand {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "printf hello; exit 0".to_string()],
            stdin: None,
            env_set: Vec::new(),
            env_remove: Vec::new(),
            path_prepend: Vec::new(),
            timeout: Duration::from_secs(5),
        };
        let out = run_capture(&cmd).expect("run");
        assert!(out.success());
        assert_eq!(out.stdout, "hello");
        assert!(!out.timed_out);
    }

    #[test]
    fn run_capture_writes_configured_stdin() {
        let cmd = FrameworkCommand {
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "IFS= read -r answer; [ \"$answer\" = yes ]".to_string(),
            ],
            stdin: Some(b"yes\n".to_vec()),
            env_set: Vec::new(),
            env_remove: Vec::new(),
            path_prepend: Vec::new(),
            timeout: Duration::from_secs(5),
        };
        let out = run_capture(&cmd).expect("run");
        assert!(out.success(), "{out:?}");
    }

    #[test]
    fn run_capture_cleans_up_after_stdin_failure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid_file = tmp.path().join("child.pid");
        let cmd = FrameworkCommand {
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf '%s' \"$$\" > \"$PID_FILE\"; exec 0<&-; exec sleep 30".to_string(),
            ],
            stdin: Some(vec![b'x'; 1024 * 1024]),
            env_set: vec![(
                "PID_FILE".to_string(),
                pid_file.to_string_lossy().into_owned(),
            )],
            env_remove: Vec::new(),
            path_prepend: Vec::new(),
            timeout: Duration::from_secs(5),
        };

        let error = run_capture(&cmd).expect_err("closed stdin must fail");
        assert!(
            matches!(&error, AdapterError::FrameworkCli { reason, .. }
                if reason.contains("failed to write child stdin")),
            "{error:?}"
        );
        let pid = std::fs::read_to_string(&pid_file).expect("child pid");
        let alive = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe child")
            .success();
        assert!(!alive, "child must be reaped after stdin failure");
    }

    #[test]
    fn run_capture_reports_nonzero_exit() {
        let cmd = FrameworkCommand {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "exit 3".to_string()],
            stdin: None,
            env_set: Vec::new(),
            env_remove: Vec::new(),
            path_prepend: Vec::new(),
            timeout: Duration::from_secs(5),
        };
        let out = run_capture(&cmd).expect("run");
        assert_eq!(out.status, Some(3));
        assert!(!out.success());
    }

    #[test]
    fn run_capture_times_out_and_kills() {
        let cmd = FrameworkCommand {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            stdin: None,
            env_set: Vec::new(),
            env_remove: Vec::new(),
            path_prepend: Vec::new(),
            timeout: Duration::from_millis(150),
        };
        let out = run_capture(&cmd).expect("run");
        assert!(out.timed_out, "expected timeout");
        assert!(!out.success());
    }

    #[test]
    fn spawn_failure_is_framework_cli_error() {
        let cmd = FrameworkCommand {
            program: "/no/such/binary/xyz".to_string(),
            args: Vec::new(),
            stdin: None,
            env_set: Vec::new(),
            env_remove: Vec::new(),
            path_prepend: Vec::new(),
            timeout: Duration::from_secs(5),
        };
        let err = run_capture(&cmd).expect_err("spawn must fail");
        assert!(matches!(err, AdapterError::FrameworkCli { .. }));
    }

    // -- run_rpc_capture ------------------------------------------------------

    /// A server that answers one line per request while stdin stays open, then
    /// exits on EOF — the shape `codex app-server --stdio` has.
    fn echo_rpc_session(
        requests: Vec<String>,
        expected: usize,
        timeout: Duration,
    ) -> FrameworkRpcSession {
        FrameworkRpcSession {
            command: FrameworkCommand {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    r#"while IFS= read -r line; do
                         id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
                         printf '{"method":"notify"}\n'
                         printf '{"id":%s,"result":{}}\n' "$id"
                       done"#
                        .to_string(),
                ],
                stdin: None,
                env_set: Vec::new(),
                env_remove: Vec::new(),
                path_prepend: Vec::new(),
                timeout,
            },
            requests,
            expected_responses: expected,
        }
    }

    #[test]
    fn run_rpc_capture_collects_every_reply_and_reaps_the_server() {
        // The point of the RPC runner: a request written after `initialize`
        // still gets answered, because stdin is held open until the replies
        // arrive. Writing both lines and closing stdin — what
        // `FrameworkCommand::stdin` does — loses the second one.
        let session = echo_rpc_session(
            vec![
                r#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#.to_string(),
                r#"{"jsonrpc":"2.0","id":1,"method":"hooks/list"}"#.to_string(),
            ],
            2,
            Duration::from_secs(10),
        );
        let out = run_rpc_capture(&session, JSON_OUTPUT_CAP).expect("session runs");
        assert!(!out.timed_out, "server must exit once stdin closes");
        assert_eq!(out.status, Some(0));
        let ids: Vec<Option<u64>> = out
            .stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|msg| msg.get("id").is_some())
            .map(|msg| msg["id"].as_u64())
            .collect();
        assert_eq!(ids, vec![Some(0), Some(1)]);
    }

    #[test]
    fn run_rpc_capture_times_out_on_a_silent_server() {
        let session = FrameworkRpcSession {
            command: FrameworkCommand {
                program: "/bin/sh".to_string(),
                // Replace the shell so killing the child also closes the
                // captured pipes; the app-server itself is likewise the
                // direct child in production.
                args: vec!["-c".to_string(), "exec sleep 30".to_string()],
                stdin: None,
                env_set: Vec::new(),
                env_remove: Vec::new(),
                path_prepend: Vec::new(),
                timeout: Duration::from_millis(150),
            },
            requests: vec![r#"{"jsonrpc":"2.0","id":1,"method":"hooks/list"}"#.to_string()],
            expected_responses: 1,
        };
        let out = run_rpc_capture(&session, JSON_OUTPUT_CAP).expect("session runs");
        assert!(out.timed_out, "a server that never answers must time out");
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn run_rpc_capture_timeout_unblocks_a_full_stdin_pipe() {
        let session = FrameworkRpcSession {
            command: FrameworkCommand {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "while :; do :; done".to_string()],
                stdin: None,
                env_set: Vec::new(),
                env_remove: Vec::new(),
                path_prepend: Vec::new(),
                timeout: Duration::from_millis(150),
            },
            // Larger than ordinary pipe capacity: a server that never reads
            // stdin leaves the writer blocked until the child is killed.
            requests: vec!["x".repeat(2 * 1024 * 1024)],
            expected_responses: 1,
        };
        let started = Instant::now();
        let out = run_rpc_capture(&session, JSON_OUTPUT_CAP).expect("session runs");
        assert!(out.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "writer join exceeded the bounded timeout: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn run_rpc_capture_returns_when_the_server_dies_early() {
        // A codex too old to know `app-server` exits immediately. The runner
        // must return the (empty) output rather than block for the timeout, so
        // the driver can report the missing capability.
        let session = FrameworkRpcSession {
            command: FrameworkCommand {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "echo 'unrecognized subcommand' >&2; exit 2".to_string(),
                ],
                stdin: None,
                env_set: Vec::new(),
                env_remove: Vec::new(),
                path_prepend: Vec::new(),
                timeout: Duration::from_secs(30),
            },
            requests: vec![r#"{"jsonrpc":"2.0","id":1,"method":"hooks/list"}"#.to_string()],
            expected_responses: 1,
        };
        let out = run_rpc_capture(&session, JSON_OUTPUT_CAP).expect("session runs");
        assert!(!out.timed_out, "must not wait out the timeout");
        assert_eq!(out.status, Some(2));
        assert!(out.stderr.contains("unrecognized subcommand"));
    }

    #[test]
    fn run_rpc_capture_rejects_stdout_over_limit() {
        let session = FrameworkRpcSession {
            command: FrameworkCommand {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    r#"IFS= read -r _; printf '{"id":1,"result":{"padding":"xxxxxxxx"}}\n'"#
                        .to_string(),
                ],
                stdin: None,
                env_set: Vec::new(),
                env_remove: Vec::new(),
                path_prepend: Vec::new(),
                timeout: Duration::from_secs(10),
            },
            requests: vec![r#"{"jsonrpc":"2.0","id":1,"method":"hooks/list"}"#.to_string()],
            expected_responses: 1,
        };
        let error = run_rpc_capture(&session, 16).expect_err("oversized response must fail");
        assert!(error.to_string().contains("exceeded the 16-byte limit"));
    }

    #[test]
    fn run_rpc_capture_rejects_unterminated_stdout_over_limit() {
        let session = FrameworkRpcSession {
            command: FrameworkCommand {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "IFS= read -r _; printf '12345'; exec sleep 30".to_string(),
                ],
                stdin: None,
                env_set: Vec::new(),
                env_remove: Vec::new(),
                path_prepend: Vec::new(),
                timeout: Duration::from_secs(10),
            },
            requests: vec![r#"{"jsonrpc":"2.0","id":1,"method":"hooks/list"}"#.to_string()],
            expected_responses: 1,
        };
        let started = Instant::now();
        let error = run_rpc_capture(&session, 4).expect_err("unterminated stdout must fail");
        assert!(error.to_string().contains("exceeded the 4-byte limit"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "overflow handling exceeded the bounded timeout: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn rpc_response_detection_ignores_notifications() {
        assert!(is_rpc_response(r#"{"id":1,"result":{}}"#));
        assert!(is_rpc_response(r#"{"id":1,"error":{"code":-1}}"#));
        assert!(!is_rpc_response(r#"{"method":"configWarning"}"#));
        assert!(!is_rpc_response(r#"{"id":null,"result":{}}"#));
        assert!(!is_rpc_response("not json at all"));
    }

    #[test]
    fn default_ops_refuse_rpc_sessions_rather_than_degrading() {
        struct PlainOps;
        impl AdapterOps for PlainOps {
            fn run_framework_cli(&self, _cmd: FrameworkCommand) -> Result<CliOutput, AdapterError> {
                unreachable!("not exercised")
            }
            fn copy_tree(&self, _src: &Path, _dst: &Path) -> Result<(), AdapterError> {
                unreachable!("not exercised")
            }
            fn copy_file(&self, _src: &Path, _dst: &Path) -> Result<(), AdapterError> {
                unreachable!("not exercised")
            }
            fn remove_tree(&self, _path: &Path) -> Result<bool, AdapterError> {
                unreachable!("not exercised")
            }
            fn write_file(&self, _path: &Path, _contents: &[u8]) -> Result<(), AdapterError> {
                unreachable!("not exercised")
            }
            fn create_symlink(&self, _link: &Path, _target: &Path) -> Result<(), AdapterError> {
                unreachable!("not exercised")
            }
            fn read_file(&self, _path: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
                unreachable!("not exercised")
            }
        }
        let session = echo_rpc_session(Vec::new(), 0, Duration::from_secs(1));
        let err = PlainOps
            .run_framework_rpc(session)
            .expect_err("the default must refuse");
        assert!(err.to_string().contains("JSON-RPC"), "{err}");
    }

    // -- declared_adapter_type ------------------------------------------------

    fn manifest_with_adapter_type(adapter_type: Option<&str>) -> ComponentManifest {
        use crate::manifest::*;
        ComponentManifest {
            schema_version: CURRENT_SCHEMA_VERSION,
            component: ComponentMeta {
                name: "test-comp".to_string(),
                version: "0.1.0".to_string(),
                layer: "runtime".to_string(),
                domain: None,
                display_name: None,
                owner: None,
                license: None,
                repository: None,
                conflicts: Vec::new(),
            },
            contract: ContractSpec::default(),
            artifact: ArtifactSpec::default(),
            source: SourceSpec::default(),
            distribution_selectors: Vec::new(),
            build: BuildSpec::default(),
            install: InstallSpec::default(),
            backends: ManifestBackends::default(),
            env_requirements: EnvRequirements::default(),
            dependencies: DependenciesSpec::default(),
            runtime_deps: Vec::new(),
            features: Vec::new(),
            adapters: vec![AdapterSpec {
                framework: Some("openclaw".to_string()),
                adapter_type: adapter_type.map(str::to_string),
                ..Default::default()
            }],
            health_check: None,
        }
    }

    #[test]
    fn declared_adapter_type_returns_plugin() {
        let manifest = manifest_with_adapter_type(Some("plugin"));
        assert_eq!(
            declared_adapter_type(&manifest, "openclaw"),
            Some("plugin".to_string())
        );
    }

    #[test]
    fn declared_adapter_type_returns_none_when_absent() {
        let manifest = manifest_with_adapter_type(None);
        assert_eq!(declared_adapter_type(&manifest, "openclaw"), None);
    }

    #[test]
    fn declared_adapter_type_returns_skill_bundle() {
        let manifest = manifest_with_adapter_type(Some("skill_bundle"));
        assert_eq!(
            declared_adapter_type(&manifest, "openclaw"),
            Some("skill_bundle".to_string())
        );
    }

    #[test]
    fn declared_adapter_type_returns_none_for_wrong_framework() {
        let manifest = manifest_with_adapter_type(Some("plugin"));
        assert_eq!(declared_adapter_type(&manifest, "hermes"), None);
    }

    #[test]
    fn unsupported_adapter_type_error_contains_details() {
        let err = AdapterError::UnsupportedAdapterType {
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            adapter_type: "service".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("service"));
        assert!(msg.contains("tokenless"));
        assert!(msg.contains("openclaw"));
        assert!(msg.contains("'plugin', 'skill_bundle', and 'extension'"));
    }

    #[test]
    fn supported_adapter_types_include_extension() {
        assert!(is_supported_adapter_type("plugin"));
        assert!(is_supported_adapter_type("skill_bundle"));
        assert!(is_supported_adapter_type("extension"));
        assert!(!is_supported_adapter_type("service"));
        assert!(!is_supported_adapter_type("magic"));
    }

    #[test]
    fn unsupported_adapter_type_unknown_value() {
        let err = AdapterError::UnsupportedAdapterType {
            component: "agentsight".to_string(),
            framework: "openclaw".to_string(),
            adapter_type: "magic".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("magic"));
        assert!(msg.contains("'plugin', 'skill_bundle', and 'extension'"));
    }

    #[test]
    fn plugin_adapter_type_passes_gate() {
        let manifest = manifest_with_adapter_type(Some("plugin"));
        let at = declared_adapter_type(&manifest, "openclaw");
        let should_reject = at.as_ref().is_some_and(|t| !is_supported_adapter_type(t));
        assert!(!should_reject, "plugin must pass the gate");
    }

    #[test]
    fn absent_adapter_type_passes_gate() {
        let manifest = manifest_with_adapter_type(None);
        let at = declared_adapter_type(&manifest, "openclaw");
        let should_reject = at.as_ref().is_some_and(|t| !is_supported_adapter_type(t));
        assert!(!should_reject, "absent adapter_type must pass the gate");
    }

    #[test]
    fn skill_bundle_adapter_type_passes_gate() {
        let manifest = manifest_with_adapter_type(Some("skill_bundle"));
        let at = declared_adapter_type(&manifest, "openclaw");
        let should_reject = at.as_ref().is_some_and(|t| !is_supported_adapter_type(t));
        assert!(!should_reject, "skill_bundle must pass the gate");
    }

    // -- validate_adapter_type_for_framework ---------------------------------

    #[test]
    fn framework_type_matrix_accepts_valid_pairs() {
        let ok = |fw: &str, at: Option<&str>| {
            validate_adapter_type_for_framework("tokenless", fw, at).is_ok()
        };
        assert!(ok("openclaw", Some("plugin")));
        assert!(ok("openclaw", Some("skill_bundle")));
        assert!(ok("openclaw", None), "openclaw defaults to plugin");
        assert!(ok("hermes", Some("skill_bundle")));
        assert!(ok("codex", Some("plugin")));
        assert!(ok("codex", None), "codex defaults to plugin");
        assert!(ok("claude-code", Some("plugin")));
        assert!(ok("claude-code", None));
        assert!(ok("cosh", Some("extension")));
        assert!(ok("qoder", Some("plugin")));
        assert!(ok("qoder", None), "qoder defaults to plugin");
        assert!(ok("dsh", Some("plugin")));
        assert!(ok("dsh", None), "dsh defaults to plugin");
        assert!(ok("qwencode", Some("extension")));
        assert!(ok("qwenpaw", Some("plugin")));
        assert!(ok("qwenpaw", None), "qwenpaw defaults to plugin");
    }

    #[test]
    fn framework_type_matrix_rejects_extension_on_plugin_frameworks() {
        for fw in [
            "openclaw",
            "hermes",
            "codex",
            "claude-code",
            "qoder",
            "qwenpaw",
        ] {
            let err = validate_adapter_type_for_framework("tokenless", fw, Some("extension"))
                .expect_err(&format!("{fw} + extension must be rejected"));
            assert!(
                matches!(err, AdapterError::InvalidAdapterInput { .. }),
                "{fw}: got {err:?}"
            );
        }
    }

    #[test]
    fn qoder_accepts_plugin_rejects_extension_and_skill_bundle() {
        validate_adapter_type_for_framework("tokenless", "qoder", Some("plugin"))
            .expect("qoder + plugin must pass");
        validate_adapter_type_for_framework("tokenless", "qoder", None)
            .expect("qoder with no adapter_type defaults to plugin");
        for at in [Some("extension"), Some("skill_bundle")] {
            let err = validate_adapter_type_for_framework("tokenless", "qoder", at)
                .expect_err(&format!("qoder + {at:?} must be rejected"));
            assert!(
                matches!(err, AdapterError::InvalidAdapterInput { .. }),
                "qoder + {at:?}: got {err:?}"
            );
        }
    }

    #[test]
    fn framework_type_matrix_rejects_skill_bundle_on_marketplace_frameworks() {
        for fw in ["codex", "claude-code"] {
            let err = validate_adapter_type_for_framework("tokenless", fw, Some("skill_bundle"))
                .expect_err(&format!("{fw} + skill_bundle must be rejected"));
            assert!(matches!(err, AdapterError::InvalidAdapterInput { .. }));
        }
    }

    #[test]
    fn cosh_requires_extension_type() {
        // cosh + plugin, cosh + skill_bundle, and cosh with no adapter_type
        // (which would default to plugin) must all be rejected.
        for at in [Some("plugin"), Some("skill_bundle"), None] {
            let err = validate_adapter_type_for_framework("tokenless", "cosh", at)
                .expect_err(&format!("cosh + {at:?} must be rejected"));
            assert!(
                matches!(err, AdapterError::InvalidAdapterInput { .. }),
                "cosh + {at:?}: got {err:?}"
            );
        }
        validate_adapter_type_for_framework("tokenless", "cosh", Some("extension"))
            .expect("cosh + extension must pass");
    }

    #[test]
    fn qwencode_requires_extension_type() {
        for adapter_type in [Some("plugin"), Some("skill_bundle"), None] {
            let error = validate_adapter_type_for_framework("tokenless", "qwencode", adapter_type)
                .expect_err(&format!("qwencode + {adapter_type:?} must be rejected"));
            assert!(
                matches!(error, AdapterError::InvalidAdapterInput { .. }),
                "qwencode + {adapter_type:?}: got {error:?}"
            );
        }
        validate_adapter_type_for_framework("tokenless", "qwencode", Some("extension"))
            .expect("qwencode + extension must pass");
    }

    #[test]
    fn framework_type_matrix_is_silent_for_unknown_framework() {
        // No built-in driver: this gate defers to UnknownFramework.
        validate_adapter_type_for_framework("tokenless", "gemini", Some("extension"))
            .expect("unknown framework must not be rejected by the type gate");
    }

    #[test]
    fn skill_bundle_with_config_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        std::fs::create_dir_all(&layout.state_dir).expect("mkdir state");
        std::fs::create_dir_all(&layout.log_dir).expect("mkdir log");
        seed_installed_state(
            &layout.state_dir,
            crate::state::InstallMode::System,
            &layout.prefix,
            "os-skills",
            ObjectStatus::Installed,
        );

        let contract = r#"
[component]
name = "os-skills"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "skill_bundle"
dest = "{datadir}/skills"

[adapters.openclaw]
skills = ["install-openclaw"]

[[adapters.openclaw.config]]
key = "plugins.entries.os-skills.enabled"
value = true
"#;
        write_contract_with_content(&layout.datadir, "os-skills", contract);

        let mut manager =
            AdapterManager::new(layout.clone(), Some(tmp.path().join("home")), "test".into());
        manager.visible_roots = vec![VisibleRoot {
            state_dir: layout.state_dir.clone(),
            contract_datadir_roots: vec![layout.datadir.clone()],
        }];
        manager.all_datadir_roots = vec![layout.datadir.clone()];

        let err = manager
            .enable("os-skills", Some("openclaw"), true)
            .expect_err("skill_bundle config must fail fast");
        assert!(
            matches!(err, AdapterError::InvalidAdapterInput { .. }),
            "expected InvalidAdapterInput, got {err:?}"
        );
        assert!(
            err.to_string()
                .contains("skill_bundle adapters do not support framework config"),
            "error must explain unsupported config: {err}"
        );
    }

    // -- scan with Adopted + datadir-only contract ----------------------------

    /// Regression: an RPM-adopted component with no state snapshot but a
    /// datadir contract must still appear as `declared=true` in scan, and
    /// its `adapter_type` must be surfaced.
    #[test]
    fn scan_adopted_component_with_datadir_only_contract() {
        use crate::state::{InstalledObject, ObjectKind, ObjectStatus, Ownership};

        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        // Write installed.toml with an Adopted component, no snapshot.
        let mut state = InstalledState {
            install_mode: crate::state::InstallMode::System,
            prefix: tmp.path().to_path_buf(),
            ..InstalledState::default()
        };
        state.upsert_object(InstalledObject {
            kind: ObjectKind::Component,
            name: "sec-core".to_string(),
            version: "0.1.0".to_string(),
            status: ObjectStatus::Adopted,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some("rpm".to_string()),
            ownership: Some(Ownership::RpmObserved),
            rpm_metadata: None,
            installed_at: "2026-06-18T00:00:00Z".to_string(),
            last_operation_id: None,
            managed: false,
            adopted: true,
            subscription_scope: crate::state::SubscriptionScope::None,
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: Vec::new(),
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        });
        std::fs::create_dir_all(&state_dir).expect("mkdir state");
        state
            .save(&state_dir.join("installed.toml"))
            .expect("save state");

        // Write a datadir contract (no state snapshot).
        let contract = r#"
[component]
name = "sec-core"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "sec-core"
source = "adapters/openclaw"
dest = "{datadir}/adapters/{component}/openclaw/"
"#;
        let contract_dir = datadir.join("components").join("sec-core");
        std::fs::create_dir_all(&contract_dir).expect("mkdir contract");
        std::fs::write(contract_dir.join("component.toml"), contract).expect("write contract");

        // Build a manager pointing at our temp dirs.
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir.clone()];

        let report = manager.scan().expect("scan");

        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan results");
        assert!(
            entry.declared,
            "adopted component with datadir contract must be declared"
        );
        assert_eq!(
            entry.adapter_type.as_deref(),
            Some("plugin"),
            "adapter_type must be surfaced from the contract"
        );
    }

    // -- user/system scope isolation ------------------------------------------

    fn valid_contract_toml(name: &str) -> String {
        format!(
            r#"
[component]
name = "{name}"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "{name}"
source = "adapters/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"
"#
        )
    }

    fn seed_installed_state(
        state_dir: &std::path::Path,
        install_mode: crate::state::InstallMode,
        prefix: &std::path::Path,
        component: &str,
        status: ObjectStatus,
    ) {
        use crate::state::{InstalledObject, ObjectKind, Ownership, SubscriptionScope};

        let mut state = InstalledState {
            install_mode,
            prefix: prefix.to_path_buf(),
            ..InstalledState::default()
        };
        state.upsert_object(InstalledObject {
            kind: ObjectKind::Component,
            name: component.to_string(),
            version: "0.1.0".to_string(),
            status,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some(if status == ObjectStatus::Adopted {
                "rpm".to_string()
            } else {
                "raw".to_string()
            }),
            ownership: Some(if status == ObjectStatus::Adopted {
                Ownership::RpmObserved
            } else {
                Ownership::RawManaged
            }),
            rpm_metadata: None,
            installed_at: "2026-06-18T00:00:00Z".to_string(),
            last_operation_id: None,
            managed: status != ObjectStatus::Adopted,
            adopted: status == ObjectStatus::Adopted,
            subscription_scope: SubscriptionScope::None,
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: Vec::new(),
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        });
        std::fs::create_dir_all(state_dir).expect("mkdir state");
        state
            .save(&state_dir.join("installed.toml"))
            .expect("save state");
    }

    fn write_contract(datadir: &std::path::Path, component: &str) {
        let dir = datadir.join("components").join(component);
        std::fs::create_dir_all(&dir).expect("mkdir contract");
        std::fs::write(dir.join("component.toml"), valid_contract_toml(component))
            .expect("write contract");
    }

    /// Contract TOML with a custom (non-convention) dest path.
    fn contract_toml_with_custom_dest(name: &str, dest: &str) -> String {
        format!(
            r#"
[component]
name = "{name}"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "{name}"
source = "adapters/openclaw"
dest = "{dest}"
"#
        )
    }

    /// Contract TOML without a `dest` field on the adapter entry.
    fn contract_toml_without_dest(name: &str) -> String {
        format!(
            r#"
[component]
name = "{name}"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "{name}"
source = "adapters/openclaw"
"#
        )
    }

    fn write_contract_with_content(datadir: &std::path::Path, component: &str, content: &str) {
        let dir = datadir.join("components").join(component);
        std::fs::create_dir_all(&dir).expect("mkdir contract");
        std::fs::write(dir.join("component.toml"), content).expect("write contract");
    }

    fn openclaw_claim(component: &str, resource_root: PathBuf) -> AdapterClaim {
        use crate::adapter::claim::{
            CLAIM_SCHEMA_VERSION, DRIVER_SCHEMA_VERSION, DriverPayload, OpenClawClaim,
        };

        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: component.to_string(),
            framework: "openclaw".to_string(),
            plugin_id: Some(component.to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-07-09T00:00:00Z".to_string(),
            resource_root,
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            // The payload references `openclaw_plugin`, so the resource has to
            // exist: the driver resolves its own plugin through that reference
            // and rejects a receipt that dangles, on the dry-run path as well as
            // the real one. These cases are about a missing adapter *source*, not
            // about receipt shape, so the fixture has to be well-formed.
            resources: vec![crate::adapter::claim::ClaimResource {
                id: "openclaw_plugin".to_string(),
                purpose: "openclaw_plugin".to_string(),
                kind: crate::adapter::claim::ClaimResourceKind::FrameworkPlugin {
                    framework: "openclaw".to_string(),
                    plugin_id: component.to_string(),
                },
            }],
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "openclaw_state".to_string(),
                plugin_resource: "openclaw_plugin".to_string(),
                skill_resources: Vec::new(),
                config_resources: Vec::new(),

                displaced_plugins: Vec::new(),
            }),
        }
    }

    #[test]
    fn unavailable_authoritative_inventory_never_reports_healthy() {
        let claim = openclaw_claim("tokenless", PathBuf::from("/missing/source"));
        let reason = "native package file query failed; re-enable the adapter".to_string();
        let source = SourceProbe {
            status: AdapterSourceStatus::Available,
            resource_root: Some(claim.resource_root.clone()),
            reason: None,
            revision: Err(reason),
        };
        let report = with_managed_conditions(
            AdapterStatusReport {
                summary: AdapterSummary::Healthy,
                conditions: Vec::new(),
            },
            &claim,
            &source,
            false,
        );

        assert_eq!(report.summary, AdapterSummary::Unknown);
        assert!(report.conditions.iter().any(|condition| {
            condition.kind == AdapterConditionKind::ManagedBundleMatches
                && condition.status == ConditionStatus::Unknown
        }));
        assert!(report.conditions.iter().any(|condition| {
            condition.kind == AdapterConditionKind::SourceRevisionMatches
                && condition.status == ConditionStatus::Unknown
        }));
    }

    #[test]
    fn changed_managed_file_degrades_an_otherwise_healthy_report() {
        use crate::adapter::claim::{AdapterSourceRevision, ManagedFileKind, ManagedSourceFile};

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("plugin.json"), b"changed").expect("managed file");
        let revision = AdapterSourceRevision {
            source_root: tmp.path().to_path_buf(),
            files: vec![ManagedSourceFile {
                relative_path: PathBuf::from("plugin.json"),
                kind: ManagedFileKind::File,
                sha256: Some("0".repeat(64)),
                symlink_target: None,
            }],
            materialized_sources: Vec::new(),
        };
        let mut claim = openclaw_claim("tokenless", tmp.path().to_path_buf());
        claim.source_revision = Some(revision.clone());
        let source = SourceProbe {
            status: AdapterSourceStatus::Available,
            resource_root: Some(tmp.path().to_path_buf()),
            reason: None,
            revision: Ok(revision),
        };
        let report = with_managed_conditions(
            AdapterStatusReport {
                summary: AdapterSummary::Healthy,
                conditions: Vec::new(),
            },
            &claim,
            &source,
            false,
        );

        assert_eq!(report.summary, AdapterSummary::Degraded);
        assert!(report.conditions.iter().any(|condition| {
            condition.kind == AdapterConditionKind::ManagedBundleMatches
                && condition.status == ConditionStatus::False
        }));
    }

    #[test]
    fn missing_materialized_inventory_is_unknown() {
        use crate::adapter::claim::{AdapterSourceRevision, ManagedFileKind, ManagedSourceFile};

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("plugin.json"), b"").expect("managed file");
        let revision = AdapterSourceRevision {
            source_root: tmp.path().to_path_buf(),
            files: vec![ManagedSourceFile {
                relative_path: PathBuf::from("plugin.json"),
                kind: ManagedFileKind::File,
                sha256: Some(
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
                ),
                symlink_target: None,
            }],
            materialized_sources: Vec::new(),
        };
        let mut claim = openclaw_claim("tokenless", tmp.path().to_path_buf());
        claim.source_revision = Some(revision.clone());
        let source = SourceProbe {
            status: AdapterSourceStatus::Available,
            resource_root: Some(tmp.path().to_path_buf()),
            reason: None,
            revision: Ok(revision),
        };
        let report = with_managed_conditions(
            AdapterStatusReport {
                summary: AdapterSummary::Healthy,
                conditions: Vec::new(),
            },
            &claim,
            &source,
            true,
        );

        assert_eq!(report.summary, AdapterSummary::Unknown);
        assert!(report.conditions.iter().any(|condition| {
            condition.kind == AdapterConditionKind::MaterializedBundleMatches
                && condition.status == ConditionStatus::Unknown
        }));
    }

    fn seed_adapter_claim(
        state_dir: &std::path::Path,
        install_mode: crate::state::InstallMode,
        prefix: &std::path::Path,
        claim: AdapterClaim,
    ) {
        let mut state = InstalledState {
            install_mode,
            prefix: prefix.to_path_buf(),
            ..InstalledState::default()
        };
        state.upsert_adapter_claim(claim);
        std::fs::create_dir_all(state_dir).expect("mkdir state");
        state
            .save(&state_dir.join("installed.toml"))
            .expect("save state");
    }

    #[test]
    fn scan_surfaces_orphaned_receipt_when_source_component_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let missing_resource = tmp
            .path()
            .join("data")
            .join("adapters")
            .join("tokenless")
            .join("openclaw");
        seed_adapter_claim(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            openclaw_claim("tokenless", missing_resource),
        );

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir,
            contract_datadir_roots: vec![tmp.path().join("data")],
        }];
        manager.all_datadir_roots = vec![tmp.path().join("data")];

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|entry| entry.component == "tokenless" && entry.framework == "openclaw")
            .expect("orphaned receipt should be listed");

        assert!(entry.enabled);
        assert_eq!(entry.source_status, Some(AdapterSourceStatus::Missing));
        assert!(
            entry
                .source_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("no visible installed component")),
            "source reason should explain missing component, got: {:?}",
            entry.source_reason
        );
    }

    #[test]
    fn status_marks_receipt_degraded_when_source_component_is_missing() {
        use crate::adapter::driver::{AdapterConditionKind, AdapterSummary, ConditionStatus};

        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let missing_resource = tmp
            .path()
            .join("data")
            .join("adapters")
            .join("tokenless")
            .join("openclaw");
        seed_adapter_claim(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            openclaw_claim("tokenless", missing_resource),
        );

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir,
            contract_datadir_roots: vec![tmp.path().join("data")],
        }];
        manager.all_datadir_roots = vec![tmp.path().join("data")];

        let report = manager.status(Some("tokenless")).expect("status");
        let entry = report.entries.first().expect("receipt status");

        assert_eq!(entry.report.summary, AdapterSummary::Degraded);
        let source_condition = entry
            .report
            .conditions
            .iter()
            .find(|condition| condition.kind == AdapterConditionKind::SourceAvailable)
            .expect("source availability condition");
        assert_eq!(source_condition.status, ConditionStatus::False);
        assert!(
            source_condition
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("no visible installed component")),
            "source condition should explain missing component, got: {:?}",
            source_condition.reason
        );
    }

    #[test]
    fn disable_dry_run_works_when_adapter_source_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let missing_resource = tmp
            .path()
            .join("data")
            .join("adapters")
            .join("tokenless")
            .join("openclaw");
        seed_adapter_claim(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            openclaw_claim("tokenless", missing_resource),
        );

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir,
            contract_datadir_roots: vec![tmp.path().join("data")],
        }];
        manager.all_datadir_roots = vec![tmp.path().join("data")];

        let outcome = manager
            .disable("tokenless", Some("openclaw"), true)
            .expect("disable dry-run should not require source root");

        assert!(outcome.dry_run);
        assert!(!outcome.claim_removed);
        assert!(outcome.report.cleanup_complete);
    }

    /// Contract TOML for a cosh extension adapter with a declared bundle
    /// entry and a `{datadir}`-relative dest (the tokenless shape).
    fn contract_toml_cosh_extension(name: &str) -> String {
        format!(
            r#"
[component]
name = "{name}"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "cosh"
adapter_type = "extension"
plugin_id = "{name}"
source = "extensions/{name}"
dest = "{{datadir}}/extensions/{{component}}/"

[adapters.bundle]
entry = "cosh-extension.json"
"#
        )
    }

    fn cosh_claim(component: &str, resource_root: PathBuf) -> AdapterClaim {
        use crate::adapter::claim::{
            CLAIM_SCHEMA_VERSION, CoshClaim, DRIVER_SCHEMA_VERSION, DriverPayload,
        };

        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: component.to_string(),
            framework: "cosh".to_string(),
            plugin_id: Some(component.to_string()),
            adapter_type: Some("extension".to_string()),
            enabled_at: "2026-07-09T00:00:00Z".to_string(),
            resource_root,
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: Vec::new(),
            driver_payload: DriverPayload::Cosh(CoshClaim {
                extension_dir_resource: "cosh_extension_dir".to_string(),
            }),
        }
    }

    /// A stale skeleton left in another datadir root (e.g. by an uninstall
    /// in a different scope) must not mask a removed adapter source: with
    /// the real bundle gone, the receipt's source must go Missing even
    /// though the second root still has a hollow directory of the same
    /// name (issue reproduced by AgenticOS Nightly).
    #[test]
    fn stale_skeleton_in_second_root_does_not_mask_missing_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let rpm_datadir = tmp.path().join("data-rpm");
        let raw_datadir = tmp.path().join("data-raw");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "tokenless",
            ObjectStatus::Installed,
        );
        write_contract_with_content(
            &rpm_datadir,
            "tokenless",
            &contract_toml_cosh_extension("tokenless"),
        );

        // Real bundle in the first root; hollow leftover in the second.
        let bundle_root = rpm_datadir.join("extensions").join("tokenless");
        std::fs::create_dir_all(&bundle_root).expect("bundle dir");
        std::fs::write(bundle_root.join("cosh-extension.json"), b"{}").expect("marker");
        let skeleton = raw_datadir.join("extensions").join("tokenless");
        std::fs::create_dir_all(&skeleton).expect("skeleton dir");

        let state_path = state_dir.join("installed.toml");
        let mut state = load_written_state(&state_path);
        state.upsert_adapter_claim(cosh_claim("tokenless", bundle_root.clone()));
        state.save(&state_path).expect("save claim");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_path;
        manager.visible_roots = vec![VisibleRoot {
            state_dir,
            contract_datadir_roots: vec![rpm_datadir.clone(), raw_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![rpm_datadir, raw_datadir];

        // With the real bundle present the source is available.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "cosh")
            .expect("cosh row");
        assert_eq!(entry.source_status, Some(AdapterSourceStatus::Available));

        // Remove the real bundle; the skeleton alone must not keep the
        // source alive.
        std::fs::remove_dir_all(&bundle_root).expect("remove bundle");
        let report = manager.scan().expect("scan after removal");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "cosh")
            .expect("cosh row after removal");
        assert_eq!(entry.source_status, Some(AdapterSourceStatus::Missing));
        assert!(
            entry
                .source_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("not a valid bundle")),
            "reason should call out the invalid bundle, got: {:?}",
            entry.source_reason
        );
        assert!(
            entry.resource_root.is_none(),
            "a hollow skeleton must not surface as the resource root"
        );
    }

    /// enable's resource-root resolution must prefer the root holding a
    /// real bundle over an earlier root that only has a stale skeleton.
    #[test]
    fn resolve_resource_root_prefers_valid_bundle_over_skeleton() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let first_datadir = tmp.path().join("data-first");
        let second_datadir = tmp.path().join("data-second");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "tokenless",
            ObjectStatus::Installed,
        );
        write_contract_with_content(
            &first_datadir,
            "tokenless",
            &contract_toml_cosh_extension("tokenless"),
        );

        // Skeleton in the contract-origin root, real bundle in the other.
        let skeleton = first_datadir.join("extensions").join("tokenless");
        std::fs::create_dir_all(&skeleton).expect("skeleton dir");
        let bundle_root = second_datadir.join("extensions").join("tokenless");
        std::fs::create_dir_all(&bundle_root).expect("bundle dir");
        std::fs::write(bundle_root.join("cosh-extension.json"), b"{}").expect("marker");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![first_datadir.clone(), second_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![first_datadir, second_datadir];

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("tokenless", &state)
            .expect("load manifest");
        let (resolved, _) = manager
            .resolve_resource_root(
                "tokenless",
                "cosh",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve");
        assert_eq!(
            resolved, bundle_root,
            "resolution must skip the skeleton and pick the real bundle"
        );
    }

    /// A root satisfying only part of a driver's mandatory file set (qoder
    /// needs plugin manifest AND hooks.json) is not a valid bundle and must
    /// not shadow a complete bundle in a later root.
    #[test]
    fn resolve_resource_root_skips_incomplete_qoder_bundle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let first_datadir = tmp.path().join("data-first");
        let second_datadir = tmp.path().join("data-second");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "tokenless",
            ObjectStatus::Installed,
        );
        let contract = r#"
[component]
name = "tokenless"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "qoder"
adapter_type = "plugin"
plugin_id = "tokenless"
source = "adapters/qoder"
dest = "{datadir}/adapters/{component}/qoder/"

[adapters.bundle]
entry = ".qoder-plugin/plugin.json"
"#;
        write_contract_with_content(&first_datadir, "tokenless", contract);

        // First root: manifest only — read_bundle would reject it for the
        // missing hooks.json. Second root: complete bundle.
        let incomplete = first_datadir.join("adapters/tokenless/qoder");
        std::fs::create_dir_all(incomplete.join(".qoder-plugin")).expect("incomplete dir");
        std::fs::write(incomplete.join(".qoder-plugin/plugin.json"), b"{}").expect("manifest");
        let complete = second_datadir.join("adapters/tokenless/qoder");
        std::fs::create_dir_all(complete.join(".qoder-plugin")).expect("complete dir");
        std::fs::write(complete.join(".qoder-plugin/plugin.json"), b"{}").expect("manifest");
        std::fs::write(complete.join("hooks.json"), b"{}").expect("hooks");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![first_datadir.clone(), second_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![first_datadir, second_datadir];

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("tokenless", &state)
            .expect("load manifest");
        let (resolved, _) = manager
            .resolve_resource_root(
                "tokenless",
                "qoder",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve");
        assert_eq!(
            resolved, complete,
            "an incomplete qoder bundle must not shadow the complete one"
        );
    }

    /// Convention-path scan rows (no contract dest) must not surface a
    /// hollow directory as a present resource either.
    #[test]
    fn scan_convention_row_hides_skeleton_resource() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "tokenless",
            ObjectStatus::Installed,
        );
        write_contract_with_content(
            &datadir,
            "tokenless",
            &contract_toml_without_dest("tokenless"),
        );
        // Empty conventional directory — a leftover, not a bundle.
        std::fs::create_dir_all(datadir.join("adapters/tokenless/openclaw")).expect("skeleton");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir,
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir.clone()];

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "openclaw")
            .expect("row");
        assert!(
            entry.resource_root.is_none(),
            "an empty convention directory must not show as a present resource"
        );

        // A real (non-empty) bundle in the same place is surfaced again.
        std::fs::write(
            datadir.join("adapters/tokenless/openclaw/openclaw.plugin.json"),
            b"{}",
        )
        .expect("bundle file");
        let report = manager.scan().expect("scan with bundle");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "openclaw")
            .expect("row with bundle");
        assert_eq!(
            entry.resource_root.as_deref(),
            Some(datadir.join("adapters/tokenless/openclaw").as_path()),
            "a real bundle must still be surfaced"
        );
    }

    /// discover_all() dedupes on (component, framework) first-wins, so a
    /// leading skeleton root used to swallow the key before validation
    /// could consider later roots. Scan must re-select across all roots.
    #[test]
    fn scan_convention_prefers_valid_bundle_in_later_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let first_datadir = tmp.path().join("data-first");
        let second_datadir = tmp.path().join("data-second");

        // Pure directory discovery: no installed state, no contract.
        std::fs::create_dir_all(&state_dir).expect("mkdir state");
        InstalledState::default()
            .save(&state_dir.join("installed.toml"))
            .expect("save empty state");

        // First root: empty skeleton. Second root: real bundle.
        std::fs::create_dir_all(first_datadir.join("adapters/tokenless/openclaw"))
            .expect("skeleton");
        let bundle = second_datadir.join("adapters/tokenless/openclaw");
        std::fs::create_dir_all(&bundle).expect("bundle dir");
        std::fs::write(bundle.join("openclaw.plugin.json"), b"{}").expect("bundle file");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir,
            contract_datadir_roots: vec![first_datadir.clone(), second_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![first_datadir, second_datadir];

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "openclaw")
            .expect("row");
        assert_eq!(
            entry.resource_root.as_deref(),
            Some(bundle.as_path()),
            "a leading skeleton must not swallow the valid bundle in a later root"
        );
    }

    /// A convention root that satisfies the driver's native marker but
    /// lacks the contract-declared bundle entry must not be surfaced:
    /// enable and the source probe honor the declared entry, so scan
    /// showing that root would advertise a path they reject.
    #[test]
    fn scan_convention_declared_entry_overrides_native_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "tokenless",
            ObjectStatus::Installed,
        );
        let contract = r#"
[component]
name = "tokenless"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "cosh"
adapter_type = "extension"
source = "adapters/cosh"

[adapters.bundle]
entry = "custom-entry.json"
"#;
        write_contract_with_content(&datadir, "tokenless", contract);

        // The native cosh manifest passes the default probe, but the
        // contract-declared entry is missing.
        let convention = datadir.join("adapters/tokenless/cosh");
        std::fs::create_dir_all(&convention).expect("convention dir");
        std::fs::write(convention.join("cosh-extension.json"), b"{}").expect("native manifest");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.path().join("home")), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir,
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir.clone()];

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "cosh")
            .expect("row");
        assert!(
            entry.resource_root.is_none(),
            "a root missing the declared bundle entry must not be surfaced"
        );

        // Adding the declared entry makes the root valid again.
        std::fs::write(convention.join("custom-entry.json"), b"{}").expect("declared entry");
        let report = manager.scan().expect("scan with entry");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "cosh")
            .expect("row with entry");
        assert_eq!(
            entry.resource_root.as_deref(),
            Some(convention.as_path()),
            "the root must reappear once the declared entry exists"
        );
    }

    // -- contract-driven resource root discovery --------------------------------

    /// Convention path still works when manifest has no dest or no manifest.
    #[test]
    fn convention_path_works_without_dest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "tokenless",
            ObjectStatus::Installed,
        );

        // Contract without dest.
        write_contract_with_content(
            &datadir,
            "tokenless",
            &contract_toml_without_dest("tokenless"),
        );

        // Convention resource directory.
        let convention = datadir.join("adapters").join("tokenless").join("openclaw");
        std::fs::create_dir_all(&convention).expect("mkdir convention");
        std::fs::write(convention.join("plugin.json"), b"{}").expect("write");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir.clone()];

        // scan: resource should be found at convention path.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "openclaw")
            .expect("tokenless/openclaw should be in scan");
        assert!(entry.declared, "must be declared");
        assert!(
            entry.resource_root.is_some(),
            "convention resource must be found"
        );
        assert_eq!(
            entry.resource_root.as_ref().unwrap(),
            &convention,
            "resource root must be the convention path"
        );
    }

    /// Convention path still works when there is no manifest at all, only
    /// resource directories (pure directory discovery).
    #[test]
    fn convention_path_works_without_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        // No installed state, no contract — just a resource directory.
        std::fs::create_dir_all(&state_dir).expect("mkdir state");
        InstalledState::default()
            .save(&state_dir.join("installed.toml"))
            .expect("save empty state");

        let convention = datadir.join("adapters").join("tokenless").join("openclaw");
        std::fs::create_dir_all(&convention).expect("mkdir convention");
        // Discovery only surfaces real bundles; an empty directory would be
        // treated as a stale skeleton, so give the bundle some content.
        std::fs::write(convention.join("openclaw.plugin.json"), b"{}").expect("bundle file");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir.clone()];

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "openclaw")
            .expect("tokenless/openclaw should be found by directory discovery");
        assert!(!entry.declared, "no manifest — must not be declared");
        assert!(
            entry.resource_root.is_some(),
            "convention resource must be found by directory discovery"
        );
    }

    #[test]
    fn convention_discovery_ignores_shared_resource_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        std::fs::create_dir_all(&state_dir).expect("mkdir state");
        InstalledState::default()
            .save(&state_dir.join("installed.toml"))
            .expect("save empty state");

        let adapters = datadir.join("adapters/tokenless");
        let openclaw = adapters.join("openclaw");
        std::fs::create_dir_all(&openclaw).expect("mkdir openclaw adapter");
        std::fs::write(openclaw.join("openclaw.plugin.json"), b"{}").expect("adapter manifest");

        let common = adapters.join("common/hooks");
        std::fs::create_dir_all(&common).expect("mkdir shared hooks");
        std::fs::write(common.join("rewrite.sh"), b"#!/bin/sh\n").expect("shared hook");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir,
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir];

        let report = manager.scan().expect("scan");
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry.component == "tokenless" && entry.framework == "openclaw"),
            "real framework adapter must remain discoverable"
        );
        assert!(
            report
                .entries
                .iter()
                .all(|entry| entry.component != "tokenless" || entry.framework != "common"),
            "shared common resources must not be reported as an adapter"
        );
    }

    /// Custom dest from contract is used for resource root when directory exists.
    #[test]
    fn declared_custom_dest_is_used() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        // Contract with custom dest.
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_custom_dest("sec-core", "{datadir}/custom/sec-core/openclaw/"),
        );

        // Resource at the custom location (not the convention path).
        let custom_root = datadir.join("custom").join("sec-core").join("openclaw");
        std::fs::create_dir_all(&custom_root).expect("mkdir custom");
        std::fs::write(custom_root.join("plugin.json"), b"{}").expect("write");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir.clone()];

        // scan: resource_root must use the custom dest path.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan");
        assert!(entry.declared);
        assert_eq!(
            entry.resource_root.as_ref(),
            Some(&custom_root),
            "scan must use the contract-declared dest, not convention"
        );

        // resolve_resource_root (used by enable) must return the custom path.
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let (resolved, _effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve");
        assert_eq!(
            resolved, custom_root,
            "enable resource root must be the contract dest"
        );
    }

    /// Contract TOML with a raw `dest` plus an RPM backend resource root.
    fn contract_toml_with_rpm_root(name: &str, rpm_root: &str) -> String {
        format!(
            r#"
[component]
name = "{name}"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "{name}"
source = "adapters/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"

[adapters.backends.rpm]
resource_root = "{rpm_root}"
"#
        )
    }

    /// Manager over one visible root, mirroring the scan/enable fixtures.
    fn manager_for(
        tmp: &std::path::Path,
        state_dir: &std::path::Path,
        datadir: &std::path::Path,
    ) -> AdapterManager {
        let layout = FsLayout::system(Some(tmp.to_path_buf()));
        let mut manager = AdapterManager::new(layout, Some(tmp.to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.to_path_buf(),
            contract_datadir_roots: vec![datadir.to_path_buf()],
        }];
        manager.all_datadir_roots = vec![datadir.to_path_buf()];
        manager
    }

    /// An RPM-installed component reads its bundle from the contract's
    /// `[adapters.backends.rpm].resource_root`, not the raw `dest`.
    #[test]
    fn rpm_component_uses_declared_rpm_resource_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        // Adopted ⇒ RPM provenance in the migrated domain record.
        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        let rpm_root = tmp.path().join("opt/agent-sec/openclaw-plugin");
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_rpm_root("sec-core", &rpm_root.to_string_lossy()),
        );
        // Bundle exists only at the RPM-provided path — the raw dest was
        // never laid down because the payload came from the RPM.
        std::fs::create_dir_all(&rpm_root).expect("mkdir rpm root");
        std::fs::write(rpm_root.join("plugin.json"), b"{}").expect("write");

        let manager = manager_for(tmp.path(), &state_dir, &datadir);

        // scan must surface the RPM root.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan");
        assert!(entry.declared);
        assert_eq!(
            entry.resource_root.as_ref(),
            Some(&rpm_root),
            "scan must select the RPM backend resource root"
        );

        // resolve_resource_root (used by enable) must return the RPM root.
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        assert!(rpm_provenance, "adopted RPM record must carry provenance");
        let (resolved, _effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve");
        assert_eq!(
            resolved, rpm_root,
            "enable resource root must be the RPM backend root"
        );
    }

    /// A declared RPM root that does not exist is an error — never a
    /// silent fallback to the raw dest, even when that directory exists.
    #[test]
    fn rpm_component_missing_rpm_root_errors_without_dest_fallback() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        let rpm_root = tmp.path().join("opt/agent-sec/openclaw-plugin");
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_rpm_root("sec-core", &rpm_root.to_string_lossy()),
        );
        // Only the raw dest holds a bundle — e.g. a stale leftover from an
        // earlier raw install. It must not shadow the missing RPM payload.
        let dest_root = datadir.join("adapters/sec-core/openclaw");
        std::fs::create_dir_all(&dest_root).expect("mkdir dest");
        std::fs::write(dest_root.join("plugin.json"), b"{}").expect("write");

        let manager = manager_for(tmp.path(), &state_dir, &datadir);

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan");
        assert!(entry.declared);
        assert!(
            entry.resource_root.is_none(),
            "scan must not fall back to the raw dest for an RPM install"
        );

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let err = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect_err("missing RPM root must fail, not fall back");
        match &err {
            AdapterError::ContractResourceRootNotFound { path, .. } => {
                assert_eq!(path, &rpm_root, "error must point at the missing RPM root");
            }
            other => panic!("expected ContractResourceRootNotFound, got: {other}"),
        }
    }

    /// Blank RPM root in scan: the declaration must surface as declared
    /// with no usable resource, even when a stale raw `dest` bundle still
    /// exists — scan must agree with the enable-time rejection instead of
    /// advertising a root enable will refuse.
    #[test]
    fn scan_blank_rpm_root_hides_stale_raw_dest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_rpm_root("sec-core", "   "),
        );
        // Stale raw bundle at the dest — must not be reported as usable.
        let dest_root = datadir.join("adapters/sec-core/openclaw");
        std::fs::create_dir_all(&dest_root).expect("mkdir dest");
        std::fs::write(dest_root.join("plugin.json"), b"{}").expect("write");

        let manager = manager_for(tmp.path(), &state_dir, &datadir);
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan");
        assert!(entry.declared, "declaration must stay visible");
        assert!(
            entry.resource_root.is_none(),
            "blank RPM root must not fall back to the stale raw dest, got {:?}",
            entry.resource_root
        );
    }

    /// A declared-but-blank RPM root is a contract defect: resolution must
    /// fail closed instead of silently selecting the raw dest.
    #[test]
    fn rpm_component_blank_rpm_root_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_rpm_root("sec-core", "  "),
        );
        // A valid bundle at the raw dest must NOT rescue the blank root.
        let dest_root = datadir.join("adapters/sec-core/openclaw");
        std::fs::create_dir_all(&dest_root).expect("mkdir dest");
        std::fs::write(dest_root.join("plugin.json"), b"{}").expect("write");

        let manager = manager_for(tmp.path(), &state_dir, &datadir);
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let err = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect_err("blank rpm root must fail, not fall back to dest");
        match &err {
            AdapterError::InvalidAdapterInput { reason, .. } => {
                assert!(
                    reason.contains("resource_root") && reason.contains("empty"),
                    "reason must name the defect: {reason}"
                );
            }
            other => panic!("expected InvalidAdapterInput, got: {other}"),
        }
    }

    /// `[adapters.backends.rpm].resource_root` vocabulary: absolute paths
    /// and `{datadir}`/`{component}` templates are usable; any other
    /// placeholder classifies as `Unsupported` — it would expand against
    /// the consuming scope's layout, not the contract's.
    #[test]
    fn rpm_root_decl_rejects_non_datadir_placeholders() {
        assert_eq!(
            RpmRootDecl::from_raw(Some("/opt/agent-sec/openclaw-plugin/")),
            RpmRootDecl::Declared("/opt/agent-sec/openclaw-plugin/".to_string())
        );
        assert_eq!(
            RpmRootDecl::from_raw(Some("{datadir}/adapters/{component}/codex/")),
            RpmRootDecl::Declared("{datadir}/adapters/{component}/codex/".to_string())
        );
        match RpmRootDecl::from_raw(Some("{libexecdir}/plugin")) {
            RpmRootDecl::Unsupported { placeholder, .. } => assert_eq!(placeholder, "libexecdir"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        // The alien placeholder wins even next to an allowed one.
        match RpmRootDecl::from_raw(Some("{datadir}/{bindir}/x")) {
            RpmRootDecl::Unsupported { placeholder, .. } => assert_eq!(placeholder, "bindir"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    /// A non-`{datadir}` layout placeholder in the RPM root must fail
    /// closed, never expand against the *consuming* manager's layout:
    /// before this rule, `{libexecdir}/plugin` consumed cross-scope (e.g.
    /// a user-mode manager reading a system RPM contract) resolved under
    /// the consumer's prefix — misreporting the RPM payload as missing or
    /// selecting a caller-writable bundle. A valid bundle planted at
    /// exactly that wrong-scope path must not be selected.
    #[test]
    fn rpm_component_non_datadir_placeholder_rejected_not_wrong_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_rpm_root("sec-core", "{libexecdir}/plugin"),
        );
        // Bundle at the consuming layout's libexecdir — where the old
        // expansion would have landed. It must stay unreachable.
        let manager = manager_for(tmp.path(), &state_dir, &datadir);
        let wrong_scope = manager.layout.libexec_dir.join("plugin");
        std::fs::create_dir_all(&wrong_scope).expect("mkdir wrong-scope bundle");
        std::fs::write(wrong_scope.join("plugin.json"), b"{}").expect("write");

        // scan: declared, but no usable resource.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan");
        assert!(entry.declared, "declaration must stay visible");
        assert!(
            entry.resource_root.is_none(),
            "an unsupported placeholder must not resolve, got {:?}",
            entry.resource_root
        );

        // enable-time resolution: hard error naming the placeholder.
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let err = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect_err("unsupported placeholder must fail, not resolve wrong-scope");
        match &err {
            AdapterError::InvalidAdapterInput { reason, .. } => {
                assert!(
                    reason.contains("libexecdir"),
                    "reason must name the placeholder: {reason}"
                );
            }
            other => panic!("expected InvalidAdapterInput, got: {other}"),
        }
    }

    /// A relative RPM root must be rejected before any filesystem probe:
    /// probing it would resolve against the process CWD, so whoever
    /// controls the working directory would control which bundle is read.
    /// A valid bundle planted at exactly that CWD-relative path must not
    /// be selected.
    #[test]
    fn rpm_component_relative_root_rejected_even_with_cwd_bundle() {
        /// Removes the CWD bundle even when an assertion fails.
        struct CwdBundle(std::path::PathBuf);
        impl Drop for CwdBundle {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let rel_name = format!("anolisa-test-cwd-bundle-{}", std::process::id());
        let cwd_bundle = std::env::current_dir().expect("cwd").join(&rel_name);
        std::fs::create_dir_all(&cwd_bundle).expect("mkdir cwd bundle");
        std::fs::write(cwd_bundle.join("plugin.json"), b"{}").expect("write");
        let _cleanup = CwdBundle(cwd_bundle);

        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");
        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_rpm_root("sec-core", &rel_name),
        );

        let manager = manager_for(tmp.path(), &state_dir, &datadir);

        // scan: declared, but the relative root is not a usable resource.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan");
        assert!(
            entry.resource_root.is_none(),
            "a relative root must not resolve against the CWD, got {:?}",
            entry.resource_root
        );

        // enable-time resolution: rejected before any filesystem probe.
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let err = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect_err("relative root must fail even with a valid CWD bundle");
        match &err {
            AdapterError::RelativeTemplateExpansion { path, .. } => {
                assert_eq!(path, &PathBuf::from(&rel_name));
            }
            other => panic!("expected RelativeTemplateExpansion, got: {other}"),
        }
    }

    /// A raw-installed component keeps using `dest` even when the contract
    /// also declares an RPM backend root (unified contract, raw install).
    #[test]
    fn raw_component_ignores_rpm_resource_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        // Installed ⇒ raw-managed provenance.
        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Installed,
        );

        let rpm_root = tmp.path().join("opt/agent-sec/openclaw-plugin");
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_rpm_root("sec-core", &rpm_root.to_string_lossy()),
        );
        // Bundles exist at both roots; the raw install must pick dest.
        let dest_root = datadir.join("adapters/sec-core/openclaw");
        for root in [&dest_root, &rpm_root] {
            std::fs::create_dir_all(root).expect("mkdir");
            std::fs::write(root.join("plugin.json"), b"{}").expect("write");
        }

        let manager = manager_for(tmp.path(), &state_dir, &datadir);

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        assert!(!rpm_provenance, "raw install must not carry RPM provenance");
        let (resolved, _effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve");
        assert_eq!(
            resolved, dest_root,
            "raw install must keep the raw dest resource root"
        );
    }

    /// Declared dest with missing directory: scan shows absent, enable returns error.
    #[test]
    fn declared_dest_missing_directory_reports_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        // Contract with custom dest, but DO NOT create the directory.
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_custom_dest("sec-core", "{datadir}/custom/sec-core/openclaw/"),
        );

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir.clone()];

        // scan: declared yes, resource absent.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan");
        assert!(entry.declared, "must be declared from contract");
        assert!(
            entry.resource_root.is_none(),
            "resource_root must be None when dest directory does not exist"
        );

        // resolve_resource_root: must return ContractResourceRootNotFound,
        // NOT silently fall back to convention.
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let err = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect_err("must fail when contract dest directory is absent");
        assert!(
            matches!(err, AdapterError::ContractResourceRootNotFound { .. }),
            "expected ContractResourceRootNotFound, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("sec-core") && msg.contains("openclaw"),
            "error must mention component and framework: {msg}"
        );
    }

    /// Declared dest with missing directory must NOT fall back to convention
    /// even when convention directory exists.
    #[test]
    fn declared_dest_missing_does_not_fallback_to_convention() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let datadir = tmp.path().join("data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        // Contract with custom dest — directory does NOT exist.
        write_contract_with_content(
            &datadir,
            "sec-core",
            &contract_toml_with_custom_dest("sec-core", "{datadir}/custom/sec-core/openclaw/"),
        );

        // Convention path DOES exist (should be ignored because contract is
        // authoritative).
        let convention = datadir.join("adapters").join("sec-core").join("openclaw");
        std::fs::create_dir_all(&convention).expect("mkdir convention");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![datadir.clone()],
        }];
        manager.all_datadir_roots = vec![datadir.clone()];

        // scan: declared yes, resource absent (convention exists but ignored).
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan");
        assert!(entry.declared);
        assert!(
            entry.resource_root.is_none(),
            "resource_root must be None — convention path must not be used when contract dest is absent"
        );

        // resolve_resource_root must error, not fall back.
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let err = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect_err("must not fall back to convention");
        assert!(
            matches!(err, AdapterError::ContractResourceRootNotFound { .. }),
            "expected ContractResourceRootNotFound, got: {err}"
        );
    }

    /// User-mode manager can discover contract-defined resource root from
    /// a system-installed/adopted component.
    #[test]
    fn user_mode_uses_system_contract_dest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (user_layout, user_home) = test_user_layout(tmp.path());
        let user_state = user_layout.state_dir.clone();
        let user_data = user_layout.datadir.clone();
        let system_layout = FsLayout::system(Some(tmp.path().join("system")));
        let sys_state = system_layout.state_dir.clone();
        let sys_data = system_layout.datadir.clone();

        // System has sec-core adopted with a custom dest.
        seed_installed_state(
            &sys_state,
            crate::state::InstallMode::System,
            &system_layout.prefix,
            "sec-core",
            ObjectStatus::Adopted,
        );
        write_contract_with_content(
            &sys_data,
            "sec-core",
            &contract_toml_with_custom_dest("sec-core", "{datadir}/custom/sec-core/openclaw/"),
        );
        // Resource in system datadir at the custom location.
        let custom_root = sys_data.join("custom").join("sec-core").join("openclaw");
        std::fs::create_dir_all(&custom_root).expect("mkdir custom");
        std::fs::write(custom_root.join("plugin.json"), b"{}").expect("write");

        // User state is empty.
        std::fs::create_dir_all(&user_state).expect("mkdir user state");
        StateStore::empty_for_layout(&user_layout)
            .save(&user_state.join("installed.toml"))
            .expect("save empty user state");

        let mut manager = AdapterManager::new(user_layout, Some(user_home), "test".into());
        manager.visible_roots = vec![
            VisibleRoot {
                state_dir: user_state,
                contract_datadir_roots: vec![user_data],
            },
            VisibleRoot {
                state_dir: sys_state,
                contract_datadir_roots: vec![sys_data.clone()],
            },
        ];
        manager.all_datadir_roots = vec![sys_data];

        // scan: user-mode must discover sec-core from the system root,
        // with resource_root pointing to the contract-declared custom path.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be in scan via system root");
        assert!(entry.declared);
        assert_eq!(
            entry.resource_root.as_ref(),
            Some(&custom_root),
            "user-mode scan must find contract-declared resource root from system scope"
        );
    }

    /// User-mode `resolve_skill_sources` expands `{datadir}` to the
    /// system datadir (the scope where the component contract lives),
    /// not to the user-mode layout's datadir.
    #[test]
    fn user_mode_skill_source_uses_system_datadir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (user_layout, user_home) = test_user_layout(tmp.path());
        let user_state = user_layout.state_dir.clone();
        let user_data = user_layout.datadir.clone();
        let system_layout = FsLayout::system(Some(tmp.path().join("system")));
        let sys_state = system_layout.state_dir.clone();
        let sys_data = system_layout.datadir.clone();

        seed_installed_state(
            &sys_state,
            crate::state::InstallMode::System,
            &system_layout.prefix,
            "sec-core",
            ObjectStatus::Adopted,
        );

        let contract = r#"
[component]
name = "sec-core"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "sec-core"
dest = "{datadir}/custom/sec-core/openclaw/"

[[adapters.openclaw.skills]]
name = "code-scanner"
source = "{datadir}/skills/code-scanner/"
"#;
        write_contract_with_content(&sys_data, "sec-core", contract);

        // Resource root and skill source in system datadir.
        let custom_root = sys_data.join("custom").join("sec-core").join("openclaw");
        std::fs::create_dir_all(&custom_root).expect("mkdir custom");
        std::fs::write(custom_root.join("plugin.json"), b"{}").expect("write");
        let skill_source = sys_data.join("skills").join("code-scanner");
        std::fs::create_dir_all(&skill_source).expect("mkdir skill source");
        std::fs::write(skill_source.join("manifest.json"), b"{}").expect("write");

        // User state empty.
        std::fs::create_dir_all(&user_state).expect("mkdir user state");
        StateStore::empty_for_layout(&user_layout)
            .save(&user_state.join("installed.toml"))
            .expect("save empty user state");

        let mut manager = AdapterManager::new(user_layout, Some(user_home), "test".into());
        manager.visible_roots = vec![
            VisibleRoot {
                state_dir: user_state,
                contract_datadir_roots: vec![user_data],
            },
            VisibleRoot {
                state_dir: sys_state,
                contract_datadir_roots: vec![sys_data.clone()],
            },
        ];
        manager.all_datadir_roots = vec![sys_data.clone()];

        // Resolve resource root — must come from system datadir.
        let state = load_written_state(&manager.state_path);
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let (resource_root, effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve resource root");
        assert_eq!(resource_root, custom_root);
        assert_eq!(
            effective_datadir, sys_data,
            "effective datadir must be the system datadir"
        );

        // Resolve skill sources — {datadir} must expand to sys_data.
        let skill_specs = declared_skills(&manifest, "openclaw");
        let skills = resolve_skill_sources(
            skill_specs,
            &manager.layout,
            &effective_datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect("resolve skills");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "code-scanner");
        assert_eq!(
            skills[0].source.as_ref(),
            Some(&skill_source),
            "skill source must resolve to system datadir path, not user datadir"
        );
    }

    /// User component must NOT fall back to system datadir contract.
    #[test]
    fn user_component_does_not_fallback_to_system_contract() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (user_layout, user_home) = test_user_layout(tmp.path());
        let user_state = user_layout.state_dir.clone();
        let user_data = user_layout.datadir.clone();
        let system_layout = FsLayout::system(Some(tmp.path().join("system")));
        let sys_state = system_layout.state_dir.clone();
        let sys_data = system_layout.datadir.clone();

        // User state has tokenless installed, no contract anywhere in user scope.
        seed_installed_state(
            &user_state,
            crate::state::InstallMode::User,
            &user_layout.prefix,
            "tokenless",
            ObjectStatus::Installed,
        );
        // System datadir has a valid contract.
        write_contract(&sys_data, "tokenless");

        let mut manager = AdapterManager::new(user_layout, Some(user_home), "test".into());
        manager.visible_roots = vec![
            VisibleRoot {
                state_dir: user_state.clone(),
                contract_datadir_roots: vec![user_data.clone()],
            },
            VisibleRoot {
                state_dir: sys_state.clone(),
                contract_datadir_roots: vec![sys_data.clone()],
            },
        ];
        manager.all_datadir_roots = vec![user_data, sys_data];

        // Scan: tokenless must NOT be declared (no user contract).
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "openclaw");
        assert!(
            entry.is_none() || !entry.unwrap().declared,
            "user component must not use system contract"
        );
        assert!(
            report.warnings.iter().any(|w| w.contains("tokenless")
                && w.contains("no component contract")
                && w.contains("another scope")),
            "scan must warn that user contract is missing and system exists, got: {:?}",
            report.warnings
        );
    }

    /// System component can use system/packaged datadir contract.
    #[test]
    fn system_component_uses_system_datadir_contract() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sys_state = tmp.path().join("sys_state");
        let sys_data = tmp.path().join("sys_data");
        let pkg_data = tmp.path().join("pkg_data");

        seed_installed_state(
            &sys_state,
            crate::state::InstallMode::System,
            tmp.path(),
            "tokenless",
            ObjectStatus::Installed,
        );
        // Contract in pkg_data (simulates /usr/share vs /usr/local/share).
        write_contract(&pkg_data, "tokenless");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = sys_state.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: sys_state.clone(),
            contract_datadir_roots: vec![sys_data, pkg_data.clone()],
        }];
        manager.all_datadir_roots = vec![pkg_data];

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "openclaw")
            .expect("tokenless/openclaw should be declared");
        assert!(
            entry.declared,
            "system component must find contract in packaged datadir"
        );
    }

    /// System-mode manager discovers a package contract under the FHS
    /// `/usr/share/anolisa` tree (simulated via temp dirs) when
    /// `package_datadir` is added to `contract_datadir_roots`.
    #[test]
    fn system_manager_discovers_package_contract_under_usr_share() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let local_datadir = tmp.path().join("usr/local/share/anolisa");
        let package_datadir = tmp.path().join("usr/share/anolisa");
        let state_dir = tmp.path().join("var/lib/anolisa");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        // Contract lives under the package datadir (simulates RPM install).
        write_contract(&package_datadir, "sec-core");

        // Adapter resource directory under the package datadir.
        let adapter_root = package_datadir
            .join("adapters")
            .join("sec-core")
            .join("openclaw");
        std::fs::create_dir_all(&adapter_root).expect("mkdir adapter");
        std::fs::write(adapter_root.join("plugin.json"), b"{}").expect("write");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![local_datadir.clone(), package_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![local_datadir, package_datadir.clone()];

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw")
            .expect("sec-core/openclaw should be discovered from package datadir");
        assert!(
            entry.declared,
            "contract from package datadir must be declared"
        );
        assert!(
            entry.resource_root.is_some(),
            "resource root must be found under package datadir"
        );

        // Verify resolve_resource_root returns the package datadir path.
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        let (resource_root, effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve resource root");
        assert_eq!(
            effective_datadir, package_datadir,
            "effective datadir must be the package datadir"
        );
        assert!(
            resource_root.starts_with(&package_datadir),
            "resource root must be under the package datadir"
        );
    }

    /// When a contract from the package datadir declares `dest` and
    /// skill `source` using `{datadir}`, the placeholder must expand to
    /// the package datadir — not the local-install datadir.
    #[test]
    fn package_contract_datadir_expands_skill_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let local_datadir = tmp.path().join("usr/local/share/anolisa");
        let package_datadir = tmp.path().join("usr/share/anolisa");
        let state_dir = tmp.path().join("var/lib/anolisa");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        let contract = r#"
[component]
name = "sec-core"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "sec-core"
dest = "{datadir}/adapters/sec-core/openclaw/"

[[adapters.openclaw.skills]]
name = "code-scanner"
source = "{datadir}/skills/code-scanner/"
"#;
        write_contract_with_content(&package_datadir, "sec-core", contract);

        // Create the adapter and skill directories under the package datadir.
        let adapter_root = package_datadir
            .join("adapters")
            .join("sec-core")
            .join("openclaw");
        std::fs::create_dir_all(&adapter_root).expect("mkdir adapter");
        std::fs::write(adapter_root.join("plugin.json"), b"{}").expect("write");
        let skill_source = package_datadir.join("skills").join("code-scanner");
        std::fs::create_dir_all(&skill_source).expect("mkdir skill source");
        std::fs::write(skill_source.join("manifest.json"), b"{}").expect("write skill");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(
            layout.clone(),
            Some(tmp.path().to_path_buf()),
            "test".into(),
        );
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![local_datadir, package_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![package_datadir.clone()];

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");

        // resource root must be under the package datadir.
        let (resource_root, effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve resource root");
        assert_eq!(
            resource_root, adapter_root,
            "resource root must be the package datadir adapter path"
        );
        assert_eq!(
            effective_datadir, package_datadir,
            "effective datadir must be the package datadir"
        );

        // skill source must also resolve under the package datadir.
        let skill_specs = declared_skills(&manifest, "openclaw");
        let skills = resolve_skill_sources(
            skill_specs,
            &layout,
            &effective_datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect("resolve skills");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "code-scanner");
        assert_eq!(
            skills[0].source.as_ref(),
            Some(&skill_source),
            "skill source {{datadir}} must expand to the package datadir"
        );
    }

    /// User-mode scan includes system-installed component via system
    /// visible root (contract resolved from system datadir).
    #[test]
    fn user_scan_includes_system_component_via_system_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (user_layout, user_home) = test_user_layout(tmp.path());
        let user_state = user_layout.state_dir.clone();
        let user_data = user_layout.datadir.clone();
        let system_layout = FsLayout::system(Some(tmp.path().join("system")));
        let sys_state = system_layout.state_dir.clone();
        let sys_data = system_layout.datadir.clone();

        // Only system state has tokenless; contract in system datadir.
        seed_installed_state(
            &sys_state,
            crate::state::InstallMode::System,
            &system_layout.prefix,
            "tokenless",
            ObjectStatus::Installed,
        );
        write_contract(&sys_data, "tokenless");

        // User state is empty.
        std::fs::create_dir_all(&user_state).expect("mkdir user state");
        StateStore::empty_for_layout(&user_layout)
            .save(&user_state.join("installed.toml"))
            .expect("save empty user state");

        let mut manager = AdapterManager::new(user_layout, Some(user_home), "test".into());
        manager.visible_roots = vec![
            VisibleRoot {
                state_dir: user_state,
                contract_datadir_roots: vec![user_data],
            },
            VisibleRoot {
                state_dir: sys_state,
                contract_datadir_roots: vec![sys_data],
            },
        ];

        // scan must find tokenless via the system root.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "tokenless" && e.framework == "openclaw")
            .expect("tokenless/openclaw should be in scan");
        assert!(
            entry.declared,
            "system component must be declared via system root"
        );
    }

    // -- copy_tree / remove_tree boundary ------------------------------------

    #[test]
    fn copy_tree_rejects_source_outside_allowed_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(allowed.join("dst")).expect("mkdir");
        std::fs::create_dir_all(&outside).expect("mkdir");
        std::fs::write(outside.join("x.txt"), b"data").expect("write");

        let ops = ManagerOps::new(
            CentralLog::open(tmp.path().join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed.clone()],
        );
        let err = ops
            .copy_tree(&outside, &allowed.join("dst/target"))
            .expect_err("source outside allowed roots must fail");
        assert!(
            matches!(err, AdapterError::ClaimValidation(_)),
            "expected ClaimValidation, got {err:?}"
        );
    }

    #[test]
    fn copy_tree_accepts_source_inside_allowed_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let allowed = tmp.path().join("allowed");
        let src = allowed.join("src");
        let dst = allowed.join("dst");
        std::fs::create_dir_all(&src).expect("mkdir src");
        std::fs::write(src.join("f.txt"), b"ok").expect("write");

        let ops = ManagerOps::new(
            CentralLog::open(tmp.path().join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed],
        );
        ops.copy_tree(&src, &dst)
            .expect("source inside root must succeed");
        assert!(dst.join("f.txt").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_rejects_symlink_inside_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let allowed = tmp.path().join("allowed");
        let src = allowed.join("src");
        let dst = allowed.join("dst");
        std::fs::create_dir_all(&src).expect("mkdir src");
        std::fs::write(src.join("ok.txt"), b"ok").expect("write");
        std::os::unix::fs::symlink("/etc/passwd", src.join("link")).expect("symlink");

        let ops = ManagerOps::new(
            CentralLog::open(tmp.path().join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed],
        );
        let err = ops
            .copy_tree(&src, &dst)
            .expect_err("symlink inside source must be rejected");
        assert!(
            matches!(err, AdapterError::Io { .. }),
            "expected Io error, got {err:?}"
        );
        assert!(
            err.to_string().contains("symlink rejected"),
            "error should mention symlink: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_rejects_symlink_source_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonicalize");
        let allowed = base.join("allowed");
        let real_dir = allowed.join("real");
        std::fs::create_dir_all(&real_dir).expect("mkdir");
        std::fs::write(real_dir.join("f.txt"), b"data").expect("write");
        let link_dir = allowed.join("link_to_dir");
        std::os::unix::fs::symlink(&real_dir, &link_dir).expect("symlink");

        let ops = ManagerOps::new(
            CentralLog::open(base.join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed.clone()],
        );
        let err = ops
            .copy_tree(&link_dir, &allowed.join("dst"))
            .expect_err("symlink-to-dir source must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("symlink rejected"),
            "error should mention symlink: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_file_rejects_symlink_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonicalize tmp");
        let allowed = base.join("allowed");
        std::fs::create_dir_all(&allowed).expect("mkdir");
        std::fs::write(allowed.join("real.txt"), b"ok").expect("write");
        std::os::unix::fs::symlink("/etc/passwd", allowed.join("link.txt")).expect("symlink");

        let ops = ManagerOps::new(
            CentralLog::open(base.join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed.clone()],
        );
        let err = ops
            .copy_file(&allowed.join("link.txt"), &allowed.join("dst.txt"))
            .expect_err("symlink source must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("symlink rejected") || msg.contains("boundary check"),
            "error should reject symlink via boundary or explicit check: {msg}"
        );

        ops.copy_file(&allowed.join("real.txt"), &allowed.join("dst.txt"))
            .expect("regular file must succeed");
        assert!(allowed.join("dst.txt").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn write_file_refuses_to_write_through_symlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonicalize");
        let allowed = base.join("allowed");
        std::fs::create_dir_all(&allowed).expect("mkdir");
        // A symlink whose target is *inside* the allowed roots (so the path
        // boundary check passes) — the explicit symlink refusal is what must
        // stop the write from following it. A symlink escaping the roots is
        // already caught earlier by `validate_ops_path`.
        let real = allowed.join("real-target");
        std::fs::write(&real, b"original").expect("seed target");
        let target = allowed.join("settings.json");
        std::os::unix::fs::symlink(&real, &target).expect("symlink");

        let ops = ManagerOps::new(
            CentralLog::open(base.join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed.clone()],
        );
        let err = ops
            .write_file(&target, b"injected")
            .expect_err("writing through a symlink must be rejected");
        assert!(matches!(err, AdapterError::Io { .. }), "got {err:?}");
        assert!(
            err.to_string().contains("symlink"),
            "error should mention symlink: {err}"
        );
        // The symlink target was not followed/overwritten.
        assert_eq!(
            std::fs::read_to_string(&real).expect("read target"),
            "original",
            "write must not have followed the symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_file_leaves_unrelated_sibling_temp_files_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonicalize");
        let allowed = base.join("allowed");
        std::fs::create_dir_all(&allowed).expect("mkdir");
        // Unrelated siblings a user might have next to the target: a plain
        // file and a symlink pointing at a sentinel outside the roots. Because
        // write_file uses unique temp names, it must neither delete these nor
        // follow the symlink; the sentinel must stay intact.
        let sentinel = base.join("sentinel");
        std::fs::write(&sentinel, b"original").expect("seed sentinel");
        let stale_file = allowed.join(".settings.json.anolisa-tmp.leftover");
        std::fs::write(&stale_file, b"unrelated").expect("seed stale file");
        let stale_link = allowed.join(".settings.json.anolisa-tmp.link");
        std::os::unix::fs::symlink(&sentinel, &stale_link).expect("plant symlink");
        let target = allowed.join("settings.json");

        let ops = ManagerOps::new(
            CentralLog::open(base.join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed.clone()],
        );
        ops.write_file(&target, b"new content")
            .expect("write must succeed via a unique temp");
        assert_eq!(
            std::fs::read_to_string(&target).expect("read target"),
            "new content"
        );
        // Unrelated siblings survive untouched.
        assert_eq!(
            std::fs::read_to_string(&stale_file).expect("read stale file"),
            "unrelated",
            "an unrelated sibling file must not be deleted"
        );
        assert!(
            stale_link
                .symlink_metadata()
                .expect("link meta")
                .is_symlink(),
            "an unrelated sibling symlink must not be deleted"
        );
        assert_eq!(
            std::fs::read_to_string(&sentinel).expect("read sentinel"),
            "original",
            "no sibling symlink may be followed"
        );
    }

    #[test]
    fn write_file_is_atomic_and_leaves_no_temp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonicalize");
        let allowed = base.join("allowed");
        std::fs::create_dir_all(&allowed).expect("mkdir");
        let target = allowed.join("settings.json");

        let ops = ManagerOps::new(
            CentralLog::open(base.join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed.clone()],
        );
        ops.write_file(&target, b"first").expect("first write");
        ops.write_file(&target, b"second").expect("overwrite");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "second");
        // No temp file (matched by the `.settings.json.anolisa-tmp` prefix)
        // may linger after a successful write.
        let leftover = std::fs::read_dir(&allowed)
            .expect("read dir")
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".settings.json.anolisa-tmp")
            });
        assert!(!leftover, "temp file should be renamed away");
    }

    #[cfg(unix)]
    #[test]
    fn write_file_preserves_mode_and_defaults_private() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonicalize");
        let allowed = base.join("allowed");
        std::fs::create_dir_all(&allowed).expect("mkdir");
        let target = allowed.join("settings.json");
        std::fs::write(&target, b"old").expect("seed");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("chmod target");

        let ops = ManagerOps::new(
            CentralLog::open(base.join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![allowed.clone()],
        );
        ops.write_file(&target, b"new").expect("overwrite");
        assert_eq!(
            std::fs::metadata(&target)
                .expect("target meta")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "atomic rename must preserve an existing private mode"
        );

        let created = allowed.join("new-settings.json");
        ops.write_file(&created, b"new").expect("create");
        assert_eq!(
            std::fs::metadata(&created)
                .expect("created meta")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "new adapter-managed files default to a private mode"
        );
    }

    // -- skill source allowed_roots integration ---------------------------------

    /// Skill source outside resource_root must be allowed by ManagerOps
    /// when added to allowed_roots. Verifies the P1 fix: copy_tree from
    /// `{datadir}/skills/<name>` succeeds when that path is in allowed_roots.
    #[test]
    fn copy_tree_accepts_skill_source_in_allowed_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resource_root = tmp
            .path()
            .join("adapters")
            .join("sec-core")
            .join("openclaw");
        let skill_source = tmp.path().join("skills").join("code-scanner");
        let framework_home = tmp.path().join("home").join("skills").join("code-scanner");

        std::fs::create_dir_all(&resource_root).expect("mkdir resource_root");
        std::fs::create_dir_all(&skill_source).expect("mkdir skill_source");
        std::fs::write(skill_source.join("manifest.json"), b"{}").expect("write");
        std::fs::create_dir_all(tmp.path().join("home").join("skills")).expect("mkdir dst parent");

        let ops = ManagerOps::new(
            CentralLog::open(tmp.path().join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            vec![
                resource_root.clone(),
                skill_source.clone(),
                tmp.path().join("home"),
            ],
        );

        ops.copy_tree(&skill_source, &framework_home)
            .expect("skill source in allowed_roots must succeed");
        assert!(
            framework_home.join("manifest.json").is_file(),
            "skill files must be copied to framework home"
        );
    }

    /// Skill source outside resource_root and NOT in allowed_roots must
    /// be rejected.
    #[test]
    fn copy_tree_rejects_skill_source_not_in_allowed_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resource_root = tmp
            .path()
            .join("adapters")
            .join("sec-core")
            .join("openclaw");
        let skill_source = tmp.path().join("skills").join("code-scanner");
        let framework_home = tmp.path().join("home").join("skills").join("code-scanner");

        std::fs::create_dir_all(&resource_root).expect("mkdir resource_root");
        std::fs::create_dir_all(&skill_source).expect("mkdir skill_source");
        std::fs::write(skill_source.join("manifest.json"), b"{}").expect("write");
        std::fs::create_dir_all(tmp.path().join("home").join("skills")).expect("mkdir dst parent");

        let ops = ManagerOps::new(
            CentralLog::open(tmp.path().join("log.jsonl")),
            "test".into(),
            "user".into(),
            "comp".into(),
            "test".into(),
            // Only resource_root and framework home — skill_source NOT included.
            vec![resource_root, tmp.path().join("home")],
        );

        let err = ops
            .copy_tree(&skill_source, &framework_home)
            .expect_err("skill source outside allowed_roots must be rejected");
        assert!(
            matches!(err, AdapterError::ClaimValidation(_)),
            "expected ClaimValidation, got {err:?}"
        );
    }

    // -- skill source boundary validation ---------------------------------------

    #[test]
    fn skill_source_under_datadir_is_accepted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let datadir = tmp.path().join("data");
        let resource_root = datadir.join("adapters").join("sec-core").join("openclaw");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let specs = vec![crate::manifest::AdapterSkillSpec {
            name: "code-scanner".to_string(),
            source: Some("{datadir}/skills/code-scanner/".to_string()),
        }];
        let skills = resolve_skill_sources(
            specs,
            &layout,
            &datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect("skill under datadir must be accepted");
        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0].source.as_ref().unwrap(),
            &datadir.join("skills").join("code-scanner"),
        );
    }

    #[test]
    fn skill_source_relative_escape_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let datadir = tmp.path().join("data");
        let resource_root = datadir.join("adapters").join("sec-core").join("openclaw");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let specs = vec![crate::manifest::AdapterSkillSpec {
            name: "code-scanner".to_string(),
            source: Some("../shared-skills/code-scanner".to_string()),
        }];
        let err = resolve_skill_sources(
            specs,
            &layout,
            &datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect_err("relative path escaping resource_root must be rejected");
        assert!(
            matches!(err, AdapterError::InvalidAdapterInput { .. }),
            "expected InvalidAdapterInput, got {err:?}"
        );
    }

    #[test]
    fn skill_source_outside_boundary_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let datadir = tmp.path().join("data");
        let resource_root = datadir.join("adapters").join("sec-core").join("openclaw");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let specs = vec![crate::manifest::AdapterSkillSpec {
            name: "x".to_string(),
            source: Some("/etc".to_string()),
        }];
        let err = resolve_skill_sources(
            specs,
            &layout,
            &datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect_err("source pointing to /etc must be rejected");
        assert!(
            matches!(err, AdapterError::InvalidAdapterInput { .. }),
            "expected InvalidAdapterInput, got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("outside the allowed roots"),
            "error must explain boundary violation: {msg}"
        );
    }

    #[test]
    fn skill_source_none_is_accepted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let datadir = tmp.path().join("data");
        let resource_root = datadir.join("adapters").join("sec-core").join("openclaw");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let specs = vec![crate::manifest::AdapterSkillSpec {
            name: "code-scanner".to_string(),
            source: None,
        }];
        let skills = resolve_skill_sources(
            specs,
            &layout,
            &datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect("no source must be accepted");
        assert_eq!(skills.len(), 1);
        assert!(skills[0].source.is_none());
    }

    // -- absolute dest keeps manifest datadir for skill sources ----------------

    /// Regression (#1104): when adapter dest is an absolute path (e.g.
    /// `/opt/agent-sec/openclaw-plugin/`), the effective_datadir must be
    /// the datadir root where the component contract was actually found,
    /// not whichever root happens to iterate first. This ensures
    /// `{datadir}` in skill sources expands to the correct root.
    #[test]
    fn absolute_dest_uses_manifest_datadir_for_skill_sources() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("var/lib/anolisa");
        let local_datadir = tmp.path().join("usr/local/share/anolisa");
        let pkg_datadir = tmp.path().join("usr/share/anolisa");
        let abs_dest = tmp.path().join("opt/agent-sec/openclaw-plugin");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        // Contract lives under pkg_datadir (simulates RPM install to
        // /usr/share/anolisa). No contract under local_datadir.
        let contract = format!(
            r#"
[component]
name = "sec-core"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "sec-core"
dest = "{}"

[[adapters.openclaw.skills]]
name = "code-scanner"
source = "{{datadir}}/skills/code-scanner/"
"#,
            abs_dest.display()
        );
        write_contract_with_content(&pkg_datadir, "sec-core", &contract);

        // Resource root at the absolute path.
        std::fs::create_dir_all(&abs_dest).expect("mkdir abs_dest");
        std::fs::write(abs_dest.join("openclaw.plugin.json"), b"{}").expect("write plugin");

        // Skill source under the package datadir (where the contract lives).
        let skill_source = pkg_datadir.join("skills").join("code-scanner");
        std::fs::create_dir_all(&skill_source).expect("mkdir skill source");
        std::fs::write(skill_source.join("manifest.json"), b"{}").expect("write skill");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(
            layout.clone(),
            Some(tmp.path().to_path_buf()),
            "test".into(),
        );
        manager.state_path = state_dir.join("installed.toml");
        // local_datadir is first — before the fix, effective_datadir
        // would incorrectly be local_datadir.
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![local_datadir.clone(), pkg_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![local_datadir.clone(), pkg_datadir.clone()];

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");

        // resource_root must be the absolute dest path.
        let (resource_root, effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve resource root");
        assert_eq!(
            resource_root, abs_dest,
            "resource_root must be the absolute dest path"
        );
        // effective_datadir must be pkg_datadir (where the contract lives),
        // NOT local_datadir (which was first in the list).
        assert_eq!(
            effective_datadir, pkg_datadir,
            "effective_datadir must be the manifest's matched datadir root, not the first candidate"
        );

        // Skill source must expand {datadir} to pkg_datadir.
        let skill_specs = declared_skills(&manifest, "openclaw");
        let skills = resolve_skill_sources(
            skill_specs,
            &layout,
            &effective_datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect("resolve skills");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "code-scanner");
        assert_eq!(
            skills[0].source.as_ref(),
            Some(&skill_source),
            "skill source must resolve to pkg_datadir path (/usr/share/anolisa/skills/code-scanner/), \
             not local_datadir (/usr/local/share/anolisa/skills/code-scanner/)"
        );
    }

    /// State snapshots have higher contract priority than datadir
    /// contracts. If the snapshot was copied from the package datadir and
    /// an earlier local datadir still contains a stale same-component
    /// contract, absolute-dest resolution must keep using the snapshot's
    /// package datadir source for `{datadir}` skill expansion.
    #[test]
    fn snapshot_contract_uses_matching_datadir_for_absolute_dest_skill_sources() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("var/lib/anolisa");
        let local_datadir = tmp.path().join("usr/local/share/anolisa");
        let pkg_datadir = tmp.path().join("usr/share/anolisa");
        let abs_dest = tmp.path().join("opt/agent-sec/openclaw-plugin");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        let package_contract = format!(
            r#"
[component]
name = "sec-core"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "sec-core"
dest = "{}"

[[adapters.openclaw.skills]]
name = "code-scanner"
source = "{{datadir}}/skills/code-scanner/"
"#,
            abs_dest.display()
        );
        let stale_local_contract =
            contract_toml_with_custom_dest("sec-core", "{datadir}/stale/sec-core/openclaw/");

        write_contract_with_content(&local_datadir, "sec-core", &stale_local_contract);
        write_contract_with_content(&pkg_datadir, "sec-core", &package_contract);

        let snapshot = FsLayout::component_manifest_snapshot_path(&state_dir, "sec-core");
        std::fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("mkdir");
        std::fs::write(&snapshot, &package_contract).expect("write snapshot");

        std::fs::create_dir_all(&abs_dest).expect("mkdir abs_dest");
        std::fs::write(abs_dest.join("openclaw.plugin.json"), b"{}").expect("write plugin");

        let skill_source = pkg_datadir.join("skills").join("code-scanner");
        std::fs::create_dir_all(&skill_source).expect("mkdir skill source");
        std::fs::write(skill_source.join("manifest.json"), b"{}").expect("write skill");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(
            layout.clone(),
            Some(tmp.path().to_path_buf()),
            "test".into(),
        );
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![local_datadir.clone(), pkg_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![local_datadir.clone(), pkg_datadir.clone()];

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        assert_eq!(
            contract_datadir_root.as_ref(),
            Some(&pkg_datadir),
            "snapshot content should match the package datadir contract, not the stale local one"
        );

        let (resource_root, effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve resource root");
        assert_eq!(resource_root, abs_dest);
        assert_eq!(effective_datadir, pkg_datadir);

        let skill_specs = declared_skills(&manifest, "openclaw");
        let skills = resolve_skill_sources(
            skill_specs,
            &layout,
            &effective_datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect("resolve skills");
        assert_eq!(skills[0].source.as_ref(), Some(&skill_source));
    }

    // -- provenance-guided contract datadir root ----------------------------

    /// Provenance selects the correct datadir root when two roots have
    /// identical contracts (Scenario B). Without provenance, the first
    /// content match wins (local_datadir) which would be wrong.
    #[test]
    fn provenance_selects_correct_datadir_for_absolute_dest_skill_sources() {
        use crate::adapter::contract::{
            ContractProvenance, ContractSourceKind, write_snapshot_provenance,
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("var/lib/anolisa");
        let local_datadir = tmp.path().join("usr/local/share/anolisa");
        let pkg_datadir = tmp.path().join("usr/share/anolisa");
        let abs_dest = tmp.path().join("opt/agent-sec/openclaw-plugin");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        let contract = format!(
            r#"
[component]
name = "sec-core"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "sec-core"
dest = "{}"

[[adapters.openclaw.skills]]
name = "code-scanner"
source = "{{datadir}}/skills/code-scanner/"
"#,
            abs_dest.display()
        );

        // Both datadirs have identical contracts — content match alone
        // would pick local_datadir (first in list), but provenance
        // points to pkg_datadir.
        write_contract_with_content(&local_datadir, "sec-core", &contract);
        write_contract_with_content(&pkg_datadir, "sec-core", &contract);

        let snapshot = FsLayout::component_manifest_snapshot_path(&state_dir, "sec-core");
        std::fs::create_dir_all(snapshot.parent().expect("snapshot parent")).expect("mkdir");
        std::fs::write(&snapshot, &contract).expect("write snapshot");

        let prov = ContractProvenance {
            schema_version: 1,
            source_kind: ContractSourceKind::Datadir,
            source_path: FsLayout::component_contract_path(&pkg_datadir, "sec-core"),
            datadir_root: pkg_datadir.clone(),
        };
        write_snapshot_provenance(&snapshot, &prov).expect("write prov");

        std::fs::create_dir_all(&abs_dest).expect("mkdir abs_dest");
        std::fs::write(abs_dest.join("openclaw.plugin.json"), b"{}").expect("write plugin");

        let skill_source = pkg_datadir.join("skills").join("code-scanner");
        std::fs::create_dir_all(&skill_source).expect("mkdir skill source");
        std::fs::write(skill_source.join("manifest.json"), b"{}").expect("write skill");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(
            layout.clone(),
            Some(tmp.path().to_path_buf()),
            "test".into(),
        );
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![local_datadir.clone(), pkg_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![local_datadir.clone(), pkg_datadir.clone()];

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        assert_eq!(
            contract_datadir_root.as_ref(),
            Some(&pkg_datadir),
            "provenance must select pkg_datadir, not local_datadir"
        );

        let (resource_root, effective_datadir) = manager
            .resolve_resource_root(
                "sec-core",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve resource root");
        assert_eq!(resource_root, abs_dest);
        assert_eq!(effective_datadir, pkg_datadir);

        let skill_specs = declared_skills(&manifest, "openclaw");
        let skills = resolve_skill_sources(
            skill_specs,
            &layout,
            &effective_datadir,
            "sec-core",
            "openclaw",
            &resource_root,
        )
        .expect("resolve skills");
        assert_eq!(
            skills[0].source.as_ref(),
            Some(&skill_source),
            "skill source must resolve to pkg_datadir, not local_datadir"
        );
    }

    /// Without provenance, snapshot content match still works (Scenario C).
    #[test]
    fn snapshot_without_provenance_falls_back_to_content_match() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let pkg_datadir = tmp.path().join("pkg_data");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "sec-core",
            ObjectStatus::Adopted,
        );

        let contract = valid_contract_toml("sec-core");
        write_contract(&pkg_datadir, "sec-core");

        let snapshot = FsLayout::component_manifest_snapshot_path(&state_dir, "sec-core");
        std::fs::create_dir_all(snapshot.parent().expect("parent")).expect("mkdir");
        std::fs::write(&snapshot, contract).expect("write snapshot");
        // Deliberately no provenance.toml.

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager =
            AdapterManager::new(layout, Some(tmp.path().to_path_buf()), "test".into());
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![pkg_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![pkg_datadir.clone()];

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (_manifest, _scoped_roots, contract_datadir_root, _rpm_provenance) = manager
            .load_visible_component_manifest("sec-core", &state)
            .expect("load manifest");
        assert_eq!(
            contract_datadir_root.as_ref(),
            Some(&pkg_datadir),
            "content matching must find pkg_datadir"
        );
    }

    // -- contract-scoped datadir priority -------------------------------------

    /// Regression: when a contract from the package datadir
    /// (`/usr/share/…`) declares `dest = "{datadir}/skills"`, `{datadir}`
    /// must bind to the package datadir — not to a local datadir
    /// (`/usr/local/share/…`) whose expanded path exists but is empty.
    #[test]
    fn contract_scoped_datadir_takes_priority_over_empty_local_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let local_datadir = tmp.path().join("usr/local/share/anolisa");
        let package_datadir = tmp.path().join("usr/share/anolisa");
        let state_dir = tmp.path().join("var/lib/anolisa");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "os-skills",
            ObjectStatus::Adopted,
        );

        let contract = r#"
[component]
name = "os-skills"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "skill_bundle"
plugin_id = "os-skills"
dest = "{datadir}/skills"
"#;
        write_contract_with_content(&package_datadir, "os-skills", contract);

        // Local skills dir exists but is empty — used to win incorrectly.
        let local_skills = local_datadir.join("skills");
        std::fs::create_dir_all(&local_skills).expect("mkdir local skills");

        // Package skills dir has real resources.
        let package_skills = package_datadir.join("skills");
        std::fs::create_dir_all(&package_skills).expect("mkdir package skills");
        std::fs::write(package_skills.join("manifest.json"), b"{}").expect("write resource");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(
            layout.clone(),
            Some(tmp.path().to_path_buf()),
            "test".into(),
        );
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![local_datadir.clone(), package_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![local_datadir.clone(), package_datadir.clone()];

        // scan must resolve to the package datadir, not the local one.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "os-skills" && e.framework == "openclaw")
            .expect("os-skills/openclaw should be in scan");
        assert!(entry.declared);
        assert_eq!(
            entry.resource_root.as_ref(),
            Some(&package_skills),
            "scan must resolve {{datadir}}/skills to the package datadir, \
             not the empty local datadir"
        );

        // enable path: resolve_resource_root must also prefer the package
        // datadir.
        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("os-skills", &state)
            .expect("load manifest");
        let (resource_root, effective_datadir) = manager
            .resolve_resource_root(
                "os-skills",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve resource root");
        assert_eq!(
            resource_root, package_skills,
            "enable must resolve to package datadir skills, not empty local"
        );
        assert_eq!(
            effective_datadir, package_datadir,
            "effective datadir must be the package datadir"
        );
    }

    /// When the contract's own datadir root lacks the target resource,
    /// `{datadir}` falls back to other roots in the scope.
    #[test]
    fn contract_scoped_datadir_falls_back_when_own_root_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let local_datadir = tmp.path().join("usr/local/share/anolisa");
        let package_datadir = tmp.path().join("usr/share/anolisa");
        let state_dir = tmp.path().join("var/lib/anolisa");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "os-skills",
            ObjectStatus::Adopted,
        );

        let contract = r#"
[component]
name = "os-skills"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "skill_bundle"
plugin_id = "os-skills"
dest = "{datadir}/skills"
"#;
        write_contract_with_content(&package_datadir, "os-skills", contract);

        // Package datadir does NOT have the skills dir.
        // Local datadir DOES have the skills dir with resources.
        let local_skills = local_datadir.join("skills");
        std::fs::create_dir_all(&local_skills).expect("mkdir local skills");
        std::fs::write(local_skills.join("manifest.json"), b"{}").expect("write resource");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(
            layout.clone(),
            Some(tmp.path().to_path_buf()),
            "test".into(),
        );
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![local_datadir.clone(), package_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![local_datadir.clone(), package_datadir.clone()];

        let state = load_written_state(&state_dir.join("installed.toml"));
        let (manifest, scoped_roots, contract_datadir_root, rpm_provenance) = manager
            .load_visible_component_manifest("os-skills", &state)
            .expect("load manifest");
        let (resource_root, effective_datadir) = manager
            .resolve_resource_root(
                "os-skills",
                "openclaw",
                &manifest,
                &scoped_roots,
                contract_datadir_root.as_deref(),
                rpm_provenance,
            )
            .expect("resolve resource root");
        assert_eq!(
            resource_root, local_skills,
            "must fall back to local datadir when package datadir lacks the resource"
        );
        assert_eq!(
            effective_datadir, local_datadir,
            "effective datadir must be the fallback local datadir"
        );
    }

    /// When the contract is resolved from a state snapshot whose
    /// provenance points to the package datadir, scan must still
    /// prioritize the package datadir for `{datadir}` expansion —
    /// the same behavior as a direct datadir hit.
    #[test]
    fn snapshot_with_provenance_prioritizes_package_datadir_in_scan() {
        use crate::adapter::contract::{
            ContractProvenance, ContractSourceKind, write_snapshot_provenance,
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let local_datadir = tmp.path().join("usr/local/share/anolisa");
        let package_datadir = tmp.path().join("usr/share/anolisa");
        let state_dir = tmp.path().join("var/lib/anolisa");

        seed_installed_state(
            &state_dir,
            crate::state::InstallMode::System,
            tmp.path(),
            "os-skills",
            ObjectStatus::Adopted,
        );

        let contract = r#"
[component]
name = "os-skills"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "skill_bundle"
plugin_id = "os-skills"
dest = "{datadir}/skills"
"#;
        // Both datadirs have the contract on disk (simulates RPM upgrade
        // that left a copy in both trees).
        write_contract_with_content(&local_datadir, "os-skills", contract);
        write_contract_with_content(&package_datadir, "os-skills", contract);

        // State snapshot + provenance pointing to the package datadir.
        let snapshot = FsLayout::component_manifest_snapshot_path(&state_dir, "os-skills");
        std::fs::create_dir_all(snapshot.parent().expect("parent")).expect("mkdir");
        std::fs::write(&snapshot, contract).expect("write snapshot");

        let prov = ContractProvenance {
            schema_version: 1,
            source_kind: ContractSourceKind::Datadir,
            source_path: FsLayout::component_contract_path(&package_datadir, "os-skills"),
            datadir_root: package_datadir.clone(),
        };
        write_snapshot_provenance(&snapshot, &prov).expect("write provenance");

        // Local skills dir exists but is empty (the decoy).
        let local_skills = local_datadir.join("skills");
        std::fs::create_dir_all(&local_skills).expect("mkdir local skills");

        // Package skills dir has real resources.
        let package_skills = package_datadir.join("skills");
        std::fs::create_dir_all(&package_skills).expect("mkdir package skills");
        std::fs::write(package_skills.join("manifest.json"), b"{}").expect("write resource");

        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let mut manager = AdapterManager::new(
            layout.clone(),
            Some(tmp.path().to_path_buf()),
            "test".into(),
        );
        manager.state_path = state_dir.join("installed.toml");
        manager.visible_roots = vec![VisibleRoot {
            state_dir: state_dir.clone(),
            contract_datadir_roots: vec![local_datadir.clone(), package_datadir.clone()],
        }];
        manager.all_datadir_roots = vec![local_datadir.clone(), package_datadir.clone()];

        // scan: provenance directs {datadir} to the package datadir even
        // though the contract was loaded from the state snapshot.
        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "os-skills" && e.framework == "openclaw")
            .expect("os-skills/openclaw should be in scan");
        assert!(entry.declared);
        assert_eq!(
            entry.resource_root.as_ref(),
            Some(&package_skills),
            "scan via snapshot+provenance must resolve {{datadir}}/skills \
             to the package datadir, not the empty local datadir"
        );
    }

    // -- plan_disable_report --------------------------------------------------

    fn make_claim_for_plan_test(
        adapter_type: Option<&str>,
        plugin_id: Option<&str>,
        resources: Vec<crate::adapter::claim::ClaimResource>,
        payload_skill_ids: Vec<String>,
        payload_config_ids: Vec<String>,
    ) -> crate::adapter::claim::AdapterClaim {
        use crate::adapter::claim::{ClaimStatus, DriverPayload, OpenClawClaim};

        AdapterClaim {
            claim_schema: 1,
            component: "test-comp".to_string(),
            framework: "openclaw".to_string(),
            plugin_id: plugin_id.map(str::to_string),
            adapter_type: adapter_type.map(str::to_string),
            enabled_at: "2026-06-30T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/fake/adapters/test-comp/openclaw"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: 1,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources,
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "state_dir".to_string(),
                plugin_resource: "plugin".to_string(),
                skill_resources: payload_skill_ids,
                config_resources: payload_config_ids,

                displaced_plugins: Vec::new(),
            }),
        }
    }

    #[test]
    fn plan_disable_report_plugin_adapter() {
        use crate::adapter::claim::{ClaimResource, ClaimResourceKind};

        let claim = make_claim_for_plan_test(
            Some("plugin"),
            Some("test-comp"),
            vec![
                ClaimResource {
                    id: "state_dir".to_string(),
                    purpose: "openclaw_state_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/user/.openclaw"),
                    },
                },
                ClaimResource {
                    id: "plugin".to_string(),
                    purpose: "openclaw_plugin".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: "openclaw".to_string(),
                        plugin_id: "test-comp".to_string(),
                    },
                },
            ],
            Vec::new(),
            Vec::new(),
        );

        let report = plan_disable_report(&claim, None);
        assert!(report.cleanup_complete);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.contains("would unregister") && m.contains("test-comp")),
            "must describe plugin unregister: {:#?}",
            report.messages
        );
        // Framework home/state_dir must NOT be listed as a removal target.
        assert!(
            !report
                .messages
                .iter()
                .any(|m| m.contains("would remove") && m.contains(".openclaw")),
            "must NOT claim to remove the framework home dir: {:#?}",
            report.messages
        );
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.contains("would remove adapter receipt")),
            "must note receipt removal: {:#?}",
            report.messages
        );
    }

    #[test]
    fn plan_disable_report_skill_bundle_adapter() {
        use crate::adapter::claim::{ClaimResource, ClaimResourceKind};

        let claim = make_claim_for_plan_test(
            Some("skill_bundle"),
            None,
            vec![
                ClaimResource {
                    id: "state_dir".to_string(),
                    purpose: "openclaw_state_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/user/.openclaw"),
                    },
                },
                ClaimResource {
                    id: "skill:os-tools".to_string(),
                    purpose: "openclaw_skill".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/user/.openclaw/skills/os-tools"),
                    },
                },
            ],
            vec!["skill:os-tools".to_string()],
            Vec::new(),
        );

        let report = plan_disable_report(&claim, None);
        assert!(report.cleanup_complete);
        // plugin resource "plugin" has no matching ClaimResource so
        // plugin unregister must NOT appear.
        assert!(
            !report
                .messages
                .iter()
                .any(|m| m.contains("would unregister")),
            "skill_bundle must not mention plugin unregister: {:#?}",
            report.messages
        );
        // Framework home/state_dir must NOT be listed.
        assert!(
            !report
                .messages
                .iter()
                .any(|m| m == "would remove /home/user/.openclaw"),
            "must NOT claim to remove the framework home: {:#?}",
            report.messages
        );
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.contains("would remove") && m.contains("skills/os-tools")),
            "must describe skill dir removal: {:#?}",
            report.messages
        );
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.contains("would remove adapter receipt")),
            "must note receipt removal: {:#?}",
            report.messages
        );
    }

    #[test]
    fn plan_disable_report_with_config_entries() {
        use crate::adapter::claim::{ClaimResource, ClaimResourceKind};

        let claim = make_claim_for_plan_test(
            Some("plugin"),
            Some("test-comp"),
            vec![ClaimResource {
                id: "config:0".to_string(),
                purpose: "openclaw_config".to_string(),
                kind: ClaimResourceKind::FrameworkConfig {
                    framework: "openclaw".to_string(),
                    key: "plugins.entries.test-comp.enabled".to_string(),
                    state: crate::adapter::claim::ConfigApplyState::Applied,
                },
            }],
            Vec::new(),
            vec!["config:0".to_string()],
        );

        let report = plan_disable_report(&claim, None);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.contains("config key") && m.contains("left in place")),
            "must note config entries are not reversed: {:#?}",
            report.messages
        );
    }

    #[test]
    fn plan_disable_report_hermes_plugin_adapter() {
        use crate::adapter::claim::{
            ClaimResource, ClaimResourceKind, ClaimStatus, DriverPayload, HermesClaim,
        };

        // Hermes stores both the home and the plugin as ExternalPath; only
        // the plugin dir is a cleanup target, and disable runs a CLI step
        // first.
        let claim = AdapterClaim {
            claim_schema: 1,
            component: "test-comp".to_string(),
            framework: "hermes".to_string(),
            plugin_id: Some("test-comp".to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-06-30T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/fake/adapters/test-comp/hermes"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: 1,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "hermes_home".to_string(),
                    purpose: "hermes_home".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/user/.hermes"),
                    },
                },
                ClaimResource {
                    id: "hermes_plugin".to_string(),
                    purpose: "hermes_plugin".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/user/.hermes/plugins/test-comp"),
                    },
                },
            ],
            driver_payload: DriverPayload::Hermes(HermesClaim {
                home_resource: "hermes_home".to_string(),
                plugin_resource: "hermes_plugin".to_string(),
                skill_resources: Vec::new(),
            }),
        };

        let report = plan_disable_report(&claim, None);
        assert!(report.cleanup_complete);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m == "would disable hermes plugin 'test-comp'"),
            "must describe the hermes CLI disable step: {:#?}",
            report.messages
        );
        assert!(
            report
                .messages
                .iter()
                .any(|m| m == "would remove /home/user/.hermes/plugins/test-comp"),
            "must describe the hermes plugin directory removal: {:#?}",
            report.messages
        );
        // Hermes home dir must NOT be a removal target.
        assert!(
            !report
                .messages
                .iter()
                .any(|m| m == "would remove /home/user/.hermes"),
            "must NOT claim to remove the hermes home: {:#?}",
            report.messages
        );
    }

    #[test]
    fn plan_disable_report_qwenpaw_plugin_adapter() {
        use crate::adapter::claim::{
            CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
            DRIVER_SCHEMA_VERSION, DriverPayload, QwenPawClaim,
        };

        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "test-comp".to_string(),
            framework: "qwenpaw".to_string(),
            plugin_id: Some("test-comp".to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: "2026-09-04T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/fake/adapters/test-comp/qwenpaw"),
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
                        path: PathBuf::from("/home/user/.qwenpaw"),
                    },
                },
                ClaimResource {
                    id: "qwenpaw_plugin".to_string(),
                    purpose: "qwenpaw_plugin_dir".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/user/.qwenpaw/plugins/test-comp"),
                    },
                },
            ],
            driver_payload: DriverPayload::QwenPaw(QwenPawClaim {
                home_resource: "qwenpaw_home".to_string(),
                plugin_resource: "qwenpaw_plugin".to_string(),
            }),
        };

        let report = plan_disable_report(&claim, None);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m == "would uninstall qwenpaw plugin 'test-comp'"),
            "must describe the qwenpaw CLI uninstall step: {:#?}",
            report.messages
        );
        assert!(
            report
                .messages
                .iter()
                .any(|m| m == "would remove /home/user/.qwenpaw/plugins/test-comp"),
            "must describe the plugin directory removal: {:#?}",
            report.messages
        );
        assert!(
            !report
                .messages
                .iter()
                .any(|m| m == "would remove /home/user/.qwenpaw"),
            "must NOT claim to remove the qwenpaw working directory: {:#?}",
            report.messages
        );
    }

    #[test]
    fn plan_disable_report_hermes_skill_bundle_omits_plugin() {
        use crate::adapter::claim::{
            ClaimResource, ClaimResourceKind, ClaimStatus, DriverPayload, HermesClaim,
        };

        // skill_bundle Hermes receipts carry no plugin resource (empty id):
        // the dry-run plan must not mention any plugin disable step.
        let claim = AdapterClaim {
            claim_schema: 1,
            component: "test-comp".to_string(),
            framework: "hermes".to_string(),
            plugin_id: None,
            adapter_type: Some("skill_bundle".to_string()),
            enabled_at: "2026-06-30T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/fake/adapters/test-comp/hermes"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: 1,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "hermes_home".to_string(),
                    purpose: "hermes_home".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/user/.hermes"),
                    },
                },
                ClaimResource {
                    id: "hermes_skill_audit".to_string(),
                    purpose: "hermes_skill".to_string(),
                    kind: ClaimResourceKind::ExternalPath {
                        path: PathBuf::from("/home/user/.hermes/skills/audit"),
                    },
                },
            ],
            driver_payload: DriverPayload::Hermes(HermesClaim {
                home_resource: "hermes_home".to_string(),
                plugin_resource: String::new(),
                skill_resources: vec!["hermes_skill_audit".to_string()],
            }),
        };

        let report = plan_disable_report(&claim, None);
        assert!(
            !report
                .messages
                .iter()
                .any(|m| m.contains("disable hermes plugin")),
            "skill_bundle must not mention a plugin disable step: {:#?}",
            report.messages
        );
        assert!(
            report
                .messages
                .iter()
                .any(|m| m == "would remove /home/user/.hermes/skills/audit"),
            "must describe skill dir removal: {:#?}",
            report.messages
        );
        assert!(
            !report
                .messages
                .iter()
                .any(|m| m == "would remove /home/user/.hermes"),
            "must NOT claim to remove the hermes home: {:#?}",
            report.messages
        );
    }
}
