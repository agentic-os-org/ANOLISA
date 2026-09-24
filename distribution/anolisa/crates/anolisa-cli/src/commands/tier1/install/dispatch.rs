//! Planning and effect adapters used by the single-component install application service.
//!
//! The handler resolves the requested component to a [`ProviderTarget`],
//! assembles host facts, asks the planner for a step sequence (decision
//! table I1–I11), and hands it to the matching executor: an owned plan runs
//! through [`RawInstallOps`], a delegated plan re-uses the native-transaction
//! executor with a [`StoreRecordSink`]. No lifecycle policy lives here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anolisa_core::component_snapshot::{
    ComponentSnapshot, ComponentSnapshotRequest, ProbeEvidence, SnapshotProbe, StateSnapshot,
};
use anolisa_core::execution::{
    CommandOutcome, CommandOutcomeStatus, ExecutionIntent, PreparedExecution,
};
use anolisa_core::executor::{DelegatedExecutionTarget, execute_delegated_steps};
use anolisa_core::facts::{
    FactsError, JournalEvidence, assemble_component_snapshot, lifecycle_facts_from_snapshot,
};
use anolisa_core::lock::InstallLock;
use anolisa_core::owned_executor::{OwnedExecutionError, execute_owned_steps};
use anolisa_core::planner::{
    HookKind, InstallRequest, Intent, NativeProbe, Plan, PlanError, ProviderTarget, Step, plan,
};
use anolisa_core::providers::{DelegatedProvider, ProviderError};
use anolisa_core::record_sink::{DelegatedIdentity, RecordContext, StoreRecordSink};
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::PackageTransaction;
use anolisa_platform::privilege;
use anolisa_platform::rpm_query::RpmPackageQuery;
use anolisa_platform::rpm_repo::RpmRepoSource;
use anolisa_platform::rpm_transaction::RpmTransaction;
use chrono::{SecondsFormat, Utc};

use anolisa_core::domain::{InstallationScope, NativePm, ProviderBinding};

use crate::commands::common;
use crate::commands::common::RepoPersistPolicy;
use crate::commands::tier1::recovery::LockedJournalGate;
use crate::commands::tier1::rpm_install;
use crate::context::{CliContext, InstallMode};
use crate::progress::{self, ProgressReporter};
use crate::repo_config::{
    BackendConfig, HostVars, RepoConfig, RepoConfigError, normalize_override_url,
};
use crate::resolution::{
    BackendKind, ComponentIndex, ComponentResolver, ResolutionSet, ResolveOptions,
    load_component_index_from_base, load_optional_component_index,
};
use crate::response::CliError;

use super::application::{
    ApplicationFailure, InstallApplicationOutcome, InstallChange, InstallSubject,
};
use super::owned_ops::{
    RawEffectFactories, RawInstallOps, ValidatedInstall, installed_version_label,
    validate_component_conflict, validate_owned_install,
};
use super::raw::{load_dry_run_install_contract, resolve_raw};
use super::render::repo_config_err;
use super::rpm::{
    PinError, RpmTarget, check_rpm_install, resolve_pinned_candidate,
    rpm_package_candidates_with_index,
};
use super::types::{RawRepositoryOrigin, RawResolution, ResolveInputs};
use super::{ANOLISA_RPM_REPO_ID, COMMAND, InstallArgs};

/// Real host backends for one component invocation.
///
/// The backends receive the repo.toml RPM source when the rpm family will
/// execute, so availability probes and install transactions do not silently
/// fall back to the host's enabled system repos.
pub(crate) fn host_backends(
    component: &str,
    args: &InstallArgs,
    ctx: &CliContext,
) -> Result<(RpmPackageQuery, RpmTransaction), CliError> {
    let command = format!("{COMMAND} {component}");
    let layout = common::resolve_layout(ctx);
    let env = anolisa_env::EnvService::detect();
    let repo_config = common::load_repo_config(ctx, &layout, COMMAND, RepoPersistPolicy::Require)?;
    let index_base_override = normalized_repo_override(args)?;
    let (resolved, view) =
        common::resolve_install_target(component, ctx, &command, index_base_override.as_deref())?;
    let store = &view.writable.state;

    let rpm_repo = if install_family(args, store, &resolved, &repo_config) == "rpm" {
        rpm_repo_source_for_invocation(&repo_config, &env, index_base_override.as_deref())?
    } else {
        None
    };
    Ok(crate::commands::tier1::rpm_backends::system(rpm_repo))
}

/// Validated `--repo` base URL, when the caller supplied one. Normalization
/// runs before identity resolution so a malformed override is refused as a
/// bad argument rather than surfacing as an unavailable component index.
pub(crate) fn normalized_repo_override(args: &InstallArgs) -> Result<Option<String>, CliError> {
    args.repo
        .as_deref()
        .map(|url| normalize_override_url(url).map_err(|err| repo_config_err(err, true)))
        .transpose()
}

/// Component index for this invocation, best-effort.
///
/// A `--repo` override's published index answers every question the install
/// asks — identity, package selection, and the batch enumeration — so the
/// invocation never mixes the override repository with the repo.toml chain.
/// Without an override the repo.toml raw backend chain is used as usual.
pub(crate) fn invocation_component_index(
    layout: &FsLayout,
    env: &anolisa_env::EnvFacts,
    repo_config: &RepoConfig,
    index_base_override: Option<&str>,
) -> Option<ComponentIndex> {
    match index_base_override {
        Some(base_url) => load_component_index_from_base(layout, base_url).ok(),
        None => load_optional_component_index(layout, env, repo_config),
    }
}

/// Provider family for this invocation: explicit `--backend` (canonical
/// spelling) wins, then the recorded provenance of an existing installation
/// (owned → raw, delegated → rpm), then repo.toml `default_backend`.
///
/// System-RPM presence deliberately plays no part here: it is a host fact
/// the planner rules on (I3), not a routing input.
fn install_family(
    args: &InstallArgs,
    store: &StateStore,
    component: &str,
    repo_config: &RepoConfig,
) -> String {
    if let Some(explicit) = args.backend.as_deref() {
        return RepoConfig::canonical_backend_name(explicit).to_string();
    }
    if let Some(installation) = store.find(ObjectKind::Component, component) {
        return match installation.binding {
            ProviderBinding::Owned { .. } => "raw".to_string(),
            ProviderBinding::Delegated { .. } => "rpm".to_string(),
        };
    }
    repo_config.default_backend.clone()
}

/// What the planning prefix decided for one component, before any side
/// effect ran: the resolved identity, the provider family, and the planned
/// route. The single-component path executes it directly; batch
/// orchestration classifies on the route to group fresh delegated installs
/// into one merged native transaction.
pub(crate) struct PlannedComponent {
    pub(crate) command: String,
    pub(crate) component: String,
    pub(crate) family: String,
    pub(crate) native_package: Option<String>,
    /// Resolved candidate for a `--version`-pinned delegated install, carried
    /// so the dry-run preview and JSON envelope report the real artifact.
    /// `None` for unpinned installs and every owned install.
    pub(crate) delegated_pin: Option<DelegatedPin>,
    pub(crate) scope: InstallationScope,
    pub(crate) now: String,
    pub(crate) store: StateStore,
    pub(crate) request: InstallRequest,
    /// Exact planner output consumed by [`ExecutionIntent`].
    pub(crate) plan: Plan,
    pub(crate) route: PlannedRoute,
}

/// Resolved metadata for a version-pinned delegated install, surfaced to the
/// dry-run preview and the JSON envelope. Built from the repository candidate
/// the pin selected, never from the raw `--version` argument alone.
#[derive(Debug, Clone)]
pub(crate) struct DelegatedPin {
    /// The `--version` value the caller requested.
    pub(crate) requested_version: String,
    /// Upstream VERSION field of the selected candidate (equals
    /// `requested_version`, restated for an unambiguous JSON contract).
    pub(crate) resolved_version: String,
    /// Full resolved EVR of the selected candidate.
    pub(crate) resolved_evr: String,
    /// Architecture of the selected candidate, checked against the freshly
    /// installed package before the record commits.
    pub(crate) resolved_arch: String,
    /// Exact NEVRA handed to the native transaction.
    pub(crate) artifact: String,
    /// Source repository the candidate came from, when reported.
    pub(crate) source_repo: Option<String>,
}

/// Resolved metadata for a version-pinned owned (raw) install, the owned
/// analog of [`DelegatedPin`]. Built from the distribution entry the exact
/// version query selected — the raw resolver only returns an entry whose
/// `version` equals the request, and the contract validator later refuses an
/// artifact whose embedded manifest disagrees with that entry, so these
/// fields prove what will actually be placed.
#[derive(Debug, Clone)]
pub(crate) struct RawPin {
    /// The `--version` value the caller requested.
    requested_version: String,
    /// Version of the resolved distribution entry (equals
    /// `requested_version`, restated for an unambiguous JSON contract).
    resolved_version: String,
    /// Exact artifact URL the pinned entry downloads from — the raw analog
    /// of the delegated pin's NEVRA.
    artifact: String,
    /// Repository base URL the distribution index was fetched from.
    source_repo: String,
}

impl RawPin {
    /// Capture the pin evidence from a settled raw resolution.
    ///
    /// Ordering is enforced by ownership, not convention:
    /// [`validate_owned_install`] takes the [`RawResolution`] by value, so
    /// the borrow here cannot compile after validation has consumed it. Any
    /// future reshuffle that moves the resolution earlier surfaces as a
    /// borrow error at this call site rather than as silently missing pin
    /// evidence.
    fn from_resolution(requested: &str, resolution: &RawResolution) -> Self {
        Self {
            requested_version: requested.to_string(),
            resolved_version: resolution.entry.version.clone(),
            artifact: resolution.artifact_url.clone(),
            source_repo: resolution.base_url.clone(),
        }
    }
}

/// Which executor family the plan routed to, or the idempotent NoOp.
pub(crate) enum PlannedRoute {
    /// I4/I8: the record already covers the request; nothing to execute.
    AlreadyInstalled { version: Option<String> },
    /// Delegated step family (I2 for a fresh install): one native
    /// transaction, a fresh observation, and a record commit.
    Delegated { steps: Vec<Step> },
    /// Owned step family; raw artifact resolution (network) is deferred to
    /// execution.
    Owned { steps: Vec<Step> },
}

impl PlannedRoute {
    /// The planned steps, empty for the NoOp route.
    pub(crate) fn steps(&self) -> &[Step] {
        match self {
            Self::AlreadyInstalled { .. } => &[],
            Self::Delegated { steps } | Self::Owned { steps } => steps,
        }
    }
}

#[expect(clippy::too_many_arguments)]
fn observe_install_snapshot(
    component: &str,
    scope: InstallationScope,
    native_package: Option<&str>,
    observed_at: &str,
    store: &StateStore,
    provider: Option<&DelegatedProvider<'_>>,
    layout: &FsLayout,
    journal_dir: &Path,
) -> Result<ComponentSnapshot, FactsError> {
    let mut probes = vec![SnapshotProbe::State, SnapshotProbe::PendingJournal];
    if native_package.is_some() {
        probes.push(SnapshotProbe::NativePackage);
    }
    assemble_component_snapshot(
        ComponentSnapshotRequest::new(component, scope, probes),
        native_package,
        observed_at,
        store,
        provider,
        layout,
        journal_dir,
    )
}

fn observe_install_state(
    component: &str,
    scope: InstallationScope,
    observed_at: &str,
    store: &StateStore,
    layout: &FsLayout,
    journal_dir: &Path,
) -> Result<ComponentSnapshot, FactsError> {
    assemble_component_snapshot(
        ComponentSnapshotRequest::new(component, scope, [SnapshotProbe::State]),
        None,
        observed_at,
        store,
        None,
        layout,
        journal_dir,
    )
}

