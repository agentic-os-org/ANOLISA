//! `anolisa update --check` — read-only RPM upgrade detection (issue #1410).
//!
//! Produces a report answering "can the installed toolchain be upgraded?" for
//! the RPM / system-image scenario. It is strictly read-only: it inspects the
//! host only through [`PackageQuery`], i.e. read-only `rpm -q` / `dnf repoquery`
//! lookups. It runs **no mutating package operation** (never a
//! [`PackageTransaction`](anolisa_platform::pkg_transaction::PackageTransaction),
//! so no `dnf install/update/remove`), never writes `installed.toml`, and never
//! persists repo/adapter state. Note the repo candidate lookup still calls
//! `dnf repoquery`, which touches the network like any read query — so the MOTD
//! path is cache-backed to keep that cost off the login hot path. Applying
//! upgrades is a separate command (`anolisa upgrade`, issue #1411).
//!
//! Three sources feed the report:
//! - **CLI**: whether the running `anolisa` binary is RPM-owned and has a newer
//!   repo candidate. A non-RPM binary is `unsupported` here (self-update is
//!   handled by `anolisa update self`).
//! - **Installed components**: each `rpm-managed` / `rpm-observed` component is
//!   compared against its repo candidates; `raw-managed` components are reported
//!   `unsupported_in_rpm_upgrade` and never touched.
//! - **Target profile** (`--target`, optional): default components the profile
//!   declares but that are not installed are reported as installable. When
//!   `--target` is omitted the release-owned default profile
//!   ([`DEFAULT_TARGET_PROFILE_NAME`]) is used, so a plain check still surfaces
//!   missing default components.
//!
//! Repo candidate ordering uses RPM's own EVR comparison
//! ([`rpm_evr_cmp`]), not semver, so real epochs/releases are ordered
//! correctly. A small on-disk cache under
//! `cache_dir/update-check.json` (keyed by the resolved target) backs the
//! low-noise MOTD path; `--refresh` bypasses it.
//!
//! Rendering lives in [`render`]; the cache and target-profile plumbing plus the
//! read-only repo-config load live here alongside the detection logic.

mod render;

#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use anolisa_core::domain::{Installation, ManagementRelation, ProviderBinding};
use anolisa_core::self_update;
use anolisa_core::state::ObjectKind;
use anolisa_core::{
    ComponentSnapshot, NativePackageProvenance, NativePackageSnapshot, ProbeEvidence, StateSnapshot,
};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError, PackageVersion, rpm_evr_cmp};
use anolisa_platform::rpm_query::RpmPackageQuery;

use super::UpdateArgs;
use crate::commands::common;
use crate::commands::state_view::{ScopedInstalledObject, StateView, StateVisibility};
use crate::commands::tier1::component_observation::{self, AggregateSelection};
use crate::context::CliContext;
use crate::progress;
use crate::repo_config::{BackendConfig, RepoConfig};
use crate::resolution::{
    BackendKind, ComponentIndex, ComponentResolver, IndexIdentity, ResolutionSet, ResolveOptions,
    resolve_index_identity, rpm_component_provide,
};
use crate::response::CliError;

/// Command label for JSON envelopes and error routing on the check path.
pub(super) const CHECK_COMMAND: &str = "update --check";

/// Filename for the cached report under the layout's `cache_dir`.
const CACHE_FILE: &str = "update-check.json";

/// How long a cached report is considered fresh for the MOTD path. Off the
/// round hour so scheduled MOTD refreshes across hosts do not all land at once.
const CACHE_TTL_SECS: i64 = 6 * 3600 + 137;

/// Release-owned default target profile evaluated when `--target` is omitted, so
/// a plain `update --check` (and the MOTD path) can report missing defaults.
const DEFAULT_TARGET_PROFILE_NAME: &str = "agentic_os-latest";

/// Built-in copy of the default target profile, compiled in as the final
/// fallback so the default check works even before any profile file is laid down
/// on disk. On-disk copies (`<etc>/profiles`, `<datadir>/profiles`) still win.
const BUILTIN_DEFAULT_TARGET_PROFILE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../profiles/agentic_os-latest.toml"
));

/// Subdirectory under `etc_dir` / packaged datadir holding target profiles.
const PROFILES_SUBDIR: &str = "profiles";

// Stable action vocabulary shared by JSON, cache, and human/MOTD rendering.
// `pub(crate)` so the `upgrade` command (issue #1411) can classify a report's
// items against the same vocabulary the check produces.
pub(crate) const ACTION_UPDATE: &str = "update";
pub(crate) const ACTION_NOOP: &str = "noop";
pub(crate) const ACTION_RECONCILE: &str = "reconcile";
pub(crate) const ACTION_INSTALL: &str = "install";
pub(crate) const ACTION_UNSUPPORTED: &str = "unsupported";
pub(crate) const ACTION_UNSUPPORTED_RPM: &str = "unsupported_in_rpm_upgrade";
pub(crate) const ACTION_ERROR: &str = "error";

/// Wire shape for `update --check` (`--json`) and the on-disk cache.
///
/// Owned `String`s (rather than `&'static str`) so the same struct round-trips
/// through the cache via `Deserialize`. Exposed `pub(crate)` so `anolisa
/// upgrade` (issue #1411) can consume the same planning output; only the fields
/// the upgrade planner reads are widened past module scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UpdateCheckReport {
    /// Target profile evaluated, when `--target` was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<String>,
    /// Upgrade backend this report covers. Always `rpm` in the first version.
    backend: String,
    /// True when at least one component/CLI `update` was found. Deliberately
    /// narrow: a missing default is an *install*, not an upgrade, so it does not
    /// set this — see [`action_required`](Self::action_required).
    upgrade_available: bool,
    /// True when there is anything to do: an upgrade, a missing default to
    /// install, or RPM state to reconcile. This is the signal machine callers
    /// should gate on when driving `anolisa upgrade`; `upgrade_available` alone
    /// intentionally covers only package version updates.
    action_required: bool,
    pub(crate) cli: CliCheck,
    pub(crate) components: Vec<ComponentCheck>,
    summary: CheckSummary,
}

