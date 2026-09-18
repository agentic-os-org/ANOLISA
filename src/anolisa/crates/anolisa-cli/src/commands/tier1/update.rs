//! `anolisa update` — unified update surface.
//!
//! Three forms:
//! - `update <component>` - update one ANOLISA-managed component.
//! - `update self` - update the `anolisa` CLI binary only.
//! - `update all` - update every ANOLISA-managed runtime, osbase, and
//!   adapter object.
//!
//! The component is a positional argument; `self` / `all` are subcommands
//! (kept mutually exclusive with the positional via
//! `args_conflicts_with_subcommands`). A component literally named `self` or
//! `all` would be shadowed by the subcommand — those are reserved.
//!
//! Explicit invariant: `update all` does **not** include CLI self-update. The
//! binary swap never shares a transaction with component updates.
//!
//! `update <component>` runs the thin-shell pipeline: assemble facts, ask
//! the planner (decision rows U1–U8), and hand the step sequence to the
//! matching executor. The plan shape follows the record's authority:
//!
//!   * **Owned** (raw) — U3: the CLI resolves the latest published version
//!     and classifies it against the recorded one; a newer version replaces
//!     the owned files through the owned executor (compensating back to the
//!     previous files on failure), the same version is a clean no-op (U2),
//!     and an older or non-orderable version refuses (U4).
//!   * **Delegated managed/adopted** — U5: `dnf update` through the
//!     delegated executor, then re-observe and refresh the cached
//!     observation. dnf picks the target version. After the record refresh
//!     persisted, the package-owned contract snapshot is refreshed too
//!     ([`complete_delegated_update`]); a refresh failure demotes the
//!     operation to `partial` instead of overstating it as `ok`.
//!   * **Delegated observed** — U6: refuses; management consent (`adopt`)
//!     is required before native transactions.
//!
//! Backend/authority is never switched by an update. `update all` (the
//! [`all`] module) plans every recorded component through the same decision
//! rows and merges the delegated refreshes (U5) into one native transaction.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{SecondsFormat, Utc};
use clap::{Parser, Subcommand};
use serde::Serialize;

use anolisa_core::adapter::claim::{self, AdapterSourceRevision};
use anolisa_core::adapter::manager::{AdapterManager, VisibleRoot};
use anolisa_core::central_log::{CentralLog, LogKind, LogRecord, LogStatus, Severity};
use anolisa_core::component_snapshot::{
    ComponentSnapshot, ComponentSnapshotRequest, SnapshotProbe,
};
use anolisa_core::domain::{InstallationScope, ManagementRelation, OwnedArtifact, ProviderBinding};
use anolisa_core::facts::{FactsError, assemble_component_snapshot, lifecycle_facts_from_snapshot};
use anolisa_core::owned_executor::OwnedExecutionError;
use anolisa_core::planner::{
    Facts, Intent, NativeProbe, OwnedUpdateResolution, Plan, PlanError, Step, UpdateRequest,
    VersionRelation, plan,
};
use anolisa_core::providers::DelegatedProvider;
use anolisa_core::state::{ObjectKind, OperationRecord};
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::PackageTransaction;
use anolisa_platform::privilege;
use anolisa_platform::rpm_query::RpmPackageQuery;
use anolisa_platform::rpm_repo::RpmRepoSource;
use anolisa_platform::rpm_transaction::RpmTransaction;

use super::install::{
    RawResolution, refresh_datadir_contract_snapshot, resolve_raw, resolve_raw_inputs_for_component,
};
use super::rpm_install;
use crate::color::Palette;
use crate::commands::common;
use crate::commands::common::RepoPersistPolicy;
use crate::context::CliContext;
use crate::repo_config::{HostVars, RepoConfig};
use crate::response::{self, CliError};

// `pub(crate)` so `anolisa upgrade` (issue #1411) can reuse the read-only
// planner (`check::compute_update_check_report`) instead of re-deriving it.
pub(crate) mod all;
mod application;
pub(crate) mod check;
mod self_update;

/// Command label for JSON envelopes and error routing.
const COMMAND: &str = "update";

const ANOLISA_RPM_REPO_ID: &str = "anolisa-configured";

/// Arguments for the unified update command surface.
///
/// `anolisa update <component>` updates a single component directly; the
/// `self` and `all` subcommands cover the CLI binary and batch
/// update. `args_conflicts_with_subcommands` keeps the positional and the
/// subcommands mutually exclusive so `update foo self` is a parse error.
#[derive(Debug, Parser)]
#[command(args_conflicts_with_subcommands = true)]
pub struct UpdateArgs {
    /// Component to update (omit when using a `self` / `all` subcommand)
    #[arg(value_name = "COMPONENT")]
    pub component: Option<String>,
    /// Update the CLI binary (`self`) or every component (`all`) instead of a
    /// single component.
    #[command(subcommand)]
    pub command: Option<UpdateCommands>,
    /// Report which RPM-backed components can be upgraded, read-only.
    ///
    /// `--check` only reads installed packages and repository candidates,
    /// never writes ANOLISA state, and never persists repo/adapter configuration. It
    /// is mutually exclusive with a component
    /// argument and with the `self` / `all` subcommands (the latter is enforced
    /// by `args_conflicts_with_subcommands`). `--motd`, `--refresh`, and
    /// `--target` are only meaningful together with `--check`.
    #[arg(long)]
    pub check: bool,
    /// With `--check`: emit a short, low-noise MOTD summary (silent when
    /// nothing can be upgraded).
    #[arg(long, requires = "check")]
    pub motd: bool,
    /// With `--check`: bypass the cached report and re-query.
    #[arg(long, requires = "check")]
    pub refresh: bool,
    /// With `--check`: evaluate against a named target profile so its missing
    /// default components are reported as installable.
    #[arg(long, requires = "check", value_name = "TARGET")]
    pub target: Option<String>,
}

/// Update operations that intentionally keep CLI self-update and batch update
/// separate from a single-component update.
#[derive(Debug, Subcommand)]
pub enum UpdateCommands {
    /// Update the anolisa CLI binary only
    #[command(name = "self")]
    SelfBin,
    /// Update every recorded ANOLISA component.
    ///
    /// Plans each component through the same decision rows as a single
    /// update and merges the delegated refreshes into one dnf transaction.
    /// Does NOT include the CLI binary itself — use `anolisa update self`
    /// for that.
    All,
}

/// Dispatches the selected `anolisa update` form.
///
/// # Errors
///
/// Returns [`CliError`] when the selected update operation fails or no target
/// is given.
pub fn handle(args: UpdateArgs, ctx: &CliContext) -> Result<(), CliError> {
    // `--check` is the read-only upgrade-detection entry (issue #1410). It is
    // dispatched before the mutating forms so a stray component/subcommand is
    // rejected instead of silently updating something.
    if args.check {
        if args.component.is_some() {
            return Err(CliError::InvalidArgument {
                command: check::CHECK_COMMAND.to_string(),
                reason: "`--check` reports upgrades for every managed component and takes no component argument; run `anolisa update --check`".to_string(),
            });
        }
        // `args_conflicts_with_subcommands` already makes `update --check self`
        // a parse error; this guard keeps the invariant explicit for any direct
        // caller that bypasses clap.
        if args.command.is_some() {
            return Err(CliError::InvalidArgument {
                command: check::CHECK_COMMAND.to_string(),
                reason: "`--check` cannot be combined with the `self` or `all` subcommands"
                    .to_string(),
            });
        }
        return check::handle_update_check(&args, ctx);
    }

    // `args_conflicts_with_subcommands` guarantees `command` and `component`
    // are never both set, so a present subcommand always wins.
    match (args.command, args.component) {
        (Some(UpdateCommands::SelfBin), _) => self_update::handle(ctx),
        (Some(UpdateCommands::All), _) => all::handle_update_all(ctx),
        (None, Some(component)) => handle_component_update(&component, ctx),
        (None, None) => Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: "specify a component to update (e.g. `anolisa update <component>`), or use `anolisa update self` / `anolisa update all`".to_string(),
        }),
    }
}

// `pub(crate)`: shared with `check` (read-only) and `upgrade` (issue #1411) so
// all three resolve the configured ANOLISA RPM repository the same way.
pub(crate) fn rpm_repo_source_for_update(
    repo_config: &RepoConfig,
    env: &anolisa_env::EnvFacts,
    command: &str,
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
        .map_err(|err| CliError::InvalidArgument {
            command: command.to_string(),
            reason: err.to_string(),
        })?;
    Ok(Some(RpmRepoSource::new(
        ANOLISA_RPM_REPO_ID,
        base_url,
        backend.gpgcheck,
    )))
}

// ── component update: the thin-shell pipeline ──

/// Wire shape for an `update <component>` result (`--json`) and its dry-run
/// preview.
#[derive(Serialize)]
struct UpdateResultPayload {
    component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    /// Version recorded/observed before the update, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    from_version: Option<String>,
    /// Version after the update (or the resolved target on dry-run).
    #[serde(skip_serializing_if = "Option::is_none")]
    to_version: Option<String>,
    /// Whether the version actually changed (false on "already latest").
    updated: bool,
    dry_run: bool,
    plan: Vec<String>,
    /// `None` on dry-run (nothing recorded).
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    /// Adapter follow-up actions produced by the update (issue #1885).
    /// Always present so consumers can rely on the key; empty when no
    /// adapter bundle changed (including no-op, dry-run, and failed updates).
    adapter_actions: Vec<AdapterAction>,
}

/// One adapter follow-up action produced by a successful component update.
/// Wire shape of the `adapter_actions` entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AdapterAction {
    pub(crate) component: String,
    pub(crate) framework: String,
    pub(crate) reason: String,
    pub(crate) command: String,
}

/// Package-manager-owned adapter revisions captured around an update.
pub(crate) type AdapterRevisionSnapshot = BTreeMap<String, Option<AdapterSourceRevision>>;

const SYSTEM_REVISION_CHANGED_REASON: &str = "adapter source revision changed during system update";

/// Capture every adapter revision declared by `component` without consulting
/// receipt state.
///
/// Contract resolution is scoped to the active layout's state and fixed
/// package datadir. File metadata comes from the installation's owning package
/// manager, so runtime-created files never enter the comparison.
pub(crate) fn adapter_revision_snapshot(
    ctx: &CliContext,
    layout: &FsLayout,
    component: &str,
) -> AdapterRevisionSnapshot {
    if ctx.install_mode != crate::context::InstallMode::System {
        return AdapterRevisionSnapshot::new();
    }
    let mut datadir_roots = vec![layout.datadir.clone()];
    if let Some(package_datadir) = layout.package_datadir()
        && !datadir_roots.contains(&package_datadir)
    {
        datadir_roots.push(package_datadir);
    }
    let Ok(state) = StateStore::load_for_layout(
        &layout.state_dir.join("installed.toml"),
        privilege::effective_uid(),
        layout,
    ) else {
        return AdapterRevisionSnapshot::new();
    };
    let mut manager = AdapterManager::new(layout.clone(), None, "system-update".into());
    manager.set_visible_roots(vec![VisibleRoot {
        state_dir: layout.state_dir.clone(),
        contract_datadir_roots: datadir_roots,
    }]);
    manager.source_revision_snapshot(component, &state)
}

/// Adapter actions a committed update of `component` leaves behind.
///
/// User updates may compare receipts owned by the same user. System updates
/// never load receipt state; they compare trusted source snapshots and direct
/// affected users to inspect their own receipts.
pub(crate) fn adapter_actions_after_update(
    ctx: &CliContext,
    store: &StateStore,
    component: &str,
    before: &AdapterRevisionSnapshot,
) -> Vec<AdapterAction> {
    if ctx.install_mode == crate::context::InstallMode::System {
        let layout = common::resolve_layout(ctx);
        return system_adapter_actions(
            component,
            before,
            &adapter_revision_snapshot(ctx, &layout, component),
        );
    }
    receipt_adapter_actions(ctx, store, component)
}

fn receipt_adapter_actions(
    ctx: &CliContext,
    store: &StateStore,
    component: &str,
) -> Vec<AdapterAction> {
    let manager = common::build_adapter_manager(ctx);
    store
        .adapter_claims
        .iter()
        .filter(|receipt| {
            receipt.component == component
                && receipt.status == anolisa_core::adapter::claim::ClaimStatus::Enabled
        })
        .filter_map(|receipt| {
            let reason = match manager.source_revision_match(receipt, store) {
                anolisa_core::adapter::managed_files::ManagedMatch::Matched => return None,
                anolisa_core::adapter::managed_files::ManagedMatch::Changed(_) => {
                    claim::SOURCE_REVISION_CHANGED_REASON.to_string()
                }
                anolisa_core::adapter::managed_files::ManagedMatch::Unknown(reason) => reason,
            };
            Some(AdapterAction {
                component: receipt.component.clone(),
                framework: receipt.framework.clone(),
                reason,
                command: format!(
                    "anolisa adapter enable {} {}",
                    receipt.component, receipt.framework
                ),
            })
        })
        .collect()
}

fn system_adapter_actions(
    component: &str,
    before: &AdapterRevisionSnapshot,
    after: &AdapterRevisionSnapshot,
) -> Vec<AdapterAction> {
    before
        .iter()
        .filter_map(|(framework, before_revision)| {
            let changed = match (before_revision, after.get(framework)) {
                (Some(before), Some(Some(after))) => before != after,
                (Some(_), _) => true,
                (None, Some(Some(_))) => true,
                (None, _) => false,
            };
            changed.then(|| AdapterAction {
                component: component.to_string(),
                framework: framework.clone(),
                reason: SYSTEM_REVISION_CHANGED_REASON.to_string(),
                command: format!("anolisa adapter status {component}"),
            })
        })
        .collect()
}

/// Human-readable notices naming every adapter a successful update left
/// with a stale resource bundle, each with its exact recovery command.
pub(crate) fn render_adapter_action_notices(actions: &[AdapterAction], color: &Palette) {
    for action in actions {
        println!(
            "{} adapter {}/{}: {}",
            color.warn("⚠"),
            action.component,
            action.framework,
            action.reason,
        );
        println!(
            "  {} {}",
            color.label("run:"),
            color.command(&action.command)
        );
    }
}

/// Wire `update <component>` to the live host: a delegated record updates
/// from the configured ANOLISA RPM repository (which must be declared in
/// repo.toml), an owned record resolves its latest published version through
/// the raw backend inside the pipeline.
fn handle_component_update(component: &str, ctx: &CliContext) -> Result<(), CliError> {
    let outcome = application::run(
        application::ApplicationRequest {
            component,
            intent: execution_intent(ctx),
        },
        ctx,
    )?;
    render_application_outcome(ctx, outcome).map(|_| ())
}

fn execution_intent(ctx: &CliContext) -> anolisa_core::execution::ExecutionIntent {
    if ctx.dry_run {
        anolisa_core::execution::ExecutionIntent::Plan
    } else {
        anolisa_core::execution::ExecutionIntent::Apply
    }
}

/// Real host backends for one component update: rpm query/transaction
/// pointed at the configured ANOLISA repo when the record is delegated, so
/// candidate probes and the update transaction never fall back to arbitrary
/// host repos.
pub(crate) fn update_backends(
    component: &str,
    ctx: &CliContext,
) -> Result<(RpmPackageQuery, RpmTransaction), CliError> {
    let command = format!("update {component}");
    let layout = common::resolve_layout(ctx);
    let (resolved, view) = common::resolve_mutation_target(component, ctx, &command)?;
    let store = &view.writable.state;
    let is_delegated = matches!(
        store
            .find(ObjectKind::Component, &resolved)
            .map(|r| &r.binding),
        Some(ProviderBinding::Delegated { .. })
    );
    if is_delegated {
        let repo_config =
            common::load_repo_config(ctx, &layout, &command, RepoPersistPolicy::Require)?;
        let env = anolisa_env::EnvService::detect();
        let repo = rpm_repo_source_for_update(&repo_config, &env, &command)?.ok_or_else(|| {
            CliError::InvalidArgument {
                command: command.clone(),
                reason: "repo.toml has no [backends.rpm] table; cannot update an RPM-backed component from the configured ANOLISA repository".to_string(),
            }
        })?;
        return Ok(super::rpm_backends::system(Some(repo)));
    }
    Ok(super::rpm_backends::system(None))
}

/// What a component update left behind, for batch summaries: a transaction
/// (or file replacement) ran, or the record already covered the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateOutcome {
    Updated,
    AlreadyCurrent,
}

/// What the single-component update pipeline reports to its caller: the
/// transaction outcome plus any adapter follow-up actions (issue #1885).
pub(crate) type ComponentUpdateResult = Result<(UpdateOutcome, Vec<AdapterAction>), CliError>;

/// What the planning prefix decided for one component update, before any
/// side effect ran. The single-component path executes it directly; batch
/// orchestration classifies on the route to group delegated refreshes (U5)
/// into one merged native transaction.
pub(crate) struct PlannedComponentUpdate {
    pub(crate) command: String,
    pub(crate) target: String,
    pub(crate) native_package: Option<String>,
    pub(crate) scope: InstallationScope,
    pub(crate) now: String,
    /// Raw artifact resolution prepared for an owned record (U2–U4).
    pub(crate) owned_execution: Option<(RawResolution, OwnedArtifact)>,
    /// `(recorded, latest published)` versions for an owned record.
    pub(crate) owned_versions: Option<(String, String)>,
    /// rpmdb EVR the planning observation saw, for the wire `from` field and
    /// the merged-failure fact check.
    pub(crate) native_from: Option<String>,
    /// Typed planner output classified by the application execution intent.
    pub(crate) plan: Plan,
    pub(crate) route: PlannedUpdateRoute,
}

/// Which executor family the update plan routed to, or the idempotent NoOp.
pub(crate) enum PlannedUpdateRoute {
    /// U2: the recorded version is already the latest published one.
    AlreadyCurrent,
    /// Delegated step family (U5: one native update, observe, refresh).
    Delegated { steps: Vec<Step> },
    /// Owned step family (U3: replace files through the raw backend).
    Owned,
}

/// Core of [`handle_component_update`] with the package backends injected so
/// tests drive every branch without a live rpmdb/dnf or real privileges.
///
/// The pipeline observes, plans (decision rows U1–U8), and routes the step
/// sequence to the matching executor: a delegated plan re-runs `dnf update`
/// through the delegated executor, an owned plan replaces the recorded files
/// with the resolved latest published version through the owned executor.
/// A committed update also reports the adapters it left with a stale
/// resource bundle (issue #1885); no-op, dry-run, and failed updates report
/// none.
#[cfg(test)]
pub(crate) fn update_component_with_deps(
    input: &str,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
    is_root: bool,
) -> ComponentUpdateResult {
    let outcome = application::run_with_dependencies(
        application::ApplicationRequest {
            component: input,
            intent: execution_intent(ctx),
        },
        ctx,
        query,
        txn,
        is_root,
        crate::test_support::raw_effects(),
    )?;
    render_application_outcome(ctx, outcome)
}

/// Planning prefix of a component update: resolve the record, assemble host
/// facts (and the owned artifact resolution when the record is owned), and
/// ask the planner for the step sequence. Read-only against the host —
/// every side effect belongs to the application execution phase.
pub(crate) fn plan_component_update(
    input: &str,
    ctx: &CliContext,
    query: &dyn PackageQuery,
    txn: &dyn PackageTransaction,
) -> Result<PlannedComponentUpdate, CliError> {
    let command = format!("update {input}");
    let layout = common::resolve_layout(ctx);
    let journal_dir = rpm_install::journal_dir(&layout);
    let uid = privilege::effective_uid();
    let scope = match ctx.install_mode {
        crate::context::InstallMode::System => InstallationScope::System,
        crate::context::InstallMode::User => InstallationScope::User { uid },
    };
    let now = now_iso8601();

    let (resolved, view) = common::resolve_mutation_target(input, ctx, &command)?;
    let store = view.writable.state;
    let target = resolved.as_str();

    // The probe target comes from the record; update never switches the
    // package a component is bound to.
    let native_package = match store.find(ObjectKind::Component, target) {
        Some(installation) => match &installation.binding {
            ProviderBinding::Delegated { package, .. } => match package.resolved_name() {
                Some(name) => Some(name.to_string()),
                None => {
                    return Err(CliError::Runtime {
                        command,
                        reason: format!(
                            "the record for '{target}' has no resolved package name; run `anolisa repair {target}` first"
                        ),
                    });
                }
            },
            ProviderBinding::Owned { .. } => None,
        },
        None => None,
    };

    let provider = DelegatedProvider::new(query, txn);
    let facts = observe_update_facts(
        target,
        scope,
        native_package.as_deref(),
        &now,
        &store,
        &provider,
        &layout,
        &journal_dir,
        &command,
    )?;

    // An owned record needs its update target resolved before planning: the
    // CLI resolves the latest published version and classifies it against
    // the recorded one; the planner turns that relation into U2–U4.
    let mut owned_execution: Option<(RawResolution, OwnedArtifact)> = None;
    let owned_resolution = match store
        .find(ObjectKind::Component, target)
        .map(|r| &r.binding)
    {
        Some(ProviderBinding::Owned { artifact }) => {
            let repo_config =
                common::load_repo_config(ctx, &layout, &command, RepoPersistPolicy::Require)?;
            let env = anolisa_env::EnvService::detect();
            let inputs = resolve_raw_inputs_for_component(
                target.to_string(),
                "raw",
                artifact.raw_package.as_deref(),
                &env,
                &repo_config,
                &command,
            )?;
            let resolution =
                resolve_raw(ctx, &layout, &env, inputs).map_err(|e| e.with_command(&command))?;
            let to_version = resolution.entry.version.clone();
            let relation = version_relation(&artifact.version, &to_version);
            owned_execution = Some((resolution, artifact.clone()));
            Some(OwnedUpdateResolution {
                to_version,
                relation,
            })
        }
        _ => None,
    };
    let owned_versions = owned_execution
        .as_ref()
        .map(|(resolution, prior)| (prior.version.clone(), resolution.entry.version.clone()));

    let intent = Intent::Update(UpdateRequest { owned_resolution });
    let lifecycle_plan = plan(&intent, &facts)
        .map_err(|err| plan_error_to_cli(err, target, &command, owned_versions.as_ref()))?;
    let route = match &lifecycle_plan {
        Plan::Execute { steps, .. } => {
            // Route by step family: owned plans replace files through the raw
            // backend, delegated plans re-run the native transaction.
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
                PlannedUpdateRoute::Delegated {
                    steps: steps.clone(),
                }
            } else {
                PlannedUpdateRoute::Owned
            }
        }
        Plan::NoOp { .. } => PlannedUpdateRoute::AlreadyCurrent,
    };

    Ok(PlannedComponentUpdate {
        command,
        target: target.to_string(),
        native_package,
        scope,
        now,
        owned_execution,
        owned_versions,
        native_from: native_observed_version(&facts),
        plan: lifecycle_plan,
        route,
    })
}

