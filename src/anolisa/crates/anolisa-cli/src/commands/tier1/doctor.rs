//! `anolisa doctor [COMPONENT]` — read-only component diagnostics.
//!
//! Doctor layers actionable findings and remediation suggestions on top of the
//! same state, health, runtime-dependency, and rpmdb probes used by the rest of
//! the CLI. It does not mutate host state; `--fix` is reserved until a repair
//! executor exists.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg(test)]
use anolisa_core::NotSupportedServiceManager;
use anolisa_core::domain::{
    Installation, InstallationScope, LifecycleStatus, ManagementRelation, ProviderBinding,
};
use anolisa_core::facts::{FactsError, JournalEvidence, JournalInventory};
use anolisa_core::manifest::RuntimeDependency;
use anolisa_core::{
    CheckEnv, CheckOutcome, CheckSpec, CheckStatus, ComponentManifest, ComponentSnapshot,
    DependencyKind, DependencyResolution, DependencyResolver, DependencyStatus, HealthEntry,
    IntegrityStatus, ManifestHealthProvenance, ManifestHealthSnapshot, NativePackageProvenance,
    NativePackageSnapshot, ProbeEvidence, ResolutionPlan, ResolverEnv, ResolverError,
    ServiceManager, ServiceRef, ServiceScope, ServiceState, StateRootScope, StateSnapshot,
    check_owned_file, run_check, service_for_install_mode, user_service_for_install_mode,
};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::privilege;
use anolisa_platform::rpm_query::RpmPackageQuery;
use chrono::{SecondsFormat, Utc};
use clap::Parser;
use serde::Serialize;

use crate::color::Palette;
use crate::commands::common;
use crate::commands::state_view::{ScopedStateRoot, StateScope, StateView, StateVisibility};
use crate::commands::tier1::component_observation::{
    self, AggregateSelection, RpmDrift, drift_probe_identity, rpm_drift_from_evidence,
};
use crate::commands::tier1::rpm_install;
use crate::context::{CliContext, InstallMode};
use crate::response::{CliError, render_json_with_status};

const COMMAND: &str = "doctor";

#[derive(Parser)]
pub struct DoctorArgs {
    /// Diagnose a specific component (default: all active installations).
    pub component: Option<String>,
    /// Reserved for automatic fixes; this release reports suggestions only.
    ///
    /// `doctor` is read-only. This option returns `NOT_IMPLEMENTED` without
    /// applying the reported fix plan; combining it with `--dry-run` is
    /// rejected as `INVALID_ARGUMENT`.
    #[arg(long)]
    pub fix: bool,
}

#[derive(Debug, Serialize)]
struct DoctorPayload {
    summary: DoctorSummary,
    components: Vec<DoctorComponent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    recovery_roots: Vec<DoctorRecoveryRoot>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct DoctorSummary {
    components_checked: usize,
    ok: usize,
    degraded: usize,
    failed: usize,
    recovery_roots_failed: usize,
    findings: usize,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorRecoveryRoot {
    scope: String,
    status: String,
    state_path: String,
    journal_dir: String,
    findings: Vec<DoctorFinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fix_plan: Vec<FixSuggestion>,
}

#[derive(Debug, Serialize)]
struct DoctorComponent {
    name: String,
    status: String,
    scope: String,
    #[serde(skip)]
    remediation_scope: StateScope,
    active: bool,
    mutable_by_current_invocation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    shadowed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    findings: Vec<DoctorFinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    health_checks: Vec<DoctorHealthCheck>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dependencies: Vec<DoctorDependency>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fix_plan: Vec<FixSuggestion>,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
/// Doctor fields shared with the snapshot conformance suite.
pub(super) struct DoctorConformanceProjection {
    /// Resolved component identity.
    pub(super) name: String,
    /// Aggregate doctor status.
    pub(super) status: String,
    /// State root scope projected by doctor.
    pub(super) scope: String,
    /// Whether this record wins multi-root precedence.
    pub(super) active: bool,
    /// Whether the current invocation may mutate this record.
    pub(super) mutable_by_current_invocation: bool,
    /// Higher-precedence scope hiding this record.
    pub(super) shadowed_by: Option<String>,
    /// State file that supplied this record.
    pub(super) state_path: Option<String>,
    /// Lifecycle status recorded in state.
    pub(super) state_status: Option<String>,
    /// Component version projected from state.
    pub(super) version: Option<String>,
    /// Finding codes emitted by the production diagnosis path.
    pub(super) finding_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DoctorFinding {
    severity: FindingSeverity,
    code: String,
    message: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum FindingSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DoctorHealthCheck {
    name: String,
    status: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DoctorDependency {
    name: String,
    kind: DependencyKind,
    status: DoctorDependencyStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DoctorDependencyStatus {
    Resolved,
    Unresolved,
    Unresolvable,
    Skipped,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct FixSuggestion {
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    reason: String,
    automatic: bool,
}

type ResolveRuntimeDependencies<'a> =
    dyn Fn(&[RuntimeDependency], &ResolverEnv) -> Result<ResolutionPlan, ResolverError> + 'a;

#[cfg(test)]
fn unexpected_runtime_dependencies(
    _: &[RuntimeDependency],
    _: &ResolverEnv,
) -> Result<ResolutionPlan, ResolverError> {
    panic!("runtime dependency resolution must not run for this fixture")
}

struct DoctorProbeContext<'a> {
    layout: &'a FsLayout,
    resolver_env: &'a ResolverEnv,
    resolve_runtime_dependencies: &'a ResolveRuntimeDependencies<'a>,
    rpm_query: &'a dyn PackageQuery,
    system_service: &'a dyn ServiceManager,
    user_service: &'a dyn ServiceManager,
    dry_run: bool,
}

struct DoctorViewContext<'a> {
    resolver_env: &'a ResolverEnv,
    resolve_runtime_dependencies: &'a ResolveRuntimeDependencies<'a>,
    rpm_query: &'a dyn PackageQuery,
    current_system_service: &'a dyn ServiceManager,
    system_scope_service: &'a dyn ServiceManager,
    user_service: &'a dyn ServiceManager,
    dry_run: bool,
}

enum DoctorJournalScan {
    Trusted(JournalInventory),
    Blocked(DoctorRecoveryRoot),
}

struct DoctorJournalRoot {
    scope: StateScope,
    state_path: String,
    journal_dir: String,
    scan: DoctorJournalScan,
}

impl<'a> DoctorViewContext<'a> {
    fn system_service_for_root(&self, root: Option<&ScopedStateRoot>) -> &'a dyn ServiceManager {
        match root.map(|root| root.scope) {
            Some(StateScope::System) => self.system_scope_service,
            Some(StateScope::User) | None => self.current_system_service,
        }
    }
}

pub fn handle(args: DoctorArgs, ctx: &CliContext) -> Result<(), CliError> {
    let command = match &args.component {
        Some(comp) => format!("doctor {comp}"),
        None => COMMAND.to_string(),
    };

    if ctx.dry_run && args.fix {
        return Err(CliError::InvalidArgument {
            command,
            reason: "--dry-run --fix is invalid; --dry-run alone prints fix plan, --fix executes"
                .to_string(),
        });
    }
    if args.fix {
        return Err(CliError::not_implemented_with_hint(
            command,
            "doctor is read-only in this release; rerun without --fix to inspect fix_plan suggestions",
        ));
    }

    let payload = diagnose(args.component.as_deref(), ctx)?;
    let has_issues = payload_has_issues(&payload);
    render_doctor(ctx, &payload, !has_issues)?;
    if has_issues {
        return Err(CliError::DiagnosticsFound {
            command: COMMAND.to_string(),
        });
    }
    Ok(())
}

fn payload_has_issues(payload: &DoctorPayload) -> bool {
    payload.summary.failed > 0
        || payload.summary.degraded > 0
        || payload.summary.recovery_roots_failed > 0
}

fn diagnose(component: Option<&str>, ctx: &CliContext) -> Result<DoctorPayload, CliError> {
    let mut view = StateView::load(ctx, COMMAND, StateVisibility::UserPlusSystem)?;
    component_observation::normalize_view_states(&mut view);
    let rpm_query = RpmPackageQuery::system();
    let component = component
        .map(|name| lookup_component_name_from_view(name, &view, ctx))
        .transpose()?;
    let env = anolisa_env::EnvService::detect();
    let resolver_env = resolver_env_from_facts(&env);
    let resolver = DependencyResolver::system();
    let resolve_runtime_dependencies =
        |deps: &[RuntimeDependency], env: &ResolverEnv| resolver.resolve(deps, env);
    let current_system_service = service_for_install_mode(ctx.install_mode.as_str(), &env);
    let system_scope_service = service_for_install_mode(InstallMode::System.as_str(), &env);
    let user_service = user_service_for_install_mode(ctx.install_mode.as_str(), &env);
    let view_ctx = DoctorViewContext {
        resolver_env: &resolver_env,
        resolve_runtime_dependencies: &resolve_runtime_dependencies,
        rpm_query: &rpm_query,
        current_system_service: current_system_service.as_ref(),
        system_scope_service: system_scope_service.as_ref(),
        user_service: user_service.as_ref(),
        dry_run: ctx.dry_run,
    };

    diagnose_from_view(&view, component.as_deref(), &view_ctx)
}

fn diagnose_from_view(
    view: &StateView,
    component: Option<&str>,
    view_ctx: &DoctorViewContext<'_>,
) -> Result<DoctorPayload, CliError> {
    let records = component_observation::select_visible_components(
        view,
        component,
        AggregateSelection::ActiveOnly,
    );
    let warnings = view.warnings.clone();
    let mut components = Vec::new();
    let journal_roots = scan_journal_roots(view);
    let mut claimed_journals = BTreeSet::new();
    let checked_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    for record in records {
        let root = record.root;
        let remediation_scope = root.scope;
        let layout = &root.layout;
        let (manifest, manifest_warning) = resolve_component_manifest(layout, &record.object.name);
        let probe_ctx = DoctorProbeContext {
            layout,
            resolver_env: view_ctx.resolver_env,
            resolve_runtime_dependencies: view_ctx.resolve_runtime_dependencies,
            rpm_query: view_ctx.rpm_query,
            system_service: view_ctx.system_service_for_root(Some(root)),
            user_service: view_ctx.user_service,
            dry_run: view_ctx.dry_run,
        };
        let snapshot = collect_doctor_snapshot(
            &record,
            manifest.as_ref(),
            manifest_warning.as_deref(),
            &probe_ctx,
            &checked_at,
        )?;
        let mut diagnosed = diagnose_component(
            &snapshot,
            remediation_scope,
            manifest.as_ref(),
            manifest_warning,
            &probe_ctx,
            &checked_at,
        )?;
        apply_component_journal_guard(&journal_roots, &mut diagnosed, &mut claimed_journals);
        dedupe_fix_plan(&mut diagnosed.fix_plan);
        diagnosed.status = component_status(&diagnosed);
        components.push(diagnosed);
    }

    if components.is_empty()
        && let Some(component) = component
    {
        let scope = installation_scope_for_root(view.writable.scope);
        let snapshot = component_observation::absent_snapshot(
            component,
            scope,
            view.writable.state_path.clone(),
        )
        .map_err(snapshot_cli_error)?;
        let probe_ctx = DoctorProbeContext {
            layout: &view.writable.layout,
            resolver_env: view_ctx.resolver_env,
            resolve_runtime_dependencies: view_ctx.resolve_runtime_dependencies,
            rpm_query: view_ctx.rpm_query,
            system_service: view_ctx.system_service_for_root(None),
            user_service: view_ctx.user_service,
            dry_run: view_ctx.dry_run,
        };
        let mut diagnosed = diagnose_component(
            &snapshot,
            view.writable.scope,
            None,
            None,
            &probe_ctx,
            &checked_at,
        )?;
        dedupe_fix_plan(&mut diagnosed.fix_plan);
        diagnosed.status = component_status(&diagnosed);
        components.push(diagnosed);
    }

    let recovery_roots = collect_root_recovery(&journal_roots, &claimed_journals, component);
    let summary = summarize(&components, &recovery_roots);
    Ok(DoctorPayload {
        summary,
        components,
        recovery_roots,
        warnings,
        dry_run: view_ctx.dry_run,
    })
}

fn snapshot_cli_error(error: component_observation::ComponentObservationError) -> CliError {
    CliError::Runtime {
        command: COMMAND.to_string(),
        reason: format!("failed to assemble component status snapshot: {error}"),
    }
}

fn installation_scope_for_root(scope: StateScope) -> InstallationScope {
    match scope {
        StateScope::System => InstallationScope::System,
        StateScope::User => InstallationScope::User {
            uid: privilege::effective_uid(),
        },
    }
}

fn collect_doctor_snapshot(
    record: &crate::commands::state_view::ScopedInstalledObject<'_>,
    manifest: Option<&ComponentManifest>,
    manifest_warning: Option<&str>,
    probe_ctx: &DoctorProbeContext<'_>,
    observed_at: &str,
) -> Result<ComponentSnapshot, CliError> {
    let owned_files = component_observation::owned_files_evidence(record);
    let native_package =
        collect_doctor_native_package(record.object, probe_ctx.rpm_query, observed_at);
    let manifest_health =
        collect_doctor_manifest_health(record.object, manifest, manifest_warning, probe_ctx);
    component_observation::snapshot_from_record(
        record,
        owned_files,
        native_package,
        manifest_health,
        ProbeEvidence::NotRequested,
        ProbeEvidence::NotRequested,
    )
    .map_err(snapshot_cli_error)
}

#[cfg(test)]
/// Collects the production doctor snapshot with command-specific probes disabled.
pub(super) fn snapshot_for_conformance(
    record: &crate::commands::state_view::ScopedInstalledObject<'_>,
    rpm_query: &dyn PackageQuery,
    observed_at: &str,
) -> Result<ComponentSnapshot, CliError> {
    let resolver_env = ResolverEnv::default();
    let service = NotSupportedServiceManager::new("conformance probe disabled".to_string());
    let probe_ctx = DoctorProbeContext {
        layout: &record.root.layout,
        resolver_env: &resolver_env,
        resolve_runtime_dependencies: &unexpected_runtime_dependencies,
        rpm_query,
        system_service: &service,
        user_service: &service,
        dry_run: false,
    };
    collect_doctor_snapshot(record, None, None, &probe_ctx, observed_at)
}

#[cfg(test)]
/// Projects a snapshot through the production doctor diagnosis path.
pub(super) fn projection_for_conformance(
    snapshot: &ComponentSnapshot,
    layout: &FsLayout,
    rpm_query: &dyn PackageQuery,
    checked_at: &str,
) -> Result<DoctorConformanceProjection, CliError> {
    let resolver_env = ResolverEnv::default();
    let service = NotSupportedServiceManager::new("conformance probe disabled".to_string());
    let probe_ctx = DoctorProbeContext {
        layout,
        resolver_env: &resolver_env,
        resolve_runtime_dependencies: &unexpected_runtime_dependencies,
        rpm_query,
        system_service: &service,
        user_service: &service,
        dry_run: false,
    };
    let remediation_scope = match snapshot.request().scope() {
        InstallationScope::System => StateScope::System,
        InstallationScope::User { .. } => StateScope::User,
    };
    let mut component = diagnose_component(
        snapshot,
        remediation_scope,
        None,
        None,
        &probe_ctx,
        checked_at,
    )?;
    component.status = component_status(&component);
    Ok(DoctorConformanceProjection {
        name: component.name,
        status: component.status,
        scope: component.scope,
        active: component.active,
        mutable_by_current_invocation: component.mutable_by_current_invocation,
        shadowed_by: component.shadowed_by,
        state_path: component.state_path,
        state_status: component.state_status,
        version: component.version,
        finding_codes: component
            .findings
            .into_iter()
            .map(|finding| finding.code)
            .collect(),
    })
}

fn collect_doctor_native_package(
    installation: &Installation,
    query: &dyn PackageQuery,
    observed_at: &str,
) -> ProbeEvidence<NativePackageSnapshot, NativePackageProvenance> {
    let ProviderBinding::Delegated { pm, package, .. } = &installation.binding else {
        return ProbeEvidence::NotRequested;
    };
    if !matches!(installation.scope, InstallationScope::System) {
        return ProbeEvidence::NotRequested;
    }
    let Some(package) = package.resolved_name() else {
        return ProbeEvidence::NotRequested;
    };
    component_observation::native_package_evidence(*pm, package, query, observed_at)
}

fn collect_doctor_manifest_health(
    installation: &Installation,
    manifest: Option<&ComponentManifest>,
    manifest_warning: Option<&str>,
    probe_ctx: &DoctorProbeContext<'_>,
) -> ProbeEvidence<ManifestHealthSnapshot, ManifestHealthProvenance> {
    let provenance = ManifestHealthProvenance {
        path: common::installed_component_manifest_path(
            probe_ctx.layout,
            &installation.name,
            COMMAND,
        )
        .unwrap_or_else(|_| {
            probe_ctx
                .layout
                .state_dir
                .join("component-manifests")
                .join(&installation.name)
                .join("component.toml")
        }),
    };
    let Some(manifest) = manifest else {
        return match manifest_warning {
            Some(reason) if !reason.starts_with("component contract unavailable") => {
                ProbeEvidence::Unavailable {
                    provenance,
                    reason: reason.to_string(),
                }
            }
            _ => ProbeEvidence::Absent { provenance },
        };
    };
    if installation.binding.is_delegated() {
        return ProbeEvidence::Present {
            provenance,
            value: ManifestHealthSnapshot {
                outcome: CheckOutcome {
                    spec_label: "component.health_check".to_string(),
                    status: CheckStatus::Skipped,
                    detail: Some(
                        "RPM components are verified through rpmdb; raw-layout health probes are skipped"
                            .to_string(),
                    ),
                    children: Vec::new(),
                },
            },
        };
    }
    let Some(spec) = manifest.health_spec() else {
        return ProbeEvidence::Absent { provenance };
    };
    let skip_active_service_probe = installation.status == LifecycleStatus::Disabled;
    let outcome = run_doctor_check(&spec, Some(manifest), probe_ctx, skip_active_service_probe);
    ProbeEvidence::Present {
        provenance,
        value: ManifestHealthSnapshot { outcome },
    }
}

fn lookup_component_name_from_view(
    input: &str,
    view: &StateView,
    ctx: &CliContext,
) -> Result<String, CliError> {
    if view.has_exact_component(input) {
        return Ok(input.to_string());
    }
    common::lookup_component_name_in_store(input, &view.writable.state, ctx, COMMAND)
}

fn diagnose_component(
    snapshot: &ComponentSnapshot,
    remediation_scope: StateScope,
    manifest: Option<&ComponentManifest>,
    manifest_warning: Option<String>,
    probe_ctx: &DoctorProbeContext<'_>,
    checked_at: &str,
) -> Result<DoctorComponent, CliError> {
    let (object, state_path) = match snapshot.state() {
        ProbeEvidence::Absent { .. } => (None, None),
        ProbeEvidence::Present {
            provenance,
            value: StateSnapshot::Active(installation),
        } => (
            Some(installation.as_ref()),
            Some(provenance.path.display().to_string()),
        ),
        ProbeEvidence::Present {
            value: StateSnapshot::Quarantined(_),
            ..
        } => {
            return Err(CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!(
                    "component snapshot for '{}' contains quarantined state",
                    snapshot.request().component()
                ),
            });
        }
        ProbeEvidence::Unavailable { reason, .. } => {
            return Err(CliError::Runtime {
                command: COMMAND.to_string(),
                reason: reason.clone(),
            });
        }
        ProbeEvidence::NotRequested => {
            return Err(CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!(
                    "component snapshot for '{}' did not request state",
                    snapshot.request().component()
                ),
            });
        }
    };
    let visibility = snapshot.state_visibility();
    let state_status = object
        .map(common::installation_status_str)
        .unwrap_or("not_installed");
    let mut out = DoctorComponent {
        name: snapshot.request().component().to_string(),
        status: "ok".to_string(),
        scope: object.map_or_else(
            || "none".to_string(),
            |_| {
                visibility.map_or_else(
                    || installation_scope_label(snapshot.request().scope()).to_string(),
                    |metadata| state_root_scope_label(metadata.root_scope).to_string(),
                )
            },
        ),
        remediation_scope,
        active: visibility.is_some_and(|metadata| metadata.active),
        mutable_by_current_invocation: visibility
            .is_some_and(|metadata| metadata.mutable_by_current_invocation),
        shadowed_by: visibility
            .and_then(|metadata| metadata.shadowed_by)
            .map(state_root_scope_label)
            .map(str::to_string),
        state_path,
        state_status: Some(state_status.to_string()),
        version: object.and_then(component_observation::record_version),
        findings: Vec::new(),
        health_checks: Vec::new(),
        dependencies: Vec::new(),
        fix_plan: Vec::new(),
    };

    add_state_finding(state_status, &mut out);
    add_persisted_health(object, probe_ctx.layout, &mut out);
    add_owned_file_health(snapshot, object, probe_ctx.layout, checked_at, &mut out);
    add_manifest_warning(manifest_warning, object, probe_ctx.layout, &mut out);
    add_snapshot_manifest_health(snapshot, object, probe_ctx.layout, &mut out);
    add_service_refs(manifest, object, probe_ctx, &mut out);
    add_runtime_dependencies(
        manifest,
        object,
        probe_ctx.resolver_env,
        probe_ctx.resolve_runtime_dependencies,
        probe_ctx.dry_run,
        &mut out,
    );
    add_rpm_drift(snapshot, object, &mut out);
    Ok(out)
}

fn installation_scope_label(scope: InstallationScope) -> &'static str {
    match scope {
        InstallationScope::System => "system",
        InstallationScope::User { .. } => "user",
    }
}

fn state_root_scope_label(scope: StateRootScope) -> &'static str {
    match scope {
        StateRootScope::User => "user",
        StateRootScope::System => "system",
    }
}

fn add_state_finding(status: &str, out: &mut DoctorComponent) {
    match status {
        "installed" | "adopted" | "observed" | "disabled" => {}
        "not_installed" => {
            out.findings.push(finding(
                FindingSeverity::Error,
                "component_not_installed",
                format!("component '{}' is not installed", out.name),
                "state",
                None,
            ));
            out.fix_plan.push(component_suggestion(
                out.remediation_scope,
                "install_component",
                "install",
                &out.name,
                "install the component before running component-level diagnostics",
            ));
        }
        "degraded" => out.findings.push(finding(
            FindingSeverity::Warning,
            "component_degraded",
            format!(
                "component '{}' is marked degraded in ANOLISA state",
                out.name
            ),
            "state",
            None,
        )),
        "failed" => out.findings.push(finding(
            FindingSeverity::Error,
            "component_failed",
            format!("component '{}' is marked failed in ANOLISA state", out.name),
            "state",
            None,
        )),
        other => out.findings.push(finding(
            FindingSeverity::Warning,
            "component_status_attention",
            format!("component '{}' has status '{other}'", out.name),
            "state",
            None,
        )),
    }
}

fn add_persisted_health(
    object: Option<&Installation>,
    layout: &FsLayout,
    out: &mut DoctorComponent,
) {
    let Some(object) = object else {
        return;
    };
    let manifest_prefix = format!("{}:", object.name);
    for entry in &object.health {
        // Delegated packages are verified through rpmdb. Persisted raw-layout
        // manifest probes expand paths under /usr/local and do not describe
        // the files owned by the package manager.
        if object.binding.is_delegated() && entry.name.starts_with(&manifest_prefix) {
            continue;
        }
        add_health_entry(entry, Some(object), layout, out);
    }
}

fn add_owned_file_health(
    snapshot: &ComponentSnapshot,
    object: Option<&Installation>,
    layout: &FsLayout,
    checked_at: &str,
    out: &mut DoctorComponent,
) {
    let ProbeEvidence::Present { value, .. } = snapshot.owned_files() else {
        return;
    };
    for observation in &value.files {
        if observation.status == IntegrityStatus::Skipped {
            continue;
        }
        let reason = match &observation.status {
            IntegrityStatus::ProbeLimitExceeded { size, limit } => Some(format!(
                "file size {size} exceeds the {limit}-byte integrity probe ceiling; \
                 the recorded digest was not re-checked this run"
            )),
            _ => None,
        };
        add_health_entry(
            &HealthEntry {
                name: format!("integrity:{}", observation.path.display()),
                status: observation.status.label().to_string(),
                checked_at: checked_at.to_string(),
                reason,
            },
            object,
            layout,
            out,
        );
    }
}

fn add_health_entry(
    entry: &HealthEntry,
    object: Option<&Installation>,
    layout: &FsLayout,
    out: &mut DoctorComponent,
) {
    out.health_checks.push(health_from_entry(entry));
    let Some(severity) = severity_for_health_status(&entry.status) else {
        return;
    };
    out.findings.push(finding(
        severity,
        format!("health_{}", sanitize_code(&entry.status)),
        format!("health check '{}' reported '{}'", entry.name, entry.status),
        "health",
        entry.reason.clone(),
    ));
    out.fix_plan.extend(suggestions_for_health(
        out.remediation_scope,
        &out.name,
        &entry.name,
        &entry.status,
        object,
        layout,
    ));
}

fn add_manifest_warning(
    warning: Option<String>,
    object: Option<&Installation>,
    layout: &FsLayout,
    out: &mut DoctorComponent,
) {
    let Some(warning) = warning else {
        return;
    };
    let rpm_backed = object
        .map(|object| object.binding.is_delegated())
        .unwrap_or(false);
    if rpm_backed && warning.starts_with("component contract unavailable") {
        return;
    }
    let fixes = if rpm_backed {
        vec![suggestion(
            "publish_component_contract",
            None,
            "include an ANOLISA component contract in the RPM package for full diagnostics",
        )]
    } else {
        let repairable = common::installed_component_manifest_path(layout, &out.name, COMMAND)
            .ok()
            .is_some_and(|path| owned_file_damage_matches(object, layout, |owned| owned == path));
        lifecycle_recovery_suggestions(
            out.remediation_scope,
            &out.name,
            object,
            RecoveryNeed::ArtifactDamage { repairable },
            "restore the installed component contract snapshot",
        )
    };
    out.findings.push(finding(
        FindingSeverity::Warning,
        "manifest_unavailable",
        "component manifest could not be loaded for full diagnostics",
        "manifest",
        Some(warning),
    ));
    out.fix_plan.extend(fixes);
}

fn add_snapshot_manifest_health(
    snapshot: &ComponentSnapshot,
    object: Option<&Installation>,
    layout: &FsLayout,
    out: &mut DoctorComponent,
) {
    let Some(object) = object else {
        return;
    };
    let ProbeEvidence::Present { value, .. } = snapshot.manifest_health() else {
        return;
    };
    let component = out.name.clone();
    add_check_outcome(&component, object, layout, &value.outcome, out);
}

fn run_doctor_check(
    spec: &CheckSpec,
    manifest: Option<&ComponentManifest>,
    probe_ctx: &DoctorProbeContext<'_>,
    skip_active_service_probe: bool,
) -> CheckOutcome {
    match spec {
        CheckSpec::AllOf { checks, .. } => {
            let children: Vec<CheckOutcome> = checks
                .iter()
                .map(|child| {
                    run_doctor_check(child, manifest, probe_ctx, skip_active_service_probe)
                })
                .collect();
            CheckOutcome {
                spec_label: format!("all_of ({} checks)", checks.len()),
                status: all_of_status(&children),
                detail: None,
                children,
            }
        }
        CheckSpec::AnyOf { checks, .. } => {
            let children: Vec<CheckOutcome> = checks
                .iter()
                .map(|child| {
                    run_doctor_check(child, manifest, probe_ctx, skip_active_service_probe)
                })
                .collect();
            CheckOutcome {
                spec_label: format!("any_of ({} checks)", checks.len()),
                status: any_of_status(&children),
                detail: None,
                children,
            }
        }
        leaf if probe_ctx.dry_run => CheckOutcome {
            spec_label: doctor_check_label(leaf),
            status: CheckStatus::Skipped,
            detail: None,
            children: Vec::new(),
        },
        CheckSpec::SystemdActive { .. } if skip_active_service_probe => CheckOutcome {
            spec_label: doctor_check_label(spec),
            status: CheckStatus::Skipped,
            detail: Some("component is disabled; active service probe skipped".to_string()),
            children: Vec::new(),
        },
        CheckSpec::SystemdActive { service } => {
            let scope = systemd_active_scope(service, manifest);
            probe_systemd_active(service, scope, probe_ctx)
        }
        leaf => run_check(
            leaf,
            &CheckEnv {
                layout: probe_ctx.layout,
                dry_run: probe_ctx.dry_run,
                service_probes: None,
            },
        ),
    }
}

fn all_of_status(children: &[CheckOutcome]) -> CheckStatus {
    if children
        .iter()
        .any(|child| child.status == CheckStatus::Failed)
    {
        CheckStatus::Failed
    } else if children.iter().all(|child| child.status == CheckStatus::Ok) {
        CheckStatus::Ok
    } else if children
        .iter()
        .all(|child| child.status == CheckStatus::Skipped)
    {
        CheckStatus::Skipped
    } else if children
        .iter()
        .all(|child| matches!(child.status, CheckStatus::Ok | CheckStatus::Skipped))
    {
        CheckStatus::Ok
    } else {
        CheckStatus::Unsupported
    }
}

fn any_of_status(children: &[CheckOutcome]) -> CheckStatus {
    if children.iter().any(|child| child.status == CheckStatus::Ok) {
        CheckStatus::Ok
    } else if children
        .iter()
        .all(|child| child.status == CheckStatus::Skipped)
    {
        CheckStatus::Skipped
    } else if children
        .iter()
        .any(|child| child.status == CheckStatus::Failed)
    {
        CheckStatus::Failed
    } else {
        CheckStatus::Unsupported
    }
}

fn doctor_check_label(spec: &CheckSpec) -> String {
    match spec {
        CheckSpec::BinaryVersion { binary, .. } => format!("binary_version binary={binary}"),
        CheckSpec::BinaryHelp { binary, .. } => format!("binary_help binary={binary}"),
        CheckSpec::SystemdActive { service } => format!("systemd_active service={service}"),
        CheckSpec::FileExists { path, .. } => format!("file_exists path={path}"),
        CheckSpec::PortListen { port, .. } => format!("port_listen port={port}"),
        CheckSpec::HttpGet { url, .. } => format!("http_get url={url}"),
        CheckSpec::BinaryCapabilities { binary, .. } => {
            format!("binary_capabilities binary={binary}")
        }
        CheckSpec::Command { argv, .. } => format!("command argv={}", argv.join(" ")),
        CheckSpec::AllOf { checks, .. } => format!("all_of ({} checks)", checks.len()),
        CheckSpec::AnyOf { checks, .. } => format!("any_of ({} checks)", checks.len()),
    }
}

fn probe_systemd_active(
    service: &str,
    scope: ServiceScope,
    probe_ctx: &DoctorProbeContext<'_>,
) -> CheckOutcome {
    let label = format!("systemd_active service={service}");
    if service.trim().is_empty() {
        return check_outcome(
            label,
            CheckStatus::Failed,
            Some("systemd_active check missing service name".to_string()),
        );
    }
    if probe_ctx.dry_run {
        return check_outcome(label, CheckStatus::Skipped, None);
    }
    let manager = service_manager_for_scope(scope, probe_ctx);
    if !manager.supported() {
        return check_outcome(
            label,
            CheckStatus::Unsupported,
            manager
                .unsupported_reason()
                .map(str::to_string)
                .or_else(|| Some("service manager not supported".to_string())),
        );
    }
    if !manager.handles_scope(scope) {
        return check_outcome(
            label,
            CheckStatus::Unsupported,
            Some(format!(
                "service manager '{}' does not handle {}-scope units",
                manager.manager(),
                service_scope_label(scope)
            )),
        );
    }
    match manager.probe_service(service) {
        Ok(outcome) => match outcome.state {
            ServiceState::Active => check_outcome(
                label,
                CheckStatus::Ok,
                Some(format!("unit '{service}' is active")),
            ),
            ServiceState::NotSupported => check_outcome(
                label,
                CheckStatus::Unsupported,
                Some(non_empty_or(outcome.message, "service manager unsupported")),
            ),
            ServiceState::NotInstalled => check_outcome(
                label,
                CheckStatus::Failed,
                Some(format!("unit '{service}' is not installed")),
            ),
            other => check_outcome(
                label,
                CheckStatus::Failed,
                Some(format!("unit '{service}' state '{}'", other.as_str())),
            ),
        },
        Err(err) => check_outcome(
            label,
            CheckStatus::Failed,
            Some(format!("systemd probe for unit '{service}' failed: {err}")),
        ),
    }
}

fn check_outcome(spec_label: String, status: CheckStatus, detail: Option<String>) -> CheckOutcome {
    CheckOutcome {
        spec_label,
        status,
        detail,
        children: Vec::new(),
    }
}

fn service_manager_for_scope<'a>(
    scope: ServiceScope,
    probe_ctx: &'a DoctorProbeContext<'_>,
) -> &'a dyn ServiceManager {
    match scope {
        ServiceScope::System => probe_ctx.system_service,
        ServiceScope::User => probe_ctx.user_service,
    }
}

