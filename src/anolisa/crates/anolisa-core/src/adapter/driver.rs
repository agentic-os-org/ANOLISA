//! Framework driver trait and the value types it exchanges with the
//! [`AdapterManager`](super::manager).
//!
//! A driver understands one framework's enable/disable/status semantics.
//! It never spawns processes or deletes paths directly: dangerous IO goes
//! through the controlled [`AdapterOps`] handle the Manager injects via
//! [`DriverCtx`], so timeout/truncation/logging and path-boundary policy
//! stay centralized. The split is deliberate — **driver owns framework
//! semantics, Manager owns dangerous-resource boundaries**.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use anolisa_platform::fs_layout::FsLayout;

use super::AdapterError;
use super::claim::AdapterClaim;
use super::managed_files::MaterializedMapping;

/// Read-only host facts a driver may inspect during [`FrameworkDriver::detect`].
#[derive(Debug, Clone, Default)]
pub struct HostEnv {
    /// Current caller's home directory, when resolvable. Frameworks whose
    /// state lives under `$HOME` use this to locate it.
    pub user_home: Option<PathBuf>,
}

/// A skill declared in the component manifest, with an optional resolved
/// source path for the skill bundle.
#[derive(Debug, Clone)]
pub struct DeclaredSkill {
    /// Skill name.
    pub name: String,
    /// Resolved source directory. When `None`, the driver falls back to
    /// `<resource_root>/skills/<name>/`.
    pub source: Option<PathBuf>,
}

/// Detection outcome for a framework.
#[derive(Debug, Clone, Serialize)]
pub struct DetectResult {
    /// Whether the framework appears usable on this host.
    pub detected: bool,
    /// Human-readable explanation (binary path found, home dir present, …).
    pub reason: String,
}

/// Everything a driver needs for one operation, constructed by the
/// Manager. The driver never locates state/log/lock paths itself.
pub struct DriverCtx<'a> {
    /// Component being enabled/disabled/queried.
    pub component: String,
    /// Framework name (matches the driver's [`FrameworkDriver::name`]).
    pub framework: String,
    /// Resolved filesystem layout for the active install mode.
    pub layout: &'a FsLayout,
    /// Resource directory under `{datadir}/adapters/<component>/<framework>/`.
    pub resource_root: PathBuf,
    /// Caller's home directory, when resolvable.
    pub user_home: Option<PathBuf>,
    /// Plugin id declared in the component's adapter manifest, if any.
    /// A driver may fall back to it when the bundle does not name one.
    pub declared_plugin_id: Option<String>,
    /// Explicit framework profiles selected by the caller for profile-scoped
    /// adapters. Drivers that are not profile-scoped ignore this list.
    pub requested_profiles: Vec<String>,
    /// Adapter type declared in the component manifest. Absent means the
    /// legacy plugin adapter model.
    pub adapter_type: Option<String>,
    /// Skills declared in the component's adapter manifest. Each entry
    /// carries an optional resolved `source` path; when absent, the driver
    /// falls back to `<resource_root>/skills/<name>/`.
    pub declared_skills: Vec<DeclaredSkill>,
    /// Post-install config key/value pairs declared in the component's
    /// adapter manifest. The driver applies these via the framework CLI.
    pub declared_config: Vec<crate::manifest::AdapterConfigSetSpec>,
    /// Bundle entry-point filename from the manifest (framework-specific
    /// section preferred over generic `[adapters.bundle]`). The driver
    /// should use this to locate the framework-native manifest inside
    /// the resource root instead of hardcoding a filename.
    pub declared_bundle_entry: Option<String>,
    /// Framework plugins declared by the component's adapter manifest as
    /// displaced by this adapter (OpenClaw: `[adapters.openclaw].displaces`).
    /// The driver disables each one while the adapter is enabled and records
    /// the transition in the receipt, so disable can reverse exactly what
    /// enable claimed. Empty for every framework that has no such concept.
    pub declared_displaces: Vec<crate::manifest::DisplacedPluginSpec>,
    /// Adapter-level framework version requirement resolved from the
    /// component manifest. The Manager derives it with
    /// `[adapters.compat].framework_version` taking precedence over the
    /// legacy `framework_version_req` compat entry; `None` means the adapter
    /// declares no minimum. A driver that can probe the framework version
    /// gates enable on this before any mutation.
    pub framework_version_req: Option<String>,
    /// Whether the caller explicitly authorized an unsafe plugin install
    /// (`--allow-unsafe-plugin-install`). Typed data, never a free-form map:
    /// only the OpenClaw plugin path consults it, and only to add the
    /// framework's own unsafe-install flag when the host also exposes it.
    /// Defaults to `false` for every non-enable operation.
    pub allow_unsafe_plugin_install: bool,
    /// True when the caller passed `--dry-run`; drivers must not mutate
    /// framework state in this mode (the Manager also guards this).
    pub dry_run: bool,
    /// Controlled IO helpers. The only sanctioned way to run a framework
    /// CLI.
    pub ops: &'a dyn AdapterOps,
}