#[expect(clippy::too_many_arguments)]
fn observe_update_snapshot(
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
    if native_package.is_some() && provider.is_some() && matches!(scope, InstallationScope::System)
    {
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

fn active_adapter_claims(store: &StateStore, component: &str) -> Vec<String> {
    store
        .adapter_claims
        .iter()
        .filter(|claim| claim.component == component)
        .map(|claim| claim.framework.clone())
        .collect()
}

#[expect(clippy::too_many_arguments)]
pub(super) fn observe_update_facts(
    component: &str,
    scope: InstallationScope,
    native_package: Option<&str>,
    observed_at: &str,
    store: &StateStore,
    provider: &DelegatedProvider<'_>,
    layout: &FsLayout,
    journal_dir: &Path,
    command: &str,
) -> Result<Facts, CliError> {
    let record_needs_native = matches!(
        store
            .find(ObjectKind::Component, component)
            .map(|record| &record.binding),
        Some(ProviderBinding::Delegated {
            relation: ManagementRelation::Managed { .. } | ManagementRelation::Adopted { .. },
            ..
        })
    );
    let snapshot = match observe_update_snapshot(
        component,
        scope,
        native_package,
        observed_at,
        store,
        Some(provider),
        layout,
        journal_dir,
    ) {
        Ok(snapshot) => snapshot,
        // A native update cannot proceed without rpm/dnf. Other relations
        // replan without the probe so the planner can name the real way out.
        Err(FactsError::Probe(anolisa_core::providers::ProviderError::Query(
            PackageQueryError::CommandMissing { command: binary },
        ))) => {
            if record_needs_native {
                return Err(tooling_missing_err(command, &binary, component));
            }
            observe_update_snapshot(
                component,
                scope,
                native_package,
                observed_at,
                store,
                None,
                layout,
                journal_dir,
            )
            .map_err(|err| CliError::Runtime {
                command: command.to_string(),
                reason: err.to_string(),
            })?
        }
        Err(err) => {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: err.to_string(),
            });
        }
    };
    lifecycle_facts_from_snapshot(&snapshot, active_adapter_claims(store, component), None).map_err(
        |err| CliError::Runtime {
            command: command.to_string(),
            reason: err.to_string(),
        },
    )
}

/// Classify a resolved `candidate` version against the `installed` one,
/// semver-aware (tolerating a leading `v`).
///
/// When either side is not valid semver, equal normalized strings are
/// [`VersionRelation::Same`] and anything else is
/// [`VersionRelation::Indeterminate`] — a non-semver version is never
/// silently treated as an upgrade, so the planner's downgrade guard (U4)
/// stays effective for it (a non-semver installed version that is actually
/// newer must not be replaced by an older published one).
fn version_relation(installed: &str, candidate: &str) -> VersionRelation {
    fn norm(s: &str) -> &str {
        let t = s.trim();
        t.strip_prefix('v').unwrap_or(t)
    }
    match (
        semver::Version::parse(norm(installed)),
        semver::Version::parse(norm(candidate)),
    ) {
        (Ok(installed), Ok(candidate)) => match candidate.cmp(&installed) {
            std::cmp::Ordering::Less => VersionRelation::Older,
            std::cmp::Ordering::Equal => VersionRelation::Same,
            std::cmp::Ordering::Greater => VersionRelation::Newer,
        },
        _ if norm(installed) == norm(candidate) => VersionRelation::Same,
        _ => VersionRelation::Indeterminate,
    }
}

/// EVR (or plain version) the pre-update native probe observed, for the
/// human/JSON "from" field.
fn native_observed_version(facts: &Facts) -> Option<String> {
    match &facts.native {
        NativeProbe::Present { observation, .. } => Some(
            observation
                .evr
                .clone()
                .unwrap_or_else(|| observation.version.clone()),
        ),
        _ => None,
    }
}

/// Whether the record, as re-read under the install lock, still authorizes
/// the planned native update: it must still be delegated, still point at the
/// same resolved package (`dnf update` ran against the pre-lock identity,
/// and recording its result against a different package would graft one
/// package's version onto another's record), and still carry management
/// consent (an observed record never plans a native update).
pub(crate) fn native_update_authorized(
    store: &StateStore,
    target: &str,
    package: Option<&str>,
) -> bool {
    match store
        .find(ObjectKind::Component, target)
        .map(|r| &r.binding)
    {
        Some(ProviderBinding::Delegated {
            relation: ManagementRelation::Managed { .. } | ManagementRelation::Adopted { .. },
            package: recorded,
            ..
        }) => recorded.resolved_name() == package,
        _ => false,
    }
}

/// Map a planning refusal to an actionable CLI error. The planner names the
/// way out; this mapping only renders it. `owned_versions` carries the
/// `(installed, latest)` pair resolved for an owned record so the downgrade
/// refusals can name both sides.
fn plan_error_to_cli(
    err: PlanError,
    target: &str,
    command: &str,
    owned_versions: Option<&(String, String)>,
) -> CliError {
    let command = command.to_string();
    match err {
        PlanError::NotInstalled => CliError::NotInstalled {
            command,
            reason: format!(
                "component '{target}' is not installed — nothing to update (run `anolisa status` to see what is installed, or `anolisa install {target}` to install it)"
            ),
        },
        PlanError::NotAdopted => CliError::InvalidArgument {
            command,
            reason: format!(
                "component '{target}' is only observed, not managed; run `anolisa adopt {target}` first, then update"
            ),
        },
        PlanError::ExternallyRemoved => CliError::InvalidArgument {
            command,
            reason: format!(
                "the package backing '{target}' was removed outside ANOLISA; run `anolisa repair {target}` to reconcile or `anolisa forget {target}` to drop the record"
            ),
        },
        PlanError::RefuseDowngrade => {
            let detail = match owned_versions {
                Some((installed, latest)) => format!(
                    "the latest version published for '{target}' is {latest}, older than the installed {installed}"
                ),
                None => format!(
                    "the latest version published for '{target}' is older than the installed one"
                ),
            };
            CliError::InvalidArgument {
                command,
                reason: format!("{detail}; refusing to downgrade (update only moves forward)"),
            }
        }
        PlanError::IndeterminateVersion => {
            let detail = match owned_versions {
                Some((installed, latest)) => format!(
                    "cannot tell whether the published {latest} is newer than the installed {installed} for '{target}' (non-semver version)"
                ),
                None => format!(
                    "cannot tell whether the published version is newer than the installed one for '{target}' (non-semver version)"
                ),
            };
            CliError::InvalidArgument {
                command,
                reason: format!(
                    "{detail}; refusing to replace it to avoid an accidental downgrade"
                ),
            }
        }
        PlanError::NeedsAttention => CliError::InvalidArgument {
            command,
            reason: format!(
                "the record for '{target}' was quarantined by the state migration; run `anolisa repair {target}` to resolve it"
            ),
        },
        PlanError::PendingOperation => CliError::Runtime {
            command,
            reason: format!(
                "a previous operation on '{target}' is pending recovery; run `anolisa repair {target}` before retrying"
            ),
        },
        PlanError::PackageUnresolved => CliError::Runtime {
            command,
            reason: format!(
                "the record for '{target}' has no resolved package name; run `anolisa repair {target}` first"
            ),
        },
        other => CliError::InvalidArgument {
            command,
            reason: format!("cannot update '{target}': {other:?}"),
        },
    }
}

/// Map an owned-executor failure to a CLI error that reports honestly what
/// happened to the host: cleanly restored, partially restored, or untouched.
fn owned_error_to_cli(
    err: OwnedExecutionError,
    target: &str,
    scope: InstallationScope,
    command: &str,
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
                format!("update of '{target}' failed at '{at}': {source}; the host was not changed")
            } else if rollback_warnings.is_empty() {
                format!(
                    "update of '{target}' failed at '{at}': {source}; the previous files were restored"
                )
            } else {
                format!(
                    "update of '{target}' failed at '{at}': {source}; restoring the previous files reported problems ({}) — run `{repair}`",
                    rollback_warnings.join("; ")
                )
            }
        }
        OwnedExecutionError::RecoveryUncertain { detail, .. } => {
            format!("update of '{target}' failed: {detail}; run `{repair}`")
        }
        other => format!("update of '{target}' failed: {other}"),
    };
    CliError::Runtime {
        command: command.to_string(),
        reason,
    }
}

/// Human-facing label for a plan step (preview rendering).
fn step_label(step: &Step) -> String {
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
        Step::BackupFiles => "back up current files".to_string(),
        Step::PlaceFiles => "place files".to_string(),
        Step::SetCapabilities => "apply file capabilities".to_string(),
        Step::RestartServices => "restart services".to_string(),
        Step::RemoveOwnedFiles => "remove owned files".to_string(),
        other => format!("{other:?}"),
    }
}

/// Actionable "rpm/dnf tooling missing" error: a delegated component cannot
/// update without the native package manager.
fn tooling_missing_err(command: &str, bin: &str, target: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "cannot update '{target}': {bin} not found on PATH — install the required RPM tooling and retry"
        ),
    }
}

/// Shared completion for a committed delegated update: the durable
/// operation record plus the contract snapshot refresh, in a crash-safe
/// order.
///
/// The journal already settled inside the executor, so `operation` is the
/// only durable signal while the refresh is in flight: it is persisted as
/// `partial` *before* the refresh and promoted to `ok` only after the
/// snapshot and provenance published (and the promotion itself persisted).
/// An exit mid-refresh therefore leaves a discoverable `partial` record
/// instead of a silently stale snapshot — the same pessimistic pattern
/// `repair` uses for its manifest reconciliation. When the pessimistic
/// record itself cannot be persisted the refresh is skipped entirely:
/// mutating the snapshot without the durable signal would reopen that
/// window.
///
/// A package that publishes no contract stays a success without new
/// warnings ([`ContractRefreshOutcome`] reports `NotApplicable`); an
/// unreadable contract or a failed snapshot/provenance write keeps the
/// operation `partial` and returns the failure for the result and central
/// log.
///
/// Both the single-component path and the merged `update all` members run
/// this after their record refresh persisted, so `status`, `doctor`, and
/// adapter resolution read the post-update contract without waiting for a
/// later `repair` or `upgrade` reconciliation.
///
/// [`ContractRefreshOutcome`]: super::install::ContractRefreshOutcome
#[expect(clippy::too_many_arguments)]
pub(crate) fn complete_delegated_update(
    layout: &FsLayout,
    ctx: &CliContext,
    target: &str,
    package: &str,
    command: &str,
    store: &mut StateStore,
    state_path: &Path,
    operation: OperationRecord,
) -> Option<String> {
    // Pessimistic durable status first, as a hard gate: the record is the
    // only durable signal while the refresh mutates the snapshot, so if it
    // cannot be persisted the refresh must not run — proceeding anyway
    // would reopen the crash window this completion exists to close.
    let mut operation = operation;
    let operation_id = operation.id.clone();
    operation.status = "partial".to_string();
    store.operations.push(operation);
    if let Err(err) = store.save(state_path) {
        return Some(format!(
            "component manifest refresh was skipped because the operation record could not be persisted first: {err}"
        ));
    }

    let refresh =
        refresh_datadir_contract_snapshot(layout, target, command, ctx.packaged_data_probe());
    if !ctx.quiet {
        for warning in &refresh.warnings {
            eprintln!("warning: {warning}");
        }
    }
    if let Some(detail) = refresh.error_detail() {
        return Some(format!(
            "component manifest refresh for package '{package}' did not complete: {detail}"
        ));
    }

    // Promote by id, not by position: the pessimistic record was pushed
    // above, but the promotion must never depend on it still being the
    // last entry.
    if let Some(operation) = store
        .operations
        .iter_mut()
        .rfind(|operation| operation.id == operation_id)
    {
        operation.status = "ok".to_string();
    }
    if let Err(err) = store.save(state_path) {
        // The durable status must never overstate what happened: an
        // unpersisted promotion keeps the operation partial.
        return Some(format!(
            "component manifest refresh completed, but the operation could not be finalized as ok: {err}"
        ));
    }
    None
}

/// Best-effort central-log record for a committed update.
///
/// A `completion_failure` (delegated contract refresh not completed) logs
/// `Partial` at `Warn` severity so `logs --severity warn` surfaces the
/// operations that still need a repair, matching repair/upgrade.
#[expect(clippy::too_many_arguments)]
pub(crate) fn append_update_log(
    layout: &FsLayout,
    ctx: &CliContext,
    component: &str,
    command: &str,
    operation_id: &str,
    started_at: &str,
    package: &str,
    to_version: Option<&str>,
    completion_failure: Option<&str>,
) {
    let log = CentralLog::open(layout.central_log.clone());
    let base = match to_version {
        Some(version) => format!("updated component {component} ({package}) to {version}"),
        None => format!("updated component {component} ({package})"),
    };
    let record = LogRecord {
        kind: LogKind::Operation,
        operation_id: Some(operation_id.to_string()),
        command: command.to_string(),
        source: "anolisa-cli".to_string(),
        component: Some(component.to_string()),
        severity: match completion_failure {
            Some(_) => Severity::Warn,
            None => Severity::Info,
        },
        message: match completion_failure {
            Some(_) => format!("{base}, but the component manifest refresh did not complete"),
            None => base,
        },
        actor: "cli".to_string(),
        install_mode: Some(ctx.install_mode.as_str().to_string()),
        started_at: started_at.to_string(),
        finished_at: Some(now_iso8601()),
        status: Some(match completion_failure {
            Some(_) => LogStatus::Partial,
            None => LogStatus::Ok,
        }),
        objects: vec![component.to_string()],
        backup_ids: Vec::new(),
        warnings: completion_failure
            .map(|failure| vec![failure.to_string()])
            .unwrap_or_default(),
        details: serde_json::Value::Null,
    };
    if let Err(err) = log.append(&record) {
        eprintln!("warning: failed to write central log: {err}");
    }
}

fn render_application_outcome(
    ctx: &CliContext,
    application_outcome: application::ApplicationOutcome,
) -> ComponentUpdateResult {
    for warning in application_outcome.warnings() {
        eprintln!("warning: {warning}");
    }
    let batch_outcome = match application_outcome.batch_outcome() {
        application::UpdateBatchOutcome::Completed(outcome) => outcome,
        application::UpdateBatchOutcome::Partial { reason }
        | application::UpdateBatchOutcome::Failed { reason } => {
            let application::ApplicationOutcome::Applied { command, .. } = &application_outcome
            else {
                unreachable!("only applied updates have a terminal failure")
            };
            return Err(CliError::Runtime {
                command: command.clone(),
                reason,
            });
        }
    };
    match application_outcome {
        application::ApplicationOutcome::NoOp { subject } => {
            render_result(
                ctx,
                &subject.component,
                subject.package.as_deref(),
                subject.from_version.as_deref(),
                subject.to_version.as_deref(),
                false,
                ctx.dry_run,
                &[],
                None,
                &[],
            )?;
            Ok((batch_outcome, Vec::new()))
        }
        application::ApplicationOutcome::Preview { subject, steps } => {
            let plan_labels: Vec<String> = steps.iter().map(step_label).collect();
            render_result(
                ctx,
                &subject.component,
                subject.package.as_deref(),
                subject.from_version.as_deref(),
                subject.to_version.as_deref(),
                false,
                true,
                &plan_labels,
                None,
                &[],
            )?;
            Ok((batch_outcome, Vec::new()))
        }
        application::ApplicationOutcome::Applied {
            command: _,
            subject,
            steps,
            outcome,
            adapter_actions,
        } => {
            let updated = matches!(batch_outcome, UpdateOutcome::Updated);
            let plan_labels: Vec<String> = steps.iter().map(step_label).collect();
            render_result(
                ctx,
                &subject.component,
                subject.package.as_deref(),
                subject.from_version.as_deref(),
                subject.to_version.as_deref(),
                updated,
                false,
                &plan_labels,
                outcome.operation_id(),
                &adapter_actions,
            )?;
            Ok((batch_outcome, adapter_actions))
        }
    }
}

/// Render the update result (or its dry-run preview). `adapter_actions`
/// lists follow-up from a committed update (issue #1885); `--quiet`
/// suppresses the human notices while `--json` always carries them.
#[expect(clippy::too_many_arguments)]
fn render_result(
    ctx: &CliContext,
    component: &str,
    package: Option<&str>,
    from_version: Option<&str>,
    to_version: Option<&str>,
    updated: bool,
    dry_run: bool,
    plan_labels: &[String],
    operation_id: Option<&str>,
    adapter_actions: &[AdapterAction],
) -> Result<(), CliError> {
    if ctx.json {
        return response::render_json(
            COMMAND,
            UpdateResultPayload {
                component: component.to_string(),
                package: package.map(str::to_string),
                from_version: from_version.map(str::to_string),
                to_version: to_version.map(str::to_string),
                updated,
                dry_run,
                plan: plan_labels.to_vec(),
                operation_id: operation_id.map(str::to_string),
                adapter_actions: adapter_actions.to_vec(),
            },
        );
    }
    if ctx.quiet {
        return Ok(());
    }
    let color = Palette::new(ctx.no_color);
    // A no-op "already latest" reads the same whether previewed or run.
    if !updated && plan_labels.is_empty() {
        println!(
            "{} {component} is already up to date{}",
            color.ok("✓"),
            from_version.map(|v| format!(" ({v})")).unwrap_or_default(),
        );
        return Ok(());
    }
    if dry_run {
        println!(
            "{} {component} {}",
            color.command("update"),
            color.muted("(dry-run — nothing updated)"),
        );
        match (from_version, to_version) {
            (Some(from), Some(to)) => {
                println!("{} {from} → {to}", color.label("would update:"));
            }
            (Some(from), None) => println!("{} {from}", color.label("current:")),
            _ => {}
        }
        for label in plan_labels {
            println!("  - {label}");
        }
        return Ok(());
    }
    if updated {
        println!(
            "{} {component} {} → {}",
            color.ok("✓ updated"),
            from_version.unwrap_or("-"),
            to_version.unwrap_or("-"),
        );
    } else {
        println!(
            "{} {component} is already up to date{}",
            color.ok("✓"),
            from_version.map(|v| format!(" ({v})")).unwrap_or_default(),
        );
    }
    if let Some(id) = operation_id {
        println!("{} {}", color.label("operation_id:"), color.id(id));
    }
    render_adapter_action_notices(adapter_actions, &color);
    Ok(())
}