/// Upgrade status of the running `anolisa` CLI binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CliCheck {
    /// RPM package that owns the CLI binary; `None` when it is not RPM-owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) package: Option<String>,
    /// Installed EVR from rpmdb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) installed: Option<String>,
    /// Newest repo candidate EVR, when an upgrade is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) available: Option<String>,
    pub(crate) action: String,
    /// Item-level failure that did not abort the whole check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

/// Upgrade status of one installed (or profile-declared) component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ComponentCheck {
    pub(crate) component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) package: Option<String>,
    /// Provenance label (`rpm-managed` / `rpm-observed` / `raw-managed`);
    /// `None` for a profile default that is not installed yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ownership: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) installed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) available: Option<String>,
    pub(crate) action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    /// Internal planner hint: this row came from a target-profile default that was
    /// absent from ANOLISA state. It is intentionally not part of JSON/cache.
    #[serde(skip)]
    pub(crate) absent_from_state: bool,
    /// Internal planner hint: the RPM package was resolved for a legacy state
    /// row whose package metadata needs to be backfilled by `upgrade`.
    #[serde(skip)]
    pub(crate) backfill_rpm_metadata: bool,
}

/// Aggregate counts used by the summary line, MOTD, and exit signalling.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CheckSummary {
    /// Upgrades found (CLI plus components).
    updates: usize,
    /// Legacy RPM rows whose resolved package metadata needs reconciliation.
    /// Defaults to zero when reading caches written before this field existed.
    #[serde(default)]
    reconciliations: usize,
    /// Profile default components absent from state.
    missing_defaults: usize,
    /// Items outside the RPM upgrade scope (raw-managed, non-RPM CLI).
    unsupported: usize,
    /// Item-level query failures that did not abort the check.
    errors: usize,
}

struct CheckedComponent {
    snapshot: ComponentSnapshot,
    report: ComponentCheck,
}

impl CheckedComponent {
    fn into_report(self) -> ComponentCheck {
        let Self {
            snapshot: _snapshot,
            report,
        } = self;
        report
    }
}

/// Cache envelope stored under `cache_dir/update-check.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateCheckCache {
    /// RFC3339 UTC timestamp used to decide cache freshness.
    generated_at: String,
    report: UpdateCheckReport,
}

/// Minimal target-profile reader.
///
/// The repository-side profile schema is not finalized, so this deliberately
/// reads only the one field the check needs (`default_components`) and tolerates
/// any other keys, letting a richer schema land later without breaking callers.
#[derive(Debug, Clone, Default, Deserialize)]
struct TargetProfile {
    #[serde(default)]
    default_components: Vec<String>,
}

impl TargetProfile {
    fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

/// Read-only inputs for [`run_update_check`]; injected so tests drive the whole
/// report without a live rpmdb/dnf.
struct CheckInputs<'a> {
    view: &'a StateView,
    query: &'a dyn PackageQuery,
    /// Path of the running executable, used to find its owning RPM.
    cli_exe_path: &'a str,
    /// Installed architecture used to filter repo candidates.
    arch: &'a str,
    /// Echoed into the report's `target` field.
    target_name: Option<String>,
    /// Loaded profile, when `--target` was given and resolved.
    target: Option<TargetProfile>,
    /// Repo-side component identity index, used to map a profile default's
    /// component name to its RPM package so an installed-but-unadopted default is
    /// not falsely reported as installable. `None` when the index is unavailable
    /// (best-effort); the check then relies on the `anolisa-component(...)`
    /// provide alone.
    component_index: Option<&'a ComponentIndex>,
    /// RPM backend config used for missing-default package resolution.
    rpm_backend: Option<&'a BackendConfig>,
}

/// Production entry point for `anolisa update --check`.
pub(super) fn handle_update_check(args: &UpdateArgs, ctx: &CliContext) -> Result<(), CliError> {
    handle_update_check_with(args, ctx, Utc::now, compute_update_check_report)
}

fn handle_update_check_with<N, C>(
    args: &UpdateArgs,
    ctx: &CliContext,
    mut now: N,
    compute: C,
) -> Result<(), CliError>
where
    N: FnMut() -> chrono::DateTime<Utc>,
    C: FnOnce(Option<&str>, &CliContext, &FsLayout) -> Result<UpdateCheckReport, CliError>,
{
    // `update --check` only understands the system / RPM-image scenario: it
    // reasons about rpm-owned components and repo candidates. In user mode there
    // is no rpmdb-backed toolchain to reason about, so refuse explicitly rather
    // than emit a misleading "nothing to upgrade" RPM report. The MOTD path
    // stays silent (a login banner must not error) — this runs before any cache
    // lookup or rpm/dnf query.
    if ctx.install_mode != crate::context::InstallMode::System {
        if args.motd {
            return Ok(());
        }
        return Err(CliError::InvalidArgument {
            command: CHECK_COMMAND.to_string(),
            reason: "`update --check` currently supports only system/RPM image scenarios; run without `--install-mode user`".to_string(),
        });
    }

    let layout = common::resolve_layout(ctx);
    let cache_path = cache_path(&layout);

    // MOTD fast path: prefer a fresh cache for the *same* target so the login
    // hook stays cheap and never blocks on the network. JSON output always
    // recomputes so machine callers get the full, current envelope.
    if args.motd
        && !args.refresh
        && !ctx.json
        && let Some(cache) = read_cache(&cache_path)
        && cache_is_usable(
            &cache,
            Some(effective_target_name(args.target.as_deref())),
            &mut now,
        )
    {
        render::render_motd(ctx, &cache.report);
        return Ok(());
    }

    // Show activity while the (potentially blocking) repo queries run, so a slow
    // repository does not look like a hung process. The MOTD path stays
    // low-noise and never animates. The guard cleans up on every exit path via
    // RAII, including the error return below.
    let feedback = if args.motd {
        progress::FeedbackMode::Disabled
    } else {
        progress::feedback_for_stderr(ctx.json, ctx.quiet)
    };
    let report = {
        let _activity = progress::Activity::start(feedback, "Checking for updates...");
        match compute(args.target.as_deref(), ctx, &layout) {
            Ok(report) => report,
            Err(err) => {
                // MOTD must stay quiet and low-noise on failure; a JSON/human
                // check surfaces the error as usual.
                if args.motd {
                    return Ok(());
                }
                return Err(err);
            }
        }
    };

    // The cache is not authoritative state; a write failure (e.g. a non-root
    // MOTD probe against a root-owned cache dir) is non-fatal.
    let _ = write_cache(&cache_path, &report, &mut now);

    render::render_report(ctx, args, &report);
    Ok(())
}