/// Planning prefix of an install: resolve the component and its provider
/// target, assemble host facts, and ask the planner for the step sequence.
/// Read-only against the host — every side effect belongs to
/// [`execute_planned`].
pub(crate) fn plan_component(
    input: &str,
    args: &InstallArgs,
    ctx: &CliContext,
    env: &anolisa_env::EnvFacts,
    rpmdb: &RpmdbProbe,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
) -> Result<PlannedComponent, CliError> {
    let command = format!("{COMMAND} {input}");
    let layout = common::resolve_layout(ctx);
    let journal_dir = rpm_install::journal_dir(&layout);
    let uid = privilege::effective_uid();
    let scope = match ctx.install_mode {
        InstallMode::System => InstallationScope::System,
        InstallMode::User => InstallationScope::User { uid },
    };
    let now = now_iso8601();
    let repo_config = common::load_repo_config(ctx, &layout, COMMAND, RepoPersistPolicy::Require)?;

    // Resolve identity across all visible roots, but bind install planning to
    // the writable scope only. A user install may therefore shadow an
    // existing system installation without mutating or inheriting it. A
    // `--repo` override makes that repository's index the identity authority.
    let index_base_override = normalized_repo_override(args)?;
    let (component, view) =
        common::resolve_install_target(input, ctx, &command, index_base_override.as_deref())?;
    let store = view.writable.state;

    if let Some(explicit) = args.backend.as_deref()
        && let Some(warning) = RepoConfig::backend_name_deprecation_warning(explicit)
    {
        progress::suspend_output(|| eprintln!("warning: {warning}"));
    }

    let family = install_family(args, &store, &component, &repo_config);

    // Backend gate: only the raw and rpm families have executors. The
    // selection call validates the name and its configuration first, so an
    // unconfigured or unknown backend stays INVALID_ARGUMENT. Over an
    // existing record the provenance conflict outranks the missing executor:
    // the request would be refused even if the backend could install.
    if family != "raw" && family != "rpm" {
        let (backend_name, _) = repo_config
            .select_backend(Some(family.as_str()))
            .map_err(|err| repo_config_err(err, true))?;
        if let Some(installation) = store.find(ObjectKind::Component, &component) {
            let installed_backend = match installation.binding {
                ProviderBinding::Owned { .. } => "raw",
                ProviderBinding::Delegated { .. } => "rpm",
            };
            return Err(CliError::InvalidArgument {
                command,
                reason: format!(
                    "component '{component}' is already installed via backend '{installed_backend}'; reinstalling it via backend '{backend_name}' is not allowed — uninstall it first or use backend '{installed_backend}'"
                ),
            });
        }
        return Err(CliError::not_implemented_with_hint(
            format!("install --backend {backend_name}"),
            format!(
                "the '{backend_name}' backend is configured but its executor is not implemented yet — only 'raw' and 'rpm' can install today",
            ),
        ));
    }

    // State and journal decide the early refusal order before provider or
    // repository resolution. The second snapshot below adds native evidence.
    let initial_snapshot =
        observe_install_state(&component, scope, &now, &store, &layout, &journal_dir).map_err(
            |err| CliError::Runtime {
                command: command.clone(),
                reason: err.to_string(),
            },
        )?;
    let active_binding = match initial_snapshot.state() {
        ProbeEvidence::Absent { .. } => None,
        ProbeEvidence::Present {
            value: StateSnapshot::Active(installation),
            ..
        } => Some(installation.binding.clone()),
        ProbeEvidence::Present {
            value: StateSnapshot::Quarantined(_),
            ..
        } => {
            return Err(plan_error_to_cli(
                PlanError::NeedsAttention,
                &component,
                &command,
            ));
        }
        ProbeEvidence::Unavailable { reason, .. } => {
            return Err(CliError::Runtime {
                command,
                reason: reason.clone(),
            });
        }
        ProbeEvidence::NotRequested => {
            unreachable!("install snapshot always requests state evidence")
        }
    };
    let journal_snapshot = observe_install_snapshot(
        &component,
        scope,
        None,
        &now,
        &store,
        None,
        &layout,
        &journal_dir,
    )
    .map_err(|err| CliError::Runtime {
        command: command.clone(),
        reason: err.to_string(),
    })?;
    match journal_snapshot.pending_journal() {
        ProbeEvidence::Present { .. } => {
            return Err(plan_error_to_cli(
                PlanError::PendingOperation,
                &component,
                &command,
            ));
        }
        ProbeEvidence::Absent { .. } => {}
        ProbeEvidence::Unavailable { reason, .. } => {
            return Err(CliError::Runtime {
                command,
                reason: reason.clone(),
            });
        }
        ProbeEvidence::NotRequested => {
            unreachable!("install snapshot always requests pending journal evidence")
        }
    }

    // Resolve the provider target and the native probe package. Nothing here
    // touches the raw repository: an active record's arms all end in a NoOp
    // or a refusal (I4–I9, I11), and a fresh owned target's plan does not
    // depend on the resolved version — only the probe answer (I3) matters,
    // and that answer must not depend on the repo being reachable.
    let mut delegated_pin: Option<DelegatedPin> = None;
    let (target, native_package): (ProviderTarget, Option<String>) = match &active_binding {
        Some(binding) => target_for_active_record(binding, &family, args, &component),
        None if family == "raw" => {
            let native_package = match scope {
                InstallationScope::System => Some(system_probe_package(
                    args,
                    &layout,
                    env,
                    rpmdb,
                    &repo_config,
                    &component,
                    query,
                    &command,
                )?),
                InstallationScope::User { .. } => None,
            };
            (
                ProviderTarget::Owned {
                    version: args.version.clone().unwrap_or_default(),
                },
                native_package,
            )
        }
        None => {
            // Delegated targets need system scope; in user scope the planner
            // refuses (its first guard), so nothing touches the rpmdb here.
            if !matches!(scope, InstallationScope::System) {
                let package = args.package.clone().unwrap_or_else(|| component.clone());
                (
                    ProviderTarget::Delegated {
                        pm: NativePm::Rpm,
                        package,
                        artifact: None,
                    },
                    None,
                )
            } else {
                let fresh = resolve_fresh_delegated(
                    args,
                    &layout,
                    env,
                    &repo_config,
                    &component,
                    query,
                    &command,
                )?;
                delegated_pin = fresh.pin;
                (fresh.target, Some(fresh.package))
            }
        }
    };

    let provider = DelegatedProvider::new(query, txn);
    // A missing rpm/dnf binary is a hard error whenever a probe was needed:
    // without it the host cannot prove the component is not an unobserved
    // system RPM, and a raw install over one could corrupt it (I3). The one
    // exception is the raw family on a host whose package authority is not
    // RPM — there is no rpmdb to protect, so the facts degrade to NotProbed.
    // The package identity is kept: the executor's locked recheck re-probes
    // it and applies the same CommandMissing policy, so tooling (and an RPM)
    // appearing between planning and placement is still caught.
    let snapshot = match observe_install_snapshot(
        &component,
        scope,
        native_package.as_deref(),
        &now,
        &store,
        Some(&provider),
        &layout,
        &journal_dir,
    ) {
        Ok(facts) => facts,
        Err(FactsError::Probe(ProviderError::Query(PackageQueryError::CommandMissing {
            command: bin,
        }))) => {
            if family != "raw" || missing_rpm_tooling_is_fatal(env, rpmdb) {
                return Err(rpm_tooling_missing_error(&command, &bin, &component));
            }
            observe_install_snapshot(
                &component,
                scope,
                None,
                &now,
                &store,
                None,
                &layout,
                &journal_dir,
            )
            .map_err(|err| CliError::Runtime {
                command: command.clone(),
                reason: err.to_string(),
            })?
        }
        Err(err) => {
            return Err(CliError::Runtime {
                command: command.clone(),
                reason: err.to_string(),
            });
        }
    };
    let active_adapter_claims = store
        .adapter_claims
        .iter()
        .filter(|claim| claim.component == component)
        .map(|claim| claim.framework.clone())
        .collect();
    let facts =
        lifecycle_facts_from_snapshot(&snapshot, active_adapter_claims, None).map_err(|err| {
            CliError::Runtime {
                command: command.clone(),
                reason: err.to_string(),
            }
        })?;

    let request = InstallRequest {
        target,
        requested_version: args.version.clone(),
    };
    let install_plan = plan(&Intent::Install(request.clone()), &facts)
        .map_err(|err| plan_error_to_cli(err, &component, &command))?;
    let route = match &install_plan {
        Plan::Execute { steps, .. } => {
            // Route by step family: a delegated plan requests one native
            // transaction, an owned plan places the resolved artifact through
            // the raw backend.
            let is_delegated_plan = steps.iter().all(|step| {
                matches!(
                    step,
                    Step::NativeTransaction { .. }
                        | Step::Observe { .. }
                        | Step::WriteRecord(_)
                        | Step::DropRecord
                )
            });
            if is_delegated_plan {
                PlannedRoute::Delegated {
                    steps: steps.clone(),
                }
            } else {
                PlannedRoute::Owned {
                    steps: steps.clone(),
                }
            }
        }
        Plan::NoOp { .. } => {
            // I4/I8: install is idempotent over a healthy record.
            let version = store
                .find(ObjectKind::Component, &component)
                .map(installed_version_label);
            PlannedRoute::AlreadyInstalled { version }
        }
    };

    if let ProviderTarget::Delegated {
        package, artifact, ..
    } = &request.target
        && matches!(route, PlannedRoute::Delegated { .. })
    {
        // Refuse an unconfigured backend before the DNF preflight: the check
        // must not query host repositories for a source that cannot exist,
        // and the refusal must read as INVALID_ARGUMENT (not a preflight
        // EXECUTION_FAILED) in both apply and dry-run modes.
        require_configured_rpm_backend(&repo_config, index_base_override.as_deref(), &command)?;
        check_rpm_install(
            &provider,
            &[artifact.as_deref().unwrap_or(package)],
            &command,
        )?;
    }

    Ok(PlannedComponent {
        command,
        component,
        family,
        native_package,
        delegated_pin,
        scope,
        now,
        store,
        request,
        plan: install_plan,
        route,
    })
}

