//! Generic osbase install entry layer — TOML-manifest-driven.
//!
//! The install pipeline reads scenario definitions from `sandbox.toml`
//! (deployed by `anolisa system setup` to `/etc/anolisa/sandbox.toml`)
//! and executes a five-phase flow:
//!
//!   1. Preflight  — kernel version gate, KVM check if required
//!   2. Packages   — `dnf install -y <packages>` from manifest
//!   3. Services   — `systemctl enable --now` for each service
//!   4. Verify     — scenario-aware checks from `verify_commands` in manifest
//!   5. State      — persist to `installed.toml`
//!
//! Currently serves the "beginner" scenario only: zero optional
//! parameters, full-stack install from manifest.

use anolisa_env::EnvFacts;
use anolisa_platform::command::{CommandRunner, InheritedLocaleCommandRunner};
use anolisa_platform::fs_layout::FsLayout;
use chrono::{SecondsFormat, Utc};

use crate::domain::{
    Installation, InstallationScope, LifecycleStatus, ManagementRelation, NativePm,
    PackageIdentity, ProviderBinding,
};
use crate::lock::{InstallLock, LockError};
use crate::sandbox_manifest::{ManifestError, SandboxManifest, ScenarioConfig};
use crate::state::ObjectKind;
use crate::state_store::StateStore;

// ===========================================================================
// Public types
// ===========================================================================

/// The three osbase domains. Each domain owns a distinct install pipeline;
/// dispatch happens in [`execute_install`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsbaseDomain {
    /// Linux kernel variants (e.g. `agentic`, `vanilla`).
    Kernel,
    /// Sandbox engines (runc / rund / firecracker / gvisor / landlock).
    Sandbox,
    /// Security primitives (LSMs, audit, seccomp profiles).
    Security,
}

impl OsbaseDomain {
    /// Stable lower-case identifier used in logs and error strings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kernel => "kernel",
            Self::Sandbox => "sandbox",
            Self::Security => "security",
        }
    }
}

/// Whether to register the engine into a containerd handler entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegisterHandler {
    /// Register with containerd via the appropriate shim.
    #[default]
    Containerd,
    /// Standalone install — no L2 runtime wiring.
    None,
}

/// Generic install request for any osbase domain.
#[derive(Debug, Clone)]
pub struct OsbaseInstallRequest {
    /// Which domain pipeline to dispatch to.
    pub domain: OsbaseDomain,
    /// Scenario name (Sandbox) or variant (Kernel/Security). Must be
    /// non-empty; matched against the manifest.
    pub target: String,
    /// L2 handler registration mode.
    pub register_handler: RegisterHandler,
    /// Additionally create a Kubernetes `RuntimeClass` after handler
    /// registration.
    pub register_runtimeclass: bool,
    /// Optional `--config` override path.
    pub config_override: Option<String>,
    /// Mark the installed engine as the default runtime for its handler.
    pub set_default: bool,
    /// Bypass non-fatal pre-flight gates.
    pub force: bool,
    /// Skip the post-install verify phase.
    pub skip_verify: bool,
    /// Produce a plan without side effects.
    pub dry_run: bool,
}

/// Aggregate outcome of a generic install.
#[derive(Debug, Clone)]
pub struct OsbaseInstallOutcome {
    pub domain: OsbaseDomain,
    pub target: String,
    pub phases: Vec<PhaseResult>,
    /// `0` success, `1` failed, `2` degraded.
    pub exit_code: i32,
    /// Real degraded-verification or phase warnings.
    pub warnings: Vec<String>,
    /// Informational hints (e.g. optional packages available). Not counted
    /// as warnings and do not affect `exit_code`.
    pub hints: Vec<String>,
}

/// Per-phase result.
#[derive(Debug, Clone)]
pub struct PhaseResult {
    pub name: String,
    pub status: PhaseStatus,
    pub message: Option<String>,
    pub duration_ms: Option<u64>,
}

/// Status of a single phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseStatus {
    Success,
    Skipped,
    Degraded,
    Failed,
}

/// Errors surfaced by the generic install entry.
#[derive(Debug, thiserror::Error)]
pub enum OsbaseInstallError {
    #[error("unsupported: {0}")]
    Unsupported(String),

    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },

    #[error("phase '{phase}' failed: {message}")]
    PhaseFailed { phase: String, message: String },

    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ===========================================================================
// Entry point
// ===========================================================================

/// Validate the request and dispatch to the appropriate domain pipeline.
pub fn execute_install(
    request: &OsbaseInstallRequest,
    env: &EnvFacts,
) -> Result<OsbaseInstallOutcome, OsbaseInstallError> {
    validate_request(request, env)?;

    match request.domain {
        OsbaseDomain::Sandbox => sandbox_dispatch(request, env),
        OsbaseDomain::Kernel => Err(OsbaseInstallError::InvalidRequest {
            reason: "kernel install not yet implemented".to_string(),
        }),
        OsbaseDomain::Security => Err(OsbaseInstallError::InvalidRequest {
            reason: "security install not yet implemented".to_string(),
        }),
    }
}

/// List all available scenarios from the manifest.
pub fn list_scenarios() -> Result<Vec<String>, OsbaseInstallError> {
    let manifest = SandboxManifest::load()?;
    Ok(manifest
        .scenario_names()
        .into_iter()
        .map(String::from)
        .collect())
}

/// Uninstall packages for a given scenario via `dnf remove -y`.
///
/// - If the scenario is not found in the manifest → error
/// - If the scenario has no packages (e.g. landlock) → "nothing to uninstall"
/// - Otherwise → `dnf remove -y <packages>`
pub fn execute_uninstall(scenario: &str, dry_run: bool) -> Result<String, OsbaseInstallError> {
    let manifest = SandboxManifest::load()?;
    execute_uninstall_with(scenario, dry_run, &manifest, &InheritedLocaleCommandRunner)
}

fn execute_uninstall_with(
    scenario: &str,
    dry_run: bool,
    manifest: &SandboxManifest,
    runner: &impl CommandRunner,
) -> Result<String, OsbaseInstallError> {
    let config = manifest.find_scenario(scenario).ok_or_else(|| {
        let available = manifest.scenario_names().join(", ");
        OsbaseInstallError::InvalidRequest {
            reason: format!("unknown sandbox scenario '{scenario}'; available: [{available}]"),
        }
    })?;

    eprintln!("[osbase] scenario: {scenario}");

    if config.packages.is_empty() {
        return Ok(format!(
            "scenario '{scenario}': nothing to uninstall (no packages defined)"
        ));
    }

    let pkg_list = config.packages.join(" ");

    if dry_run {
        eprintln!("[osbase] [dry-run] would remove packages: {pkg_list}");
        eprintln!("[osbase] [dry-run] no packages will be removed in dry-run mode");
        return Ok(format!("dry-run: would uninstall: {pkg_list}"));
    }

    eprintln!("[osbase] removing packages: {pkg_list}");

    match run_dnf_remove(&config.packages, runner) {
        Ok(msg) => {
            eprintln!("[osbase] dnf remove completed (exit_code=0)");
            eprintln!("[osbase] removed successfully");
            Ok(msg)
        }
        Err(msg) => {
            eprintln!("[osbase] dnf remove failed");
            Err(OsbaseInstallError::PhaseFailed {
                phase: "uninstall".to_string(),
                message: msg,
            })
        }
    }
}