/// Build the report from live host state (repo config, rpmdb/dnf, target
/// profile). Split from [`run_update_check`] so the pure logic stays testable.
///
/// This is the shared, read-only planner behind both `anolisa update --check`
/// and `anolisa upgrade` (issue #1411): it runs only read-only rpm/dnf queries,
/// never a [`PackageTransaction`](anolisa_platform::pkg_transaction::PackageTransaction),
/// never writes `installed.toml`, and never persists repo/adapter config. The
/// `upgrade` command consumes the returned report and converts it into an RPM
/// transaction plan; it does not re-derive the plan. Takes the resolved
/// `--target` value directly (rather than the whole `UpdateArgs`) so callers
/// without an `UpdateArgs` can reuse it.
pub(crate) fn compute_update_check_report(
    target: Option<&str>,
    ctx: &CliContext,
    layout: &FsLayout,
) -> Result<UpdateCheckReport, CliError> {
    let repo_config = load_repo_config_read_only(ctx, layout)?;
    let env = anolisa_env::EnvService::detect();
    let repo = super::rpm_repo_source_for_update(&repo_config, &env, CHECK_COMMAND)?.ok_or_else(
        || CliError::InvalidArgument {
            command: CHECK_COMMAND.to_string(),
            reason: "repo.toml has no [backends.rpm] table; `update --check` needs the configured ANOLISA RPM repository to look up upgrade candidates".to_string(),
        },
    )?;
    let query = RpmPackageQuery::system_with_repo(repo);
    let installed = common::load_state_store(ctx, CHECK_COMMAND)?;
    let view = StateView::load_with_writable_state(
        ctx,
        CHECK_COMMAND,
        StateVisibility::UserPlusSystem,
        installed,
    )?;

    let exe = self_update::resolve_current_exe().map_err(|err| CliError::Runtime {
        command: CHECK_COMMAND.to_string(),
        reason: format!("cannot resolve the running executable: {err}"),
    })?;
    let exe_path = exe
        .to_str()
        .ok_or_else(|| CliError::Runtime {
            command: CHECK_COMMAND.to_string(),
            reason: format!(
                "running executable path is not valid UTF-8: {}",
                exe.display()
            ),
        })?
        .to_string();

    // An omitted `--target` resolves to the release default profile, so the
    // report always carries a target and can surface missing defaults.
    let (target_name, target_profile) =
        load_effective_target_profile(layout, target, ctx.packaged_data_probe())?;

    // Best-effort component identity index so a profile default already present
    // on the host (but not adopted into ANOLISA state) is checked against rpmdb
    // via its package name rather than reported as installable. Loaded only on
    // this (uncached) path, so the MOTD fast path is unaffected; a missing index
    // is non-fatal (the check falls back to the `anolisa-component(...)` provide).
    let component_index =
        crate::resolution::load_optional_component_index(layout, &env, &repo_config);

    run_update_check(CheckInputs {
        view: &view,
        query: &query,
        cli_exe_path: &exe_path,
        arch: &env.arch,
        target_name: Some(target_name),
        target: Some(target_profile),
        component_index: component_index.as_ref(),
        rpm_backend: repo_config.backends.get("rpm"),
    })
}

/// Load repo config without ever writing it, keeping `--check` read-only.
///
/// The dry-run load path fetches a missing config into memory only (never
/// persisting `<etc_dir>/repo.toml`), which is exactly the read-only guarantee
/// the check needs even on a host that has not provisioned repo config yet. The
/// no-write behaviour of the dry-run path is covered by
/// `repo_config::tests::load_dry_run_fetches_without_writing`.
fn load_repo_config_read_only(ctx: &CliContext, layout: &FsLayout) -> Result<RepoConfig, CliError> {
    RepoConfig::load(layout, true, ctx.packaged_data_probe())
        .map(|loaded| loaded.config)
        .map_err(|err| CliError::Runtime {
            command: CHECK_COMMAND.to_string(),
            reason: format!("failed to load repo config: {err}"),
        })
}

/// Pure report builder over the injected read-only inputs.
fn run_update_check(inputs: CheckInputs<'_>) -> Result<UpdateCheckReport, CliError> {
    let mut summary = CheckSummary::default();

    let cli = build_cli_check(inputs.query, inputs.cli_exe_path, inputs.arch, &mut summary);

    let mut components = Vec::new();
    let observed_at = super::now_iso8601();
    for record in component_observation::select_visible_components(
        inputs.view,
        None,
        AggregateSelection::ActiveOnly,
    ) {
        components.push(
            check_component(
                inputs.query,
                inputs.component_index,
                inputs.rpm_backend,
                &record,
                inputs.arch,
                &observed_at,
                &mut summary,
            )?
            .into_report(),
        );
    }

    // Profile defaults absent from ANOLISA state are surfaced as a gap (issue
    // #1411 performs the install). The default's canonical identity comes
    // first (issue #2630): a name absent from state is a new identity that
    // only the component index may authorize, and comparing against state and
    // the other defaults under the canonical name keeps an alias default from
    // duplicating an already-tracked component as absent. Absence from
    // `installed.toml` is still not the same as absence from the host, so each
    // remaining candidate is cross-checked against rpmdb — a default already
    // installed as an RPM (but never adopted) is evaluated for upgrades
    // instead of falsely reported as missing.
    if let Some(profile) = &inputs.target {
        let mut seen_defaults = std::collections::BTreeSet::new();
        for name in &profile.default_components {
            // An exact state identity precedes index normalization
            // (issue #2630): a record tracked under the literal default name
            // was already checked by the installed loop above, and must not
            // be re-normalized into a second identity — or, with no index,
            // into a spurious validation error.
            if inputs
                .view
                .writable
                .state
                .find(ObjectKind::Component, name)
                .is_some()
            {
                seen_defaults.insert(name.clone());
                continue;
            }
            let canonical = match resolve_index_identity(name, inputs.component_index) {
                IndexIdentity::Resolved(component) => component,
                IndexIdentity::Unsupported => {
                    components.push(default_component_error(
                        name,
                        None,
                        format!(
                            "unsupported component '{name}' in target-profile defaults; the component index has no entry for it"
                        ),
                        &mut summary,
                    ));
                    continue;
                }
                IndexIdentity::Unavailable => {
                    components.push(default_component_error(
                        name,
                        None,
                        format!(
                            "component index is unavailable; cannot validate target-profile default '{name}'"
                        ),
                        &mut summary,
                    ));
                    continue;
                }
            };
            if !seen_defaults.insert(canonical.clone())
                || inputs
                    .view
                    .writable
                    .state
                    .find(ObjectKind::Component, &canonical)
                    .is_some()
            {
                continue;
            }
            components.push(check_default_component(
                inputs.query,
                inputs.component_index,
                inputs.rpm_backend,
                &canonical,
                inputs.arch,
                &mut summary,
            ));
        }
    }

    let upgrade_available = summary.updates > 0;
    let action_required =
        upgrade_available || summary.reconciliations > 0 || summary.missing_defaults > 0;
    Ok(UpdateCheckReport {
        target: inputs.target_name,
        backend: "rpm".to_string(),
        upgrade_available,
        action_required,
        cli,
        components,
        summary,
    })
}