/// Execute prepared single-component work and return a typed application result.
#[expect(clippy::too_many_arguments)]
pub(super) fn execute_planned(
    planned: PlannedComponent,
    prepared: PreparedExecution,
    args: &InstallArgs,
    ctx: &CliContext,
    env: &anolisa_env::EnvFacts,
    rpmdb: &RpmdbProbe,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
    planned_components: &HashSet<String>,
    reporter: &mut dyn ProgressReporter,
    effects: RawEffectFactories<'_>,
) -> Result<InstallApplicationOutcome, ApplicationFailure> {
    let PlannedComponent {
        command,
        mut component,
        family,
        native_package,
        delegated_pin,
        scope,
        now,
        store,
        request,
        plan: _,
        route,
    } = planned;
    let layout = common::resolve_layout(ctx);
    let repo_config = common::load_repo_config(ctx, &layout, COMMAND, RepoPersistPolicy::Require)?;
    let index_base_override = normalized_repo_override(args)?;
    let state_path = layout.state_dir.join("installed.toml");
    let journal_dir = rpm_install::journal_dir(&layout);

    // Only a settled owned plan resolves the raw artifact (network) — every
    // planning refusal is independent of the raw repo being reachable.
    let (steps, preview, resolution) = match (route, prepared) {
        (PlannedRoute::AlreadyInstalled { version, .. }, PreparedExecution::NoOp { reason }) => {
            reporter.finish();
            return Ok(InstallApplicationOutcome::NoOp {
                subject: InstallSubject {
                    component,
                    package: native_package,
                    version,
                    backend: family,
                    requested_version: None,
                    resolved_version: None,
                    source_repo: None,
                    artifact: None,
                },
                reason,
            });
        }
        (PlannedRoute::Delegated { .. }, PreparedExecution::Preview { steps, .. }) => {
            (steps, true, None)
        }
        (PlannedRoute::Delegated { .. }, PreparedExecution::Apply { steps, .. }) => {
            (steps, false, None)
        }
        (PlannedRoute::Delegated { .. }, PreparedExecution::NoOp { .. }) => {
            unreachable!("a delegated execution route cannot prepare as no-op")
        }
        (PlannedRoute::Owned { .. }, prepared) => {
            let (steps, preview) = match prepared {
                PreparedExecution::Preview { steps, .. } => (steps, true),
                PreparedExecution::Apply { steps, .. } => (steps, false),
                PreparedExecution::NoOp { .. } => {
                    unreachable!("an owned route cannot prepare as no-op")
                }
            };
            reporter.report(&format!("Resolving {component}..."));
            let resolution = resolve_owned_artifact(
                args,
                ctx,
                &layout,
                env,
                &repo_config,
                component.clone(),
                &command,
            )?;
            component = resolution.component.clone();
            (steps, preview, Some(resolution))
        }
        (PlannedRoute::AlreadyInstalled { .. }, _) => {
            unreachable!("a no-op route cannot prepare executable steps")
        }
    };

    // Pin evidence for an owned `--version` install, captured while the
    // resolution is still whole (contract validation consumes it below).
    let raw_pin = match (args.version.as_deref(), resolution.as_ref()) {
        (Some(requested), Some(resolution)) => Some(RawPin::from_resolution(requested, resolution)),
        _ => None,
    };

    if preview {
        reporter.finish();
        let mut warnings: Vec<String> = resolution
            .iter()
            .flat_map(|resolution| resolution.warnings.iter().cloned())
            .collect();
        if let Some(resolution) = resolution.as_ref() {
            match load_dry_run_install_contract(ctx, &layout, resolution)? {
                Some(contract) => {
                    validate_component_conflict(
                        &contract.manifest,
                        &store,
                        planned_components,
                        &command,
                    )?;
                }
                None => warnings.push(format!(
                    "dry-run could not validate component conflicts for '{}' because the repository has no lightweight meta.toml; the full artifact was not downloaded",
                    resolution.component
                )),
            }
        }
        // A pinned dry-run reports the version it resolved against the
        // repository, not the raw `--version` echo — the pin fields carry
        // the exact candidate so the preview proves what would be installed.
        let base_version = resolution
            .as_ref()
            .map(|r| r.entry.version.clone())
            .or_else(|| args.version.clone());
        let mut subject = InstallSubject {
            component,
            package: native_package,
            version: base_version,
            backend: family,
            requested_version: None,
            resolved_version: None,
            source_repo: None,
            artifact: None,
        };
        if let Some(pin) = &delegated_pin {
            apply_delegated_pin(&mut subject, pin);
        }
        if let Some(pin) = &raw_pin {
            apply_raw_pin(&mut subject, pin);
        }
        return Ok(InstallApplicationOutcome::Preview {
            subject,
            steps,
            warnings,
        });
    }

    if let Some(resolution) = resolution {
        reporter.report(&format!("Downloading and verifying {component}..."));
        // Download, digest check, and contract validation run before the
        // lock: they are side-effect free outside the download cache, and a
        // contract refusal (mode mismatch, component conflict, malformed
        // hooks) is an argument error, not a failed transaction.
        let validated = validate_owned_install(ctx, &layout, &store, resolution, &command)?;
        reporter.report(&format!("Installing {component}..."));
        let provider = DelegatedProvider::new(query, txn);
        return install_applied(
            &component,
            ctx,
            &layout,
            &state_path,
            &journal_dir,
            scope,
            &now,
            &request,
            &provider,
            &command,
            reporter,
            InstallApply::Owned {
                validated: Box::new(validated),
                raw_pin: raw_pin.as_ref(),
                native_package: native_package.as_deref(),
                degraded_rpmdb: (!missing_rpm_tooling_is_fatal(env, rpmdb)).then_some(rpmdb),
            },
            effects,
        );
    }

    reporter.report(&format!("Installing {component}..."));
    let provider = DelegatedProvider::new(query, txn);
    let package = native_package.unwrap_or_else(|| component.clone());
    install_applied(
        &component,
        ctx,
        &layout,
        &state_path,
        &journal_dir,
        scope,
        &now,
        &request,
        &provider,
        &command,
        reporter,
        InstallApply::Delegated {
            package: &package,
            delegated_pin: delegated_pin.as_ref(),
            repo_config: &repo_config,
            index_base_override: index_base_override.as_deref(),
            is_root,
        },
        effects,
    )
}

fn apply_delegated_pin(subject: &mut InstallSubject, pin: &DelegatedPin) {
    subject.requested_version = Some(pin.requested_version.clone());
    subject.resolved_version = Some(pin.resolved_evr.clone());
    subject.source_repo.clone_from(&pin.source_repo);
    subject.artifact = Some(pin.artifact.clone());
    if subject.version.is_none() {
        subject.version = Some(pin.resolved_version.clone());
    }
}

fn apply_raw_pin(subject: &mut InstallSubject, pin: &RawPin) {
    subject.requested_version = Some(pin.requested_version.clone());
    subject.resolved_version = Some(pin.resolved_version.clone());
    subject.source_repo = Some(pin.source_repo.clone());
    subject.artifact = Some(pin.artifact.clone());
    if subject.version.is_none() {
        subject.version = Some(pin.resolved_version.clone());
    }
}

/// Target shape for an existing active record. No remote resolution: the
/// planner's active-record arms only compare identities and versions.
fn target_for_active_record(
    binding: &ProviderBinding,
    family: &str,
    args: &InstallArgs,
    component: &str,
) -> (ProviderTarget, Option<String>) {
    match binding {
        ProviderBinding::Owned { artifact } => {
            let target = if family == "raw" {
                ProviderTarget::Owned {
                    version: artifact.version.clone(),
                }
            } else {
                // Family switch over an owned record: the planner refuses
                // with ProvenanceConflict (I11) before probing anything.
                ProviderTarget::Delegated {
                    pm: NativePm::Rpm,
                    package: args
                        .package
                        .clone()
                        .unwrap_or_else(|| component.to_string()),
                    artifact: None,
                }
            };
            (target, None)
        }
        ProviderBinding::Delegated { package, .. } => {
            let recorded = package.resolved_name().map(str::to_string);
            let package = args
                .package
                .clone()
                .or(recorded)
                .unwrap_or_else(|| component.to_string());
            let target = if family == "raw" {
                ProviderTarget::Owned {
                    version: args.version.clone().unwrap_or_default(),
                }
            } else {
                // Version pins over an existing record are out of scope: the
                // planner's active-record arms decide the outcome (I6–I9) and
                // never re-resolve a pinned artifact.
                ProviderTarget::Delegated {
                    pm: NativePm::Rpm,
                    package: package.clone(),
                    artifact: None,
                }
            };
            (target, Some(package))
        }
    }
}

/// Owned artifact resolution for a settled I1 plan: repo.toml → base_url →
/// package → distribution index entry.
fn resolve_owned_artifact(
    args: &InstallArgs,
    ctx: &CliContext,
    layout: &FsLayout,
    env: &anolisa_env::EnvFacts,
    repo_config: &RepoConfig,
    component: String,
    command: &str,
) -> Result<RawResolution, CliError> {
    let (backend_name, backend) = repo_config
        .select_backend(Some("raw"))
        .map_err(|err| repo_config_err(err, true))?;

    let mut warnings: Vec<String> = Vec::new();
    let (base_url, repository_origin) = match args.repo.as_deref() {
        Some(override_url) => {
            let normalized =
                normalize_override_url(override_url).map_err(|err| repo_config_err(err, true))?;
            if normalized.starts_with("http://") {
                warnings.push(format!(
                    "--repo uses plaintext http ({normalized}) — artifacts are still sha256-verified on the raw backend, but the index itself is unauthenticated",
                ));
            }
            (normalized, Some(RawRepositoryOrigin::CliOverride))
        }
        None => {
            let host = HostVars {
                os: env.os.clone(),
                arch: env.arch.clone(),
            };
            let base_url = repo_config
                .resolved_base_url(backend_name, backend, &host)
                // Variable errors are fixed by editing [vars] in repo.toml.
                .map_err(|err| repo_config_err(err, true))?;
            let origin = repo_config
                .source_path()
                .map(|path| RawRepositoryOrigin::Config(path.to_path_buf()));
            (base_url, origin)
        }
    };
    let index_base_override = normalized_repo_override(args)?;
    let package = resolve_raw_package(
        layout,
        env,
        repo_config,
        backend,
        &component,
        args.package.as_deref(),
        index_base_override.as_deref(),
    );
    resolve_raw(
        ctx,
        layout,
        env,
        ResolveInputs {
            component,
            package,
            backend: backend_name.to_string(),
            base_url,
            repository_origin,
            version: args.version.as_deref(),
            warnings,
        },
    )
    .map_err(|err| err.with_command(command))
}

/// Whether missing rpm/dnf tooling is fatal for the system-RPM presence
/// probe (planner rule I3).
///
/// The probe protects unobserved system RPMs, which can only exist where
/// RPM is the host's package authority. When rpm tooling is present the
/// probe always runs; this only decides what a missing binary means. A
/// host whose `/etc/os-release` `ID`/`ID_LIKE` classifies as deb-family
/// normally has no rpmdb to protect, so the probe degrades to NotProbed
/// instead of demanding rpm/dnf the distro does not ship — unless an rpmdb
/// exists on disk (RPMs were installed before the rpm binary went away),
/// in which case there is a database to protect and the family inference
/// yields to the filesystem evidence. Rpm-family and unrecognized hosts
/// keep the hard error, so a genuinely rpm-based host with broken tooling
/// still fails closed.
pub(crate) fn missing_rpm_tooling_is_fatal(
    env: &anolisa_env::EnvFacts,
    rpmdb: &RpmdbProbe,
) -> bool {
    anolisa_env::package_family(env.os_id.as_deref(), env.os_id_like.as_deref()).as_deref()
        != Some("deb")
        || rpmdb.rpmdb_exists()
}

/// Where filesystem evidence of a host rpmdb is probed.
///
/// The probe targets the databases the host's `PackageQuery` would answer
/// from — never the ANOLISA install layout: a `--prefix` relocates where
/// components are placed, not where the host keeps its rpmdb. Beyond the
/// two system locations, Debian's own rpm package defaults `%_dbpath` to
/// `~/.rpmdb`, so the invoking user's home is probed as well. Deliberately
/// file-based: the check must work exactly when the rpm binary does not.
pub(crate) struct RpmdbProbe {
    /// Host filesystem root holding `var/lib/rpm` and
    /// `usr/lib/sysimage/rpm`; `/` in production.
    system_root: PathBuf,
    /// Home directory probed for Debian's `~/.rpmdb` default dbpath.
    home: Option<PathBuf>,
}

impl RpmdbProbe {
    /// Production probe: the real host root plus the invoking user's home.
    pub(crate) fn for_host(env: &anolisa_env::EnvFacts) -> Self {
        Self {
            system_root: PathBuf::from("/"),
            home: Some(env.home.clone()),
        }
    }