fn service_scope_label(scope: ServiceScope) -> &'static str {
    match scope {
        ServiceScope::System => "system",
        ServiceScope::User => "user",
    }
}

fn systemd_active_scope(service: &str, manifest: Option<&ComponentManifest>) -> ServiceScope {
    manifest
        .map(|manifest| anolisa_core::declared_unit_scope(&manifest.install.services, service))
        .unwrap_or(ServiceScope::System)
}

fn add_service_refs(
    manifest: Option<&ComponentManifest>,
    object: Option<&Installation>,
    probe_ctx: &DoctorProbeContext<'_>,
    out: &mut DoctorComponent,
) {
    let Some(object) = object else {
        return;
    };
    let ProviderBinding::Owned { artifact } = &object.binding else {
        return;
    };
    if artifact.services.is_empty() {
        return;
    }
    let explicit_systemd = manifest
        .and_then(ComponentManifest::health_spec)
        .map(|spec| {
            let mut units = BTreeSet::new();
            collect_systemd_active_units(&spec, &mut units);
            units
        })
        .unwrap_or_default();
    let skip_active_service_probe = object.status == LifecycleStatus::Disabled;

    for service in &artifact.services {
        if explicit_systemd.contains(&service.name) {
            continue;
        }
        add_service_ref(
            service,
            manifest,
            object,
            probe_ctx,
            skip_active_service_probe,
            out,
        );
    }
}

fn add_service_ref(
    service: &ServiceRef,
    manifest: Option<&ComponentManifest>,
    object: &Installation,
    probe_ctx: &DoctorProbeContext<'_>,
    skip_active_service_probe: bool,
    out: &mut DoctorComponent,
) {
    let name = format!("service_ref:{}", service.name);
    if !service_should_be_active(service, manifest) {
        out.health_checks.push(DoctorHealthCheck {
            name,
            status: "skipped".to_string(),
            source: "service_ref".to_string(),
            detail: Some("service is not declared to start during install".to_string()),
            checked_at: None,
        });
        return;
    }
    if skip_active_service_probe {
        out.health_checks.push(DoctorHealthCheck {
            name,
            status: "skipped".to_string(),
            source: "service_ref".to_string(),
            detail: Some("component is disabled; active service probe skipped".to_string()),
            checked_at: None,
        });
        return;
    }
    if probe_ctx.dry_run {
        out.health_checks.push(DoctorHealthCheck {
            name,
            status: "skipped".to_string(),
            source: "service_ref".to_string(),
            detail: Some("dry-run: service probe not executed".to_string()),
            checked_at: None,
        });
        return;
    }

    let manager = service_manager_for_scope(service.scope, probe_ctx);
    if !manager.supported() || !manager.handles_scope(service.scope) {
        let detail = manager
            .unsupported_reason()
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "service manager '{}' does not handle {}-scope units",
                    manager.manager(),
                    service_scope_label(service.scope)
                )
            });
        out.health_checks.push(DoctorHealthCheck {
            name,
            status: "skipped".to_string(),
            source: "service_ref".to_string(),
            detail: Some(detail),
            checked_at: None,
        });
        return;
    }

    match manager.probe_service(&service.name) {
        Ok(outcome) => add_service_ref_outcome(
            service,
            outcome.state,
            Some(outcome.message),
            object,
            probe_ctx.layout,
            out,
        ),
        Err(err) => {
            out.health_checks.push(DoctorHealthCheck {
                name,
                status: "probe_error".to_string(),
                source: "service_ref".to_string(),
                detail: Some(err.to_string()),
                checked_at: None,
            });
            out.findings.push(finding(
                FindingSeverity::Error,
                "service_probe_failed",
                format!("service '{}' could not be probed", service.name),
                "service_ref",
                Some(err.to_string()),
            ));
            out.fix_plan.push(component_suggestion(
                out.remediation_scope,
                "inspect_logs",
                "logs",
                &out.name,
                "inspect service-manager errors for the component",
            ));
        }
    }
}

fn add_service_ref_outcome(
    service: &ServiceRef,
    state: ServiceState,
    detail: Option<String>,
    object: &Installation,
    layout: &FsLayout,
    out: &mut DoctorComponent,
) {
    let status = state.as_str().to_string();
    out.health_checks.push(DoctorHealthCheck {
        name: format!("service_ref:{}", service.name),
        status: status.clone(),
        source: "service_ref".to_string(),
        detail: detail.clone(),
        checked_at: None,
    });
    match state {
        ServiceState::Active | ServiceState::NotSupported => {}
        ServiceState::Activating | ServiceState::Deactivating => {
            out.findings.push(finding(
                FindingSeverity::Warning,
                "service_not_ready",
                format!("service '{}' is '{}'", service.name, state.as_str()),
                "service_ref",
                detail,
            ));
            out.fix_plan.push(component_suggestion(
                out.remediation_scope,
                "inspect_logs",
                "logs",
                &out.name,
                "inspect service startup progress",
            ));
        }
        ServiceState::NotInstalled => {
            out.findings.push(finding(
                FindingSeverity::Error,
                "service_unit_missing",
                format!("service unit '{}' is not installed", service.name),
                "service_ref",
                detail,
            ));
            let repairable = owned_file_damage_matches(Some(object), layout, |path| {
                path.file_name()
                    .is_some_and(|name| name == std::ffi::OsStr::new(&service.name))
            });
            out.fix_plan.extend(lifecycle_recovery_suggestions(
                out.remediation_scope,
                &out.name,
                Some(object),
                RecoveryNeed::ArtifactDamage { repairable },
                "restore the missing service unit",
            ));
        }
        ServiceState::Inactive | ServiceState::Failed | ServiceState::Unknown => {
            out.findings.push(finding(
                FindingSeverity::Error,
                "service_not_active",
                format!("service '{}' is '{}'", service.name, state.as_str()),
                "service_ref",
                detail,
            ));
            out.fix_plan.push(component_suggestion(
                out.remediation_scope,
                "restart_component",
                "restart",
                &out.name,
                "restart the component service",
            ));
            out.fix_plan.push(component_suggestion(
                out.remediation_scope,
                "inspect_logs",
                "logs",
                &out.name,
                "inspect service logs for the component",
            ));
        }
    }
}

fn collect_systemd_active_units(spec: &CheckSpec, out: &mut BTreeSet<String>) {
    match spec {
        CheckSpec::SystemdActive { service } => {
            out.insert(service.clone());
        }
        CheckSpec::AllOf { checks, .. } | CheckSpec::AnyOf { checks, .. } => {
            for child in checks {
                collect_systemd_active_units(child, out);
            }
        }
        _ => {}
    }
}

fn service_should_be_active(service: &ServiceRef, manifest: Option<&ComponentManifest>) -> bool {
    let Some(manifest) = manifest else {
        return true;
    };
    manifest
        .install
        .services
        .iter()
        .find(|decl| decl.covers_unit(&service.name))
        .map(|decl| decl.start)
        .unwrap_or(true)
}