/// Determine whether the running CLI binary is RPM-owned and, if so, whether a
/// newer repo candidate exists.
fn build_cli_check(
    query: &dyn PackageQuery,
    exe_path: &str,
    arch: &str,
    summary: &mut CheckSummary,
) -> CliCheck {
    let providers = match query.what_provides_installed(exe_path) {
        Ok(providers) => providers,
        // No rpm/dnf on the host: the CLI cannot be shown RPM-owned, so treat it
        // as out of RPM upgrade scope rather than a hard failure.
        Err(PackageQueryError::CommandMissing { .. }) => {
            summary.unsupported += 1;
            return cli_unsupported("rpm/dnf not found; cannot determine CLI package ownership");
        }
        Err(err) => {
            summary.errors += 1;
            return cli_error(
                None,
                format!("cannot determine CLI package ownership: {err}"),
            );
        }
    };

    let package = match providers.as_slice() {
        [] => {
            summary.unsupported += 1;
            return cli_unsupported(
                "anolisa CLI is not RPM-owned; use `anolisa update self` to update the binary",
            );
        }
        [only] => only.clone(),
        _ => {
            summary.errors += 1;
            return cli_error(
                None,
                format!(
                    "CLI executable is provided by multiple RPM packages ({})",
                    providers.join(", ")
                ),
            );
        }
    };

    let installed = match query.query_installed(&package) {
        Ok(Some(info)) => info.version,
        Ok(None) => {
            summary.errors += 1;
            return cli_error(
                Some(package),
                "CLI package is reported as owner but is absent from rpmdb".to_string(),
            );
        }
        Err(PackageQueryError::UnexpectedOutput { .. }) => {
            summary.errors += 1;
            return cli_error(
                Some(package),
                "CLI package has multiple installed versions in rpmdb".to_string(),
            );
        }
        Err(err) => {
            summary.errors += 1;
            return cli_error(Some(package), format!("rpm query failed: {err}"));
        }
    };
    let installed_evr = installed.to_string();

    match repo_upgrade(query, &package, arch, &installed) {
        Ok(Some(available)) => {
            summary.updates += 1;
            CliCheck {
                package: Some(package),
                installed: Some(installed_evr),
                available: Some(available),
                action: ACTION_UPDATE.to_string(),
                error: None,
            }
        }
        Ok(None) => CliCheck {
            package: Some(package),
            installed: Some(installed_evr),
            available: None,
            action: ACTION_NOOP.to_string(),
            error: None,
        },
        Err(err) => {
            summary.errors += 1;
            cli_error(Some(package), format!("repo candidate query failed: {err}"))
        }
    }
}