impl DriverCtx<'_> {
    /// Whether this operation is for a skill-only adapter.
    pub fn is_skill_bundle(&self) -> bool {
        self.adapter_type.as_deref() == Some("skill_bundle")
    }
}

/// Parsed framework-native bundle from the resource directory.
#[derive(Debug, Clone)]
pub struct AdapterBundle {
    /// Resource root the bundle was read from.
    pub resource_root: PathBuf,
    /// Framework-native plugin id resolved from the bundle (or the
    /// manifest-declared fallback).
    pub plugin_id: Option<String>,
}

/// What [`FrameworkDriver::apply_enable`] would do, for `--dry-run` and
/// human confirmation. Carries no executable data.
#[derive(Debug, Clone, Serialize)]
pub struct DriverPlan {
    /// Framework name.
    pub framework: String,
    /// Component name.
    pub component: String,
    /// Ordered human-readable descriptions of the enable steps.
    pub actions: Vec<String>,
    /// Display form of the framework CLI registration command, when one is
    /// run. Display-only — never parsed back into an argv.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub register_command: Option<String>,
}

/// Driver-private state produced by [`FrameworkDriver::prepare_enable`] and
/// handed to [`FrameworkDriver::apply_enable`] within the same locked enable.
///
/// It carries the host capabilities a driver resolved by read-only probing
/// *before* the first mutation, so apply can act on them without re-probing —
/// keeping every probe to once per enable while still completing all probing
/// up front. It is deliberately **not** part of the receipt: the persisted
/// [`AdapterClaim`] stays pure typed resource data, and this transient value
/// never outlives the enable call. Most drivers need no such state and return
/// [`PreparedEnable::None`].
#[derive(Debug, Clone, Default)]
pub enum PreparedEnable {
    /// The driver needs no prepared host state for apply.
    #[default]
    None,
    /// OpenClaw install/verify capabilities resolved before the first
    /// mutation. `apply_enable` uses these to choose consent arguments, the
    /// unsafe-retry hint, and the runtime-inspect form without a second probe.
    OpenClaw {
        /// The host's installer supports accepting the plugin's declared capabilities.
        supports_accept_capabilities: bool,
        /// The host's enable command supports accepting declared capabilities.
        supports_enable_accept_capabilities: bool,
        /// The host's `plugins install --help` exposes the unsafe flag.
        supports_unsafe_install: bool,
        /// The host's `plugins inspect --help` exposes `--json`.
        supports_inspect_json: bool,
        /// The host's `plugins inspect --help` exposes `--runtime`.
        supports_inspect_runtime: bool,
        /// Original manifest indices of config entries selected by the
        /// detected host version. Apply uses this transient intent to avoid
        /// claiming an entry until its `config set` command succeeds.
        selected_config_indices: Vec<usize>,
        /// Plugin ids whose displacement this enable's own probe claimed, i.e.
        /// the ones it is about to turn off itself.
        ///
        /// `apply_enable` re-confirms *only* these before mutating. A displacement
        /// carried over from a prior receipt by
        /// [`FrameworkDriver::preserve_reenable_facts`] is deliberately absent:
        /// that plugin is already off *because of this adapter*, so re-probing it
        /// would read "somebody else turned it off" and release ownership the
        /// receipt legitimately holds.
        freshly_claimed_displacements: Vec<String>,
    },
    /// Qoder native-plugin capabilities resolved before installation.
    QoderNative {
        /// Exact qodercli program selected during the read-only preflight.
        program: String,
    },
}

/// Manager-owned persistence channel for resources applied incrementally.
///
/// Drivers use it for write-ahead intent and confirmed-success transitions.
/// The Manager validates and saves the updated receipt while the enable lock
/// is still held, keeping cleanup state accurate across partial failure.
pub trait EnableProgress {
    /// Validate and persist the current enable receipt.
    ///
    /// # Errors
    ///
    /// Returns claim-validation or installed-state persistence failures.
    fn persist_claim(&mut self, claim: &AdapterClaim) -> Result<(), AdapterError>;
}

