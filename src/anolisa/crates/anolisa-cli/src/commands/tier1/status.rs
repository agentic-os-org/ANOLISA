//! `anolisa status [COMPONENT]` — read-only view of installed components.
//!
//! Reads `installed.toml` via the shared [`crate::commands::common`] helper
//! and lists every `Component`-kind object, or filters down to a single
//! name. A missing state file is the expected fresh-install case and yields
//! an empty result. When the component index is available, unsupported names
//! are rejected with discovery guidance; without an index, new names cannot
//! be validated and fail explicitly. Exact identities already present in state
//! always win and remain inspectable offline.
//!
//! This handler collects state-on-disk and live probes into a request-scoped
//! [`ComponentSnapshot`] before projecting [`ComponentRecord`]. Synthesized
//! data is limited to read-only rpmdb, integrity, adapter, and manifest health
//! observations. Quarantined
//! legacy records are surfaced as `needs-attention` rows with their exits
//! (`repair`/`forget`) — `status` is the only command that shows them
//! unprompted. Nothing here ever writes state.

use std::path::PathBuf;

use chrono::{SecondsFormat, Utc};
use clap::Parser;
use serde::Serialize;
use thiserror::Error;

#[cfg(test)]
use anolisa_core::SnapshotProbe;
use anolisa_core::adapter::claim::ClaimStatus;
use anolisa_core::adapter::manager::{AdapterSourceStatus, ScanEntry};
#[cfg(test)]
use anolisa_core::domain::NativePm;
use anolisa_core::domain::{Installation, InstallationScope, ProviderBinding};
use anolisa_core::state::ObjectKind;
use anolisa_core::state_migration::QuarantineReason;
#[cfg(test)]
use anolisa_core::state_store::StateStore;
use anolisa_core::{
    AdapterObservation, AdapterProvenance, AdapterSourceSnapshot, CheckEnv, CheckOutcome,
    CheckStatus, ComponentManifest, ComponentSnapshot, HealthEntry, IntegrityStatus,
    ManifestHealthProvenance, ManifestHealthSnapshot, NativePackageProvenance,
    NativePackageSnapshot, OwnedFilesProvenance, ProbeEvidence, ServiceManager, ServiceProbes,
    SnapshotContractError, StateRootScope, StateSnapshot, run_check,
    service_for_install_mode as service_factory, user_service_for_install_mode,
};
use anolisa_env::EnvService;
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::rpm_query::RpmPackageQuery;

use crate::color::{Palette, pad_right};
use crate::commands::common;
use crate::commands::common::RepoPersistPolicy;
#[cfg(test)]
use crate::commands::state_view::StateScope;
use crate::commands::state_view::{StateView, StateVisibility};
use crate::commands::telemetry::status_command_for_service_target;
use crate::commands::tier1::component_observation::{
    self, AggregateSelection, ComponentObservationError, RpmDrift, drift_probe_identity,
    rpm_drift_from_evidence,
};
use crate::commands::tier1::install::rpm_package_candidates_with_index;
use crate::context::{CliContext, InstallMode};
use crate::repo_config::BackendConfig;
use crate::resolution::{
    ComponentIndex, IndexIdentity, load_optional_component_index, resolve_index_identity,
};
use crate::response::{CliError, render_json};

const COMMAND: &str = "status";

#[derive(Parser)]
pub struct StatusArgs {
    /// Show detail for a specific ANOLISA component (omit for aggregate view).
    ///
    /// Run `anolisa list` to view supported component names.
    ///
    /// Use `anolisa telemetry status` for telemetry service state.
    pub component: Option<String>,
}

/// Summary of one adapter associated with a component, derived from
/// `AdapterManager::scan()`. Included in the component status record
/// when adapter declarations/resources/receipts exist.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AdapterSummaryRecord {
    pub(crate) component: String,
    pub(crate) framework: String,
    pub(crate) declared: bool,
    pub(crate) resource_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resource_root: Option<String>,
    pub(crate) driver_available: bool,
    pub(crate) framework_detected: bool,
    pub(crate) enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) claim_status: Option<ClaimStatus>,
    /// Source health for enabled adapter receipts. Present only for receipt
    /// rows so JSON consumers can identify orphaned user receipts separately
    /// from adapters that are simply not enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_status: Option<String>,
    /// Explanation for a missing adapter source, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_reason: Option<String>,
}

/// JSON-shaped record for a single component, used in both the wire
/// envelope and the human renderer. Fields are projected straight from the
/// matching [`Installation`] on disk;
/// optional/empty fields are skipped when absent so synthetic
/// `not_installed` records stay compact.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ComponentRecord {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) scope: String,
    pub(crate) active: bool,
    pub(crate) mutable_by_current_invocation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shadowed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) installed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_operation_id: Option<String>,
    /// Feature flags the install record marks as enabled.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) enabled_features: Vec<String>,
    /// Last-known health probe entries persisted in state. Empty until a
    /// background probe wires up — but still surfaced verbatim today so
    /// users see whatever the install runner recorded.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) health: Vec<HealthEntry>,
    /// Associated adapter summaries from `AdapterManager::scan()`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) adapters: Vec<AdapterSummaryRecord>,
    /// RPM package name, set only for `observed` rows (rpmdb hit, no state)
    /// and adopted `rpm-observed` rows (§8). Absent for raw components.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rpm_package: Option<String>,
    /// Full EVR of the RPM, paired with [`rpm_package`](Self::rpm_package).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rpm_evr: Option<String>,
    /// Source repo/label that supplied the RPM (e.g. `@System`); `None` when
    /// it could not be determined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rpm_source_repo: Option<String>,
    /// System packages auto-installed by the provisioner during component
    /// install. Empty when no packages were provisioned.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) provisioned_packages: Vec<String>,
}

/// JSON-shaped record for a quarantined legacy record: migration preserved
/// it verbatim because provenance could not be established, so no intent
/// except `repair`/`forget` will touch it. Surfaced by `status` as
/// `needs-attention` — the record is invisible to every other command, and
/// this is where the user finds out it exists.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct QuarantinedRecord {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) scope: String,
    /// Always `needs-attention`.
    pub(crate) status: &'static str,
    pub(crate) version: String,
    /// Why migration refused to classify the record.
    pub(crate) reason: String,
    /// Suggested exit: `repair` rebuilds from system reality, `forget`
    /// drops the record.
    pub(crate) hint: String,
}

#[derive(Serialize)]
struct StatusPayload {
    components: Vec<ComponentRecord>,
    /// Quarantined legacy records needing user action; absent when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    quarantined: Vec<QuarantinedRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[cfg(test)]
pub(crate) type AggregateRecordSelection = AggregateSelection;

/// Internal failure while constructing or projecting a status snapshot.
#[derive(Debug, Error)]
pub(crate) enum StatusSnapshotError {
    /// Collected evidence violated the shared snapshot contract.
    #[error("component snapshot contract failed for {component:?}: {source}")]
    Contract {
        /// Component whose request and evidence disagreed.
        component: String,
        /// Underlying request/evidence contract violation.
        #[source]
        source: SnapshotContractError,
    },
    /// A status projection received state evidence it cannot render.
    #[error("component snapshot for {component:?} does not contain active state")]
    MissingActiveState { component: String },
}

impl From<ComponentObservationError> for StatusSnapshotError {
    fn from(error: ComponentObservationError) -> Self {
        match error {
            ComponentObservationError::Contract { component, source } => {
                Self::Contract { component, source }
            }
        }
    }
}

enum AdapterScanEvidence<'a> {
    #[cfg(test)]
    NotRequested,
    Available(&'a [ScanEntry]),
    Unavailable(String),
}

pub fn handle(args: StatusArgs, ctx: &CliContext) -> Result<(), CliError> {
    let mut view = StateView::load(ctx, COMMAND, StateVisibility::UserPlusSystem)?;
    component_observation::normalize_view_states(&mut view);
    let layout = common::resolve_layout(ctx);
    let adapter_scan = common::build_adapter_manager(ctx).scan();

    let query = RpmPackageQuery::system();
    // A named query uses the component index both to validate/resolve its
    // identity and, in system mode, to map the observed RPM probe below.
    // Loading remains best-effort so installed state is inspectable offline.
    let repo_config = args
        .component
        .is_some()
        .then(|| {
            common::load_repo_config(ctx, &layout, COMMAND, RepoPersistPolicy::BestEffort).ok()
        })
        .flatten();
    let rpm_backend = repo_config.as_ref().and_then(|c| c.backends.get("rpm"));
    let env = EnvService::detect();
    let component_index = repo_config
        .as_ref()
        .and_then(|cfg| load_optional_component_index(&layout, &env, cfg));
    let selected_component = args
        .component
        .as_deref()
        .map(|target| resolve_component_target(target, &view, component_index.as_ref()))
        .transpose()?;

    let system_scope_service = service_factory(InstallMode::System.as_str(), &env);
    let current_system_service = service_factory(ctx.install_mode.as_str(), &env);
    let user_service = user_service_for_install_mode(ctx.install_mode.as_str(), &env);
    let service_backends = ServiceProbeBackends {
        system_scope: system_scope_service.as_ref(),
        current_system: current_system_service.as_ref(),
        user: user_service.as_ref(),
    };

    let adapter_evidence = match &adapter_scan {
        Ok(report) => AdapterScanEvidence::Available(&report.entries),
        Err(error) => AdapterScanEvidence::Unavailable(error.to_string()),
    };
    let mut records = select_components_from_view_with_adapter_evidence(
        &view,
        selected_component.as_deref(),
        AggregateSelection::AllVisible,
        adapter_evidence,
        Some(&service_backends),
        Some(&query),
    )
    .map_err(status_snapshot_cli_error)?;

    // Read-only Observed report (§8): when a named component is absent from
    // ANOLISA state but present in rpmdb (system mode), upgrade the synthetic
    // `not_installed` row to `observed` with the package/EVR/repo. This never
    // writes state — adopting is `install`'s job.
    if let Some(target) = selected_component.as_deref()
        && ctx.install_mode == InstallMode::System
        && records.len() == 1
        && records[0].status == "not_installed"
        && let Some(observed) =
            observed_record(target, rpm_backend, component_index.as_ref(), &query)
    {
        records = vec![observed];
    }

    let quarantined = select_quarantined_from_view(&view, selected_component.as_deref());

    if ctx.json {
        return render_json(
            COMMAND,
            StatusPayload {
                components: records,
                quarantined,
                warnings: view.warnings,
            },
        );
    }

    if !ctx.quiet {
        render_warnings(&view.warnings);
        render_human(&records, ctx.verbose, ctx.no_color);
        render_quarantined(&quarantined, ctx.no_color);
    }
    Ok(())
}

fn status_snapshot_cli_error(error: StatusSnapshotError) -> CliError {
    CliError::Runtime {
        command: COMMAND.to_string(),
        reason: format!("failed to assemble component status snapshot: {error}"),
    }
}

fn resolve_component_target(
    input: &str,
    view: &StateView,
    component_index: Option<&ComponentIndex>,
) -> Result<String, CliError> {
    if view.has_exact_component(input) {
        return Ok(input.to_string());
    }

    // Exhaustive on the verdict so a future `IndexIdentity` variant is a
    // compile error here instead of silently collapsing into one refusal.
    match resolve_index_identity(input, component_index) {
        IndexIdentity::Resolved(component) => Ok(component),
        IndexIdentity::Unsupported => {
            reject_telemetry_service_target(input)?;
            Err(common::unsupported_component_error(COMMAND, input))
        }
        IndexIdentity::Unavailable => {
            reject_telemetry_service_target(input)?;
            Err(common::component_index_unavailable_error(COMMAND, input))
        }
    }
}

/// A telemetry service name is a management target, not a component; the
/// redirect outranks both generic refusals so the guidance stays useful
/// whether or not an index could be loaded.
fn reject_telemetry_service_target(input: &str) -> Result<(), CliError> {
    match status_command_for_service_target(input) {
        Some(command) => Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "unsupported component '{input}'; run `{command}` to inspect telemetry state"
            ),
        }),
        None => Ok(()),
    }
}

/// Project the quarantined records of every visible root, optionally
/// filtered to one name. Read-only, like everything else in `status`:
/// the records stay on disk untouched until `repair` or `forget`.
fn select_quarantined_from_view(
    view: &StateView,
    selected: Option<&str>,
) -> Vec<QuarantinedRecord> {
    view.visible_roots
        .iter()
        .flat_map(|root| {
            root.state
                .quarantined
                .iter()
                .filter(|q| selected.is_none_or(|name| q.record.name == name))
                .map(|q| {
                    let scope = root.scope.label();
                    let repair = scoped_component_command(scope, "repair", &q.record.name);
                    let forget = scoped_component_command(scope, "forget", &q.record.name);
                    QuarantinedRecord {
                        name: q.record.name.clone(),
                        kind: kind_label(q.record.kind).to_string(),
                        scope: scope.to_string(),
                        status: "needs-attention",
                        version: q.record.version.clone(),
                        reason: quarantine_reason_text(&q.reason),
                        hint: format!(
                            "run `{repair}` to rebuild the record from system reality, or `{forget}` to drop it"
                        ),
                    }
                })
        })
        .collect()
}

fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Component => "component",
        ObjectKind::Adapter => "adapter",
        ObjectKind::Osbase => "osbase",
        ObjectKind::Capability => "capability",
    }
}

fn quarantine_reason_text(reason: &QuarantineReason) -> String {
    match reason {
        QuarantineReason::UnknownBackend { backend } => format!(
            "its record names install backend '{backend}', which this version cannot interpret"
        ),
        QuarantineReason::NoEvidence => {
            "its record carries no provenance evidence (no ownership, backend, source, or file list)"
                .to_string()
        }
    }
}