/// Evaluate one installed component against the RPM upgrade scope.
fn check_component(
    query: &dyn PackageQuery,
    component_index: Option<&ComponentIndex>,
    rpm_backend: Option<&BackendConfig>,
    record: &ScopedInstalledObject<'_>,
    arch: &str,
    observed_at: &str,
    summary: &mut CheckSummary,
) -> Result<CheckedComponent, CliError> {
    let state_snapshot = update_check_snapshot(record, ProbeEvidence::NotRequested)?;
    let installation = active_snapshot_installation(&state_snapshot)?;
    let component = installation.name.clone();

    // Owned components are explicitly out of the RPM upgrade path. Nothing is
    // queried, touched, or migrated — only reported.
    let (manager, identity, relation, last_observed) = match &installation.binding {
        ProviderBinding::Owned { artifact } => {
            summary.unsupported += 1;
            let report = ComponentCheck {
                component,
                package: artifact.raw_package.clone(),
                ownership: Some("owned".to_string()),
                installed: Some(artifact.version.clone()),
                available: None,
                action: ACTION_UNSUPPORTED_RPM.to_string(),
                error: None,
                absent_from_state: false,
                backfill_rpm_metadata: false,
            };
            return Ok(CheckedComponent {
                snapshot: state_snapshot,
                report,
            });
        }
        ProviderBinding::Delegated {
            pm,
            package,
            relation,
            last_observed,
            ..
        } => (*pm, package, relation, last_observed),
    };

    let ownership_label = relation.label().to_string();
    // Recorded version for error rows, where rpmdb could not be consulted:
    // the observation cache is the best (stale) display value available.
    let recorded_version = last_observed
        .as_ref()
        .map(|obs| obs.evr.clone().unwrap_or_else(|| obs.version.clone()));
    let (package, backfill_rpm_metadata) = match identity
        .resolved_name()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(package) => (package.to_string(), false),
        None => {
            match resolve_legacy_component_package(query, component_index, rpm_backend, &component)
            {
                Ok(package) => (package, true),
                Err(reason) => {
                    summary.errors += 1;
                    return Ok(CheckedComponent {
                        snapshot: state_snapshot,
                        report: component_error(
                            component,
                            None,
                            ownership_label,
                            recorded_version,
                            reason,
                        ),
                    });
                }
            }
        }
    };

    let native =
        component_observation::observe_native_package(manager, &package, query, observed_at);
    let installed_version = native.installed_version;
    let query_error = native.query_error;
    let snapshot = update_check_snapshot(record, native.evidence)?;
    let installed = match snapshot.native_package() {
        ProbeEvidence::Present {
            value: NativePackageSnapshot::Installed(observation),
            ..
        } => {
            let version = installed_version.ok_or_else(|| CliError::Runtime {
                command: CHECK_COMMAND.to_string(),
                reason: format!(
                    "component snapshot for '{}' lost the structured native package version",
                    snapshot.request().component()
                ),
            })?;
            (
                version,
                observation
                    .evr
                    .clone()
                    .unwrap_or_else(|| observation.version.clone()),
            )
        }
        // rpmdb no longer has the package (e.g. removed with `rpm -e`): item
        // error, not a crash — the rest of the check continues.
        ProbeEvidence::Absent { .. } => {
            summary.errors += 1;
            return Ok(CheckedComponent {
                snapshot,
                report: component_error(
                    component,
                    Some(package),
                    ownership_label,
                    recorded_version.clone(),
                    "package recorded in ANOLISA state is not present in rpmdb; run `anolisa forget` or reinstall".to_string(),
                ),
            });
        }
        ProbeEvidence::Present {
            value:
                NativePackageSnapshot::MultipleVersions | NativePackageSnapshot::UnexpectedOutput { .. },
            ..
        } => {
            summary.errors += 1;
            return Ok(CheckedComponent {
                snapshot,
                report: component_error(
                    component,
                    Some(package),
                    ownership_label,
                    recorded_version.clone(),
                    "rpmdb reports multiple installed versions for this package".to_string(),
                ),
            });
        }
        ProbeEvidence::Unavailable { .. }
            if matches!(query_error, Some(PackageQueryError::CommandMissing { .. })) =>
        {
            summary.errors += 1;
            return Ok(CheckedComponent {
                snapshot,
                report: component_error(
                    component,
                    Some(package),
                    ownership_label,
                    recorded_version.clone(),
                    "rpm/dnf not found; cannot query the installed version".to_string(),
                ),
            });
        }
        ProbeEvidence::Unavailable { reason, .. } => {
            summary.errors += 1;
            let reason = reason.clone();
            return Ok(CheckedComponent {
                snapshot,
                report: component_error(
                    component,
                    Some(package),
                    ownership_label,
                    recorded_version.clone(),
                    format!("rpm query failed: {reason}"),
                ),
            });
        }
        ProbeEvidence::NotRequested => {
            return Err(CliError::Runtime {
                command: CHECK_COMMAND.to_string(),
                reason: format!(
                    "component snapshot for '{}' did not request native package evidence",
                    snapshot.request().component()
                ),
            });
        }
    };
    let installed_evr = installed.1;

    let report = match repo_upgrade(query, &package, arch, &installed.0) {
        Ok(Some(available)) => {
            summary.updates += 1;
            ComponentCheck {
                component,
                package: Some(package),
                ownership: Some(ownership_label),
                installed: Some(installed_evr),
                available: Some(available),
                action: ACTION_UPDATE.to_string(),
                error: None,
                absent_from_state: false,
                backfill_rpm_metadata,
            }
        }
        Ok(None) => {
            let action = if backfill_rpm_metadata {
                summary.reconciliations += 1;
                ACTION_RECONCILE
            } else {
                ACTION_NOOP
            };
            ComponentCheck {
                component,
                package: Some(package),
                ownership: Some(ownership_label),
                installed: Some(installed_evr),
                available: None,
                action: action.to_string(),
                error: None,
                absent_from_state: false,
                backfill_rpm_metadata,
            }
        }
        Err(err) => {
            summary.errors += 1;
            component_error(
                component,
                Some(package),
                ownership_label,
                Some(installed_evr),
                format!("repo candidate query failed: {err}"),
            )
        }
    };
    Ok(CheckedComponent { snapshot, report })
}

#[cfg(test)]
/// Returns the snapshot and report produced by one production component check.
pub(in crate::commands::tier1) fn checked_component_for_conformance(
    record: &ScopedInstalledObject<'_>,
    query: &dyn PackageQuery,
    observed_at: &str,
) -> Result<(ComponentSnapshot, ComponentCheck), CliError> {
    let checked = check_component(
        query,
        None,
        None,
        record,
        "x86_64",
        observed_at,
        &mut CheckSummary::default(),
    )?;
    Ok((checked.snapshot, checked.report))
}

#[cfg(test)]
/// Assembles update-check evidence supplied directly by a conformance fixture.
pub(in crate::commands::tier1) fn snapshot_with_native_for_conformance(
    record: &ScopedInstalledObject<'_>,
    native_package: ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
) -> Result<ComponentSnapshot, CliError> {
    update_check_snapshot(record, native_package)
}

fn update_check_snapshot(
    record: &ScopedInstalledObject<'_>,
    native_package: ProbeEvidence<NativePackageSnapshot, NativePackageProvenance>,
) -> Result<ComponentSnapshot, CliError> {
    component_observation::snapshot_from_record(
        record,
        ProbeEvidence::NotRequested,
        native_package,
        ProbeEvidence::NotRequested,
        ProbeEvidence::NotRequested,
        ProbeEvidence::NotRequested,
    )
    .map_err(|error| CliError::Runtime {
        command: CHECK_COMMAND.to_string(),
        reason: format!("failed to assemble component snapshot: {error}"),
    })
}

fn active_snapshot_installation(snapshot: &ComponentSnapshot) -> Result<&Installation, CliError> {
    match snapshot.state() {
        ProbeEvidence::Present {
            value: StateSnapshot::Active(installation),
            ..
        } => Ok(installation),
        ProbeEvidence::Absent { .. }
        | ProbeEvidence::Unavailable { .. }
        | ProbeEvidence::NotRequested
        | ProbeEvidence::Present {
            value: StateSnapshot::Quarantined(_),
            ..
        } => Err(CliError::Runtime {
            command: CHECK_COMMAND.to_string(),
            reason: format!(
                "component snapshot for '{}' does not contain active state",
                snapshot.request().component()
            ),
        }),
    }
}

/// Item error for a target-profile default, counted into the summary.
fn default_component_error(
    name: &str,
    package: Option<String>,
    reason: String,
    summary: &mut CheckSummary,
) -> ComponentCheck {
    summary.errors += 1;
    ComponentCheck {
        component: name.to_string(),
        package,
        ownership: None,
        installed: None,
        available: None,
        action: ACTION_ERROR.to_string(),
        error: Some(reason),
        absent_from_state: true,
        backfill_rpm_metadata: false,
    }
}