#[cfg(test)]
impl EnableProgress for () {
    fn persist_claim(&mut self, _claim: &AdapterClaim) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Outcome of [`FrameworkDriver::disable`].
#[derive(Debug, Clone, Serialize)]
pub struct DisableReport {
    /// True when every claimed resource was successfully released. When
    /// false, the Manager keeps the receipt and marks it `cleanup_failed`.
    ///
    /// A false here does **not** mean nothing was released: cleanup can be
    /// partial, and the two fields cannot express that split, since `messages` is
    /// prose. So a driver that releases anything while `cleanup_complete` ends up
    /// false must also strike that ownership from the claim it was handed — the
    /// Manager re-persists the claim it kept, and a receipt still claiming a
    /// release that already happened makes the retry perform it a second time.
    pub cleanup_complete: bool,
    /// Human-readable notes (e.g. "openclaw CLI not found; assuming
    /// registry absent").
    pub messages: Vec<String>,
}

// ---------------------------------------------------------------------------
// Status report
// ---------------------------------------------------------------------------

/// Full status of one receipt: a human summary plus machine-readable
/// conditions. JSON output must keep `conditions` so distinct failures are
/// not flattened into one label.
#[derive(Debug, Clone, Serialize)]
pub struct AdapterStatusReport {
    /// One-line health summary.
    pub summary: AdapterSummary,
    /// Individual signals behind the summary.
    pub conditions: Vec<AdapterCondition>,
}

/// Human-facing one-line adapter health summary.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterSummary {
    /// Every critical signal is healthy.
    Healthy,
    /// At least one critical signal is missing or has drifted.
    Degraded,
    /// A prior cleanup did not complete.
    CleanupFailed,
    /// State cannot currently be verified reliably.
    Unknown,
}

/// One machine-readable signal contributing to a status summary.
#[derive(Debug, Clone, Serialize)]
pub struct AdapterCondition {
    /// Which signal this is.
    pub kind: AdapterConditionKind,
    /// Tri-state result.
    pub status: ConditionStatus,
    /// Optional human explanation (e.g. "plugin not in `plugins list`").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional pointer to the claim resource this condition is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<ClaimResourceRef>,
}

/// Reference to a [`ClaimResource`](super::claim::ClaimResource) by id.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimResourceRef {
    /// Resource id.
    pub id: String,
}

/// The kinds of signal a condition can report.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterConditionKind {
    /// The component/adapter source that originally supplied this receipt is
    /// still visible through ANOLISA's installed-state view. This manager-level
    /// condition lets automation distinguish a broken framework registration
    /// from an orphaned receipt whose system/user component source disappeared.
    SourceAvailable,
    /// The framework itself is detectable on the host.
    FrameworkDetected,
    /// Package-manager-owned source files still match current package metadata.
    ManagedBundleMatches,
    /// The package-manager-owned adapter inputs still equal the enable-time
    /// revision, including the resolved source root.
    SourceRevisionMatches,
    /// Files ANOLISA explicitly copied still match their receipt entries.
    MaterializedBundleMatches,
    /// The plugin is still present in the framework registry.
    PluginRegistered,
    /// The framework reports the plugin's declared resources as loaded.
    PluginResourcesLoaded,
    /// The framework plugins this adapter displaced are still displaced, so the
    /// tool names the adapter registered are still the ones answering. A host
    /// that re-enabled one of them has the original first-wins collision back
    /// even though the adapter's own plugin still lists and loads fine.
    DisplacedPluginsReleased,
    /// The framework-native activation policy currently enables the plugin.
    ActivationEnabled,
    /// A marketplace source is still registered (future drivers).
    MarketplaceRegistered,
    /// A claimed directory tree still exists unmodified (future drivers).
    TreePresent,
    /// Injected JSON keys still equal ANOLISA's last-applied values
    /// (future drivers).
    JsonKeysPresent,
    /// A claimed symlink still points at the expected target (future
    /// drivers).
    SymlinkPresent,
    /// A prior cleanup completed.
    CleanupComplete,
    /// Whether this driver supports reliable read-only verification at
    /// all. `False` means the related conditions are `Unknown` rather than
    /// faked healthy.
    VerificationSupported,
}