/// Scope-routed service managers for the manifest health probe, built once
/// per invocation. Mirrors doctor's routing: system units on a system-scope
/// record probe through the real system manager regardless of invocation
/// mode; user units probe through the invocation's user manager (only a
/// user-mode invocation carries the caller's session bus).
pub(crate) struct ServiceProbeBackends<'a> {
    /// Answers system-scope units on system-scope records.
    pub(crate) system_scope: &'a dyn ServiceManager,
    /// Answers system-scope units on user-scope records: the invocation-mode
    /// factory, a real manager only in a system-mode invocation.
    pub(crate) current_system: &'a dyn ServiceManager,
    /// Answers user-scope units (`systemctl --user`).
    pub(crate) user: &'a dyn ServiceManager,
}

impl ServiceProbeBackends<'_> {
    /// System-unit manager for a record living in `scope` ("system"/"user").
    fn system_for_record_scope(&self, scope: &str) -> &dyn ServiceManager {
        if scope == "system" {
            self.system_scope
        } else {
            self.current_system
        }
    }
}

/// Pure selector: project the [`StateStore`] down to component records,
/// optionally filtered to a single name. Extracted so tests can exercise
/// the filtering/synthetic-not-installed logic without mocking
/// `CliContext` or touching the filesystem. Service probes answer
/// unsupported here — routing is exercised through the snapshot collector
/// with injected backends.
#[cfg(test)]
pub(crate) fn select_components(
    state: &StateStore,
    layout: &FsLayout,
    install_mode: &str,
    name: Option<&str>,
    adapter_scan: Option<&[ScanEntry]>,
) -> Vec<ComponentRecord> {
    use anolisa_core::NotSupportedServiceManager;

    let quiet = NotSupportedServiceManager::new("service probes disabled in this selector".into());
    let backends = ServiceProbeBackends {
        system_scope: &quiet,
        current_system: &quiet,
        user: &quiet,
    };
    let scope = if install_mode == "system" {
        StateScope::System
    } else {
        StateScope::User
    };
    let mut state = state.clone();
    for installation in &mut state.installations {
        installation.scope = match scope {
            StateScope::System => InstallationScope::System,
            StateScope::User => InstallationScope::User { uid: 1000 },
        };
    }
    let root = crate::commands::state_view::ScopedStateRoot {
        scope,
        layout: layout.clone(),
        state_path: layout.state_dir.join("installed.toml"),
        writable: true,
        state,
    };
    let view = StateView {
        writable: root.clone(),
        visible_roots: vec![root],
        unavailable_roots: Vec::new(),
        warnings: Vec::new(),
    };
    select_components_from_view(
        &view,
        name,
        AggregateRecordSelection::AllVisible,
        adapter_scan,
        Some(&backends),
        None,
    )
    .expect("test snapshot evidence must satisfy its request")
}

/// `manifest_probe` lets selector tests choose whether the status projection
/// executes each record's snapshot-declared health check.
#[cfg(test)]
pub(crate) fn select_components_from_view(
    view: &StateView,
    name: Option<&str>,
    aggregate_selection: AggregateRecordSelection,
    adapter_scan: Option<&[ScanEntry]>,
    manifest_probe: Option<&ServiceProbeBackends<'_>>,
    rpm_query: Option<&dyn PackageQuery>,
) -> Result<Vec<ComponentRecord>, StatusSnapshotError> {
    let adapter_evidence = adapter_scan.map_or(
        AdapterScanEvidence::NotRequested,
        AdapterScanEvidence::Available,
    );
    select_components_from_view_with_adapter_evidence(
        view,
        name,
        aggregate_selection,
        adapter_evidence,
        manifest_probe,
        rpm_query,
    )
}

fn select_components_from_view_with_adapter_evidence(
    view: &StateView,
    name: Option<&str>,
    aggregate_selection: AggregateSelection,
    adapter_scan: AdapterScanEvidence<'_>,
    manifest_probe: Option<&ServiceProbeBackends<'_>>,
    rpm_query: Option<&dyn PackageQuery>,
) -> Result<Vec<ComponentRecord>, StatusSnapshotError> {
    let selected =
        component_observation::select_visible_components(view, name, aggregate_selection);

    let Some(target_records) = (!selected.is_empty()).then_some(selected) else {
        return Ok(name
            .map(|target| vec![not_installed_record(target)])
            .unwrap_or_default());
    };

    let checked_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let adapter_state_paths = view
        .visible_roots
        .iter()
        .map(|root| root.state_path.clone())
        .collect::<Vec<_>>();
    target_records
        .into_iter()
        .map(|record| {
            let snapshot = collect_status_snapshot(
                &record,
                &adapter_scan,
                &adapter_state_paths,
                manifest_probe,
                rpm_query,
                &checked_at,
            );
            snapshot.and_then(|snapshot| record_from_snapshot(&snapshot, &checked_at))
        })
        .collect()
}

fn collect_status_snapshot(
    record: &crate::commands::state_view::ScopedInstalledObject<'_>,
    adapter_scan: &AdapterScanEvidence<'_>,
    adapter_state_paths: &[PathBuf],
    manifest_probe: Option<&ServiceProbeBackends<'_>>,
    rpm_query: Option<&dyn PackageQuery>,
    observed_at: &str,
) -> Result<ComponentSnapshot, StatusSnapshotError> {
    let manifest_health = collect_manifest_health(
        &record.root.layout,
        record.scope().label(),
        manifest_probe,
        record.object,
    );
    let owned_files = component_observation::owned_files_evidence(record);
    let pre_native_status = projected_health_status(
        common::installation_status_str(record.object),
        &owned_files,
        &manifest_health,
    );
    let native_package =
        collect_native_package(record.object, rpm_query, &pre_native_status, observed_at);
    let adapters =
        collect_adapter_observations(&record.object.name, adapter_scan, adapter_state_paths);

    component_observation::snapshot_from_record(
        record,
        owned_files,
        native_package,
        manifest_health,
        adapters,
        ProbeEvidence::NotRequested,
    )
    .map_err(StatusSnapshotError::from)
}

#[cfg(test)]
/// Collects the production status snapshot with adapter probes disabled.
pub(super) fn snapshot_for_conformance(
    record: &crate::commands::state_view::ScopedInstalledObject<'_>,
    rpm_query: Option<&dyn PackageQuery>,
    observed_at: &str,
) -> Result<ComponentSnapshot, StatusSnapshotError> {
    collect_status_snapshot(
        record,
        &AdapterScanEvidence::NotRequested,
        &[],
        None,
        rpm_query,
        observed_at,
    )
}

#[cfg(test)]
/// Projects a snapshot through the production status record path.
pub(super) fn projection_for_conformance(
    snapshot: &ComponentSnapshot,
    checked_at: &str,
) -> Result<ComponentRecord, StatusSnapshotError> {
    record_from_snapshot(snapshot, checked_at)
}

fn collect_native_package(
    installation: &Installation,
    query: Option<&dyn PackageQuery>,
    status: &str,
    observed_at: &str,
) -> ProbeEvidence<NativePackageSnapshot, NativePackageProvenance> {
    if !matches!(status, "installed" | "adopted" | "observed")
        || !matches!(installation.scope, InstallationScope::System)
    {
        return ProbeEvidence::NotRequested;
    }
    let ProviderBinding::Delegated { pm, package, .. } = &installation.binding else {
        return ProbeEvidence::NotRequested;
    };
    let Some(package) = package.resolved_name() else {
        return ProbeEvidence::NotRequested;
    };
    let Some(query) = query else {
        return ProbeEvidence::NotRequested;
    };

    component_observation::native_package_evidence(*pm, package, query, observed_at)
}

fn collect_manifest_health(
    layout: &FsLayout,
    install_mode: &str,
    service_backends: Option<&ServiceProbeBackends<'_>>,
    component: &Installation,
) -> ProbeEvidence<ManifestHealthSnapshot, ManifestHealthProvenance> {
    let Some(service_backends) = service_backends else {
        return ProbeEvidence::NotRequested;
    };
    if component.binding.is_delegated() {
        return ProbeEvidence::NotRequested;
    }

    let path = match common::installed_component_manifest_path(layout, &component.name, COMMAND) {
        Ok(path) => path,
        Err(_) => {
            return ProbeEvidence::Absent {
                provenance: ManifestHealthProvenance {
                    path: layout.state_dir.join("component-manifests"),
                },
            };
        }
    };
    let provenance = ManifestHealthProvenance { path: path.clone() };
    if !path.is_file() {
        return ProbeEvidence::Absent { provenance };
    }
    let manifest = match ComponentManifest::from_file(&path) {
        Ok(manifest) => manifest,
        Err(error) => {
            return ProbeEvidence::Unavailable {
                provenance,
                reason: format!("failed to parse installed manifest snapshot: {error}"),
            };
        }
    };
    let Some(spec) = manifest.health_spec() else {
        return ProbeEvidence::Absent { provenance };
    };

    let outcome = run_check(
        &spec,
        &CheckEnv {
            layout,
            dry_run: false,
            service_probes: Some(ServiceProbes {
                system: service_backends.system_for_record_scope(install_mode),
                user: service_backends.user,
                declared: &manifest.install.services,
            }),
        },
    );
    ProbeEvidence::Present {
        provenance,
        value: ManifestHealthSnapshot { outcome },
    }
}

fn collect_adapter_observations(
    component: &str,
    scan: &AdapterScanEvidence<'_>,
    state_paths: &[PathBuf],
) -> ProbeEvidence<Vec<AdapterObservation>, AdapterProvenance> {
    let provenance = AdapterProvenance {
        state_paths: state_paths.to_vec(),
    };
    match scan {
        #[cfg(test)]
        AdapterScanEvidence::NotRequested => ProbeEvidence::NotRequested,
        AdapterScanEvidence::Unavailable(reason) => ProbeEvidence::Unavailable {
            provenance,
            reason: reason.clone(),
        },
        AdapterScanEvidence::Available(entries) => {
            let observations = entries
                .iter()
                .filter(|entry| entry.component == component)
                .map(adapter_observation_from_scan)
                .collect::<Vec<_>>();
            if observations.is_empty() {
                ProbeEvidence::Absent { provenance }
            } else {
                ProbeEvidence::Present {
                    provenance,
                    value: observations,
                }
            }
        }
    }
}

fn adapter_observation_from_scan(entry: &ScanEntry) -> AdapterObservation {
    AdapterObservation {
        component: entry.component.clone(),
        framework: entry.framework.clone(),
        declared: entry.declared,
        resource_root: entry.resource_root.clone(),
        driver_available: entry.driver_available,
        framework_detected: entry.framework_detected,
        adapter_type: entry.adapter_type.clone(),
        enabled: entry.enabled,
        claim_status: entry.claim_status,
        source_status: entry.source_status.map(|status| match status {
            AdapterSourceStatus::Available => AdapterSourceSnapshot::Available,
            AdapterSourceStatus::Missing => AdapterSourceSnapshot::Missing,
        }),
        source_reason: entry.source_reason.clone(),
    }
}

fn not_installed_record(name: &str) -> ComponentRecord {
    ComponentRecord {
        name: name.to_string(),
        status: "not_installed".to_string(),
        scope: "none".to_string(),
        active: false,
        mutable_by_current_invocation: false,
        shadowed_by: None,
        state_path: None,
        version: None,
        installed_at: None,
        last_operation_id: None,
        enabled_features: Vec::new(),
        health: Vec::new(),
        adapters: Vec::new(),
        rpm_package: None,
        rpm_evr: None,
        rpm_source_repo: None,
        provisioned_packages: Vec::new(),
    }
}

#[cfg(test)]
fn record_from_object(
    layout: &FsLayout,
    install_mode: &str,
    manifest_probe: Option<&ServiceProbeBackends<'_>>,
    installation: &Installation,
) -> ComponentRecord {
    let scope = if install_mode == "system" {
        StateScope::System
    } else {
        StateScope::User
    };
    let mut state = StateStore::empty();
    let mut installation = installation.clone();
    installation.scope = match scope {
        StateScope::System => InstallationScope::System,
        StateScope::User => InstallationScope::User { uid: 1000 },
    };
    state.upsert(installation.clone());
    let root = crate::commands::state_view::ScopedStateRoot {
        scope,
        layout: layout.clone(),
        state_path: layout.state_dir.join("installed.toml"),
        writable: true,
        state,
    };
    let view = StateView {
        writable: root.clone(),
        visible_roots: vec![root],
        unavailable_roots: Vec::new(),
        warnings: Vec::new(),
    };
    select_components_from_view(
        &view,
        Some(&installation.name),
        AggregateRecordSelection::AllVisible,
        None,
        manifest_probe,
        None,
    )
    .expect("test snapshot evidence must satisfy its request")
    .remove(0)
}

/// Probe rpmdb for `component` and, if a matching system RPM is installed,
/// build an `observed` record (§8). Returns `None` when nothing is installed
/// or the host has no rpm tooling — the caller keeps the `not_installed` row.
///
/// Read-only and best-effort: a single hard query failure on a candidate
/// stops the probe (returns `None`) rather than failing `status`. `query` is
/// injected so tests can drive this without a live rpmdb.
fn observed_record(
    component: &str,
    rpm_backend: Option<&BackendConfig>,
    component_index: Option<&ComponentIndex>,
    query: &dyn PackageQuery,
) -> Option<ComponentRecord> {
    // Same resolver as adopt (§5), minus the CLI `--package` override (status
    // takes no such flag).
    let candidates =
        rpm_package_candidates_with_index(None, rpm_backend, component_index, query, component)
            .ok()?;
    for target in candidates {
        let Ok(Some(info)) = query.query_installed(&target.package) else {
            // Not this candidate (absent), or a hard error / multi-version
            // drift: status does not adjudicate drift, so move on.
            continue;
        };
        let evr = info.version.to_string();
        let source_repo = query.installed_origin(&target.package).ok().flatten();
        return Some(ComponentRecord {
            name: target.component,
            status: "observed".to_string(),
            scope: "system".to_string(),
            active: false,
            mutable_by_current_invocation: false,
            shadowed_by: None,
            state_path: None,
            version: Some(evr.clone()),
            installed_at: None,
            last_operation_id: None,
            enabled_features: Vec::new(),
            health: Vec::new(),
            adapters: Vec::new(),
            rpm_package: Some(info.name),
            rpm_evr: Some(evr),
            rpm_source_repo: source_repo,
            provisioned_packages: Vec::new(),
        });
    }
    None
}