/// Evaluate a profile default that is absent from ANOLISA state.
///
/// `name` is the default's canonical identity — the caller validated it
/// against the component index and deduplicated it against state before
/// this evaluation runs (issue #2630).
///
/// A supported default missing from `installed.toml` may still be installed
/// on the host — e.g. baked into a system image and never adopted, which is
/// common for `legacy_adopt` packages predating the `anolisa-component(...)`
/// provide. Reporting such a default as `install` would be a false positive
/// on the default-profile MOTD, so rpmdb is consulted first (via the
/// component provide and its index-mapped package name). A
/// present-but-unadopted default is checked for upgrades like an rpm-observed
/// component; only a default genuinely absent from rpmdb too is reported
/// installable.
fn check_default_component(
    query: &dyn PackageQuery,
    index: Option<&ComponentIndex>,
    rpm_backend: Option<&BackendConfig>,
    name: &str,
    arch: &str,
    summary: &mut CheckSummary,
) -> ComponentCheck {
    match probe_default_package(query, index, name) {
        // Present on the host but unadopted: evaluate for upgrades.
        DefaultProbe::Installed(package) => {
            check_present_default(query, name, &package, arch, summary)
        }
        // Genuinely absent from both ANOLISA state and rpmdb.
        DefaultProbe::Missing => {
            // Resolve the RPM package name using the shared install resolver so
            // package_map and available Provides fallbacks stay available.
            // Missing/ambiguous resolution is an item error: otherwise the check
            // would claim an installable action that `upgrade` cannot apply.
            let package = match resolve_default_install_package(query, index, rpm_backend, name) {
                Ok(package) => package,
                Err(reason) => {
                    return default_component_error(name, None, reason, summary);
                }
            };
            match query.query_installed(&package) {
                Ok(Some(_)) => {
                    return check_present_default(query, name, &package, arch, summary);
                }
                Ok(None) => {}
                Err(err) => {
                    let reason = format!(
                        "cannot query installed version of resolved package for default component '{name}': {err}"
                    );
                    return default_component_error(name, Some(package), reason, summary);
                }
            }
            summary.missing_defaults += 1;
            ComponentCheck {
                component: name.to_string(),
                package: Some(package),
                ownership: None,
                installed: None,
                available: None,
                action: ACTION_INSTALL.to_string(),
                error: None,
                absent_from_state: true,
                backfill_rpm_metadata: false,
            }
        }
        // Could not determine presence (query failure, ambiguous providers).
        // This is an item error, not a missing default — reporting "install"
        // here would be the false positive the rpmdb cross-check exists to
        // avoid.
        DefaultProbe::Indeterminate(reason) => default_component_error(name, None, reason, summary),
    }
}

/// Resolve the package to install for a genuinely missing target-profile default.
///
/// This delegates to the shared RPM resolver instead of using only the component
/// index, preserving site-local package_map and repo-side available-provides
/// fallbacks for `anolisa upgrade`.
fn resolve_default_install_package(
    query: &dyn PackageQuery,
    index: Option<&ComponentIndex>,
    rpm_backend: Option<&BackendConfig>,
    name: &str,
) -> Result<String, String> {
    let resolver = ComponentResolver::new(index, rpm_backend, Some(query));
    match resolver.resolve(name, BackendKind::Rpm, ResolveOptions::default()) {
        Ok(ResolutionSet::Unique(target)) => Ok(target.package),
        Ok(ResolutionSet::None) => Err(format!(
            "cannot resolve RPM package for default component '{name}': no package mapping or provider found"
        )),
        Ok(ResolutionSet::Ambiguous(targets)) => {
            let packages = targets
                .iter()
                .map(|target| target.package.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "cannot resolve RPM package for default component '{name}': multiple candidates ({packages})"
            ))
        }
        Err(err) => Err(format!(
            "cannot resolve RPM package for default component '{name}': {err}"
        )),
    }
}