/// Tri-state condition result. `Unknown` is distinct from `False`: it
/// means "could not verify", never "verified absent".
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConditionStatus {
    /// Signal verified present/healthy.
    True,
    /// Signal verified absent/unhealthy.
    False,
    /// Signal could not be verified.
    Unknown,
}

// ---------------------------------------------------------------------------
// Controlled IO helpers
// ---------------------------------------------------------------------------

/// A framework CLI invocation, built by a driver from a static command
/// template plus validated arguments. Executed by [`AdapterOps`] as an
/// argv array — never through a shell.
#[derive(Debug, Clone)]
pub struct FrameworkCommand {
    /// Executable to run (resolved via `PATH` with [`Self::path_prepend`]).
    pub program: String,
    /// Argument vector. Each element is passed verbatim; no shell parsing.
    pub args: Vec<String>,
    /// Optional bytes written to the child's stdin before the pipe is closed.
    pub stdin: Option<Vec<u8>>,
    /// Environment variables to set on the child.
    pub env_set: Vec<(String, String)>,
    /// Environment variables to remove from the child.
    pub env_remove: Vec<String>,
    /// Directories prepended to `PATH` before spawning.
    pub path_prepend: Vec<PathBuf>,
    /// Hard timeout; the child is killed if it exceeds this.
    pub timeout: Duration,
}

/// One line-delimited JSON-RPC session with a framework server that speaks
/// stdio.
///
/// Distinct from [`FrameworkCommand::stdin`], which writes its bytes and
/// immediately closes the pipe: a request/response server treats that EOF as
/// a shutdown signal and may drop requests it has not dispatched yet (Codex's
/// `app-server` answers only `initialize` before tearing down). The runner
/// therefore keeps stdin open until [`Self::expected_responses`] id-bearing
/// replies have been read, then closes it so the child exits.
#[derive(Debug, Clone)]
pub struct FrameworkRpcSession {
    /// Command that starts the server in stdio mode. Its
    /// [`FrameworkCommand::timeout`] bounds the whole session, and its
    /// [`FrameworkCommand::stdin`] is ignored in favor of
    /// [`Self::requests`].
    pub command: FrameworkCommand,
    /// Serialized JSON-RPC request objects, written in order, one per line.
    pub requests: Vec<String>,
    /// How many id-bearing responses to await before closing the child's
    /// stdin. A session that times out before reaching this count still
    /// returns whatever was read, with `timed_out` set.
    pub expected_responses: usize,
}

/// Captured output of a [`FrameworkCommand`]. stdout/stderr are truncated
/// to a bounded size before being returned and logged.
#[derive(Debug, Clone)]
pub struct CliOutput {
    /// Exit code, or `None` when the child was killed (e.g. on timeout).
    pub status: Option<i32>,
    /// True when the child was killed because it exceeded the timeout.
    pub timed_out: bool,
    /// Truncated stdout (UTF-8 lossy).
    pub stdout: String,
    /// Truncated stderr (UTF-8 lossy).
    pub stderr: String,
}

impl CliOutput {
    /// True iff the process exited zero and was not killed by timeout.
    pub fn success(&self) -> bool {
        self.status == Some(0) && !self.timed_out
    }
}

/// The closed set of dangerous IO a driver may perform, mediated by the
/// Manager.
///
/// The boundary grows one reviewed method at a time rather than shipping
/// unused surface: `run_framework_cli` plus the tree-copy helpers landed
/// with OpenClaw/Hermes, [`write_file`](AdapterOps::write_file) /
/// [`create_symlink`](AdapterOps::create_symlink) landed with the
/// Codex/Claude Code drivers, and [`read_file`](AdapterOps::read_file)
/// landed with the Qoder driver (which must read the user's
/// `settings.json` back before merging into it).
/// [`run_framework_rpc`](AdapterOps::run_framework_rpc) landed with Codex
/// hook trust, whose framework-side read and write are only reachable through
/// the `codex app-server` JSON-RPC protocol.
pub trait AdapterOps {
    /// Spawn a framework CLI with a timeout, capture and truncate its
    /// output, and record the invocation in the central log. The argv is
    /// executed directly (no shell), so receipt-derived data cannot inject
    /// extra commands.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the process cannot be spawned.
    /// A non-zero exit or a timeout is reported through [`CliOutput`], not
    /// as an error, so the driver decides how to interpret it.
    fn run_framework_cli(&self, cmd: FrameworkCommand) -> Result<CliOutput, AdapterError>;