/// Compare an RPM component's recorded EVR against live rpmdb reality.
///
/// `status` is read-only and best-effort: an unrunnable or anomalous query
/// (`rpm`/`dnf` missing, spawn/permission/parse failure) yields `None` rather
/// than crying drift on a read we cannot trust. A same-name multi-version rpmdb
/// is a genuine divergence from the single recorded version, so it classifies
/// as drift. `query` is injected so tests drive this without a live rpmdb.
#[cfg(test)]
pub(crate) fn probe_rpm_drift(
    package: &str,
    recorded_evr: Option<&str>,
    query: &dyn PackageQuery,
) -> Option<RpmDrift> {
    let evidence =
        component_observation::native_package_evidence(NativePm::Rpm, package, query, "");
    rpm_drift_from_evidence(&evidence, recorded_evr)
}

fn apply_native_package_to_record(
    record: &mut ComponentRecord,
    installation: &Installation,
    evidence: &ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
    checked_at: &str,
) {
    // Only adjudicate drift on a clean live projection; never demote a
    // failed, degraded, or disabled record.
    if !matches!(record.status.as_str(), "installed" | "adopted" | "observed") {
        return;
    }
    let Some((package, recorded_evr)) = drift_probe_identity(installation) else {
        return;
    };
    let (status, reason) = match rpm_drift_from_evidence(evidence, recorded_evr) {
        Some(RpmDrift::Drifted { reason }) => ("drifted", reason),
        Some(RpmDrift::Missing) => (
            "missing",
            format!("package {package} recorded in ANOLISA state is no longer present in rpmdb"),
        ),
        None => return,
    };
    record.status = status.to_string();
    record.health.push(HealthEntry {
        name: "rpm:drift".to_string(),
        status: status.to_string(),
        checked_at: checked_at.to_string(),
        reason: Some(reason),
    });
}