/// RFC3339 UTC timestamp, seconds precision (matches the install path).
pub(crate) fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    use anolisa_core::execution::{CommandOutcomeStatus, ExecutionIntent};
    use anolisa_core::self_update::{self, ProgressFn};
    use anolisa_platform::pkg_query::PackageVersion;
    use anolisa_platform::pkg_transaction::PackageTransactionError;

    use super::self_update::application::{
        SelfUpdateApplicationOutcome, SelfUpdateApplied, SelfUpdateChange, SelfUpdateExecution,
        SelfUpdateFailure, SelfUpdateFailureContext, SelfUpdateOps, SelfUpdateRequest,
        append_self_update_log as append_self_update_log_with_intent, redact_known_urls,
        run_application_with_deps, run_self_update_with_deps as run_self_update_with_intent,
    };

    #[allow(clippy::too_many_arguments)]
    fn run_self_update_with_deps(
        endpoint_url: &str,
        current_version: &str,
        ctx: &CliContext,
        ops: &dyn SelfUpdateOps,
        query: &dyn PackageQuery,
        txn: &dyn PackageTransaction,
        is_root: bool,
        on_progress: Option<&ProgressFn>,
    ) -> Result<SelfUpdateExecution, Box<SelfUpdateFailure>> {
        let intent = if ctx.dry_run {
            ExecutionIntent::Plan
        } else {
            ExecutionIntent::Apply
        };
        run_self_update_with_intent(
            endpoint_url,
            current_version,
            intent,
            ops,
            query,
            txn,
            is_root,
            on_progress,
        )
    }

    fn append_self_update_log(
        ctx: &CliContext,
        started_at: &str,
        outcome: Result<&SelfUpdateExecution, &SelfUpdateFailure>,
    ) {
        let intent = if ctx.dry_run {
            ExecutionIntent::Plan
        } else {
            ExecutionIntent::Apply
        };
        let _ = append_self_update_log_with_intent(ctx, started_at, intent, outcome);
    }

    /// Read the single central-log record `append_self_update_log` wrote,
    /// asserting the log holds exactly one line.
    fn only_self_update_record(ctx: &CliContext) -> serde_json::Value {
        let raw = std::fs::read_to_string(&common::resolve_layout(ctx).central_log)
            .expect("read central log");
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 1, "expected exactly one record, got {lines:?}");
        serde_json::from_str(lines[0]).expect("parse record")
    }

    fn central_log_is_absent(ctx: &CliContext) -> bool {
        !common::resolve_layout(ctx).central_log.exists()
    }

    /// Issue #2992: an applied binary self-update must be auditable, and the
    /// record must carry the version transition it applied.
    #[test]
    fn binary_self_update_records_the_version_transition() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let run = SelfUpdateExecution::Applied(SelfUpdateApplied::Binary {
            from: "0.1.0".into(),
            to: "0.2.0".into(),
        });

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Ok(&run));

        let record = only_self_update_record(&ctx);
        assert_eq!(record["kind"], "operation");
        assert_eq!(record["command"], "update self");
        assert_eq!(record["source"], "anolisa-cli");
        assert_eq!(record["status"], "ok");
        assert_eq!(record["severity"], "info");
        assert_eq!(record["started_at"], "2026-06-01T10:00:00Z");
        assert!(record["finished_at"].is_string());
        assert!(
            record["operation_id"]
                .as_str()
                .expect("operation id")
                .starts_with("op-update-self-"),
            "operation id must self-classify: {}",
            record["operation_id"]
        );
        // The versions are the point of the record: they must be recoverable
        // without re-parsing the human message.
        assert_eq!(record["details"]["current_version"], "0.1.0");
        assert_eq!(record["details"]["latest_version"], "0.2.0");
        assert_eq!(record["details"]["apply_mode"], "binary");
        assert_eq!(record["details"]["updated"], true);
        let message = record["message"].as_str().expect("message");
        assert!(
            message.contains("0.1.0") && message.contains("0.2.0"),
            "message must state the transition: {message}"
        );
    }

    /// The RPM path delegates to dnf, so the package it moved and the versions
    /// rpm actually reports must both be recorded.
    #[test]
    fn rpm_self_update_records_package_and_observed_versions() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let run = SelfUpdateExecution::Applied(SelfUpdateApplied::RpmPackage {
            from: "0.1.0".into(),
            to: "0.2.0".into(),
            package: "anolisa".to_string(),
            before_version: Some("0.1.0".to_string()),
            after_version: Some("0.2.0".to_string()),
        });

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Ok(&run));

        let record = only_self_update_record(&ctx);
        assert_eq!(record["status"], "ok");
        assert_eq!(record["severity"], "info");
        // `objects` carries the RPM package exactly like `anolisa upgrade`
        // records a CLI package update, so `logs anolisa` finds it.
        assert_eq!(record["objects"][0], "anolisa");
        assert_eq!(record["details"]["apply_mode"], "rpm_package");
        assert_eq!(record["details"]["package"], "anolisa");
        assert_eq!(record["details"]["rpm_version_before"], "0.1.0");
        assert_eq!(record["details"]["rpm_version_after"], "0.2.0");
    }

    /// dnf treats a package already at the latest version as a successful
    /// no-op, and one object cannot be partly applied — so this is `ok`, and
    /// must not reach the warn/error set `anolisa bug` collects.
    #[test]
    fn rpm_self_update_that_did_not_move_the_version_is_ok_not_a_warning() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let run = SelfUpdateExecution::Applied(SelfUpdateApplied::RpmPackage {
            from: "0.1.0".into(),
            to: "0.2.0".into(),
            package: "anolisa".to_string(),
            before_version: Some("0.1.0".to_string()),
            after_version: Some("0.1.0".to_string()),
        });

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Ok(&run));

        let record = only_self_update_record(&ctx);
        assert_eq!(record["status"], "ok");
        assert_eq!(record["severity"], "info");
        // The record still states plainly that nothing moved.
        assert_eq!(record["details"]["updated"], false);
        assert_eq!(record["details"]["rpm_version_after"], "0.1.0");

        // `anolisa bug` collects `warn` and above; a successful no-op is not a
        // diagnostic finding and must stay out of that set.
        let diagnostics = CentralLog::open(common::resolve_layout(&ctx).central_log)
            .query(&anolisa_core::LogFilter {
                severity_at_least: Some(Severity::Warn),
                ..Default::default()
            })
            .expect("query central log");
        assert!(
            diagnostics.is_empty(),
            "a successful dnf no-op must not surface as a diagnostic: {diagnostics:?}"
        );
    }

    /// A failed dnf delegation is the case with the most context to lose, so
    /// it is driven through the real run: the record must keep the package,
    /// the target version, and the version rpm reports after the failure.
    #[test]
    fn failed_dnf_self_update_records_package_and_observed_versions() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa");
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"])
            .with_installed_versions("anolisa", vec![Some("0.1.0"), Some("0.1.0")]);
        let txn = FakeSelfTxn::new("anolisa").failing();

        let failure = run_self_update_with_deps(
            "https://example.invalid/release-manifest.toml",
            "0.1.0",
            &ctx,
            &ops,
            &query,
            &txn,
            true,
            None,
        )
        .expect_err("a failed dnf transaction must not report success");

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Err(&failure));

        assert_eq!(
            query.installed_queries.get(),
            2,
            "the installed version must be re-observed after dnf failed"
        );
        let record = only_self_update_record(&ctx);
        assert_eq!(record["status"], "failed");
        assert_eq!(record["severity"], "error");
        // Everything the run had learned survives into the record.
        assert_eq!(record["details"]["current_version"], "0.1.0");
        assert_eq!(record["details"]["latest_version"], "0.2.0");
        assert_eq!(record["details"]["apply_mode"], "rpm_package");
        assert_eq!(record["details"]["package"], "anolisa");
        assert_eq!(record["details"]["rpm_version_before"], "0.1.0");
        assert_eq!(record["details"]["rpm_version_after"], "0.1.0");
        // Observed on both sides and unchanged, so `updated` is a reading, not
        // an assumption.
        assert_eq!(record["details"]["updated"], false);
        // `logs anolisa` must reach the failure, not just the applied record.
        assert_eq!(record["objects"][0], "anolisa");
        let message = record["message"].as_str().expect("message");
        assert!(
            message.contains("dnf"),
            "message must carry the failure reason: {message}"
        );
    }

    /// A non-zero dnf exit does not mean the rpmdb stayed put. When the
    /// re-observation shows the version did move, the record must say so
    /// instead of contradicting the versions printed beside it.
    #[test]
    fn failed_dnf_that_still_moved_the_version_records_it_as_updated() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa");
        // dnf fails, but rpm reports a different version afterwards.
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"])
            .with_installed_versions("anolisa", vec![Some("0.1.0"), Some("0.2.0")]);
        let txn = FakeSelfTxn::new("anolisa").failing();

        let failure = run_self_update_with_deps(
            "https://example.invalid/release-manifest.toml",
            "0.1.0",
            &ctx,
            &ops,
            &query,
            &txn,
            true,
            None,
        )
        .expect_err("a failed dnf transaction must not report success");

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Err(&failure));

        let record = only_self_update_record(&ctx);
        assert_eq!(record["status"], "failed");
        assert_eq!(record["details"]["rpm_version_before"], "0.1.0");
        assert_eq!(record["details"]["rpm_version_after"], "0.2.0");
        assert_eq!(
            record["details"]["updated"], true,
            "a moved rpmdb must not be recorded as `updated: false`"
        );
    }

    /// A failed binary swap knows its target version even though nothing was
    /// applied; the record must not throw that away.
    #[test]
    fn failed_binary_self_update_records_the_target_version() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa").failing_binary_update();
        let query = FakeSelfQuery::new("/usr/bin/anolisa", Vec::new());
        let txn = FakeSelfTxn::new("anolisa");

        let failure = run_self_update_with_deps(
            "https://example.invalid/release-manifest.toml",
            "0.1.0",
            &ctx,
            &ops,
            &query,
            &txn,
            false,
            None,
        )
        .expect_err("a failed binary swap must not report success");

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Err(&failure));

        let record = only_self_update_record(&ctx);
        assert_eq!(record["status"], "failed");
        assert_eq!(record["severity"], "error");
        assert_eq!(record["details"]["current_version"], "0.1.0");
        assert_eq!(record["details"]["latest_version"], "0.2.0");
        assert_eq!(record["details"]["apply_mode"], "binary");
        // Nothing was observed on this path — a binary swap can fail with the
        // executable in an uncertain state — so `updated` is absent rather
        // than a definitive claim.
        assert!(record["details"].get("updated").is_none());
        // No RPM package is involved, so those fields stay absent rather than
        // being recorded as unknown.
        assert!(record["details"].get("package").is_none());
        assert!(record["objects"].as_array().expect("objects").is_empty());
        let message = record["message"].as_str().expect("message");
        assert!(
            message.contains("sha256 mismatch"),
            "message must carry the failure reason: {message}"
        );
    }

    /// A manifest that never resolved leaves only the running version, and the
    /// record must say exactly that much rather than inventing a target.
    #[test]
    fn failed_self_update_before_the_manifest_records_only_what_is_known() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let failure = SelfUpdateFailure {
            error: CliError::Runtime {
                command: "update self".to_string(),
                reason: "failed to fetch release manifest".to_string(),
            },
            context: SelfUpdateFailureContext::new("0.1.0", "https://example.invalid/m.toml"),
        };

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Err(&failure));

        let record = only_self_update_record(&ctx);
        assert_eq!(record["status"], "failed");
        assert_eq!(record["details"]["current_version"], "0.1.0");
        assert!(record["details"].get("updated").is_none());
        assert!(record["details"].get("latest_version").is_none());
        assert!(record["details"].get("apply_mode").is_none());
    }

    /// `ANOLISA_UPDATE_URL` is operator-supplied and the fetch error quotes it
    /// verbatim, so a mirror carrying credentials must not reach the log the
    /// `anolisa bug` bundle is built from.
    ///
    /// Driven through the real run path. The endpoints deliberately hold
    /// characters that break a scan of the message — quotes, a newline,
    /// non-ASCII — and delimiters inside the password that end the authority
    /// early, which is the case that must fail closed rather than pass through.
    ///
    /// Assertions run on the *decoded* record, not the raw JSONL: serialisation
    /// escapes `"` and a newline, so a check against the raw bytes would pass
    /// while the credential is sitting in the file.
    #[test]
    fn failed_self_update_does_not_persist_endpoint_credentials() {
        let secrets = [
            "s3cr3t-token",
            "s3'cr3t",
            "s3\"cr3t",
            "s3\ncr3t",
            "s3\u{4e2d}cr3t",
            "s3/cr3t",
            "s3?cr3t",
            "s3#cr3t",
            "p@ss!w$rd",
            "deadbeef",
            "dead'beef",
            "a(b)c*d",
        ];
        for endpoint in [
            "https://ci:s3cr3t-token@mirror.invalid/release.toml?X-Amz-Signature=deadbeef",
            "https://ci:s3'cr3t@mirror.invalid/release.toml?X-Amz-Signature=dead'beef",
            "https://ci:s3\"cr3t@mirror.invalid/release.toml?X-Amz-Signature=deadbeef",
            "https://ci:s3\ncr3t@mirror.invalid/release.toml?X-Amz-Signature=deadbeef",
            "https://ci:s3\u{4e2d}cr3t@mirror.invalid/release.toml?X-Amz-Signature=deadbeef",
            // Delimiters inside the userinfo end the authority early: these
            // must fail closed, not slip through unredacted.
            "https://ci:s3/cr3t@mirror.invalid/release.toml?X-Amz-Signature=deadbeef",
            "https://ci:s3?cr3t@mirror.invalid/release.toml",
            "https://ci:s3#cr3t@mirror.invalid/release.toml",
            "https://ci:p@ss!w$rd&x=1@mirror.invalid/release.toml?sig=a(b)c*d,e;f=g",
            "https://ci:s3%27cr3t@mirror.invalid/release.toml?X-Amz-Signature=deadbeef",
            // Not a URL at all: unparseable input must not leak either.
            "\u{4e2d}https://ci:s3cr3t-token@mirror.invalid/release.toml",
        ] {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let ctx = self_ctx(tmp.path().to_path_buf(), false);
            let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa").failing_check_update(endpoint);
            let query = FakeSelfQuery::new("/usr/bin/anolisa", Vec::new());
            let txn = FakeSelfTxn::new("anolisa");

            let failure =
                run_self_update_with_deps(endpoint, "0.1.0", &ctx, &ops, &query, &txn, false, None)
                    .expect_err("an unreachable manifest endpoint must not report success");

            append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Err(&failure));

            // Decode first: escaping would hide a leaked `"` or newline.
            let record = only_self_update_record(&ctx);
            let decoded = serde_json::to_string(&record).expect("re-encode");
            let message = record["message"].as_str().expect("message");
            for secret in secrets {
                assert!(
                    !message.contains(secret),
                    "credential leaked into the message for {endpoint}: {secret}"
                );
                assert!(
                    !record["details"].to_string().contains(secret),
                    "credential leaked into details for {endpoint}: {secret}"
                );
                assert!(
                    !decoded.contains(secret),
                    "credential leaked into the record for {endpoint}: {secret}"
                );
            }
        }
    }

    /// The endpoint stays recorded in a form that carries no credentials, so a
    /// withheld failure text still leaves the operator knowing what was
    /// contacted. Scheme and host only: a path segment can hold a bearer token
    /// as readily as userinfo can.
    #[test]
    fn failed_self_update_records_the_endpoint_without_credentials() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let endpoint = "https://ci:s3cr3t-token@mirror.invalid/release.toml?sig=deadbeef";
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa").failing_check_update(endpoint);
        let query = FakeSelfQuery::new("/usr/bin/anolisa", Vec::new());
        let txn = FakeSelfTxn::new("anolisa");

        let failure =
            run_self_update_with_deps(endpoint, "0.1.0", &ctx, &ops, &query, &txn, false, None)
                .expect_err("an unreachable manifest endpoint must not report success");

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Err(&failure));

        let record = only_self_update_record(&ctx);
        assert_eq!(record["details"]["endpoint"], "https://mirror.invalid");
    }

    /// A transport error is free to quote the URL in a form this run never
    /// handled — percent-encoded, normalised, or a redirect target — which a
    /// literal replacement cannot catch. Such a message must be withheld.
    #[test]
    fn a_message_with_an_unrecognised_credential_url_is_withheld() {
        let known = "https://mirror.invalid/release.toml";
        // The transport re-encoded the password, so the known value no longer
        // matches what the text carries.
        let text = "failed: https://ci:s3%22cr3t@mirror.invalid/release.toml: Dns Failed";

        let out = redact_known_urls(text, &[known.to_string()]);

        assert!(
            !out.contains("s3%22cr3t"),
            "a re-encoded credential must not survive: {out}"
        );
        assert!(
            out.contains("withheld"),
            "the text must be withheld rather than partly redacted: {out}"
        );
    }

    /// Withholding is the fallback, not the rule: an ordinary failure keeps its
    /// transport detail so the record stays useful.
    #[test]
    fn a_message_without_credentials_keeps_its_transport_detail() {
        let known = "https://ci:tok@mirror.invalid/release.toml?sig=abc";
        let text = "failed to fetch release manifest from \
                    https://ci:tok@mirror.invalid/release.toml?sig=abc: Dns Failed: not known";

        let out = redact_known_urls(text, &[known.to_string()]);

        assert!(!out.contains("tok"), "userinfo must go: {out}");
        assert!(!out.contains("abc"), "query must go: {out}");
        assert!(
            !out.contains("release.toml"),
            "the path leaves with the rest of the URL: {out}"
        );
        assert!(
            out.contains("Dns Failed: not known"),
            "the transport reason must survive: {out}"
        );
    }

    /// Replacement matches a literal value, so a query the transport extended
    /// leaves behind whatever the known value did not cover. Redacting the run
    /// out to the next whitespace takes the remainder with it.
    #[test]
    fn a_query_extended_past_the_known_value_is_swallowed() {
        let known = "https://mirror.invalid/release.toml?sig=abc";
        let text = "failed to fetch release manifest from \
                    https://mirror.invalid/release.toml?sig=abcSECRET: Dns Failed";

        let out = redact_known_urls(text, &[known.to_string()]);

        assert!(
            !out.contains("SECRET"),
            "the appended parameter must not survive: {out}"
        );
        assert!(
            out.contains("Dns Failed"),
            "swallowing the run must not cost the transport reason: {out}"
        );
    }

    /// A path segment carries a bearer token as readily as userinfo does, and
    /// nothing about the string says which segment is which. Neither the text
    /// nor the recorded endpoint may keep it.
    #[test]
    fn a_credential_in_the_url_path_is_not_persisted() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let endpoint = "https://mirror.invalid/download/bearer-PATHTOKEN/release.toml";
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa").failing_check_update(endpoint);
        let query = FakeSelfQuery::new("/usr/bin/anolisa", Vec::new());
        let txn = FakeSelfTxn::new("anolisa");

        let failure =
            run_self_update_with_deps(endpoint, "0.1.0", &ctx, &ops, &query, &txn, false, None)
                .expect_err("an unreachable manifest endpoint must not report success");

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Err(&failure));

        let record = only_self_update_record(&ctx);
        let rendered = record.to_string();
        assert!(
            !rendered.contains("PATHTOKEN"),
            "a path credential must not reach the record: {rendered}"
        );
        assert_eq!(record["details"]["endpoint"], "https://mirror.invalid");
    }

    /// An up-to-date run changed nothing, so it is not an operation to audit.
    #[test]
    fn already_latest_self_update_writes_no_record() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let run = SelfUpdateExecution::AlreadyLatest {
            version: "0.2.0".into(),
        };

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Ok(&run));

        assert!(central_log_is_absent(&ctx), "a no-op must not be audited");
    }

    /// `--dry-run` must leave the log exactly as it found it — including when
    /// the preview itself failed.
    #[test]
    fn dry_run_self_update_writes_no_record() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), true);
        let run = SelfUpdateExecution::Preview {
            from: "0.1.0".into(),
            to: "0.2.0".into(),
        };
        let failure = SelfUpdateFailure {
            error: CliError::Runtime {
                command: "update self".to_string(),
                reason: "release manifest unreachable".to_string(),
            },
            context: SelfUpdateFailureContext::new("0.1.0", "https://example.invalid/m.toml"),
        };

        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Ok(&run));
        append_self_update_log(&ctx, "2026-06-01T10:00:00Z", Err(&failure));

        assert!(
            central_log_is_absent(&ctx),
            "a dry-run preview must never write"
        );
    }

    fn self_manifest(version: &str) -> self_update::ReleaseManifest {
        self_update::ReleaseManifest {
            schema_version: 1,
            version: version.to_string(),
            artifacts: vec![self_update::ReleaseArtifact {
                os: self_update::current_os().to_string(),
                arch: self_update::current_arch().to_string(),
                url: "https://example.invalid/anolisa.tar.gz".to_string(),
                sha256: "0".repeat(64),
                size: Some(1),
            }],
        }
    }

    fn package_info(name: &str, version: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: PackageVersion {
                epoch: None,
                version: version.to_string(),
                release: None,
            },
            arch: "x86_64".to_string(),
            origin: None,
        }
    }

    fn self_ctx(prefix: PathBuf, dry_run: bool) -> CliContext {
        crate::test_support::context_for_root(
            &prefix,
            crate::context::InstallMode::System,
            Some(prefix.clone()),
            crate::test_support::TestContextOptions {
                dry_run,
                ..Default::default()
            },
        )
    }

    struct FakeSelfUpdateOps {
        manifest: Option<self_update::ReleaseManifest>,
        current_exe: PathBuf,
        resolved_executables: Cell<usize>,
        binary_updates: Cell<usize>,
        binary_update_fails: bool,
        check_update_error: Option<self_update::SelfUpdateError>,
    }

    impl FakeSelfUpdateOps {
        fn new(current_exe: &str) -> Self {
            Self {
                manifest: Some(self_manifest("0.2.0")),
                current_exe: PathBuf::from(current_exe),
                resolved_executables: Cell::new(0),
                binary_updates: Cell::new(0),
                binary_update_fails: false,
                check_update_error: None,
            }
        }

        /// The manifest request fails against `endpoint`, reproducing the real
        /// `FetchManifest` text: it quotes the endpoint, and so does the
        /// transport error nested in it.
        fn failing_check_update(mut self, endpoint: &str) -> Self {
            self.check_update_error = Some(self_update::SelfUpdateError::FetchManifest {
                url: endpoint.to_string(),
                reason: format!("{endpoint}: Dns Failed: resolve dns name"),
            });
            self
        }

        /// The binary swap fails after the manifest resolved, so the run knows
        /// its target version but never applies it.
        fn failing_binary_update(mut self) -> Self {
            self.binary_update_fails = true;
            self
        }
    }

    impl SelfUpdateOps for FakeSelfUpdateOps {
        fn check_update(
            &self,
            _endpoint_url: &str,
            _current_version: &str,
        ) -> Result<Option<self_update::ReleaseManifest>, self_update::SelfUpdateError> {
            if let Some(err) = &self.check_update_error {
                return Err(match err {
                    self_update::SelfUpdateError::FetchManifest { url, reason } => {
                        self_update::SelfUpdateError::FetchManifest {
                            url: url.clone(),
                            reason: reason.clone(),
                        }
                    }
                    other => self_update::SelfUpdateError::ParseManifest(other.to_string()),
                });
            }
            Ok(self.manifest.clone())
        }

        fn resolve_current_exe(&self) -> Result<PathBuf, self_update::SelfUpdateError> {
            self.resolved_executables
                .set(self.resolved_executables.get() + 1);
            Ok(self.current_exe.clone())
        }

        fn perform_binary_update(
            &self,
            _artifact: &self_update::ReleaseArtifact,
            current_exe: &Path,
            _on_progress: Option<&ProgressFn>,
        ) -> Result<(), self_update::SelfUpdateError> {
            assert_eq!(current_exe, self.current_exe.as_path());
            self.binary_updates.set(self.binary_updates.get() + 1);
            if self.binary_update_fails {
                return Err(self_update::SelfUpdateError::ChecksumMismatch {
                    expected: "aaaa".to_string(),
                    actual: "bbbb".to_string(),
                });
            }
            Ok(())
        }
    }

    struct FakeSelfQuery {
        expected_capability: String,
        providers: FakeSelfProviders,
        queries: Cell<usize>,
        expected_installed_package: Option<String>,
        installed_versions: RefCell<VecDeque<Option<String>>>,
        installed_queries: Cell<usize>,
    }

    enum FakeSelfProviders {
        Packages(Vec<String>),
        CommandMissing,
    }

    impl FakeSelfQuery {
        fn new(expected_capability: &str, providers: Vec<&str>) -> Self {
            Self {
                expected_capability: expected_capability.to_string(),
                providers: FakeSelfProviders::Packages(
                    providers.into_iter().map(str::to_string).collect(),
                ),
                queries: Cell::new(0),
                expected_installed_package: None,
                installed_versions: RefCell::new(VecDeque::new()),
                installed_queries: Cell::new(0),
            }
        }

        fn missing_rpm(expected_capability: &str) -> Self {
            Self {
                expected_capability: expected_capability.to_string(),
                providers: FakeSelfProviders::CommandMissing,
                queries: Cell::new(0),
                expected_installed_package: None,
                installed_versions: RefCell::new(VecDeque::new()),
                installed_queries: Cell::new(0),
            }
        }

        fn with_installed_versions(
            mut self,
            expected_package: &str,
            versions: Vec<Option<&str>>,
        ) -> Self {
            self.expected_installed_package = Some(expected_package.to_string());
            self.installed_versions = RefCell::new(
                versions
                    .into_iter()
                    .map(|version| version.map(str::to_string))
                    .collect(),
            );
            self
        }
    }

    impl PackageQuery for FakeSelfQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            self.installed_queries.set(self.installed_queries.get() + 1);
            if let Some(expected_package) = &self.expected_installed_package {
                assert_eq!(package, expected_package);
            }
            Ok(self
                .installed_versions
                .borrow_mut()
                .pop_front()
                .flatten()
                .map(|version| package_info(package, &version)))
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }

        fn what_provides_installed(
            &self,
            capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            self.queries.set(self.queries.get() + 1);
            assert_eq!(capability, self.expected_capability);
            match &self.providers {
                FakeSelfProviders::Packages(providers) => Ok(providers.clone()),
                FakeSelfProviders::CommandMissing => Err(PackageQueryError::CommandMissing {
                    command: "rpm".to_string(),
                }),
            }
        }
    }

    struct FakeSelfTxn {
        expected_package: String,
        update_calls: Cell<usize>,
        update_fails: bool,
    }

    impl FakeSelfTxn {
        fn new(expected_package: &str) -> Self {
            Self {
                expected_package: expected_package.to_string(),
                update_calls: Cell::new(0),
                update_fails: false,
            }
        }

        /// dnf refuses the transaction, leaving the installed version to be
        /// re-observed rather than assumed.
        fn failing(mut self) -> Self {
            self.update_fails = true;
            self
        }
    }

    impl PackageTransaction for FakeSelfTxn {
        fn check_install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("this path must not preflight an install")
        }

        fn install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("self-update must not run dnf install");
        }

        fn update(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            let &[package] = packages else {
                panic!("expected exactly one package, got {packages:?}");
            };
            self.update_calls.set(self.update_calls.get() + 1);
            assert_eq!(package, self.expected_package);
            if self.update_fails {
                return Err(PackageTransactionError::TransactionFailed {
                    command: "dnf".to_string(),
                    operation: "update".to_string(),
                    code: Some(1),
                    stderr: "Error: Transaction test error".to_string(),
                });
            }
            Ok(())
        }

        fn reinstall(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("self-update must not run dnf reinstall");
        }

        fn remove(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("self-update must not run dnf remove");
        }
    }

    #[test]
    fn self_update_preview_stops_before_every_apply_dependency() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        // Keep the context in apply mode to prove the typed request, not the
        // legacy context flag, controls the application branch.
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa");
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"]);
        let txn = FakeSelfTxn::new("anolisa");

        let outcome = run_application_with_deps(
            SelfUpdateRequest {
                endpoint_url: "https://example.invalid/release-manifest.toml",
                current_version: "0.1.0",
                intent: ExecutionIntent::Plan,
            },
            &ctx,
            &ops,
            &query,
            &txn,
            true,
            None,
            "2026-06-01T10:00:00Z",
        )
        .expect("preview should succeed");

        assert!(matches!(
            outcome,
            SelfUpdateApplicationOutcome::Preview { from, to }
                if from == "0.1.0" && to == "0.2.0"
        ));
        assert_eq!(ops.resolved_executables.get(), 0);
        assert_eq!(ops.binary_updates.get(), 0);
        assert_eq!(query.queries.get(), 0);
        assert_eq!(query.installed_queries.get(), 0);
        assert_eq!(txn.update_calls.get(), 0);
        assert!(central_log_is_absent(&ctx));
    }

    #[test]
    fn already_latest_application_outcome_has_no_effect_or_audit() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let mut ops = FakeSelfUpdateOps::new("/usr/bin/anolisa");
        ops.manifest = None;
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"]);
        let txn = FakeSelfTxn::new("anolisa");

        let outcome = run_application_with_deps(
            SelfUpdateRequest {
                endpoint_url: "https://example.invalid/release-manifest.toml",
                current_version: "0.1.0",
                intent: ExecutionIntent::Apply,
            },
            &ctx,
            &ops,
            &query,
            &txn,
            true,
            None,
            "2026-06-01T10:00:00Z",
        )
        .expect("already-latest should succeed");

        assert!(matches!(
            outcome,
            SelfUpdateApplicationOutcome::AlreadyLatest { version } if version == "0.1.0"
        ));
        assert_eq!(ops.resolved_executables.get(), 0);
        assert_eq!(query.queries.get(), 0);
        assert_eq!(txn.update_calls.get(), 0);
        assert!(central_log_is_absent(&ctx));
    }

    #[test]
    fn preview_failure_does_not_write_a_failure_audit() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let endpoint = "https://token@example.invalid/secret/release.toml?sig=abc";
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa").failing_check_update(endpoint);
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"]);
        let txn = FakeSelfTxn::new("anolisa");

        let failure = run_application_with_deps(
            SelfUpdateRequest {
                endpoint_url: endpoint,
                current_version: "0.1.0",
                intent: ExecutionIntent::Plan,
            },
            &ctx,
            &ops,
            &query,
            &txn,
            true,
            None,
            "2026-06-01T10:00:00Z",
        )
        .expect_err("manifest failure must remain terminal");

        assert_eq!(failure.error.code(), "EXECUTION_FAILED");
        assert!(failure.warnings.is_empty());
        assert_eq!(ops.resolved_executables.get(), 0);
        assert_eq!(query.queries.get(), 0);
        assert_eq!(txn.update_calls.get(), 0);
        assert!(central_log_is_absent(&ctx));
    }

    #[test]
    fn binary_apply_returns_completed_change_and_durable_audit_id() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/opt/anolisa/bin/anolisa");
        let query = FakeSelfQuery::new("/opt/anolisa/bin/anolisa", Vec::new());
        let txn = FakeSelfTxn::new("anolisa");

        let application_outcome = run_application_with_deps(
            SelfUpdateRequest {
                endpoint_url: "https://example.invalid/release-manifest.toml",
                current_version: "0.1.0",
                intent: ExecutionIntent::Apply,
            },
            &ctx,
            &ops,
            &query,
            &txn,
            false,
            None,
            "2026-06-01T10:00:00Z",
        )
        .expect("binary apply should succeed");

        let SelfUpdateApplicationOutcome::Applied { result, outcome } = application_outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(
            result,
            SelfUpdateApplied::Binary {
                from: "0.1.0".to_string(),
                to: "0.2.0".to_string(),
            }
        );
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(
            outcome.changes(),
            &[SelfUpdateChange::BinaryReplaced {
                from: "0.1.0".to_string(),
                to: "0.2.0".to_string(),
            }]
        );
        assert!(outcome.warnings().is_empty());
        let operation_id = outcome.operation_id().expect("durable audit id");
        let record = only_self_update_record(&ctx);
        assert_eq!(
            record
                .get("operation_id")
                .and_then(serde_json::Value::as_str),
            Some(operation_id)
        );
    }

    #[test]
    fn rpm_noop_is_completed_with_a_typed_delegation_change() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa");
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"])
            .with_installed_versions("anolisa", vec![Some("0.1.0"), Some("0.1.0")]);
        let txn = FakeSelfTxn::new("anolisa");

        let application_outcome = run_application_with_deps(
            SelfUpdateRequest {
                endpoint_url: "https://example.invalid/release-manifest.toml",
                current_version: "0.1.0",
                intent: ExecutionIntent::Apply,
            },
            &ctx,
            &ops,
            &query,
            &txn,
            true,
            None,
            "2026-06-01T10:00:00Z",
        )
        .expect("rpm no-op should remain successful");

        let SelfUpdateApplicationOutcome::Applied { result, outcome } = application_outcome else {
            panic!("expected applied outcome");
        };
        assert!(matches!(result, SelfUpdateApplied::RpmPackage { .. }));
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(
            outcome.changes(),
            &[SelfUpdateChange::RpmUpdateDelegated {
                package: "anolisa".to_string(),
                before_version: Some("0.1.0".to_string()),
                after_version: Some("0.1.0".to_string()),
            }]
        );
    }

    #[test]
    fn applied_audit_failure_is_a_warning_without_an_operation_id() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let log_path = common::resolve_layout(&ctx).central_log;
        std::fs::create_dir_all(&log_path).expect("replace central-log file with a directory");
        let ops = FakeSelfUpdateOps::new("/opt/anolisa/bin/anolisa");
        let query = FakeSelfQuery::new("/opt/anolisa/bin/anolisa", Vec::new());
        let txn = FakeSelfTxn::new("anolisa");

        let application_outcome = run_application_with_deps(
            SelfUpdateRequest {
                endpoint_url: "https://example.invalid/release-manifest.toml",
                current_version: "0.1.0",
                intent: ExecutionIntent::Apply,
            },
            &ctx,
            &ops,
            &query,
            &txn,
            false,
            None,
            "2026-06-01T10:00:00Z",
        )
        .expect("audit failure must not fail an applied update");

        let SelfUpdateApplicationOutcome::Applied { outcome, .. } = application_outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(outcome.operation_id(), None);
        assert_eq!(outcome.warnings().len(), 1);
        assert!(outcome.warnings()[0].starts_with("failed to write central log:"));
    }

    #[test]
    fn terminal_failure_carries_a_non_terminal_audit_warning() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let ctx = self_ctx(tmp.path().to_path_buf(), false);
        let log_path = common::resolve_layout(&ctx).central_log;
        std::fs::create_dir_all(&log_path).expect("replace central-log file with a directory");
        let ops = FakeSelfUpdateOps::new("/opt/anolisa/bin/anolisa").failing_binary_update();
        let query = FakeSelfQuery::new("/opt/anolisa/bin/anolisa", Vec::new());
        let txn = FakeSelfTxn::new("anolisa");

        let failure = run_application_with_deps(
            SelfUpdateRequest {
                endpoint_url: "https://example.invalid/release-manifest.toml",
                current_version: "0.1.0",
                intent: ExecutionIntent::Apply,
            },
            &ctx,
            &ops,
            &query,
            &txn,
            false,
            None,
            "2026-06-01T10:00:00Z",
        )
        .expect_err("binary failure must remain terminal");

        assert_eq!(failure.error.code(), "EXECUTION_FAILED");
        assert_eq!(failure.warnings.len(), 1);
        assert!(failure.warnings[0].starts_with("failed to write central log:"));
    }

    #[test]
    fn rpm_owned_self_update_delegates_to_dnf_without_binary_swap() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa");
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"])
            .with_installed_versions("anolisa", vec![Some("0.1.0"), Some("0.2.0")]);
        let txn = FakeSelfTxn::new("anolisa");

        let run = run_self_update_with_deps(
            "https://example.invalid/release-manifest.toml",
            "0.1.0",
            &c,
            &ops,
            &query,
            &txn,
            true,
            None,
        )
        .expect("rpm-owned self update should succeed through dnf");

        assert_eq!(
            run,
            SelfUpdateExecution::Applied(SelfUpdateApplied::RpmPackage {
                from: "0.1.0".to_string(),
                to: "0.2.0".to_string(),
                package: "anolisa".to_string(),
                before_version: Some("0.1.0".to_string()),
                after_version: Some("0.2.0".to_string())
            })
        );
        assert_eq!(query.queries.get(), 1, "rpm ownership must be probed");
        assert_eq!(
            query.installed_queries.get(),
            2,
            "rpm package version must be checked before and after dnf"
        );
        assert_eq!(txn.update_calls.get(), 1, "dnf update must run once");
        assert_eq!(
            ops.binary_updates.get(),
            0,
            "RPM-owned executable must not be overwritten directly"
        );
    }

    #[test]
    fn rpm_owned_self_update_dnf_noop_is_not_reported_as_updated() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa");
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"])
            .with_installed_versions("anolisa", vec![Some("0.1.0"), Some("0.1.0")]);
        let txn = FakeSelfTxn::new("anolisa");

        let run = run_self_update_with_deps(
            "https://example.invalid/release-manifest.toml",
            "0.1.0",
            &c,
            &ops,
            &query,
            &txn,
            true,
            None,
        )
        .expect("dnf no-op is still a successful delegation");

        assert_eq!(
            run,
            SelfUpdateExecution::Applied(SelfUpdateApplied::RpmPackage {
                from: "0.1.0".to_string(),
                to: "0.2.0".to_string(),
                package: "anolisa".to_string(),
                before_version: Some("0.1.0".to_string()),
                after_version: Some("0.1.0".to_string()),
            })
        );
    }

    #[test]
    fn non_rpm_self_update_keeps_binary_replacement_path() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/opt/anolisa/bin/anolisa");
        let query = FakeSelfQuery::new("/opt/anolisa/bin/anolisa", Vec::new());
        let txn = FakeSelfTxn::new("anolisa");

        let run = run_self_update_with_deps(
            "https://example.invalid/release-manifest.toml",
            "0.1.0",
            &c,
            &ops,
            &query,
            &txn,
            false,
            None,
        )
        .expect("non-rpm self update should use binary replacement");

        assert_eq!(
            run,
            SelfUpdateExecution::Applied(SelfUpdateApplied::Binary {
                from: "0.1.0".to_string(),
                to: "0.2.0".to_string(),
            })
        );
        assert_eq!(query.queries.get(), 1);
        assert_eq!(txn.update_calls.get(), 0, "dnf must not run");
        assert_eq!(ops.binary_updates.get(), 1, "binary replacement must run");
    }

    #[test]
    fn missing_rpm_tooling_keeps_binary_replacement_path() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/opt/anolisa/bin/anolisa");
        let query = FakeSelfQuery::missing_rpm("/opt/anolisa/bin/anolisa");
        let txn = FakeSelfTxn::new("anolisa");

        let run = run_self_update_with_deps(
            "https://example.invalid/release-manifest.toml",
            "0.1.0",
            &c,
            &ops,
            &query,
            &txn,
            false,
            None,
        )
        .expect("missing rpm must not block raw self-update");

        assert_eq!(
            run,
            SelfUpdateExecution::Applied(SelfUpdateApplied::Binary {
                from: "0.1.0".to_string(),
                to: "0.2.0".to_string(),
            })
        );
        assert_eq!(query.queries.get(), 1);
        assert_eq!(txn.update_calls.get(), 0, "dnf must not run");
        assert_eq!(ops.binary_updates.get(), 1, "binary replacement must run");
    }

    #[test]
    fn rpm_owned_self_update_requires_root_before_dnf() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = self_ctx(tmp.path().to_path_buf(), false);
        let ops = FakeSelfUpdateOps::new("/usr/bin/anolisa");
        let query = FakeSelfQuery::new("/usr/bin/anolisa", vec!["anolisa"]);
        let txn = FakeSelfTxn::new("anolisa");

        let err = run_self_update_with_deps(
            "https://example.invalid/release-manifest.toml",
            "0.1.0",
            &c,
            &ops,
            &query,
            &txn,
            false,
            None,
        )
        .expect_err("rpm-owned self update needs root");

        assert_eq!(err.error.code(), "EXECUTION_FAILED");
        assert!(
            err.error.reason().contains("root") && err.error.reason().contains("sudo"),
            "reason must point at sudo: {}",
            err.error.reason()
        );
        // The refusal happens before dnf, so there is no observed version to
        // record — but the package it would have moved is already known.
        assert_eq!(err.context.package.as_deref(), Some("anolisa"));
        assert_eq!(err.context.rpm_version_before, None);
        assert_eq!(txn.update_calls.get(), 0, "dnf must not run");
        assert_eq!(
            ops.binary_updates.get(),
            0,
            "binary replacement must not run"
        );
    }

    // ── component update: delegated (RPM-backed) records ──

    use anolisa_core::domain::LifecycleStatus;
    use anolisa_core::state::{
        FileOwner, InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectStatus,
        OwnedFile, OwnedFileKind, Ownership, RpmMetadata, ServiceRef,
    };
    use anolisa_platform::pkg_query::PackageInfo;

    use crate::context::InstallMode;

    /// In-memory rpm world implementing **both** [`PackageQuery`] and
    /// [`PackageTransaction`], so one fake drives the whole update flow.
    ///
    /// `installed` mutates: a successful [`update`](PackageTransaction::update)
    /// applies `upgrade_to`, modelling rpmdb advancing after dnf runs — so the
    /// pre-update query and the post-update refresh return different EVRs.
    pub(super) struct FakeRpm {
        package: String,
        repository: Option<&'static str>,
        installed: RefCell<Option<PackageInfo>>,
        /// PackageInfo the rpmdb holds after a successful update; `None` keeps
        /// the same version (a no-op "already latest").
        upgrade_to: Option<PackageInfo>,
        /// `false` makes the dnf transaction fail.
        update_succeeds: bool,
        /// `true` makes `query_installed` report a same-name multi-version drift.
        multi_version: bool,
        /// `true` makes the *post-update* `query_installed` report the package
        /// gone, modelling a failed rpmdb re-read after a successful dnf update.
        post_update_missing: bool,
        /// Runs after a successful dnf update — models the RPM transaction
        /// replacing package-owned files (e.g. an adapter resource bundle
        /// under the datadir).
        on_update: Option<Box<dyn Fn()>>,
        query_calls: Cell<usize>,
        update_calls: Cell<usize>,
    }

    impl FakeRpm {
        pub(super) fn new(package: &str, installed: Option<PackageInfo>) -> Self {
            Self {
                package: package.to_string(),
                repository: None,
                installed: RefCell::new(installed),
                upgrade_to: None,
                update_succeeds: true,
                multi_version: false,
                post_update_missing: false,
                on_update: None,
                query_calls: Cell::new(0),
                update_calls: Cell::new(0),
            }
        }
        fn upgrading_to(mut self, info: PackageInfo) -> Self {
            self.upgrade_to = Some(info);
            self
        }
        fn failing_update(mut self) -> Self {
            self.update_succeeds = false;
            self
        }
        fn multi_version(mut self) -> Self {
            self.multi_version = true;
            self
        }
        pub(super) fn post_update_missing(mut self) -> Self {
            self.post_update_missing = true;
            self
        }

        pub(super) fn update_call_count(&self) -> usize {
            self.update_calls.get()
        }
        fn with_on_update(mut self, hook: Box<dyn Fn()>) -> Self {
            self.on_update = Some(hook);
            self
        }
    }

    impl PackageQuery for FakeRpm {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            if package != self.package {
                return Ok(None);
            }
            self.query_calls.set(self.query_calls.get() + 1);
            // Simulate a failed post-update re-read: the package "vanishes" only
            // after dnf update has run, so the pre-update query still succeeds.
            if self.post_update_missing && self.update_calls.get() > 0 {
                return Ok(None);
            }
            if self.multi_version {
                return Err(PackageQueryError::UnexpectedOutput {
                    command: "rpm".to_string(),
                    detail: "2 installed versions".to_string(),
                });
            }
            Ok(self.installed.borrow().clone())
        }

        fn query_available(&self, package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            let _ = package;
            Ok(Vec::new())
        }
    }

    impl PackageTransaction for FakeRpm {
        fn repository_source(&self) -> Option<&str> {
            self.repository
        }
        fn check_install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("this path must not preflight an install")
        }

        fn install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            // The update flow never installs; a call here is a routing bug.
            panic!("update path must not delegate a dnf install");
        }

        fn update(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            let &[package] = packages else {
                panic!("expected exactly one package, got {packages:?}");
            };
            self.update_calls.set(self.update_calls.get() + 1);
            assert_eq!(package, self.package, "update targeted the wrong package");
            if !self.update_succeeds {
                return Err(PackageTransactionError::TransactionFailed {
                    command: "dnf".to_string(),
                    operation: "update".to_string(),
                    code: Some(1),
                    stderr: "repo unreachable".to_string(),
                });
            }
            if let Some(next) = &self.upgrade_to {
                *self.installed.borrow_mut() = Some(next.clone());
            }
            if let Some(hook) = &self.on_update {
                hook();
            }
            Ok(())
        }

        fn reinstall(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            // The update flow never reinstalls; a call here is a routing bug.
            panic!("update path must not delegate a dnf reinstall");
        }

        fn remove(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            // The update flow never removes; a call here is a routing bug.
            panic!("update path must not delegate a dnf remove");
        }
    }

    pub(crate) fn pkg_info(
        name: &str,
        version: &str,
        release: Option<&str>,
        arch: &str,
    ) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: PackageVersion {
                epoch: None,
                version: version.to_string(),
                release: release.map(str::to_string),
            },
            arch: arch.to_string(),
            origin: None,
        }
    }

    pub(crate) fn ctx(prefix: PathBuf, install_mode: InstallMode, dry_run: bool) -> CliContext {
        // Identity resolution consults the component index for names absent
        // from state; a seeded local index keeps fixture names supported.
        if install_mode == InstallMode::System {
            crate::commands::tier1::install::tests::seed_repo_config_with_index(
                &anolisa_platform::fs_layout::FsLayout::system(Some(prefix.clone())),
                crate::commands::tier1::install::tests::TEST_INDEX_COMPONENTS,
            );
        }
        crate::test_support::context_for_root(
            &prefix,
            install_mode,
            Some(prefix.clone()),
            crate::test_support::TestContextOptions {
                dry_run,
                ..Default::default()
            },
        )
    }

    /// Build a legacy (v4) RPM-backed component object; the store migrates it
    /// on load, so these tests double as migration coverage.
    pub(crate) fn rpm_object(
        component: &str,
        package: &str,
        evr: &str,
        ownership: Ownership,
        status: ObjectStatus,
    ) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: component.to_string(),
            version: evr.to_string(),
            status,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some("rpm".to_string()),
            ownership: Some(ownership),
            rpm_metadata: Some(RpmMetadata {
                package_name: package.to_string(),
                evr: Some(evr.to_string()),
                arch: Some("x86_64".to_string()),
                source_repo: Some("@System".to_string()),
            }),
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: Some("op-prior".to_string()),
            managed: !matches!(ownership, Ownership::RpmObserved),
            adopted: matches!(ownership, Ownership::RpmObserved),
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: Vec::new(),
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        }
    }

    /// A v4 rpm-observed object that was never adopted (`adopted = false`),
    /// migrating to the Observed relation.
    fn rpm_observed_unadopted(component: &str, package: &str, evr: &str) -> InstalledObject {
        let mut obj = rpm_object(
            component,
            package,
            evr,
            Ownership::RpmObserved,
            ObjectStatus::Installed,
        );
        obj.adopted = false;
        obj
    }

    /// A legacy (v4) raw-managed component object (no rpm metadata).
    fn raw_object(component: &str, version: &str) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: component.to_string(),
            version: version.to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: Some("https://example.com/x".to_string()),
            raw_package: None,
            install_backend: Some("raw".to_string()),
            ownership: Some(Ownership::RawManaged),
            rpm_metadata: None,
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: None,
            managed: true,
            adopted: false,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: Vec::new(),
            external_modified_files: Vec::new(),
            services: Vec::new(),
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        }
    }

    /// Seed `installed.toml` for `ctx`'s scope with one v4 object.
    pub(crate) fn seed(ctx: &CliContext, obj: InstalledObject) {
        let layout = common::resolve_layout(ctx);
        std::fs::create_dir_all(&layout.state_dir).expect("mkdir state");
        let mut state = InstalledState {
            install_mode: match ctx.install_mode {
                InstallMode::System => StateInstallMode::System,
                InstallMode::User => StateInstallMode::User,
            },
            prefix: layout.prefix.clone(),
            ..Default::default()
        };
        state.objects.push(obj);
        crate::test_support::write_legacy_state(&state, &layout.state_dir.join("installed.toml"))
            .expect("seed state");
    }

    pub(crate) fn write_repo_toml(ctx: &CliContext, body: &str) {
        let layout = common::resolve_layout(ctx);
        std::fs::create_dir_all(&layout.etc_dir).expect("mkdir etc");
        std::fs::write(layout.etc_dir.join("repo.toml"), body).expect("write repo.toml");
    }

    /// Load the (migrated) v5 store.
    pub(crate) fn load_store(ctx: &CliContext) -> StateStore {
        let layout = common::resolve_layout(ctx);
        StateStore::load(&layout.state_dir.join("installed.toml"), 0).expect("load state")
    }

    /// Find `name`'s component record in the (migrated) v5 store.
    fn find_component(ctx: &CliContext, name: &str) -> anolisa_core::domain::Installation {
        load_store(ctx)
            .find(ObjectKind::Component, name)
            .cloned()
            .expect("component record present")
    }

    /// The delegated binding's cached observation EVR, for version assertions.
    fn observed_evr(record: &anolisa_core::domain::Installation) -> Option<String> {
        match &record.binding {
            ProviderBinding::Delegated { last_observed, .. } => last_observed
                .as_ref()
                .map(|o| o.evr.clone().unwrap_or_else(|| o.version.clone())),
            _ => None,
        }
    }

    /// The owned binding's artifact, for raw-path assertions.
    fn owned_artifact(record: &anolisa_core::domain::Installation) -> OwnedArtifact {
        match &record.binding {
            ProviderBinding::Owned { artifact } => artifact.clone(),
            other => panic!("expected an owned record, got {other:?}"),
        }
    }

    /// Adopted RPM component, root, real run: dnf update runs, the cached
    /// observation is refreshed from rpmdb, and the delegated relation is
    /// preserved (update never changes authority).
    #[test]
    fn rpm_observed_update_refreshes_evr_and_keeps_ownership() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64")),
        )
        .upgrading_to(pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64"));

        update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true).expect("update ok");
        assert_eq!(rpm.update_calls.get(), 1, "dnf update must run once");

        let record = find_component(&c, "copilot-shell");
        assert_eq!(
            observed_evr(&record).as_deref(),
            Some("2.3.0-1.al8"),
            "observation refreshed from rpmdb"
        );
        assert!(
            matches!(
                &record.binding,
                ProviderBinding::Delegated {
                    relation: ManagementRelation::Adopted { .. },
                    ..
                }
            ),
            "the management relation must be preserved: {:?}",
            record.binding
        );
        assert_eq!(record.status, LifecycleStatus::Installed);
        assert_ne!(record.last_operation_id.as_deref(), Some("op-prior"));
    }

    #[test]
    fn update_records_source_only_when_native_identity_changes() {
        for (version, arch, source) in [
            ("1.0.0", "x86_64", "@System"),
            ("1.1.0", "x86_64", "configured"),
            ("1.0.0", "aarch64", "configured"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
            seed(
                &c,
                rpm_object(
                    "copilot-shell",
                    "copilot-shell",
                    "1.0.0-1.al8",
                    Ownership::RpmManaged,
                    ObjectStatus::Installed,
                ),
            );
            let mut rpm = FakeRpm::new(
                "copilot-shell",
                Some(pkg_info("copilot-shell", "1.0.0", Some("1.al8"), "x86_64")),
            )
            .upgrading_to(pkg_info("copilot-shell", version, Some("1.al8"), arch));
            rpm.repository = Some("configured");
            update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true).unwrap();
            let record = find_component(&c, "copilot-shell");
            let ProviderBinding::Delegated {
                last_observed: Some(observation),
                ..
            } = record.binding
            else {
                panic!("missing observation")
            };
            assert_eq!(observation.source_repo.as_deref(), Some(source));
        }
    }

    /// rpm-managed component updates the same way (different relation).
    #[test]
    fn rpm_managed_update_refreshes_evr() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "1.0.0-1.al8",
                Ownership::RpmManaged,
                ObjectStatus::Installed,
            ),
        );
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "1.0.0", Some("1.al8"), "x86_64")),
        )
        .upgrading_to(pkg_info("copilot-shell", "1.1.0", Some("1.al8"), "x86_64"));

        update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true).expect("update ok");

        assert_eq!(
            rpm.query_calls.get(),
            3,
            "update must observe before planning, under the lock, and after the transaction"
        );

        let record = find_component(&c, "copilot-shell");
        assert_eq!(observed_evr(&record).as_deref(), Some("1.1.0-1.al8"));
        assert!(matches!(
            &record.binding,
            ProviderBinding::Delegated {
                relation: ManagementRelation::Managed { .. },
                ..
            }
        ));
        assert_eq!(record.status, LifecycleStatus::Installed);
    }

    /// Publish a package-owned contract in the FHS package datadir plus the
    /// stale pre-update snapshot an earlier install left behind, as an RPM
    /// upgrade of the package would leave them.
    pub(crate) fn seed_package_contract_and_stale_snapshot(
        c: &CliContext,
        component: &str,
    ) -> (PathBuf, PathBuf) {
        let layout = common::resolve_layout(c);
        let package_datadir = layout.package_datadir().expect("package datadir");
        let source = FsLayout::component_contract_path(&package_datadir, component);
        std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
        std::fs::write(&source, "framework = \"new\"\n").expect("write package contract");
        let snapshot = FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
        std::fs::create_dir_all(snapshot.parent().expect("snapshot parent"))
            .expect("mkdir snapshot");
        std::fs::write(&snapshot, "framework = \"old\"\n").expect("write stale snapshot");
        (source, snapshot)
    }

    /// The durable status of the single recorded `update <component>`
    /// operation.
    fn update_operation_status(c: &CliContext, target: &str) -> String {
        let command = format!("update {target}");
        let store = load_store(c);
        let ops: Vec<_> = store
            .operations
            .iter()
            .filter(|op| op.command == command)
            .collect();
        assert_eq!(ops.len(), 1, "exactly one update operation recorded");
        ops[0].status.clone()
    }

    /// A committed delegated update refreshes the contract snapshot and its
    /// provenance directly — `status`/`doctor` read the new contract without
    /// waiting for a later repair.
    #[test]
    fn rpm_update_refreshes_contract_snapshot_and_provenance() {
        use anolisa_core::adapter::contract::{ContractSourceKind, read_snapshot_provenance};

        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "1.0.0-1.al8",
                Ownership::RpmManaged,
                ObjectStatus::Installed,
            ),
        );
        let (source, snapshot) = seed_package_contract_and_stale_snapshot(&c, "copilot-shell");
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "1.0.0", Some("1.al8"), "x86_64")),
        )
        .upgrading_to(pkg_info("copilot-shell", "1.1.0", Some("1.al8"), "x86_64"));

        update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true).expect("update ok");

        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("read snapshot"),
            "framework = \"new\"\n",
            "the snapshot must carry the post-update contract"
        );
        let provenance = read_snapshot_provenance(&snapshot).expect("snapshot provenance");
        assert_eq!(provenance.source_kind, ContractSourceKind::Datadir);
        assert_eq!(provenance.source_path, source);
        assert_eq!(update_operation_status(&c, "copilot-shell"), "ok");
    }

    /// A package that publishes no contract stays a clean success
    /// (`NotApplicable`): no snapshot write, no demotion.
    #[test]
    fn rpm_update_without_package_contract_stays_ok() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "1.0.0-1.al8",
                Ownership::RpmManaged,
                ObjectStatus::Installed,
            ),
        );
        // A stale snapshot without a package-owned contract: nothing
        // authoritative exists to refresh from, so it must stay untouched.
        let layout = common::resolve_layout(&c);
        let snapshot =
            FsLayout::component_manifest_snapshot_path(&layout.state_dir, "copilot-shell");
        std::fs::create_dir_all(snapshot.parent().expect("snapshot parent"))
            .expect("mkdir snapshot");
        std::fs::write(&snapshot, "framework = \"old\"\n").expect("write stale snapshot");
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "1.0.0", Some("1.al8"), "x86_64")),
        )
        .upgrading_to(pkg_info("copilot-shell", "1.1.0", Some("1.al8"), "x86_64"));

        update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true).expect("update ok");

        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("read snapshot"),
            "framework = \"old\"\n",
            "no package contract means no snapshot write"
        );
        assert_eq!(update_operation_status(&c, "copilot-shell"), "ok");
    }

    /// A contract refresh that cannot complete demotes the committed update
    /// to `partial` and reports it — the durable operation never overstates
    /// what happened, and the record still carries the refreshed EVR.
    #[test]
    fn rpm_update_contract_refresh_failure_is_partial() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "1.0.0-1.al8",
                Ownership::RpmManaged,
                ObjectStatus::Installed,
            ),
        );
        let layout = common::resolve_layout(&c);
        let package_datadir = layout.package_datadir().expect("package datadir");
        let source = FsLayout::component_contract_path(&package_datadir, "copilot-shell");
        std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
        std::fs::write(&source, "framework = \"new\"\n").expect("write package contract");
        // A non-empty directory where the snapshot file belongs blocks the
        // refresh's atomic replacement.
        let snapshot =
            FsLayout::component_manifest_snapshot_path(&layout.state_dir, "copilot-shell");
        std::fs::create_dir_all(&snapshot).expect("create blocking snapshot directory");
        std::fs::write(snapshot.join("keep"), b"x").expect("write blocking marker");
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "1.0.0", Some("1.al8"), "x86_64")),
        )
        .upgrading_to(pkg_info("copilot-shell", "1.1.0", Some("1.al8"), "x86_64"));

        let err = update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true)
            .expect_err("a failed contract refresh must not report a clean success");

        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("committed") && err.reason().contains("repair"),
            "reason must state the update committed and point at repair: {}",
            err.reason()
        );
        assert_eq!(
            observed_evr(&find_component(&c, "copilot-shell")).as_deref(),
            Some("1.1.0-1.al8"),
            "the committed record refresh must be kept"
        );
        assert_eq!(update_operation_status(&c, "copilot-shell"), "partial");
        // The central-log record must be discoverable by `logs --severity
        // warn`, exactly like repair/upgrade partials.
        let log = std::fs::read_to_string(&common::resolve_layout(&c).central_log)
            .expect("read central log");
        let record: serde_json::Value =
            serde_json::from_str(log.lines().last().expect("log record")).expect("parse record");
        assert_eq!(record["status"], "partial");
        assert_eq!(record["severity"], "warn");
    }

    /// When the pessimistic `partial` record cannot be persisted, the
    /// refresh must not run: mutating the snapshot without a durable signal
    /// would reopen the crash window this completion exists to close.
    #[test]
    fn refresh_is_skipped_when_partial_record_cannot_be_persisted() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let (_, snapshot) = seed_package_contract_and_stale_snapshot(&c, "copilot-shell");
        let layout = common::resolve_layout(&c);
        // A non-empty directory at the state path blocks the pessimistic
        // save's atomic replacement.
        let state_path = layout.state_dir.join("installed.toml");
        std::fs::create_dir_all(&state_path).expect("create blocking state directory");
        std::fs::write(state_path.join("keep"), b"x").expect("write blocking marker");
        let mut store = StateStore::load(&tmp.path().join("absent.toml"), 0).expect("empty store");

        let failure = complete_delegated_update(
            &layout,
            &c,
            "copilot-shell",
            "copilot-shell",
            "update copilot-shell",
            &mut store,
            &state_path,
            OperationRecord {
                id: "op-update-test".to_string(),
                command: "update copilot-shell".to_string(),
                status: String::new(),
                started_at: now_iso8601(),
                finished_at: Some(now_iso8601()),
                parent_operation_id: None,
            },
        )
        .expect("a blocked pessimistic save must fail the completion");

        assert!(
            failure.contains("skipped") && failure.contains("could not be persisted"),
            "failure must explain the refresh was gated on the record: {failure}"
        );
        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("read snapshot"),
            "framework = \"old\"\n",
            "the refresh must not run without the durable partial record"
        );
    }

    /// U6 (breaking): an observed record carries no management consent, so
    /// update refuses and points at `adopt` — dnf never runs on a system RPM
    /// that was merely observed.
    #[test]
    fn observed_component_requires_adoption_first() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_observed_unadopted("copilot-shell", "copilot-shell", "2.2.0-1.al8"),
        );
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64")),
        );

        let err = update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true)
            .expect_err("an observed record must refuse to update");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("adopt"),
            "reason must point at adopt: {}",
            err.reason()
        );
        assert_eq!(rpm.update_calls.get(), 0, "dnf must not run");
    }

    /// Non-root real run is refused with an actionable message; dnf never runs
    /// and state is untouched.
    #[test]
    fn non_root_update_is_refused_without_running_dnf() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64")),
        );

        let err = update_component_with_deps("copilot-shell", &c, &rpm, &rpm, false)
            .expect_err("must refuse without root");
        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("root") && err.reason().contains("sudo"),
            "reason must point at sudo: {}",
            err.reason()
        );
        assert_eq!(rpm.update_calls.get(), 0, "dnf must not run without root");
        assert_eq!(
            find_component(&c, "copilot-shell")
                .last_operation_id
                .as_deref(),
            Some("op-prior"),
            "state must be unchanged"
        );
    }

    /// Dry-run previews the plan without running dnf, needing root, or writing
    /// state or the contract snapshot — even for a non-root caller.
    #[test]
    fn dry_run_previews_without_dnf_or_state_write() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        let (_, snapshot) = seed_package_contract_and_stale_snapshot(&c, "copilot-shell");
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64")),
        );

        update_component_with_deps("copilot-shell", &c, &rpm, &rpm, false).expect("dry-run ok");
        assert_eq!(rpm.update_calls.get(), 0, "dry-run must not run dnf");
        assert_eq!(
            find_component(&c, "copilot-shell")
                .last_operation_id
                .as_deref(),
            Some("op-prior"),
            "dry-run must not write state"
        );
        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("read snapshot"),
            "framework = \"old\"\n",
            "dry-run must not touch the snapshot"
        );
    }

    /// A component absent from state routes to NOT_INSTALLED (exit 2), not a
    /// runtime failure, and never runs dnf.
    #[test]
    fn unknown_component_routes_to_not_installed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        let rpm = FakeRpm::new("copilot-shell", None);
        let err = update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true)
            .expect_err("absent component must error");
        assert_eq!(err.code(), "NOT_INSTALLED");
        assert_eq!(err.exit_code(), 2);
        assert!(err.reason().contains("not installed"));
        assert_eq!(rpm.update_calls.get(), 0);
    }

    /// Regression: a bare `anolisa update` (no component, no subcommand) fails
    /// validation as INVALID_ARGUMENT and does not provision repo config.
    #[test]
    fn bare_update_errors_before_repo_config_provisioning() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        // Deliberately not the shared ctx helper: this test asserts that the
        // invalid invocation writes no repo.toml, so nothing may pre-seed one.
        let c = crate::test_support::context_for_root(
            tmp.path(),
            InstallMode::System,
            Some(tmp.path().to_path_buf()),
            Default::default(),
        );
        let repo_toml = common::resolve_layout(&c).etc_dir.join("repo.toml");

        let err = handle(
            UpdateArgs {
                component: None,
                command: None,
                check: false,
                motd: false,
                refresh: false,
                target: None,
            },
            &c,
        )
        .expect_err("bare `update` must fail validation");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert_eq!(err.exit_code(), 2);
        assert!(
            !repo_toml.exists(),
            "no repo config must be written for an invalid invocation: {} exists",
            repo_toml.display()
        );
    }

    #[test]
    fn rpm_update_repo_source_uses_repo_toml_backend() {
        let repo = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "rpm"

[backends.rpm]
base_url = "https://repo.example/$os/$arch/os"
gpgcheck = false
"#,
        )
        .expect("repo config");
        let env = anolisa_env::EnvService::detect_for("linux");

        let source = rpm_repo_source_for_update(&repo, &env, "update copilot-shell")
            .expect("repo source")
            .expect("rpm backend");

        assert_eq!(source.id(), ANOLISA_RPM_REPO_ID);
        assert_eq!(
            source.base_url(),
            format!("https://repo.example/linux/{}/os", env.arch)
        );
        assert_eq!(source.gpgcheck(), Some(false));
    }

    #[test]
    fn rpm_update_requires_configured_rpm_backend() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        write_repo_toml(
            &c,
            r#"schema_version = 1
default_backend = "raw"

[backends.raw]
base_url = "https://repo.example/raw/v1"
"#,
        );

        let err = handle_component_update("copilot-shell", &c)
            .expect_err("rpm update must require rpm backend");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("[backends.rpm]"),
            "reason must explain the missing rpm backend: {}",
            err.reason()
        );
    }

    /// State records the RPM but rpmdb no longer has it (rpm -e drift, U7):
    /// refuse with repair/forget pointers rather than running dnf blindly.
    #[test]
    fn missing_from_rpmdb_refuses_with_forget_hint() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        // rpmdb reports nothing installed for the package.
        let rpm = FakeRpm::new("copilot-shell", None);
        let err = update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true)
            .expect_err("drift must error");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("forget"),
            "reason must point at forget: {}",
            err.reason()
        );
        assert_eq!(rpm.update_calls.get(), 0);
    }

    /// `dnf update` failure surfaces as EXECUTION_FAILED with a repair pointer
    /// and does not refresh state or the contract snapshot.
    #[test]
    fn dnf_failure_surfaces_and_leaves_state_untouched() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        let (_, snapshot) = seed_package_contract_and_stale_snapshot(&c, "copilot-shell");
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64")),
        )
        .failing_update();

        let err = update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true)
            .expect_err("dnf failure must propagate");
        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("dnf update failed") && err.reason().contains("repair"),
            "reason must carry the dnf failure and a repair pointer: {}",
            err.reason()
        );
        assert_eq!(
            find_component(&c, "copilot-shell")
                .last_operation_id
                .as_deref(),
            Some("op-prior"),
            "failed update must not refresh the record"
        );
        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("read snapshot"),
            "framework = \"old\"\n",
            "a failed native transaction must not touch the snapshot"
        );
    }

    /// A same-name multi-version rpmdb (e.g. installonly packages) still
    /// counts as present, so dnf update runs — but the post-update observation
    /// cannot be recorded as one version, so the refresh fails honestly
    /// instead of caching an ambiguous EVR.
    #[test]
    fn multi_version_rpmdb_updates_but_refuses_to_record_ambiguity() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64")),
        )
        .multi_version();
        let err = update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true)
            .expect_err("an ambiguous post-update observation must error");
        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("multiple installed versions"),
            "got: {}",
            err.reason()
        );
        assert_eq!(
            rpm.update_calls.get(),
            1,
            "dnf update ran against the present package"
        );
        assert_eq!(
            find_component(&c, "copilot-shell")
                .last_operation_id
                .as_deref(),
            Some("op-prior"),
            "the ambiguous observation must not be recorded"
        );
    }

    /// No-op update (already latest): dnf runs (it owns the resolution), the
    /// EVR is unchanged, and the refreshed observation is still recorded.
    #[test]
    fn already_latest_reports_no_change() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.3.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        // upgrade_to is None => update() is a no-op; EVR stays the same.
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.3.0", Some("1.al8"), "x86_64")),
        );
        update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true).expect("update ok");
        assert_eq!(rpm.update_calls.get(), 1);
        let record = find_component(&c, "copilot-shell");
        assert_eq!(observed_evr(&record).as_deref(), Some("2.3.0-1.al8"));
        // Operation still recorded (last_operation_id advanced from the seed).
        assert_ne!(record.last_operation_id.as_deref(), Some("op-prior"));
    }

    /// dnf update applied, but the post-update rpmdb re-read cannot confirm the
    /// new EVR: surface a repair-pointing failure rather than recording the
    /// stale EVR, and leave the recorded state as-is.
    #[test]
    fn refresh_failure_after_successful_update_errors_and_leaves_state() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "2.2.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64")),
        )
        .post_update_missing();

        let err = update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true)
            .expect_err("a failed post-update refresh must surface");
        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("repair"),
            "reason must point at repair: {}",
            err.reason()
        );
        assert_eq!(rpm.update_calls.get(), 1, "dnf update did run");
        assert_eq!(
            find_component(&c, "copilot-shell")
                .last_operation_id
                .as_deref(),
            Some("op-prior"),
            "no stale EVR may be recorded as success"
        );
    }

    /// Post-lock guard (pure): the locked re-read only authorizes recording
    /// when the record is still delegated, consent still holds, and the
    /// package identity is unchanged — recording against a re-pointed record
    /// would graft one package's version onto another's metadata.
    #[test]
    fn native_update_authority_is_recomputed_from_the_locked_read() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "anolisa-pkg-b",
                "1.0.0-1.al8",
                Ownership::RpmObserved,
                ObjectStatus::Adopted,
            ),
        );
        let store = load_store(&c);

        assert!(native_update_authorized(
            &store,
            "copilot-shell",
            Some("anolisa-pkg-b")
        ));
        assert!(
            !native_update_authorized(&store, "copilot-shell", Some("anolisa-pkg-a")),
            "a re-pointed package identity must not be recorded against"
        );
        assert!(
            !native_update_authorized(&store, "missing", Some("anolisa-pkg-b")),
            "a vanished record never authorizes recording"
        );
    }

    /// The locked re-read also refuses when consent was downgraded to
    /// observed while dnf ran.
    #[test]
    fn native_update_authority_requires_management_consent() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_observed_unadopted("copilot-shell", "copilot-shell", "1.0.0-1.al8"),
        );
        let store = load_store(&c);
        assert!(
            !native_update_authorized(&store, "copilot-shell", Some("copilot-shell")),
            "an observed record must not authorize a native update"
        );
    }

    // ── component update: owned (raw) records ──

    fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use tar::{Builder, Header};
        let enc = GzEncoder::new(Vec::new(), Compression::default());
        let mut tar = Builder::new(enc);
        for (path, data) in entries {
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, *path, *data)
                .expect("append tar entry");
        }
        tar.into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish gzip")
    }

    fn raw_manifest(component: &str, version: &str) -> String {
        format!(
            r#"[component]
name = "{component}"
version = "{version}"

[component.layout]
modes = ["system", "user"]

[[component.layout.files]]
source = "bin/{component}"
target = "{{bindir}}/{component}"
mode = "0755"
type = "executable"
"#
        )
    }

    /// tar.gz carrying the embedded manifest plus the binary it declares.
    fn raw_artifact(component: &str, version: &str, body: &[u8]) -> Vec<u8> {
        let manifest = raw_manifest(component, version);
        tar_gz(&[
            (".anolisa/component.toml", manifest.as_bytes()),
            (format!("bin/{component}").as_str(), body),
        ])
    }

    /// tar.gz whose manifest declares the binary but omits it, so the install
    /// runner fails after the old files have been backed up and removed.
    fn raw_artifact_missing_binary(component: &str, version: &str) -> Vec<u8> {
        let manifest = raw_manifest(component, version);
        tar_gz(&[(".anolisa/component.toml", manifest.as_bytes())])
    }

    /// Publish one version of `component` to a local file:// raw repo under
    /// `root` and point `layout`'s repo.toml at it. Returns the repo base URL.
    fn publish_raw_repo(
        root: &Path,
        layout: &FsLayout,
        component: &str,
        version: &str,
        artifact: &[u8],
    ) -> String {
        use sha2::{Digest, Sha256};
        let v1 = root.join("v1");
        std::fs::create_dir_all(&v1).expect("create repo dirs");
        let artifact_name = format!("{component}.tar.gz");
        std::fs::write(v1.join(&artifact_name), artifact).expect("write artifact");
        let sha = format!("{:x}", Sha256::digest(artifact));
        let env = anolisa_env::EnvService::detect();
        let index = format!(
            r#"schema_version = 1
channel = "stable"
publisher = "test"

[[entries]]
component = "{component}"
version = "{version}"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{artifact_name}"
os = "{os}"
arch = "{arch}"
install_modes = ["system", "user"]
sha256 = "{sha}"
"#,
            os = env.os,
            arch = env.arch,
        );
        std::fs::write(v1.join("index.toml"), index).expect("write index");
        let base_url = format!("file://{}", v1.display());

        std::fs::create_dir_all(&layout.etc_dir).expect("etc dir");
        std::fs::write(
            layout.etc_dir.join("repo.toml"),
            format!(
                "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"{base_url}\"\n"
            ),
        )
        .expect("write repo.toml");
        base_url
    }

    /// Seed an installed raw component at `version` with one owned binary
    /// holding `body` plus its manifest snapshot.
    fn seed_installed_raw(ctx: &CliContext, component: &str, version: &str, body: &[u8]) {
        use sha2::{Digest, Sha256};
        let layout = common::resolve_layout(ctx);
        std::fs::create_dir_all(&layout.bin_dir).expect("bin dir");
        let bin = layout.bin_dir.join(component);
        std::fs::write(&bin, body).expect("write bin");
        let bin_sha = format!("{:x}", Sha256::digest(body));

        let manifest_path = common::installed_component_manifest_path(&layout, component, "update")
            .expect("manifest path");
        if let Some(parent) = manifest_path.parent() {
            std::fs::create_dir_all(parent).expect("manifest dir");
        }
        std::fs::write(&manifest_path, raw_manifest(component, version)).expect("write manifest");

        let files = vec![
            OwnedFile {
                path: bin,
                owner: FileOwner::Anolisa,
                sha256: Some(bin_sha),
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            },
            OwnedFile {
                path: manifest_path,
                owner: FileOwner::Anolisa,
                sha256: None,
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            },
        ];
        let mut obj = raw_object(component, version);
        obj.files = files;
        seed(ctx, obj);
    }

    /// Raw update resolves the latest published version, replaces the owned
    /// files, preserves the owned authority, and records the operation. The
    /// manifest is replaced as part of the owned artifact — the delegated
    /// datadir-contract refresh must not run.
    #[test]
    fn raw_update_upgrades_to_latest_and_preserves_ownership() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        seed_installed_raw(&c, "foo", "0.1.0", b"old v1 binary\n");
        // A datadir contract that only the delegated refresh would consult;
        // an owned update must never copy it over the owned manifest.
        let layout = common::resolve_layout(&c);
        let package_datadir = layout.package_datadir().expect("package datadir");
        let contract = FsLayout::component_contract_path(&package_datadir, "foo");
        std::fs::create_dir_all(contract.parent().expect("contract parent"))
            .expect("mkdir contract");
        std::fs::write(&contract, "framework = \"datadir\"\n").expect("write datadir contract");
        let new_body: &[u8] = b"#!/bin/sh\necho foo v2\n";
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &raw_artifact("foo", "0.2.0", new_body),
        );
        let rpm = FakeRpm::new("unused", None);

        update_component_with_deps("foo", &c, &rpm, &rpm, false).expect("raw update must succeed");

        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            new_body,
            "binary must be replaced with the v2 payload"
        );
        let manifest = FsLayout::component_manifest_snapshot_path(&layout.state_dir, "foo");
        assert_eq!(
            std::fs::read_to_string(&manifest).expect("read manifest"),
            raw_manifest("foo", "0.2.0"),
            "the manifest must be the owned artifact's, not the datadir contract"
        );
        assert!(
            !FsLayout::provenance_path_for_snapshot(&manifest).exists(),
            "an owned update must not publish contract provenance"
        );
        let record = find_component(&c, "foo");
        let artifact = owned_artifact(&record);
        assert_eq!(artifact.version, "0.2.0");
        assert_eq!(record.status, LifecycleStatus::Installed);
        assert!(
            record
                .last_operation_id
                .as_deref()
                .is_some_and(|id| id.starts_with("op-update-")),
            "operation id must carry the update verb, got {:?}",
            record.last_operation_id
        );
        let store = load_store(&c);
        assert!(
            store.operations.iter().any(|o| o.command == "update foo"),
            "update operation must be recorded"
        );
        assert!(
            layout
                .backup_dir
                .read_dir()
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
            "backups must be pruned after a successful update"
        );
    }

    /// When the recorded version already matches the latest published version,
    /// update is a clean no-op (U2): no file or state change, no operation
    /// recorded.
    #[test]
    fn raw_update_already_latest_is_noop() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        let body: &[u8] = b"current binary\n";
        seed_installed_raw(&c, "foo", "0.2.0", body);
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &raw_artifact("foo", "0.2.0", b"unused\n"),
        );
        let rpm = FakeRpm::new("unused", None);

        let effects = crate::test_support::RawEffectRecorder::default();
        let outcome = effects
            .with(|f| {
                application::run_with_dependencies(
                    application::ApplicationRequest {
                        component: "foo",
                        intent: execution_intent(&c),
                    },
                    &c,
                    &rpm,
                    &rpm,
                    false,
                    f,
                )
            })
            .expect("read-only update outcome");
        assert!(effects.calls().is_empty());
        render_application_outcome(&c, outcome).expect("render");

        let layout = common::resolve_layout(&c);
        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            body,
            "no-op must not touch the binary"
        );
        assert!(
            load_store(&c).operations.is_empty(),
            "no-op records no operation"
        );
    }

    /// Dry-run reports without touching the filesystem or recorded state.
    #[test]
    fn raw_update_dry_run_does_not_touch_files_or_state() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, true);
        let body: &[u8] = b"old binary\n";
        seed_installed_raw(&c, "foo", "0.1.0", body);
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &raw_artifact("foo", "0.2.0", b"new\n"),
        );
        let rpm = FakeRpm::new("unused", None);

        let effects = crate::test_support::RawEffectRecorder::default();
        let outcome = effects
            .with(|f| {
                application::run_with_dependencies(
                    application::ApplicationRequest {
                        component: "foo",
                        intent: execution_intent(&c),
                    },
                    &c,
                    &rpm,
                    &rpm,
                    false,
                    f,
                )
            })
            .expect("read-only update outcome");
        assert!(effects.calls().is_empty());
        render_application_outcome(&c, outcome).expect("render");

        let layout = common::resolve_layout(&c);
        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            body,
            "dry-run must not touch the binary"
        );
        assert_eq!(
            owned_artifact(&find_component(&c, "foo")).version,
            "0.1.0",
            "dry-run must not change the recorded version"
        );
    }

    /// A failure while installing the new version compensates back: the old
    /// files are restored from backup — bytes *and* mode — and the recorded
    /// version is unchanged. Restoring an owned binary without its
    /// executable bit leaves the component unusable after a rollback the
    /// CLI called clean, so the mode is part of what "restored" means.
    #[test]
    fn raw_update_rolls_back_on_install_failure() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        let body: &[u8] = b"original v1 binary\n";
        seed_installed_raw(&c, "foo", "0.1.0", body);
        let layout = common::resolve_layout(&c);
        let bin = layout.bin_dir.join("foo");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
                .expect("seed the binary executable, as an install would");
        }
        publish_raw_repo(
            &tmp.path().join("repo"),
            &layout,
            "foo",
            "0.2.0",
            &raw_artifact_missing_binary("foo", "0.2.0"),
        );
        let rpm = FakeRpm::new("unused", None);

        let err = update_component_with_deps("foo", &c, &rpm, &rpm, false)
            .expect_err("install of the new version must fail");
        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("the previous files were restored"),
            "the failure must report the compensation honestly: {}",
            err.reason()
        );

        assert_eq!(
            std::fs::read(&bin).expect("read bin"),
            body,
            "old binary must be restored from backup"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&bin)
                    .expect("stat bin")
                    .permissions()
                    .mode()
                    & 0o7777,
                0o755,
                "restored binary must keep the executable bit it had before the update"
            );
        }
        assert_eq!(
            owned_artifact(&find_component(&c, "foo")).version,
            "0.1.0",
            "failed update must not change the recorded version"
        );
    }

    /// Restoring a file writes a new inode, which carries no capability
    /// xattr — so rollback has to re-apply the capabilities confirmed in the
    /// prior state, or the restored binary comes back stripped of them.
    ///
    /// The recording capability backend is independent of host support;
    /// user scope still exercises the original rollback routing.
    #[test]
    fn raw_update_rollback_reapplies_prior_capabilities() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("home"), InstallMode::User, false);
        seed_installed_raw(&c, "foo", "0.1.0", b"original v1 binary\n");
        let layout = common::resolve_layout(&c);
        let bin = layout.bin_dir.join("foo");

        // Model a successfully applied optional grant: the manifest declares
        // it and the prior state confirms that it landed.
        let manifest_path = common::installed_component_manifest_path(&layout, "foo", "update")
            .expect("manifest path");
        let with_caps = format!(
            "{}\n[[component.capabilities]]\npath = \"{{bindir}}/foo\"\ncaps = [\"cap_net_bind_service\"]\noptional = true\n",
            raw_manifest("foo", "0.1.0")
        );
        std::fs::write(&manifest_path, &with_caps).expect("rewrite installed manifest");
        let state_path = layout.state_dir.join("installed.toml");
        let mut store =
            StateStore::load_for_layout(&state_path, privilege::effective_uid(), &layout)
                .expect("load active state");
        let ProviderBinding::Owned { artifact } = &mut store
            .find_mut(ObjectKind::Component, "foo")
            .expect("component")
            .binding
        else {
            panic!("expected owned record");
        };
        artifact
            .files
            .iter_mut()
            .find(|file| file.path == bin)
            .expect("binary row")
            .capabilities = vec!["cap_net_bind_service".to_string()];
        store.save(&state_path).expect("persist recorded grant");

        publish_raw_repo(
            &tmp.path().join("repo"),
            &layout,
            "foo",
            "0.2.0",
            &raw_artifact_missing_binary("foo", "0.2.0"),
        );
        let rpm = FakeRpm::new("unused", None);

        let effects = crate::test_support::RawEffectRecorder::default();
        let err = effects
            .with(|f| {
                application::run_with_dependencies(
                    application::ApplicationRequest {
                        component: "foo",
                        intent: execution_intent(&c),
                    },
                    &c,
                    &rpm,
                    &rpm,
                    false,
                    f,
                )
            })
            .err()
            .expect("install of the new version must fail");
        assert_eq!(
            effects.calls(),
            vec![
                "capability factory user".to_string(),
                "capability factory user".to_string(),
                format!("apply {} cap_net_bind_service", bin.display()),
            ]
        );
        assert!(
            err.reason().contains("the previous files were restored"),
            "the failure must report the compensation honestly: {}",
            err.reason()
        );

        assert_eq!(
            effects.capability_contents(),
            vec![b"original v1 binary\n".to_vec()]
        );
        // The forward path never reached SetCapabilities, so any capability
        // record in the log was written by the rollback.
        let log = std::fs::read_to_string(&layout.central_log).expect("read central log");
        let applied: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|record| record["command"] == "capability:apply")
            .collect();
        assert_eq!(
            applied.len(),
            1,
            "rollback must re-apply the prior contract's capabilities exactly once: {log}"
        );
        assert_eq!(applied[0]["details"]["path"], bin.display().to_string());
        assert_eq!(applied[0]["details"]["caps"][0], "cap_net_bind_service");
    }

    /// A manifest can declare an optional grant that did not land. Rollback
    /// must not infer it from the snapshot and expand privilege while
    /// restoring a failed update.
    #[test]
    fn raw_update_rollback_does_not_infer_unrecorded_optional_capability() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("home"), InstallMode::User, false);
        seed_installed_raw(&c, "foo", "0.1.0", b"original v1 binary\n");
        let layout = common::resolve_layout(&c);

        let manifest_path = common::installed_component_manifest_path(&layout, "foo", "update")
            .expect("manifest path");
        let with_optional_capability = format!(
            "{}\n[[component.capabilities]]\npath = \"{{bindir}}/foo\"\ncaps = [\"cap_net_bind_service\"]\noptional = true\n",
            raw_manifest("foo", "0.1.0")
        );
        std::fs::write(&manifest_path, with_optional_capability)
            .expect("rewrite installed manifest");

        publish_raw_repo(
            &tmp.path().join("repo"),
            &layout,
            "foo",
            "0.2.0",
            &raw_artifact_missing_binary("foo", "0.2.0"),
        );
        let rpm = FakeRpm::new("unused", None);

        let err = update_component_with_deps("foo", &c, &rpm, &rpm, false)
            .expect_err("install of the new version must fail");
        let reason = err.reason();
        assert!(
            reason.contains("the previous files were restored"),
            "an unrecorded optional grant must not degrade rollback: {reason}"
        );
        let capability_was_attempted = std::fs::read_to_string(&layout.central_log)
            .ok()
            .is_some_and(|log| log.lines().any(|line| line.contains("capability:apply")));
        assert!(
            !capability_was_attempted,
            "rollback must not infer the unrecorded optional grant"
        );
        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            b"original v1 binary\n",
            "the prior binary must still be restored"
        );
    }

    /// A pre-v5 state row has no capability metadata even when its required
    /// grant succeeded. The locked update must hydrate that contract before
    /// building the rollback snapshot, while keeping the inference in-memory
    /// when the update fails.
    /// Hydration support is injected independently of the user-mode executor,
    /// which never invokes setcap, so both host policies are deterministic.
    #[test]
    fn raw_update_rollback_hydrates_legacy_required_capability() {
        for (supported, optional) in [(true, false), (false, false), (true, true), (false, true)] {
            let inferred = supported && !optional;
            let tmp = tempfile::tempdir().expect("tmpdir");
            let c = ctx(tmp.path().join("home"), InstallMode::User, false);
            seed_installed_raw(&c, "foo", "0.1.0", b"original v1 binary\n");
            let layout = common::resolve_layout(&c);
            let bin = layout.bin_dir.join("foo");

            let manifest_path = common::installed_component_manifest_path(&layout, "foo", "update")
                .expect("manifest path");
            let with_required_capability = format!(
                "{}\n[[component.capabilities]]\npath = \"{{bindir}}/foo\"\ncaps = [\"cap_net_bind_service\"]\noptional = {optional}\n",
                raw_manifest("foo", "0.1.0")
            );
            std::fs::write(&manifest_path, with_required_capability)
                .expect("rewrite installed manifest");

            publish_raw_repo(
                &tmp.path().join("repo"),
                &layout,
                "foo",
                "0.2.0",
                &raw_artifact_missing_binary("foo", "0.2.0"),
            );
            let rpm = FakeRpm::new("unused", None);

            let planned = plan_component_update("foo", &c, &rpm, &rpm).expect("plan update");
            let Plan::Execute { steps, .. } = planned.plan else {
                panic!("expected an executable raw update");
            };
            let (resolution, prior) = planned.owned_execution.expect("owned resolution");
            let mut hydration_calls = 0;
            let mut effects = crate::test_support::RawEffectRecorder::default();
            effects.supported = false;
            let err = effects
                .with(|f| {
                    application::apply_owned_with_hydration(
                        &planned.target,
                        &c,
                        &layout,
                        &layout.state_dir.join("installed.toml"),
                        &rpm_install::journal_dir(&layout),
                        planned.scope,
                        &planned.now,
                        steps,
                        resolution,
                        prior,
                        &planned.command,
                        |store, layout| {
                            hydration_calls += 1;
                            common::hydrate_owned_file_contracts_with_capability_support(
                                store, layout, supported,
                            )
                        },
                        f,
                    )
                })
                .err()
                .expect("install of the new version must fail")
                .into_cli_error();
            assert_eq!(hydration_calls, 1);
            let expected = if inferred {
                vec!["capability factory user"]
            } else {
                vec![]
            };
            assert_eq!(effects.calls(), expected);
            assert!(effects.capability_contents().is_empty());
            assert!(
                err.reason().contains("the previous files were restored"),
                "{}",
                err.reason()
            );

            let log = match std::fs::read_to_string(&layout.central_log) {
                Ok(log) => log,
                Err(err) if !inferred && err.kind() == std::io::ErrorKind::NotFound => {
                    String::new()
                }
                Err(err) => panic!("read central log: {err}"),
            };
            let applied: Vec<serde_json::Value> = log
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|record| record["command"] == "capability:apply")
                .collect();
            assert_eq!(
                applied.len(),
                usize::from(inferred),
                "rollback must only attempt a supported inferred grant: {log}"
            );
            if inferred {
                assert_eq!(applied[0]["details"]["path"], bin.display().to_string());
                assert_eq!(applied[0]["details"]["caps"][0], "cap_net_bind_service");
            }

            let state_path = layout.state_dir.join("installed.toml");
            let store =
                StateStore::load_for_layout(&state_path, privilege::effective_uid(), &layout)
                    .expect("reload state");
            let ProviderBinding::Owned { artifact } = &store
                .find(ObjectKind::Component, "foo")
                .expect("component")
                .binding
            else {
                panic!("expected owned record");
            };
            let binary_row = artifact
                .files
                .iter()
                .find(|file| file.path == bin)
                .expect("binary row");
            assert!(
                binary_row.capabilities.is_empty(),
                "failed update must not persist inferred legacy metadata"
            );
            assert_eq!(
                std::fs::read(&bin).expect("read restored binary"),
                b"original v1 binary\n"
            );
        }
    }

    #[test]
    fn raw_effect_update_hydrates_legacy_capabilities_through_factory() {
        for supported in [false, true] {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let c = ctx(tmp.path().join("home"), InstallMode::User, false);
            seed_installed_raw(&c, "foo", "0.1.0", b"prior bytes\n");
            let layout = common::resolve_layout(&c);
            let bin = layout.bin_dir.join("foo");
            let manifest_path = common::installed_component_manifest_path(&layout, "foo", "update")
                .expect("manifest path");
            std::fs::write(
                manifest_path,
                format!(
                    "{}\n[[component.capabilities]]\npath = \"{{bindir}}/foo\"\ncaps = [\"cap_net_bind_service\"]\noptional = false\n",
                    raw_manifest("foo", "0.1.0")
                ),
            )
            .expect("legacy manifest");
            publish_raw_repo(
                &tmp.path().join("repo"),
                &layout,
                "foo",
                "0.2.0",
                &raw_artifact_missing_binary("foo", "0.2.0"),
            );
            let state_path = layout.state_dir.join("installed.toml");
            let before = std::fs::read(&state_path).expect("state");
            let rpm = FakeRpm::new("unused", None);
            let mut effects = crate::test_support::RawEffectRecorder::default();
            effects.supported = supported;
            let error = effects
                .with(|f| {
                    application::run_with_dependencies(
                        application::ApplicationRequest {
                            component: "foo",
                            intent: execution_intent(&c),
                        },
                        &c,
                        &rpm,
                        &rpm,
                        false,
                        f,
                    )
                })
                .err()
                .expect("new artifact is missing its binary");
            assert!(
                error.reason().contains("the previous files were restored"),
                "{error}"
            );
            let mut expected = vec!["capability factory user".to_string()];
            if supported {
                expected.extend([
                    "capability factory user".to_string(),
                    format!("apply {} cap_net_bind_service", bin.display()),
                ]);
            }
            assert_eq!(effects.calls(), expected);
            assert_eq!(
                effects.capability_contents(),
                if supported {
                    vec![b"prior bytes\n".to_vec()]
                } else {
                    vec![]
                }
            );
            assert_eq!(
                std::fs::read(&bin).expect("restored binary"),
                b"prior bytes\n"
            );
            assert_eq!(std::fs::read(&state_path).expect("unchanged state"), before);
        }
    }

    /// resolve_raw always selects the highest published version; if the index
    /// only offers an older release, update must refuse rather than downgrade
    /// (U4).
    #[test]
    fn raw_update_refuses_downgrade() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        let body: &[u8] = b"installed 0.2.0\n";
        seed_installed_raw(&c, "foo", "0.2.0", body);
        // The repo only publishes the older 0.1.0.
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.1.0",
            &raw_artifact("foo", "0.1.0", b"older\n"),
        );
        let rpm = FakeRpm::new("unused", None);

        let err = update_component_with_deps("foo", &c, &rpm, &rpm, false)
            .expect_err("a downgrade must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(
            err.reason().contains("refusing to downgrade"),
            "got: {}",
            err.reason()
        );

        let layout = common::resolve_layout(&c);
        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            body,
            "refused downgrade must not touch the binary"
        );
        assert_eq!(
            owned_artifact(&find_component(&c, "foo")).version,
            "0.2.0",
            "refused downgrade must not change the recorded version"
        );
    }

    /// A newer artifact that declares a dependency the host lacks must be
    /// refused with nothing effectively changed: the owned update runs the
    /// same runtime preflight as a fresh install during download-verify.
    #[test]
    fn raw_update_refuses_when_new_artifact_adds_unmet_dependency() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        let old_body: &[u8] = b"installed 0.1.0\n";
        seed_installed_raw(&c, "foo", "0.1.0", old_body);

        // v2's contract adds a system-package whose probe can never succeed, so
        // the preflight fails on every host.
        let deps = r#"