    /// Spawn a framework CLI whose stdout is structured JSON that must be
    /// captured beyond the ordinary diagnostic-output limit. The Manager
    /// still applies a larger finite bound and keeps stderr at the normal
    /// cap, so unexpectedly large framework output cannot grow without
    /// limit.
    ///
    /// Custom operation providers default to the ordinary runner; the
    /// production Manager overrides this method with the structured-output
    /// capture policy.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the process cannot be spawned.
    fn run_framework_cli_json(&self, cmd: FrameworkCommand) -> Result<CliOutput, AdapterError> {
        self.run_framework_cli(cmd)
    }

    /// Drive a line-delimited JSON-RPC session against a framework server,
    /// holding the child's stdin open until the expected replies arrive (see
    /// [`FrameworkRpcSession`]). Returns the collected stdout lines under the
    /// same bounded structured-output policy as
    /// [`Self::run_framework_cli_json`].
    ///
    /// The default implementation refuses rather than degrading to
    /// write-then-EOF, which would silently drop requests: an operation
    /// provider that has not opted in must fail loudly.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the process cannot be spawned or
    /// the provider does not support RPC sessions.
    fn run_framework_rpc(&self, session: FrameworkRpcSession) -> Result<CliOutput, AdapterError> {
        Err(AdapterError::FrameworkCli {
            program: session.command.program,
            reason: "this operation provider does not support stdio JSON-RPC sessions".to_string(),
        })
    }

    /// Recursively copy a directory tree from `src` to `dst`. The Manager
    /// validates that `dst` is under an allowed external root before
    /// executing. `src` must be under the resource root or an allowed
    /// root. Creates `dst` and any missing parents.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Io`] on filesystem failure;
    /// [`AdapterError::ClaimValidation`] if `dst` fails boundary check.
    fn copy_tree(&self, src: &Path, dst: &Path) -> Result<(), AdapterError>;

    /// Copy a single file from `src` to `dst`. Both paths are validated
    /// against the allowed roots. Creates parent directories of `dst`.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Io`] on filesystem failure;
    /// [`AdapterError::ClaimValidation`] if either path fails boundary check.
    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), AdapterError>;

    /// Remove exactly one filesystem entry without recursively deleting a
    /// directory. Files and symlinks are unlinked; empty directories are
    /// removed so a materialized path can change from a directory prefix to
    /// a file. Returns `Ok(false)` when the path does not exist.
    ///
    /// The Manager validates that `path` is under an allowed root. Refusing
    /// non-empty directories preserves runtime-created files that were never
    /// owned by an adapter receipt.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Io`] on filesystem failure, including a non-empty
    /// directory; [`AdapterError::ClaimValidation`] if `path` fails boundary
    /// validation.
    fn remove_path(&self, path: &Path) -> Result<bool, AdapterError> {
        Err(AdapterError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "operation provider does not support exact path removal",
            ),
        })
    }

    /// Remove a directory tree rooted at `path`. The Manager validates
    /// that `path` is under an allowed external root before executing.
    /// Returns `Ok(false)` when the path does not exist (idempotent).
    ///
    /// # Errors
    ///
    /// [`AdapterError::Io`] on filesystem failure;
    /// [`AdapterError::ClaimValidation`] if `path` fails boundary check.
    fn remove_tree(&self, path: &Path) -> Result<bool, AdapterError>;

    /// Write `contents` to `path`, creating parent directories. The
    /// Manager validates that `path` is under an allowed external root
    /// before executing. An existing file is overwritten (idempotent
    /// re-enable). The bytes are pure data built by the driver — never an
    /// executable path or argv.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Io`] on filesystem failure;
    /// [`AdapterError::ClaimValidation`] if `path` fails boundary check.
    fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), AdapterError>;

    /// Create a symlink at `link` pointing to `target`, creating `link`'s
    /// parent directories. The Manager validates that **both** `link` and
    /// `target` are under an allowed external root before executing. When a
    /// symlink already exists at `link`, it is replaced (idempotent
    /// re-enable); a non-symlink at `link` is an error so ANOLISA never
    /// clobbers a real file it did not create.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Io`] on filesystem failure or when `link` exists and
    /// is not a symlink; [`AdapterError::ClaimValidation`] if either path
    /// fails boundary check.
    fn create_symlink(&self, link: &Path, target: &Path) -> Result<(), AdapterError>;

    /// Read `path`, returning `Ok(None)` when it does not exist (so a
    /// caller merging into a possibly-absent config file can treat "absent"
    /// distinctly from "unreadable"). The Manager validates that `path` is
    /// under an allowed external root before reading, so a driver cannot use
    /// this to exfiltrate an arbitrary file (e.g. `~/.ssh/id_rsa`); a
    /// symlink at `path` is refused rather than followed out of the boundary.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Io`] on a read failure other than not-found;
    /// [`AdapterError::ClaimValidation`] if `path` fails boundary check.
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, AdapterError>;
}