fn record_from_snapshot(
    snapshot: &ComponentSnapshot,
    checked_at: &str,
) -> Result<ComponentRecord, StatusSnapshotError> {
    let ProbeEvidence::Present {
        provenance: state_provenance,
        value: StateSnapshot::Active(installation),
    } = snapshot.state()
    else {
        return Err(StatusSnapshotError::MissingActiveState {
            component: snapshot.request().component().to_string(),
        });
    };
    let mut health = installation.health.clone();
    append_owned_file_health(snapshot.owned_files(), checked_at, &mut health);
    append_manifest_health(
        &installation.name,
        snapshot.manifest_health(),
        checked_at,
        &mut health,
    );
    let status = projected_health_status(
        common::installation_status_str(installation),
        snapshot.owned_files(),
        snapshot.manifest_health(),
    );
    // Surface RPM provenance for delegated rows so human/JSON output shows
    // the package/EVR/repo behind the record.
    let (rpm_package, rpm_evr, rpm_source_repo) = match &installation.binding {
        ProviderBinding::Delegated {
            package,
            last_observed,
            ..
        } => (
            package.resolved_name().map(str::to_string),
            last_observed.as_ref().and_then(|o| o.evr.clone()),
            last_observed.as_ref().and_then(|o| o.source_repo.clone()),
        ),
        ProviderBinding::Owned { .. } => (None, None, None),
    };

    let visibility = snapshot.state_visibility();
    let mut record = ComponentRecord {
        name: installation.name.clone(),
        status,
        scope: visibility.map_or_else(
            || installation_scope_label(snapshot.request().scope()).to_string(),
            |metadata| state_root_scope_label(metadata.root_scope).to_string(),
        ),
        active: visibility.is_none_or(|metadata| metadata.active),
        mutable_by_current_invocation: visibility
            .is_some_and(|metadata| metadata.mutable_by_current_invocation),
        shadowed_by: visibility
            .and_then(|metadata| metadata.shadowed_by)
            .map(state_root_scope_label)
            .map(str::to_string),
        state_path: Some(state_provenance.path.display().to_string()),
        version: component_observation::record_version(installation),
        installed_at: Some(installation.installed_at.clone()),
        last_operation_id: installation.last_operation_id.clone(),
        enabled_features: installation.enabled_features.clone(),
        health,
        adapters: adapter_summaries_from_snapshot(snapshot.adapters()),
        rpm_package,
        rpm_evr,
        rpm_source_repo,
        provisioned_packages: match &installation.binding {
            ProviderBinding::Owned { artifact } => artifact.provisioned_packages.clone(),
            ProviderBinding::Delegated { .. } => Vec::new(),
        },
    };
    apply_native_package_to_record(
        &mut record,
        installation,
        snapshot.native_package(),
        checked_at,
    );
    Ok(record)
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

fn adapter_summaries_from_snapshot(
    evidence: &ProbeEvidence<Vec<AdapterObservation>, AdapterProvenance>,
) -> Vec<AdapterSummaryRecord> {
    let ProbeEvidence::Present { value, .. } = evidence else {
        return Vec::new();
    };
    value
        .iter()
        .map(|observation| AdapterSummaryRecord {
            component: observation.component.clone(),
            framework: observation.framework.clone(),
            declared: observation.declared,
            resource_present: observation.resource_root.is_some(),
            resource_root: observation
                .resource_root
                .as_ref()
                .map(|path| path.display().to_string()),
            driver_available: observation.driver_available,
            framework_detected: observation.framework_detected,
            enabled: observation.enabled,
            claim_status: observation.claim_status,
            source_status: observation.source_status.map(|status| match status {
                AdapterSourceSnapshot::Available => "available".to_string(),
                AdapterSourceSnapshot::Missing => "missing".to_string(),
            }),
            source_reason: observation.source_reason.clone(),
        })
        .collect()
}

/// Project typed owned-file observations into the existing health-entry wire.
fn append_owned_file_health(
    evidence: &ProbeEvidence<anolisa_core::OwnedFilesSnapshot, OwnedFilesProvenance>,
    checked_at: &str,
    entries: &mut Vec<HealthEntry>,
) {
    let ProbeEvidence::Present { value, .. } = evidence else {
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
        entries.push(HealthEntry {
            name: format!("integrity:{}", observation.path.display()),
            status: observation.status.label().to_string(),
            checked_at: checked_at.to_string(),
            reason,
        });
    }
}

/// Apply integrity and manifest-health evidence without downgrading an
/// already disabled, degraded, or failed lifecycle status.
fn projected_health_status(
    base_status: &str,
    owned_files: &ProbeEvidence<anolisa_core::OwnedFilesSnapshot, OwnedFilesProvenance>,
    manifest_health: &ProbeEvidence<ManifestHealthSnapshot, ManifestHealthProvenance>,
) -> String {
    let mut had_failure = false;
    let mut had_unverified = false;
    if let ProbeEvidence::Present { value, .. } = owned_files {
        for observation in &value.files {
            if observation.status.is_failure() {
                had_failure = true;
            } else if matches!(
                observation.status,
                IntegrityStatus::Unverified | IntegrityStatus::ProbeLimitExceeded { .. }
            ) {
                had_unverified = true;
            }
        }
    }
    let integrity_status = match base_status {
        "installed" | "adopted" | "observed" if had_failure => "failed".to_string(),
        "installed" | "adopted" | "observed" if had_unverified => "degraded".to_string(),
        _ => base_status.to_string(),
    };

    let broken = match manifest_health {
        ProbeEvidence::Unavailable { .. } => Some("degraded"),
        ProbeEvidence::Present { value, .. } => match value.outcome.status {
            CheckStatus::Failed => Some("failed"),
            CheckStatus::Unsupported => Some("degraded"),
            CheckStatus::Ok | CheckStatus::Skipped => None,
        },
        ProbeEvidence::NotRequested | ProbeEvidence::Absent { .. } => None,
    };
    match integrity_status.as_str() {
        "installed" | "adopted" | "observed" => broken.unwrap_or(&integrity_status).to_string(),
        _ => integrity_status,
    }
}

/// Project typed manifest-health evidence into the existing health-entry wire.
fn append_manifest_health(
    component: &str,
    evidence: &ProbeEvidence<ManifestHealthSnapshot, ManifestHealthProvenance>,
    checked_at: &str,
    entries: &mut Vec<HealthEntry>,
) {
    match evidence {
        ProbeEvidence::Unavailable { reason, .. } => entries.push(HealthEntry {
            name: format!("{component}:manifest_snapshot"),
            status: "unreadable".to_string(),
            checked_at: checked_at.to_string(),
            reason: Some(reason.clone()),
        }),
        ProbeEvidence::Present { value, .. } => {
            push_check_entries(component, &value.outcome, checked_at, entries);
        }
        ProbeEvidence::NotRequested | ProbeEvidence::Absent { .. } => {}
    }
}

/// Flatten a check-outcome tree into wire health entries: aggregates keep
/// their summary row, leaves carry the probe detail.
fn push_check_entries(
    component: &str,
    outcome: &CheckOutcome,
    checked_at: &str,
    entries: &mut Vec<HealthEntry>,
) {
    entries.push(HealthEntry {
        name: format!("{component}:{}", outcome.spec_label),
        status: outcome.status.as_str().to_string(),
        checked_at: checked_at.to_string(),
        reason: outcome.detail.clone(),
    });
    for child in &outcome.children {
        push_check_entries(component, child, checked_at, entries);
    }
}

fn render_human(records: &[ComponentRecord], verbose: bool, no_color: bool) {
    let color = Palette::new(no_color);
    if records.is_empty() {
        println!("{}", color.muted("no installed components"));
        return;
    }

    println!(
        "{}",
        color.header(format!(
            "{:<28}  {:<8}  {:<14}  {:<10}  {}",
            "NAME", "SCOPE", "STATUS", "VERSION", "INSTALLED_AT"
        ))
    );
    for record in records {
        let version = record.version.as_deref().unwrap_or("-");
        let installed_at = record.installed_at.as_deref().unwrap_or("-");
        println!(
            "{name:<28}  {scope:<8}  {status:<14}  {version:<10}  {installed_at}",
            name = record.name,
            scope = record.scope,
            status = color.status(pad_right(&record.status, 14)),
            version = version,
            installed_at = color.muted(installed_at),
        );
        for (label, value) in scope_metadata_pairs(record) {
            println!("    {} {}", color.label(format!("{label}:")), value);
        }
        // RPM provenance for observed / adopted rpm-observed rows (§8).
        if let Some(pkg) = record.rpm_package.as_deref() {
            let repo = record.rpm_source_repo.as_deref().unwrap_or("unknown repo");
            let evr = record.rpm_evr.as_deref().unwrap_or("-");
            println!("    {} {pkg} ({evr}, {repo})", color.label("rpm package:"),);
        }
        // Observed = present in rpmdb but not yet tracked; point at adopt.
        if record.status == "observed" {
            let adopt = scoped_component_command(&record.scope, "adopt", &record.name);
            println!(
                "    {} run '{adopt}' to record the system RPM as adopted",
                color.label("hint:"),
            );
        }
        // Drift / missing point at the repair / forget remediation (#960).
        if record.status == "drifted" {
            let repair = scoped_component_command(&record.scope, "repair", &record.name);
            println!(
                "    {} run '{repair}' to refresh ANOLISA state from rpmdb",
                color.label("hint:"),
            );
        } else if record.status == "missing" {
            let repair = scoped_component_command(&record.scope, "repair", &record.name);
            let forget = scoped_component_command(&record.scope, "forget", &record.name);
            println!(
                "    {} run '{repair}' to reconcile the missing installation, or '{forget}' to drop its stale record",
                color.label("hint:"),
            );
        }
        if verbose {
            if let Some(op) = record.last_operation_id.as_deref() {
                println!("    {} {}", color.label("last_operation_id:"), color.id(op));
            }
            if !record.enabled_features.is_empty() {
                println!(
                    "    {} {}",
                    color.label("enabled_features:"),
                    record.enabled_features.join(", ")
                );
            }
            if !record.provisioned_packages.is_empty() {
                println!(
                    "    {} {}",
                    color.label("provisioned_packages:"),
                    record.provisioned_packages.join(", ")
                );
            }
            for entry in &record.health {
                println!(
                    "    {} {} @ {}",
                    color.label(format!("health[{}]:", entry.name)),
                    color.status(&entry.status),
                    color.muted(&entry.checked_at)
                );
            }
        }
        if !record.adapters.is_empty() {
            println!("    {}", color.label("Associated Adapters:"));
            for adapter in &record.adapters {
                println!("      {}/{}", adapter.component, adapter.framework);
                println!(
                    "        {} {}",
                    color.label("Resource:"),
                    if adapter.resource_present {
                        "present"
                    } else {
                        "missing"
                    }
                );
                println!(
                    "        {} {}",
                    color.label("Framework:"),
                    if adapter.framework_detected {
                        "detected"
                    } else {
                        "not detected"
                    }
                );
                println!(
                    "        {} {}",
                    color.label("Driver:"),
                    if adapter.driver_available {
                        "available"
                    } else {
                        "missing"
                    }
                );
                println!(
                    "        {} {}",
                    color.label("State:"),
                    color.status(adapter_state_label(adapter))
                );
            }
        }
    }
}

fn scoped_component_command(scope: &str, operation: &str, component: &str) -> String {
    if scope == "system" {
        format!("sudo anolisa --install-mode system {operation} {component}")
    } else {
        format!("anolisa --install-mode user {operation} {component}")
    }
}

fn scope_metadata_pairs(record: &ComponentRecord) -> Vec<(&'static str, String)> {
    let mut pairs = vec![
        ("active", record.active.to_string()),
        (
            "mutable_by_current_invocation",
            record.mutable_by_current_invocation.to_string(),
        ),
    ];
    if let Some(scope) = record.shadowed_by.as_deref() {
        pairs.push(("shadowed_by", scope.to_string()));
    }
    if let Some(path) = record.state_path.as_deref() {
        pairs.push(("state_path", path.to_string()));
    }
    pairs
}

fn render_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

/// Render quarantined records after the component table. Each row names
/// the record, why it was refused, and the two exits — without this
/// section a quarantined record is invisible: every other command only
/// mentions it when it happens to block them.
fn render_quarantined(records: &[QuarantinedRecord], no_color: bool) {
    if records.is_empty() {
        return;
    }
    let color = Palette::new(no_color);
    println!();
    println!("{}", color.header("NEEDS ATTENTION"));
    for record in records {
        println!(
            "{} {} ({}, {}) — quarantined: {}",
            color.warn("!"),
            record.name,
            record.kind,
            record.scope,
            record.reason,
        );
        println!("    {} {}", color.label("hint:"), record.hint);
    }
}

fn adapter_state_label(adapter: &AdapterSummaryRecord) -> &'static str {
    match (
        adapter.enabled,
        adapter.claim_status,
        adapter.source_status.as_deref(),
    ) {
        (_, Some(ClaimStatus::CleanupFailed), _) => "cleanup_failed",
        (true, _, Some("missing")) => "orphaned",
        (true, Some(ClaimStatus::Enabled), _) => "enabled",
        (true, None, _) => "enabled",
        (false, _, _) => "not enabled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::state_view::{ScopedStateRoot, StateScope, StateView};
    use crate::repo_config::RepoConfig;
    use anolisa_core::{
        FileOwner, HealthEntry, InstalledObject, InstalledState, NotSupportedServiceManager,
        ObjectKind, ObjectStatus, OwnedFile, OwnedFileKind, SubscriptionScope,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn snapshot_contract_error_names_component_and_probe() {
        let error = StatusSnapshotError::Contract {
            component: "agentsight".to_string(),
            source: SnapshotContractError::MissingRequestedEvidence {
                probe: SnapshotProbe::Adapters,
            },
        };

        assert_eq!(
            error.to_string(),
            "component snapshot contract failed for \"agentsight\": requested probe Adapters has no evidence",
        );
    }

    #[test]
    fn telemetry_service_target_redirects_when_component_index_is_unavailable() {
        let view = scoped_status_view(InstalledState::default(), InstalledState::default());
        let error = resolve_component_target("anolisa-telemetry", &view, None)
            .expect_err("a telemetry service is not a component target");

        assert_eq!(error.code(), "INVALID_ARGUMENT");
        assert_eq!(error.command(), COMMAND);
        assert_eq!(
            error.reason(),
            "unsupported component 'anolisa-telemetry'; run `anolisa telemetry status` to inspect telemetry state",
        );
    }

    #[test]
    fn exact_stored_component_identity_wins_over_management_redirect() {
        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "anolisa-telemetry",
            "1.0.0",
            ObjectStatus::Installed,
        ));
        let view = scoped_status_view(state, InstalledState::default());
        let index = status_component_index();

        assert_eq!(
            resolve_component_target("anolisa-telemetry", &view, Some(&index))
                .expect("exact component identity must win"),
            "anolisa-telemetry",
        );
        assert_eq!(
            resolve_component_target("anolisa-telemetry", &view, None)
                .expect("exact component identity remains inspectable offline"),
            "anolisa-telemetry",
        );
    }

    #[test]
    fn component_index_rejects_unsupported_target_with_discovery_help() {
        let index = status_component_index();
        let view = scoped_status_view(InstalledState::default(), InstalledState::default());
        let error = resolve_component_target("unknown-component", &view, Some(&index))
            .expect_err("an authoritative index can reject unknown names");

        assert_eq!(error.code(), "INVALID_ARGUMENT");
        assert_eq!(
            error.reason(),
            "unsupported component 'unknown-component'; run `anolisa list` to view supported components",
        );
    }

    #[test]
    fn component_index_accepts_canonical_names_and_aliases() {
        let index = status_component_index();
        let view = scoped_status_view(InstalledState::default(), InstalledState::default());

        assert_eq!(
            resolve_component_target("cosh", &view, Some(&index)).expect("canonical component"),
            "cosh",
        );
        assert_eq!(
            resolve_component_target("copilot-shell", &view, Some(&index)).expect("package alias"),
            "cosh",
        );
    }

    #[test]
    fn unknown_target_fails_when_component_index_is_unavailable() {
        let view = scoped_status_view(InstalledState::default(), InstalledState::default());
        let error = resolve_component_target("unknown-component", &view, None)
            .expect_err("new component identities require the component index");

        assert_eq!(error.code(), "EXECUTION_FAILED");
        assert_eq!(
            error.reason(),
            "component index is unavailable; cannot validate component 'unknown-component'; run `anolisa list` to inspect repository metadata",
        );
    }

    fn status_component_index() -> ComponentIndex {
        ComponentIndex::from_toml_str(
            r#"
schema_version = 2

[[components]]
name = "cosh"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.backends]]
kind = "rpm"
package = "copilot-shell"

[[components.aliases]]
kind = "rpm-package"
name = "copilot-shell"
"#,
            "components.toml",
        )
        .expect("component index")
    }

    #[test]
    fn lifecycle_hints_preserve_record_scope() {
        assert_eq!(
            scoped_component_command("system", "repair", "agentsight"),
            "sudo anolisa --install-mode system repair agentsight",
        );
        assert_eq!(
            scoped_component_command("user", "forget", "cosh"),
            "anolisa --install-mode user forget cosh",
        );
    }

    /// Build a system-mode FsLayout rooted under `prefix` and pre-create
    /// `bin_dir` so the path-safety guard in [`anolisa_core::check_owned_file`]
    /// has a canonical root to anchor on. Tests place owned files under
    /// `layout.bin_dir` to stay inside the ANOLISA-owned roots.
    fn test_layout(prefix: &Path) -> FsLayout {
        let layout = FsLayout::system(Some(prefix.to_path_buf()));
        std::fs::create_dir_all(&layout.bin_dir).expect("mkdir bin_dir");
        layout
    }

    /// Convenience for tests that don't exercise integrity at all — they
    /// only care about projection/filtering and the layout will never be
    /// touched. Uses a throwaway prefix that we don't bother creating.
    fn dummy_layout() -> FsLayout {
        FsLayout::system(Some(PathBuf::from("/tmp/anolisa-status-tests-noop")))
    }

    /// Write an installed-manifest snapshot for `component` under the
    /// layout's state_dir — the file `manifest_health_probe` consumes.
    fn write_manifest_snapshot(layout: &FsLayout, component: &str, manifest: &str) {
        let dir = layout.state_dir.join("component-manifests").join(component);
        std::fs::create_dir_all(&dir).expect("snapshot dir");
        std::fs::write(dir.join("component.toml"), manifest).expect("write snapshot");
    }

    /// View selector with quiet service backends, for tests that don't
    /// exercise `systemd_active` routing.
    fn select_components_from_view_quiet(
        view: &StateView,
        name: Option<&str>,
        adapter_scan: Option<&[ScanEntry]>,
        rpm_query: Option<&dyn PackageQuery>,
    ) -> Vec<ComponentRecord> {
        let quiet = NotSupportedServiceManager::new("service probes disabled in this test".into());
        select_components_from_view(
            view,
            name,
            AggregateRecordSelection::AllVisible,
            adapter_scan,
            Some(&ServiceProbeBackends {
                system_scope: &quiet,
                current_system: &quiet,
                user: &quiet,
            }),
            rpm_query,
        )
        .expect("test snapshot evidence must satisfy its request")
    }

    /// Baseline component install record. Owned `files` default to empty
    /// so projection-only tests never touch the filesystem; integrity
    /// tests attach files explicitly before upserting.
    fn component_object(name: &str, version: &str, status: ObjectStatus) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: name.to_string(),
            version: version.to_string(),
            status,
            manifest_digest: Some("sha256:abc".to_string()),
            distribution_source: Some("builtin".to_string()),
            raw_package: None,
            install_backend: None,
            ownership: Some(Ownership::RawManaged),
            rpm_metadata: None,
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: Some("op-20260601-001".to_string()),
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

    /// Migrate a v4 fixture state into a v5 store; fixtures must migrate
    /// cleanly so tests exercise exactly the objects they seeded.
    fn store_with(state: &InstalledState) -> StateStore {
        store_with_scope(state, InstallationScope::System)
    }

    fn store_with_scope(state: &InstalledState, scope: InstallationScope) -> StateStore {
        let migration = anolisa_core::state_migration::migrate_state(&state.objects, scope);
        assert!(
            migration.quarantined.is_empty(),
            "fixtures must migrate cleanly"
        );
        let mut store = StateStore::empty();
        store.installations = migration.active;
        store
    }

    fn scoped_status_view(user_state: InstalledState, system_state: InstalledState) -> StateView {
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
            state: store_with_scope(&user_state, InstallationScope::User { uid: 1000 }),
        };
        let system_root = ScopedStateRoot {
            scope: StateScope::System,
            layout: FsLayout::system(Some(PathBuf::from("/tmp/anolisa-system"))),
            state_path: PathBuf::from("/tmp/anolisa-system-state/installed.toml"),
            writable: false,
            state: store_with(&system_state),
        };
        StateView {
            writable: user_root.clone(),
            visible_roots: vec![user_root, system_root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn user_status_view_includes_system_component() {
        let user_state = InstalledState::default();
        let mut system_state = InstalledState::default();
        system_state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let view = scoped_status_view(user_state, system_state);

        let records = select_components_from_view_quiet(&view, Some("agentsight"), None, None);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "agentsight");
        assert_eq!(records[0].status, "installed");
        assert_eq!(records[0].scope, "system");
        assert!(records[0].active);
        assert!(!records[0].mutable_by_current_invocation);
        assert_eq!(
            records[0].state_path.as_deref(),
            Some("/tmp/anolisa-system-state/installed.toml")
        );
    }

    #[test]
    fn quarantined_records_surface_as_needs_attention() {
        use anolisa_core::state_migration::QuarantinedObject;

        let mut view = scoped_status_view(InstalledState::default(), InstalledState::default());
        view.visible_roots[0]
            .state
            .quarantined
            .push(QuarantinedObject {
                record: component_object("legacy-flatpak", "0.9.0", ObjectStatus::Installed),
                reason: QuarantineReason::UnknownBackend {
                    backend: "flatpak".to_string(),
                },
            });
        view.visible_roots[1]
            .state
            .quarantined
            .push(QuarantinedObject {
                record: component_object("legacy-bare", "0.1.0", ObjectStatus::Installed),
                reason: QuarantineReason::NoEvidence,
            });

        let all = select_quarantined_from_view(&view, None);
        assert_eq!(all.len(), 2);

        let flatpak = &all[0];
        assert_eq!(flatpak.name, "legacy-flatpak");
        assert_eq!(flatpak.kind, "component");
        assert_eq!(flatpak.scope, "user");
        assert_eq!(flatpak.status, "needs-attention");
        assert_eq!(flatpak.version, "0.9.0");
        assert!(flatpak.reason.contains("'flatpak'"), "{}", flatpak.reason);
        assert!(
            flatpak
                .hint
                .contains("anolisa --install-mode user repair legacy-flatpak")
                && flatpak
                    .hint
                    .contains("anolisa --install-mode user forget legacy-flatpak"),
            "{}",
            flatpak.hint
        );

        let bare = &all[1];
        assert_eq!(bare.scope, "system");
        assert!(
            bare.reason.contains("no provenance evidence"),
            "{}",
            bare.reason
        );
        assert!(
            bare.hint
                .contains("sudo anolisa --install-mode system repair legacy-bare")
                && bare
                    .hint
                    .contains("sudo anolisa --install-mode system forget legacy-bare"),
            "{}",
            bare.hint
        );

        // A named query surfaces only the matching quarantined record —
        // this is how `status <name>` explains why other intents refuse it.
        let named = select_quarantined_from_view(&view, Some("legacy-bare"));
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, "legacy-bare");
        assert!(select_quarantined_from_view(&view, Some("unrelated")).is_empty());
    }

    /// The `quarantined` key is new wire surface: it must be absent (not
    /// `[]`) when nothing is quarantined, so existing consumers see an
    /// unchanged envelope on healthy state.
    #[test]
    fn status_payload_omits_quarantined_when_empty() {
        let payload = StatusPayload {
            components: Vec::new(),
            quarantined: Vec::new(),
            warnings: Vec::new(),
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert!(json.get("quarantined").is_none(), "{json}");
    }

    #[test]
    fn scoped_status_view_uses_snapshot_from_record_root() {
        let user_home = tempfile::tempdir().expect("user home");
        let user_layout = FsLayout::user_with_overrides(
            user_home.path().join("home"),
            Some(user_home.path().join("data")),
            Some(user_home.path().join("config")),
            Some(user_home.path().join("state")),
            Some(user_home.path().join("cache")),
            Some(user_home.path().join("runtime")),
        );
        std::fs::create_dir_all(&user_layout.bin_dir).expect("user bin dir");

        let system_prefix = tempfile::tempdir().expect("system prefix");
        let system_layout = test_layout(system_prefix.path());
        let system_probe = system_layout.bin_dir.join("agentsight");
        std::fs::write(&system_probe, b"binary").expect("write system probe");

        // The manifest snapshot lives under the record root's state_dir, so
        // the probe path must resolve against the system root — the user
        // root has no snapshot at all.
        write_manifest_snapshot(
            &system_layout,
            "agentsight",
            &format!(
                r#"
                [component]
                name = "agentsight"
                version = "0.1.0"

                [component.health_check]
                type = "file_exists"
                path = "{}"
            "#,
                system_probe.display()
            ),
        );

        let user_state_path = user_layout.state_dir.join("installed.toml");
        let system_state_path = system_layout.state_dir.join("installed.toml");
        let user_root = ScopedStateRoot {
            scope: StateScope::User,
            layout: user_layout,
            state_path: user_state_path.clone(),
            writable: true,
            state: StateStore::empty(),
        };
        let mut system_state = InstalledState::default();
        system_state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let system_root = ScopedStateRoot {
            scope: StateScope::System,
            layout: system_layout,
            state_path: system_state_path.clone(),
            writable: false,
            state: store_with(&system_state),
        };
        let view = StateView {
            writable: user_root.clone(),
            visible_roots: vec![user_root, system_root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        };

        let records = select_components_from_view_quiet(&view, Some("agentsight"), None, None);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].scope, "system");
        assert_eq!(records[0].status, "installed");
        let health = records[0]
            .health
            .iter()
            .find(|entry| entry.name.starts_with("agentsight:file_exists"))
            .expect("system snapshot health entry present");
        assert_eq!(health.status, "ok");
    }

    #[test]
    fn named_status_view_reports_shadowed_system_record() {
        let mut user_state = InstalledState::default();
        user_state.upsert_object(component_object(
            "agentsight",
            "0.2.0",
            ObjectStatus::Installed,
        ));
        let mut system_state = InstalledState::default();
        system_state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let view = scoped_status_view(user_state, system_state);

        let records = select_components_from_view_quiet(&view, Some("agentsight"), None, None);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].scope, "user");
        assert!(records[0].active);
        assert_eq!(records[1].scope, "system");
        assert!(!records[1].active);
        assert_eq!(records[1].shadowed_by.as_deref(), Some("user"));
    }

    #[test]
    fn unnamed_status_view_reports_shadowed_system_record() {
        let mut user_state = InstalledState::default();
        user_state.upsert_object(component_object(
            "agentsight",
            "0.2.0",
            ObjectStatus::Installed,
        ));
        let mut system_state = InstalledState::default();
        system_state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let view = scoped_status_view(user_state, system_state);

        let records = select_components_from_view_quiet(&view, None, None, None);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].scope, "user");
        assert!(records[0].active);
        assert_eq!(records[1].scope, "system");
        assert!(!records[1].active);
        assert_eq!(records[1].shadowed_by.as_deref(), Some("user"));
    }

    #[test]
    fn system_status_view_projects_only_visible_system_root() {
        let mut system_state = InstalledState::default();
        system_state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let system_root = ScopedStateRoot {
            scope: StateScope::System,
            layout: FsLayout::system(Some(PathBuf::from("/tmp/anolisa-system"))),
            state_path: PathBuf::from("/tmp/anolisa-system-state/installed.toml"),
            writable: true,
            state: store_with(&system_state),
        };
        let view = StateView {
            writable: system_root.clone(),
            visible_roots: vec![system_root],
            unavailable_roots: Vec::new(),
            warnings: Vec::new(),
        };

        let records = select_components_from_view_quiet(&view, None, None, None);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "agentsight");
        assert_eq!(records[0].scope, "system");
    }

    /// A missing `installed.toml` is the fresh-install case and must
    /// surface as an empty result, not an error. Verifies the helper
    /// stack (`StateStore::load` -> `select_components`) collapses
    /// "no file" to "no components".
    #[test]
    fn missing_state_file_yields_empty_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");
        let state = StateStore::load(&path, 0).expect("missing file is not an error");
        let records = select_components(&state, &dummy_layout(), "system", None, None);
        assert!(records.is_empty());
    }

    #[test]
    fn unfiltered_listing_returns_all_components() {
        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        state.upsert_object(component_object(
            "tokenless",
            "0.2.0",
            ObjectStatus::Partial,
        ));

        let records = select_components(&store_with(&state), &dummy_layout(), "system", None, None);
        assert_eq!(records.len(), 2);
        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"agentsight"));
        assert!(names.contains(&"tokenless"));
        // Partial maps to the wire-friendly `degraded` label.
        let tokenless = records
            .iter()
            .find(|r| r.name == "tokenless")
            .expect("present");
        assert_eq!(tokenless.status, "degraded");
    }

    #[test]
    fn filter_miss_yields_synthetic_not_installed_record() {
        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let records = select_components(
            &store_with(&state),
            &dummy_layout(),
            "system",
            Some("ws-ckpt"),
            None,
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "ws-ckpt");
        assert_eq!(records[0].status, "not_installed");
        assert!(records[0].version.is_none());
        assert!(records[0].installed_at.is_none());
        assert!(records[0].last_operation_id.is_none());
        assert!(records[0].enabled_features.is_empty());
    }

    #[test]
    fn filter_hit_returns_stored_record() {
        let mut state = InstalledState::default();
        // No owned files -> integrity probe is a no-op so the
        // state-projected record passes through clean.
        let mut obj = component_object("agentsight", "0.3.1", ObjectStatus::Installed);
        obj.enabled_features = vec!["bpf-events".to_string()];
        obj.health = vec![HealthEntry {
            name: "binary".to_string(),
            status: "ok".to_string(),
            checked_at: "2026-06-01T10:01:00Z".to_string(),
            reason: None,
        }];
        state.upsert_object(obj);

        let records = select_components(
            &store_with(&state),
            &dummy_layout(),
            "system",
            Some("agentsight"),
            None,
        );
        assert_eq!(records.len(), 1);
        let only = &records[0];
        assert_eq!(only.name, "agentsight");
        assert_eq!(only.status, "installed");
        assert_eq!(only.version.as_deref(), Some("0.3.1"));
        assert_eq!(only.installed_at.as_deref(), Some("2026-06-01T10:00:00Z"));
        assert_eq!(only.last_operation_id.as_deref(), Some("op-20260601-001"));
        // State-projected fields must reach the wire record verbatim.
        assert_eq!(only.enabled_features, vec!["bpf-events"]);
        assert_eq!(only.health.len(), 1);
        assert_eq!(only.health[0].name, "binary");
        assert_eq!(only.health[0].status, "ok");
    }

    /// Component whose owned files are all present on disk with matching
    /// sha256 stays `installed` and the wire record gains one
    /// `integrity:<path>` health entry per file with `status = "ok"`.
    #[test]
    fn integrity_probe_present_file_with_matching_sha_keeps_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        let file_path = layout.bin_dir.join("agentsight");
        std::fs::write(&file_path, b"payload").expect("write");

        let mut state = InstalledState::default();
        let mut comp = component_object("agentsight", "0.1.0", ObjectStatus::Installed);
        comp.files = vec![OwnedFile {
            path: file_path.clone(),
            owner: FileOwner::Anolisa,
            sha256: Some(
                "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_string(),
            ),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }];
        state.upsert_object(comp);

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "installed");
        // Exactly one integrity entry, status "ok", with the path in the name.
        let integrity: Vec<&HealthEntry> = only
            .health
            .iter()
            .filter(|h| h.name.starts_with("integrity:"))
            .collect();
        assert_eq!(integrity.len(), 1);
        assert_eq!(integrity[0].status, "ok");
        assert!(integrity[0].name.ends_with("agentsight"));
    }

    /// Missing owned file on disk escalates the component status to
    /// `"failed"` and emits a `missing_file` health entry. The original
    /// `installed` ObjectStatus is NOT mutated — escalation is purely
    /// at the wire layer (`status` field).
    #[test]
    fn integrity_probe_missing_file_escalates_to_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        let missing_path = layout.bin_dir.join("anolisa-integrity-missing");

        let mut state = InstalledState::default();
        let mut comp = component_object("agentsight", "0.1.0", ObjectStatus::Installed);
        comp.files = vec![OwnedFile {
            path: missing_path,
            owner: FileOwner::Anolisa,
            sha256: Some("deadbeef".to_string()),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }];
        state.upsert_object(comp);

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "failed", "missing file -> failed");
        let integrity = only
            .health
            .iter()
            .find(|h| h.name.starts_with("integrity:"))
            .expect("integrity entry present");
        assert_eq!(integrity.status, "missing_file");
    }

    /// Tampered file (sha256 mismatch) escalates to `"failed"` and
    /// emits a `sha256_mismatch` health entry — distinct from
    /// `missing_file` so the user can tell which kind of drift occurred.
    #[test]
    fn integrity_probe_sha_mismatch_escalates_to_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        let file_path = layout.bin_dir.join("agentsight");
        std::fs::write(&file_path, b"tampered-payload").expect("write");

        let mut state = InstalledState::default();
        let mut comp = component_object("agentsight", "0.1.0", ObjectStatus::Installed);
        comp.files = vec![OwnedFile {
            path: file_path,
            owner: FileOwner::Anolisa,
            sha256: Some(
                "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            ),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }];
        state.upsert_object(comp);

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "failed", "sha mismatch -> failed");
        let integrity = only
            .health
            .iter()
            .find(|h| h.name.starts_with("integrity:"))
            .expect("integrity entry present");
        assert_eq!(integrity.status, "sha256_mismatch");
    }

    /// File exists but no sha256 was recorded -> degrade (not fail). We
    /// can't prove tampering either way; "degraded" signals the user
    /// should treat the install with skepticism without claiming it's
    /// broken.
    #[test]
    fn integrity_probe_unverified_file_degrades_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        let file_path = layout.bin_dir.join("agentsight");
        std::fs::write(&file_path, b"payload").expect("write");

        let mut state = InstalledState::default();
        let mut comp = component_object("agentsight", "0.1.0", ObjectStatus::Installed);
        comp.files = vec![OwnedFile {
            path: file_path,
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }];
        state.upsert_object(comp);

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "degraded");
        let integrity = only
            .health
            .iter()
            .find(|h| h.name.starts_with("integrity:"))
            .expect("integrity entry present");
        assert_eq!(integrity.status, "unverified");
    }

    /// A disabled component MUST stay disabled even if its owned files
    /// are gone — `disabled` is a deliberate state set by the user, not
    /// a drift signal we should overwrite from a sha probe.
    #[test]
    fn integrity_probe_does_not_escalate_disabled_component() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        let missing_path = layout.bin_dir.join("anolisa-integrity-still-disabled");

        let mut state = InstalledState::default();
        let mut comp = component_object("agentsight", "0.1.0", ObjectStatus::Disabled);
        comp.files = vec![OwnedFile {
            path: missing_path,
            owner: FileOwner::Anolisa,
            sha256: Some("deadbeef".to_string()),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }];
        state.upsert_object(comp);

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "disabled");
        // The integrity entry is still surfaced so users can see the drift,
        // even though the wire status stays disabled.
        let integrity = only
            .health
            .iter()
            .find(|h| h.name.starts_with("integrity:"))
            .expect("integrity entry present");
        assert_eq!(integrity.status, "missing_file");
    }

    /// A forged `installed.toml` pointing an `owner = anolisa` file at a
    /// path outside the ANOLISA-owned roots must be refused by `status`
    /// without any stat or read happening. We point at `/etc/shadow` —
    /// if the path-safety guard fell through, integrity would either
    /// open the file (worst case) or report `MissingFile` on a host where
    /// it doesn't exist. `out_of_bounds` is the only status that proves
    /// the guard fired before IO.
    #[test]
    fn integrity_probe_refuses_path_outside_owned_roots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());

        let mut state = InstalledState::default();
        let mut comp = component_object("agentsight", "0.1.0", ObjectStatus::Installed);
        comp.files = vec![OwnedFile {
            path: PathBuf::from("/etc/shadow"),
            owner: FileOwner::Anolisa,
            sha256: Some("deadbeef".to_string()),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }];
        state.upsert_object(comp);

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "failed", "out-of-bounds path -> failed");
        let integrity = only
            .health
            .iter()
            .find(|h| h.name.starts_with("integrity:"))
            .expect("integrity entry present");
        assert_eq!(
            integrity.status, "out_of_bounds",
            "path-safety guard must fire before any stat",
        );
    }

    // -----------------------------------------------------------------
    // Manifest health probe tests
    // -----------------------------------------------------------------

    /// Snapshot-declared check passing keeps the wire status at
    /// `installed` and emits the engine's `ok` entry under the
    /// `<component>:<check label>` name.
    #[test]
    fn manifest_health_snapshot_check_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        let probe_path = layout.bin_dir.join("agentsight");
        std::fs::write(&probe_path, b"binary").expect("write probe binary");
        write_manifest_snapshot(
            &layout,
            "agentsight",
            &format!(
                r#"
                [component]
                name = "agentsight"
                version = "0.1.0"

                [component.health_check]
                type = "file_exists"
                path = "{}"
            "#,
                probe_path.display()
            ),
        );

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "installed");
        let entry = only
            .health
            .iter()
            .find(|h| h.name.starts_with("agentsight:file_exists"))
            .expect("snapshot health entry present");
        assert_eq!(entry.status, "ok");
    }

    #[test]
    fn repaired_cosh_ng_status_skips_the_legacy_snapshot_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        write_manifest_snapshot(
            &layout,
            "cosh",
            r#"
                [component]
                name = "cosh"
                version = "0.13.0"

                [component.health_check]
                type = "file_exists"
                path = "{bindir}/cosh-cli"

                [backends.rpm]
                package = "cosh-ng"
            "#,
        );

        let mut state = InstalledState::default();
        state.upsert_object(rpm_observed_object("cosh-ng", "cosh-ng", "0.13.0-1.al8"));

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("cosh-ng"),
            None,
        );

        assert_eq!(records[0].status, "adopted");
        assert!(
            records[0]
                .health
                .iter()
                .all(|entry| !entry.name.starts_with("cosh-ng:file_exists")),
            "delegated RPM must not run raw-layout manifest checks"
        );
    }

    /// Failing snapshot check escalates the wire status to `failed` with
    /// the probe detail in the entry reason.
    #[test]
    fn manifest_health_snapshot_check_failure_escalates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        let missing = layout.bin_dir.join("ghost-binary");
        write_manifest_snapshot(
            &layout,
            "agentsight",
            &format!(
                r#"
                [component]
                name = "agentsight"
                version = "0.1.0"

                [component.health_check]
                type = "file_exists"
                path = "{}"
            "#,
                missing.display()
            ),
        );

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "failed", "failed check -> failed");
        let entry = only
            .health
            .iter()
            .find(|h| h.name.starts_with("agentsight:file_exists"))
            .expect("snapshot health entry present");
        assert_eq!(entry.status, "failed");
        assert!(
            entry.reason.as_deref().unwrap_or("").contains("missing"),
            "reason names the missing file: {:?}",
            entry.reason
        );
    }

    /// A user-scope record whose snapshot declares a user-scope service
    /// probes `systemd_active` through the user manager — never the
    /// system factory (deliberately unsupported in user mode, which would
    /// spuriously degrade a healthy `systemctl --user` unit).
    #[test]
    fn manifest_health_routes_user_scope_service_to_the_user_manager() {
        use anolisa_core::{FakeServiceManager, ServiceOp, ServiceState};

        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        write_manifest_snapshot(
            &layout,
            "agent-memory",
            r#"
                [component]
                name = "agent-memory"
                version = "0.1.0"

                [component.layout]
                modes = ["system", "user"]

                [[component.services]]
                unit = "anolisa-memory@.service"
                scope = "user"

                [component.health_check]
                type = "systemd_active"
                service = "anolisa-memory@alice.service"
            "#,
        );

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agent-memory",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let store = store_with(&state);

        let system = FakeServiceManager::new();
        let user = FakeServiceManager::new();
        user.set_state(ServiceState::Active);
        let backends = ServiceProbeBackends {
            system_scope: &system,
            current_system: &system,
            user: &user,
        };

        let record = record_from_object(&layout, "user", Some(&backends), &store.installations[0]);

        assert_eq!(record.status, "installed");
        let entry = record
            .health
            .iter()
            .find(|h| h.name.starts_with("agent-memory:systemd_active"))
            .expect("systemd health entry present");
        assert_eq!(entry.status, "ok");
        assert_eq!(
            user.calls(),
            vec![(ServiceOp::Probe, "anolisa-memory@alice.service".to_string())]
        );
        assert!(
            system.calls().is_empty(),
            "system manager must not be probed for a user-scope unit"
        );
    }

    /// A system-scope record whose health check targets a user-scope
    /// service still routes to the user manager: probing the system
    /// namespace would misreport a healthy user unit as failed.
    #[test]
    fn manifest_health_routes_user_scope_service_on_a_system_record() {
        use anolisa_core::{FakeServiceManager, ServiceOp, ServiceState};

        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        write_manifest_snapshot(
            &layout,
            "agent-memory",
            r#"
                [component]
                name = "agent-memory"
                version = "0.1.0"

                [component.layout]
                modes = ["system", "user"]

                [[component.services]]
                unit = "anolisa-memory@.service"
                scope = "user"

                [component.health_check]
                type = "systemd_active"
                service = "anolisa-memory@alice.service"
            "#,
        );

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agent-memory",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let store = store_with(&state);

        let system_scope = FakeServiceManager::new();
        let current_system = FakeServiceManager::new();
        let user = FakeServiceManager::new();
        user.set_state(ServiceState::Active);
        let backends = ServiceProbeBackends {
            system_scope: &system_scope,
            current_system: &current_system,
            user: &user,
        };

        let record =
            record_from_object(&layout, "system", Some(&backends), &store.installations[0]);

        assert_eq!(record.status, "installed");
        assert_eq!(
            user.calls(),
            vec![(ServiceOp::Probe, "anolisa-memory@alice.service".to_string())]
        );
        assert!(
            system_scope.calls().is_empty() && current_system.calls().is_empty(),
            "no system manager may be probed for a user-scope unit"
        );
    }

    /// `binary_version` runs the declared binary end to end through the
    /// shared engine: exit 0 keeps `installed`, a failing binary escalates.
    #[test]
    fn manifest_health_snapshot_binary_version_probes_the_binary() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        std::fs::create_dir_all(&layout.bin_dir).expect("bin dir");
        let exe = layout.bin_dir.join("agentsight");
        std::fs::write(&exe, "#!/bin/sh\nexit 0\n").expect("write probe script");
        let mut perms = std::fs::metadata(&exe).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&exe, perms).expect("chmod");
        write_manifest_snapshot(
            &layout,
            "agentsight",
            &format!(
                r#"
                [component]
                name = "agentsight"
                version = "0.1.0"

                [component.health_check]
                type = "binary_version"
                binary = "{}"
            "#,
                exe.display()
            ),
        );

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        assert_eq!(records[0].status, "installed");

        // Same component, now with a failing probe binary.
        std::fs::write(&exe, "#!/bin/sh\nexit 3\n").expect("rewrite probe script");
        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        assert_eq!(records[0].status, "failed", "non-zero probe -> failed");
    }

    /// A probe the engine refuses to run (path outside ANOLISA-owned
    /// roots) is `unsupported`: the component degrades because nothing
    /// was proven, but it is not reported broken. The path-safety rules
    /// themselves are covered by the engine's own tests.
    #[test]
    fn manifest_health_out_of_bounds_probe_degrades() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        write_manifest_snapshot(
            &layout,
            "agentsight",
            r#"
                [component]
                name = "agentsight"
                version = "0.1.0"

                [component.health_check]
                type = "file_exists"
                path = "/etc/passwd"
            "#,
        );

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "degraded", "unsupported check -> degraded");
        let entry = only
            .health
            .iter()
            .find(|h| h.name.starts_with("agentsight:file_exists"))
            .expect("snapshot health entry present");
        assert_eq!(entry.status, "unsupported");
    }

    /// No snapshot on disk is the adopted / pre-snapshot case: silent, no
    /// manifest entries, status untouched.
    #[test]
    fn manifest_health_missing_snapshot_is_silent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "installed");
        assert!(
            only.health
                .iter()
                .all(|h| h.name.starts_with("integrity:") || !h.name.contains(':')),
            "no manifest entries without a snapshot: {:?}",
            only.health
        );
    }

    /// An unreadable snapshot degrades the component and says why — the
    /// record exists but its contract cannot be verified.
    #[test]
    fn manifest_health_unreadable_snapshot_degrades() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        write_manifest_snapshot(&layout, "agentsight", "not = [valid toml");

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "degraded", "unreadable snapshot -> degraded");
        let entry = only
            .health
            .iter()
            .find(|h| h.name == "agentsight:manifest_snapshot")
            .expect("snapshot parse entry present");
        assert_eq!(entry.status, "unreadable");
    }

    /// A snapshot with no declared check and no executable payload rows
    /// has nothing to probe — silent, like the missing-snapshot case.
    #[test]
    fn manifest_health_snapshot_without_check_is_silent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        write_manifest_snapshot(
            &layout,
            "agentsight",
            r#"
                [component]
                name = "agentsight"
                version = "0.1.0"
            "#,
        );

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        assert_eq!(records[0].status, "installed");
    }

    /// A failed integrity probe is never downgraded by a passing manifest
    /// check: escalation only moves toward more-broken.
    #[test]
    fn manifest_health_does_not_downgrade_failed_integrity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        // The manifest probe target exists (check passes)...
        let probe_path = layout.bin_dir.join("agentsight");
        std::fs::write(&probe_path, b"binary").expect("write probe binary");
        write_manifest_snapshot(
            &layout,
            "agentsight",
            &format!(
                r#"
                [component]
                name = "agentsight"
                version = "0.1.0"

                [component.health_check]
                type = "file_exists"
                path = "{}"
            "#,
                probe_path.display()
            ),
        );

        // ...but an owned file is missing, so integrity already failed.
        let mut state = InstalledState::default();
        let mut comp = component_object("agentsight", "0.1.0", ObjectStatus::Installed);
        comp.files = vec![OwnedFile {
            path: layout.bin_dir.join("gone-payload"),
            owner: FileOwner::Anolisa,
            sha256: Some("deadbeef".to_string()),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }];
        state.upsert_object(comp);

        let records = select_components(
            &store_with(&state),
            &layout,
            "system",
            Some("agentsight"),
            None,
        );
        let only = &records[0];
        assert_eq!(only.status, "failed", "manifest ok must not mask integrity");
        let entry = only
            .health
            .iter()
            .find(|h| h.name.starts_with("agentsight:file_exists"))
            .expect("manifest entry still present");
        assert_eq!(entry.status, "ok");
    }

    // -----------------------------------------------------------------
    // Adapter summary tests
    // -----------------------------------------------------------------

    fn sample_scan_entry(component: &str, framework: &str, enabled: bool) -> ScanEntry {
        ScanEntry {
            component: component.to_string(),
            framework: framework.to_string(),
            declared: true,
            resource_root: Some(PathBuf::from(format!(
                "/usr/local/share/anolisa/adapters/{component}/{framework}"
            ))),
            driver_available: true,
            framework_detected: true,
            adapter_type: Some("plugin".to_string()),
            enabled,
            claim_status: if enabled {
                Some(ClaimStatus::Enabled)
            } else {
                None
            },
            source_status: enabled.then_some(AdapterSourceStatus::Available),
            source_reason: None,
        }
    }

    #[test]
    fn component_record_has_no_adapters_by_default() {
        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let records = select_components(
            &store_with(&state),
            &dummy_layout(),
            "system",
            Some("agentsight"),
            None,
        );
        assert!(records[0].adapters.is_empty());
    }

    #[test]
    fn adapter_scan_failure_is_unavailable_and_keeps_output_empty() {
        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let view = scoped_status_view(InstalledState::default(), state);
        let visible = view.visible_components();
        let state_paths = view
            .visible_roots
            .iter()
            .map(|root| root.state_path.clone())
            .collect::<Vec<_>>();
        let snapshot = collect_status_snapshot(
            &visible[0],
            &AdapterScanEvidence::Unavailable("scan failed".to_string()),
            &state_paths,
            None,
            None,
            "2026-08-25T00:00:00Z",
        )
        .expect("status snapshot");

        assert!(matches!(
            snapshot.adapters(),
            ProbeEvidence::Unavailable { reason, provenance }
                if reason == "scan failed" && provenance.state_paths == state_paths
        ));
        let record =
            record_from_snapshot(&snapshot, "2026-08-25T00:00:00Z").expect("status projection");
        assert!(record.adapters.is_empty());
        assert_eq!(record.status, "installed");
    }

    #[test]
    fn adapter_summaries_filtered_to_requested_component() {
        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "tokenless",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let scan = vec![
            sample_scan_entry("tokenless", "openclaw", true),
            sample_scan_entry("agentsight", "openclaw", false),
        ];
        let records = select_components(
            &store_with(&state),
            &dummy_layout(),
            "system",
            Some("tokenless"),
            Some(&scan),
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].adapters.len(), 1);
        assert_eq!(records[0].adapters[0].component, "tokenless");
        assert_eq!(records[0].adapters[0].framework, "openclaw");
        assert!(records[0].adapters[0].enabled);
        assert_eq!(
            records[0].adapters[0].claim_status,
            Some(ClaimStatus::Enabled)
        );
    }

    #[test]
    fn adapter_summaries_included_in_unfiltered_listing() {
        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "tokenless",
            "0.1.0",
            ObjectStatus::Installed,
        ));

        let scan = vec![sample_scan_entry("tokenless", "openclaw", true)];
        let records = select_components(
            &store_with(&state),
            &dummy_layout(),
            "system",
            None,
            Some(&scan),
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].adapters.len(), 1);
        assert_eq!(records[0].adapters[0].component, "tokenless");
    }

    #[test]
    fn synthetic_not_installed_record_has_no_adapters() {
        let state = InstalledState::default();
        let scan = vec![sample_scan_entry("ghost", "openclaw", false)];
        let records = select_components(
            &store_with(&state),
            &dummy_layout(),
            "system",
            Some("ghost"),
            Some(&scan),
        );
        assert_eq!(records[0].status, "not_installed");
        assert!(records[0].adapters.is_empty());
    }

    #[test]
    fn adapter_summary_json_serialization() {
        let record = AdapterSummaryRecord {
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            declared: true,
            resource_present: true,
            resource_root: Some("/data/adapters/tokenless/openclaw".to_string()),
            driver_available: true,
            framework_detected: true,
            enabled: true,
            claim_status: Some(ClaimStatus::Enabled),
            source_status: Some("available".to_string()),
            source_reason: None,
        };
        let json = serde_json::to_value(&record).expect("serialize");
        assert_eq!(json["component"], "tokenless");
        assert_eq!(json["framework"], "openclaw");
        assert_eq!(json["declared"], true);
        assert_eq!(json["resource_present"], true);
        assert_eq!(json["driver_available"], true);
        assert_eq!(json["framework_detected"], true);
        assert_eq!(json["enabled"], true);
        assert_eq!(json["claim_status"], "enabled");
    }

    #[test]
    fn adapter_summary_skips_empty_adapters_in_json() {
        let record = ComponentRecord {
            name: "agentsight".to_string(),
            status: "installed".to_string(),
            scope: "system".to_string(),
            active: true,
            mutable_by_current_invocation: true,
            shadowed_by: None,
            state_path: Some("/tmp/anolisa-system-state/installed.toml".to_string()),
            version: Some("0.1.0".to_string()),
            installed_at: Some("2026-06-01T10:00:00Z".to_string()),
            last_operation_id: None,
            enabled_features: Vec::new(),
            health: Vec::new(),
            adapters: Vec::new(),
            rpm_package: None,
            rpm_evr: None,
            rpm_source_repo: None,
            provisioned_packages: Vec::new(),
        };
        let json = serde_json::to_value(&record).expect("serialize");
        assert!(
            json.get("adapters").is_none(),
            "empty adapters must be omitted from JSON"
        );
        assert!(
            json.get("rpm_package").is_none(),
            "empty rpm fields must be omitted from JSON"
        );
    }

    #[test]
    fn scope_metadata_pairs_expose_mutability_and_state_path() {
        let record = ComponentRecord {
            name: "agentsight".to_string(),
            status: "installed".to_string(),
            scope: "system".to_string(),
            active: false,
            mutable_by_current_invocation: false,
            shadowed_by: Some("user".to_string()),
            state_path: Some("/tmp/anolisa-system-state/installed.toml".to_string()),
            version: Some("0.1.0".to_string()),
            installed_at: Some("2026-06-01T10:00:00Z".to_string()),
            last_operation_id: None,
            enabled_features: Vec::new(),
            health: Vec::new(),
            adapters: Vec::new(),
            rpm_package: None,
            rpm_evr: None,
            rpm_source_repo: None,
            provisioned_packages: Vec::new(),
        };

        let pairs = scope_metadata_pairs(&record);

        assert!(pairs.contains(&("active", "false".to_string())));
        assert!(pairs.contains(&("mutable_by_current_invocation", "false".to_string())));
        assert!(pairs.contains(&("shadowed_by", "user".to_string())));
        assert!(pairs.contains(&(
            "state_path",
            "/tmp/anolisa-system-state/installed.toml".to_string()
        )));
    }

    #[test]
    fn adapter_state_label_values() {
        let base = AdapterSummaryRecord {
            component: "x".to_string(),
            framework: "y".to_string(),
            declared: true,
            resource_present: true,
            resource_root: None,
            driver_available: true,
            framework_detected: true,
            enabled: true,
            claim_status: Some(ClaimStatus::Enabled),
            source_status: Some("available".to_string()),
            source_reason: None,
        };

        assert_eq!(adapter_state_label(&base), "enabled");

        let mut cleanup = base.clone();
        cleanup.claim_status = Some(ClaimStatus::CleanupFailed);
        assert_eq!(adapter_state_label(&cleanup), "cleanup_failed");

        let mut enabled_no_claim = base.clone();
        enabled_no_claim.claim_status = None;
        assert_eq!(adapter_state_label(&enabled_no_claim), "enabled");

        let mut not_enabled = base.clone();
        not_enabled.enabled = false;
        not_enabled.source_status = None;
        assert_eq!(adapter_state_label(&not_enabled), "not enabled");

        let mut orphaned = base.clone();
        orphaned.source_status = Some("missing".to_string());
        orphaned.source_reason = Some("no visible installed component".to_string());
        assert_eq!(adapter_state_label(&orphaned), "orphaned");
    }

    // ── rpm-observed status (#958) ──────────────────────────────────

    use anolisa_core::{Ownership, RpmMetadata};
    use anolisa_platform::pkg_query::{PackageInfo, PackageQueryError, PackageVersion};

    /// An adopted `rpm-observed` component record.
    fn rpm_observed_object(name: &str, package: &str, evr: &str) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: name.to_string(),
            version: evr.to_string(),
            status: ObjectStatus::Adopted,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some("rpm".to_string()),
            ownership: Some(Ownership::RpmObserved),
            rpm_metadata: Some(RpmMetadata {
                package_name: package.to_string(),
                evr: Some(evr.to_string()),
                arch: Some("x86_64".to_string()),
                source_repo: Some("@System".to_string()),
            }),
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: Some("op-adopt-001".to_string()),
            managed: false,
            adopted: true,
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

    /// P2 gate: an rpm-observed row must keep `adopted` even when a manifest
    /// health check would fail a raw install — ANOLISA owns none of its files
    /// and never laid out the raw tree, so those checks do not apply (§8).
    #[test]
    fn delegated_rows_skip_manifest_health_escalation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(dir.path());
        let probe_path = layout.bin_dir.join("ghost-binary");
        // The snapshot declares a check that would fail a raw install
        // (missing probe target).
        write_manifest_snapshot(
            &layout,
            "copilot-shell",
            &format!(
                r#"
                [component]
                name = "copilot-shell"
                version = "2.3.0-1.al8"

                [component.health_check]
                type = "file_exists"
                path = "{}"
            "#,
                probe_path.display()
            ),
        );

        // Control: a raw install with the same failing check escalates.
        let mut raw_state = InstalledState::default();
        raw_state.upsert_object(component_object(
            "copilot-shell",
            "2.3.0",
            ObjectStatus::Installed,
        ));
        let raw = select_components(
            &store_with(&raw_state),
            &layout,
            "system",
            Some("copilot-shell"),
            None,
        );
        assert_eq!(raw[0].status, "failed", "raw install must escalate");

        // rpm-observed with the same snapshot stays adopted and surfaces the
        // RPM provenance fields.
        let mut obs_state = InstalledState::default();
        obs_state.upsert_object(rpm_observed_object(
            "copilot-shell",
            "copilot-shell",
            "2.3.0-1.al8",
        ));
        let obs = select_components(
            &store_with(&obs_state),
            &layout,
            "system",
            Some("copilot-shell"),
            None,
        );
        assert_eq!(obs[0].status, "adopted", "rpm-observed must not escalate");
        assert_eq!(obs[0].rpm_package.as_deref(), Some("copilot-shell"));
        assert_eq!(obs[0].rpm_evr.as_deref(), Some("2.3.0-1.al8"));
        assert_eq!(obs[0].rpm_source_repo.as_deref(), Some("@System"));

        // rpm-managed rows lay out files via RPM macros too: the same failing
        // raw-layout check must not escalate a managed row either.
        let mut managed_state = InstalledState::default();
        let mut managed = rpm_observed_object("copilot-shell", "copilot-shell", "2.3.0-1.al8");
        managed.status = ObjectStatus::Installed;
        managed.ownership = Some(Ownership::RpmManaged);
        managed.managed = true;
        managed.adopted = false;
        managed_state.upsert_object(managed);
        let rows = select_components(
            &store_with(&managed_state),
            &layout,
            "system",
            Some("copilot-shell"),
            None,
        );
        assert_eq!(rows[0].status, "installed", "rpm-managed must not escalate");
        assert!(
            !rows[0]
                .health
                .iter()
                .any(|entry| entry.name.contains("file_exists")),
            "rpm-managed rows must not run raw-layout manifest checks",
        );
    }

    /// Configurable [`PackageQuery`] for the observed-probe tests.
    #[derive(Default)]
    struct FakeQuery {
        installed: Vec<(String, PackageInfo)>,
        origins: Vec<(String, String)>,
    }

    impl PackageQuery for FakeQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            Ok(self
                .installed
                .iter()
                .find(|(n, _)| n == package)
                .map(|(_, i)| i.clone()))
        }
        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }
        fn installed_origin(&self, package: &str) -> Result<Option<String>, PackageQueryError> {
            Ok(self
                .origins
                .iter()
                .find(|(n, _)| n == package)
                .map(|(_, r)| r.clone()))
        }
        fn what_provides_installed(
            &self,
            capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            // Fake convention: every installed package provides
            // `anolisa-component(<its own name>)`.
            Ok(capability
                .strip_prefix("anolisa-component(")
                .and_then(|rest| rest.strip_suffix(')'))
                .into_iter()
                .filter(|name| self.installed.iter().any(|(n, _)| n == name))
                .map(str::to_string)
                .collect())
        }
    }

    fn pkg_info(name: &str, version: &str, release: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: PackageVersion {
                epoch: None,
                version: version.to_string(),
                release: Some(release.to_string()),
            },
            arch: "x86_64".to_string(),
            origin: None,
        }
    }

    #[test]
    fn status_lookup_name_uses_component_index_alias_before_state_selection() {
        let idx = ComponentIndex::from_toml_str(
            r#"
schema_version = 2

[[components]]
name = "cosh"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.backends]]
kind = "rpm"
package = "copilot-shell"
legacy_adopt = true