    /// Probe against isolated roots, so tests never read the runner's
    /// real rpmdb (or the developer's `~/.rpmdb`).
    #[cfg(test)]
    pub(crate) fn with_roots(system_root: PathBuf, home: Option<PathBuf>) -> Self {
        Self { system_root, home }
    }

    /// Probe that never finds a database, for tests that assert behavior
    /// on hosts without any rpmdb regardless of the runner's filesystem.
    #[cfg(test)]
    pub(crate) fn absent() -> Self {
        Self {
            system_root: PathBuf::from("/nonexistent-rpmdb-root"),
            home: None,
        }
    }

    /// Whether any known rpmdb location holds a database file, covering
    /// the bdb, ndb, and sqlite backends.
    fn rpmdb_exists(&self) -> bool {
        let dirs = [
            Some(self.system_root.join("var/lib/rpm")),
            Some(self.system_root.join("usr/lib/sysimage/rpm")),
            self.home.as_ref().map(|home| home.join(".rpmdb")),
        ];
        dirs.into_iter().flatten().any(|db| {
            ["rpmdb.sqlite", "Packages", "Packages.db"]
                .iter()
                .any(|file| db.join(file).exists())
        })
    }
}

/// RPM package name a raw install probes for the planner's I3 rule.
///
/// When candidate resolution cannot settle on a single package — rpm
/// tooling is missing on a host whose package authority is not RPM, or the
/// candidates are not unique — an explicit `--package` override is already
/// the known probe identity and wins over the component-name fallback, so
/// the executor's locked recheck re-probes the exact RPM the user named.
#[expect(clippy::too_many_arguments)]
fn system_probe_package(
    args: &InstallArgs,
    layout: &FsLayout,
    env: &anolisa_env::EnvFacts,
    rpmdb: &RpmdbProbe,
    repo_config: &RepoConfig,
    component: &str,
    query: &dyn PackageQuery,
    command: &str,
) -> Result<String, CliError> {
    let fallback = || {
        args.package
            .clone()
            .unwrap_or_else(|| component.to_string())
    };
    let index_base_override = normalized_repo_override(args)?;
    let component_index =
        invocation_component_index(layout, env, repo_config, index_base_override.as_deref());
    let candidates = match rpm_package_candidates_with_index(
        args.package.as_deref(),
        repo_config.backends.get("rpm"),
        component_index.as_ref(),
        query,
        component,
    ) {
        Ok(candidates) => candidates,
        Err(PackageQueryError::CommandMissing { command: bin }) => {
            if missing_rpm_tooling_is_fatal(env, rpmdb) {
                return Err(rpm_tooling_missing_error(command, &bin, component));
            }
            return Ok(fallback());
        }
        Err(err) => return Err(pkg_query_err(err, command)),
    };
    Ok(match candidates.as_slice() {
        [single] => single.package.clone(),
        _ => fallback(),
    })
}

/// Fresh delegated resolution result: the provider target, its bare package,
/// and — when a `--version` was pinned — the resolved candidate metadata for
/// reporting. The component identity is the caller's and never re-mapped.
struct FreshDelegated {
    target: ProviderTarget,
    package: String,
    pin: Option<DelegatedPin>,
}

/// Fresh delegated resolution: the component must resolve to exactly one
/// ANOLISA RPM package. When `--version` is set, the bare package is further
/// resolved to an exact repository candidate for the host architecture, and
/// the resulting NEVRA becomes the native transaction's artifact spec while
/// the bare package stays the observation/record identity.
fn resolve_fresh_delegated(
    args: &InstallArgs,
    layout: &FsLayout,
    env: &anolisa_env::EnvFacts,
    repo_config: &RepoConfig,
    component: &str,
    query: &dyn PackageQuery,
    command: &str,
) -> Result<FreshDelegated, CliError> {
    let index_base_override = normalized_repo_override(args)?;
    let component_index =
        invocation_component_index(layout, env, repo_config, index_base_override.as_deref());
    let candidates = rpm_package_candidates_with_index(
        args.package.as_deref(),
        repo_config.backends.get("rpm"),
        component_index.as_ref(),
        query,
        component,
    )
    .map_err(|err| match err {
        PackageQueryError::CommandMissing { command: bin } => {
            rpm_tooling_missing_error(command, &bin, component)
        }
        err => pkg_query_err(err, command),
    })?;
    match candidates.as_slice() {
        [] => Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "no RPM package is mapped for component '{component}'; add an rpm backend entry to the repo-side component index or publish Provides: anolisa-component({component})"
            ),
        }),
        [single] => {
            let package = single.package.clone();
            // A `--version` pin resolves the bare package to an exact
            // repository candidate before any mutation; the NEVRA is the only
            // value that reaches the native transaction. An unpinned install
            // keeps `artifact` as `None` (repository default).
            let (artifact, pin) = match args.version.as_deref() {
                Some(version) => {
                    let pinned = resolve_pinned_candidate(query, &package, version, &env.arch)
                        .map_err(|err| {
                            pin_error_to_cli(err, command, component, &package, version, &env.arch)
                        })?;
                    let pin = DelegatedPin {
                        requested_version: version.to_string(),
                        resolved_version: pinned.version.clone(),
                        resolved_evr: pinned.evr.clone(),
                        resolved_arch: pinned.arch.clone(),
                        artifact: pinned.artifact.clone(),
                        source_repo: pinned.source_repo.clone(),
                    };
                    (Some(pinned.artifact), Some(pin))
                }
                None => (None, None),
            };
            Ok(FreshDelegated {
                target: ProviderTarget::Delegated {
                    pm: NativePm::Rpm,
                    package: package.clone(),
                    artifact,
                },
                package,
                pin,
            })
        }
        many => Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "multiple RPM candidates match '{component}': {}; cannot resolve unambiguously — pin one with `--package <name>` or fix the component index / package metadata",
                many.iter()
                    .map(RpmTarget::label)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }),
    }
}

/// Map a version-pin resolution failure to a CLI error. Runs during read-only
/// planning, so every arm reports that nothing was changed and never widens
/// the version constraint.
fn pin_error_to_cli(
    err: PinError,
    command: &str,
    component: &str,
    package: &str,
    version: &str,
    arch: &str,
) -> CliError {
    match err {
        PinError::Query(PackageQueryError::CommandMissing { command: bin }) => {
            rpm_tooling_missing_error(command, &bin, component)
        }
        PinError::Query(err) => pkg_query_err(err, command),
        PinError::VersionAbsent => CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "version '{version}' of component '{component}' (package '{package}') is not available in the configured ANOLISA RPM repository; nothing was changed — check `anolisa list` / the published versions and retry with an available `--version`"
            ),
        },
        PinError::ArchUnsupported { offered } => CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "version '{version}' of component '{component}' (package '{package}') is not available for this host architecture '{arch}' (repository offers: {}); nothing was changed",
                offered.join(", ")
            ),
        },
    }
}

enum InstallApply<'a> {
    Owned {
        validated: Box<ValidatedInstall>,
        raw_pin: Option<&'a RawPin>,
        native_package: Option<&'a str>,
        degraded_rpmdb: Option<&'a RpmdbProbe>,
    },
    Delegated {
        package: &'a str,
        delegated_pin: Option<&'a DelegatedPin>,
        repo_config: &'a RepoConfig,
        index_base_override: Option<&'a str>,
        is_root: bool,
    },
}

/// Apply one provider-specific install under the shared lock and journal protocol.
#[expect(clippy::too_many_arguments)]
fn install_applied(
    target: &str,
    ctx: &CliContext,
    layout: &FsLayout,
    state_path: &Path,
    journal_dir: &Path,
    scope: InstallationScope,
    now: &str,
    request: &InstallRequest,
    provider: &DelegatedProvider,
    command: &str,
    reporter: &mut dyn ProgressReporter,
    apply: InstallApply<'_>,
    effects: RawEffectFactories<'_>,
) -> Result<InstallApplicationOutcome, ApplicationFailure> {
    if let InstallApply::Delegated {
        repo_config,
        index_base_override,
        is_root,
        ..
    } = &apply
    {
        require_configured_rpm_backend(repo_config, *index_base_override, command)?;
        if !is_root {
            return Err(CliError::PermissionDenied {
                command: command.to_string(),
                reason: "installing an RPM-backed component runs dnf and requires root".to_string(),
                hint: Some(format!("sudo anolisa install {target}")),
            }
            .into());
        }
    }

    let (native_package, degraded_rpmdb) = match &apply {
        InstallApply::Owned {
            native_package,
            degraded_rpmdb,
            ..
        } => (*native_package, *degraded_rpmdb),
        InstallApply::Delegated { package, .. } => (Some(*package), None),
    };

    let lock = InstallLock::acquire(&layout.lock_file).map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to acquire install lock: {err}"),
    })?;
    let mut store = StateStore::load_for_layout(state_path, privilege::effective_uid(), layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to load installed state: {err}"),
        })?;
    let locked_steps = locked_install_steps(
        target,
        scope,
        native_package,
        provider,
        now,
        &store,
        layout,
        journal_dir,
        request,
        degraded_rpmdb,
        command,
    )?;

    if let ProviderTarget::Delegated {
        package, artifact, ..
    } = &request.target
    {
        check_rpm_install(provider, &[artifact.as_deref().unwrap_or(package)], command)?;
    }

    let evidence = JournalEvidence::new(journal_dir, &store.operations);
    let mut journal_gate = LockedJournalGate::load(&lock, evidence, command)?;
    let mut journal = journal_gate.begin(COMMAND, target, state_path.to_path_buf(), command)?;
    let operation_id = journal.operation_id.clone();

    let (subject, change, provider_warnings) = match apply {
        InstallApply::Owned {
            validated, raw_pin, ..
        } => {
            // Owned installs rely on exact filesystem errors rather than a
            // blanket root pre-check because a relocated prefix may be writable.
            let package = validated.package().to_string();
            let version = validated.version().to_string();
            let mut warnings = validated.warnings().to_vec();
            let (result, retained_note) = {
                let mut ops = RawInstallOps::new(
                    ctx,
                    effects,
                    layout,
                    target.to_string(),
                    scope,
                    now.to_string(),
                    operation_id.clone(),
                    *validated,
                    &mut store,
                    state_path,
                    journal_gate.inventory(),
                );
                let result = execute_owned_steps(&locked_steps, &mut ops, &mut journal);
                let note = ops.retained_packages_note();
                (result, note)
            };
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(err) => {
                    let error = owned_error_to_cli(err, target, scope, command, &retained_note);
                    return Err(ApplicationFailure::from_journal(error, &journal));
                }
            };
            warnings.extend(outcome.warnings);
            let mut subject = InstallSubject {
                component: target.to_string(),
                package: Some(package),
                version: Some(version),
                backend: "raw".to_string(),
                requested_version: None,
                resolved_version: None,
                source_repo: None,
                artifact: None,
            };
            if let Some(pin) = raw_pin {
                apply_raw_pin(&mut subject, pin);
            }
            (subject, InstallChange::OwnedInstalled, warnings)
        }
        InstallApply::Delegated {
            package,
            delegated_pin,
            ..
        } => {
            let context = RecordContext {
                kind: ObjectKind::Component,
                name: target.to_string(),
                scope,
                now: now.to_string(),
                operation_id: Some(operation_id.clone()),
                delegated: Some(DelegatedIdentity {
                    pm: NativePm::Rpm,
                    package: package.to_string(),
                }),
                owned_artifact: None,
            };
            let mut exec_target = DelegatedExecutionTarget::new(NativePm::Rpm, Some(package));
            if let Some(pin) = delegated_pin {
                exec_target = exec_target.with_pinned_artifact(
                    &pin.artifact,
                    &pin.resolved_evr,
                    &pin.resolved_arch,
                );
            }
            let execution = {
                let mut sink = StoreRecordSink::new(&mut store, state_path, context);
                execute_delegated_steps(
                    &locked_steps,
                    exec_target,
                    provider,
                    &mut sink,
                    &mut journal,
                    now,
                )
            };
            let outcome = match execution {
                Ok(outcome) => outcome,
                Err(err) => {
                    let error = CliError::Runtime {
                        command: command.to_string(),
                        reason: format!(
                            "install of '{target}' failed: {err}; the native transaction is never undone automatically — run `anolisa repair {target}` to reconcile"
                        ),
                    };
                    return Err(ApplicationFailure::from_journal(error, &journal));
                }
            };
            let mut subject = InstallSubject {
                component: target.to_string(),
                package: Some(package.to_string()),
                version: outcome
                    .observation
                    .as_ref()
                    .map(|observation| observation.version.clone()),
                backend: "rpm".to_string(),
                requested_version: None,
                resolved_version: None,
                source_repo: None,
                artifact: None,
            };
            if let Some(pin) = delegated_pin {
                apply_delegated_pin(&mut subject, pin);
            }
            (subject, InstallChange::DelegatedInstalled, Vec::new())
        }
    };

    reporter.report(&format!("Finalizing {target} installation..."));

    // Operation history is best-effort bookkeeping on top of the committed
    // record: the install already succeeded, so a history-write failure
    // degrades to a warning instead of unwinding anything.
    store.operations.push(OperationRecord {
        id: operation_id.clone(),
        command: command.to_string(),
        status: "ok".to_string(),
        started_at: now.to_string(),
        finished_at: Some(now_iso8601()),
        parent_operation_id: None,
    });
    let mut warnings = Vec::new();
    if let Err(err) = store.save(state_path) {
        warnings.push(format!("failed to record operation history: {err}"));
    }
    warnings.extend(provider_warnings);
    if change == InstallChange::DelegatedInstalled {
        warnings.extend(super::io_util::snapshot_datadir_contract(
            layout,
            target,
            command,
            ctx.packaged_data_probe(),
        ));
    }
    reporter.finish();
    Ok(InstallApplicationOutcome::Applied {
        subject,
        steps: locked_steps,
        outcome: CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            Some(operation_id),
            vec![change],
            warnings,
        ),
    })
}