// ---------------------------------------------------------------------------
// Driver trait
// ---------------------------------------------------------------------------

/// One framework's adapter semantics. Built-in and closed: new frameworks
/// ship in an ANOLISA release, never as a runtime plugin.
pub trait FrameworkDriver: Send + Sync {
    /// Framework name, e.g. `"openclaw"`.
    fn name(&self) -> &'static str;

    /// Read-only probe for whether the framework is usable. May inspect
    /// `PATH` and the filesystem; must not spawn the framework.
    fn detect(&self, env: &HostEnv) -> DetectResult;

    /// Cheap read-only check that `resource_root` plausibly holds a real
    /// bundle for this framework. The Manager consults it when resolving
    /// the adapter source across multiple datadir roots so a stale or
    /// empty leftover directory in one root cannot shadow (or impersonate)
    /// the real bundle in another.
    ///
    /// Must mirror [`Self::read_bundle`]'s *mandatory file* checks without
    /// parsing or digesting, and must never be stricter than `read_bundle`
    /// (a root this probe rejects must also fail `read_bundle`, otherwise
    /// resolution could skip a bundle enable would accept). The default
    /// accepts any non-empty directory, matching the frameworks whose
    /// `read_bundle` requires no specific file (OpenClaw, Hermes).
    fn probe_bundle(&self, resource_root: &Path, _declared_entry: Option<&str>) -> bool {
        resource_root
            .read_dir()
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
    }

    /// External roots (outside ANOLISA-owned roots) this driver is allowed
    /// to write into. The Manager validates every
    /// [`ClaimResourceKind::ExternalPath`](super::claim::ClaimResourceKind::ExternalPath)
    /// against this set. The result may expand `ctx.user_home`, but must
    /// not be derived from receipt contents (which would let a forged
    /// receipt authorize itself).
    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf>;

    /// Parse the framework-native bundle from the resource directory.
    ///
    /// # Errors
    ///
    /// [`AdapterError::BundleInvalid`] when required files are missing or
    /// unreadable.
    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError>;

    /// Describe what [`Self::apply_enable`] would do, for dry-run and
    /// confirmation.
    ///
    /// `prior` is the validated receipt this enable would replace, when there is
    /// one — i.e. the operation is a re-enable. A driver that carries facts
    /// across a re-enable (see [`Self::preserve_reenable_facts`]) must plan from
    /// those facts and not from a fresh host probe: by re-enable time the host
    /// already reflects the prior enable's mutations, so a probe can report the
    /// exact opposite of what the real lifecycle will do. Drivers that fully
    /// supersede their prior receipt ignore it.
    ///
    /// # Errors
    ///
    /// Propagates bundle/validation errors encountered while planning.
    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        prior: Option<&AdapterClaim>,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError>;

    /// Describe cleanup that will run before a validated prior receipt is
    /// replaced during re-enable.
    ///
    /// The Manager prepends these descriptions to the ordinary enable plan,
    /// preserving the real execution order. Implementations must remain
    /// read-only; the default represents drivers whose replacement happens in
    /// place and requires no separate cleanup.
    ///
    /// # Errors
    ///
    /// Returns a driver-specific receipt consistency error.
    fn plan_reenable_cleanup(
        &self,
        _prior: &AdapterClaim,
        _ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        Ok(Vec::new())
    }