/// Execute `dnf remove -y -q <packages>`.
fn run_dnf_remove(packages: &[String], runner: &impl CommandRunner) -> Result<String, String> {
    let mut args = vec!["remove", "-y", "-q"];
    args.extend(packages.iter().map(String::as_str));
    let output = runner
        .run("dnf", &args)
        .map_err(|e| format!("failed to execute dnf: {e}"))?;

    if output.code == Some(0) {
        Ok(format!("uninstalled: {}", packages.join(" ")))
    } else {
        let stderr = output.stderr;
        // Print stderr on failure for diagnostics
        let stderr_str = stderr.trim();
        if !stderr_str.is_empty() {
            eprintln!("[osbase] dnf stderr:\n{stderr_str}");
        }
        Err(format!(
            "dnf remove failed (exit={}): {}",
            output.code.unwrap_or(-1),
            stderr.lines().take(5).collect::<Vec<_>>().join("\n")
        ))
    }
}

/// Lightweight request validation.
pub fn validate_request(
    request: &OsbaseInstallRequest,
    env: &EnvFacts,
) -> Result<(), OsbaseInstallError> {
    if request.target.trim().is_empty() {
        return Err(OsbaseInstallError::InvalidRequest {
            reason: "target must not be empty".to_string(),
        });
    }

    if request.register_runtimeclass && request.register_handler == RegisterHandler::None {
        return Err(OsbaseInstallError::InvalidRequest {
            reason: "--register-runtimeclass requires a non-None --register-handler".to_string(),
        });
    }

    if env.uid != 0 {
        return Err(OsbaseInstallError::InvalidRequest {
            reason: "osbase requires root (uid=0); re-run with sudo".to_string(),
        });
    }

    Ok(())
}

// ===========================================================================
// Sandbox dispatch — manifest-driven
// ===========================================================================

/// Load the manifest, find the scenario, and run the simplified install.
fn sandbox_dispatch(
    request: &OsbaseInstallRequest,
    env: &EnvFacts,
) -> Result<OsbaseInstallOutcome, OsbaseInstallError> {
    let layout = FsLayout::system(None);
    let runner = InheritedLocaleCommandRunner;
    let runtime = HostManifestInstallRuntime { runner: &runner };
    let manifest = SandboxManifest::load()?;
    sandbox_dispatch_with(request, env, &layout, &runtime, &manifest)
}

fn sandbox_dispatch_with(
    request: &OsbaseInstallRequest,
    env: &EnvFacts,
    layout: &FsLayout,
    runtime: &impl ManifestInstallRuntime,
    manifest: &SandboxManifest,
) -> Result<OsbaseInstallOutcome, OsbaseInstallError> {
    let scenario = manifest.find_scenario(&request.target).ok_or_else(|| {
        let available = manifest.scenario_names().join(", ");
        OsbaseInstallError::InvalidRequest {
            reason: format!(
                "unknown sandbox scenario '{}'; available: [{}]",
                request.target, available
            ),
        }
    })?;

    // Clone what we need before running phases (avoid borrow issues)
    let scenario = scenario.clone();

    if request.dry_run {
        load_state_for_layout(layout)?;
        eprintln!("[osbase] scenario: {}", scenario.name);
        let outcome = build_dry_run_outcome(request, &scenario);
        // Print phase plan in pipeline order so Direct and Helper paths
        // produce identical user-facing output.
        for phase in &outcome.phases {
            let msg = phase.message.as_deref().unwrap_or("");
            eprintln!("[osbase] [dry-run] {}: {msg}", phase.name);
        }
        for hint in &outcome.hints {
            eprintln!("[osbase] [dry-run] hint: {hint}");
        }
        return Ok(outcome);
    }

    run_manifest_install(request, env, &scenario, layout, runtime)
}

/// Build a dry-run outcome showing what would happen.
fn build_dry_run_outcome(
    request: &OsbaseInstallRequest,
    scenario: &ScenarioConfig,
) -> OsbaseInstallOutcome {
    let mut phases = Vec::new();

    // Preflight
    let mut preflight_msg = format!("check kernel {}", scenario.requires_kernel);
    if scenario.requires_kvm {
        preflight_msg.push_str("; check /dev/kvm");
    }
    phases.push(PhaseResult {
        name: "preflight".to_string(),
        status: PhaseStatus::Skipped,
        message: Some(preflight_msg),
        duration_ms: None,
    });

    // Packages
    let pkg_msg = if scenario.packages.is_empty() {
        "no packages to install".to_string()
    } else {
        format!("dnf install -y {}", scenario.packages.join(" "))
    };
    phases.push(PhaseResult {
        name: "packages".to_string(),
        status: PhaseStatus::Skipped,
        message: Some(pkg_msg),
        duration_ms: None,
    });

    // Services
    if scenario.services.is_empty() {
        phases.push(PhaseResult {
            name: "services".to_string(),
            status: PhaseStatus::Skipped,
            message: Some("no services for this scenario".to_string()),
            duration_ms: None,
        });
    } else {
        phases.push(PhaseResult {
            name: "services".to_string(),
            status: PhaseStatus::Skipped,
            message: Some(format!(
                "systemctl enable --now {}",
                scenario.services.join(" ")
            )),
            duration_ms: None,
        });
    }

    // Verify
    phases.push(PhaseResult {
        name: "verify".to_string(),
        status: PhaseStatus::Skipped,
        message: Some("post-install checks".to_string()),
        duration_ms: None,
    });

    // State
    phases.push(PhaseResult {
        name: "state".to_string(),
        status: PhaseStatus::Skipped,
        message: Some("persist to installed.toml".to_string()),
        duration_ms: None,
    });

    let mut hints = vec!["dry-run mode: no changes made".to_string()];
    if !scenario.packages_optional.is_empty() {
        hints.push(format!(
            "optional packages available: {}",
            scenario.packages_optional.join(" ")
        ));
    }

    OsbaseInstallOutcome {
        domain: request.domain,
        target: request.target.clone(),
        phases,
        exit_code: 0,
        warnings: vec![],
        hints,
    }
}

/// Enable and start systemd services.
fn run_enable_services(services: &[String], runner: &impl CommandRunner) -> Result<String, String> {
    let mut enabled = Vec::new();
    for svc in services {
        let output = runner
            .run("systemctl", &["enable", "--now", svc])
            .map_err(|e| format!("failed to run systemctl: {e}"))?;
        if output.code == Some(0) {
            eprintln!("[osbase] services: {svc}.service active \u{2713}");
            enabled.push(svc.clone());
        } else {
            let stderr = output.stderr;
            return Err(format!(
                "systemctl enable --now {svc} failed: {}",
                stderr.trim()
            ));
        }
    }
    Ok(format!("enabled: {}", enabled.join(", ")))
}

/// Result of scenario-aware post-install verification.
enum VerifyOutcome {
    /// All verify commands passed.
    Passed(String),
    /// No verify commands or services defined; nothing to verify.
    NothingToVerify,
    /// One or more checks failed (degraded, not fatal).
    Failed(String),
}

trait ManifestInstallRuntime {
    fn install_packages(&self, packages: &[String]) -> Result<String, String>;
    fn enable_services(&self, services: &[String]) -> Result<String, String>;
    fn verify(&self, scenario: &ScenarioConfig) -> VerifyOutcome;
}

struct HostManifestInstallRuntime<'a, R> {
    runner: &'a R,
}