#[expect(clippy::too_many_arguments)]
fn locked_install_steps(
    target: &str,
    scope: InstallationScope,
    native_package: Option<&str>,
    provider: &DelegatedProvider<'_>,
    now: &str,
    store: &StateStore,
    layout: &FsLayout,
    journal_dir: &Path,
    request: &InstallRequest,
    degraded_rpmdb: Option<&RpmdbProbe>,
    command: &str,
) -> Result<Vec<Step>, CliError> {
    let state_snapshot = observe_install_state(target, scope, now, store, layout, journal_dir)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: err.to_string(),
        })?;
    if !matches!(state_snapshot.state(), ProbeEvidence::Absent { .. }) {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "a record for '{target}' appeared while this install was resolving; nothing was changed — re-run `anolisa install {target}`"
            ),
        });
    }

    if let Some(rpmdb) = degraded_rpmdb
        && rpmdb.rpmdb_exists()
    {
        return Err(rpmdb_appeared_error(command, target));
    }

    let snapshot = match observe_install_snapshot(
        target,
        scope,
        native_package,
        now,
        store,
        Some(provider),
        layout,
        journal_dir,
    ) {
        Ok(snapshot) => snapshot,
        Err(FactsError::Probe(ProviderError::Query(PackageQueryError::CommandMissing {
            command: bin,
        }))) => match degraded_rpmdb {
            None => return Err(rpm_tooling_missing_error(command, &bin, target)),
            Some(rpmdb) if rpmdb.rpmdb_exists() => {
                return Err(rpmdb_appeared_error(command, target));
            }
            Some(_) => {
                observe_install_snapshot(target, scope, None, now, store, None, layout, journal_dir)
                    .map_err(|err| CliError::Runtime {
                        command: command.to_string(),
                        reason: err.to_string(),
                    })?
            }
        },
        Err(FactsError::Probe(err)) => {
            let package = native_package.unwrap_or(target);
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!("locked rpm query failed for '{package}': {err}"),
            });
        }
        Err(err) => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: err.to_string(),
            });
        }
    };

    if matches!(snapshot.native_package(), ProbeEvidence::Present { .. }) {
        let package = native_package.unwrap_or(target);
        return Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!(
                "system RPM '{package}' appeared while '{target}' was being resolved; nothing was changed — run `sudo anolisa --install-mode system adopt {target}` or retry after removing the external package"
            ),
        });
    }

    let active_adapter_claims = store
        .adapter_claims
        .iter()
        .filter(|claim| claim.component == target)
        .map(|claim| claim.framework.clone())
        .collect();
    let facts =
        lifecycle_facts_from_snapshot(&snapshot, active_adapter_claims, None).map_err(|err| {
            CliError::Runtime {
                command: command.to_string(),
                reason: err.to_string(),
            }
        })?;
    let prepared = ExecutionIntent::Apply.prepare(
        plan(&Intent::Install(request.clone()), &facts)
            .map_err(|err| plan_error_to_cli(err, target, command))?,
    );
    match prepared {
        PreparedExecution::Apply { steps, .. } => Ok(steps),
        PreparedExecution::NoOp { .. } => Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "the facts for '{target}' changed while this install was resolving; nothing was changed — re-run `anolisa install {target}`"
            ),
        }),
        PreparedExecution::Preview { .. } => {
            unreachable!("ExecutionIntent::Apply cannot produce a preview")
        }
    }
}

/// Locked recheck that the native package is still absent before mutation.
///
/// Presence evidence is never muted — when the probe can run and reports
/// the package, the install refuses regardless of the host family.
///
/// `degraded` selects the CommandMissing policy: `None` keeps the hard
/// error, while `Some(probe)` is the deb-family degrade. The degraded
/// verdict is a planning-time snapshot, so it is re-verified against the
/// filesystem under the lock: if an rpmdb has appeared since — an external
/// process installed RPMs, whether or not this process can see the rpm
/// binary, and whether or not the probe identity matches what that process
/// installed — the vacuous-probe justification is gone and the install
/// refuses. A retry re-plans against the now-present database with full
/// candidate resolution.
pub(crate) fn revalidate_native_absence(
    package: Option<&str>,
    provider: &DelegatedProvider,
    now: &str,
    target: &str,
    command: &str,
    degraded: Option<&RpmdbProbe>,
) -> Result<(), CliError> {
    let Some(package) = package else {
        return Ok(());
    };
    if let Some(rpmdb) = degraded
        && rpmdb.rpmdb_exists()
    {
        return Err(rpmdb_appeared_error(command, target));
    }
    match provider.observe(package, now) {
        Ok(NativeProbe::Absent) => Ok(()),
        Ok(NativeProbe::Present { .. } | NativeProbe::MultipleVersions { .. }) => {
            Err(CliError::InvalidArgument {
                command: command.to_string(),
                reason: format!(
                    "system RPM '{package}' appeared while '{target}' was being resolved; nothing was changed — run `sudo anolisa --install-mode system adopt {target}` or retry after removing the external package"
                ),
            })
        }
        Ok(NativeProbe::NotProbed) => Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!("locked system-RPM probe for '{package}' did not run"),
        }),
        Err(ProviderError::Query(PackageQueryError::CommandMissing { command: bin })) => {
            match degraded {
                None => Err(rpm_tooling_missing_error(command, &bin, target)),
                Some(rpmdb) if rpmdb.rpmdb_exists() => Err(rpmdb_appeared_error(command, target)),
                Some(_) => Ok(()),
            }
        }
        Err(err) => Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!("locked rpm query failed for '{package}': {err}"),
        }),
    }
}

/// Refusal when the locked recheck finds rpmdb evidence that was absent at
/// planning time: the raw placement stops before any mutation and the retry
/// re-runs the presence probe against the database.
fn rpmdb_appeared_error(command: &str, target: &str) -> CliError {
    CliError::InvalidArgument {
        command: command.to_string(),
        reason: format!(
            "an rpm database appeared while '{target}' was being resolved; nothing was changed — retry `anolisa install {target}` so the system-RPM presence check runs against it"
        ),
    }
}

/// DNF repository source for this invocation's RPM family.
///
/// A normalized `--repo` override replaces the configured base URL — the
/// flag is a one-off base URL for the selected backend, so the repository
/// that resolved identity and package also serves availability queries, the
/// native transaction, and the locked re-checks. Settings other than the URL
/// (gpgcheck) stay with the repo.toml rpm backend when one is configured.
pub(crate) fn rpm_repo_source_for_invocation(
    repo_config: &RepoConfig,
    env: &anolisa_env::EnvFacts,
    index_base_override: Option<&str>,
) -> Result<Option<RpmRepoSource>, CliError> {
    match index_base_override {
        Some(base_url) => {
            let gpgcheck = repo_config
                .backends
                .get("rpm")
                .and_then(|backend| backend.gpgcheck);
            Ok(Some(RpmRepoSource::new(
                ANOLISA_RPM_REPO_ID,
                base_url.to_string(),
                gpgcheck,
            )))
        }
        None => configured_rpm_repo_source(repo_config, env),
    }
}

pub(crate) fn configured_rpm_repo_source(
    repo_config: &RepoConfig,
    env: &anolisa_env::EnvFacts,
) -> Result<Option<RpmRepoSource>, CliError> {
    let Some(backend) = repo_config.backends.get("rpm") else {
        return Ok(None);
    };
    let host = HostVars {
        os: env.os.clone(),
        arch: env.arch.clone(),
    };
    let base_url = repo_config
        .resolved_base_url("rpm", backend, &host)
        .map_err(|err| repo_config_err(err, true))?;
    Ok(Some(RpmRepoSource::new(
        ANOLISA_RPM_REPO_ID,
        base_url,
        backend.gpgcheck,
    )))
}

/// Require a repository the delegated transaction can be pinned to.
///
/// Without one, dnf would resolve against arbitrary host repos. A normalized
/// `--repo` override IS that repository — the DNF source is built from it —
/// so the configured-backend requirement only applies when no override
/// pinned the source.
pub(crate) fn require_configured_rpm_backend(
    repo_config: &RepoConfig,
    index_base_override: Option<&str>,
    command: &str,
) -> Result<(), CliError> {
    if index_base_override.is_some() || repo_config.backends.contains_key("rpm") {
        Ok(())
    } else {
        Err(repo_config_err(
            RepoConfigError::BackendNotConfigured {
                name: "rpm".to_string(),
            },
            true,
        )
        .with_command(command))
    }
}

/// Resolve the raw distribution package for a settled component identity.
///
/// Explicit package overrides and repo.toml `package_map` entries choose the
/// distribution package first; otherwise the raw backend row of this
/// invocation's component index — the `--repo` override's index when one was
/// given — decides. The component itself is never rewritten — identity was
/// settled by lifecycle resolution before this pass.
pub(crate) fn resolve_raw_package(
    layout: &FsLayout,
    env: &anolisa_env::EnvFacts,
    repo_config: &RepoConfig,
    backend: &BackendConfig,
    component: &str,
    cli_override: Option<&str>,
    index_base_override: Option<&str>,
) -> String {
    if cli_override.is_some() || backend.package_map.contains_key(component) {
        return repo_config.package_name(backend, component, cli_override);
    }

    let component_index = invocation_component_index(layout, env, repo_config, index_base_override);
    let resolver = ComponentResolver::new(component_index.as_ref(), None, None);
    match resolver.resolve(component, BackendKind::Raw, ResolveOptions::default()) {
        Ok(ResolutionSet::Unique(target)) => target.package,
        _ => repo_config.package_name(backend, component, cli_override),
    }
}