    /// Build the pure-data receipt for a future enable operation without
    /// mutating framework state, together with any driver-private
    /// [`PreparedEnable`] state (host capabilities resolved by read-only
    /// probing) that [`Self::apply_enable`] should reuse instead of
    /// re-probing.
    ///
    /// `prior` is the validated receipt this enable would replace, when there is
    /// one — the same argument [`Self::plan_enable`] receives, and validated by
    /// the Manager *before* this hook runs so a forged receipt cannot shape the
    /// real enable any more than it may shape the plan. A driver that inherits
    /// ownership across a re-enable (see [`Self::preserve_reenable_facts`]) needs
    /// it for the same reason: what the prior receipt already owns is what this
    /// enable must not later treat as its own doing. Drivers that fully supersede
    /// their prior receipt ignore it.
    ///
    /// The Manager validates and persists this claim before
    /// [`Self::apply_enable`] runs, so a later framework-side failure stays
    /// visible to status/disable. The returned [`PreparedEnable`] is **not**
    /// persisted — it flows in-memory to `apply_enable` within the same locked
    /// enable, keeping the receipt pure typed resource data.
    ///
    /// # Errors
    ///
    /// [`AdapterError::BundleInvalid`] when the bundle cannot produce the
    /// framework identifiers needed for a receipt.
    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        prior: Option<&AdapterClaim>,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError>;

    /// Carry still-relevant facts from a validated prior receipt into the
    /// newly prepared receipt before it replaces prior state.
    ///
    /// Most drivers fully supersede their prior receipt and need no merge.
    /// Drivers whose mutations are not reversed by re-enable can override
    /// this hook so an early re-enable failure does not erase cleanup facts.
    ///
    /// `ctx` carries the contract being enabled *now*. A driver that inherits
    /// ownership must filter it through that contract: the prior receipt records
    /// what an older version of the component claimed, and a declaration the new
    /// version dropped is exactly the fact that must not survive. Whatever this
    /// hook declines to carry over, [`Self::cleanup_replaced_claim`] is then
    /// responsible for releasing before the prior receipt stops being durable.
    ///
    /// # Errors
    ///
    /// Returns a driver-specific receipt consistency error.
    fn preserve_reenable_facts(
        &self,
        _prior: &AdapterClaim,
        _next: &mut AdapterClaim,
        _ctx: &DriverCtx,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Release prior resources that the replacement receipt cannot continue
    /// to own.
    ///
    /// The Manager calls this only after validating both receipts and keeps
    /// the prior receipt durable until cleanup completes. Drivers should make
    /// the operation idempotent and return `cleanup_complete = false` rather
    /// than abandon any resource still requiring a later disable retry. The
    /// default keeps existing re-enable behavior for drivers whose new receipt
    /// fully supersedes the old one in place.
    ///
    /// `prior` is mutable for the same reason [`Self::disable`]'s claim is: a
    /// driver that hands something back on the host while cleanup is still
    /// incomplete must be able to strike that ownership from the receipt the
    /// Manager is about to keep, or the kept receipt describes a host state
    /// that no longer exists and a retry undoes whatever the operator did in
    /// between. The Manager persists the mutation before reporting the failure,
    /// whether that failure is an incomplete report or an error returned
    /// partway through the cleanup.
    ///
    /// # Errors
    ///
    /// Returns a driver-specific cleanup error.
    fn cleanup_replaced_claim(
        &self,
        _prior: &mut AdapterClaim,
        _next: &AdapterClaim,
        _ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        Ok(DisableReport {
            cleanup_complete: true,
            messages: Vec::new(),
        })
    }

    /// Describe what [`Self::disable`] would hand back to the framework, for
    /// `disable --dry-run`.
    ///
    /// The Manager can list the resources a receipt claims, but only the driver
    /// knows which of them it would actually restore and under what condition —
    /// a restore can be vetoed by host state the receipt does not record. Letting
    /// the Manager compose that text is how a preview comes to promise a mutation
    /// the real disable then declines to perform, and because a declined restore
    /// still counts as completed cleanup the receipt disappears afterwards,
    /// leaving the operator nothing to notice the divergence with.
    ///
    /// Implementations must be **read-only**: probing the host is fine, mutating
    /// it or the receipt is not. The default is empty, for drivers whose disable
    /// hands nothing back.
    ///
    /// # Errors
    ///
    /// Returns a driver-specific receipt consistency error.
    fn plan_disable_restores(
        &self,
        _claim: &AdapterClaim,
        _ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        Ok(Vec::new())
    }

    /// Validate a persisted receipt the Manager is about to act on, beyond the
    /// generic [`AdapterClaim::validate_with_trust`] pass.
    ///
    /// Called on **both** the real and the `--dry-run` path of `disable`, before
    /// either one does anything. The generic pass checks paths and trust; it does
    /// not know that a driver's payload cross-references its own resource list,
    /// so a receipt whose references dangle, are mistyped, or are duplicated
    /// passes it and then fails the moment the driver tries to consume it. Running
    /// this hook on the dry-run path too is what keeps a preview honest: a plan
    /// that succeeds for a receipt the real operation will refuse tells the
    /// operator the disable is available when it is not.
    ///
    /// The default accepts, for drivers whose payload carries no such references.
    ///
    /// # Errors
    ///
    /// Returns a driver-specific receipt consistency error.
    fn validate_claim(&self, _claim: &AdapterClaim) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Validate the final prepared receipt after re-enable facts have been
    /// preserved, but before the Manager persists it or mutates framework
    /// state.
    ///
    /// Drivers can use this hook for ownership conflicts that are visible
    /// during read-only preparation but may be resolved by a validated prior
    /// receipt.
    ///
    /// # Errors
    ///
    /// Returns a driver-specific ownership or receipt consistency error.
    fn validate_prepared_enable(&self, _claim: &AdapterClaim) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Source-to-resource copies this driver will explicitly materialize.
    ///
    /// The Manager maps only package-owned source entries through these
    /// declarations before apply. Framework-native staging or copies remain
    /// outside this contract.
    fn materialized_mappings(
        &self,
        _resource_root: &Path,
        _adapter_type: Option<&str>,
        _declared_skills: &[DeclaredSkill],
    ) -> Vec<MaterializedMapping> {
        Vec::new()
    }

    /// Concrete destination roots for [`Self::materialized_mappings`].
    ///
    /// Dry-run cleanup compares these paths with the prior receipt so its
    /// stale-output plan matches live re-enable even when a stable resource
    /// id moves to a different framework path.
    ///
    /// # Errors
    ///
    /// Returns the same destination or identifier validation error as
    /// [`Self::prepare_enable`].
    fn materialized_destination_roots(
        &self,
        _bundle: &AdapterBundle,
        _ctx: &DriverCtx,
    ) -> Result<BTreeMap<String, PathBuf>, AdapterError> {
        Ok(BTreeMap::new())
    }

    /// Whether this claim is expected to carry a materialized-file list.
    /// Receipts with an empty list report `Unknown`; drivers whose framework
    /// performs its own copy omit the condition entirely.
    fn materialized_verification_applicable(&self, _claim: &AdapterClaim) -> bool {
        false
    }

    /// Idempotently apply an already-persisted enable receipt to framework
    /// state, reusing the [`PreparedEnable`] state produced by
    /// [`Self::prepare_enable`] so no read-only host probe runs twice in one
    /// enable. When a driver applies resources incrementally, it journals
    /// intent and confirmation transitions through `progress`.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] or [`AdapterError::BundleInvalid`] on
    /// failure.
    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        prepared: &PreparedEnable,
        ctx: &DriverCtx,
        progress: &mut dyn EnableProgress,
    ) -> Result<(), AdapterError>;