impl<R: CommandRunner> ManifestInstallRuntime for HostManifestInstallRuntime<'_, R> {
    fn install_packages(&self, packages: &[String]) -> Result<String, String> {
        run_dnf_install(packages, self.runner)
    }

    fn enable_services(&self, services: &[String]) -> Result<String, String> {
        run_enable_services(services, self.runner)
    }

    fn verify(&self, scenario: &ScenarioConfig) -> VerifyOutcome {
        run_post_verify(scenario, self.runner)
    }
}

/// Scenario-aware post-install verification.
///
/// If `scenario.verify_commands` is non-empty, each entry is executed as a
/// shell-style command (split on whitespace). Otherwise, falls back to
/// `systemctl is-active` for each service declared in the scenario.
fn run_post_verify(scenario: &ScenarioConfig, runner: &impl CommandRunner) -> VerifyOutcome {
    let mut checks = Vec::new();

    if !scenario.verify_commands.is_empty() {
        // Use explicit verify commands from manifest.
        for cmd_str in &scenario.verify_commands {
            let parts: Vec<&str> = cmd_str.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }
            let (bin, args) = (parts[0], &parts[1..]);
            if let Err(e) = run_verify_cmd(bin, args, cmd_str, runner) {
                return VerifyOutcome::Failed(e);
            }
            checks.push(cmd_str.as_str());
        }
    } else if !scenario.services.is_empty() {
        // Fallback: check each service is active.
        for svc in &scenario.services {
            if let Err(e) = run_verify_cmd(
                "systemctl",
                &["is-active", svc],
                &format!("{svc} active"),
                runner,
            ) {
                return VerifyOutcome::Failed(e);
            }
            checks.push(svc.as_str());
        }
    } else {
        // No verify commands and no services — nothing to verify.
        return VerifyOutcome::NothingToVerify;
    }

    VerifyOutcome::Passed(format!("all checks passed: {}", checks.join(", ")))
}

/// Run a single verification command and report result.
fn run_verify_cmd(
    cmd: &str,
    args: &[&str],
    label: &str,
    runner: &impl CommandRunner,
) -> Result<(), String> {
    let output = runner
        .run(cmd, args)
        .map_err(|e| format!("{label}: command not found — is the package installed? ({e})"))?;
    if output.code == Some(0) {
        let stdout = output.stdout;
        let first_line = stdout.lines().next().unwrap_or("");
        eprintln!("[osbase] verify: {label} \u{2713} {first_line}");
        Ok(())
    } else {
        let stderr = output.stderr;
        let hint = stderr.lines().next().unwrap_or("").trim();
        if hint.is_empty() {
            Err(format!(
                "{label} failed (exit {})",
                output.code.unwrap_or(-1)
            ))
        } else {
            Err(format!(
                "{label} failed (exit {}): {hint}",
                output.code.unwrap_or(-1)
            ))
        }
    }
}

fn load_state_for_layout(layout: &FsLayout) -> Result<StateStore, OsbaseInstallError> {
    let state_path = layout.state_dir.join("installed.toml");
    StateStore::load_for_layout(
        &state_path,
        anolisa_platform::privilege::effective_uid(),
        layout,
    )
    .map_err(|error| OsbaseInstallError::PhaseFailed {
        phase: "state".to_string(),
        message: format!("failed to load state: {error}"),
    })
}