/// True when the store holds a quarantined record for this component.
pub(crate) fn quarantined(store: &StateStore, component: &str) -> bool {
    store
        .quarantined
        .iter()
        .any(|q| q.record.kind == ObjectKind::Component && q.record.name == component)
}

/// Map an owned-executor failure to a CLI error that reports honestly what
/// happened to the host: cleanly unwound, partially unwound, or untouched.
fn owned_error_to_cli(
    err: OwnedExecutionError,
    target: &str,
    scope: InstallationScope,
    command: &str,
    retained_note: &str,
) -> CliError {
    let repair = common::scoped_component_command(scope, "repair", target);
    let reason = match err {
        OwnedExecutionError::StepFailed {
            step,
            source,
            rolled_back,
            rollback_warnings,
            ..
        } => {
            let at = step_label(&step);
            if !rolled_back {
                format!(
                    "install of '{target}' failed at '{at}': {source}; the host was not changed{retained_note}"
                )
            } else if rollback_warnings.is_empty() {
                format!(
                    "install of '{target}' failed at '{at}': {source}; this run's changes were undone{retained_note}"
                )
            } else {
                format!(
                    "install of '{target}' failed at '{at}': {source}; undoing this run's changes reported problems ({}) — run `{repair}`{retained_note}",
                    rollback_warnings.join("; ")
                )
            }
        }
        OwnedExecutionError::RecoveryUncertain { detail, .. } => {
            format!("install of '{target}' failed: {detail}; run `{repair}`{retained_note}")
        }
        other => format!("install of '{target}' failed: {other}{retained_note}"),
    };
    CliError::Runtime {
        command: command.to_string(),
        reason,
    }
}

/// Actionable "rpm/dnf tooling missing" error. The system-RPM presence check
/// needs the native tooling; installing without it could place raw files
/// over an unobserved system RPM.
fn rpm_tooling_missing_error(command: &str, bin: &str, target: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "cannot install '{target}': {bin} not found on PATH — install the required RPM tooling and retry"
        ),
    }
}

fn pkg_query_err(err: PackageQueryError, command: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!("rpm query failed: {err}"),
    }
}

/// Map a planning refusal to an actionable CLI error. The planner names the
/// way out; this mapping only renders it.
fn plan_error_to_cli(err: PlanError, target: &str, command: &str) -> CliError {
    let command = command.to_string();
    match err {
        PlanError::AlreadyPresentOnSystem => CliError::InvalidArgument {
            command,
            reason: format!(
                "'{target}' is already installed as a system RPM that ANOLISA does not manage; run `anolisa adopt {target}` to start tracking it"
            ),
        },
        PlanError::UseUpdate => CliError::InvalidArgument {
            command,
            reason: format!(
                "component '{target}' is already installed at a different version; run `anolisa update {target} --version <version>` to change versions"
            ),
        },
        PlanError::AlreadyManaged => CliError::InvalidArgument {
            command,
            reason: format!(
                "component '{target}' is already managed through the native package manager; run `anolisa update {target}` to move versions"
            ),
        },
        PlanError::ExternallyRemoved => CliError::InvalidArgument {
            command,
            reason: format!(
                "the package backing '{target}' was removed outside ANOLISA; run `anolisa repair {target}` to reconcile or `anolisa forget {target}` to drop the record"
            ),
        },
        PlanError::TrackedButAbsent => CliError::InvalidArgument {
            command,
            reason: format!(
                "'{target}' is tracked but its package is no longer installed; run `anolisa forget {target}` to drop the record, then install again"
            ),
        },
        PlanError::NeedsAttention => CliError::InvalidArgument {
            command,
            reason: format!(
                "the record for '{target}' was quarantined by the state migration; run `anolisa repair {target}` to resolve it"
            ),
        },
        PlanError::ProvenanceConflict => CliError::InvalidArgument {
            command,
            reason: format!(
                "the requested backend/package conflicts with the recorded provenance of '{target}'; uninstall it first or re-run without the conflicting override"
            ),
        },
        PlanError::DelegatedRequiresSystemScope => CliError::InvalidArgument {
            command,
            reason: format!(
                "installing '{target}' through the RPM backend requires system mode; re-run with sudo or use `--backend raw`"
            ),
        },
        PlanError::PendingOperation => CliError::Runtime {
            command,
            reason: format!(
                "a previous operation on '{target}' is pending recovery; run `anolisa repair {target}` before retrying"
            ),
        },
        other => CliError::InvalidArgument {
            command,
            reason: format!("cannot install '{target}': {other:?}"),
        },
    }
}

/// Human-facing label for a plan step (preview rendering).
pub(crate) fn step_label(step: &Step) -> String {
    match step {
        Step::NativeTransaction {
            action, packages, ..
        } => format!(
            "RPM package manager: {} {}",
            action.verb(),
            packages.join(" ")
        ),
        Step::Observe { packages } => format!("observe {}", packages.join(" ")),
        Step::WriteRecord(write) => format!("record: {}", write.label()),
        Step::DropRecord => "record: drop".to_string(),
        Step::DownloadVerify => "download and verify artifact".to_string(),
        Step::ProvisionRuntimeDeps => "provision runtime dependencies".to_string(),
        Step::RunHook(kind) => format!(
            "run {} hooks",
            match kind {
                HookKind::PreInstall => "pre-install",
                HookKind::PostInstall => "post-install",
                HookKind::PreUninstall => "pre-uninstall",
                HookKind::PostUninstall => "post-uninstall",
            }
        ),
        Step::BackupFiles => "back up current files".to_string(),
        Step::PlaceFiles => "place files".to_string(),
        Step::SetCapabilities => "apply file capabilities".to_string(),
        Step::EnableServices => "enable services".to_string(),
        Step::RestartServices => "restart services".to_string(),
        Step::StopServices => "stop services".to_string(),
        Step::RemoveOwnedFiles => "remove owned files".to_string(),
        other => format!("{other:?}"),
    }
}

fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::super::application::{InstallRequest as ApplicationRequest, run_with_dependencies};
    use super::super::tests::*;
    use super::super::{InstallResultPayload, dry_run_detail_lines, handle};
    use super::*;
    use crate::repo_config::RepoConfig;
    use anolisa_platform::fs_layout::FsLayout;
    use std::cell::{Cell, RefCell};
    use tempfile::tempdir;

    #[derive(Default)]
    struct RecordingProgress {
        messages: RefCell<Vec<String>>,
        finished: Cell<bool>,
    }

    impl ProgressReporter for RecordingProgress {
        fn report(&self, message: &str) {
            self.messages.borrow_mut().push(message.to_string());
        }

        fn finish(&mut self) {
            self.finished.set(true);
        }
    }

    #[test]
    fn missing_rpm_tooling_fatality_follows_host_package_family() {
        let rpmdb = RpmdbProbe::absent();
        let env = |id: Option<&str>, id_like: Option<&str>| anolisa_env::EnvFacts {
            os_id: id.map(str::to_string),
            os_id_like: id_like.map(str::to_string),
            ..linux_env()
        };
        // rpm-family hosts must treat missing rpm/dnf as a hard I3 error.
        assert!(missing_rpm_tooling_is_fatal(
            &env(Some("alinux"), None),
            &rpmdb
        ));
        assert!(missing_rpm_tooling_is_fatal(
            &env(Some("fedora"), None),
            &rpmdb
        ));
        // deb-family hosts have no rpmdb; the probe degrades to NotProbed.
        assert!(!missing_rpm_tooling_is_fatal(
            &env(Some("ubuntu"), None),
            &rpmdb
        ));
        // A derivative declaring deb lineage via ID_LIKE also degrades.
        assert!(!missing_rpm_tooling_is_fatal(
            &env(Some("zorin"), Some("ubuntu debian")),
            &rpmdb
        ));
        // Unrecognized or undetected hosts fail closed.
        assert!(missing_rpm_tooling_is_fatal(
            &env(Some("alpine"), Some("musl")),
            &rpmdb
        ));
        assert!(missing_rpm_tooling_is_fatal(&env(None, None), &rpmdb));
    }

    #[test]
    fn rpmdb_on_disk_overrides_the_deb_family_relaxation() {
        // A deb-family host that installed RPMs before losing the rpm
        // binary still owns a database the raw path could corrupt: the
        // on-disk rpmdb evidence must win over the family inference, for
        // every db backend and both host system locations.
        let deb_env = anolisa_env::EnvFacts {
            os_id: Some("ubuntu".to_string()),
            os_id_like: Some("debian".to_string()),
            ..linux_env()
        };
        for (dir, file) in [
            ("var/lib/rpm", "rpmdb.sqlite"),
            ("var/lib/rpm", "Packages"),
            ("var/lib/rpm", "Packages.db"),
            ("usr/lib/sysimage/rpm", "rpmdb.sqlite"),
        ] {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let rpmdb = RpmdbProbe::with_roots(tmp.path().to_path_buf(), None);
            assert!(
                !missing_rpm_tooling_is_fatal(&deb_env, &rpmdb),
                "without an rpmdb the deb-family host degrades"
            );
            let db = tmp.path().join(dir);
            std::fs::create_dir_all(&db).expect("rpmdb dir");
            std::fs::write(db.join(file), b"").expect("rpmdb file");
            assert!(
                missing_rpm_tooling_is_fatal(&deb_env, &rpmdb),
                "{dir}/{file} must force the hard error"
            );
        }
    }

    #[test]
    fn debian_default_home_dbpath_also_counts_as_rpmdb_evidence() {
        // Debian's own rpm package defaults %_dbpath to ~/.rpmdb, so a
        // database in the invoking user's home is host evidence too.
        let deb_env = anolisa_env::EnvFacts {
            os_id: Some("debian".to_string()),
            os_id_like: None,
            ..linux_env()
        };
        let system = tempfile::tempdir().expect("system root");
        let home = tempfile::tempdir().expect("home");
        let rpmdb =
            RpmdbProbe::with_roots(system.path().to_path_buf(), Some(home.path().to_path_buf()));
        assert!(
            !missing_rpm_tooling_is_fatal(&deb_env, &rpmdb),
            "no database anywhere degrades"
        );
        let db = home.path().join(".rpmdb");
        std::fs::create_dir_all(&db).expect("home rpmdb dir");
        std::fs::write(db.join("Packages"), b"").expect("home rpmdb file");
        assert!(
            missing_rpm_tooling_is_fatal(&deb_env, &rpmdb),
            "~/.rpmdb must force the hard error"
        );
    }

    #[test]
    fn rpmdb_evidence_ignores_the_install_prefix() {
        // A --prefix relocates where components are placed, not where the
        // host keeps its rpmdb: a database inside the install prefix is
        // ANOLISA's own tree, while the host probe roots stay clean — the
        // degrade must follow the host, not the layout.
        let deb_env = anolisa_env::EnvFacts {
            os_id: Some("ubuntu".to_string()),
            os_id_like: Some("debian".to_string()),
            ..linux_env()
        };
        let host_root = tempfile::tempdir().expect("host root");
        let prefix = tempfile::tempdir().expect("install prefix");
        let inside_prefix = prefix.path().join("var/lib/rpm");
        std::fs::create_dir_all(&inside_prefix).expect("prefix rpm dir");
        std::fs::write(inside_prefix.join("rpmdb.sqlite"), b"").expect("prefix file");
        let rpmdb = RpmdbProbe::with_roots(host_root.path().to_path_buf(), None);
        assert!(
            !missing_rpm_tooling_is_fatal(&deb_env, &rpmdb),
            "a file under the install prefix is not host rpmdb evidence"
        );
        // Conversely a host database keeps the hard error no matter what
        // prefix the install targets.
        let host_db = host_root.path().join("var/lib/rpm");
        std::fs::create_dir_all(&host_db).expect("host rpm dir");
        std::fs::write(host_db.join("rpmdb.sqlite"), b"").expect("host file");
        assert!(
            missing_rpm_tooling_is_fatal(&deb_env, &rpmdb),
            "host rpmdb evidence is independent of the install prefix"
        );
    }

    #[test]
    fn locked_recheck_refuses_on_rpmdb_evidence_even_when_probe_reports_absent() {
        // The essence of the planning-degrade races: under the lock the
        // probe may answer Absent for a stale or fallback identity (e.g. a
        // differently named provider was installed meanwhile), or still
        // fail with CommandMissing while another process grew a database.
        // With the degraded policy, on-disk rpmdb evidence must refuse
        // regardless of what the per-package query reports.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let rpmdb = RpmdbProbe::with_roots(tmp.path().to_path_buf(), None);
        let db = tmp.path().join("var/lib/rpm");
        std::fs::create_dir_all(&db).expect("rpmdb dir");
        std::fs::write(db.join("rpmdb.sqlite"), b"").expect("rpmdb file");
        // The default FakeQuery reports every package as absent.
        let q = FakeQuery::default();
        let provider = DelegatedProvider::new(&q, &NoTxn);

        let err = revalidate_native_absence(
            Some("copilot-shell"),
            &provider,
            "2026-01-01T00:00:00Z",
            "copilot-shell",
            COMMAND,
            Some(&rpmdb),
        )
        .expect_err("rpmdb evidence must refuse under the degraded policy");
        assert!(
            err.reason().contains("rpm database appeared"),
            "got: {}",
            err.reason()
        );

        // The same evidence with the fatal policy defers to the probe: the
        // absent answer from working tooling is authoritative there.
        revalidate_native_absence(
            Some("copilot-shell"),
            &provider,
            "2026-01-01T00:00:00Z",
            "copilot-shell",
            COMMAND,
            None,
        )
        .expect("the fatal policy trusts the probe answer");
    }

    #[test]
    fn delegated_install_reports_execution_and_finalization() {
        let (_tmp, mut ctx) = system_ctx_with_configured_rpm_repo(false);
        ctx.quiet = true;
        let fake = FakeInstaller::new(
            "copilot-shell",
            pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"),
        )
        .with_origin("anolisa");
        let mut install_args = args("copilot-shell");
        install_args.backend = Some("rpm".to_string());
        let mut reporter = RecordingProgress::default();

        let outcome = run_with_dependencies(
            ApplicationRequest {
                component: "copilot-shell",
                args: &install_args,
                intent: ExecutionIntent::Apply,
            },
            &ctx,
            &linux_env(),
            &RpmdbProbe::absent(),
            &fake,
            &fake,
            true,
            &HashSet::new(),
            &mut reporter,
            crate::test_support::raw_effects(),
        )
        .expect("delegated install");

        let InstallApplicationOutcome::Applied {
            subject,
            steps,
            outcome: command_outcome,
        } = &outcome
        else {
            panic!("fresh delegated install must return an applied outcome");
        };
        assert_eq!(subject.component, "copilot-shell");
        assert_eq!(subject.package.as_deref(), Some("copilot-shell"));
        assert_eq!(subject.version.as_deref(), Some("2.3.0"));
        assert_eq!(subject.backend, "rpm");
        assert!(!steps.is_empty());
        assert_eq!(command_outcome.status(), &CommandOutcomeStatus::Completed);
        assert!(command_outcome.operation_id().is_some());
        assert_eq!(
            command_outcome.changes(),
            &[InstallChange::DelegatedInstalled]
        );
        assert_eq!(
            outcome.batch_outcome(),
            super::super::InstallOutcome::Installed
        );
        assert_eq!(
            reporter.messages.borrow().as_slice(),
            [
                "Installing copilot-shell...",
                "Finalizing copilot-shell installation...",
            ]
        );
        assert!(
            reporter.finished.get(),
            "progress must stop before the final result is rendered"
        );
    }

    #[test]
    fn pinned_payload_json_exposes_requested_resolved_repo_and_artifact() {
        // The JSON envelope for a version-pinned install must carry the
        // requested/resolved version, the source repo, the exact artifact, and
        // the bare package — while keeping `version` as a compatible answer.
        let pin = DelegatedPin {
            requested_version: "0.6.2".to_string(),
            resolved_version: "0.6.2".to_string(),
            resolved_evr: "0.6.2-1.alnx4".to_string(),
            resolved_arch: "x86_64".to_string(),
            artifact: "agentsight-0.6.2-1.alnx4.x86_64".to_string(),
            source_repo: Some("anolisa-configured".to_string()),
        };
        let payload = InstallResultPayload {
            component: "agentsight".to_string(),
            package: Some("agentsight".to_string()),
            version: None,
            backend: "rpm".to_string(),
            action: "planned",
            operation_id: None,
            requested_version: None,
            resolved_version: None,
            source_repo: None,
            artifact: None,
            dry_run: true,
            plan: vec![step_label(&Step::NativeTransaction {
                pm: NativePm::Rpm,
                action: anolisa_core::planner::NativeAction::Install,
                packages: vec!["agentsight-0.6.2-1.alnx4.x86_64".to_string()],
            })],
        }
        .with_pin(&pin);

        let json = serde_json::to_value(&payload).expect("serialize payload");
        assert_eq!(json["component"], "agentsight");
        assert_eq!(json["package"], "agentsight");
        assert_eq!(json["requested_version"], "0.6.2");
        assert_eq!(json["resolved_version"], "0.6.2-1.alnx4");
        assert_eq!(json["source_repo"], "anolisa-configured");
        assert_eq!(json["artifact"], "agentsight-0.6.2-1.alnx4.x86_64");
        assert_eq!(
            json["plan"][0],
            "RPM package manager: install agentsight-0.6.2-1.alnx4.x86_64"
        );
        // `version` stays present as the upstream version (compatible field).
        assert_eq!(json["version"], "0.6.2");
    }

    #[test]
    fn pinned_dry_run_detail_lines_show_package_resolved_version_and_artifact() {
        // The human dry-run contract: bare package, requested version, resolved
        // EVR, exact artifact, and source repository, each on its own line.
        let pin = DelegatedPin {
            requested_version: "0.6.2".to_string(),
            resolved_version: "0.6.2".to_string(),
            resolved_evr: "0.6.2-1.alnx4".to_string(),
            resolved_arch: "x86_64".to_string(),
            artifact: "agentsight-0.6.2-1.alnx4.x86_64".to_string(),
            source_repo: Some("anolisa-configured".to_string()),
        };
        let payload = InstallResultPayload {
            component: "agentsight".to_string(),
            package: Some("agentsight".to_string()),
            version: None,
            backend: "rpm".to_string(),
            action: "planned",
            operation_id: None,
            requested_version: None,
            resolved_version: None,
            source_repo: None,
            artifact: None,
            dry_run: true,
            plan: Vec::new(),
        }
        .with_pin(&pin);

        assert_eq!(
            dry_run_detail_lines(&payload),
            vec![
                "package: agentsight".to_string(),
                "requested version: 0.6.2".to_string(),
                "resolved version: 0.6.2-1.alnx4".to_string(),
                "artifact: agentsight-0.6.2-1.alnx4.x86_64".to_string(),
                "repository: anolisa-configured".to_string(),
            ]
        );
    }

    #[test]
    fn unpinned_dry_run_detail_lines_are_empty() {
        // No pin → no detail lines; the unpinned/owned preview is unchanged.
        let payload = InstallResultPayload {
            component: "agentsight".to_string(),
            package: Some("agentsight".to_string()),
            version: Some("0.6.2".to_string()),
            backend: "rpm".to_string(),
            action: "planned",
            operation_id: None,
            requested_version: None,
            resolved_version: None,
            source_repo: None,
            artifact: None,
            dry_run: true,
            plan: vec!["dnf install agentsight".to_string()],
        };
        assert!(dry_run_detail_lines(&payload).is_empty());
    }

    #[test]
    fn unpinned_payload_json_omits_pin_fields() {
        // Without a pin the additive fields must not appear, preserving the
        // pre-existing wire contract for plain installs.
        let payload = InstallResultPayload {
            component: "agentsight".to_string(),
            package: Some("agentsight".to_string()),
            version: Some("0.6.2".to_string()),
            backend: "rpm".to_string(),
            action: "installed",
            operation_id: None,
            requested_version: None,
            resolved_version: None,
            source_repo: None,
            artifact: None,
            dry_run: false,
            plan: Vec::new(),
        };
        let json = serde_json::to_value(&payload).expect("serialize payload");
        assert!(json.get("requested_version").is_none());
        assert!(json.get("resolved_version").is_none());
        assert!(json.get("source_repo").is_none());
        assert!(json.get("artifact").is_none());
    }

    fn sample_raw_pin() -> RawPin {
        RawPin {
            requested_version: "0.1.0".to_string(),
            resolved_version: "0.1.0".to_string(),
            artifact: "https://example.com/anolisa/agentsight-0.1.0.tar.gz".to_string(),
            source_repo: "https://example.com/anolisa".to_string(),
        }
    }

    #[test]
    fn raw_pinned_payload_json_exposes_requested_resolved_repo_and_artifact() {
        // The owned pin must fill the same additive envelope fields as the
        // delegated pin: requested/resolved version, source repo, and the
        // exact artifact (the URL, the raw analog of a NEVRA). `version`
        // remains the resolved distribution version for compatibility.
        let pin = sample_raw_pin();
        let payload = InstallResultPayload {
            component: "agentsight".to_string(),
            package: Some("agentsight".to_string()),
            version: Some(pin.resolved_version.clone()),
            backend: "raw".to_string(),
            action: "installed",
            operation_id: Some("op-install-1".to_string()),
            requested_version: Some(pin.requested_version),
            resolved_version: Some(pin.resolved_version),
            source_repo: Some(pin.source_repo),
            artifact: Some(pin.artifact),
            dry_run: false,
            plan: Vec::new(),
        };

        let json = serde_json::to_value(&payload).expect("serialize payload");
        assert_eq!(json["backend"], "raw");
        assert_eq!(json["requested_version"], "0.1.0");
        assert_eq!(json["resolved_version"], "0.1.0");
        assert_eq!(json["source_repo"], "https://example.com/anolisa");
        assert_eq!(
            json["artifact"],
            "https://example.com/anolisa/agentsight-0.1.0.tar.gz"
        );
        // `version` was None going in — the pin must fill the compatible
        // field with the resolved entry version.
        assert_eq!(json["version"], "0.1.0");
    }

    #[test]
    fn raw_pinned_dry_run_detail_lines_show_resolved_version_and_artifact() {
        // The raw pinned preview shares the delegated detail contract: the
        // resolved candidate is spelled out instead of echoing the request.
        let pin = sample_raw_pin();
        let payload = InstallResultPayload {
            component: "agentsight".to_string(),
            package: None,
            version: Some(pin.resolved_version.clone()),
            backend: "raw".to_string(),
            action: "planned",
            operation_id: None,
            requested_version: Some(pin.requested_version),
            resolved_version: Some(pin.resolved_version),
            source_repo: Some(pin.source_repo),
            artifact: Some(pin.artifact),
            dry_run: true,
            plan: Vec::new(),
        };

        assert_eq!(
            dry_run_detail_lines(&payload),
            vec![
                "requested version: 0.1.0".to_string(),
                "resolved version: 0.1.0".to_string(),
                "artifact: https://example.com/anolisa/agentsight-0.1.0.tar.gz".to_string(),
                "repository: https://example.com/anolisa".to_string(),
            ]
        );
    }

    #[test]
    fn raw_resolution_does_not_rewrite_exact_state_identity() {
        let tmp = tempdir().expect("tempdir");
        let repo_root = tmp.path().join("repo");
        let repo_v1 = repo_root.join("v1");
        std::fs::create_dir_all(&repo_v1).expect("repo dir");
        std::fs::write(
            repo_v1.join("components-v2.toml"),
            r#"
schema_version = 2

[[components]]
name = "cosh"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.backends]]
kind = "raw"
package = "cosh"