[[component.dependencies]]
name = "absent-tool"
kind = "system-package"
probe = "anolisa-nonexistent-probe-xyz --version"
packages = { rpm = "absent-tool", deb = "absent-tool" }
"#;
        let manifest = format!("{}{}", raw_manifest("foo", "0.2.0"), deps);
        let new_body: &[u8] = b"#!/bin/sh\necho foo v2\n";
        let artifact = tar_gz(&[
            (".anolisa/component.toml", manifest.as_bytes()),
            ("bin/foo", new_body),
        ]);
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &artifact,
        );
        let rpm = FakeRpm::new("unused", None);

        let err = update_component_with_deps("foo", &c, &rpm, &rpm, false)
            .expect_err("update must refuse when the new artifact adds an unmet dependency");
        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("missing runtime dependencies"),
            "error must come from the runtime preflight, got: {}",
            err.reason()
        );

        let layout = common::resolve_layout(&c);
        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            old_body,
            "refused update must not change the old binary"
        );
        assert_eq!(
            owned_artifact(&find_component(&c, "foo")).version,
            "0.1.0",
            "refused update must not change the recorded version"
        );
    }

    /// A successful update resets transient state: status returns to Installed
    /// and stale service rows from the old version are cleared (the new
    /// manifest declares no services here).
    #[test]
    fn raw_update_resets_status_and_clears_stale_state() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        seed_installed_raw(&c, "foo", "0.1.0", b"old\n");
        // Poison transient state as if a prior op had failed and left rows.
        {
            let layout = common::resolve_layout(&c);
            let path = layout.state_dir.join("installed.toml");
            let mut state = toml::from_str::<InstalledState>(
                &std::fs::read_to_string(&path).expect("read legacy fixture"),
            )
            .expect("load state");
            let obj = state
                .objects
                .iter_mut()
                .find(|object| object.kind == ObjectKind::Component && object.name == "foo")
                .expect("seeded object");
            obj.status = ObjectStatus::Failed;
            obj.services = vec![ServiceRef {
                name: "stale.service".to_string(),
                manager: "systemd".to_string(),
                restartable: true,
                enabled: false,
                scope: anolisa_core::ServiceScope::System,
            }];
            crate::test_support::write_legacy_state(&state, &path).expect("save poisoned state");
        }
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &raw_artifact("foo", "0.2.0", b"new\n"),
        );
        let rpm = FakeRpm::new("unused", None);

        update_component_with_deps("foo", &c, &rpm, &rpm, false).expect("update must succeed");

        let record = find_component(&c, "foo");
        let artifact = owned_artifact(&record);
        assert_eq!(artifact.version, "0.2.0");
        assert_eq!(
            record.status,
            LifecycleStatus::Installed,
            "status must reset to Installed after a clean update"
        );
        assert!(
            artifact.services.is_empty(),
            "stale services must be cleared when the new manifest declares none"
        );
    }

    /// version_relation classifies semver pairs and, crucially, refuses to
    /// guess a direction for non-semver versions so the downgrade guard holds.
    #[test]
    fn version_relation_classifies_semver_and_non_semver() {
        // Plain semver precedence.
        assert_eq!(version_relation("0.1.0", "0.2.0"), VersionRelation::Newer);
        assert_eq!(version_relation("0.2.0", "0.1.0"), VersionRelation::Older);
        assert_eq!(version_relation("1.0.0", "1.0.0"), VersionRelation::Same);
        // A leading `v` is normalized away before comparison.
        assert_eq!(version_relation("v1.2.3", "1.2.3"), VersionRelation::Same);
        // Non-semver: equal normalized strings are Same, anything else is
        // Indeterminate — never silently treated as an upgrade.
        assert_eq!(
            version_relation("2026.06", "2026.06"),
            VersionRelation::Same
        );
        assert_eq!(
            version_relation("2026.06", "0.5.0"),
            VersionRelation::Indeterminate
        );
        assert_eq!(
            version_relation("0.5.0", "nightly"),
            VersionRelation::Indeterminate
        );
    }

    /// A non-semver installed version cannot be ordered against the published
    /// one, so update refuses rather than risk replacing a newer custom build
    /// with an older published release (U4).
    #[test]
    fn raw_update_refuses_non_semver_version() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        let body: &[u8] = b"calver build\n";
        seed_installed_raw(&c, "foo", "2026.06", body);
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.5.0",
            &raw_artifact("foo", "0.5.0", b"older semver\n"),
        );
        let rpm = FakeRpm::new("unused", None);

        let err = update_component_with_deps("foo", &c, &rpm, &rpm, false)
            .expect_err("a non-orderable version must be refused");
        assert_eq!(err.code(), "INVALID_ARGUMENT");

        let layout = common::resolve_layout(&c);
        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            body,
            "refused update must not touch the binary"
        );
        assert_eq!(
            owned_artifact(&find_component(&c, "foo")).version,
            "2026.06",
            "refused update must not change the recorded version"
        );
    }

    /// The recorded version is re-validated under the install lock: a prior
    /// snapshot that no longer matches the locked read (a concurrent update
    /// landed in between) aborts before anything is touched.
    #[test]
    fn raw_update_aborts_on_concurrent_version_drift() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        let body: &[u8] = b"already at 0.2.0\n";
        seed_installed_raw(&c, "foo", "0.2.0", body);
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &raw_artifact("foo", "0.2.0", b"new payload\n"),
        );

        let layout = common::resolve_layout(&c);
        let state_path = layout.state_dir.join("installed.toml");
        let journal_dir = rpm_install::journal_dir(&layout);
        let env = anolisa_env::EnvService::detect();
        let repo_config =
            common::load_repo_config(&c, &layout, "update foo", RepoPersistPolicy::Require)
                .expect("repo config");
        let inputs = resolve_raw_inputs_for_component(
            "foo".to_string(),
            "raw",
            None,
            &env,
            &repo_config,
            "update foo",
        )
        .expect("resolve inputs");
        let resolution = resolve_raw(&c, &layout, &env, inputs).expect("resolve latest");

        // The pre-lock snapshot carries a stale version, as if a concurrent
        // update advanced the record between resolve and lock.
        let mut stale = owned_artifact(&find_component(&c, "foo"));
        stale.version = "0.1.0".to_string();

        let err = application::apply_owned(
            "foo",
            &c,
            &layout,
            &state_path,
            &journal_dir,
            InstallationScope::System,
            "2026-07-16T00:00:00Z",
            Vec::new(),
            resolution,
            stale,
            "update foo",
            crate::test_support::raw_effects(),
        )
        .err()
        .expect("a drifted snapshot must abort under the lock")
        .into_cli_error();
        assert_eq!(err.code(), "EXECUTION_FAILED");
        assert!(
            err.reason().contains("while this update was resolving"),
            "got: {}",
            err.reason()
        );

        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            body,
            "aborted update must not touch the binary"
        );
        assert_eq!(
            owned_artifact(&find_component(&c, "foo")).version,
            "0.2.0",
            "aborted update must not change the recorded version"
        );
    }

    /// A component installed with `--package` (recorded on the artifact as
    /// `raw_package`) updates against that package, not one re-derived from
    /// the component name. Published only under the non-default key `altpkg`,
    /// so a re-derived `foo` would resolve nothing.
    #[test]
    fn raw_update_reuses_recorded_package() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        seed_installed_raw(&c, "foo", "0.1.0", b"old foo\n");
        // Record the alternate package the component was installed from.
        {
            let layout = common::resolve_layout(&c);
            let path = layout.state_dir.join("installed.toml");
            let mut state = toml::from_str::<InstalledState>(
                &std::fs::read_to_string(&path).expect("read legacy fixture"),
            )
            .expect("load state");
            state
                .objects
                .iter_mut()
                .find(|object| object.kind == ObjectKind::Component && object.name == "foo")
                .expect("seeded object")
                .raw_package = Some("altpkg".to_string());
            crate::test_support::write_legacy_state(&state, &path).expect("save state");
        }
        let new_body: &[u8] = b"new foo fetched via altpkg\n";
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "altpkg",
            "0.2.0",
            &raw_artifact("foo", "0.2.0", new_body),
        );
        let rpm = FakeRpm::new("unused", None);

        update_component_with_deps("foo", &c, &rpm, &rpm, false)
            .expect("update must resolve via the recorded package");

        let layout = common::resolve_layout(&c);
        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            new_body,
            "binary must be replaced with the version fetched via the recorded package"
        );
        let artifact = owned_artifact(&find_component(&c, "foo"));
        assert_eq!(artifact.version, "0.2.0");
        assert_eq!(
            artifact.raw_package.as_deref(),
            Some("altpkg"),
            "the recorded package must survive the update"
        );
    }

    /// With no recorded package, update derives it from the component name; if
    /// the index only publishes a non-default package the resolve fails. This
    /// is the failing half that proves the recorded package is what made
    /// [`raw_update_reuses_recorded_package`] succeed.
    #[test]
    fn raw_update_without_recorded_package_cannot_find_alt_package() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        seed_installed_raw(&c, "foo", "0.1.0", b"old foo\n");
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "altpkg",
            "0.2.0",
            &raw_artifact("foo", "0.2.0", b"unreachable\n"),
        );
        let rpm = FakeRpm::new("unused", None);

        update_component_with_deps("foo", &c, &rpm, &rpm, false)
            .expect_err("deriving 'foo' must not resolve the 'altpkg'-only index");

        let layout = common::resolve_layout(&c);
        assert_eq!(
            std::fs::read(layout.bin_dir.join("foo")).expect("read bin"),
            b"old foo\n",
            "a failed resolve must not touch the binary"
        );
    }

    /// An owned component routes to the raw backend and never runs dnf —
    /// even when an RPM of the same name is installed.
    ///
    /// Uses System mode: `resolve_layout` honours `prefix` only for System
    /// mode, so a User-mode test would read and mutate the real user home.
    #[test]
    fn raw_component_update_never_runs_dnf() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        seed_installed_raw(&c, "copilot-shell", "0.1.0", b"old\n");
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "copilot-shell",
            "0.2.0",
            &raw_artifact("copilot-shell", "0.2.0", b"new\n"),
        );
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "2.2.0", Some("1.al8"), "x86_64")),
        );

        update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true)
            .expect("raw update must succeed");

        assert_eq!(
            rpm.update_calls.get(),
            0,
            "raw update must never run dnf on the system RPM"
        );
        assert_eq!(
            owned_artifact(&find_component(&c, "copilot-shell")).version,
            "0.2.0",
            "the raw component must be updated to the published version"
        );
    }

    // ── adapter receipts left stale by an update (issue #1885) ──

    use anolisa_core::adapter::claim::{
        AdapterClaim, AdapterSourceRevision, CLAIM_SCHEMA_VERSION, ClaimStatus, CoshClaim,
        DRIVER_SCHEMA_VERSION, DriverPayload, ManagedFileKind, ManagedSourceFile,
    };

    /// An enabled receipt carrying the managed source revision at enable time.
    pub(crate) fn enabled_claim(
        component: &str,
        framework: &str,
        resource_root: &Path,
    ) -> AdapterClaim {
        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: component.to_string(),
            framework: framework.to_string(),
            plugin_id: None,
            adapter_type: Some("extension".to_string()),
            enabled_at: "2026-07-01T00:00:00Z".to_string(),
            resource_root: resource_root.to_path_buf(),
            bundle_digest: None,
            source_revision: recorded_source_revision(resource_root),
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

    fn recorded_source_revision(root: &Path) -> Option<AdapterSourceRevision> {
        use sha2::{Digest, Sha256};

        fn collect(root: &Path, dir: &Path, files: &mut Vec<ManagedSourceFile>) -> Option<()> {
            for entry in std::fs::read_dir(dir).ok()? {
                let entry = entry.ok()?;
                let path = entry.path();
                let file_type = entry.file_type().ok()?;
                if file_type.is_dir() {
                    collect(root, &path, files)?;
                } else if file_type.is_symlink() {
                    files.push(ManagedSourceFile {
                        relative_path: path.strip_prefix(root).ok()?.to_path_buf(),
                        kind: ManagedFileKind::Symlink,
                        sha256: None,
                        symlink_target: Some(std::fs::read_link(path).ok()?),
                    });
                } else if file_type.is_file() {
                    files.push(ManagedSourceFile {
                        relative_path: path.strip_prefix(root).ok()?.to_path_buf(),
                        kind: ManagedFileKind::File,
                        sha256: Some(format!("{:x}", Sha256::digest(std::fs::read(path).ok()?))),
                        symlink_target: None,
                    });
                }
            }
            Some(())
        }

        let source_root = std::fs::canonicalize(root).ok()?;
        let mut files = Vec::new();
        collect(root, root, &mut files)?;
        files.sort();
        (!files.is_empty()).then_some(AdapterSourceRevision {
            source_root,
            files,
            materialized_sources: Vec::new(),
        })
    }

    /// Upsert a receipt into `ctx`'s seeded state.
    pub(crate) fn seed_claim(ctx: &CliContext, receipt: AdapterClaim) {
        let layout = common::resolve_layout(ctx);
        let state_path = layout.state_dir.join("installed.toml");
        let mut store =
            StateStore::load_for_layout(&state_path, 0, &layout).expect("load seeded state");
        store.upsert_adapter_claim(receipt);
        store.save(&state_path).expect("save state");
    }

    fn expected_action(component: &str, framework: &str) -> AdapterAction {
        AdapterAction {
            component: component.to_string(),
            framework: framework.to_string(),
            reason: claim::SOURCE_REVISION_CHANGED_REASON.to_string(),
            command: format!("anolisa adapter enable {component} {framework}"),
        }
    }

    fn expected_system_action(component: &str, framework: &str) -> AdapterAction {
        AdapterAction {
            component: component.to_string(),
            framework: framework.to_string(),
            reason: SYSTEM_REVISION_CHANGED_REASON.to_string(),
            command: format!("anolisa adapter status {component}"),
        }
    }

    /// `raw_manifest` plus an `[[adapters]]` entry so the artifact also
    /// ships an adapter resource bundle under `adapters/<component>/openclaw/`.
    pub(crate) fn raw_manifest_with_adapter(component: &str, version: &str) -> String {
        format!(
            r#"{base}
[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
source = "adapters/{component}/openclaw"
dest = "{{datadir}}/adapters/{{component}}/openclaw/"
"#,
            base = raw_manifest(component, version)
        )
    }

    /// tar.gz carrying the with-adapter manifest, the binary it declares,
    /// and an adapter bundle with the given `bundle` files.
    fn raw_artifact_with_adapter(
        component: &str,
        version: &str,
        body: &[u8],
        bundle: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let manifest = raw_manifest_with_adapter(component, version);
        let mut entries: Vec<(String, Vec<u8>)> = vec![
            (".anolisa/component.toml".to_string(), manifest.into_bytes()),
            (format!("bin/{component}"), body.to_vec()),
        ];
        entries.extend(bundle.iter().map(|(path, data)| {
            (
                format!("adapters/{component}/openclaw/{path}"),
                data.to_vec(),
            )
        }));
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(path, data)| (path.as_str(), data.as_slice()))
            .collect();
        tar_gz(&refs)
    }

    /// Seed the adapter bundle an earlier version shipped plus an enabled
    /// receipt whose managed source revision matches it; returns the root.
    fn seed_bundle_and_claim(ctx: &CliContext, component: &str, content: &[u8]) -> PathBuf {
        let layout = common::resolve_layout(ctx);
        let manifest_path =
            FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
        std::fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
            .expect("manifest dir");
        std::fs::write(
            &manifest_path,
            raw_manifest_with_adapter(component, "0.1.0"),
        )
        .expect("adapter manifest");
        let bundle_root = layout
            .datadir
            .join("adapters")
            .join(component)
            .join("openclaw");
        std::fs::create_dir_all(&bundle_root).expect("bundle root");
        std::fs::write(bundle_root.join("plugin.json"), content).expect("bundle file");
        seed_claim(ctx, enabled_claim(component, "openclaw", &bundle_root));
        bundle_root
    }

    /// Seed a raw-installed component that ships an adapter bundle: the
    /// record owns the bundle file (as a real install with `[[adapters]]`
    /// would), and an enabled receipt carries its managed source revision.
    /// Returns the bundle root.
    fn seed_raw_component_with_bundle(
        ctx: &CliContext,
        component: &str,
        version: &str,
        body: &[u8],
        bundle_content: &[u8],
    ) -> PathBuf {
        use sha2::{Digest, Sha256};
        seed_installed_raw(ctx, component, version, body);
        let layout = common::resolve_layout(ctx);
        let manifest_path =
            FsLayout::component_manifest_snapshot_path(&layout.state_dir, component);
        std::fs::write(
            &manifest_path,
            raw_manifest_with_adapter(component, version),
        )
        .expect("adapter manifest");
        let bundle_root = layout
            .datadir
            .join("adapters")
            .join(component)
            .join("openclaw");
        std::fs::create_dir_all(&bundle_root).expect("bundle root");
        std::fs::write(bundle_root.join("plugin.json"), bundle_content).expect("bundle file");

        // Record the bundle file as owned so the update may replace it —
        // placing over an unowned existing file is (rightly) refused. This
        // must stay a v4 write: seeding the claim below migrates the file
        // to v5.
        let state_path = layout.state_dir.join("installed.toml");
        let mut state = toml::from_str::<InstalledState>(
            &std::fs::read_to_string(&state_path).expect("read legacy fixture"),
        )
        .expect("load seeded state");
        let obj = state
            .objects
            .iter_mut()
            .find(|object| object.kind == ObjectKind::Component && object.name == component)
            .expect("seeded object");
        obj.files.push(OwnedFile {
            path: bundle_root.join("plugin.json"),
            owner: FileOwner::Anolisa,
            sha256: Some(format!("{:x}", Sha256::digest(bundle_content))),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        });
        crate::test_support::write_legacy_state(&state, &state_path).expect("save state");

        seed_claim(ctx, enabled_claim(component, "openclaw", &bundle_root));
        bundle_root
    }

    /// System updates must never inspect receipt-selected resource paths.
    #[test]
    fn system_update_actions_ignore_receipts() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("system"), InstallMode::System, false);
        let bundle_root = tmp.path().join("receipt-controlled");
        std::fs::create_dir_all(&bundle_root).expect("bundle root");
        std::fs::write(bundle_root.join("plugin.json"), b"v1").expect("bundle file");
        let mut store = StateStore::empty();
        store.upsert_adapter_claim(enabled_claim("tokenless", "openclaw", &bundle_root));
        std::fs::write(bundle_root.join("plugin.json"), b"v2").expect("updated bundle");

        assert!(
            adapter_actions_after_update(&c, &store, "tokenless", &AdapterRevisionSnapshot::new())
                .is_empty(),
            "system mode must ignore receipt-controlled bundle paths"
        );
    }

    /// A privileged source snapshot must not honor the caller-controlled
    /// packaged-data override.
    #[test]
    fn system_revision_snapshot_ignores_packaged_data_override() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let untrusted = tmp.path().join("caller-data");
        let contract = FsLayout::component_contract_path(&untrusted, "tokenless");
        std::fs::create_dir_all(contract.parent().expect("contract parent")).expect("contract dir");
        std::fs::write(contract, raw_manifest_with_adapter("tokenless", "1.0.0"))
            .expect("contract");
        let bundle = untrusted.join("adapters/tokenless/openclaw");
        std::fs::create_dir_all(&bundle).expect("bundle dir");
        std::fs::write(bundle.join("plugin.json"), b"untrusted").expect("bundle");

        let c = ctx(tmp.path().join("system"), InstallMode::System, false)
            .with_packaged_data_root(untrusted);
        let layout = common::resolve_layout(&c);

        assert!(
            adapter_revision_snapshot(&c, &layout, "tokenless").is_empty(),
            "system snapshots must ignore environment-selected data roots"
        );
    }

    /// A trusted source revision change yields a generic status action without
    /// loading any user's receipt.
    #[test]
    fn system_revision_change_reports_status_action() {
        let before = AdapterRevisionSnapshot::from([(
            "openclaw".to_string(),
            Some(test_source_revision("0")),
        )]);
        let after = AdapterRevisionSnapshot::from([(
            "openclaw".to_string(),
            Some(test_source_revision("1")),
        )]);

        assert_eq!(
            system_adapter_actions("tokenless", &before, &after),
            vec![expected_system_action("tokenless", "openclaw")]
        );
    }

    #[test]
    fn system_revision_becoming_unavailable_reports_status_action() {
        let before = AdapterRevisionSnapshot::from([(
            "openclaw".to_string(),
            Some(test_source_revision("0")),
        )]);
        let after = AdapterRevisionSnapshot::from([("openclaw".to_string(), None)]);

        assert_eq!(
            system_adapter_actions("tokenless", &before, &after),
            vec![expected_system_action("tokenless", "openclaw")]
        );
    }

    fn test_source_revision(digest_digit: &str) -> AdapterSourceRevision {
        AdapterSourceRevision {
            source_root: PathBuf::from("/usr/share/anolisa/adapters/tokenless/openclaw"),
            files: vec![ManagedSourceFile {
                relative_path: PathBuf::from("plugin.json"),
                kind: ManagedFileKind::File,
                sha256: Some(digest_digit.repeat(64)),
                symlink_target: None,
            }],
            materialized_sources: Vec::new(),
        }
    }

    /// A system raw update reports a trusted adapter source change without
    /// consulting a receipt.
    #[test]
    fn raw_update_reports_adapter_action_when_bundle_changed() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        let bundle_root =
            seed_raw_component_with_bundle(&c, "foo", "0.1.0", b"old v1 binary\n", b"v1");
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &raw_artifact_with_adapter("foo", "0.2.0", b"new\n", &[("plugin.json", b"v2")]),
        );
        let rpm = FakeRpm::new("unused", None);

        let (outcome, actions) =
            update_component_with_deps("foo", &c, &rpm, &rpm, false).expect("update ok");

        assert_eq!(outcome, UpdateOutcome::Updated);
        assert_eq!(
            std::fs::read(bundle_root.join("plugin.json")).expect("read bundle"),
            b"v2",
            "the update itself must have replaced the bundle"
        );
        assert_eq!(
            actions,
            vec![expected_system_action("foo", "openclaw")],
            "one changed trusted bundle yields one system action"
        );
    }

    /// The same update with an unchanged bundle reports nothing.
    #[test]
    fn raw_update_with_unchanged_bundle_reports_no_adapter_action() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        seed_raw_component_with_bundle(&c, "foo", "0.1.0", b"old v1 binary\n", b"v1");
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &raw_artifact_with_adapter("foo", "0.2.0", b"new\n", &[("plugin.json", b"v1")]),
        );
        let rpm = FakeRpm::new("unused", None);
        let layout = common::resolve_layout(&c);
        let before = adapter_revision_snapshot(&c, &layout, "foo");

        let (outcome, actions) =
            update_component_with_deps("foo", &c, &rpm, &rpm, false).expect("update ok");
        let after = adapter_revision_snapshot(&c, &layout, "foo");

        assert_eq!(outcome, UpdateOutcome::Updated);
        assert_eq!(before, after, "unchanged managed adapter revision");
        assert!(
            actions.is_empty(),
            "an unchanged bundle must not report an action: {actions:?}"
        );
    }

    /// An already-current update is a no-op and must not claim a
    /// post-update mismatch — even when the receipt was already stale
    /// before the update ran.
    #[test]
    fn already_current_update_reports_no_adapter_action_even_with_stale_receipt() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        seed_installed_raw(&c, "foo", "0.2.0", b"current binary\n");
        let layout = common::resolve_layout(&c);
        let bundle_root = layout.datadir.join("adapters/foo/openclaw");
        std::fs::create_dir_all(&bundle_root).expect("bundle root");
        std::fs::write(bundle_root.join("plugin.json"), b"v2").expect("bundle file");
        // A receipt stale from an earlier drift — not from this update.
        seed_claim(&c, enabled_claim("foo", "openclaw", &bundle_root));
        std::fs::write(bundle_root.join("plugin.json"), b"already stale")
            .expect("drift bundle file");
        publish_raw_repo(
            &tmp.path().join("repo"),
            &layout,
            "foo",
            "0.2.0",
            &raw_artifact("foo", "0.2.0", b"unused\n"),
        );
        let rpm = FakeRpm::new("unused", None);

        let (outcome, actions) =
            update_component_with_deps("foo", &c, &rpm, &rpm, false).expect("no-op ok");

        assert_eq!(outcome, UpdateOutcome::AlreadyCurrent);
        assert!(
            actions.is_empty(),
            "a no-op update must not report adapter actions: {actions:?}"
        );
    }

    /// A failed update reports no action and leaves the receipt in place
    /// so the retry (or `adapter status`) still sees it.
    #[test]
    fn failed_update_reports_no_adapter_action_and_keeps_receipt() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        seed_installed_raw(&c, "foo", "0.1.0", b"original v1 binary\n");
        let layout = common::resolve_layout(&c);
        let bundle_root = layout.datadir.join("adapters/foo/openclaw");
        std::fs::create_dir_all(&bundle_root).expect("bundle root");
        std::fs::write(bundle_root.join("plugin.json"), b"v1").expect("bundle file");
        let receipt = enabled_claim("foo", "openclaw", &bundle_root);
        seed_claim(&c, receipt.clone());
        publish_raw_repo(
            &tmp.path().join("repo"),
            &layout,
            "foo",
            "0.2.0",
            &raw_artifact_missing_binary("foo", "0.2.0"),
        );
        let rpm = FakeRpm::new("unused", None);

        let err = update_component_with_deps("foo", &c, &rpm, &rpm, false)
            .expect_err("the new artifact is broken; the update must fail");
        assert_eq!(err.code(), "EXECUTION_FAILED");

        let store = load_store(&c);
        assert_eq!(
            store.find_adapter_claim("foo", "openclaw"),
            Some(&receipt),
            "a failed update must not touch the receipt"
        );
    }

    /// A successful update never rewrites the enable-time source revision:
    /// the framework side was not reapplied, so the receipt stays stale until
    /// an explicit `adapter enable`.
    #[test]
    fn successful_update_leaves_the_receipt_untouched() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("sys"), InstallMode::System, false);
        let bundle_root =
            seed_raw_component_with_bundle(&c, "foo", "0.1.0", b"old v1 binary\n", b"v1");
        let receipt = enabled_claim("foo", "openclaw", &bundle_root);
        publish_raw_repo(
            &tmp.path().join("repo"),
            &common::resolve_layout(&c),
            "foo",
            "0.2.0",
            &raw_artifact_with_adapter("foo", "0.2.0", b"new\n", &[("plugin.json", b"v2")]),
        );
        let rpm = FakeRpm::new("unused", None);

        let (_, actions) =
            update_component_with_deps("foo", &c, &rpm, &rpm, false).expect("update ok");
        assert_eq!(actions, vec![expected_system_action("foo", "openclaw")]);

        let store = load_store(&c);
        assert_eq!(
            store.find_adapter_claim("foo", "openclaw"),
            Some(&receipt),
            "update must report the drift, not erase it"
        );
    }

    /// A delegated update cannot infer adapter drift from directory bytes
    /// when its test-only native backend supplies no rpmdb file inventory.
    #[test]
    fn delegated_update_requires_authoritative_inventory_for_adapter_actions() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "1.0.0-1.al8",
                Ownership::RpmManaged,
                ObjectStatus::Installed,
            ),
        );
        let bundle_root = seed_bundle_and_claim(&c, "copilot-shell", b"v1");

        let replaced = bundle_root.clone();
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "1.0.0", Some("1.al8"), "x86_64")),
        )
        .upgrading_to(pkg_info("copilot-shell", "1.1.0", Some("1.al8"), "x86_64"))
        .with_on_update(Box::new(move || {
            // The upgraded RPM replaces the datadir bundle.
            std::fs::write(replaced.join("plugin.json"), b"v2").expect("bundle replaced");
        }));

        let (outcome, actions) =
            update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true).expect("update ok");

        assert_eq!(outcome, UpdateOutcome::Updated);
        assert_eq!(rpm.update_calls.get(), 1);
        assert!(
            actions.is_empty(),
            "unmanaged directory bytes must not stand in for rpmdb metadata: {actions:?}"
        );
        assert_eq!(update_operation_status(&c, "copilot-shell"), "ok");
    }

    /// A delegated no-op (dnf ran, nothing moved) changed no files, so it
    /// reports no action even with a stale receipt present.
    #[test]
    fn delegated_noop_update_reports_no_adapter_action() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, false);
        seed(
            &c,
            rpm_object(
                "copilot-shell",
                "copilot-shell",
                "1.0.0-1.al8",
                Ownership::RpmManaged,
                ObjectStatus::Installed,
            ),
        );
        let layout = common::resolve_layout(&c);
        let bundle_root = layout.datadir.join("adapters/copilot-shell/openclaw");
        std::fs::create_dir_all(&bundle_root).expect("bundle root");
        std::fs::write(bundle_root.join("plugin.json"), b"v1").expect("bundle file");
        seed_claim(&c, enabled_claim("copilot-shell", "openclaw", &bundle_root));
        // upgrade_to is None => dnf update is a no-op; EVR stays the same.
        let rpm = FakeRpm::new(
            "copilot-shell",
            Some(pkg_info("copilot-shell", "1.0.0", Some("1.al8"), "x86_64")),
        );

        let (outcome, actions) =
            update_component_with_deps("copilot-shell", &c, &rpm, &rpm, true).expect("update ok");

        assert_eq!(outcome, UpdateOutcome::AlreadyCurrent);
        assert!(
            actions.is_empty(),
            "a no-op update must not report adapter actions: {actions:?}"
        );
    }

    /// The `adapter_actions` key is stable wire surface: always present,
    /// with exactly `component`/`framework`/`reason`/`command` per entry
    /// (issue #1885).
    #[test]
    fn update_result_json_carries_stable_adapter_actions_shape() {
        let payload = UpdateResultPayload {
            component: "foo".to_string(),
            package: Some("foo".to_string()),
            from_version: Some("0.1.0".to_string()),
            to_version: Some("0.2.0".to_string()),
            updated: true,
            dry_run: false,
            plan: vec!["place files".to_string()],
            operation_id: Some("op-update-1".to_string()),
            adapter_actions: vec![expected_action("foo", "openclaw")],
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(
            json["adapter_actions"],
            serde_json::json!([{
                "component": "foo",
                "framework": "openclaw",
                "reason": "adapter source revision changed since enable",
                "command": "anolisa adapter enable foo openclaw",
            }]),
            "{json}"
        );

        let mut empty = payload;
        empty.adapter_actions = Vec::new();
        let json = serde_json::to_value(&empty).expect("serialize");
        assert_eq!(
            json["adapter_actions"],
            serde_json::json!([]),
            "the key stays present and empty when no receipt went stale: {json}"
        );
    }

    // ── CLI surface: `update <component>` is the direct form ────────────

    use clap::Parser;

    /// `update <component>` parses to the positional, with no subcommand.
    #[test]
    fn update_component_parses_as_positional() {
        let a = UpdateArgs::try_parse_from(["update", "copilot-shell"]).expect("parse");
        assert_eq!(a.component.as_deref(), Some("copilot-shell"));
        assert!(a.command.is_none());
    }

    /// `update self` parses to the self subcommand, not a component named
    /// "self" (subcommands take precedence over the positional).
    #[test]
    fn update_self_parses_as_subcommand() {
        let a = UpdateArgs::try_parse_from(["update", "self"]).expect("parse");
        assert!(matches!(a.command, Some(UpdateCommands::SelfBin)));
        assert!(a.component.is_none());
    }

    /// `update all` parses to the all subcommand.
    #[test]
    fn update_all_parses_as_subcommand() {
        let a = UpdateArgs::try_parse_from(["update", "all"]).expect("parse");
        assert!(matches!(a.command, Some(UpdateCommands::All)));
        assert!(a.component.is_none());
    }

    /// A positional and a subcommand are mutually exclusive.
    #[test]
    fn update_component_with_subcommand_is_a_parse_error() {
        UpdateArgs::try_parse_from(["update", "copilot-shell", "self"])
            .expect_err("positional + subcommand must conflict");
    }

    /// `update` with no target is an INVALID_ARGUMENT, not a panic or a silent
    /// no-op — the dispatcher needs a target.
    #[test]
    fn update_with_no_target_is_invalid_argument() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().to_path_buf(), InstallMode::System, true);
        let args = UpdateArgs {
            component: None,
            command: None,
            check: false,
            motd: false,
            refresh: false,
            target: None,
        };
        let err = handle(args, &c).expect_err("must require a target");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("specify a component"));
    }
    #[test]
    fn raw_effect_update_routes_scopes_and_preserves_restart_warnings() {
        use anolisa_core::ServiceOp;
        for mode in [InstallMode::System, InstallMode::User] {
            for fail in [None, Some(ServiceOp::Restart)] {
                let tmp = tempfile::tempdir().expect("tmpdir");
                let c = ctx(tmp.path().join("root"), mode, false);
                seed_installed_raw(&c, "foo", "0.1.0", b"old binary\n");
                let layout = common::resolve_layout(&c);
                let scope = if mode == InstallMode::User {
                    "user"
                } else {
                    "system"
                };
                let manifest = format!(
                    "{}\n[[component.services]]\nunit = \"foo.service\"\nscope = \"{scope}\"\nenable = true\nstart = true\n\n[[component.capabilities]]\npath = \"{{bindir}}/foo\"\ncaps = [\"CAP_BPF\"]\noptional = true\n",
                    raw_manifest("foo", "0.2.0")
                );
                publish_raw_repo(
                    &tmp.path().join("repo"),
                    &layout,
                    "foo",
                    "0.2.0",
                    &tar_gz(&[
                        (".anolisa/component.toml", manifest.as_bytes()),
                        ("bin/foo", b"new binary\n"),
                    ]),
                );
                let rpm = FakeRpm::new("unused", None);
                let mut effects = crate::test_support::RawEffectRecorder::default();
                effects.fail_service = fail;
                let outcome = effects
                    .with(|f| {
                        application::run_with_dependencies(
                            application::ApplicationRequest {
                                component: "foo",
                                intent: execution_intent(&c),
                            },
                            &c,
                            &rpm,
                            &rpm,
                            false,
                            f,
                        )
                    })
                    .expect("best effort update");
                let scope_label = if mode == InstallMode::User {
                    "User"
                } else {
                    "System"
                };
                assert_eq!(
                    effects.calls(),
                    vec![
                        format!("capability factory {scope}"),
                        format!("capability factory {scope}"),
                        format!("apply {} CAP_BPF", layout.bin_dir.join("foo").display()),
                        format!("service factory {scope} {scope_label}"),
                        format!("DaemonReload {scope_label} "),
                        format!("Enable {scope_label} foo.service"),
                        format!("Restart {scope_label} foo.service"),
                    ]
                );
                assert_eq!(
                    outcome
                        .warnings()
                        .iter()
                        .filter(|w| w.contains("restart foo.service failed"))
                        .count(),
                    usize::from(fail.is_some())
                );
                assert_eq!(owned_artifact(&find_component(&c, "foo")).version, "0.2.0");
                render_application_outcome(&c, outcome).expect("existing renderer");
            }
        }
    }
    #[test]
    fn raw_effect_replay_failed_capability_restore_retains_error_context() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let c = ctx(tmp.path().join("root"), InstallMode::System, false);
        seed_installed_raw(&c, "foo", "0.1.0", b"prior bytes\n");
        let layout = common::resolve_layout(&c);
        let bin = layout.bin_dir.join("foo");
        let mut store = load_store(&c);
        let ProviderBinding::Owned { artifact } = &mut store
            .find_mut(ObjectKind::Component, "foo")
            .expect("record")
            .binding
        else {
            panic!("owned");
        };
        artifact
            .files
            .iter_mut()
            .find(|f| f.path == bin)
            .expect("binary")
            .capabilities = vec!["CAP_BPF".to_string()];
        store
            .save(&layout.state_dir.join("installed.toml"))
            .expect("prior grant");
        publish_raw_repo(
            &tmp.path().join("repo"),
            &layout,
            "foo",
            "0.2.0",
            &raw_artifact_missing_binary("foo", "0.2.0"),
        );
        let rpm = FakeRpm::new("unused", None);
        let mut effects = crate::test_support::RawEffectRecorder::default();
        effects.fail_capability = true;
        let error = effects
            .with(|f| {
                application::run_with_dependencies(
                    application::ApplicationRequest {
                        component: "foo",
                        intent: execution_intent(&c),
                    },
                    &c,
                    &rpm,
                    &rpm,
                    false,
                    f,
                )
            })
            .err()
            .expect("failed replay");
        assert_eq!(error.code(), "EXECUTION_FAILED");
        assert_eq!(error.exit_code(), 1);
        assert!(
            error
                .reason()
                .contains("restored files but could not re-apply their file capabilities"),
            "{error}"
        );
        assert_eq!(
            effects.calls(),
            vec![
                "capability factory system".to_string(),
                "capability factory system".to_string(),
                format!("apply {} CAP_BPF", bin.display())
            ]
        );
        assert_eq!(
            effects.capability_contents(),
            vec![b"prior bytes\n".to_vec()]
        );
        assert_eq!(std::fs::read(&bin).expect("restored"), b"prior bytes\n");
        assert_eq!(owned_artifact(&find_component(&c, "foo")).version, "0.1.0");
    }
}