[[components.aliases]]
kind = "rpm-package"
name = "copilot-shell"
"#,
            "components.toml",
        )
        .expect("component index");

        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "cosh",
            "2.6.0-1.alnx4",
            ObjectStatus::Installed,
        ));
        let resolved = match resolve_index_identity("copilot-shell", Some(&idx)) {
            IndexIdentity::Resolved(component) => component,
            other => panic!("expected the alias to resolve, got {other:?}"),
        };
        assert_eq!(resolved, "cosh");
        let records = select_components(
            &store_with(&state),
            &dummy_layout(),
            "system",
            Some(&resolved),
            None,
        );

        assert_eq!(records[0].name, "cosh");
        assert_eq!(records[0].status, "installed");
    }

    #[test]
    fn observed_record_reports_installed_default_name() {
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.3.0", "1.al8"),
            )],
            origins: vec![("copilot-shell".to_string(), "@System".to_string())],
        };
        let rec = observed_record("copilot-shell", None, None, &q).expect("observed");
        assert_eq!(rec.status, "observed");
        assert_eq!(rec.rpm_package.as_deref(), Some("copilot-shell"));
        assert_eq!(rec.rpm_evr.as_deref(), Some("2.3.0-1.al8"));
        assert_eq!(rec.rpm_source_repo.as_deref(), Some("@System"));
        assert_eq!(rec.version.as_deref(), Some("2.3.0-1.al8"));
    }

    #[test]
    fn observed_record_none_when_not_installed() {
        let q = FakeQuery::default();
        assert!(observed_record("copilot-shell", None, None, &q).is_none());
    }

    #[test]
    fn observed_record_honors_package_map() {
        // package_map renames the component's RPM; the probe must query the
        // mapped name, not the default.
        let repo = RepoConfig::from_toml_str(
            "schema_version = 1\ndefault_backend = \"rpm\"\n[backends.rpm]\nbase_url = \"https://e/x\"\n[backends.rpm.package_map]\ncopilot-shell = \"site-copilot\"\n",
        )
        .expect("repo");
        let backend = repo.backends.get("rpm");
        let q = FakeQuery {
            installed: vec![(
                "site-copilot".to_string(),
                pkg_info("site-copilot", "9.9", "1"),
            )],
            origins: Vec::new(),
        };
        let rec = observed_record("copilot-shell", backend, None, &q).expect("observed");
        assert_eq!(rec.rpm_package.as_deref(), Some("site-copilot"));
        assert_eq!(rec.rpm_source_repo, None);
    }

    // ── rpm drift adjudication (#960) ───────────────────────────────

    /// Query whose `query_installed` always returns a preset anomalous error,
    /// to exercise the drift classification of the non-`Ok` branches.
    struct ErrQuery(PackageQueryError);

    impl PackageQuery for ErrQuery {
        fn query_installed(&self, _: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            Err(match &self.0 {
                PackageQueryError::UnexpectedOutput { command, detail } => {
                    PackageQueryError::UnexpectedOutput {
                        command: command.clone(),
                        detail: detail.clone(),
                    }
                }
                _ => PackageQueryError::CommandMissing {
                    command: "rpm".to_string(),
                },
            })
        }
        fn query_available(&self, _: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn native_query_failure_is_unavailable_and_keeps_recorded_status() {
        let mut state = InstalledState::default();
        state.upsert_object(rpm_observed_object(
            "copilot-shell",
            "copilot-shell",
            "2.2.0-1.al8",
        ));
        let view = scoped_status_view(InstalledState::default(), state);
        let visible = view.visible_components();
        let state_paths = view
            .visible_roots
            .iter()
            .map(|root| root.state_path.clone())
            .collect::<Vec<_>>();
        let query = ErrQuery(PackageQueryError::CommandMissing {
            command: "rpm".to_string(),
        });
        let snapshot = collect_status_snapshot(
            &visible[0],
            &AdapterScanEvidence::NotRequested,
            &state_paths,
            None,
            Some(&query),
            "2026-08-25T00:00:00Z",
        )
        .expect("status snapshot");

        assert!(matches!(
            snapshot.native_package(),
            ProbeEvidence::Unavailable { reason, provenance }
                if reason == "command not found: rpm" && provenance.package == "copilot-shell"
        ));
        let record =
            record_from_snapshot(&snapshot, "2026-08-25T00:00:00Z").expect("status projection");
        assert_eq!(record.status, "adopted");
        assert!(!record.health.iter().any(|entry| entry.name == "rpm:drift"));
    }

    /// rpmdb EVR matching the recorded one is not drift.
    #[test]
    fn probe_rpm_drift_none_when_evr_matches() {
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.3.0", "1.al8"),
            )],
            origins: Vec::new(),
        };
        assert!(probe_rpm_drift("copilot-shell", Some("2.3.0-1.al8"), &q).is_none());
    }

    /// A newer rpmdb EVR than recorded (manual `dnf update`) is drift.
    #[test]
    fn probe_rpm_drift_detects_evr_mismatch() {
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.3.0", "1.al8"),
            )],
            origins: Vec::new(),
        };
        assert!(matches!(
            probe_rpm_drift("copilot-shell", Some("2.2.0-1.al8"), &q),
            Some(RpmDrift::Drifted { .. })
        ));
    }

    /// The package gone from rpmdb (manual `rpm -e`) is Missing.
    #[test]
    fn probe_rpm_drift_detects_missing() {
        let q = FakeQuery::default();
        assert!(matches!(
            probe_rpm_drift("copilot-shell", Some("2.2.0-1.al8"), &q),
            Some(RpmDrift::Missing)
        ));
    }

    /// A same-name multi-version rpmdb is surfaced as drift, not a silent pass.
    #[test]
    fn probe_rpm_drift_multi_version_is_drifted() {
        let q = ErrQuery(PackageQueryError::UnexpectedOutput {
            command: "rpm".to_string(),
            detail: "2 installed versions".to_string(),
        });
        assert!(matches!(
            probe_rpm_drift("copilot-shell", Some("2.2.0-1.al8"), &q),
            Some(RpmDrift::Drifted { .. })
        ));
    }

    /// Missing rpm/dnf tooling cannot prove drift; the recorded status stands.
    #[test]
    fn probe_rpm_drift_tooling_missing_keeps_status() {
        let q = ErrQuery(PackageQueryError::CommandMissing {
            command: "rpm".to_string(),
        });
        assert!(probe_rpm_drift("copilot-shell", Some("2.2.0-1.al8"), &q).is_none());
    }

    /// The scoped status selector overrides an adopted rpm-observed row to
    /// `drifted` and records a `rpm:drift` health entry when rpmdb has moved on.
    #[test]
    fn select_components_from_view_overrides_rpm_status_to_drifted() {
        let mut state = InstalledState::default();
        state.upsert_object(rpm_observed_object(
            "copilot-shell",
            "copilot-shell",
            "2.2.0-1.al8",
        ));
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.3.0", "1.al8"),
            )],
            origins: Vec::new(),
        };
        let view = scoped_status_view(InstalledState::default(), state);
        let records =
            select_components_from_view_quiet(&view, Some("copilot-shell"), None, Some(&q));

        assert_eq!(records[0].status, "drifted");
        assert!(
            records[0]
                .health
                .iter()
                .any(|h| h.name == "rpm:drift" && h.status == "drifted"),
            "a rpm:drift health entry must be recorded",
        );
    }

    /// The scoped status selector overrides to `missing` when rpmdb no longer
    /// has the package (the `rpm -e` case must not be silently reinstalled).
    #[test]
    fn select_components_from_view_overrides_rpm_status_to_missing() {
        let mut state = InstalledState::default();
        state.upsert_object(rpm_observed_object(
            "copilot-shell",
            "copilot-shell",
            "2.2.0-1.al8",
        ));
        let q = FakeQuery::default();
        let view = scoped_status_view(InstalledState::default(), state);
        let records =
            select_components_from_view_quiet(&view, Some("copilot-shell"), None, Some(&q));

        assert_eq!(records[0].status, "missing");
        assert!(
            records[0]
                .health
                .iter()
                .any(|h| h.name == "rpm:drift" && h.status == "missing"),
        );
    }

    /// A raw component (no RPM metadata) is never touched by drift adjudication,
    /// even when the injected query would report the package absent.
    #[test]
    fn select_components_from_view_leaves_non_rpm_component_untouched() {
        let mut state = InstalledState::default();
        state.upsert_object(component_object(
            "agentsight",
            "0.1.0",
            ObjectStatus::Installed,
        ));
        let q = FakeQuery::default();
        let view = scoped_status_view(InstalledState::default(), state);
        let records = select_components_from_view_quiet(&view, Some("agentsight"), None, Some(&q));

        assert_eq!(records[0].status, "installed", "raw row must not drift");
        assert!(
            !records[0].health.iter().any(|h| h.name == "rpm:drift"),
            "no drift entry for a non-RPM component",
        );
    }

    /// Drift never demotes a record that integrity/manifest health already
    /// escalated past a clean projection: a `failed` RPM row keeps its
    /// more-severe status even when rpmdb has moved on, and gains no drift entry.
    #[test]
    fn select_components_from_view_keeps_escalated_rpm_status() {
        let mut state = InstalledState::default();
        let mut object = rpm_observed_object("copilot-shell", "copilot-shell", "2.2.0-1.al8");
        object.status = ObjectStatus::Failed;
        state.upsert_object(object);
        // rpmdb has drifted, but the failed status must survive untouched.
        let q = FakeQuery {
            installed: vec![(
                "copilot-shell".to_string(),
                pkg_info("copilot-shell", "2.3.0", "1.al8"),
            )],
            origins: Vec::new(),
        };
        let view = scoped_status_view(InstalledState::default(), state);
        let records =
            select_components_from_view_quiet(&view, Some("copilot-shell"), None, Some(&q));

        assert_eq!(
            records[0].status, "failed",
            "escalated status must not be demoted to drifted",
        );
        assert!(
            !records[0].health.iter().any(|h| h.name == "rpm:drift"),
            "no drift entry when the record is already escalated",
        );
    }

    // -----------------------------------------------------------------
    // Adapter observation projection over a real scan collector (#3310)
    //
    // The tests above feed the projection a hand-written `ScanEntry`. These
    // drive the real [`AdapterManager::scan`] collector over a staged fixture
    // and project its output, so a drift between what the collector emits and
    // what `component status` assumes fails here rather than in production.
    // -----------------------------------------------------------------

    const OBS_COMPONENT: &str = "observation-demo";
    const OBS_FRAMEWORK: &str = "cosh";

    const OBS_MANIFEST: &str = r#"[component]