/// Execute the five-phase manifest-driven install:
/// 1. Preflight (kernel + KVM)
/// 2. Packages (full stack from manifest)
/// 3. Services (systemctl enable --now)
/// 4. Verify (scenario-aware: verify_commands from manifest, or service checks)
/// 5. State (persist to installed.toml)
fn run_manifest_install(
    request: &OsbaseInstallRequest,
    env: &EnvFacts,
    scenario: &ScenarioConfig,
    layout: &FsLayout,
    runtime: &impl ManifestInstallRuntime,
) -> Result<OsbaseInstallOutcome, OsbaseInstallError> {
    let mut phases = Vec::new();
    let mut warnings = Vec::new();

    eprintln!("[osbase] scenario: {}", scenario.name);

    // ─── Phase 1: Preflight ──────────────────────────────────────────────
    // Preflight is read-only (kernel/KVM checks); runs before the lock.
    let preflight_result = run_preflight(env, scenario, request.force);
    match preflight_result {
        Ok(msg) => {
            phases.push(PhaseResult {
                name: "preflight".to_string(),
                status: PhaseStatus::Success,
                message: Some(msg),
                duration_ms: None,
            });
        }
        Err(reason) => {
            eprintln!("[osbase] error: {reason}");
            phases.push(PhaseResult {
                name: "preflight".to_string(),
                status: PhaseStatus::Failed,
                message: Some(reason),
                duration_ms: None,
            });
            return Ok(OsbaseInstallOutcome {
                domain: request.domain,
                target: request.target.clone(),
                phases,
                exit_code: 1,
                warnings,
                hints: vec![],
            });
        }
    }

    // ─── Acquire InstallLock ─────────────────────────────────────────────
    // Lock covers the full mutation window: packages → services → state.
    // Held until the function returns (drop releases the lock).
    let _lock = InstallLock::acquire(&layout.lock_file).map_err(|e| match e {
        LockError::Held { path } => OsbaseInstallError::PhaseFailed {
            phase: "lock".to_string(),
            message: format!(
                "install lock at {} is held by another process; try again later",
                path.display()
            ),
        },
        other => OsbaseInstallError::PhaseFailed {
            phase: "lock".to_string(),
            message: format!("failed to acquire install lock: {other}"),
        },
    })?;
    let state_path = layout.state_dir.join("installed.toml");
    let mut store = load_state_for_layout(layout)?;

    // ─── Phase 2: Packages ───────────────────────────────────────────────
    if scenario.packages.is_empty() {
        phases.push(PhaseResult {
            name: "packages".to_string(),
            status: PhaseStatus::Skipped,
            message: Some("no packages required for this scenario".to_string()),
            duration_ms: None,
        });
    } else {
        let pkg_list = scenario.packages.join(" ");
        eprintln!("[osbase] installing packages: {pkg_list}");
        match runtime.install_packages(&scenario.packages) {
            Ok(msg) => {
                eprintln!("[osbase] dnf install completed (exit_code=0)");
                phases.push(PhaseResult {
                    name: "packages".to_string(),
                    status: PhaseStatus::Success,
                    message: Some(msg),
                    duration_ms: None,
                });
            }
            Err(reason) => {
                eprintln!("[osbase] dnf install failed");
                phases.push(PhaseResult {
                    name: "packages".to_string(),
                    status: PhaseStatus::Failed,
                    message: Some(reason),
                    duration_ms: None,
                });
                return Ok(OsbaseInstallOutcome {
                    domain: request.domain,
                    target: request.target.clone(),
                    phases,
                    exit_code: 1,
                    warnings,
                    hints: vec![],
                });
            }
        }
    }

    // ─── Phase 3: Services ───────────────────────────────────────────────
    if scenario.services.is_empty() {
        phases.push(PhaseResult {
            name: "services".to_string(),
            status: PhaseStatus::Skipped,
            message: Some("no services for this scenario".to_string()),
            duration_ms: None,
        });
    } else {
        eprintln!(
            "[osbase] enabling services: {}",
            scenario.services.join(", ")
        );
        match runtime.enable_services(&scenario.services) {
            Ok(msg) => {
                phases.push(PhaseResult {
                    name: "services".to_string(),
                    status: PhaseStatus::Success,
                    message: Some(msg),
                    duration_ms: None,
                });
            }
            Err(reason) => {
                eprintln!("[osbase] service enablement failed: {reason}");
                phases.push(PhaseResult {
                    name: "services".to_string(),
                    status: PhaseStatus::Failed,
                    message: Some(reason),
                    duration_ms: None,
                });
                return Ok(OsbaseInstallOutcome {
                    domain: request.domain,
                    target: request.target.clone(),
                    phases,
                    exit_code: 1,
                    warnings,
                    hints: vec![],
                });
            }
        }
    }

    // ─── Phase 4: Verify ─────────────────────────────────────────────────
    if !request.skip_verify {
        match runtime.verify(scenario) {
            VerifyOutcome::Passed(msg) => {
                phases.push(PhaseResult {
                    name: "verify".to_string(),
                    status: PhaseStatus::Success,
                    message: Some(msg),
                    duration_ms: None,
                });
            }
            VerifyOutcome::NothingToVerify => {
                phases.push(PhaseResult {
                    name: "verify".to_string(),
                    status: PhaseStatus::Skipped,
                    message: Some("no verify commands defined for this scenario".to_string()),
                    duration_ms: None,
                });
            }
            VerifyOutcome::Failed(reason) => {
                // Verify failure is degraded, not fatal
                eprintln!("[osbase] verify degraded: {reason}");
                warnings.push(format!("verify degraded: {reason}"));
                phases.push(PhaseResult {
                    name: "verify".to_string(),
                    status: PhaseStatus::Degraded,
                    message: Some(reason),
                    duration_ms: None,
                });
            }
        }
    } else {
        eprintln!("[osbase] verify: skipped (--no-verify)");
        phases.push(PhaseResult {
            name: "verify".to_string(),
            status: PhaseStatus::Skipped,
            message: Some("skipped by --no-verify".to_string()),
            duration_ms: None,
        });
    }

    // ─── Phase 5: State ─────────────────────────────────────────────────────
    // Lock is already held (acquired before Phase 2).
    let state_result = (|| -> Result<String, String> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        // The scenario's packages are dnf-managed: record a delegated,
        // managed installation. The record marks existence; dnf owns the
        // package facts.
        let installation = Installation {
            kind: ObjectKind::Osbase,
            name: format!("sandbox-{}", scenario.name),
            scope: InstallationScope::System,
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Unresolved {
                    component_hint: format!("sandbox-{}", scenario.name),
                },
                relation: ManagementRelation::Managed { since: now.clone() },
                last_observed: None,
            },
            status: LifecycleStatus::Installed,
            installed_at: now,
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: vec![],
            health: vec![],
        };
        store.upsert(installation);
        store
            .save(&state_path)
            .map_err(|e| format!("failed to save state: {e}"))?;
        Ok(format!(
            "sandbox-{} recorded in {}",
            scenario.name,
            state_path.display()
        ))
    })();

    match state_result {
        Ok(msg) => {
            eprintln!("[osbase] state: {msg}");
            phases.push(PhaseResult {
                name: "state".to_string(),
                status: PhaseStatus::Success,
                message: Some(msg),
                duration_ms: None,
            });
        }
        Err(reason) => {
            // State persistence failure after packages/services were mutated
            // is a hard error: the machine has changed but we have no record.
            eprintln!("[osbase] state: FAILED: {reason}");
            warnings.push(format!(
                "state persistence failed after packages/services were modified: {reason}"
            ));
            phases.push(PhaseResult {
                name: "state".to_string(),
                status: PhaseStatus::Failed,
                message: Some(reason),
                duration_ms: None,
            });
            return Ok(OsbaseInstallOutcome {
                domain: request.domain,
                target: request.target.clone(),
                phases,
                exit_code: 1,
                warnings,
                hints: vec![],
            });
        }
    }

    eprintln!("[osbase] installed successfully");

    // Optional packages hint — informational only, not a warning.
    let mut hints = Vec::new();
    if !scenario.packages_optional.is_empty() {
        let hint = format!(
            "optional packages available: {}",
            scenario.packages_optional.join(" ")
        );
        eprintln!("[osbase] {hint}");
        hints.push(hint);
    }

    let exit_code = if phases.iter().any(|p| p.status == PhaseStatus::Degraded) {
        2
    } else {
        0
    };

    Ok(OsbaseInstallOutcome {
        domain: request.domain,
        target: request.target.clone(),
        phases,
        exit_code,
        warnings,
        hints,
    })
}

// ===========================================================================
// Phase implementations
// ===========================================================================

/// Preflight: check kernel version and KVM availability.
fn run_preflight(env: &EnvFacts, scenario: &ScenarioConfig, force: bool) -> Result<String, String> {
    let mut checks_passed = Vec::new();

    // Kernel version check
    match scenario.check_kernel(env.kernel.as_deref()) {
        Ok(()) => {
            eprintln!(
                "[osbase] preflight: kernel {} \u{2713}",
                scenario.requires_kernel
            );
            checks_passed.push(format!(
                "kernel {} satisfies {}",
                env.kernel.as_deref().unwrap_or("unknown"),
                scenario.requires_kernel
            ));
        }
        Err(reason) => {
            if force {
                eprintln!(
                    "[osbase] preflight: kernel {} \u{2713} (forced)",
                    scenario.requires_kernel
                );
                checks_passed.push(format!("kernel check FORCED (would fail: {reason})"));
            } else {
                eprintln!(
                    "[osbase] preflight: kernel {} \u{2717}",
                    scenario.requires_kernel
                );
                return Err(reason);
            }
        }
    }

    // KVM check
    if scenario.requires_kvm {
        if std::path::Path::new("/dev/kvm").exists() {
            eprintln!("[osbase] preflight: KVM required \u{2014} checking /dev/kvm... \u{2713}");
            checks_passed.push("/dev/kvm available".to_string());
        } else if force {
            eprintln!(
                "[osbase] preflight: KVM required \u{2014} checking /dev/kvm... \u{2713} (forced)"
            );
            checks_passed.push("/dev/kvm NOT found (forced)".to_string());
        } else {
            eprintln!("[osbase] preflight: KVM required \u{2014} checking /dev/kvm... \u{2717}");
            return Err("KVM not available (required by this scenario)".to_string());
        }
    }

    Ok(checks_passed.join("; "))
}