[[components.aliases]]
kind = "raw-package"
name = "legacy-name"

[[components]]
name = "sec-core"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.backends]]
kind = "raw"
package = "agent-sec-core"
"#,
        )
        .expect("component index");
        let repo_config = RepoConfig::from_toml_str(&format!(
            "schema_version = 1\ndefault_backend = \"raw\"\n[backends.raw]\nbase_url = \"file://{}\"\n",
            repo_root.display()
        ))
        .expect("repo config");
        let backend = repo_config.backends.get("raw").expect("raw backend");
        let layout = FsLayout::system(Some(tmp.path().join("root")));
        let env = anolisa_env::EnvService::detect();

        // An exact state identity that only appears as an index alias keeps
        // its own name as the distribution package: alias rows resolve
        // identities before this pass and never redirect package selection.
        let exact_alias_name = resolve_raw_package(
            &layout,
            &env,
            &repo_config,
            backend,
            "legacy-name",
            None,
            None,
        );
        let canonical =
            resolve_raw_package(&layout, &env, &repo_config, backend, "cosh", None, None);
        let mapped_package =
            resolve_raw_package(&layout, &env, &repo_config, backend, "sec-core", None, None);

        assert_eq!(exact_alias_name, "legacy-name");
        assert_eq!(canonical, "cosh");
        assert_eq!(mapped_package, "agent-sec-core");
    }

    /// A `--repo` override's published index decides the raw package, the
    /// same authority identity resolution consults — the repo.toml chain is
    /// never mixed in (issue #2630 review follow-up).
    #[test]
    fn repo_override_index_decides_the_raw_package() {
        let tmp = tempdir().expect("tempdir");
        let override_v1 = tmp.path().join("override").join("v1");
        std::fs::create_dir_all(&override_v1).expect("override repo");
        std::fs::write(
            override_v1.join("components-v2.toml"),
            r#"
schema_version = 2

[[components]]
name = "cosh"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.backends]]
kind = "raw"
package = "cosh-artifact"
"#,
        )
        .expect("component index");
        let repo_config = RepoConfig::from_toml_str(
            "schema_version = 1\ndefault_backend = \"raw\"\n[backends.raw]\nbase_url = \"https://example.invalid/raw\"\n",
        )
        .expect("repo config");
        let backend = repo_config.backends.get("raw").expect("raw backend");
        let layout = FsLayout::system(Some(tmp.path().join("root")));
        let env = anolisa_env::EnvService::detect();
        let override_base = format!("file://{}", override_v1.display());

        let with_override = resolve_raw_package(
            &layout,
            &env,
            &repo_config,
            backend,
            "cosh",
            None,
            Some(&override_base),
        );
        let without_override =
            resolve_raw_package(&layout, &env, &repo_config, backend, "cosh", None, None);

        assert_eq!(
            with_override, "cosh-artifact",
            "the override repository's index must decide the raw package"
        );
        assert_eq!(
            without_override, "cosh",
            "without an override the unreachable repo.toml index falls back to the component name"
        );
    }

    #[test]
    fn install_unknown_component_is_invalid_argument() {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().join("sys");
        let mut a = args("no-such-component");
        a.repo = Some(write_empty_repo(&tmp.path().join("repo")));

        let err =
            handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix))).expect_err("must error");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("no-such-component"));
    }

    #[test]
    fn install_unsupported_mode_is_invalid_argument() {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().join("sys");
        let mut a = args("agentsight");
        a.repo = Some(write_local_repo_component(
            &tmp.path().join("repo"),
            "agentsight",
            "0.2.0",
            &["user"],
        ));

        let err =
            handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix))).expect_err("must error");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("install mode is not supported"),
            "got: {}",
            err.reason()
        );
    }

    #[test]
    fn install_manifest_mode_mismatch_is_invalid_argument() {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().join("sys");
        let mut a = args("agentsight");
        a.repo = Some(write_local_repo_component_with_modes(
            &tmp.path().join("repo"),
            "agentsight",
            "0.2.0",
            &["system"],
            &["user"],
        ));

        let err =
            handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(prefix))).expect_err("must error");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason()
                .contains("inconsistent with the distribution index")
                && err.reason().contains("system-mode support"),
            "got: {}",
            err.reason()
        );
    }

    #[test]
    fn install_unconfigured_backend_is_invalid_argument() {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().to_path_buf();
        seed_repo_config_with_index(&FsLayout::system(Some(prefix.clone())), &["agentsight"]);
        let mut a = args("agentsight");
        a.backend = Some("npm".to_string());
        let err = handle(a, &ctx_with_prefix(false, Some(prefix))).expect_err("must error");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("npm"), "got: {}", err.reason());
        assert!(
            err.reason().contains("repo.toml"),
            "reason must point at repo.toml: {}",
            err.reason()
        );
    }

    #[test]
    fn install_unknown_backend_is_invalid_argument() {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().to_path_buf();
        seed_repo_config_with_index(&FsLayout::system(Some(prefix.clone())), &["agentsight"]);
        let mut a = args("agentsight");
        a.backend = Some("pip".to_string());
        let err = handle(a, &ctx_with_prefix(false, Some(prefix))).expect_err("must error");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("pip"));
    }

    #[test]
    fn install_configured_npm_backend_is_not_implemented() {
        let tmp = tempdir().expect("tmpdir");
        let prefix = tmp.path().to_path_buf();
        let layout = FsLayout::system(Some(prefix.clone()));
        std::fs::create_dir_all(&layout.etc_dir).expect("etc dir");
        let v1 = layout.etc_dir.join("test-index-repo").join("v1");
        write_component_index_v2(&v1, &["agentsight"]);
        std::fs::write(
            layout.etc_dir.join("repo.toml"),
            format!(
                r#"schema_version = 1
default_backend = "raw"

[backends.raw]
base_url = "file://{}"

[backends.npm]
base_url = "https://registry.npmjs.org"
scope = "@anolisa"
"#,
                v1.display()
            ),
        )
        .expect("write repo.toml");

        let mut a = args("agentsight");
        a.backend = Some("npm".to_string());
        let err = handle(a, &ctx_with_prefix(false, Some(prefix))).expect_err("must error");
        assert_eq!(err.code(), "NOT_IMPLEMENTED");
        assert!(err.reason().contains("npm"), "got: {}", err.reason());
    }

    #[test]
    fn install_invalid_repo_override_is_invalid_argument() {
        let tmp = tempdir().expect("tmpdir");
        let mut a = args("agentsight");
        a.repo = Some("ftp://example.com/repo".to_string());
        let err = handle_with_fake_rpm(a, &ctx_with_prefix(false, Some(tmp.path().to_path_buf())))
            .expect_err("must error");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("ftp"), "got: {}", err.reason());
    }

    /// A `--repo` override replaces the DNF source's base URL — the same
    /// repository that resolved identity and package also serves queries and
    /// the native transaction — while backend-scoped settings (gpgcheck)
    /// stay with the repo.toml rpm backend when one is configured.
    #[test]
    fn rpm_repo_source_honors_the_repo_override() {
        let repo = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "rpm"
[backends.rpm]
base_url = "https://repo.example/anolisa"
gpgcheck = false
"#,
        )
        .expect("parse repo");

        let overridden =
            rpm_repo_source_for_invocation(&repo, &linux_env(), Some("https://mirror.example/os"))
                .expect("resolve rpm repo")
                .expect("override source");
        assert_eq!(overridden.id(), ANOLISA_RPM_REPO_ID);
        assert_eq!(overridden.base_url(), "https://mirror.example/os");
        assert_eq!(overridden.gpgcheck(), Some(false));

        let configured = rpm_repo_source_for_invocation(&repo, &linux_env(), None)
            .expect("resolve rpm repo")
            .expect("configured source");
        assert_eq!(configured.base_url(), "https://repo.example/anolisa");

        // Without a configured rpm backend the override still yields a
        // source; backend-scoped settings simply stay unset.
        let raw_only = RepoConfig::from_toml_str(
            "schema_version = 1\ndefault_backend = \"raw\"\n[backends.raw]\nbase_url = \"https://example.com/anolisa\"\n",
        )
        .expect("parse repo");
        let overridden = rpm_repo_source_for_invocation(
            &raw_only,
            &linux_env(),
            Some("https://mirror.example/os"),
        )
        .expect("resolve rpm repo")
        .expect("override source");
        assert_eq!(overridden.gpgcheck(), None);
    }

    #[test]
    fn configured_rpm_repo_source_uses_repo_toml_backend() {
        let repo = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "rpm"
[vars]
releasever = "4"
[backends.rpm]
base_url = "http://repo.example/alinux/$releasever/agentic-os/$basearch/os/"
insecure = true
gpgcheck = false
"#,
        )
        .expect("parse repo");
        let source = configured_rpm_repo_source(&repo, &linux_env())
            .expect("resolve rpm repo")
            .expect("rpm repo exists");
        assert_eq!(source.id(), ANOLISA_RPM_REPO_ID);
        assert_eq!(
            source.base_url(),
            "http://repo.example/alinux/4/agentic-os/x86_64/os"
        );
        assert_eq!(source.gpgcheck(), Some(false));
    }

    #[test]
    fn install_family_follows_flag_then_record_then_default() {
        use anolisa_core::domain::{
            Installation, LifecycleStatus, ManagementRelation, PackageIdentity,
        };

        let repo = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "raw"
[backends.raw]
base_url = "https://example.com/anolisa"
[backends.rpm]
base_url = "https://repo.example/anolisa"
"#,
        )
        .expect("parse repo");

        // Explicit --backend wins, canonicalized.
        let mut a = args("cosh");
        a.backend = Some("rpm".to_string());
        let store = StateStore::empty();
        assert_eq!(install_family(&a, &store, "cosh", &repo), "rpm");

        // Recorded provenance is sticky: a delegated record routes to rpm
        // even though the default backend is raw.
        let a = args("cosh");
        let mut store = StateStore::empty();
        store.upsert(Installation {
            kind: ObjectKind::Component,
            name: "cosh".to_string(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved {
                    name: "copilot-shell".to_string(),
                },
                relation: ManagementRelation::Managed {
                    since: "2026-07-01T00:00:00Z".to_string(),
                },
                last_observed: None,
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-07-01T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        });
        assert_eq!(install_family(&a, &store, "cosh", &repo), "rpm");

        // No flag, no record: the default backend decides.
        let store = StateStore::empty();
        assert_eq!(install_family(&a, &store, "cosh", &repo), "raw");
    }
}