    /// Read-only status check against a receipt. Must not mutate state.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when a verification probe cannot run
    /// (as opposed to running and finding the plugin absent).
    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError>;

    /// Idempotently disable the adapter, removing only what the receipt
    /// declares ANOLISA took over.
    ///
    /// The claim is mutable so a driver whose cleanup can *partially* fail can
    /// keep the receipt in step with the host. When cleanup completes the
    /// Manager removes the receipt and the mutation is moot; when it does not,
    /// the Manager keeps and re-persists this claim, and any ownership the
    /// driver already handed back must have been struck from it first. A
    /// driver that cannot report that per resource — because the release is
    /// only visible as prose in [`DisableReport::messages`] — leaves the kept
    /// receipt claiming a transition it no longer owns, and a retry then
    /// performs it a second time, over whatever the operator did in between.
    /// Drivers that release nothing the receipt tracks may ignore the mutability.
    ///
    /// Returning an error is not a way out of that duty either. The Manager
    /// persists the claim as the driver left it before propagating the error, so
    /// a release already performed on the host is recorded even when the cleanup
    /// never got to report on it — but only the mutations actually made: an
    /// error raised before the first one leaves the receipt untouched.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when de-registration fails in a way
    /// that is not simply "framework absent".
    fn disable(
        &self,
        claim: &mut AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError>;
}

/// Scan candidate directories of `PATH` for an executable named `name`,
/// honoring the executable bit on Unix. Shared by drivers' `detect`.
pub fn find_binary_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() && is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(crate) fn is_executable(_path: &Path) -> bool {
    true
}