name = "observation-demo"
version = "0.1.0"

[component.layout]
modes = ["system"]

[[adapters]]
framework = "cosh"
adapter_type = "extension"
source = "adapters/observation-demo/cosh"
dest = "{datadir}/adapters/{component}/cosh/"
"#;

    /// Stage a system-mode world whose installed state declares
    /// `OBS_COMPONENT` with a `cosh` extension adapter and a valid on-disk
    /// bundle, then return the tempdir (keeping the fixture alive), the layout,
    /// and the entries the real scan collector produced. With `receipt`, an
    /// enabled receipt for the same component/framework is planted first, so
    /// the row is receipt-backed instead of a bare candidate.
    ///
    /// The `cosh` driver needs no framework CLI for `scan`: detection is
    /// side-effect-free, so this fixture spawns nothing.
    fn stage_real_scan(receipt: bool) -> (tempfile::TempDir, FsLayout, Vec<ScanEntry>) {
        use anolisa_core::InstallMode as StateInstallMode;
        use anolisa_core::adapter::claim::{
            AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, CoshClaim,
            DRIVER_SCHEMA_VERSION, DriverPayload,
        };
        use anolisa_core::adapter::manager::AdapterManager;

        let root = tempfile::tempdir().expect("tempdir");
        let layout = test_layout(root.path());
        let resource_root = layout
            .datadir
            .join("adapters")
            .join(OBS_COMPONENT)
            .join(OBS_FRAMEWORK);
        std::fs::create_dir_all(&resource_root).expect("resource root");
        let marker = resource_root.join("cosh-extension.json");
        std::fs::write(
            &marker,
            format!(r#"{{"id":"{OBS_COMPONENT}","name":"Observation Demo"}}"#),
        )
        .expect("bundle marker");
        write_manifest_snapshot(&layout, OBS_COMPONENT, OBS_MANIFEST);

        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let extension_dir = home
            .join(".copilot-shell")
            .join("extensions")
            .join(OBS_COMPONENT);
        std::fs::create_dir_all(&extension_dir).expect("cosh extension dir");
        std::fs::write(
            extension_dir.join(".anolisa-adapter"),
            b"ANOLISA-managed cosh extension\n",
        )
        .expect("ownership marker");

        let mut component = component_object(OBS_COMPONENT, "0.1.0", ObjectStatus::Installed);
        component.files.push(OwnedFile {
            path: marker,
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        });
        let mut state = InstalledState {
            install_mode: StateInstallMode::System,
            prefix: layout.prefix.clone(),
            ..InstalledState::default()
        };
        state.objects.push(component);
        if receipt {
            state.adapter_claims.push(AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: OBS_COMPONENT.to_string(),
                framework: OBS_FRAMEWORK.to_string(),
                plugin_id: Some(OBS_COMPONENT.to_string()),
                adapter_type: Some("extension".to_string()),
                enabled_at: "2026-01-01T00:00:00Z".to_string(),
                resource_root: resource_root.clone(),
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
                        path: extension_dir,
                    },
                }],
                driver_payload: DriverPayload::Cosh(CoshClaim {
                    extension_dir_resource: "cosh_extension_dir".to_string(),
                }),
            });
        }
        std::fs::create_dir_all(&layout.state_dir).expect("state dir");
        state
            .save(&layout.state_dir.join("installed.toml"))
            .expect("save state");

        let manager = AdapterManager::new(layout.clone(), Some(home), "tester".to_string());
        let entries = manager.scan().expect("real scan").entries;
        (root, layout, entries)
    }

    /// The real scan row for the fixture component.
    fn obs_row(entries: &[ScanEntry]) -> &ScanEntry {
        entries
            .iter()
            .find(|entry| entry.component == OBS_COMPONENT && entry.framework == OBS_FRAMEWORK)
            .unwrap_or_else(|| {
                panic!(
                    "real scan produced no {OBS_COMPONENT}/{OBS_FRAMEWORK} row; got {:?}",
                    entries
                        .iter()
                        .map(|entry| (entry.component.clone(), entry.framework.clone()))
                        .collect::<Vec<_>>()
                )
            })
    }

    /// Unwrap `Present`, naming the evidence shape that arrived instead.
    fn present_observations(
        evidence: ProbeEvidence<Vec<AdapterObservation>, AdapterProvenance>,
    ) -> (AdapterProvenance, Vec<AdapterObservation>) {
        match evidence {
            ProbeEvidence::Present { provenance, value } => (provenance, value),
            ProbeEvidence::Absent { provenance } => {
                panic!("real scan rows must project as Present, got Absent {provenance:?}")
            }
            ProbeEvidence::Unavailable { reason, .. } => {
                panic!("a successful scan must not project as Unavailable: {reason}")
            }
            ProbeEvidence::NotRequested => panic!("the adapter probe was requested"),
        }
    }

    #[test]
    fn adapter_observation_projection_preserves_a_real_receipt_scan() {
        let (_root, layout, entries) = stage_real_scan(true);
        let row = obs_row(&entries);
        // Pre-condition: the collector really produced a receipt-backed row.
        assert!(row.declared, "the installed manifest declares the adapter");
        assert!(
            row.enabled,
            "the planted receipt makes the row receipt-backed"
        );
        assert_eq!(row.claim_status, Some(ClaimStatus::Enabled));
        assert_eq!(row.source_status, Some(AdapterSourceStatus::Available));

        let state_paths = vec![layout.state_dir.join("installed.toml")];
        let (provenance, observations) = present_observations(collect_adapter_observations(
            OBS_COMPONENT,
            &AdapterScanEvidence::Available(&entries),
            &state_paths,
        ));
        assert_eq!(provenance.state_paths, state_paths);
        assert_eq!(observations.len(), 1);

        // Field-by-field fidelity: the projection copies the collector's
        // verdict, it never re-derives or defaults one.
        let observation = &observations[0];
        assert_eq!(observation.component, row.component);
        assert_eq!(observation.framework, row.framework);
        assert_eq!(observation.declared, row.declared);
        assert_eq!(observation.resource_root, row.resource_root);
        assert_eq!(observation.driver_available, row.driver_available);
        assert_eq!(observation.framework_detected, row.framework_detected);
        assert_eq!(observation.adapter_type, row.adapter_type);
        assert_eq!(observation.enabled, row.enabled);
        assert_eq!(observation.claim_status, row.claim_status);
        assert_eq!(observation.source_reason, row.source_reason);
        assert_eq!(
            observation.source_status,
            Some(AdapterSourceSnapshot::Available),
            "the manager's source verdict maps 1:1 onto the stable snapshot"
        );
    }

    #[test]
    fn adapter_observation_projection_keeps_a_real_candidate_row_source_free() {
        let (_root, layout, entries) = stage_real_scan(false);
        let row = obs_row(&entries);
        assert!(row.declared, "the installed manifest declares the adapter");
        assert!(!row.enabled, "no receipt was planted");

        let state_paths = vec![layout.state_dir.join("installed.toml")];
        let (_, observations) = present_observations(collect_adapter_observations(
            OBS_COMPONENT,
            &AdapterScanEvidence::Available(&entries),
            &state_paths,
        ));
        assert_eq!(observations.len(), 1);
        let observation = &observations[0];
        assert!(observation.declared);
        assert!(!observation.enabled);
        assert_eq!(observation.claim_status, None);
        assert_eq!(
            observation.source_status, None,
            "a candidate row must not gain a source verdict in projection"
        );
        assert_eq!(observation.source_reason, None);
        assert_eq!(observation.resource_root, row.resource_root);
    }

    #[test]
    fn adapter_observation_projection_reports_absent_for_an_unrelated_component() {
        let (_root, layout, entries) = stage_real_scan(true);
        assert!(!entries.is_empty(), "pre-condition: the fixture has a row");

        let state_paths = vec![layout.state_dir.join("installed.toml")];
        // A component the real scan never saw is confirmed Absent — the
        // collector was consulted and established absence — rather than being
        // projected as an empty Present value.
        match collect_adapter_observations(
            "no-such-component",
            &AdapterScanEvidence::Available(&entries),
            &state_paths,
        ) {
            ProbeEvidence::Absent { provenance } => {
                assert_eq!(provenance.state_paths, state_paths);
            }
            ProbeEvidence::Present { value, .. } => {
                panic!("an unrelated component must not project rows: {value:?}")
            }
            ProbeEvidence::Unavailable { reason, .. } => {
                panic!("a successful scan must not be Unavailable: {reason}")
            }
            ProbeEvidence::NotRequested => panic!("the adapter probe was requested"),
        }
    }
}