fn non_empty_or(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

fn add_check_outcome(
    component: &str,
    object: &Installation,
    layout: &FsLayout,
    outcome: &CheckOutcome,
    out: &mut DoctorComponent,
) {
    let status = outcome.status.as_str().to_string();
    out.health_checks.push(DoctorHealthCheck {
        name: outcome.spec_label.clone(),
        status: status.clone(),
        source: "structured_health".to_string(),
        detail: outcome.detail.clone(),
        checked_at: None,
    });
    match outcome.status {
        CheckStatus::Ok | CheckStatus::Skipped => {}
        CheckStatus::Unsupported => {
            out.findings.push(finding(
                FindingSeverity::Warning,
                "structured_health_unsupported",
                format!(
                    "health check '{}' could not be verified",
                    outcome.spec_label
                ),
                "structured_health",
                outcome.detail.clone(),
            ));
            out.fix_plan.push(suggestion(
                "fix_manifest",
                None,
                "adjust the component health check declaration",
            ));
        }
        CheckStatus::Failed => {
            out.findings.push(finding(
                FindingSeverity::Error,
                "structured_health_failed",
                format!("health check '{}' failed", outcome.spec_label),
                "structured_health",
                outcome.detail.clone(),
            ));
            out.fix_plan.extend(suggestions_for_structured_health(
                out.remediation_scope,
                component,
                object,
                layout,
                outcome,
            ));
        }
    }
    for child in &outcome.children {
        add_check_outcome(component, object, layout, child, out);
    }
}

fn add_runtime_dependencies(
    manifest: Option<&ComponentManifest>,
    object: Option<&Installation>,
    resolver_env: &ResolverEnv,
    resolve_runtime_dependencies: &ResolveRuntimeDependencies<'_>,
    dry_run: bool,
    out: &mut DoctorComponent,
) {
    let Some(object) = object else {
        return;
    };
    let Some(manifest) = manifest else {
        return;
    };
    if manifest.runtime_deps.is_empty() {
        return;
    }
    if object.binding.is_delegated() {
        for dep in &manifest.runtime_deps {
            out.dependencies.push(DoctorDependency {
                name: dep.name.clone(),
                kind: dep.kind,
                status: DoctorDependencyStatus::Skipped,
                note: Some("RPM backend owns runtime dependency resolution".to_string()),
                detail: None,
            });
        }
        return;
    }
    if dry_run {
        for dep in &manifest.runtime_deps {
            out.dependencies.push(DoctorDependency {
                name: dep.name.clone(),
                kind: dep.kind,
                status: DoctorDependencyStatus::Skipped,
                note: Some("dry-run: dependency probe not executed".to_string()),
                detail: None,
            });
        }
        return;
    }

    match resolve_runtime_dependencies(&manifest.runtime_deps, resolver_env) {
        Ok(plan) => {
            for warning in plan.warnings {
                out.findings.push(finding(
                    FindingSeverity::Warning,
                    "dependency_warning",
                    warning,
                    "dependency",
                    None,
                ));
            }
            for resolution in plan.resolutions {
                add_dependency_resolution(&resolution, out);
            }
        }
        Err(err) => {
            out.findings.push(finding(
                FindingSeverity::Error,
                "invalid_dependency_declaration",
                "runtime dependency declaration is invalid",
                "dependency",
                Some(err.to_string()),
            ));
            out.fix_plan.push(suggestion(
                "fix_manifest",
                None,
                "fix the component runtime dependency declaration",
            ));
        }
    }
}

fn add_dependency_resolution(resolution: &DependencyResolution, out: &mut DoctorComponent) {
    let (status, note) = match &resolution.status {
        DependencyStatus::Resolved => (DoctorDependencyStatus::Resolved, None),
        DependencyStatus::Unresolved { remediation } => (
            DoctorDependencyStatus::Unresolved,
            Some(remediation.clone()),
        ),
        DependencyStatus::Unresolvable { reason } => {
            (DoctorDependencyStatus::Unresolvable, Some(reason.clone()))
        }
        DependencyStatus::ProbeFailed { error } => (
            DoctorDependencyStatus::Unresolvable,
            Some(format!("dependency probe failed: {error}")),
        ),
    };
    out.dependencies.push(DoctorDependency {
        name: resolution.name.clone(),
        kind: resolution.kind,
        status,
        note: note.clone(),
        detail: match &resolution.status {
            DependencyStatus::ProbeFailed { error } => Some(error.to_string()),
            _ => resolution.detail.clone(),
        },
    });

    match &resolution.status {
        DependencyStatus::Resolved => {}
        DependencyStatus::ProbeFailed { error } => {
            out.findings.push(finding(
                FindingSeverity::Error,
                "dependency_probe_failed",
                format!(
                    "runtime dependency '{}' [{}] could not be probed",
                    resolution.name,
                    resolution.kind.as_str()
                ),
                "dependency",
                Some(error.to_string()),
            ));
            out.fix_plan.push(suggestion(
                "inspect_dependency_probe",
                None,
                "inspect the dependency query failure before installing packages",
            ));
        }
        DependencyStatus::Unresolved { remediation } => {
            out.findings.push(finding(
                FindingSeverity::Error,
                "dependency_unresolved",
                format!(
                    "runtime dependency '{}' [{}] is missing",
                    resolution.name,
                    resolution.kind.as_str()
                ),
                "dependency",
                resolution.detail.clone(),
            ));
            out.fix_plan
                .push(suggestion_for_dependency(resolution.kind, remediation));
        }
        DependencyStatus::Unresolvable { reason } => {
            out.findings.push(finding(
                FindingSeverity::Error,
                "dependency_unresolvable",
                format!(
                    "runtime dependency '{}' [{}] cannot be satisfied automatically",
                    resolution.name,
                    resolution.kind.as_str()
                ),
                "dependency",
                Some(reason.clone()),
            ));
            out.fix_plan.push(suggestion(
                if resolution.kind == DependencyKind::SystemPackage {
                    "inspect_package_state"
                } else {
                    "satisfy_platform_requirement"
                },
                None,
                reason.clone(),
            ));
        }
    }
}

fn add_rpm_drift(
    snapshot: &ComponentSnapshot,
    object: Option<&Installation>,
    out: &mut DoctorComponent,
) {
    let Some(object) = object else {
        return;
    };
    if !object.binding.is_delegated() {
        return;
    }
    let Some((package, recorded_evr)) = drift_probe_identity(object) else {
        out.health_checks.push(DoctorHealthCheck {
            name: "rpmdb".to_string(),
            status: "unverified".to_string(),
            source: "rpm".to_string(),
            detail: Some("RPM package metadata is missing from ANOLISA state".to_string()),
            checked_at: None,
        });
        out.findings.push(finding(
            FindingSeverity::Warning,
            "rpm_metadata_missing",
            format!(
                "component '{}' is RPM-backed but has no recorded RPM package metadata",
                out.name
            ),
            "rpm",
            None,
        ));
        out.fix_plan.push(component_suggestion(
            out.remediation_scope,
            "repair_state",
            "repair",
            &out.name,
            "backfill RPM package metadata from rpmdb",
        ));
        return;
    };
    match rpm_drift_from_evidence(snapshot.native_package(), recorded_evr) {
        Some(RpmDrift::Drifted { reason }) => {
            out.health_checks.push(DoctorHealthCheck {
                name: format!("rpmdb:{package}"),
                status: "failed".to_string(),
                source: "rpm".to_string(),
                detail: Some(reason.clone()),
                checked_at: None,
            });
            out.findings.push(finding(
                FindingSeverity::Error,
                "rpm_drifted",
                format!(
                    "RPM package for component '{}' drifted from ANOLISA state",
                    out.name
                ),
                "rpm",
                Some(reason),
            ));
            out.fix_plan.push(component_suggestion(
                out.remediation_scope,
                "repair_state",
                "repair",
                &out.name,
                "refresh ANOLISA state from rpmdb",
            ));
        }
        Some(RpmDrift::Missing) => {
            out.health_checks.push(DoctorHealthCheck {
                name: format!("rpmdb:{package}"),
                status: "failed".to_string(),
                source: "rpm".to_string(),
                detail: Some("recorded RPM package is absent from rpmdb".to_string()),
                checked_at: None,
            });
            out.findings.push(finding(
                FindingSeverity::Error,
                "rpm_missing",
                format!(
                    "RPM package '{package}' recorded for component '{}' is missing",
                    out.name
                ),
                "rpm",
                None,
            ));
            out.fix_plan.extend(lifecycle_recovery_suggestions(
                out.remediation_scope,
                &out.name,
                Some(object),
                RecoveryNeed::DelegatedPackageMissing,
                "reconcile the recorded package that is gone from rpmdb",
            ));
        }
        None => out.health_checks.push(DoctorHealthCheck {
            name: format!("rpmdb:{package}"),
            status: "ok".to_string(),
            source: "rpm".to_string(),
            detail: None,
            checked_at: None,
        }),
    }
}

/// Validate each visible state root once before attributing journals to
/// components. A malformed entry makes the whole root untrusted because an
/// earlier matching journal cannot prove that a later entry is unrelated.
fn scan_journal_roots(view: &StateView) -> Vec<DoctorJournalRoot> {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();

    for root in &view.visible_roots {
        let state_path = root.state_path.display().to_string();
        if !seen.insert(state_path.clone()) {
            continue;
        }
        let journal_dir = rpm_install::journal_dir(&root.layout);
        let evidence = JournalEvidence::new(&journal_dir, &root.state.operations);
        roots.push(scan_journal_root(root.scope, state_path, evidence));
    }

    // A state file can be unavailable while its journal directory remains
    // readable. Doctor still owns reporting recovery state for that root.
    for root in &view.unavailable_roots {
        let state_path = root.state_path.display().to_string();
        if !seen.insert(state_path.clone()) {
            continue;
        }
        let journal_dir = journal_dir_for_state_path(&root.state_path);
        let evidence = JournalEvidence::new(&journal_dir, &[]);
        roots.push(scan_journal_root(root.scope, state_path, evidence));
    }

    roots
}

fn scan_journal_root(
    scope: StateScope,
    state_path: String,
    evidence: JournalEvidence<'_>,
) -> DoctorJournalRoot {
    let journal_dir_text = evidence.journal_dir().display().to_string();
    let scan = match JournalInventory::load(evidence) {
        Ok(inventory) => DoctorJournalScan::Trusted(inventory),
        Err(err) => DoctorJournalScan::Blocked(journal_scan_failure(
            scope,
            &state_path,
            &journal_dir_text,
            err,
        )),
    };
    DoctorJournalRoot {
        scope,
        state_path,
        journal_dir: journal_dir_text,
        scan,
    }
}

fn journal_dir_for_state_path(state_path: &Path) -> PathBuf {
    state_path
        .parent()
        .map(|parent| parent.join("journal"))
        .unwrap_or_else(|| PathBuf::from("journal"))
}

fn journal_scan_failure(
    scope: StateScope,
    state_path: &str,
    journal_dir: &str,
    err: FactsError,
) -> DoctorRecoveryRoot {
    let (finding, fix) = match err {
        FactsError::JournalLoad { path, source } => (
            finding(
                FindingSeverity::Error,
                "operation_journal_unreadable",
                "an operation journal cannot be validated safely",
                "journal",
                Some(format!("{}: {source}", path.display())),
            ),
            suggestion(
                "inspect_operation_journal",
                None,
                format!(
                    "preserve and inspect {}; its subject and status cannot be trusted, so lifecycle commands remain blocked for this state root",
                    path.display()
                ),
            ),
        ),
        FactsError::JournalScan { dir, source } => (
            finding(
                FindingSeverity::Error,
                "operation_journal_scan_failed",
                "the operation journal directory cannot be inspected safely",
                "journal",
                Some(format!("{}: {source}", dir.display())),
            ),
            suggestion(
                "inspect_operation_journal_directory",
                None,
                format!(
                    "restore read access to {} before running lifecycle commands in this state root",
                    dir.display()
                ),
            ),
        ),
        err => (
            finding(
                FindingSeverity::Error,
                "operation_journal_check_failed",
                "operation recovery state cannot be inspected safely",
                "journal",
                Some(err.to_string()),
            ),
            suggestion(
                "inspect_operation_recovery_state",
                None,
                "inspect the operation journal directory before running lifecycle commands",
            ),
        ),
    };
    DoctorRecoveryRoot {
        scope: scope.label().to_string(),
        status: "failed".to_string(),
        state_path: state_path.to_string(),
        journal_dir: journal_dir.to_string(),
        findings: vec![finding],
        fix_plan: vec![fix],
    }
}

/// Apply trusted, subject-specific recovery ordering to a diagnosed
/// component. Root-wide failures and unattributed legacy journals suppress
/// executable lifecycle advice without duplicating the root finding once per
/// component.
fn apply_component_journal_guard(
    roots: &[DoctorJournalRoot],
    out: &mut DoctorComponent,
    claimed_journals: &mut BTreeSet<String>,
) {
    let Some(state_path) = out.state_path.as_deref() else {
        return;
    };
    let Some(root) = roots.iter().find(|root| root.state_path == state_path) else {
        return;
    };
    let DoctorJournalScan::Trusted(inventory) = &root.scan else {
        remove_lifecycle_commands(&mut out.fix_plan);
        return;
    };

    if inventory
        .entries()
        .iter()
        .any(|entry| entry.is_effectively_pending() && entry.transaction().subject.is_none())
    {
        remove_lifecycle_commands(&mut out.fix_plan);
        return;
    }

    let matching = inventory
        .entries()
        .iter()
        .filter(|entry| {
            entry.is_effectively_pending()
                && entry.transaction().subject.as_deref() == Some(out.name.as_str())
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return;
    }

    remove_lifecycle_commands(&mut out.fix_plan);
    let paths = matching
        .iter()
        .map(|entry| {
            let path = entry.path().display().to_string();
            claimed_journals.insert(path.clone());
            path
        })
        .collect::<Vec<_>>();
    out.findings.push(finding(
        FindingSeverity::Error,
        "operation_pending",
        format!(
            "component '{}' has an unfinished lifecycle operation",
            out.name
        ),
        "journal",
        Some(format!("pending journal: {}", paths.join(", "))),
    ));
    out.fix_plan.push(component_suggestion(
        out.remediation_scope,
        "repair_component",
        "repair",
        &out.name,
        "recover the pending operation before running another lifecycle command",
    ));
    dedupe_fix_plan(&mut out.fix_plan);
}

fn collect_root_recovery(
    roots: &[DoctorJournalRoot],
    claimed_journals: &BTreeSet<String>,
    selected_component: Option<&str>,
) -> Vec<DoctorRecoveryRoot> {
    let mut recovery = Vec::new();
    for root in roots {
        let DoctorJournalScan::Trusted(inventory) = &root.scan else {
            if let DoctorJournalScan::Blocked(blocked) = &root.scan {
                recovery.push(blocked.clone());
            }
            continue;
        };
        let mut findings = Vec::new();
        let mut fix_plan = Vec::new();
        for entry in inventory
            .entries()
            .iter()
            .filter(|entry| entry.is_effectively_pending())
        {
            let path = entry.path().display().to_string();
            match entry.transaction().subject.as_deref() {
                None => {
                    findings.push(finding(
                        FindingSeverity::Error,
                        "operation_pending_unattributed",
                        "the state root has an unfinished operation with no trustworthy component attribution",
                        "journal",
                        Some(format!("pending journal: {path}")),
                    ));
                    fix_plan.push(suggestion(
                        "inspect_operation_journal",
                        None,
                        format!(
                            "preserve and inspect {path}; lifecycle commands remain blocked for this state root until its subject is established"
                        ),
                    ));
                }
                Some(subject)
                    if !claimed_journals.contains(&path)
                        && selected_component.is_none_or(|selected| selected == subject) =>
                {
                    findings.push(finding(
                        FindingSeverity::Error,
                        "operation_pending_unrepresented",
                        format!(
                            "component '{subject}' has an unfinished operation but no selected active record"
                        ),
                        "journal",
                        Some(format!("pending journal: {path}")),
                    ));
                    fix_plan.push(component_suggestion(
                        root.scope,
                        "repair_component",
                        "repair",
                        subject,
                        "recover the pending operation before running another lifecycle command",
                    ));
                }
                Some(_) => {}
            }
        }
        if findings.is_empty() {
            continue;
        }
        dedupe_fix_plan(&mut fix_plan);
        recovery.push(DoctorRecoveryRoot {
            scope: root.scope.label().to_string(),
            status: "failed".to_string(),
            state_path: root.state_path.clone(),
            journal_dir: root.journal_dir.clone(),
            findings,
            fix_plan,
        });
    }
    recovery
}

fn remove_lifecycle_commands(fix_plan: &mut Vec<FixSuggestion>) {
    fix_plan.retain(|fix| {
        fix.command
            .as_deref()
            .is_none_or(|command| !is_component_lifecycle_command(command))
    });
}

fn is_component_lifecycle_command(command: &str) -> bool {
    let command = command.strip_prefix("sudo ").unwrap_or(command);
    let Some(rest) = command.strip_prefix("anolisa ") else {
        return false;
    };
    let rest = rest
        .strip_prefix("--install-mode system ")
        .or_else(|| rest.strip_prefix("--install-mode user "))
        .unwrap_or(rest);
    matches!(
        rest.split_whitespace().next(),
        Some("install" | "update" | "uninstall" | "repair" | "forget" | "adopt" | "restart")
    )
}

/// Load the installed manifest snapshot for `component`. The snapshot is
/// the only source for an installed component's contract — the same
/// per-installation copy consumed by uninstall hooks, adapter discovery,
/// status health probes, and contract reconciliation. A missing snapshot
/// is reported as a note (adopted records and installs from before the
/// snapshot machinery have none), an unreadable one as a parse warning.
fn resolve_component_manifest(
    layout: &FsLayout,
    component: &str,
) -> (Option<ComponentManifest>, Option<String>) {
    match common::installed_component_manifest_path(layout, component, COMMAND) {
        Ok(path) if path.is_file() => match ComponentManifest::from_file(&path) {
            Ok(manifest) => (Some(manifest), None),
            Err(err) => (
                None,
                Some(format!(
                    "failed to parse installed manifest snapshot at {}: {err}",
                    path.display()
                )),
            ),
        },
        Ok(_) => (
            None,
            Some(format!(
                "component contract unavailable for '{component}': no installed manifest snapshot found"
            )),
        ),
        Err(err) => (None, Some(err.reason())),
    }
}

fn resolver_env_from_facts(facts: &anolisa_env::EnvFacts) -> ResolverEnv {
    ResolverEnv {
        kernel: facts.kernel.clone(),
        pkg_base: facts
            .os_id
            .as_deref()
            .and_then(anolisa_env::pkg_base_from_id),
        btf: facts.btf,
        cap_bpf: facts.cap_bpf,
    }
}

fn summarize(
    components: &[DoctorComponent],
    recovery_roots: &[DoctorRecoveryRoot],
) -> DoctorSummary {
    let mut summary = DoctorSummary {
        components_checked: components.len(),
        ok: 0,
        degraded: 0,
        failed: 0,
        recovery_roots_failed: recovery_roots.len(),
        findings: components.iter().map(|c| c.findings.len()).sum::<usize>()
            + recovery_roots
                .iter()
                .map(|root| root.findings.len())
                .sum::<usize>(),
    };
    for component in components {
        match component.status.as_str() {
            "ok" => summary.ok += 1,
            "failed" | "not_installed" => summary.failed += 1,
            _ => summary.degraded += 1,
        }
    }
    summary
}

fn component_status(component: &DoctorComponent) -> String {
    if component
        .findings
        .iter()
        .any(|f| f.severity == FindingSeverity::Error)
    {
        if component
            .findings
            .iter()
            .any(|f| f.code == "component_not_installed")
        {
            "not_installed".to_string()
        } else {
            "failed".to_string()
        }
    } else if component
        .findings
        .iter()
        .any(|f| f.severity == FindingSeverity::Warning)
    {
        "degraded".to_string()
    } else {
        "ok".to_string()
    }
}

fn render_doctor(ctx: &CliContext, payload: &DoctorPayload, ok: bool) -> Result<(), CliError> {
    if ctx.json {
        return render_json_with_status(COMMAND, ok, payload);
    }
    if !ctx.quiet {
        render_human(payload, ctx.no_color);
    }
    Ok(())
}

fn render_human(payload: &DoctorPayload, no_color: bool) {
    let color = Palette::new(no_color);
    println!(
        "{} {} checked, {} ok, {} degraded, {} failed",
        color.header("Doctor:"),
        payload.summary.components_checked,
        color.ok(payload.summary.ok),
        color.warn(payload.summary.degraded),
        color.err(payload.summary.failed),
    );
    for warning in &payload.warnings {
        println!("{} {warning}", color.warn("warning:"));
    }
    if payload.summary.recovery_roots_failed > 0 {
        println!(
            "{} {} state root(s) have blocked recovery state",
            color.err("recovery:"),
            payload.summary.recovery_roots_failed,
        );
    }
    for root in &payload.recovery_roots {
        println!(
            "\n{} {} {} ({}, journal={})",
            color.label("Recovery root:"),
            root.state_path,
            color.status(&root.status),
            root.scope,
            root.journal_dir,
        );
        for finding in &root.findings {
            let sev = match finding.severity {
                FindingSeverity::Warning => color.warn("warning"),
                FindingSeverity::Error => color.err("error"),
            };
            println!("  {sev} [{}] {}", finding.code, finding.message);
            if let Some(detail) = &finding.detail {
                println!("    {} {detail}", color.muted("detail:"));
            }
        }
        if !root.fix_plan.is_empty() {
            println!("  {}", color.label("Recommended:"));
            for fix in &root.fix_plan {
                match &fix.command {
                    Some(command) => println!(
                        "    {} {}",
                        color.command(command),
                        color.muted(format!("({})", fix.reason))
                    ),
                    None => println!("    {} {}", fix.action, color.muted(&fix.reason)),
                }
            }
        }
    }
    if payload.components.is_empty() {
        println!("{}", color.muted("no installed components"));
        return;
    }
    for component in &payload.components {
        let mut scope_meta = format!("scope={}", component.scope);
        if !component.active
            && let Some(scope) = component.shadowed_by.as_deref()
        {
            scope_meta.push_str(&format!(", shadowed_by={scope}"));
        }
        if !component.mutable_by_current_invocation {
            scope_meta.push_str(", read-only");
        }
        println!(
            "\n{} {} ({}) {}",
            color.label(&component.name),
            color.status(&component.status),
            component.version.as_deref().unwrap_or("-"),
            color.muted(scope_meta),
        );
        if component.findings.is_empty() {
            println!("  {}", color.ok("no issues found"));
            continue;
        }
        for finding in &component.findings {
            let sev = match finding.severity {
                FindingSeverity::Warning => color.warn("warning"),
                FindingSeverity::Error => color.err("error"),
            };
            println!("  {sev} [{}] {}", finding.code, finding.message);
            if let Some(detail) = &finding.detail {
                println!("    {} {detail}", color.muted("detail:"));
            }
        }
        if !component.fix_plan.is_empty() {
            println!("  {}", color.label("Recommended:"));
            for fix in &component.fix_plan {
                match &fix.command {
                    Some(command) => println!(
                        "    {} {}",
                        color.command(command),
                        color.muted(format!("({})", fix.reason))
                    ),
                    None => println!("    {} {}", fix.action, color.muted(&fix.reason)),
                }
            }
        }
    }
}

fn health_from_entry(entry: &HealthEntry) -> DoctorHealthCheck {
    DoctorHealthCheck {
        name: entry.name.clone(),
        status: entry.status.clone(),
        source: "status_health".to_string(),
        detail: entry.reason.clone(),
        checked_at: Some(entry.checked_at.clone()),
    }
}

fn severity_for_health_status(status: &str) -> Option<FindingSeverity> {
    match status {
        // `probe_limit_exceeded` sits with `unverified`: the bytes were
        // never examined, so there is no defect to report. The entry still
        // appears verbatim in `health_checks` above, so the operator can
        // see the file went unhashed without it being raised as a fault.
        "ok" | "skipped" | "unverified" | "probe_limit_exceeded" => None,
        "not_supported" | "unsupported" | "unsupported_kind" | "out_of_bounds"
        | "unsupported_target" | "not_regular_file" | "timeout" => Some(FindingSeverity::Warning),
        _ => Some(FindingSeverity::Error),
    }
}

fn suggestions_for_health(
    remediation_scope: StateScope,
    component: &str,
    check_name: &str,
    status: &str,
    object: Option<&Installation>,
    layout: &FsLayout,
) -> Vec<FixSuggestion> {
    match status {
        "missing_file" | "sha256_mismatch" | "mode_mismatch" | "capability_mismatch" => {
            let repairable = check_name.strip_prefix("integrity:").is_some_and(|path| {
                owned_file_damage_matches(object, layout, |owned| owned == Path::new(path))
            });
            lifecycle_recovery_suggestions(
                remediation_scope,
                component,
                object,
                RecoveryNeed::ArtifactDamage { repairable },
                "restore missing or modified ANOLISA-owned files",
            )
        }
        "command_failed" | "command_error" | "probe_error" | "timeout" => {
            vec![component_suggestion(
                remediation_scope,
                "inspect_logs",
                "logs",
                component,
                "inspect runtime logs for the failing health probe",
            )]
        }
        "out_of_bounds" | "unsupported_target" | "unsupported_kind" | "not_regular_file"
        | "invalid_check" => vec![suggestion(
            "fix_manifest",
            None,
            format!("fix the manifest health check '{check_name}'"),
        )],
        _ => vec![suggestion(
            "inspect_component",
            None,
            format!("inspect health check '{check_name}'"),
        )],
    }
}

fn suggestions_for_structured_health(
    remediation_scope: StateScope,
    component: &str,
    object: &Installation,
    layout: &FsLayout,
    outcome: &CheckOutcome,
) -> Vec<FixSuggestion> {
    let owned_path = outcome
        .spec_label
        .strip_prefix("file_exists path=")
        .or_else(|| outcome.spec_label.strip_prefix("binary_version binary="))
        .or_else(|| outcome.spec_label.strip_prefix("binary_help binary="));
    if let Some(path) = owned_path {
        let repairable =
            owned_file_damage_matches(Some(object), layout, |owned| owned == Path::new(path));
        lifecycle_recovery_suggestions(
            remediation_scope,
            component,
            Some(object),
            RecoveryNeed::ArtifactDamage { repairable },
            "restore files required by the structured health check",
        )
    } else {
        vec![component_suggestion(
            remediation_scope,
            "inspect_logs",
            "logs",
            component,
            "inspect runtime logs for the failing structured health check",
        )]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryNeed {
    ArtifactDamage { repairable: bool },
    DelegatedPackageMissing,
}

/// Whether the exact owned-file evidence will route `repair` through R2.
///
/// Keeping this probe identical to facts assembly prevents doctor from
/// advertising a repair command that the lifecycle planner would reduce to
/// `NothingToRepair`.
fn owned_file_damage_matches(
    object: Option<&Installation>,
    layout: &FsLayout,
    matches_evidence: impl Fn(&Path) -> bool,
) -> bool {
    let Some(Installation {
        binding: ProviderBinding::Owned { artifact },
        ..
    }) = object
    else {
        return false;
    };
    artifact
        .files
        .iter()
        .any(|file| matches_evidence(&file.path) && check_owned_file(layout, file).is_failure())
}

/// Map a diagnostic fact to commands the lifecycle planner can actually
/// execute for the record's authority class.
fn lifecycle_recovery_suggestions(
    remediation_scope: StateScope,
    component: &str,
    object: Option<&Installation>,
    need: RecoveryNeed,
    reason: impl Into<String>,
) -> Vec<FixSuggestion> {
    let reason = reason.into();
    match object.map(|object| &object.binding) {
        None => vec![component_suggestion(
            remediation_scope,
            "install_component",
            "install",
            component,
            reason,
        )],
        Some(ProviderBinding::Owned { .. }) => match need {
            RecoveryNeed::ArtifactDamage { repairable: true } => vec![component_suggestion(
                remediation_scope,
                "repair_component",
                "repair",
                component,
                reason,
            )],
            _ => vec![suggestion(
                "inspect_component",
                None,
                format!("{reason}; the finding is not backed by a damaged owned-file record"),
            )],
        },
        Some(ProviderBinding::Delegated {
            relation: ManagementRelation::Managed { .. },
            ..
        }) if need == RecoveryNeed::DelegatedPackageMissing => {
            vec![component_suggestion(
                remediation_scope,
                "repair_component",
                "repair",
                component,
                reason,
            )]
        }
        Some(ProviderBinding::Delegated { .. })
            if need == RecoveryNeed::DelegatedPackageMissing =>
        {
            vec![
                component_suggestion(
                    remediation_scope,
                    "forget_state",
                    "forget",
                    component,
                    "first drop the stale tracking record; no native package removal is performed",
                ),
                component_suggestion(
                    remediation_scope,
                    "install_component",
                    "install",
                    component,
                    "then reinstall after the forget command succeeds",
                ),
            ]
        }
        Some(ProviderBinding::Delegated { .. }) => vec![suggestion(
            "inspect_component",
            None,
            format!(
                "{reason}; this delegated record does not grant authority to rewrite package-owned files"
            ),
        )],
    }
}

fn suggestion_for_dependency(kind: DependencyKind, remediation: &str) -> FixSuggestion {
    let command = remediation
        .starts_with("sudo ")
        .then(|| remediation.to_string())
        .or_else(|| {
            remediation
                .starts_with("anolisa ")
                .then(|| remediation.to_string())
        });
    let action = match kind {
        DependencyKind::LanguageRuntime => "install_runtime",
        DependencyKind::SystemPackage => "install_package",
        DependencyKind::PlatformCapability => "satisfy_platform_requirement",
    };
    suggestion(action, command, remediation)
}

fn component_suggestion(
    scope: StateScope,
    action: impl Into<String>,
    operation: &str,
    component: &str,
    reason: impl Into<String>,
) -> FixSuggestion {
    let mode = match scope {
        StateScope::User => InstallMode::User,
        StateScope::System => InstallMode::System,
    };
    suggestion(
        action,
        Some(common::scoped_component_command_for_mode(
            mode, operation, component,
        )),
        reason,
    )
}

fn suggestion(
    action: impl Into<String>,
    command: Option<String>,
    reason: impl Into<String>,
) -> FixSuggestion {
    FixSuggestion {
        action: action.into(),
        command,
        reason: reason.into(),
        automatic: false,
    }
}

fn finding(
    severity: FindingSeverity,
    code: impl Into<String>,
    message: impl Into<String>,
    source: impl Into<String>,
    detail: Option<String>,
) -> DoctorFinding {
    DoctorFinding {
        severity,
        code: code.into(),
        message: message.into(),
        source: source.into(),
        detail,
    }
}

fn sanitize_code(status: &str) -> String {
    status
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn dedupe_fix_plan(fix_plan: &mut Vec<FixSuggestion>) {
    let mut seen = BTreeSet::new();
    fix_plan.retain(|item| seen.insert(item.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::state_view::{StateScope, UnavailableStateRoot};
    use anolisa_core::domain::{
        InstallationScope, ManagementRelation, NativePm, OwnedArtifact, PackageIdentity,
    };
    use anolisa_core::planner::{
        Facts, InstallRequest, Intent, NativeAction, NativeProbe, NoOpReason, Plan, PlanError,
        ProviderTarget, RecordFacts, Step, plan,
    };
    use anolisa_core::state::{
        FileOwner, InstalledObject, ObjectStatus, OperationRecord, OwnedFile, OwnedFileKind,
    };
    use anolisa_core::state_migration::{QuarantineReason, QuarantinedObject};
    use anolisa_core::state_store::StateStore;
    use anolisa_core::transaction::Transaction;
    use anolisa_core::{
        DependencyStatus, FakeServiceManager, HealthEntry, NotSupportedServiceManager, ObjectKind,
        ServiceOp,
    };
    use anolisa_platform::pkg_query::{PackageInfo, PackageQueryError, PackageVersion};
    use std::cell::Cell;
    use std::path::PathBuf;

    struct MissingPackageQuery;

    impl PackageQuery for MissingPackageQuery {
        fn query_installed(
            &self,
            _package: &str,
        ) -> Result<Option<PackageInfo>, PackageQueryError> {
            Ok(None)
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }
    }

    struct CountingPackageQuery {
        calls: Cell<usize>,
    }

    impl PackageQuery for CountingPackageQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            self.calls.set(self.calls.get() + 1);
            Ok(Some(PackageInfo {
                name: package.to_string(),
                version: PackageVersion {
                    epoch: None,
                    version: "1.0.0".to_string(),
                    release: Some("1.al4".to_string()),
                },
                arch: "x86_64".to_string(),
                origin: None,
            }))
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }
    }

    fn installation(name: &str, status: LifecycleStatus, binding: ProviderBinding) -> Installation {
        Installation {
            kind: ObjectKind::Component,
            name: name.to_string(),
            scope: InstallationScope::System,
            binding,
            status,
            installed_at: "2026-06-01T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: anolisa_core::SubscriptionScope::None,
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    fn owned_object(name: &str, status: LifecycleStatus) -> Installation {
        installation(
            name,
            status,
            ProviderBinding::Owned {
                artifact: OwnedArtifact {
                    version: "1.0.0".to_string(),
                    distribution_source: None,
                    raw_package: None,
                    manifest_digest: None,
                    files: Vec::new(),
                    services: Vec::new(),
                    external_modified_files: Vec::new(),
                    provisioned_packages: Vec::new(),
                },
            },
        )
    }

    /// Delegated fixture with an unresolved package: rpm-backed for the
    /// gating checks without depending on the host rpmdb in probes.
    fn delegated_object(name: &str, status: LifecycleStatus) -> Installation {
        installation(
            name,
            status,
            ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Unresolved {
                    component_hint: name.to_string(),
                },
                relation: ManagementRelation::Managed {
                    since: "2026-06-01T00:00:00Z".to_string(),
                },
                last_observed: None,
            },
        )
    }

    fn resolved_delegated_object(
        name: &str,
        package: &str,
        relation: ManagementRelation,
    ) -> Installation {
        installation(
            name,
            LifecycleStatus::Installed,
            ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved {
                    name: package.to_string(),
                },
                relation,
                last_observed: None,
            },
        )
    }

    fn planner_facts(object: Installation, native: NativeProbe) -> Facts {
        Facts {
            scope: object.scope,
            record: RecordFacts::Active(object),
            native,
            pending_journal: false,
            active_adapter_claims: Vec::new(),
            owned_files_verified: None,
        }
    }

    fn push_owned_service(installation: &mut Installation, service: ServiceRef) {
        let ProviderBinding::Owned { artifact } = &mut installation.binding else {
            panic!("fixture must be owned to carry services");
        };
        artifact.services.push(service);
    }

    fn push_owned_file(installation: &mut Installation, path: PathBuf) {
        let ProviderBinding::Owned { artifact } = &mut installation.binding else {
            panic!("fixture must be owned to carry files");
        };
        artifact.files.push(OwnedFile {
            path,
            owner: FileOwner::Anolisa,
            sha256: Some("0".repeat(64)),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        });
    }

    fn service_ref(name: &str, scope: ServiceScope) -> ServiceRef {
        ServiceRef {
            name: name.to_string(),
            manager: scope.manager_label().to_string(),
            restartable: true,
            enabled: true,
            scope,
        }
    }

    fn empty_component(name: &str) -> DoctorComponent {
        DoctorComponent {
            name: name.to_string(),
            status: "ok".to_string(),
            scope: "system".to_string(),
            remediation_scope: StateScope::System,
            active: true,
            mutable_by_current_invocation: true,
            shadowed_by: None,
            state_path: Some("/tmp/anolisa-system-state/installed.toml".to_string()),
            state_status: None,
            version: None,
            findings: Vec::new(),
            health_checks: Vec::new(),
            dependencies: Vec::new(),
            fix_plan: Vec::new(),
        }
    }

    fn probe_context<'a>(
        layout: &'a FsLayout,
        resolver_env: &'a ResolverEnv,
        rpm_query: &'a dyn PackageQuery,
        system_service: &'a dyn ServiceManager,
        user_service: &'a dyn ServiceManager,
        dry_run: bool,
    ) -> DoctorProbeContext<'a> {
        DoctorProbeContext {
            layout,
            resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query,
            system_service,
            user_service,
            dry_run,
        }
    }

    fn diagnose_missing_delegated(
        layout: &FsLayout,
        relation: ManagementRelation,
    ) -> DoctorPayload {
        let object = resolved_delegated_object("cosh", "copilot-shell", relation);
        let view = system_view_with_layout(layout.clone(), state_with_component(object));
        let resolver_env = ResolverEnv::default();
        let query = MissingPackageQuery;
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let view_ctx = DoctorViewContext {
            resolver_env: &resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query: &query,
            current_system_service: &system_service,
            system_scope_service: &system_service,
            user_service: &user_service,
            dry_run: false,
        };
        diagnose_from_view(&view, Some("cosh"), &view_ctx).expect("diagnose test view")
    }

    fn diagnose_inactive_owned(layout: &FsLayout) -> DoctorPayload {
        let mut object = owned_object("agentsight", LifecycleStatus::Installed);
        push_owned_service(
            &mut object,
            service_ref("agentsight.service", ServiceScope::System),
        );
        let view = system_view_with_layout(layout.clone(), state_with_component(object));
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let view_ctx = DoctorViewContext {
            resolver_env: &resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query: &rpm_query,
            current_system_service: &system_service,
            system_scope_service: &system_service,
            user_service: &user_service,
            dry_run: false,
        };
        diagnose_from_view(&view, Some("agentsight"), &view_ctx).expect("diagnose test view")
    }

    fn state_with_component(installation: Installation) -> StateStore {
        let mut store = StateStore::empty();
        store.installations.push(installation);
        store
    }

    fn state_with_committed_legacy_journal(layout: &FsLayout) -> StateStore {
        let pending =
            rpm_install::begin_fresh_install(layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin legacy journal");
        let operation_id = pending.transaction.operation_id.clone();
        drop(pending);
        let mut state = StateStore::empty();
        state.operations.push(OperationRecord {
            id: operation_id,
            command: "install cosh".to_string(),
            status: "ok".to_string(),
            started_at: "2026-07-21T00:00:00Z".to_string(),
            finished_at: Some("2026-07-21T00:00:01Z".to_string()),
            parent_operation_id: None,
        });
        state
    }

    fn quarantined_state(name: &str) -> StateStore {
        let mut store = StateStore::empty();
        store.quarantined.push(QuarantinedObject {
            record: InstalledObject {
                kind: ObjectKind::Component,
                name: name.to_string(),
                version: "0.1.0".to_string(),
                status: ObjectStatus::Failed,
                manifest_digest: None,
                distribution_source: None,
                raw_package: None,
                install_backend: None,
                ownership: None,
                rpm_metadata: None,
                installed_at: "2026-06-01T00:00:00Z".to_string(),
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
            },
            reason: QuarantineReason::NoEvidence,
        });
        store
    }

    fn scoped_doctor_view(mut user_state: StateStore, mut system_state: StateStore) -> StateView {
        for installation in &mut user_state.installations {
            installation.scope = InstallationScope::User { uid: 1000 };
        }
        for installation in &mut system_state.installations {
            installation.scope = InstallationScope::System;
        }
        let user_root = ScopedStateRoot {
            scope: StateScope::User,
            layout: FsLayout::user_with_overrides(
                PathBuf::from("/tmp/anolisa-home"),
                None,
                None,
                Some(PathBuf::from("/tmp/anolisa-user-state")),
                None,
                None,
            ),
            state_path: PathBuf::from("/tmp/anolisa-user-state/installed.toml"),
            writable: true,
            state: user_state,
        };
        let system_root = ScopedStateRoot {
            scope: StateScope::System,
            layout: FsLayout::system(Some(PathBuf::from("/tmp/anolisa-system"))),
            state_path: PathBuf::from("/tmp/anolisa-system-state/installed.toml"),
            writable: false,
            state: system_state,
        };
        StateView {
            writable: user_root.clone(),
            visible_roots: vec![user_root, system_root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// System-only view over a caller-supplied layout, for tests that need
    /// on-disk fixtures (e.g. installed-manifest snapshots) under a tempdir.
    fn system_view_with_layout(layout: FsLayout, system_state: StateStore) -> StateView {
        let system_root = ScopedStateRoot {
            scope: StateScope::System,
            state_path: layout.state_dir.join("installed.toml"),
            layout,
            writable: true,
            state: system_state,
        };
        StateView {
            writable: system_root.clone(),
            visible_roots: vec![system_root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Write an installed-manifest snapshot for `component` under the
    /// layout's state_dir — the file `resolve_component_manifest` consumes.
    fn write_manifest_snapshot(layout: &FsLayout, component: &str, manifest: &str) {
        let dir = layout.state_dir.join("component-manifests").join(component);
        std::fs::create_dir_all(&dir).expect("snapshot dir");
        std::fs::write(dir.join("component.toml"), manifest).expect("write snapshot");
    }

    fn system_only_doctor_view(system_state: StateStore) -> StateView {
        let system_root = ScopedStateRoot {
            scope: StateScope::System,
            layout: FsLayout::system(Some(PathBuf::from("/tmp/anolisa-system"))),
            state_path: PathBuf::from("/tmp/anolisa-system-state/installed.toml"),
            writable: true,
            state: system_state,
        };
        StateView {
            writable: system_root.clone(),
            visible_roots: vec![system_root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn diagnose_test_view(view: &StateView, component: Option<&str>) -> DoctorPayload {
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let view_ctx = DoctorViewContext {
            resolver_env: &resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query: &rpm_query,
            current_system_service: &system_service,
            system_scope_service: &system_service,
            user_service: &user_service,
            dry_run: true,
        };
        diagnose_from_view(view, component, &view_ctx).expect("diagnose test view")
    }

    #[test]
    fn user_doctor_view_includes_system_component_as_read_only() {
        let view = scoped_doctor_view(
            StateStore::empty(),
            state_with_component(delegated_object("agentsight", LifecycleStatus::Installed)),
        );

        let payload = diagnose_test_view(&view, Some("agentsight"));

        assert_eq!(payload.components.len(), 1);
        let component = &payload.components[0];
        assert_eq!(component.name, "agentsight");
        assert_eq!(component.scope, "system");
        assert!(component.active);
        assert!(!component.mutable_by_current_invocation);
        assert_eq!(
            component.state_path.as_deref(),
            Some("/tmp/anolisa-system-state/installed.toml")
        );
        assert_eq!(
            component
                .fix_plan
                .iter()
                .find(|fix| fix.action == "repair_state")
                .and_then(|fix| fix.command.as_deref()),
            Some("sudo anolisa --install-mode system repair agentsight")
        );
    }

    #[test]
    fn named_doctor_view_reports_shadowed_system_record() {
        let view = scoped_doctor_view(
            state_with_component(delegated_object("agentsight", LifecycleStatus::Installed)),
            state_with_component(delegated_object("agentsight", LifecycleStatus::Installed)),
        );

        let payload = diagnose_test_view(&view, Some("agentsight"));

        assert_eq!(payload.components.len(), 2);
        assert_eq!(payload.components[0].scope, "user");
        assert!(payload.components[0].active);
        assert!(payload.components[0].mutable_by_current_invocation);
        assert_eq!(payload.components[1].scope, "system");
        assert!(!payload.components[1].active);
        assert_eq!(payload.components[1].shadowed_by.as_deref(), Some("user"));
        assert!(!payload.components[1].mutable_by_current_invocation);
        assert_eq!(
            payload.components[0]
                .fix_plan
                .iter()
                .find(|fix| fix.action == "repair_state")
                .and_then(|fix| fix.command.as_deref()),
            Some("anolisa --install-mode user repair agentsight")
        );
        assert_eq!(
            payload.components[1]
                .fix_plan
                .iter()
                .find(|fix| fix.action == "repair_state")
                .and_then(|fix| fix.command.as_deref()),
            Some("sudo anolisa --install-mode system repair agentsight")
        );
    }

    #[test]
    fn unnamed_doctor_view_diagnoses_only_active_components() {
        let user_state =
            state_with_component(delegated_object("agentsight", LifecycleStatus::Installed));
        let mut system_state =
            state_with_component(delegated_object("agentsight", LifecycleStatus::Failed));
        system_state
            .installations
            .push(delegated_object("agent-memory", LifecycleStatus::Installed));
        let view = scoped_doctor_view(user_state, system_state);

        let payload = diagnose_test_view(&view, None);

        assert_eq!(payload.components.len(), 2);
        assert!(payload.components.iter().all(|component| component.active));
        assert!(
            payload
                .components
                .iter()
                .any(|component| { component.name == "agentsight" && component.scope == "user" })
        );
        assert!(
            payload.components.iter().any(|component| {
                component.name == "agent-memory" && component.scope == "system"
            })
        );
        assert_eq!(payload.summary.failed, 0);
    }

    #[test]
    fn named_missing_component_uses_state_absent_snapshot() {
        let view = system_only_doctor_view(StateStore::empty());

        let payload = diagnose_test_view(&view, Some("ghost"));

        assert_eq!(payload.components.len(), 1);
        let component = &payload.components[0];
        assert_eq!(component.name, "ghost");
        assert_eq!(component.scope, "none");
        assert!(!component.active);
        assert!(!component.mutable_by_current_invocation);
        assert_eq!(component.state_path, None);
        assert_eq!(component.state_status.as_deref(), Some("not_installed"));
        assert_eq!(component.status, "not_installed");
    }

    #[test]
    fn doctor_queries_native_package_exactly_once() {
        let object =
            resolved_delegated_object("cosh", "copilot-shell", ManagementRelation::Observed);
        let view = system_only_doctor_view(state_with_component(object));
        let resolver_env = ResolverEnv::default();
        let query = CountingPackageQuery {
            calls: Cell::new(0),
        };
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let view_ctx = DoctorViewContext {
            resolver_env: &resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query: &query,
            current_system_service: &system_service,
            system_scope_service: &system_service,
            user_service: &user_service,
            dry_run: false,
        };

        let payload =
            diagnose_from_view(&view, Some("cosh"), &view_ctx).expect("diagnose test view");

        assert_eq!(query.calls.get(), 1);
        assert!(
            payload.components[0]
                .health_checks
                .iter()
                .any(|check| check.name == "rpmdb:copilot-shell" && check.status == "ok")
        );
    }

    #[test]
    fn doctor_projects_owned_file_evidence_from_snapshot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let missing = layout.bin_dir.join("agentsight");
        let mut object = owned_object("agentsight", LifecycleStatus::Installed);
        push_owned_file(&mut object, missing.clone());
        let view = system_view_with_layout(layout, state_with_component(object));

        let payload = diagnose_test_view(&view, Some("agentsight"));

        let component = &payload.components[0];
        assert!(component.health_checks.iter().any(|check| {
            check.name == format!("integrity:{}", missing.display())
                && check.status == "missing_file"
        }));
        assert!(
            component
                .findings
                .iter()
                .any(|finding| finding.code == "health_missing_file")
        );
    }

    #[test]
    fn doctor_reports_installed_manifest_parse_failure_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        write_manifest_snapshot(&layout, "agentsight", "invalid = [");
        let view = system_view_with_layout(
            layout,
            state_with_component(owned_object("agentsight", LifecycleStatus::Installed)),
        );

        let payload = diagnose_test_view(&view, Some("agentsight"));

        let manifest_findings = payload.components[0]
            .findings
            .iter()
            .filter(|finding| finding.code == "manifest_unavailable")
            .collect::<Vec<_>>();
        assert_eq!(manifest_findings.len(), 1);
        assert!(
            manifest_findings[0].detail.as_deref().is_some_and(
                |detail| detail.contains("failed to parse installed manifest snapshot")
            )
        );
    }

    #[test]
    fn system_doctor_view_uses_only_visible_system_root() {
        let view = system_only_doctor_view(state_with_component(delegated_object(
            "system-tool",
            LifecycleStatus::Installed,
        )));

        let payload = diagnose_test_view(&view, None);

        assert_eq!(payload.components.len(), 1);
        assert_eq!(payload.components[0].name, "system-tool");
        assert_eq!(payload.components[0].scope, "system");
        assert!(payload.components[0].mutable_by_current_invocation);
        assert_eq!(
            payload.components[0]
                .fix_plan
                .iter()
                .find(|fix| fix.action == "repair_state")
                .and_then(|fix| fix.command.as_deref()),
            Some("sudo anolisa --install-mode system repair system-tool")
        );
    }

    #[test]
    fn system_record_service_ref_uses_system_scope_manager_in_user_view() {
        let mut object = owned_object("agentsight", LifecycleStatus::Installed);
        push_owned_service(
            &mut object,
            service_ref("agentsight.service", ServiceScope::System),
        );
        let view = scoped_doctor_view(StateStore::empty(), state_with_component(object));
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let invocation_system_service =
            NotSupportedServiceManager::new("user-mode system manager".to_string());
        let system_scope_service = FakeServiceManager::new();
        system_scope_service.set_state(ServiceState::Active);
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let view_ctx = DoctorViewContext {
            resolver_env: &resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query: &rpm_query,
            current_system_service: &invocation_system_service,
            system_scope_service: &system_scope_service,
            user_service: &user_service,
            dry_run: false,
        };

        let payload =
            diagnose_from_view(&view, Some("agentsight"), &view_ctx).expect("diagnose test view");

        let service_ref = payload.components[0]
            .health_checks
            .iter()
            .find(|check| check.name == "service_ref:agentsight.service")
            .expect("service ref health check");
        assert_eq!(service_ref.status, "active");
        assert_eq!(
            system_scope_service.calls(),
            vec![(ServiceOp::Probe, "agentsight.service".to_string())]
        );
    }

    #[test]
    fn system_record_user_service_ref_uses_invocation_user_manager() {
        let mut object = owned_object("agent-memory", LifecycleStatus::Installed);
        push_owned_service(
            &mut object,
            service_ref("anolisa-memory@user.service", ServiceScope::User),
        );
        let view = scoped_doctor_view(StateStore::empty(), state_with_component(object));
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let invocation_system_service =
            NotSupportedServiceManager::new("user-mode system manager".to_string());
        let system_scope_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        user_service.set_state(ServiceState::Active);
        let view_ctx = DoctorViewContext {
            resolver_env: &resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query: &rpm_query,
            current_system_service: &invocation_system_service,
            system_scope_service: &system_scope_service,
            user_service: &user_service,
            dry_run: false,
        };

        let payload =
            diagnose_from_view(&view, Some("agent-memory"), &view_ctx).expect("diagnose test view");

        let service_ref = payload.components[0]
            .health_checks
            .iter()
            .find(|check| check.name == "service_ref:anolisa-memory@user.service")
            .expect("service ref health check");
        assert_eq!(service_ref.status, "active");
        assert!(system_scope_service.calls().is_empty());
        assert_eq!(
            user_service.calls(),
            vec![(ServiceOp::Probe, "anolisa-memory@user.service".to_string())]
        );
    }

    /// The snapshot-declared health check has exactly one executor —
    /// doctor's own structured pass. The status projection inside
    /// `diagnose_from_view` must not run it a second time (it used to,
    /// doubling every binary/command/systemd probe).
    #[test]
    fn doctor_probes_structured_health_exactly_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        write_manifest_snapshot(
            &layout,
            "agentsight",
            r#"
                [component]
                name = "agentsight"
                version = "0.1.0"

                [component.layout]
                modes = ["system"]

                [[component.services]]
                unit = "agentsight.service"

                [component.health_check]
                type = "systemd_active"
                service = "agentsight.service"
            "#,
        );
        let view = system_view_with_layout(
            layout,
            state_with_component(owned_object("agentsight", LifecycleStatus::Installed)),
        );
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        system_service.set_state(ServiceState::Active);
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let view_ctx = DoctorViewContext {
            resolver_env: &resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query: &rpm_query,
            current_system_service: &system_service,
            system_scope_service: &system_service,
            user_service: &user_service,
            dry_run: false,
        };

        let payload =
            diagnose_from_view(&view, Some("agentsight"), &view_ctx).expect("diagnose test view");

        assert_eq!(
            system_service.calls(),
            vec![(ServiceOp::Probe, "agentsight.service".to_string())],
            "the structured check must probe exactly once"
        );
        assert_eq!(
            payload.components[0]
                .health_checks
                .iter()
                .filter(|check| check.name.contains("systemd_active"))
                .count(),
            1,
            "only doctor's own pass may report the structured check"
        );
    }

    /// `doctor --dry-run` must not start any probe process: the projection
    /// no longer executes manifest health, and doctor's own pass skips
    /// every leaf under dry-run.
    #[test]
    fn doctor_dry_run_probes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        write_manifest_snapshot(
            &layout,
            "agentsight",
            r#"
                [component]
                name = "agentsight"
                version = "0.1.0"

                [component.layout]
                modes = ["system"]

                [[component.services]]
                unit = "agentsight.service"

                [component.health_check]
                type = "systemd_active"
                service = "agentsight.service"
            "#,
        );
        let view = system_view_with_layout(
            layout,
            state_with_component(owned_object("agentsight", LifecycleStatus::Installed)),
        );
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let view_ctx = DoctorViewContext {
            resolver_env: &resolver_env,
            resolve_runtime_dependencies: &unexpected_runtime_dependencies,
            rpm_query: &rpm_query,
            current_system_service: &system_service,
            system_scope_service: &system_service,
            user_service: &user_service,
            dry_run: true,
        };

        let payload =
            diagnose_from_view(&view, Some("agentsight"), &view_ctx).expect("diagnose test view");

        assert!(
            system_service.calls().is_empty() && user_service.calls().is_empty(),
            "dry-run must not touch any service manager"
        );
        let check = payload.components[0]
            .health_checks
            .iter()
            .find(|check| check.name.contains("systemd_active"))
            .expect("structured check present");
        assert_eq!(check.status, "skipped");
    }

    #[test]
    fn read_only_system_fix_plan_uses_system_mode_command() {
        let mut component = empty_component("agentsight");
        component.mutable_by_current_invocation = false;
        component.fix_plan.push(component_suggestion(
            component.remediation_scope,
            "repair_state",
            "repair",
            &component.name,
            "refresh ANOLISA state",
        ));

        assert_eq!(
            component.fix_plan[0].command.as_deref(),
            Some("sudo anolisa --install-mode system repair agentsight")
        );
    }

    #[test]
    fn missing_component_recommends_install() {
        let mut component = empty_component("ghost");
        add_state_finding("not_installed", &mut component);

        assert_eq!(component.findings[0].code, "component_not_installed");
        assert_eq!(
            component.fix_plan[0].command.as_deref(),
            Some("sudo anolisa --install-mode system install ghost")
        );
    }

    #[test]
    fn failed_owned_health_recommends_repair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let missing = layout.bin_dir.join("agentsight");
        let entry = HealthEntry {
            name: format!("integrity:{}", missing.display()),
            status: "missing_file".to_string(),
            checked_at: "2026-06-01T00:00:00Z".to_string(),
            reason: Some("missing file".to_string()),
        };
        let mut obj = owned_object("agentsight", LifecycleStatus::Installed);
        push_owned_file(&mut obj, missing);
        let mut component = empty_component("agentsight");
        add_health_entry(&entry, Some(&obj), &layout, &mut component);

        assert_eq!(component.findings[0].severity, FindingSeverity::Error);
        assert_eq!(
            component.fix_plan[0].command.as_deref(),
            Some("sudo anolisa --install-mode system repair agentsight")
        );

        let mut facts = planner_facts(obj, NativeProbe::NotProbed);
        facts.owned_files_verified = Some(false);
        assert!(matches!(
            plan(&Intent::Repair, &facts),
            Ok(Plan::Execute { .. })
        ));
        assert_eq!(
            plan(
                &Intent::Install(InstallRequest {
                    target: ProviderTarget::Owned {
                        version: "1.0.0".to_string(),
                    },
                    requested_version: None,
                }),
                &facts,
            ),
            Ok(Plan::NoOp {
                reason: NoOpReason::AlreadyInstalled,
            })
        );
    }

    #[test]
    #[cfg(unix)]
    fn mode_mismatch_health_recommends_repair() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        fs::create_dir_all(&layout.bin_dir).expect("bin dir");
        let binary = layout.bin_dir.join("agentsight");
        fs::write(&binary, b"payload").expect("write binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).expect("chmod");

        let mut object = owned_object("agentsight", LifecycleStatus::Installed);
        push_owned_file(&mut object, binary.clone());
        let ProviderBinding::Owned { artifact } = &mut object.binding else {
            panic!("fixture must be owned");
        };
        artifact.files[0].mode = Some("0755".to_string());

        let suggestions = suggestions_for_health(
            StateScope::System,
            "agentsight",
            &format!("integrity:{}", binary.display()),
            "mode_mismatch",
            Some(&object),
            &layout,
        );

        assert_eq!(suggestions[0].action, "repair_component");
        assert_eq!(
            suggestions[0].command.as_deref(),
            Some("sudo anolisa --install-mode system repair agentsight")
        );

        let structured = CheckOutcome {
            spec_label: format!("binary_version binary={}", binary.display()),
            status: CheckStatus::Failed,
            detail: Some("Permission denied (os error 13)".to_string()),
            children: Vec::new(),
        };
        let suggestions = suggestions_for_structured_health(
            StateScope::System,
            "agentsight",
            &object,
            &layout,
            &structured,
        );
        assert_eq!(suggestions[0].action, "repair_component");
        assert_eq!(
            suggestions[0].command.as_deref(),
            Some("sudo anolisa --install-mode system repair agentsight")
        );
    }

    #[test]
    fn missing_owned_manifest_recommends_repair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let manifest_path =
            common::installed_component_manifest_path(&layout, "agentsight", COMMAND)
                .expect("manifest path");
        let mut object = owned_object("agentsight", LifecycleStatus::Installed);
        push_owned_file(&mut object, manifest_path);
        let mut component = empty_component("agentsight");

        add_manifest_warning(
            Some("component contract unavailable".to_string()),
            Some(&object),
            &layout,
            &mut component,
        );

        assert_eq!(
            component.fix_plan[0].command.as_deref(),
            Some("sudo anolisa --install-mode system repair agentsight")
        );
    }

    #[test]
    fn missing_recorded_service_unit_recommends_repair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let service = service_ref("agentsight.service", ServiceScope::System);
        let mut object = owned_object("agentsight", LifecycleStatus::Installed);
        push_owned_file(
            &mut object,
            layout.systemd_unit_dir.join("agentsight.service"),
        );
        let mut component = empty_component("agentsight");

        add_service_ref_outcome(
            &service,
            ServiceState::NotInstalled,
            None,
            &object,
            &layout,
            &mut component,
        );

        assert_eq!(component.findings[0].code, "service_unit_missing");
        assert_eq!(
            component.fix_plan[0].command.as_deref(),
            Some("sudo anolisa --install-mode system repair agentsight")
        );
    }

    struct ServiceProbeRunner {
        scope: ServiceScope,
        response:
            std::sync::Arc<std::sync::Mutex<Option<anolisa_platform::command::CommandOutput>>>,
    }

    impl anolisa_platform::command::CommandRunner for ServiceProbeRunner {
        fn run(
            &self,
            program: &str,
            args: &[&str],
        ) -> std::io::Result<anolisa_platform::command::CommandOutput> {
            assert_eq!(program, "systemctl");
            let expected = if self.scope == ServiceScope::System {
                vec!["is-active", "service-tool.service"]
            } else {
                vec!["--user", "is-active", "service-tool.service"]
            };
            assert_eq!(args, expected);
            Ok(self
                .response
                .lock()
                .unwrap()
                .take()
                .expect("probe must run exactly once"))
        }
    }

    #[test]
    fn doctor_real_service_backend_distinguishes_probe_error_from_unit_state() {
        use anolisa_core::SystemdServiceManager;
        use anolisa_platform::command::CommandOutput;
        use std::sync::{Arc, Mutex};

        for scope in [ServiceScope::System, ServiceScope::User] {
            for (code, stdout, stderr, expected_status, expected_finding) in [
                (
                    1,
                    "",
                    "Failed to connect to bus: Host is down\n",
                    "probe_error",
                    Some("service_probe_failed"),
                ),
                (
                    3,
                    "unknown\n",
                    "",
                    "not_installed",
                    Some("service_unit_missing"),
                ),
                (
                    4,
                    "unknown\n",
                    "",
                    "not_installed",
                    Some("service_unit_missing"),
                ),
                (3, "inactive\n", "", "inactive", Some("service_not_active")),
                (3, "failed\n", "", "failed", Some("service_not_active")),
                (
                    3,
                    "maintenance\n",
                    "",
                    "unknown",
                    Some("service_not_active"),
                ),
                (0, "active\n", "", "active", None),
            ] {
                for (dry_run, disabled) in [(false, false), (true, false), (false, true)] {
                    let temp = tempfile::tempdir().unwrap();
                    let layout = FsLayout::system(Some(temp.path().to_path_buf()));
                    write_manifest_snapshot(
                        &layout,
                        "service-tool",
                        "[component]\nname = \"service-tool\"\nversion = \"1.0.0\"\n",
                    );
                    let mut object = owned_object(
                        "service-tool",
                        if disabled {
                            LifecycleStatus::Disabled
                        } else {
                            LifecycleStatus::Installed
                        },
                    );
                    push_owned_service(&mut object, service_ref("service-tool.service", scope));
                    let view = system_view_with_layout(layout, state_with_component(object));
                    let response = Arc::new(Mutex::new(Some(CommandOutput {
                        code: Some(code),
                        stdout: stdout.to_string(),
                        stderr: stderr.to_string(),
                    })));
                    let manager = SystemdServiceManager::with_runner(
                        scope,
                        ServiceProbeRunner {
                            scope,
                            response: response.clone(),
                        },
                    );
                    let env = ResolverEnv::default();
                    let ctx = DoctorViewContext {
                        resolver_env: &env,
                        resolve_runtime_dependencies: &unexpected_runtime_dependencies,
                        rpm_query: &MissingPackageQuery,
                        current_system_service: &manager,
                        system_scope_service: &manager,
                        user_service: &manager,
                        dry_run,
                    };
                    let payload = diagnose_from_view(&view, Some("service-tool"), &ctx).unwrap();
                    let component = &payload.components[0];
                    let health = component
                        .health_checks
                        .iter()
                        .find(|check| check.source == "service_ref")
                        .unwrap();
                    assert_eq!(response.lock().unwrap().is_some(), dry_run || disabled);
                    let service_findings: Vec<_> = component
                        .findings
                        .iter()
                        .filter(|finding| finding.source == "service_ref")
                        .collect();
                    if dry_run || disabled {
                        assert_eq!(health.status, "skipped");
                        assert!(service_findings.is_empty());
                        continue;
                    }
                    assert_eq!(health.status, expected_status);
                    assert_eq!(
                        service_findings
                            .iter()
                            .map(|finding| finding.code.as_str())
                            .collect::<Vec<_>>(),
                        expected_finding.into_iter().collect::<Vec<_>>()
                    );
                    if code == 1 {
                        let detail = "systemctl is-active service-tool.service exited with status 1: Failed to connect to bus: Host is down";
                        assert_eq!(health.detail.as_deref(), Some(detail));
                        assert_eq!(service_findings[0].detail.as_deref(), Some(detail));
                        assert!(
                            component
                                .fix_plan
                                .iter()
                                .any(|fix| fix.action == "inspect_logs"
                                    && fix.command.as_deref()
                                        == Some(
                                            "sudo anolisa --install-mode system logs service-tool"
                                        ))
                        );
                        assert!(
                            !component
                                .fix_plan
                                .iter()
                                .any(|fix| fix.action == "repair_component"
                                    || fix.action == "restart_component")
                        );
                    }
                    let json = serde_json::to_value(component).unwrap();
                    assert_eq!(
                        json["health_checks"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|check| check["source"] == "service_ref")
                            .unwrap()["status"],
                        expected_status
                    );
                }
            }
        }
    }

    #[test]
    fn managed_missing_package_recommends_executable_repair() {
        let object = resolved_delegated_object(
            "cosh",
            "copilot-shell",
            ManagementRelation::Managed {
                since: "2026-06-01T00:00:00Z".to_string(),
            },
        );
        let fixes = lifecycle_recovery_suggestions(
            StateScope::System,
            "cosh",
            Some(&object),
            RecoveryNeed::DelegatedPackageMissing,
            "restore the missing package",
        );

        assert_eq!(fixes.len(), 1);
        assert_eq!(
            fixes[0].command.as_deref(),
            Some("sudo anolisa --install-mode system repair cosh")
        );

        let facts = planner_facts(object, NativeProbe::Absent);
        assert!(matches!(
            plan(&Intent::Repair, &facts),
            Ok(Plan::Execute { steps, .. })
                if matches!(
                    steps.first(),
                    Some(Step::NativeTransaction {
                        action: NativeAction::Install,
                        packages,
                        ..
                    }) if packages == &["copilot-shell".to_string()]
                )
        ));
        assert_eq!(
            plan(
                &Intent::Install(InstallRequest {
                    target: ProviderTarget::Delegated {
                        pm: NativePm::Rpm,
                        package: "copilot-shell".to_string(),
                        artifact: None,
                    },
                    requested_version: None,
                }),
                &facts,
            ),
            Err(PlanError::ExternallyRemoved)
        );
    }

    #[test]
    fn tracked_missing_package_requires_forget_before_install() {
        for relation in [
            ManagementRelation::Adopted {
                since: "2026-06-01T00:00:00Z".to_string(),
            },
            ManagementRelation::Observed,
        ] {
            let object = resolved_delegated_object("cosh", "copilot-shell", relation);
            let fixes = lifecycle_recovery_suggestions(
                StateScope::System,
                "cosh",
                Some(&object),
                RecoveryNeed::DelegatedPackageMissing,
                "reconcile missing package",
            );

            assert_eq!(fixes.len(), 2);
            assert_eq!(
                fixes[0].command.as_deref(),
                Some("sudo anolisa --install-mode system forget cosh")
            );
            assert_eq!(
                fixes[1].command.as_deref(),
                Some("sudo anolisa --install-mode system install cosh")
            );

            let facts = planner_facts(object, NativeProbe::Absent);
            assert_eq!(
                plan(
                    &Intent::Install(InstallRequest {
                        target: ProviderTarget::Delegated {
                            pm: NativePm::Rpm,
                            package: "copilot-shell".to_string(),
                            artifact: None,
                        },
                        requested_version: None,
                    }),
                    &facts,
                ),
                Err(PlanError::TrackedButAbsent)
            );
        }
    }

    #[test]
    fn pending_journal_replaces_stale_record_sequence_with_repair() {
        for relation in [
            ManagementRelation::Adopted {
                since: "2026-06-01T00:00:00Z".to_string(),
            },
            ManagementRelation::Observed,
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            let journal = Transaction::begin_with_subject(
                "install",
                Some("cosh"),
                layout.state_dir.join("installed.toml"),
                &rpm_install::journal_dir(&layout),
            )
            .expect("pending journal");
            drop(journal);

            let payload = diagnose_missing_delegated(&layout, relation);
            let component = &payload.components[0];
            let lifecycle_commands: Vec<&str> = component
                .fix_plan
                .iter()
                .filter_map(|fix| fix.command.as_deref())
                .filter(|command| is_component_lifecycle_command(command))
                .collect();

            assert_eq!(
                lifecycle_commands,
                vec!["sudo anolisa --install-mode system repair cosh"]
            );
            assert!(
                component
                    .findings
                    .iter()
                    .any(|finding| finding.code == "operation_pending")
            );
            assert!(component.fix_plan.iter().all(|fix| {
                fix.command.as_deref() != Some("sudo anolisa --install-mode system forget cosh")
            }));
            assert!(component.fix_plan.iter().all(|fix| {
                fix.command.as_deref() != Some("sudo anolisa --install-mode system install cosh")
            }));
        }
    }

    #[test]
    fn pending_system_component_repair_is_qualified_after_journal_guard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let user_layout = FsLayout::user_with_overrides(
            root.join("home"),
            Some(root.join("user-data")),
            Some(root.join("user-config")),
            Some(root.join("user-state")),
            Some(root.join("user-cache")),
            Some(root.join("user-runtime")),
        );
        let system_layout = FsLayout::system(Some(root.join("system")));
        let user_root = ScopedStateRoot {
            scope: StateScope::User,
            state_path: user_layout.state_dir.join("installed.toml"),
            layout: user_layout,
            writable: true,
            state: StateStore::empty(),
        };
        let system_state_path = system_layout.state_dir.join("installed.toml");
        let system_root = ScopedStateRoot {
            scope: StateScope::System,
            state_path: system_state_path.clone(),
            layout: system_layout.clone(),
            writable: false,
            state: state_with_component(delegated_object("agentsight", LifecycleStatus::Installed)),
        };
        let view = StateView {
            writable: user_root.clone(),
            visible_roots: vec![user_root, system_root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let journal = Transaction::begin_with_subject(
            "update",
            Some("agentsight"),
            system_state_path,
            &rpm_install::journal_dir(&system_layout),
        )
        .expect("pending journal");
        drop(journal);

        let payload = diagnose_test_view(&view, Some("agentsight"));
        let component = &payload.components[0];
        let lifecycle_commands = component
            .fix_plan
            .iter()
            .filter_map(|fix| fix.command.as_deref())
            .filter(|command| is_component_lifecycle_command(command))
            .collect::<Vec<_>>();

        assert_eq!(
            lifecycle_commands,
            vec!["sudo anolisa --install-mode system repair agentsight"]
        );
        assert!(
            component
                .findings
                .iter()
                .any(|finding| finding.code == "operation_pending")
        );
    }

    #[test]
    fn writable_system_root_recovery_uses_system_mode_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let view = system_view_with_layout(layout.clone(), StateStore::empty());
        let journal = Transaction::begin_with_subject(
            "update",
            Some("agentsight"),
            view.writable.state_path.clone(),
            &rpm_install::journal_dir(&layout),
        )
        .expect("pending journal");
        drop(journal);

        let payload = diagnose_test_view(&view, None);

        assert_eq!(payload.recovery_roots.len(), 1);
        assert_eq!(
            payload.recovery_roots[0]
                .fix_plan
                .iter()
                .find(|fix| fix.action == "repair_component")
                .and_then(|fix| fix.command.as_deref()),
            Some("sudo anolisa --install-mode system repair agentsight"),
            "unexpected recovery root: {:#?}",
            payload.recovery_roots[0]
        );
    }

    #[test]
    fn committed_legacy_journal_does_not_create_a_recovery_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let state = state_with_committed_legacy_journal(&layout);
        let view = system_view_with_layout(layout, state);

        let payload = diagnose_test_view(&view, None);

        assert!(payload.recovery_roots.is_empty());
    }

    #[test]
    fn committed_legacy_journal_does_not_suppress_component_lifecycle_advice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let state = state_with_committed_legacy_journal(&layout);
        let view = system_view_with_layout(layout, state);
        let roots = scan_journal_roots(&view);
        let mut component = empty_component("cosh");
        component.state_path = Some(view.writable.state_path.display().to_string());
        component.fix_plan.push(component_suggestion(
            component.remediation_scope,
            "install_component",
            "install",
            &component.name,
            "restore the component",
        ));
        let mut claimed_journals = BTreeSet::new();

        apply_component_journal_guard(&roots, &mut component, &mut claimed_journals);

        assert_eq!(
            component
                .fix_plan
                .iter()
                .filter_map(|fix| fix.command.as_deref())
                .collect::<Vec<_>>(),
            vec!["sudo anolisa --install-mode system install cosh"]
        );
        assert!(
            component
                .findings
                .iter()
                .all(|finding| finding.code != "operation_pending")
        );
    }

    #[test]
    fn unavailable_system_root_does_not_borrow_user_operation_history() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let user_layout = FsLayout::user_with_overrides(
            root.join("home"),
            Some(root.join("user-data")),
            Some(root.join("user-config")),
            Some(root.join("user-state")),
            Some(root.join("user-cache")),
            Some(root.join("user-runtime")),
        );
        let system_layout = FsLayout::system(Some(root.join("system")));
        let pending = rpm_install::begin_fresh_install(
            &system_layout,
            "cosh",
            "copilot-shell",
            "install cosh",
        )
        .expect("begin system legacy journal");
        let operation_id = pending.transaction.operation_id.clone();
        drop(pending);
        let mut user_state = StateStore::empty();
        user_state.operations.push(OperationRecord {
            id: operation_id,
            command: "install cosh".to_string(),
            status: "ok".to_string(),
            started_at: "2026-07-21T00:00:00Z".to_string(),
            finished_at: Some("2026-07-21T00:00:01Z".to_string()),
            parent_operation_id: None,
        });
        let user_root = ScopedStateRoot {
            scope: StateScope::User,
            state_path: user_layout.state_dir.join("installed.toml"),
            layout: user_layout,
            writable: true,
            state: user_state,
        };
        let system_state_path = system_layout.state_dir.join("installed.toml");
        let view = StateView {
            writable: user_root.clone(),
            visible_roots: vec![user_root],
            unavailable_roots: vec![UnavailableStateRoot {
                scope: StateScope::System,
                state_path: system_state_path,
                reason: "system state unavailable".to_string(),
            }],
            warnings: vec!["system state unavailable".to_string()],
        };

        let payload = diagnose_test_view(&view, None);

        assert!(payload.recovery_roots.iter().any(|recovery| {
            recovery.scope == "system"
                && recovery
                    .findings
                    .iter()
                    .any(|finding| finding.code == "operation_pending_unattributed")
        }));
    }

    #[test]
    fn corrupt_journal_suppresses_executable_lifecycle_advice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let journal_dir = rpm_install::journal_dir(&layout);
        std::fs::create_dir_all(&journal_dir).expect("journal dir");
        let path = journal_dir.join("broken.journal.toml");
        std::fs::write(&path, "invalid = [").expect("corrupt journal");

        let payload = diagnose_missing_delegated(&layout, ManagementRelation::Observed);
        let component = &payload.components[0];

        assert!(component.fix_plan.iter().all(|fix| {
            fix.command
                .as_deref()
                .is_none_or(|command| !is_component_lifecycle_command(command))
        }));
        assert!(payload.recovery_roots[0].findings.iter().any(|finding| {
            finding.code == "operation_journal_unreadable"
                && finding
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains(path.to_string_lossy().as_ref()))
        }));
        assert!(payload.recovery_roots[0].fix_plan.iter().any(|fix| {
            fix.action == "inspect_operation_journal"
                && fix.command.is_none()
                && fix.reason.contains(path.to_string_lossy().as_ref())
        }));
    }

    #[test]
    fn corrupt_journal_suppresses_inactive_service_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let journal_dir = rpm_install::journal_dir(&layout);
        std::fs::create_dir_all(&journal_dir).expect("journal dir");
        std::fs::write(journal_dir.join("broken.journal.toml"), "invalid = [")
            .expect("corrupt journal");

        let payload = diagnose_inactive_owned(&layout);
        let component = &payload.components[0];

        assert!(
            component
                .fix_plan
                .iter()
                .all(|fix| fix.action != "restart_component")
        );
        assert!(component.fix_plan.iter().any(|fix| {
            fix.action == "inspect_logs"
                && fix.command.as_deref()
                    == Some("sudo anolisa --install-mode system logs agentsight")
        }));
    }

    #[test]
    fn unattributed_journal_suppresses_inactive_service_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let _pending = Transaction::begin(
            "install",
            layout.state_dir.join("installed.toml"),
            &rpm_install::journal_dir(&layout),
        )
        .expect("unattributed pending journal");

        let payload = diagnose_inactive_owned(&layout);
        let component = &payload.components[0];

        assert!(
            component
                .fix_plan
                .iter()
                .all(|fix| fix.action != "restart_component")
        );
        assert!(component.fix_plan.iter().any(|fix| {
            fix.action == "inspect_logs"
                && fix.command.as_deref()
                    == Some("sudo anolisa --install-mode system logs agentsight")
        }));
    }

    #[test]
    fn matching_journal_replaces_inactive_service_restart_with_repair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let _pending = Transaction::begin_with_subject(
            "update",
            Some("agentsight"),
            layout.state_dir.join("installed.toml"),
            &rpm_install::journal_dir(&layout),
        )
        .expect("matching pending journal");

        let payload = diagnose_inactive_owned(&layout);
        let component = &payload.components[0];

        assert!(
            component
                .fix_plan
                .iter()
                .all(|fix| fix.action != "restart_component")
        );
        assert_eq!(
            component
                .fix_plan
                .iter()
                .filter_map(|fix| fix.command.as_deref())
                .filter(|command| is_component_lifecycle_command(command))
                .collect::<Vec<_>>(),
            vec!["sudo anolisa --install-mode system repair agentsight"]
        );
        assert!(
            component
                .fix_plan
                .iter()
                .any(|fix| fix.action == "inspect_logs")
        );
    }

    #[test]
    fn future_journal_schema_suppresses_executable_lifecycle_advice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let journal_dir = rpm_install::journal_dir(&layout);
        let journal = Transaction::begin_with_subject(
            "install",
            Some("cosh"),
            layout.state_dir.join("installed.toml"),
            &journal_dir,
        )
        .expect("journal");
        let path = journal.journal_path.clone();
        drop(journal);
        let text = std::fs::read_to_string(&path).expect("read journal");
        std::fs::write(
            &path,
            text.replacen(
                &format!(
                    "schema_version = {}",
                    anolisa_core::transaction::JOURNAL_SCHEMA_VERSION
                ),
                "schema_version = 999",
                1,
            ),
        )
        .expect("future schema");

        let payload = diagnose_missing_delegated(
            &layout,
            ManagementRelation::Adopted {
                since: "2026-06-01T00:00:00Z".to_string(),
            },
        );
        let component = &payload.components[0];

        assert!(component.fix_plan.iter().all(|fix| {
            fix.command
                .as_deref()
                .is_none_or(|command| !is_component_lifecycle_command(command))
        }));
        assert!(payload.recovery_roots[0].findings.iter().any(|finding| {
            finding.code == "operation_journal_unreadable"
                && finding
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("schema_version 999"))
        }));
    }

    #[test]
    fn corrupt_journal_is_reported_without_active_components() {
        for state in [StateStore::empty(), quarantined_state("legacy-name")] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            let journal_dir = rpm_install::journal_dir(&layout);
            std::fs::create_dir_all(&journal_dir).expect("journal dir");
            let path = journal_dir.join("broken.journal.toml");
            std::fs::write(&path, "invalid = [").expect("corrupt journal");
            let view = system_view_with_layout(layout, state);

            let payload = diagnose_test_view(&view, None);

            assert!(payload.components.is_empty());
            assert!(payload_has_issues(&payload));
            assert_eq!(payload.recovery_roots.len(), 1);
            assert_eq!(payload.recovery_roots[0].findings.len(), 1);
            assert!(
                payload.recovery_roots[0].findings[0]
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains(path.to_string_lossy().as_ref()))
            );
        }
    }

    #[test]
    fn corrupt_journal_is_reported_once_for_a_multi_component_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let journal_dir = rpm_install::journal_dir(&layout);
        std::fs::create_dir_all(&journal_dir).expect("journal dir");
        std::fs::write(journal_dir.join("broken.journal.toml"), "invalid = [")
            .expect("corrupt journal");
        let mut state = StateStore::empty();
        state
            .installations
            .push(owned_object("component-a", LifecycleStatus::Installed));
        state
            .installations
            .push(owned_object("component-b", LifecycleStatus::Installed));
        let view = system_view_with_layout(layout, state);

        let payload = diagnose_test_view(&view, None);

        assert_eq!(payload.components.len(), 2);
        assert_eq!(payload.recovery_roots.len(), 1);
        assert_eq!(payload.recovery_roots[0].findings.len(), 1);
        assert!(payload.components.iter().all(|component| {
            component.findings.iter().all(|finding| {
                finding.code != "operation_journal_unreadable"
                    && finding.code != "operation_journal_scan_failed"
            }) && component.fix_plan.iter().all(|fix| {
                fix.command
                    .as_deref()
                    .is_none_or(|command| !is_component_lifecycle_command(command))
            })
        }));
    }

    #[test]
    fn unproven_artifact_damage_has_no_noop_command() {
        let layout = FsLayout::system(None);
        let owned = owned_object("agentsight", LifecycleStatus::Installed);
        let owned_fixes = lifecycle_recovery_suggestions(
            StateScope::System,
            "agentsight",
            Some(&owned),
            RecoveryNeed::ArtifactDamage { repairable: false },
            "restore an untracked service unit",
        );
        assert_eq!(owned_fixes.len(), 1);
        assert_eq!(owned_fixes[0].command, None);

        let managed = resolved_delegated_object(
            "cosh",
            "copilot-shell",
            ManagementRelation::Managed {
                since: "2026-06-01T00:00:00Z".to_string(),
            },
        );
        let managed_fixes = lifecycle_recovery_suggestions(
            StateScope::System,
            "cosh",
            Some(&managed),
            RecoveryNeed::ArtifactDamage { repairable: false },
            "restore package-owned files",
        );
        assert_eq!(managed_fixes.len(), 1);
        assert_eq!(managed_fixes[0].command, None);

        assert!(!owned_file_damage_matches(Some(&owned), &layout, |_| true));
    }

    #[test]
    fn unresolved_dependency_becomes_remediation() {
        let resolution = DependencyResolution {
            name: "btrfs-progs".to_string(),
            kind: DependencyKind::SystemPackage,
            status: DependencyStatus::Unresolved {
                remediation: "sudo dnf install btrfs-progs".to_string(),
            },
            detail: None,
        };
        let mut component = empty_component("ws-ckpt");
        add_dependency_resolution(&resolution, &mut component);

        assert_eq!(
            component.dependencies[0].status,
            DoctorDependencyStatus::Unresolved
        );
        assert_eq!(component.findings[0].code, "dependency_unresolved");
        assert_eq!(
            component.fix_plan[0].command.as_deref(),
            Some("sudo dnf install btrfs-progs")
        );
    }

    #[test]
    fn rpm_record_discards_raw_layout_manifest_health() {
        let mut obj = delegated_object("agentsight", LifecycleStatus::Installed);
        obj.health.push(HealthEntry {
            name: "agentsight:command:launcher".to_string(),
            status: "command_error".to_string(),
            checked_at: "2026-06-01T00:00:00Z".to_string(),
            reason: Some("raw bindir probe failed".to_string()),
        });
        obj.health.push(HealthEntry {
            name: "persisted:last".to_string(),
            status: "ok".to_string(),
            checked_at: "2026-06-01T00:00:00Z".to_string(),
            reason: None,
        });
        let layout = FsLayout::system(None);
        let mut component = empty_component("agentsight");

        add_persisted_health(Some(&obj), &layout, &mut component);

        assert_eq!(component.health_checks.len(), 1);
        assert_eq!(component.health_checks[0].name, "persisted:last");
    }

    #[test]
    fn rpm_missing_contract_does_not_degrade_component() {
        let obj = delegated_object("tokenless", LifecycleStatus::Installed);
        let layout = FsLayout::system(None);
        let mut component = empty_component("tokenless");

        add_manifest_warning(
            Some(
                "component contract unavailable for 'tokenless': no installed snapshot or catalog entry found"
                    .to_string(),
            ),
            Some(&obj),
            &layout,
            &mut component,
        );

        assert!(component.findings.is_empty());
        assert!(component.fix_plan.is_empty());
    }

    #[test]
    fn unverified_health_is_informational_when_state_is_clean() {
        let entry = HealthEntry {
            name: "integrity:/var/lib/anolisa/component-manifests/agent-memory/component.toml"
                .to_string(),
            status: "unverified".to_string(),
            checked_at: "2026-06-01T00:00:00Z".to_string(),
            reason: None,
        };
        let obj = owned_object("agent-memory", LifecycleStatus::Installed);
        let layout = FsLayout::system(None);
        let mut component = empty_component("agent-memory");

        add_state_finding("installed", &mut component);
        add_health_entry(&entry, Some(&obj), &layout, &mut component);

        assert!(component.findings.is_empty());
        assert!(component.fix_plan.is_empty());
        assert_eq!(component.health_checks[0].status, "unverified");
    }

    #[test]
    fn structured_systemd_active_uses_service_manager() {
        let layout = FsLayout::system(None);
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        system_service.set_state(ServiceState::Active);
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let ctx = probe_context(
            &layout,
            &resolver_env,
            &rpm_query,
            &system_service,
            &user_service,
            false,
        );

        let outcome = run_doctor_check(
            &CheckSpec::SystemdActive {
                service: "agentsight.service".to_string(),
            },
            None,
            &ctx,
            false,
        );

        assert_eq!(outcome.status, CheckStatus::Ok);
        assert_eq!(
            system_service.calls(),
            vec![(ServiceOp::Probe, "agentsight.service".to_string())]
        );
    }

    #[test]
    fn structured_systemd_active_uses_manifest_service_scope() {
        let manifest = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "agent-memory"
            version = "0.1.0"

            [component.layout]
            modes = ["system"]

            [[component.services]]
            unit = "anolisa-memory@.service"
            scope = "user"

            [component.health_check]
            type = "systemd_active"
            service = "anolisa-memory@root.service"
        "#,
        )
        .expect("parse manifest");
        let layout = FsLayout::system(None);
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        user_service.set_state(ServiceState::Active);
        let ctx = probe_context(
            &layout,
            &resolver_env,
            &rpm_query,
            &system_service,
            &user_service,
            false,
        );

        let outcome = run_doctor_check(
            &CheckSpec::SystemdActive {
                service: "anolisa-memory@root.service".to_string(),
            },
            Some(&manifest),
            &ctx,
            false,
        );

        assert_eq!(outcome.status, CheckStatus::Ok);
        assert!(system_service.calls().is_empty());
        assert_eq!(
            user_service.calls(),
            vec![(ServiceOp::Probe, "anolisa-memory@root.service".to_string())]
        );
    }

    #[test]
    fn structured_systemd_active_fails_when_unit_is_inactive() {
        let layout = FsLayout::system(None);
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let ctx = probe_context(
            &layout,
            &resolver_env,
            &rpm_query,
            &system_service,
            &user_service,
            false,
        );

        let outcome = run_doctor_check(
            &CheckSpec::SystemdActive {
                service: "agentsight.service".to_string(),
            },
            None,
            &ctx,
            false,
        );

        assert_eq!(outcome.status, CheckStatus::Failed);
        assert!(
            outcome
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("inactive")
        );
    }

    #[test]
    fn structured_systemd_active_is_skipped_for_disabled_component() {
        let layout = FsLayout::system(None);
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let ctx = probe_context(
            &layout,
            &resolver_env,
            &rpm_query,
            &system_service,
            &user_service,
            false,
        );

        let outcome = run_doctor_check(
            &CheckSpec::SystemdActive {
                service: "agentsight.service".to_string(),
            },
            None,
            &ctx,
            true,
        );

        assert_eq!(outcome.status, CheckStatus::Skipped);
        assert!(system_service.calls().is_empty());
        assert!(
            outcome
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("component is disabled")
        );
    }

    #[test]
    fn service_ref_inactive_unit_becomes_recommendation() {
        let layout = FsLayout::system(None);
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let ctx = probe_context(
            &layout,
            &resolver_env,
            &rpm_query,
            &system_service,
            &user_service,
            false,
        );
        let mut obj = owned_object("agentsight", LifecycleStatus::Installed);
        push_owned_service(
            &mut obj,
            service_ref("agentsight.service", ServiceScope::System),
        );
        let mut component = empty_component("agentsight");

        add_service_refs(None, Some(&obj), &ctx, &mut component);

        assert_eq!(component.health_checks[0].status, "inactive");
        assert_eq!(component.findings[0].code, "service_not_active");
        assert_eq!(
            component.fix_plan[0].command.as_deref(),
            Some("sudo anolisa --install-mode system restart agentsight")
        );
    }

    #[test]
    fn service_ref_inactive_unit_is_skipped_for_disabled_component() {
        let layout = FsLayout::system(None);
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let ctx = probe_context(
            &layout,
            &resolver_env,
            &rpm_query,
            &system_service,
            &user_service,
            false,
        );
        let mut obj = owned_object("agentsight", LifecycleStatus::Disabled);
        push_owned_service(
            &mut obj,
            service_ref("agentsight.service", ServiceScope::System),
        );
        let mut component = empty_component("agentsight");

        add_service_refs(None, Some(&obj), &ctx, &mut component);

        assert_eq!(component.health_checks[0].status, "skipped");
        assert!(
            component.health_checks[0]
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("component is disabled")
        );
        assert!(component.findings.is_empty());
        assert!(component.fix_plan.is_empty());
        assert!(system_service.calls().is_empty());
    }

    #[test]
    fn service_ref_start_false_is_skipped() {
        let manifest = ComponentManifest::from_toml_str(
            r#"
            [component]
            name = "agent-memory"
            version = "0.1.0"

            [component.layout]
            modes = ["system"]

            [[component.services]]
            unit = "anolisa-memory@.service"
            scope = "user"
            enable = false
            start = false
        "#,
        )
        .expect("parse manifest");
        let layout = FsLayout::system(None);
        let resolver_env = ResolverEnv::default();
        let rpm_query = RpmPackageQuery::system();
        let system_service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let ctx = probe_context(
            &layout,
            &resolver_env,
            &rpm_query,
            &system_service,
            &user_service,
            false,
        );
        let mut obj = owned_object("agent-memory", LifecycleStatus::Installed);
        push_owned_service(
            &mut obj,
            service_ref("anolisa-memory@root.service", ServiceScope::User),
        );
        let mut component = empty_component("agent-memory");

        add_service_refs(Some(&manifest), Some(&obj), &ctx, &mut component);

        assert_eq!(component.health_checks[0].status, "skipped");
        assert!(component.findings.is_empty());
    }

    #[test]
    fn component_status_escalates_findings() {
        let mut component = empty_component("tokenless");
        component.findings = vec![finding(
            FindingSeverity::Warning,
            "health_unverified",
            "unverified",
            "health",
            None,
        )];
        assert_eq!(component_status(&component), "degraded");
        component.findings.push(finding(
            FindingSeverity::Error,
            "dependency_unresolved",
            "missing",
            "dependency",
            None,
        ));
        assert_eq!(component_status(&component), "failed");
    }

    const RUNTIME_DEPS: &str = r#"
        [[component.dependencies]]
        name = "libfoo"
        kind = "system-package"
        [[component.dependencies]]
        name = "node"
        kind = "language-runtime"
        version = ">=20"
        [[component.dependencies]]
        name = "kernel-btrfs"
        kind = "platform-capability"
        check = "btrfs"
    "#;

    struct RuntimeRunner {
        calls: std::cell::RefCell<Vec<String>>,
        outputs: std::cell::RefCell<
            std::collections::VecDeque<std::io::Result<anolisa_platform::command::CommandOutput>>,
        >,
    }

    impl RuntimeRunner {
        fn new(outputs: Vec<std::io::Result<anolisa_platform::command::CommandOutput>>) -> Self {
            Self {
                calls: Default::default(),
                outputs: std::cell::RefCell::new(outputs.into()),
            }
        }
    }

    impl anolisa_platform::command::CommandRunner for &RuntimeRunner {
        fn run(
            &self,
            program: &str,
            args: &[&str],
        ) -> std::io::Result<anolisa_platform::command::CommandOutput> {
            self.calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            self.outputs
                .borrow_mut()
                .pop_front()
                .expect("unexpected runtime probe")
        }
    }

    fn runtime_output(
        code: Option<i32>,
        stdout: &str,
    ) -> std::io::Result<anolisa_platform::command::CommandOutput> {
        Ok(anolisa_platform::command::CommandOutput {
            code,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }

    fn diagnose_runtime_fixture(
        layout: &FsLayout,
        object: Option<Installation>,
        deps: Option<&str>,
        dry_run: bool,
        resolve: &ResolveRuntimeDependencies<'_>,
    ) -> DoctorPayload {
        diagnose_runtime_fixture_with_env(
            layout,
            object,
            deps,
            dry_run,
            resolve,
            &ResolverEnv {
                pkg_base: Some("rpm".into()),
                ..Default::default()
            },
        )
    }

    fn diagnose_runtime_fixture_with_env(
        layout: &FsLayout,
        object: Option<Installation>,
        deps: Option<&str>,
        dry_run: bool,
        resolve: &ResolveRuntimeDependencies<'_>,
        env: &ResolverEnv,
    ) -> DoctorPayload {
        if let Some(deps) = deps {
            write_manifest_snapshot(
                layout,
                "runtime-tool",
                &format!("[component]\nname = \"runtime-tool\"\nversion = \"1.0.0\"\n{deps}"),
            );
        }
        let view = system_view_with_layout(
            layout.clone(),
            object
                .map(state_with_component)
                .unwrap_or_else(StateStore::empty),
        );
        let service = FakeServiceManager::new();
        let user_service = FakeServiceManager::with_scope(ServiceScope::User);
        let ctx = DoctorViewContext {
            resolver_env: env,
            resolve_runtime_dependencies: resolve,
            rpm_query: &MissingPackageQuery,
            current_system_service: &service,
            system_scope_service: &service,
            user_service: &user_service,
            dry_run,
        };
        let payload = diagnose_from_view(&view, Some("runtime-tool"), &ctx)
            .expect("diagnose runtime fixture");
        assert!(service.calls().is_empty());
        assert!(user_service.calls().is_empty());
        payload
    }

    #[test]
    fn doctor_dpkg_state_output() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for scenario in ["installed", "config-files", "unpacked", "failure"] {
            for mode in ["human", "json", "dry-run"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        format!("{module}::doctor_dpkg_output_child"),
                        "--exact".into(),
                        "--nocapture".into(),
                    ])
                    .env("ANOLISA_TEST_DPKG_OUTPUT", format!("{scenario}:{mode}"))
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.stderr.is_empty());
                let stdout = String::from_utf8(output.stdout).unwrap();
                let rendered = stdout
                    .split_once("DPKG_BEGIN\n")
                    .unwrap()
                    .1
                    .split_once("DPKG_END\n")
                    .unwrap()
                    .0;
                assert!(
                    stdout.contains(if mode == "dry-run" || scenario == "installed" {
                        "DOMAIN_EXIT=0"
                    } else {
                        "DOMAIN_EXIT=2"
                    })
                );
                if mode == "dry-run" {
                    assert!(!rendered.contains("[dependency_"));
                    continue;
                }
                let (status, finding, action) = match scenario {
                    "installed" => ("resolved", "", ""),
                    "config-files" => ("unresolved", "dependency_unresolved", "install_package"),
                    "unpacked" => (
                        "unresolvable",
                        "dependency_unresolvable",
                        "inspect_package_state",
                    ),
                    _ => (
                        "unresolvable",
                        "dependency_probe_failed",
                        "inspect_dependency_probe",
                    ),
                };
                if mode == "json" {
                    let json: serde_json::Value = serde_json::from_str(rendered).unwrap();
                    assert_eq!(json["command"], "doctor");
                    assert_eq!(json["ok"], scenario == "installed");
                    let component = &json["data"]["components"][0];
                    assert_eq!(component["dependencies"][0]["status"], status);
                    if scenario == "installed" {
                        assert!(component.get("findings").is_none());
                        assert!(component.get("fix_plan").is_none());
                    } else {
                        assert_eq!(component["findings"][0]["code"], finding);
                        assert_eq!(component["fix_plan"].as_array().unwrap().len(), 1);
                        assert_eq!(component["fix_plan"][0]["action"], action);
                        if scenario == "config-files" {
                            assert_eq!(
                                component["fix_plan"][0]["command"],
                                "sudo apt install libfoo"
                            );
                        } else {
                            assert!(component["fix_plan"][0]["command"].is_null());
                            assert_eq!(component["fix_plan"][0]["automatic"], false);
                        }
                    }
                } else if scenario != "installed" {
                    assert!(
                        rendered.contains(&format!("[{finding}]")),
                        "{scenario}/{mode}: {rendered}"
                    );
                    if scenario != "config-files" {
                        assert!(rendered.contains(action));
                    }
                }
                match scenario {
                    "config-files" => assert!(rendered.contains("sudo apt install libfoo")),
                    "unpacked" => {
                        assert!(rendered.contains("install ok unpacked"));
                        assert!(!rendered.contains("sudo apt install"));
                    }
                    "failure" => {
                        assert!(rendered.contains("database unavailable"));
                        assert!(!rendered.contains("sudo apt install"));
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn doctor_dpkg_output_child() {
        let (_, module) = module_path!().split_once("::").unwrap();
        if std::env::args().skip(1).collect::<Vec<_>>()
            != [
                format!("{module}::doctor_dpkg_output_child"),
                "--exact".into(),
                "--nocapture".into(),
            ]
        {
            return;
        }
        let Ok(value) = std::env::var("ANOLISA_TEST_DPKG_OUTPUT") else {
            return;
        };
        let Some((scenario, mode)) = value.split_once(':') else {
            return;
        };
        if !matches!(
            scenario,
            "installed" | "config-files" | "unpacked" | "failure"
        ) || !matches!(mode, "human" | "json" | "dry-run")
        {
            return;
        }
        let sandbox = crate::test_support::TestSandbox::new();
        let ctx = sandbox.context_with(
            crate::context::InstallMode::System,
            crate::test_support::TestContextOptions {
                json: mode == "json",
                dry_run: mode == "dry-run",
                quiet: false,
                ..Default::default()
            },
        );
        let runner = RuntimeRunner::new(vec![Ok(anolisa_platform::command::CommandOutput {
            code: Some(if scenario == "failure" { 2 } else { 0 }),
            stdout: if scenario == "failure" {
                String::new()
            } else {
                format!("libfoo\tamd64\tinstall ok {scenario}\n")
            },
            stderr: if scenario == "failure" {
                "database unavailable\n"
            } else {
                ""
            }
            .into(),
        })]);
        let resolver = DependencyResolver::with_probes(&runner, || panic!("no btrfs read"));
        let payload = diagnose_runtime_fixture_with_env(
            ctx.layout(),
            Some(owned_object("runtime-tool", LifecycleStatus::Installed)),
            Some("[[component.dependencies]]\nname = \"libfoo\"\nkind = \"system-package\""),
            ctx.dry_run,
            &|deps, env| resolver.resolve(deps, env),
            &ResolverEnv {
                pkg_base: Some("deb".into()),
                ..Default::default()
            },
        );
        if ctx.dry_run {
            assert!(runner.calls.borrow().is_empty());
        } else {
            assert_eq!(
                *runner.calls.borrow(),
                [
                    "dpkg-query --show --showformat=${Package}\t${Architecture}\t${Status}\n -- libfoo"
                ]
            );
            assert!(runner.outputs.borrow().is_empty());
        }
        let has_issues = payload_has_issues(&payload);
        println!("DPKG_BEGIN");
        render_doctor(&ctx, &payload, !has_issues).unwrap();
        println!("DPKG_END");
        let code = if has_issues {
            CliError::DiagnosticsFound {
                command: COMMAND.into(),
            }
            .exit_code()
        } else {
            0
        };
        println!("DOMAIN_EXIT={code}");
    }

    #[test]
    fn doctor_native_rpm_probe_failure_output() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for mode in [
            "human",
            "json",
            "quiet",
            "dry-run",
            "stdout-human",
            "stdout-json",
            "custom-system-human",
            "custom-system-json",
            "custom-system-dry-run",
            "custom-language-human",
            "custom-language-json",
            "custom-language-dry-run",
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    format!("{module}::doctor_native_rpm_output_child"),
                    "--exact".into(),
                    "--nocapture".into(),
                ])
                .env("ANOLISA_TEST_NATIVE_RPM_OUTPUT", mode)
                .output()
                .expect("isolated renderer");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            let stdout = String::from_utf8(output.stdout).unwrap();
            let rendered = stdout
                .split_once("RPM_OUTPUT_BEGIN\n")
                .unwrap()
                .1
                .split_once("RPM_OUTPUT_END\n")
                .unwrap()
                .0;
            assert!(stdout.contains(if mode.ends_with("dry-run") {
                "DOMAIN_EXIT=0"
            } else {
                "DOMAIN_EXIT=2"
            }));
            assert!(!rendered.contains("sudo dnf install"));
            assert!(!rendered.contains("fix_manifest"));
            match mode {
                "json" | "stdout-json" | "custom-system-json" | "custom-language-json" => {
                    let json: serde_json::Value = serde_json::from_str(rendered).unwrap();
                    assert_eq!(json["command"], "doctor");
                    assert_eq!(json["ok"], false);
                    assert_eq!(json["schema_version"], crate::response::SCHEMA_VERSION);
                    let component = &json["data"]["components"][0];
                    assert_eq!(component["fix_plan"].as_array().unwrap().len(), 1);
                    assert_eq!(component["dependencies"][0]["status"], "unresolvable");
                    assert_eq!(
                        component["dependencies"][0]["detail"],
                        if mode.starts_with("custom-") {
                            "unexpected libfoo output: libfoo --version failed (code None); stdout: partial version\n"
                        } else if mode == "stdout-json" {
                            "unexpected rpm output: rpm -q libfoo failed (code Some(1)); stdout: package libfoo is not installed\nextra\n"
                        } else {
                            "rpm failed (code Some(1)): rpmdb broken\n"
                        }
                    );
                    assert_eq!(component["findings"][0]["code"], "dependency_probe_failed");
                    assert_eq!(
                        component["fix_plan"][0]["action"],
                        "inspect_dependency_probe"
                    );
                    assert_eq!(component["fix_plan"][0]["automatic"], false);
                    assert!(component["fix_plan"][0]["command"].is_null());
                }
                "human" | "stdout-human" | "custom-system-human" | "custom-language-human" => {
                    assert!(rendered.contains("[dependency_probe_failed]"));
                    if mode.starts_with("custom-") {
                        assert!(rendered.contains("libfoo --version failed (code None)"));
                        assert!(rendered.contains("partial version\n"));
                    } else if mode == "stdout-human" {
                        assert!(rendered.contains("code Some(1)"));
                        assert!(rendered.contains("package libfoo is not installed\nextra\n"));
                    } else {
                        assert!(rendered.contains("rpmdb broken"));
                    }
                    assert!(rendered.contains("inspect_dependency_probe"));
                }
                "quiet" => assert!(rendered.is_empty()),
                "dry-run" | "custom-system-dry-run" | "custom-language-dry-run" => {
                    assert!(!rendered.contains("dependency_probe_failed"))
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn doctor_native_rpm_output_child() {
        let (_, module) = module_path!().split_once("::").unwrap();
        if std::env::args().skip(1).collect::<Vec<_>>()
            != [
                format!("{module}::doctor_native_rpm_output_child"),
                "--exact".into(),
                "--nocapture".into(),
            ]
        {
            return;
        }
        let Ok(mode) = std::env::var("ANOLISA_TEST_NATIVE_RPM_OUTPUT") else {
            return;
        };
        if !matches!(
            mode.as_str(),
            "human"
                | "json"
                | "quiet"
                | "dry-run"
                | "stdout-human"
                | "stdout-json"
                | "custom-system-human"
                | "custom-system-json"
                | "custom-system-dry-run"
                | "custom-language-human"
                | "custom-language-json"
                | "custom-language-dry-run"
        ) {
            return;
        }
        let sandbox = crate::test_support::TestSandbox::new();
        let ctx = sandbox.context_with(
            crate::context::InstallMode::System,
            crate::test_support::TestContextOptions {
                json: mode.ends_with("json"),
                quiet: mode == "quiet",
                dry_run: mode.ends_with("dry-run"),
                ..Default::default()
            },
        );
        let runner = RuntimeRunner::new(vec![Ok(anolisa_platform::command::CommandOutput {
            code: if mode.starts_with("custom-") {
                None
            } else {
                Some(1)
            },
            stdout: if mode.starts_with("custom-") {
                "partial version\n"
            } else if mode.starts_with("stdout-") {
                "package libfoo is not installed\nextra\n"
            } else {
                "package libfoo is not installed\n"
            }
            .into(),
            stderr: if mode.starts_with("stdout-") || mode.starts_with("custom-") {
                ""
            } else {
                "rpmdb broken\n"
            }
            .into(),
        })]);
        let resolver = DependencyResolver::with_probes(&runner, || panic!("no btrfs probe"));
        let payload = diagnose_runtime_fixture(
            ctx.layout(),
            Some(owned_object("runtime-tool", LifecycleStatus::Installed)),
            Some(if mode.starts_with("custom-system-") {
                "[[component.dependencies]]\nname = \"libfoo\"\nkind = \"system-package\"\nprobe = \"libfoo --version\""
            } else if mode.starts_with("custom-language-") {
                "[[component.dependencies]]\nname = \"libfoo\"\nkind = \"language-runtime\"\nversion = \">=20\""
            } else {
                "[[component.dependencies]]\nname = \"libfoo\"\nkind = \"system-package\""
            }),
            ctx.dry_run,
            &|deps, env| resolver.resolve(deps, env),
        );
        assert_eq!(runner.calls.borrow().len(), usize::from(!ctx.dry_run));
        if !ctx.dry_run {
            assert!(runner.outputs.borrow().is_empty());
            assert_eq!(
                *runner.calls.borrow(),
                [if mode.starts_with("custom-") {
                    "libfoo --version"
                } else {
                    "rpm -q libfoo"
                }]
            );
        }
        let has_issues = payload_has_issues(&payload);
        println!("RPM_OUTPUT_BEGIN");
        render_doctor(&ctx, &payload, !has_issues).expect("render");
        println!("RPM_OUTPUT_END");
        let code = if has_issues {
            CliError::DiagnosticsFound {
                command: COMMAND.into(),
            }
            .exit_code()
        } else {
            0
        };
        println!("DOMAIN_EXIT={code}");
    }

    #[test]
    fn doctor_runtime_dependencies_use_real_resolver_exactly_once() {
        for healthy in [true, false] {
            let temp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(temp.path().to_path_buf()));
            let runner = RuntimeRunner::new(vec![
                runtime_output(
                    Some(if healthy { 0 } else { 1 }),
                    if healthy {
                        ""
                    } else {
                        "package libfoo is not installed\n"
                    },
                ),
                runtime_output(Some(0), if healthy { "v20.0.0" } else { "v18.0.0" }),
            ]);
            let reads = Cell::new(0);
            let resolutions = Cell::new(0);
            let resolver = DependencyResolver::with_probes(&runner, || {
                reads.set(reads.get() + 1);
                Ok(if healthy { "btrfs\n" } else { "ext4\n" }.to_string())
            });
            let resolve = |deps: &[RuntimeDependency], env: &ResolverEnv| {
                resolutions.set(resolutions.get() + 1);
                resolver.resolve(deps, env)
            };
            let payload = diagnose_runtime_fixture(
                &layout,
                Some(owned_object("runtime-tool", LifecycleStatus::Installed)),
                Some(RUNTIME_DEPS),
                false,
                &resolve,
            );
            assert_eq!(resolutions.get(), 1);
            assert_eq!(reads.get(), 1);
            assert_eq!(*runner.calls.borrow(), ["rpm -q libfoo", "node --version"]);
            assert!(runner.outputs.borrow().is_empty());
            let component = &payload.components[0];
            assert_eq!(
                component
                    .dependencies
                    .iter()
                    .map(|dep| dep.name.as_str())
                    .collect::<Vec<_>>(),
                ["libfoo", "node", "kernel-btrfs"]
            );
            assert_eq!(payload_has_issues(&payload), !healthy);
            assert_eq!(component.status, if healthy { "ok" } else { "failed" });
            if healthy {
                assert!(
                    component
                        .dependencies
                        .iter()
                        .all(|dep| dep.status == DoctorDependencyStatus::Resolved)
                );
                assert!(component.findings.is_empty());
                assert!(component.fix_plan.is_empty());
                assert_eq!(payload.summary.ok, 1);
            } else {
                assert_eq!(
                    component
                        .dependencies
                        .iter()
                        .map(|dep| dep.status)
                        .collect::<Vec<_>>(),
                    [
                        DoctorDependencyStatus::Unresolved,
                        DoctorDependencyStatus::Unresolved,
                        DoctorDependencyStatus::Unresolvable
                    ]
                );
                assert_eq!(
                    component.dependencies[1].detail.as_deref(),
                    Some("found 18.0.0, need >=20")
                );
                assert_eq!(
                    component
                        .findings
                        .iter()
                        .map(|finding| finding.code.as_str())
                        .collect::<Vec<_>>(),
                    [
                        "dependency_unresolved",
                        "dependency_unresolved",
                        "dependency_unresolvable"
                    ]
                );
                assert_eq!(component.fix_plan.len(), 3);
                assert_eq!(
                    component.fix_plan[0].command.as_deref(),
                    Some("sudo dnf install libfoo")
                );
                assert_eq!(component.fix_plan[2].action, "satisfy_platform_requirement");
                assert_eq!(payload.summary.failed, 1);
                assert_eq!(
                    CliError::DiagnosticsFound {
                        command: COMMAND.to_string()
                    }
                    .exit_code(),
                    2
                );
            }
            let json = serde_json::to_value(&payload).expect("serialize report");
            assert_eq!(json["dry_run"], false);
            assert_eq!(
                json["components"][0]["dependencies"][0]["status"],
                if healthy { "resolved" } else { "unresolved" }
            );
        }
    }

    #[test]
    fn doctor_runtime_probe_errors_distinguish_execution_faults_from_platform_limits() {
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(temp.path().to_path_buf()));
            let runner = RuntimeRunner::new(vec![
                Err(std::io::Error::new(kind, "scripted spawn failure")),
                runtime_output(None, ""),
            ]);
            let resolver = DependencyResolver::with_probes(&runner, || {
                Err(std::io::Error::new(kind, "scripted read failure"))
            });
            let payload = diagnose_runtime_fixture(
                &layout,
                Some(owned_object("runtime-tool", LifecycleStatus::Installed)),
                Some(RUNTIME_DEPS),
                false,
                &|deps, env| resolver.resolve(deps, env),
            );
            let component = &payload.components[0];
            assert_eq!(
                component.dependencies[0].status,
                DoctorDependencyStatus::Unresolvable
            );
            assert_eq!(component.findings[0].code, "dependency_probe_failed");
            assert!(
                !component
                    .fix_plan
                    .iter()
                    .any(|fix| fix.command.as_deref() == Some("sudo dnf install libfoo"))
            );
            assert_eq!(
                component.dependencies[1].status,
                DoctorDependencyStatus::Unresolvable
            );
            assert_eq!(component.findings[1].code, "dependency_probe_failed");
            assert_eq!(
                component.dependencies[1].detail.as_deref(),
                Some("node failed (code None): ")
            );
            assert!(
                !component
                    .fix_plan
                    .iter()
                    .any(|fix| fix.action == "install_runtime")
            );
            assert_eq!(
                component.dependencies[2].status,
                DoctorDependencyStatus::Unresolvable
            );
            assert_eq!(
                component.findings[2].detail.as_deref(),
                Some("could not read /proc/filesystems to verify btrfs support")
            );
            assert_eq!(*runner.calls.borrow(), ["rpm -q libfoo", "node --version"]);
        }
    }

    #[test]
    fn doctor_runtime_declaration_error_uses_manifest_finding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(temp.path().to_path_buf()));
        let runner = RuntimeRunner::new(Vec::new());
        let resolver = DependencyResolver::with_probes(&runner, || -> std::io::Result<String> {
            panic!("unknown checks must not read filesystems")
        });
        let payload = diagnose_runtime_fixture(
            &layout,
            Some(owned_object("runtime-tool", LifecycleStatus::Installed)),
            Some(
                "[[component.dependencies]]\nname = \"future\"\nkind = \"platform-capability\"\ncheck = \"unknown\"",
            ),
            false,
            &|deps, env| resolver.resolve(deps, env),
        );
        let component = &payload.components[0];
        assert!(component.dependencies.is_empty());
        assert_eq!(component.findings.len(), 1);
        assert_eq!(component.findings[0].code, "invalid_dependency_declaration");
        assert_eq!(
            component.findings[0].detail.as_deref(),
            Some("dependency 'future' has unknown platform-capability check 'unknown'")
        );
        assert_eq!(component.fix_plan[0].action, "fix_manifest");
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn doctor_runtime_skip_paths_never_resolve() {
        for scenario in [
            "absent",
            "no-manifest",
            "empty",
            "rpm",
            "dry-run",
            "rpm-dry-run",
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(temp.path().to_path_buf()));
            let object = match scenario {
                "absent" => None,
                "rpm" | "rpm-dry-run" => {
                    Some(delegated_object("runtime-tool", LifecycleStatus::Installed))
                }
                _ => Some(owned_object("runtime-tool", LifecycleStatus::Installed)),
            };
            let deps = match scenario {
                "no-manifest" => None,
                "empty" => Some(""),
                _ => Some(RUNTIME_DEPS),
            };
            let payload = diagnose_runtime_fixture(
                &layout,
                object,
                deps,
                scenario.contains("dry-run"),
                &unexpected_runtime_dependencies,
            );
            let dependencies = &payload.components[0].dependencies;
            if matches!(scenario, "rpm" | "dry-run" | "rpm-dry-run") {
                assert_eq!(dependencies.len(), 3);
                let note = if scenario.starts_with("rpm") {
                    "RPM backend owns runtime dependency resolution"
                } else {
                    "dry-run: dependency probe not executed"
                };
                assert!(
                    dependencies
                        .iter()
                        .all(|dep| dep.status == DoctorDependencyStatus::Skipped
                            && dep.note.as_deref() == Some(note))
                );
            } else {
                assert!(dependencies.is_empty(), "{scenario}");
            }
        }
    }

    #[test]
    fn doctor_runtime_warning_and_version_detail_keep_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(temp.path().to_path_buf()));
        let runner = RuntimeRunner::new(vec![
            runtime_output(Some(0), ""),
            runtime_output(Some(0), "unknown version"),
        ]);
        let resolver = DependencyResolver::with_probes(&runner, || Ok("ext4\n".to_string()));
        let resolve = |deps: &[RuntimeDependency], env: &ResolverEnv| {
            let mut plan = resolver.resolve(deps, env)?;
            // The resolver currently emits no aggregate warnings; exercise the
            // callback's existing warning projection without changing that policy.
            plan.warnings.push("scripted resolver warning".to_string());
            Ok(plan)
        };
        let payload = diagnose_runtime_fixture(
            &layout,
            Some(owned_object("runtime-tool", LifecycleStatus::Disabled)),
            Some(RUNTIME_DEPS),
            false,
            &resolve,
        );
        let component = &payload.components[0];
        assert_eq!(
            component.dependencies[1].status,
            DoctorDependencyStatus::Resolved
        );
        assert_eq!(
            component.dependencies[1].detail.as_deref(),
            Some("version not verified against '>=20'")
        );
        assert_eq!(
            component
                .findings
                .iter()
                .map(|finding| finding.code.as_str())
                .collect::<Vec<_>>(),
            ["dependency_warning", "dependency_unresolvable"]
        );
        assert_eq!(component.findings[0].severity, FindingSeverity::Warning);
        assert_eq!(component.findings[0].message, "scripted resolver warning");
        assert_eq!(*runner.calls.borrow(), ["rpm -q libfoo", "node --version"]);
    }

    #[test]
    fn doctor_runtime_output_preserves_human_json_and_quiet() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for healthy in [true, false] {
            for mode in ["human", "json", "quiet"] {
                // Capture the real renderer without redirecting process-global stdout
                // in this test process, which runs other diagnostics concurrently.
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        format!("{module}::doctor_runtime_output_child"),
                        "--exact".to_string(),
                        "--nocapture".to_string(),
                    ])
                    .env("ANOLISA_TEST_DOCTOR_RUNTIME_OUTPUT", mode)
                    .env("ANOLISA_TEST_DOCTOR_RUNTIME_HEALTHY", healthy.to_string())
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(if healthy { 0 } else { 2 }));
                assert!(output.stderr.is_empty());
                let stdout = String::from_utf8(output.stdout).unwrap();
                let (_, rendered) = stdout.split_once("DOCTOR_OUTPUT_BEGIN\n").unwrap();
                let (rendered, _) = rendered.split_once("DOCTOR_OUTPUT_END\n").unwrap();
                match mode {
                    "json" => {
                        let json: serde_json::Value = serde_json::from_str(rendered).unwrap();
                        assert_eq!(json["command"], "doctor");
                        assert_eq!(json["schema_version"], crate::response::SCHEMA_VERSION);
                        assert_eq!(json["ok"], healthy);
                        assert_eq!(json["data"]["dry_run"], false);
                        assert_eq!(json["data"]["summary"]["failed"], usize::from(!healthy));
                        let component = &json["data"]["components"][0];
                        assert_eq!(component["dependencies"][0]["name"], "libfoo");
                        assert_eq!(component["dependencies"][1]["name"], "node");
                        assert_eq!(component["dependencies"][2]["name"], "kernel-btrfs");
                        if !healthy {
                            assert_eq!(
                                component["fix_plan"][0]["command"],
                                "sudo dnf install libfoo"
                            );
                            assert_eq!(component["findings"][2]["code"], "dependency_unresolvable");
                        }
                    }
                    "human" if healthy => {
                        assert!(
                            rendered.starts_with("Doctor: 1 checked, 1 ok, 0 degraded, 0 failed\n")
                        );
                        assert!(rendered.contains("no issues found"));
                    }
                    "human" => {
                        assert!(
                            rendered.starts_with("Doctor: 1 checked, 0 ok, 0 degraded, 1 failed\n")
                        );
                        let package = rendered.find("[dependency_unresolved]").unwrap();
                        let detail = rendered.find("found 18.0.0, need >=20").unwrap();
                        let platform = rendered.find("[dependency_unresolvable]").unwrap();
                        let fix = rendered.find("Recommended:").unwrap();
                        assert!(package < detail && detail < platform && platform < fix);
                        assert!(rendered.contains("sudo dnf install libfoo"));
                        assert!(rendered.contains("satisfy_platform_requirement"));
                    }
                    "quiet" => assert!(rendered.is_empty()),
                    _ => unreachable!(),
                }
            }
        }
    }

    #[test]
    fn doctor_runtime_output_child() {
        let Ok(mode) = std::env::var("ANOLISA_TEST_DOCTOR_RUNTIME_OUTPUT") else {
            return;
        };
        let healthy = std::env::var("ANOLISA_TEST_DOCTOR_RUNTIME_HEALTHY").unwrap() == "true";
        let sandbox = crate::test_support::TestSandbox::new();
        let ctx = sandbox.context_with(
            crate::context::InstallMode::System,
            crate::test_support::TestContextOptions {
                json: mode == "json",
                quiet: mode == "quiet",
                ..Default::default()
            },
        );
        let runner = RuntimeRunner::new(vec![
            runtime_output(
                Some(if healthy { 0 } else { 1 }),
                if healthy {
                    ""
                } else {
                    "package libfoo is not installed\n"
                },
            ),
            runtime_output(Some(0), if healthy { "v20.0.0" } else { "v18.0.0" }),
        ]);
        let resolver = DependencyResolver::with_probes(&runner, || {
            Ok(if healthy { "btrfs\n" } else { "ext4\n" }.to_string())
        });
        let payload = diagnose_runtime_fixture(
            ctx.layout(),
            Some(owned_object("runtime-tool", LifecycleStatus::Installed)),
            Some(RUNTIME_DEPS),
            false,
            &|deps, env| resolver.resolve(deps, env),
        );
        let has_issues = payload_has_issues(&payload);
        println!("DOCTOR_OUTPUT_BEGIN");
        render_doctor(&ctx, &payload, !has_issues).unwrap();
        println!("DOCTOR_OUTPUT_END");
        let code = if has_issues {
            CliError::DiagnosticsFound {
                command: COMMAND.to_string(),
            }
            .exit_code()
        } else {
            0
        };
        drop(sandbox);
        std::process::exit(code.into());
    }
}