fn resolve_legacy_component_package(
    query: &dyn PackageQuery,
    index: Option<&ComponentIndex>,
    rpm_backend: Option<&BackendConfig>,
    name: &str,
) -> Result<String, String> {
    let resolver = ComponentResolver::new(index, rpm_backend, Some(query));
    match resolver.resolve(name, BackendKind::Rpm, ResolveOptions::default()) {
        Ok(ResolutionSet::Unique(target)) => Ok(target.package),
        Ok(ResolutionSet::None) => Err(format!(
            "component '{name}' is recorded as RPM-backed without package metadata, and no RPM package could be resolved; run `anolisa repair {name}`"
        )),
        Ok(ResolutionSet::Ambiguous(targets)) => Err(format!(
            "component '{name}' is recorded as RPM-backed without package metadata, and multiple RPM packages were resolved ({}); run `anolisa repair {name}`",
            targets
                .iter()
                .map(|target| target.package.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Err(err) => Err(format!(
            "cannot resolve the RPM package for legacy component '{name}': {err}"
        )),
    }
}

/// Outcome of probing rpmdb for a profile default that is absent from ANOLISA
/// state. Kept distinct from a plain `Option` so a query failure or an ambiguous
/// provider set is never collapsed into "missing" (which would be reported as an
/// installable default — the very false positive this cross-check prevents).
enum DefaultProbe {
    /// Installed on the host under this RPM package (an adopt/upgrade target).
    Installed(String),
    /// Not installed on the host — a genuine missing default.
    Missing,
    /// Presence could not be determined; carries a human-readable reason.
    Indeterminate(String),
}

/// Probe rpmdb for the RPM package backing `component` when it is absent from
/// ANOLISA state.
///
/// Two signals are consulted, both read-only:
/// 1. A package that Provides `anolisa-component(name)`. An empty provider set
///    falls through to the index fallback; exactly one provider is the target;
///    multiple providers are ambiguous and reported as indeterminate (mirroring
///    the CLI-owner "multiple packages = error" rule) rather than silently
///    picking one.
/// 2. For `legacy_adopt` packages that may lack that provide, the RPM package
///    name(s) the component index maps to.
///
/// Any query error (rpm/dnf missing, permission denied, malformed rpmdb output,
/// repoquery failure) yields [`DefaultProbe::Indeterminate`] — never `Missing`.
fn probe_default_package(
    query: &dyn PackageQuery,
    index: Option<&ComponentIndex>,
    name: &str,
) -> DefaultProbe {
    match query.what_provides_installed(&rpm_component_provide(name)) {
        Ok(providers) => match providers.as_slice() {
            [] => {}
            [only] => return DefaultProbe::Installed(only.clone()),
            many => {
                return DefaultProbe::Indeterminate(format!(
                    "default component '{name}' is provided by multiple installed RPM packages ({}); cannot pick an upgrade target",
                    many.join(", ")
                ));
            }
        },
        Err(err) => {
            return DefaultProbe::Indeterminate(format!(
                "cannot determine whether default component '{name}' is installed: {err}"
            ));
        }
    }

    for package in index_rpm_packages(index, name) {
        match query.query_installed(&package) {
            Ok(Some(_)) => return DefaultProbe::Installed(package),
            Ok(None) => continue,
            Err(err) => {
                return DefaultProbe::Indeterminate(format!(
                    "cannot query installed version of '{package}' for default component '{name}': {err}"
                ));
            }
        }
    }
    DefaultProbe::Missing
}

/// RPM package names the component index maps `component` to: the `rpm` backend
/// package plus any `rpm-package` aliases (historical names). Empty when there
/// is no index or no RPM mapping for the component. Duplicates (e.g. a backend
/// and an alias sharing a name) are collapsed so rpmdb is not queried twice for
/// the same package, order-preserving.
fn index_rpm_packages(index: Option<&ComponentIndex>, component: &str) -> Vec<String> {
    let Some(index) = index else {
        return Vec::new();
    };
    let Some(entry) = index.components.iter().find(|e| e.name == component) else {
        return Vec::new();
    };
    let mut packages: Vec<String> = Vec::new();
    let mut push_unique = |package: &str| {
        if !package.is_empty() && !packages.iter().any(|p| p == package) {
            packages.push(package.to_string());
        }
    };
    for backend in &entry.backends {
        if backend.kind == "rpm" {
            push_unique(&backend.package);
        }
    }
    for alias in &entry.aliases {
        if alias.kind == "rpm-package" {
            push_unique(&alias.name);
        }
    }
    packages
}

/// Evaluate an installed-but-unadopted default against the RPM upgrade scope,
/// mirroring [`check_component`]'s query→candidate flow. Ownership is reported
/// as `rpm-observed`: present on the host, not managed through ANOLISA state.
fn check_present_default(
    query: &dyn PackageQuery,
    name: &str,
    package: &str,
    arch: &str,
    summary: &mut CheckSummary,
) -> ComponentCheck {
    let ownership_label = ManagementRelation::Observed.label().to_string();
    let installed = match query.query_installed(package) {
        Ok(Some(info)) => info.version,
        // The probe just resolved `package` as an installed provider, so an
        // absent package here means it raced away mid-check. That is an
        // inconsistency we cannot resolve, not evidence the default is missing —
        // report an item error rather than flipping back to "install".
        Ok(None) => {
            summary.errors += 1;
            return component_error(
                name.to_string(),
                Some(package.to_string()),
                ownership_label,
                None,
                format!(
                    "default component '{name}' resolved to package '{package}' but it is absent from rpmdb"
                ),
            );
        }
        Err(err) => {
            summary.errors += 1;
            return component_error(
                name.to_string(),
                Some(package.to_string()),
                ownership_label,
                None,
                format!("rpm query failed: {err}"),
            );
        }
    };
    let installed_evr = installed.to_string();

    match repo_upgrade(query, package, arch, &installed) {
        Ok(Some(available)) => {
            summary.updates += 1;
            ComponentCheck {
                component: name.to_string(),
                package: Some(package.to_string()),
                ownership: Some(ownership_label),
                installed: Some(installed_evr),
                available: Some(available),
                action: ACTION_UPDATE.to_string(),
                error: None,
                absent_from_state: true,
                backfill_rpm_metadata: false,
            }
        }
        Ok(None) => ComponentCheck {
            component: name.to_string(),
            package: Some(package.to_string()),
            ownership: Some(ownership_label),
            installed: Some(installed_evr),
            available: None,
            action: ACTION_NOOP.to_string(),
            error: None,
            absent_from_state: true,
            backfill_rpm_metadata: false,
        },
        Err(err) => {
            summary.errors += 1;
            component_error(
                name.to_string(),
                Some(package.to_string()),
                ownership_label,
                Some(installed_evr),
                format!("repo candidate query failed: {err}"),
            )
        }
    }
}

/// Newest repo candidate strictly newer than `installed`, filtered to the
/// installed arch (plus `noarch`). Returns `None` when no candidate is a genuine
/// upgrade; ordering uses RPM's own EVR comparison
/// ([`rpm_evr_cmp`]) so epochs/releases/non-semver versions are ordered the way
/// `dnf` would, and a stale/downgrade candidate is never reported as available.
fn repo_upgrade(
    query: &dyn PackageQuery,
    package: &str,
    arch: &str,
    installed: &PackageVersion,
) -> Result<Option<String>, PackageQueryError> {
    let mut best: Option<PackageVersion> = None;
    for info in query.query_available(package)? {
        if info.arch != arch && info.arch != "noarch" {
            continue;
        }
        if rpm_evr_cmp(&info.version, installed) != Ordering::Greater {
            continue;
        }
        best = match best {
            Some(current) if rpm_evr_cmp(&current, &info.version) != Ordering::Less => {
                Some(current)
            }
            _ => Some(info.version),
        };
    }
    Ok(best.map(|version| version.to_string()))
}

fn cli_unsupported(reason: &str) -> CliCheck {
    CliCheck {
        package: None,
        installed: None,
        available: None,
        action: ACTION_UNSUPPORTED.to_string(),
        error: Some(reason.to_string()),
    }
}

fn cli_error(package: Option<String>, reason: String) -> CliCheck {
    CliCheck {
        package,
        installed: None,
        available: None,
        action: ACTION_ERROR.to_string(),
        error: Some(reason),
    }
}

fn component_error(
    component: String,
    package: Option<String>,
    ownership: String,
    installed: Option<String>,
    reason: String,
) -> ComponentCheck {
    ComponentCheck {
        component,
        package,
        ownership: Some(ownership),
        installed,
        available: None,
        action: ACTION_ERROR.to_string(),
        error: Some(reason),
        absent_from_state: false,
        backfill_rpm_metadata: false,
    }
}

/// Resolved profile name reported for a given `--target` value: the raw name
/// when supplied, otherwise the release default. Used both to build the report's
/// `target` field and to key the MOTD cache, so both agree on the same name.
fn effective_target_name(target: Option<&str>) -> &str {
    target.unwrap_or(DEFAULT_TARGET_PROFILE_NAME)
}

/// Resolve the effective target profile and its reported name.
///
/// An omitted `--target` resolves to [`DEFAULT_TARGET_PROFILE_NAME`] through the
/// same lookup path as an explicit target, so disk profiles can override the
/// built-in bootstrap fallback consistently.
fn load_effective_target_profile(
    layout: &FsLayout,
    target: Option<&str>,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> Result<(String, TargetProfile), CliError> {
    let name = effective_target_name(target);
    Ok((
        name.to_string(),
        load_target_profile_by_name(layout, name, packaged_data_probe)?,
    ))
}

/// Resolve and read a named target profile. The name is validated as a single
/// path segment so `--target` cannot escape the profiles directory.
///
/// Lookup order (first hit wins): `<etc_dir>/profiles/<name>.toml`, then
/// `<packaged_datadir>/profiles/<name>.toml`, then — only for the release
/// default name — the built-in profile compiled into the binary. A missing
/// non-default profile is a hard [`CliError::InvalidArgument`] listing the
/// searched paths.
fn load_target_profile_by_name(
    layout: &FsLayout,
    name: &str,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> Result<TargetProfile, CliError> {
    validate_target_name(name)?;

    let mut searched = Vec::new();

    let etc_path = layout
        .etc_dir
        .join(PROFILES_SUBDIR)
        .join(format!("{name}.toml"));
    if let Some(profile) = try_read_profile(&etc_path, name)? {
        return Ok(profile);
    }
    searched.push(etc_path);

    let packaged_root = crate::packaged::packaged_datadir_root(layout, packaged_data_probe)
        .unwrap_or_else(|| layout.datadir.clone());
    let packaged_path = packaged_root
        .join(PROFILES_SUBDIR)
        .join(format!("{name}.toml"));
    if let Some(profile) = try_read_profile(&packaged_path, name)? {
        return Ok(profile);
    }
    searched.push(packaged_path);

    // Only the release default has a compiled-in fallback; any other name that
    // reached here genuinely has no profile on disk.
    if name == DEFAULT_TARGET_PROFILE_NAME {
        return load_builtin_default_profile();
    }

    Err(CliError::InvalidArgument {
        command: CHECK_COMMAND.to_string(),
        reason: format!(
            "cannot find target profile '{name}'; searched {}",
            searched
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

/// Read one candidate profile path. Distinguishes "absent" (`Ok(None)`, so the
/// caller keeps looking) from "present but unreadable/invalid" (`Err`, a real
/// misconfiguration the caller must not paper over with a fallback).
fn try_read_profile(path: &Path, name: &str) -> Result<Option<TargetProfile>, CliError> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(CliError::InvalidArgument {
                command: CHECK_COMMAND.to_string(),
                reason: format!(
                    "cannot read target profile '{name}' at {}: {err}",
                    path.display()
                ),
            });
        }
    };
    TargetProfile::from_toml_str(&body)
        .map(Some)
        .map_err(|err| CliError::InvalidArgument {
            command: CHECK_COMMAND.to_string(),
            reason: format!("target profile '{name}' is invalid: {err}"),
        })
}

/// Parse the compiled-in release default profile. A parse failure here is a
/// build-time bug in the packaged asset, not user input, so it is a
/// [`CliError::Runtime`].
fn load_builtin_default_profile() -> Result<TargetProfile, CliError> {
    TargetProfile::from_toml_str(BUILTIN_DEFAULT_TARGET_PROFILE).map_err(|err| CliError::Runtime {
        command: CHECK_COMMAND.to_string(),
        reason: format!("built-in default target profile is invalid: {err}"),
    })
}

/// Reject target names that are not a single, safe path segment.
fn validate_target_name(target: &str) -> Result<(), CliError> {
    if target.trim().is_empty()
        || target == "."
        || target == ".."
        || target.contains('/')
        || target.contains('\\')
    {
        return Err(CliError::InvalidArgument {
            command: CHECK_COMMAND.to_string(),
            reason: format!("target profile name '{target}' is not a valid profile identifier"),
        });
    }
    Ok(())
}

// ── cache ────────────────────────────────────────────────────────────────

/// Cache location under the active layout's `cache_dir`
/// (`/var/cache/anolisa/update-check.json` in system mode).
fn cache_path(layout: &FsLayout) -> PathBuf {
    layout.cache_dir.join(CACHE_FILE)
}

/// Read and parse a cached report; any error (missing, unreadable, malformed)
/// yields `None` so a bad cache never blocks a fresh query.
fn read_cache(path: &Path) -> Option<UpdateCheckCache> {
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

/// Whether a cached report may be reused for a MOTD render: it must be fresh AND
/// have been computed for the same target, so a prior `--target` run never leaks
/// its report into a plain MOTD (or a different target's).
fn cache_is_usable(
    cache: &UpdateCheckCache,
    target: Option<&str>,
    now: impl FnOnce() -> chrono::DateTime<Utc>,
) -> bool {
    is_fresh(&cache.generated_at, now) && cache.report.target.as_deref() == target
}

/// Whether a cache timestamp is within [`CACHE_TTL_SECS`] of now (and not in the
/// future, which would indicate a corrupt/adversarial timestamp).
fn is_fresh(generated_at: &str, now: impl FnOnce() -> chrono::DateTime<Utc>) -> bool {
    match chrono::DateTime::parse_from_rfc3339(generated_at) {
        Ok(ts) => {
            let age = now().signed_duration_since(ts.with_timezone(&Utc));
            age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(CACHE_TTL_SECS)
        }
        Err(_) => false,
    }
}

/// Best-effort cache write; failures are swallowed by the caller.
fn write_cache(
    path: &Path,
    report: &UpdateCheckReport,
    now: impl FnOnce() -> chrono::DateTime<Utc>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cache = UpdateCheckCache {
        generated_at: now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        report: report.clone(),
    };
    let body = serde_json::to_string_pretty(&cache)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(path, body)
}