/// Execute `dnf install -y -q <packages>`.
fn run_dnf_install(packages: &[String], runner: &impl CommandRunner) -> Result<String, String> {
    let mut args = vec!["install", "-y", "-q"];
    args.extend(packages.iter().map(String::as_str));
    let output = runner
        .run("dnf", &args)
        .map_err(|e| format!("failed to execute dnf: {e}"))?;

    if output.code == Some(0) {
        Ok(format!("installed: {}", packages.join(" ")))
    } else {
        let stderr = output.stderr;
        // Print stderr on failure for diagnostics
        let stderr_str = stderr.trim();
        if !stderr_str.is_empty() {
            eprintln!("[osbase] dnf stderr:\n{stderr_str}");
        }
        Err(format!(
            "dnf install failed (exit={}): {}",
            output.code.unwrap_or(-1),
            stderr.lines().take(5).collect::<Vec<_>>().join("\n")
        ))
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::io;

    use anolisa_platform::command::CommandOutput;

    use super::*;
    use crate::state::OperationRecord;

    #[derive(Default)]
    struct CountingRuntime {
        package_calls: Cell<usize>,
        service_calls: Cell<usize>,
        verify_calls: Cell<usize>,
    }

    impl ManifestInstallRuntime for CountingRuntime {
        fn install_packages(&self, packages: &[String]) -> Result<String, String> {
            self.package_calls.set(self.package_calls.get() + 1);
            Ok(format!("installed: {}", packages.join(" ")))
        }

        fn enable_services(&self, services: &[String]) -> Result<String, String> {
            self.service_calls.set(self.service_calls.get() + 1);
            Ok(format!("enabled: {}", services.join(", ")))
        }

        fn verify(&self, _scenario: &ScenarioConfig) -> VerifyOutcome {
            self.verify_calls.set(self.verify_calls.get() + 1);
            VerifyOutcome::NothingToVerify
        }
    }

    fn write_operation_only_user_state(system_layout: &FsLayout, home: &std::path::Path) {
        let user_layout =
            FsLayout::user_with_overrides(home.to_path_buf(), None, None, None, None, None);
        let mut store = StateStore::empty_for_layout(&user_layout);
        store.operations.push(OperationRecord {
            id: "op-1".to_string(),
            command: "install cosh".to_string(),
            status: "started".to_string(),
            started_at: "2026-07-21T00:00:00Z".to_string(),
            finished_at: None,
            parent_operation_id: None,
        });
        store
            .save(&system_layout.state_dir.join("installed.toml"))
            .expect("save mismatched state");
    }

    fn req(domain: OsbaseDomain, target: &str) -> OsbaseInstallRequest {
        OsbaseInstallRequest {
            domain,
            target: target.to_string(),
            register_handler: RegisterHandler::Containerd,
            register_runtimeclass: false,
            config_override: None,
            set_default: false,
            force: false,
            skip_verify: false,
            dry_run: true,
        }
    }

    #[test]
    fn validate_rejects_empty_target() {
        let r = req(OsbaseDomain::Sandbox, "  ");
        assert!(matches!(
            validate_request(&r, &root_env()),
            Err(OsbaseInstallError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn validate_rejects_runtimeclass_without_handler() {
        let mut r = req(OsbaseDomain::Sandbox, "runc");
        r.register_handler = RegisterHandler::None;
        r.register_runtimeclass = true;
        assert!(matches!(
            validate_request(&r, &root_env()),
            Err(OsbaseInstallError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn validate_accepts_minimal_request() {
        assert!(validate_request(&req(OsbaseDomain::Sandbox, "runc"), &root_env()).is_ok());
    }

    #[test]
    fn validate_rejects_non_root_uid() {
        let r = req(OsbaseDomain::Sandbox, "runc");
        let env = test_env(); // uid=1000
        match validate_request(&r, &env) {
            Err(OsbaseInstallError::InvalidRequest { reason }) => {
                assert!(
                    reason.contains("sudo"),
                    "expected hint pointing at sudo, got: {reason}"
                );
            }
            other => panic!("expected InvalidRequest for non-root uid, got {other:?}"),
        }
    }

    #[test]
    fn kernel_domain_is_stub() {
        let r = req(OsbaseDomain::Kernel, "agentic");
        let env = root_env();
        let err = execute_install(&r, &env).expect_err("kernel stub");
        assert!(matches!(err, OsbaseInstallError::InvalidRequest { .. }));
    }

    #[test]
    fn security_domain_is_stub() {
        let r = req(OsbaseDomain::Security, "selinux");
        let env = root_env();
        let err = execute_install(&r, &env).expect_err("security stub");
        assert!(matches!(err, OsbaseInstallError::InvalidRequest { .. }));
    }

    #[test]
    fn unknown_sandbox_scenario_is_invalid_request() {
        let r = req(OsbaseDomain::Sandbox, "nope-not-a-scenario");
        let env = root_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().join("system")));
        let err = sandbox_dispatch_with(
            &r,
            &env,
            &layout,
            &CountingRuntime::default(),
            &builtin_manifest(),
        )
        .expect_err("unknown scenario");
        match err {
            OsbaseInstallError::InvalidRequest { reason } => {
                assert!(reason.contains("nope-not-a-scenario"));
                assert!(reason.contains("available"));
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn known_scenarios_resolve_dry_run() {
        let env = root_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().join("system")));
        let runtime = CountingRuntime::default();
        for s in ["runc", "rund", "firecracker", "gvisor", "landlock"] {
            let r = req(OsbaseDomain::Sandbox, s);
            let outcome = sandbox_dispatch_with(&r, &env, &layout, &runtime, &builtin_manifest())
                .unwrap_or_else(|_| panic!("scenario '{s}' should work"));
            assert_eq!(outcome.exit_code, 0);
            assert_eq!(outcome.target, s);

            // Every dry-run must produce exactly five phases in canonical order.
            let phase_names: Vec<&str> = outcome.phases.iter().map(|p| p.name.as_str()).collect();
            assert_eq!(
                phase_names,
                vec!["preflight", "packages", "services", "verify", "state"],
                "scenario '{s}' should produce exactly five phases in order"
            );
            // All phases must be Skipped in dry-run mode.
            for phase in &outcome.phases {
                assert_eq!(
                    phase.status,
                    PhaseStatus::Skipped,
                    "scenario '{s}' phase '{}' should be Skipped in dry-run, got {:?}",
                    phase.name,
                    phase.status
                );
            }
        }
    }

    #[test]
    fn dry_run_rejects_mismatched_state_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().join("system")));
        write_operation_only_user_state(&layout, &tmp.path().join("home"));
        let runtime = CountingRuntime::default();

        let err = sandbox_dispatch_with(
            &req(OsbaseDomain::Sandbox, "runc"),
            &root_env(),
            &layout,
            &runtime,
            &builtin_manifest(),
        )
        .expect_err("dry-run must validate the executable state scope");

        assert!(matches!(
            err,
            OsbaseInstallError::PhaseFailed { ref phase, .. } if phase == "state"
        ));
        assert_eq!(runtime.package_calls.get(), 0);
        assert_eq!(runtime.service_calls.get(), 0);
        assert_eq!(runtime.verify_calls.get(), 0);
    }

    #[test]
    fn state_scope_is_validated_before_native_side_effects() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().join("system")));
        write_operation_only_user_state(&layout, &tmp.path().join("home"));
        let runtime = CountingRuntime::default();
        let mut request = req(OsbaseDomain::Sandbox, "runc");
        request.dry_run = false;

        let err = sandbox_dispatch_with(
            &request,
            &root_env(),
            &layout,
            &runtime,
            &builtin_manifest(),
        )
        .expect_err("state mismatch must stop the install");

        assert!(matches!(
            err,
            OsbaseInstallError::PhaseFailed { ref phase, .. } if phase == "state"
        ));
        assert_eq!(runtime.package_calls.get(), 0);
        assert_eq!(runtime.service_calls.get(), 0);
        assert_eq!(runtime.verify_calls.get(), 0);
    }

    #[test]
    fn list_scenarios_returns_all() {
        let manifest = builtin_manifest();
        let names = manifest.scenario_names();
        assert!(names.contains(&"runc"));
        assert!(names.contains(&"gvisor"));
        assert!(names.contains(&"landlock"));
    }

    fn builtin_manifest() -> SandboxManifest {
        SandboxManifest::load_with_search_paths(&[]).expect("builtin manifest")
    }

    const OUTPUT_CHILD: &str = "osbase_install::tests::command_output_child";
    const OUTPUT_ENV: &str = "ANOLISA_TEST_OSBASE_COMMAND_OUTPUT";

    #[test]
    fn command_output_child() {
        let expected_args = ["--exact", OUTPUT_CHILD, "--nocapture"];
        if std::env::args().skip(1).collect::<Vec<_>>() != expected_args
            || std::env::var(OUTPUT_ENV).as_deref() != Ok("capture")
        {
            return;
        }

        install_real_runtime_orders_effects_and_persists_degraded_verification();
        let manifest = fixture_manifest();
        let runner = ScriptedRunner::new([
            ScriptedCommand::new(
                "probe-a",
                &["--version"],
                output(Some(0), "v1\nhidden second line", ""),
            ),
            ScriptedCommand::new(
                "dnf",
                &["remove", "-y", "-q", "pkg-a", "pkg-b"],
                output(
                    Some(1),
                    "No match for argument: pkg-a",
                    "  failure detail\n \n",
                ),
            ),
            ScriptedCommand::new(
                "dnf",
                &["remove", "-y", "-q", "pkg-a", "pkg-b"],
                output(Some(0), "", ""),
            ),
        ]);
        run_verify_cmd("probe-a", &["--version"], "probe-a --version", &runner).unwrap();
        let error = execute_uninstall_with("fixture", false, &manifest, &runner).unwrap_err();
        assert_eq!(
            error.to_string(),
            "phase 'uninstall' failed: dnf remove failed (exit=1):   failure detail\n "
        );
        execute_uninstall_with("fixture", false, &manifest, &runner).unwrap();
        execute_uninstall_with("fixture", true, &manifest, &runner).unwrap();
        runner.assert_finished();
    }

    #[test]
    fn command_stderr_preserves_diagnostics_and_order() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", OUTPUT_CHILD, "--nocapture"])
            .env(OUTPUT_ENV, "capture")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8(out.stderr).unwrap();
        let mut rest = stderr.as_str();
        for expected in [
            "[osbase] installing packages: pkg-a pkg-b",
            "[osbase] dnf install completed (exit_code=0)",
            "[osbase] services: svc-a.service active ✓",
            "[osbase] services: svc-b.service active ✓",
            "[osbase] verify: probe-a --version ✓ v1",
            "[osbase] installed successfully",
            "[osbase] dnf install failed",
            "[osbase] service enablement failed: systemctl enable --now svc-b failed: service error",
            "[osbase] verify degraded: probe-a --version failed (exit 1): verify error",
            "[osbase] verify: skipped (--no-verify)",
            "[osbase] verify: probe-a --version ✓ v1",
            "[osbase] dnf stderr:\nfailure detail\n",
            "[osbase] dnf remove failed",
            "[osbase] dnf remove completed (exit_code=0)",
            "[osbase] removed successfully",
            "[osbase] [dry-run] would remove packages: pkg-a pkg-b",
            "[osbase] [dry-run] no packages will be removed in dry-run mode",
        ] {
            let index = rest.find(expected).unwrap_or_else(|| {
                panic!("missing or out-of-order diagnostic: {expected}\n{stderr}")
            });
            rest = &rest[index + expected.len()..];
        }
        assert!(!stderr.contains("hidden second line"));
        assert!(!stderr.contains("No match for argument: pkg-a"));
    }

    #[test]
    fn preset_output_environment_does_not_enter_child_without_exact_args() {
        for value in ["capture", "invalid"] {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([OUTPUT_CHILD, "--nocapture"])
                .env(OUTPUT_ENV, value)
                .output()
                .unwrap();
            assert!(out.status.success());
            assert!(out.stderr.is_empty());
            assert!(String::from_utf8(out.stdout).unwrap().contains("1 passed"));
        }
    }

    struct ScriptedCommand {
        program: &'static str,
        args: Vec<String>,
        result: io::Result<CommandOutput>,
    }

    impl ScriptedCommand {
        fn new(program: &'static str, args: &[&str], result: io::Result<CommandOutput>) -> Self {
            Self {
                program,
                args: args.iter().map(|arg| (*arg).to_string()).collect(),
                result,
            }
        }
    }

    struct ScriptedRunner(RefCell<VecDeque<ScriptedCommand>>);

    impl ScriptedRunner {
        fn new(commands: impl IntoIterator<Item = ScriptedCommand>) -> Self {
            Self(RefCell::new(commands.into_iter().collect()))
        }

        fn assert_finished(&self) {
            assert!(self.0.borrow().is_empty(), "expected commands were not run");
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            let expected = self.0.borrow_mut().pop_front().expect("unexpected command");
            assert_eq!(program, expected.program);
            assert_eq!(args, expected.args);
            expected.result
        }
    }

    fn output(code: Option<i32>, stdout: &str, stderr: &str) -> io::Result<CommandOutput> {
        Ok(CommandOutput {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    fn fixture_manifest() -> SandboxManifest {
        SandboxManifest::parse(
            r#"
            [[scenario]]
            name = "fixture"
            packages = ["pkg-a", "pkg-b"]
            services = ["svc-a", "svc-b"]
            verify_commands = ["  ", "probe-a --version", "probe-b check"]
        "#,
        )
        .expect("fixture manifest")
    }

    #[test]
    fn dnf_results_use_exit_status_and_preserve_diagnostics() {
        for action in ["install", "remove"] {
            let success = if action == "install" {
                "installed: pkg-a pkg-b"
            } else {
                "uninstalled: pkg-a pkg-b"
            };
            let cases = [
                (
                    output(Some(0), "ignored stdout", "ignored stderr"),
                    Ok(success.to_string()),
                ),
                (
                    Err(io::Error::new(io::ErrorKind::NotFound, "missing dnf")),
                    Err("failed to execute dnf: missing dnf".into()),
                ),
                (
                    output(Some(7), "stdout only", ""),
                    Err(format!("dnf {action} failed (exit=7): ")),
                ),
                (
                    output(None, "", " \n first\nsecond\nthird\nfourth\nfifth"),
                    Err(format!(
                        "dnf {action} failed (exit=-1):  \n first\nsecond\nthird\nfourth"
                    )),
                ),
            ];
            for (result, expected) in cases {
                let runner = ScriptedRunner::new([ScriptedCommand::new(
                    "dnf",
                    &[action, "-y", "-q", "pkg-a", "pkg-b"],
                    result,
                )]);
                let packages = vec!["pkg-a".into(), "pkg-b".into()];
                let actual = if action == "install" {
                    run_dnf_install(&packages, &runner)
                } else {
                    run_dnf_remove(&packages, &runner)
                };
                assert_eq!(actual, expected);
                runner.assert_finished();
            }

            let markers = if action == "install" {
                ["Nothing to do", "already installed"]
            } else {
                ["No packages marked for removal", "No match for argument"]
            };
            for marker in markers {
                for code in [Some(0), Some(1), Some(3), Some(100), Some(200), None] {
                    for marker_in_stdout in [false, true] {
                        let (stdout, stderr) = if marker_in_stdout {
                            (marker, "another package failed")
                        } else {
                            ("another package failed", marker)
                        };
                        let runner = ScriptedRunner::new([ScriptedCommand::new(
                            "dnf",
                            &[action, "-y", "-q", "pkg-a", "pkg-b"],
                            output(code, stdout, stderr),
                        )]);
                        let packages = vec!["pkg-a".into(), "pkg-b".into()];
                        let actual = if action == "install" {
                            run_dnf_install(&packages, &runner)
                        } else {
                            run_dnf_remove(&packages, &runner)
                        };
                        if code == Some(0) {
                            assert_eq!(actual.unwrap(), success);
                        } else {
                            assert_eq!(
                                actual.unwrap_err(),
                                format!(
                                    "dnf {action} failed (exit={}): {stderr}",
                                    code.unwrap_or(-1)
                                )
                            );
                        }
                        runner.assert_finished();
                    }
                }
            }
        }
    }

    #[test]
    fn services_fail_fast_with_unchanged_errors() {
        for (result, expected) in [
            (
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
                "failed to run systemctl: denied",
            ),
            (
                output(Some(3), "ignored", "  failed\n "),
                "systemctl enable --now svc-a failed: failed",
            ),
            (
                output(None, "ignored", " \n "),
                "systemctl enable --now svc-a failed: ",
            ),
        ] {
            let runner = ScriptedRunner::new([ScriptedCommand::new(
                "systemctl",
                &["enable", "--now", "svc-a"],
                result,
            )]);
            let runtime = HostManifestInstallRuntime { runner: &runner };
            assert_eq!(
                runtime
                    .enable_services(&["svc-a".into(), "svc-b".into()])
                    .unwrap_err(),
                expected
            );
            runner.assert_finished();
        }
        let runner = ScriptedRunner::new([]);
        assert_eq!(run_enable_services(&[], &runner).unwrap(), "enabled: ");
        runner.assert_finished();
    }

    #[test]
    fn verify_failures_preserve_first_line_and_stop_checks() {
        for (result, expected) in [
            (
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
                "probe-a --version: command not found — is the package installed? (denied)",
            ),
            (
                output(Some(9), "ignored", "  first  \nsecond"),
                "probe-a --version failed (exit 9): first",
            ),
            (
                output(None, "ignored", " \nsecond"),
                "probe-a --version failed (exit -1)",
            ),
            (
                output(Some(1), "stdout only", ""),
                "probe-a --version failed (exit 1)",
            ),
        ] {
            let runner =
                ScriptedRunner::new([ScriptedCommand::new("probe-a", &["--version"], result)]);
            let manifest = fixture_manifest();
            let runtime = HostManifestInstallRuntime { runner: &runner };
            match runtime.verify(&manifest.scenarios[0]) {
                VerifyOutcome::Failed(reason) => assert_eq!(reason, expected),
                _ => panic!("expected failed verification"),
            }
            runner.assert_finished();
        }
    }

    #[test]
    fn verify_fallback_and_empty_checks_keep_existing_semantics() {
        let mut scenario = fixture_manifest().scenarios.remove(0);
        scenario.verify_commands.clear();
        let runner = ScriptedRunner::new([
            ScriptedCommand::new(
                "systemctl",
                &["is-active", "svc-a"],
                output(Some(0), "active\n", ""),
            ),
            ScriptedCommand::new(
                "systemctl",
                &["is-active", "svc-b"],
                output(Some(0), "active\n", ""),
            ),
        ]);
        assert!(
            matches!(run_post_verify(&scenario, &runner), VerifyOutcome::Passed(msg) if msg == "all checks passed: svc-a, svc-b")
        );
        runner.assert_finished();
        let runner = ScriptedRunner::new([ScriptedCommand::new(
            "systemctl",
            &["is-active", "svc-a"],
            output(Some(3), "inactive", ""),
        )]);
        assert!(
            matches!(run_post_verify(&scenario, &runner), VerifyOutcome::Failed(msg) if msg == "svc-a active failed (exit 3)")
        );
        runner.assert_finished();
        scenario.services.clear();
        let runner = ScriptedRunner::new([]);
        assert!(matches!(
            run_post_verify(&scenario, &runner),
            VerifyOutcome::NothingToVerify
        ));
        scenario.verify_commands.push(" \t ".into());
        scenario.services.push("must-not-probe".into());
        assert!(
            matches!(run_post_verify(&scenario, &runner), VerifyOutcome::Passed(msg) if msg == "all checks passed: ")
        );
        runner.assert_finished();
    }

    #[test]
    fn install_real_runtime_orders_effects_and_persists_degraded_verification() {
        for terminal in ["complete", "packages", "services", "verify", "skip-verify"] {
            let tmp = tempfile::tempdir().unwrap();
            let layout = FsLayout::system(Some(tmp.path().join("system")));
            let manifest = fixture_manifest();
            let mut request = req(OsbaseDomain::Sandbox, "fixture");
            request.dry_run = false;
            request.skip_verify = terminal == "skip-verify";
            let mut commands = vec![ScriptedCommand::new(
                "dnf",
                &["install", "-y", "-q", "pkg-a", "pkg-b"],
                output(
                    Some(if terminal == "packages" { 1 } else { 0 }),
                    "",
                    "package error",
                ),
            )];
            if terminal != "packages" {
                commands.push(ScriptedCommand::new(
                    "systemctl",
                    &["enable", "--now", "svc-a"],
                    output(Some(0), "", ""),
                ));
                commands.push(ScriptedCommand::new(
                    "systemctl",
                    &["enable", "--now", "svc-b"],
                    output(
                        Some(if terminal == "services" { 1 } else { 0 }),
                        "",
                        "service error",
                    ),
                ));
                if terminal != "services" && !request.skip_verify {
                    commands.push(ScriptedCommand::new(
                        "probe-a",
                        &["--version"],
                        output(
                            Some(if terminal == "verify" { 1 } else { 0 }),
                            "v1\nsecond",
                            "verify error",
                        ),
                    ));
                    if terminal != "verify" {
                        commands.push(ScriptedCommand::new(
                            "probe-b",
                            &["check"],
                            output(Some(0), "", ""),
                        ));
                    }
                }
            }
            let runner = ScriptedRunner::new(commands);
            let runtime = HostManifestInstallRuntime { runner: &runner };
            let outcome =
                sandbox_dispatch_with(&request, &root_env(), &layout, &runtime, &manifest).unwrap();
            runner.assert_finished();
            assert!(layout.lock_file.exists());
            let state_path = layout.state_dir.join("installed.toml");
            if matches!(terminal, "packages" | "services") {
                assert_eq!(outcome.exit_code, 1);
                assert_eq!(outcome.phases.last().unwrap().name, terminal);
                assert_eq!(outcome.phases.last().unwrap().status, PhaseStatus::Failed);
                assert!(!state_path.exists());
                assert!(outcome.warnings.is_empty());
            } else {
                assert_eq!(outcome.exit_code, if terminal == "verify" { 2 } else { 0 });
                assert_eq!(
                    outcome
                        .phases
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>(),
                    ["preflight", "packages", "services", "verify", "state"]
                );
                let expected = match terminal {
                    "verify" => PhaseStatus::Degraded,
                    "skip-verify" => PhaseStatus::Skipped,
                    _ => PhaseStatus::Success,
                };
                assert_eq!(outcome.phases[3].status, expected);
                assert_eq!(outcome.phases[4].status, PhaseStatus::Success);
                if terminal == "verify" {
                    assert_eq!(
                        outcome.warnings,
                        ["verify degraded: probe-a --version failed (exit 1): verify error"]
                    );
                } else {
                    assert!(outcome.warnings.is_empty());
                }
                let state = std::fs::read_to_string(&state_path).unwrap();
                assert!(state.contains("sandbox-fixture"));
                load_state_for_layout(&layout).expect("persisted state is readable");
            }
        }
    }

    #[test]
    fn install_preview_and_pre_effect_rejections_never_run_commands() {
        for case in ["preview", "unknown", "preflight", "state"] {
            let tmp = tempfile::tempdir().unwrap();
            let layout = FsLayout::system(Some(tmp.path().join("system")));
            let runner = ScriptedRunner::new([]);
            let runtime = HostManifestInstallRuntime { runner: &runner };
            let mut manifest = fixture_manifest();
            let mut request = req(
                OsbaseDomain::Sandbox,
                if case == "unknown" {
                    "missing"
                } else {
                    "fixture"
                },
            );
            request.dry_run = case == "preview";
            if case == "preflight" {
                manifest.scenarios[0].requires_kernel = ">=999.0".into();
            }
            if case == "state" {
                write_operation_only_user_state(&layout, &tmp.path().join("home"));
            }
            let state_path = layout.state_dir.join("installed.toml");
            let before = std::fs::read(&state_path).ok();
            let result = sandbox_dispatch_with(&request, &root_env(), &layout, &runtime, &manifest);
            match case {
                "preview" => assert_eq!(result.unwrap().exit_code, 0),
                "preflight" => assert_eq!(result.unwrap().phases[0].status, PhaseStatus::Failed),
                "unknown" => assert!(matches!(
                    result,
                    Err(OsbaseInstallError::InvalidRequest { .. })
                )),
                "state" => assert!(
                    matches!(result, Err(OsbaseInstallError::PhaseFailed { phase, .. }) if phase == "state")
                ),
                _ => unreachable!(),
            }
            assert_eq!(layout.lock_file.exists(), case == "state");
            assert_eq!(std::fs::read(state_path).ok(), before);
            runner.assert_finished();
        }
    }

    #[test]
    fn install_empty_phases_do_not_run_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = FsLayout::system(Some(tmp.path().join("system")));
        let mut manifest = fixture_manifest();
        let scenario = &mut manifest.scenarios[0];
        scenario.packages.clear();
        scenario.services.clear();
        scenario.verify_commands.clear();
        let runner = ScriptedRunner::new([]);
        let mut request = req(OsbaseDomain::Sandbox, "fixture");
        request.dry_run = false;
        let outcome = sandbox_dispatch_with(
            &request,
            &root_env(),
            &layout,
            &HostManifestInstallRuntime { runner: &runner },
            &manifest,
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 0);
        for phase in &outcome.phases[1..4] {
            assert_eq!(phase.status, PhaseStatus::Skipped);
        }
        assert!(layout.state_dir.join("installed.toml").exists());
        runner.assert_finished();
    }

    #[test]
    fn dnf_failure_markers_cannot_advance_install_or_succeed_uninstall() {
        for code in [Some(1), Some(3), Some(200), None] {
            let tmp = tempfile::tempdir().unwrap();
            let layout = FsLayout::system(Some(tmp.path().join("system")));
            let manifest = fixture_manifest();
            let mut request = req(OsbaseDomain::Sandbox, "fixture");
            request.dry_run = false;
            let runner = ScriptedRunner::new([ScriptedCommand::new(
                "dnf",
                &["install", "-y", "-q", "pkg-a", "pkg-b"],
                output(
                    code,
                    "Package pkg-a is already installed.\nNothing to do.",
                    "pkg-b failed",
                ),
            )]);
            let outcome = sandbox_dispatch_with(
                &request,
                &root_env(),
                &layout,
                &HostManifestInstallRuntime { runner: &runner },
                &manifest,
            )
            .unwrap();
            assert_eq!(outcome.exit_code, 1);
            assert_eq!(
                outcome
                    .phases
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>(),
                ["preflight", "packages"]
            );
            assert_eq!(outcome.phases[1].status, PhaseStatus::Failed);
            assert_eq!(
                outcome.phases[1].message,
                Some(format!(
                    "dnf install failed (exit={}): pkg-b failed",
                    code.unwrap_or(-1)
                ))
            );
            assert!(!layout.state_dir.join("installed.toml").exists());
            assert!(outcome.warnings.is_empty());
            runner.assert_finished();

            let runner = ScriptedRunner::new([ScriptedCommand::new(
                "dnf",
                &["remove", "-y", "-q", "pkg-a", "pkg-b"],
                output(
                    code,
                    "No match for argument: pkg-a\nNo packages marked for removal.",
                    "pkg-b failed",
                ),
            )]);
            let err = execute_uninstall_with("fixture", false, &manifest, &runner).unwrap_err();
            assert!(
                matches!(err, OsbaseInstallError::PhaseFailed { phase, message } if phase == "uninstall" && message == format!("dnf remove failed (exit={}): pkg-b failed", code.unwrap_or(-1)))
            );
            runner.assert_finished();
        }
    }

    #[test]
    fn uninstall_uses_injected_runner_only_for_nonempty_apply() {
        let manifest = fixture_manifest();
        let runner = ScriptedRunner::new([]);
        assert_eq!(
            execute_uninstall_with("fixture", true, &manifest, &runner).unwrap(),
            "dry-run: would uninstall: pkg-a pkg-b"
        );
        assert!(
            matches!(execute_uninstall_with("missing", false, &manifest, &runner), Err(OsbaseInstallError::InvalidRequest { reason }) if reason == "unknown sandbox scenario 'missing'; available: [fixture]")
        );
        let mut empty = manifest.clone();
        empty.scenarios[0].packages.clear();
        for dry_run in [false, true] {
            assert_eq!(
                execute_uninstall_with("fixture", dry_run, &empty, &runner).unwrap(),
                "scenario 'fixture': nothing to uninstall (no packages defined)"
            );
        }
        runner.assert_finished();
        for result in [
            output(Some(0), "", ""),
            output(Some(5), "", "remove failed"),
            output(None, "", ""),
            Err(io::Error::new(io::ErrorKind::NotFound, "missing dnf")),
        ] {
            let expected = match &result {
                Ok(out) if out.code == Some(0) => None,
                Ok(out) => Some(format!(
                    "dnf remove failed (exit={}): {}",
                    out.code.unwrap_or(-1),
                    out.stderr
                )),
                Err(_) => Some("failed to execute dnf: missing dnf".into()),
            };
            let runner = ScriptedRunner::new([ScriptedCommand::new(
                "dnf",
                &["remove", "-y", "-q", "pkg-a", "pkg-b"],
                result,
            )]);
            let actual = execute_uninstall_with("fixture", false, &manifest, &runner);
            match expected {
                None => assert_eq!(actual.unwrap(), "uninstalled: pkg-a pkg-b"),
                Some(expected) => assert!(
                    matches!(actual, Err(OsbaseInstallError::PhaseFailed { phase, message }) if phase == "uninstall" && message == expected)
                ),
            }
            runner.assert_finished();
        }
    }

    fn test_env() -> EnvFacts {
        EnvFacts {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            libc: None,
            kernel: Some("6.6.30".to_string()),
            pkg_base: None,
            os_id: Some("alinux".to_string()),
            os_id_like: None,
            os_version: Some("4".to_string()),
            btf: None,
            cap_bpf: None,
            container: None,
            user: "tester".to_string(),
            uid: 1000,
            home: std::path::PathBuf::from("/home/tester"),
        }
    }

    fn root_env() -> EnvFacts {
        EnvFacts {
            uid: 0,
            user: "root".to_string(),
            ..test_env()
        }
    }
}
