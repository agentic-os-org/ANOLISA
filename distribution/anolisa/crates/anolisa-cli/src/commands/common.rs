//! Shared helpers for tier1 / tier2 command handlers.
//!
//! Access to the skeleton-stable command inputs: [`FsLayout`], the v5
//! [`StateStore`], and repo configuration. Keep this module thin —
//! handlers compose these calls; we do not introduce a service layer here.

use std::path::PathBuf;

use anolisa_core::adapter::manager::{AdapterManager, VisibleRoot};
use anolisa_core::domain::{
    Installation, InstallationScope, NativePm, PackageIdentity, ProviderBinding,
};
use anolisa_core::facts::{JournalEvidence, JournalInventory};
use anolisa_core::state_store::StateStore;
use anolisa_core::{ComponentManifest, ObjectKind};
use anolisa_platform::fs_layout::FsLayout;

use crate::color::Palette;
use crate::commands::state_view::{StateView, StateVisibility};
use crate::context::{CliContext, InstallMode};
use crate::packaged;
use crate::repo_config::{RepoConfig, RepoConfigProvisioning};
use crate::response::CliError;

/// State subdirectory where install stores the exact component contract
/// used for each installed component.
const INSTALLED_COMPONENT_MANIFESTS_SUBDIR: &str = "component-manifests";
/// Filename used for the locally persisted installed component contract.
const INSTALLED_COMPONENT_MANIFEST_FILE: &str = "component.toml";

/// Build the layout for the active install mode, honoring `--prefix`
/// (system-mode) and the current process user's home (user-mode).
pub fn resolve_layout(ctx: &CliContext) -> FsLayout {
    ctx.layout().clone()
}

/// Build a consistent package-transaction permission error.
pub(crate) fn package_permission_error(command: &str, bin: &str, action: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!("permission denied running {bin}; re-run the {action} with sudo"),
    }
}

/// Renders an explicit-scope remediation command so diagnostics never rely
/// on the caller's default installation mode.
pub(crate) fn scoped_component_command(
    scope: InstallationScope,
    operation: &str,
    component: &str,
) -> String {
    let mode = match scope {
        InstallationScope::System => InstallMode::System,
        InstallationScope::User { .. } => InstallMode::User,
    };
    scoped_component_command_for_mode(mode, operation, component)
}

pub(crate) fn scoped_component_command_for_mode(
    mode: InstallMode,
    operation: &str,
    component: &str,
) -> String {
    match mode {
        InstallMode::System => {
            format!("sudo anolisa --install-mode system {operation} {component}")
        }
        InstallMode::User => {
            format!("anolisa --install-mode user {operation} {component}")
        }
    }
}

/// Build a consistent non-zero package-transaction error.
pub(crate) fn package_transaction_failed_error(
    command: &str,
    operation: &str,
    code: Option<i32>,
    stderr: &str,
) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "dnf {operation} failed (exit {}): {}",
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr.trim(),
        ),
    }
}

/// Render repo config provisioning performed by commands that need repo access.
fn render_repo_config_provisioning(ctx: &CliContext, provisioning: &RepoConfigProvisioning) {
    if ctx.quiet || ctx.json {
        return;
    }
    let color = Palette::new(ctx.no_color);
    // Routed through `suspend_output` so these persistent lines cannot
    // interleave with a live activity spinner (issue #1452); a no-op when no
    // spinner is running, which is the case for most callers.
    crate::progress::suspend_output(|| match provisioning {
        RepoConfigProvisioning::Existing => {}
        RepoConfigProvisioning::Downloaded { url, dest } => {
            println!(
                "{} repo config was missing; downloaded {} to {}",
                color.ok("✓"),
                url,
                color.path(dest.display().to_string()),
            );
        }
        RepoConfigProvisioning::FetchedForDryRun { url, dest } => {
            println!(
                "fetched repo config from {url} for dry-run; would write to {}",
                color.path(dest.display().to_string()),
            );
        }
        RepoConfigProvisioning::DownloadedPersistFailed { url, dest, reason } => {
            eprintln!(
                "{} repo config fetched from {} but could not write to {}: {}",
                color.warn("⚠"),
                url,
                color.path(dest.display().to_string()),
                reason,
            );
        }
    });
}

/// Controls whether a failed persistence of downloaded repo config is fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepoPersistPolicy {
    /// Mutating commands: persistence failure is an error.
    Require,
    /// Read-only / best-effort commands: warn and use in-memory config.
    BestEffort,
}

/// Enforce the persist policy against a load result.
///
/// Extracted so the policy branch can be unit-tested without a real
/// network fetch or filesystem setup.
fn enforce_repo_persist_policy(
    provisioning: &RepoConfigProvisioning,
    persist_policy: RepoPersistPolicy,
    command: &str,
) -> Result<(), CliError> {
    if persist_policy == RepoPersistPolicy::Require
        && let RepoConfigProvisioning::DownloadedPersistFailed { dest, reason, .. } = provisioning
    {
        return Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "repo config downloaded but could not be written to {}: {reason}",
                dest.display()
            ),
        });
    }
    Ok(())
}

/// Load `repo.toml`, provisioning it if every local source is missing.
///
/// `persist_policy` governs what happens when a freshly-downloaded config
/// cannot be written to disk:
/// - [`RepoPersistPolicy::Require`]: command fails (use for mutating ops).
/// - [`RepoPersistPolicy::BestEffort`]: warn and return the in-memory config.
pub(crate) fn load_repo_config(
    ctx: &CliContext,
    layout: &FsLayout,
    command: &str,
    persist_policy: RepoPersistPolicy,
) -> Result<RepoConfig, CliError> {
    let repo_load =
        RepoConfig::load(layout, ctx.dry_run, ctx.packaged_data_probe()).map_err(|err| {
            CliError::Runtime {
                command: command.to_string(),
                reason: err.to_string(),
            }
        })?;
    enforce_repo_persist_policy(&repo_load.provisioning, persist_policy, command)?;
    render_repo_config_provisioning(ctx, &repo_load.provisioning);
    Ok(repo_load.config)
}

/// Load the v5 [`StateStore`] for this context, migrating a legacy state file
/// in memory. A missing file is a fresh store.
pub fn load_state_store(ctx: &CliContext, command: &str) -> Result<StateStore, CliError> {
    let layout = resolve_layout(ctx);
    let path = layout.state_dir.join("installed.toml");
    StateStore::load_for_layout(&path, anolisa_platform::privilege::effective_uid(), &layout)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "failed to load installed state at {}: {err}",
                path.display()
            ),
        })
}

fn reject_visible_non_writable_component_from_view(
    view: &StateView,
    command: &str,
    component: &str,
) -> Result<(), CliError> {
    view.reject_non_writable_component_mutation(command, component)
}

/// Resolve an install target without treating a read-only system record as
/// the installation being mutated.
///
/// Exact identities across the visible user-plus-system view — including
/// names claimed by an on-disk operation journal — still precede the
/// component index, which is the sole authority for every other identity:
/// an input the index does not recognize is rejected instead of being pinned
/// literally. A normalized `--repo` override in `index_base_override` makes
/// that repository's published index the identity authority for this
/// invocation. The returned state view keeps planning scoped to its writable
/// root, so a user install can create a user record that shadows an existing
/// system installation without inheriting or changing it.
pub(crate) fn resolve_install_target(
    input: &str,
    ctx: &CliContext,
    command: &str,
    index_base_override: Option<&str>,
) -> Result<(String, StateView), CliError> {
    resolve_lifecycle_target(input, ctx, command, false, index_base_override)
        .map(|(component, view, _)| (component, view))
}

/// Resolve an adopt target under install's narrow identity precedence.
///
/// A fresh adopt writes a brand-new component record, so like install it
/// lets only exact component records (active or quarantined) and
/// effectively-pending operation journals precede the component index.
/// Terminal journals, legacy capability rows, and dropped capability names
/// stay addressable on the mutation paths, which act on the recorded state
/// and own the migration guidance; here they must not re-authorize an
/// identity the index no longer recognizes just because `package_map` or
/// RPM `Provides` could still resolve a package for it.
pub(crate) fn resolve_adopt_target(
    input: &str,
    ctx: &CliContext,
    command: &str,
) -> Result<(String, StateView), CliError> {
    resolve_lifecycle_target(input, ctx, command, false, None)
        .map(|(component, view, _)| (component, view))
}

/// Resolve a lifecycle target and reject a visible installation that the
/// current invocation cannot mutate.
///
/// Exact component names and package identities are resolved from visible
/// state first; every other identity must come from the component index.
/// Index-supported but absent targets flow on to the planner's normal
/// `not-installed` path, while unrecognized names fail as unsupported (or
/// as index-unavailable when nothing could validate them).
pub(crate) fn resolve_mutation_target(
    input: &str,
    ctx: &CliContext,
    command: &str,
) -> Result<(String, StateView), CliError> {
    resolve_lifecycle_target(input, ctx, command, true, None)
        .map(|(component, view, _)| (component, view))
}

/// Resolve an adapter target across visible component and local receipt
/// identities without requiring the source component's state to be writable.
pub(crate) fn resolve_adapter_target(
    input: &str,
    ctx: &CliContext,
    command: &str,
) -> Result<(String, StateView), CliError> {
    let writable_state = load_state_store(ctx, command)?;
    let view = StateView::load_with_writable_state(
        ctx,
        command,
        StateVisibility::UserPlusSystem,
        writable_state,
    )?;
    resolve_adapter_target_from_view(input, command, view, || {
        component_identity_from_repo_index(input, ctx, command, None)
    })
}

fn resolve_adapter_target_from_view(
    input: &str,
    command: &str,
    view: StateView,
    identity: impl FnOnce() -> Result<String, CliError>,
) -> Result<(String, StateView), CliError> {
    let exact_identity = view.has_exact_component(input)
        || !view
            .writable
            .state
            .adapter_claims_for_component(input)
            .is_empty();
    if exact_identity {
        return Ok((input.to_string(), view));
    }
    view.reject_incomplete_alias_visibility(command, input)?;
    let component = identity()?;
    Ok((component, view))
}

fn resolve_lifecycle_target(
    input: &str,
    ctx: &CliContext,
    command: &str,
    reject_read_only: bool,
    index_base_override: Option<&str>,
) -> Result<(String, StateView, bool), CliError> {
    let writable_state = load_state_store(ctx, command)?;
    let layout = resolve_layout(ctx);
    let journal_dir = layout.state_dir.join("journal");
    let inventory = JournalInventory::load(JournalEvidence::new(
        &journal_dir,
        &writable_state.operations,
    ))
    .map_err(|err| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to inspect pending operation journals: {err}"),
    })?;
    // A journal on disk is exact local state: its recorded identity (subject
    // or legacy step target) stays addressable without index validation so
    // interrupted or still-uncleared operations remain recoverable offline.
    // Record-creating commands (install, adopt) share this precedence only
    // for effectively-pending journals — enough to reach the
    // pending-operation recovery guidance — because a terminal journal left
    // behind by a finished operation must not keep authorizing an identity
    // the component index no longer recognizes.
    let writable_journal_identity = if reject_read_only {
        crate::commands::tier1::rpm_install::journal_claims_component(&inventory, input)
    } else {
        crate::commands::tier1::rpm_install::pending_journal_claims_component(&inventory, input)
    };
    let view = StateView::load_with_writable_state(
        ctx,
        command,
        StateVisibility::UserPlusSystem,
        writable_state,
    )?;

    if writable_journal_identity {
        return Ok((input.to_string(), view, true));
    }
    resolve_lifecycle_target_from_view(input, command, reject_read_only, view, || {
        component_identity_from_repo_index(input, ctx, command, index_base_override)
    })
}

fn resolve_lifecycle_target_from_view(
    input: &str,
    command: &str,
    reject_read_only: bool,
    view: StateView,
    identity: impl FnOnce() -> Result<String, CliError>,
) -> Result<(String, StateView, bool), CliError> {
    // Mutations pin any recorded object name so non-component legacy records
    // (capabilities, dropped capability names) reach their owning command's
    // migration guidance. Record-creating commands (install, adopt) accept
    // only exact component records: a historical non-component row must not
    // authorize a fresh identity the component index no longer recognizes.
    let exact_identity = if reject_read_only {
        view.has_exact_object(input)
    } else {
        view.has_exact_component(input)
    };
    if reject_read_only
        && let Some(component) = view.resolve_mutation_component_identity(command, input)?
    {
        return Ok((component, view, true));
    }
    if exact_identity {
        return Ok((input.to_string(), view, true));
    }
    // A record-creating command may mint an independent writable-scope
    // identity, but only one the component index authorizes. Remapping an
    // alias to its canonical name additionally requires complete visibility:
    // an unreadable root cannot prove the alias is not an exact name
    // recorded there.
    let component = identity()?;
    if component != input {
        view.reject_incomplete_alias_visibility(command, input)?;
    }
    if reject_read_only {
        reject_visible_non_writable_component_from_view(&view, command, &component)?;
    }
    Ok((component, view, false))
}

/// Resolve a user-supplied component name to the stable state key.
///
/// 1. Exact state match wins — a component installed under its literal name
///    is never re-mapped and stays addressable offline.
/// 2. Otherwise the repo-side component index is the sole identity authority
///    (canonical names and declared aliases, e.g. `copilot-shell` → `cosh`)
///    via in-memory lookup only — no rpmdb/dnf queries are triggered.
///
/// # Errors
///
/// An input the index does not recognize is an unsupported component; when
/// no index can be loaded at all, the name cannot be validated and the
/// explicit index-unavailable error is returned instead (issue #2630).
pub(crate) fn lookup_component_name_in_store(
    input: &str,
    store: &anolisa_core::state_store::StateStore,
    ctx: &CliContext,
    command: &str,
) -> Result<String, CliError> {
    if store.contains_record(ObjectKind::Component, input) {
        return Ok(input.to_string());
    }
    component_identity_from_repo_index(input, ctx, command, None)
}

/// Step 2 of component-name resolution: establish the identity through the
/// repo-side component index, the sole authority for names absent from
/// exact local state. A normalized `--repo` base URL in
/// `index_base_override` replaces the repo.toml chain entirely, so the
/// overridden repository's index is the authority for the invocation.
fn component_identity_from_repo_index(
    input: &str,
    ctx: &CliContext,
    command: &str,
    index_base_override: Option<&str>,
) -> Result<String, CliError> {
    let layout = resolve_layout(ctx);
    let component_index = match index_base_override {
        Some(base_url) => crate::resolution::load_component_index_from_base(&layout, base_url).ok(),
        None => {
            let repo_config =
                load_repo_config(ctx, &layout, command, RepoPersistPolicy::BestEffort).ok();
            let env = anolisa_env::EnvService::detect();
            repo_config.as_ref().and_then(|cfg| {
                crate::resolution::load_optional_component_index(&layout, &env, cfg)
            })
        }
    };

    match crate::resolution::resolve_index_identity(input, component_index.as_ref()) {
        crate::resolution::IndexIdentity::Resolved(component) => Ok(component),
        crate::resolution::IndexIdentity::Unsupported => {
            Err(unsupported_component_error(command, input))
        }
        crate::resolution::IndexIdentity::Unavailable => {
            Err(component_index_unavailable_error(command, input))
        }
    }
}

/// Public error for a name the authoritative component index does not know.
pub(crate) fn unsupported_component_error(command: &str, input: &str) -> CliError {
    CliError::InvalidArgument {
        command: command.to_string(),
        reason: format!(
            "unsupported component '{input}'; run `anolisa list` to view supported components"
        ),
    }
}

/// Public error for a name that cannot be validated because no component
/// index could be loaded. Distinct from [`unsupported_component_error`] so
/// callers can tell "the index rejected this" from "nothing could check it".
pub(crate) fn component_index_unavailable_error(command: &str, input: &str) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "component index is unavailable; cannot validate component '{input}'; run `anolisa list` to inspect repository metadata"
        ),
    }
}

/// Path for the component manifest saved as part of an installed component's
/// local state.
pub fn installed_component_manifest_path(
    layout: &FsLayout,
    component: &str,
    command: &str,
) -> Result<PathBuf, CliError> {
    Ok(
        installed_component_manifest_dir(layout, component, command)?
            .join(INSTALLED_COMPONENT_MANIFEST_FILE),
    )
}

/// Legacy manifest directory eligible for cleanup after a record mutation.
pub(crate) fn legacy_component_manifest_dir_for_installation(
    layout: &FsLayout,
    installation: &Installation,
    command: &str,
) -> Result<Option<PathBuf>, CliError> {
    Ok(legacy_cosh_ng_manifest_path(layout, installation, command)?
        .and_then(|path| path.parent().map(PathBuf::from)))
}

fn legacy_cosh_ng_manifest_path(
    layout: &FsLayout,
    installation: &Installation,
    command: &str,
) -> Result<Option<PathBuf>, CliError> {
    if installation.name != "cosh-ng"
        || !matches!(
            &installation.binding,
            ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved { name },
                ..
            } if name == "cosh-ng"
        )
    {
        return Ok(None);
    }

    let legacy = installed_component_manifest_path(layout, "cosh", command)?;
    let matches_legacy_identity = legacy.is_file()
        && ComponentManifest::from_file(&legacy).is_ok_and(|manifest| {
            manifest.component.name == "cosh" && manifest.rpm_package() == Some("cosh-ng")
        });
    Ok(matches_legacy_identity.then_some(legacy))
}

/// Directory for the component manifest saved as part of an installed
/// component's local state.
pub fn installed_component_manifest_dir(
    layout: &FsLayout,
    component: &str,
    command: &str,
) -> Result<PathBuf, CliError> {
    validate_component_path_segment(component, command)?;
    Ok(layout
        .state_dir
        .join(INSTALLED_COMPONENT_MANIFESTS_SUBDIR)
        .join(component))
}

fn validate_component_path_segment(component: &str, command: &str) -> Result<(), CliError> {
    if component.trim().is_empty()
        || component == "."
        || component == ".."
        || component.contains('/')
        || component.contains('\\')
    {
        return Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason: format!("component name '{component}' cannot be used as a local path segment"),
        });
    }
    Ok(())
}

/// Wire-friendly status label for a v5
/// [`Installation`], same vocabulary as
/// [`installation_status_str`] vocabulary: a delegated adopted/observed row
/// reports its management relation (the legacy state collapsed both into one
/// `adopted` status), any other row reports its lifecycle health.
pub(crate) fn installation_status_str(
    installation: &anolisa_core::domain::Installation,
) -> &'static str {
    use anolisa_core::domain::{LifecycleStatus, ManagementRelation, ProviderBinding};
    if installation.status == LifecycleStatus::Installed
        && let ProviderBinding::Delegated { relation, .. } = &installation.binding
    {
        match relation {
            ManagementRelation::Adopted { .. } => return "adopted",
            ManagementRelation::Observed => return "observed",
            ManagementRelation::Managed { .. } => {}
        }
    }
    match installation.status {
        LifecycleStatus::Installed => "installed",
        LifecycleStatus::Partial => "degraded",
        LifecycleStatus::Disabled => "disabled",
        LifecycleStatus::Failed => "failed",
    }
}

/// True iff the wire status label denotes a component that is actively
/// serving (i.e. `installed`, `degraded`, `adopted`, or `observed`). Used by
/// `list --enabled` to exclude `disabled`/`failed`/`not_installed`.
pub(crate) fn status_is_enabled(status_label: &str) -> bool {
    matches!(
        status_label,
        "installed" | "degraded" | "adopted" | "observed"
    )
}

/// Build an [`AdapterManager`] for the active layout, shared between
/// `adapter` and `status` handlers.
pub(crate) fn build_adapter_manager(ctx: &CliContext) -> AdapterManager {
    let (mut manager, layout) = new_adapter_manager(ctx);

    match StateView::load(ctx, "adapter", StateVisibility::UserPlusSystem) {
        Ok(view) => configure_adapter_manager(&mut manager, &view, ctx.packaged_data_probe()),
        Err(_) => {
            manager.set_visible_roots(vec![visible_root_for_adapter(
                &layout,
                ctx.packaged_data_probe(),
            )]);
        }
    }

    manager
}

/// Build an adapter manager from the same state visibility snapshot used to
/// resolve a component-specific adapter target.
pub(crate) fn build_adapter_manager_from_view(
    ctx: &CliContext,
    view: &StateView,
) -> AdapterManager {
    let (mut manager, _) = new_adapter_manager(ctx);
    configure_adapter_manager(&mut manager, view, ctx.packaged_data_probe());
    manager
}

fn new_adapter_manager(ctx: &CliContext) -> (AdapterManager, FsLayout) {
    let layout = resolve_layout(ctx);
    let env = anolisa_env::EnvService::detect();
    (
        AdapterManager::new(layout.clone(), Some(env.home), env.user),
        layout,
    )
}

fn configure_adapter_manager(
    manager: &mut AdapterManager,
    view: &StateView,
    packaged_data_probe: &packaged::PackagedDataProbe,
) {
    let roots = view
        .visible_roots
        .iter()
        .map(|root| visible_root_for_adapter(&root.layout, packaged_data_probe))
        .collect();
    manager.set_visible_roots(roots);
    for warning in &view.warnings {
        manager.push_visibility_warning(warning.clone());
    }
}

fn visible_root_for_adapter(
    layout: &FsLayout,
    packaged_data_probe: &packaged::PackagedDataProbe,
) -> VisibleRoot {
    VisibleRoot {
        state_dir: layout.state_dir.clone(),
        contract_datadir_roots: adapter_contract_datadir_roots(layout, packaged_data_probe),
    }
}

fn adapter_contract_datadir_roots(
    layout: &FsLayout,
    packaged_data_probe: &packaged::PackagedDataProbe,
) -> Vec<PathBuf> {
    // Two independent datadir-discovery mechanisms are layered here:
    //
    //   packaged_datadir_root()  — runtime probe: env override → exe-sibling
    //                              `../share/anolisa/` → layout.datadir.
    //                              Discovers wherever the *running binary's*
    //                              packaged tree actually lives on disk.
    //
    //   layout.package_datadir() — FHS constant: `/usr/share/anolisa` (rebased
    //                              under prefix). System roots use it so
    //                              RPM-installed contracts are found even when
    //                              the binary is at `/usr/local/bin/`.
    //
    // Both are added (deduped) because they cover different scenarios: the
    // exe-sibling probe handles relocated installs; the FHS constant handles
    // cross-install-method discovery (raw binary + RPM components).
    let mut roots = vec![layout.datadir.clone()];
    if let Some(packaged) = packaged::packaged_datadir_root(layout, packaged_data_probe)
        && !roots.contains(&packaged)
    {
        roots.push(packaged);
    }
    if let Some(pkg_dd) = layout.package_datadir()
        && !roots.contains(&pkg_dd)
    {
        roots.push(pkg_dd);
    }
    roots
}

/// In-memory migration of pre-v4 symlink entries.
///
/// Loads each component's installed manifest, resolves its
/// `FileKind::Symlink` entries, and upgrades matching
/// `kind = File` `OwnedFile` entries to `kind = Symlink` with the
/// manifest-declared referent — but only when every disk-level
/// invariant holds (link exists, points at the manifest-declared
/// target, referent is a regular file, and any recorded sha256
/// matches the referent content).
///
/// Returns the number of entries migrated. Errors in individual
/// components are silently skipped (conservative: the entry stays
/// `kind = File` and the integrity probe reports `symlink_refused`).
pub fn migrate_v3_symlinks(store: &mut StateStore, layout: &FsLayout) -> usize {
    use std::collections::HashMap;
    use std::fs;

    use anolisa_core::domain::ProviderBinding;
    use anolisa_core::expand_layout_placeholders;
    use anolisa_core::manifest::{ComponentManifest, FileKind};
    use anolisa_core::path_safety::validate_owned_path;
    use anolisa_core::state::{ObjectKind, OwnedFileKind};
    use sha2::{Digest, Sha256};

    const MAX_MIGRATE_PROBE_BYTES: u64 = 256 * 1024 * 1024;

    fn hex_lower(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    fn hash_file_streaming(path: &std::path::Path) -> std::io::Result<String> {
        use std::io::Read;
        let f = fs::File::open(path)?;
        let mut reader = std::io::BufReader::new(f);
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 8 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_MIGRATE_PROBE_BYTES {
                return Err(std::io::Error::other(
                    "file exceeds migration probe ceiling",
                ));
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex_lower(&hasher.finalize()))
    }

    // Pre-v4 symlink entries can only exist on records that came through the
    // legacy-file migration; a native v5 file was written by code that already
    // records symlinks as symlinks.
    if !store.migrated_from_legacy() {
        return 0;
    }

    let mut migrated = 0usize;

    for installation in &mut store.installations {
        if installation.kind != ObjectKind::Component {
            continue;
        }
        let name = installation.name.clone();
        let ProviderBinding::Owned { artifact } = &mut installation.binding else {
            continue;
        };
        let has_legacy = artifact.files.iter().any(|f| f.kind == OwnedFileKind::File);
        if !has_legacy {
            continue;
        }

        if validate_component_path_segment(&name, "migrate").is_err() {
            continue;
        }
        let manifest_path = layout
            .state_dir
            .join(INSTALLED_COMPONENT_MANIFESTS_SUBDIR)
            .join(&name)
            .join(INSTALLED_COMPONENT_MANIFEST_FILE);
        let toml_str = match fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let manifest = match ComponentManifest::from_toml_str(&toml_str) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let mut expected: HashMap<PathBuf, PathBuf> = HashMap::new();
        for spec in &manifest.install.files {
            if spec.kind != FileKind::Symlink {
                continue;
            }
            let dest_template = match spec.install_path() {
                Some(t) => t,
                None => continue,
            };
            let referent_template = match spec.source.as_deref() {
                Some(t) => t,
                None => continue,
            };
            let dest =
                match expand_layout_placeholders(dest_template, layout, &[("component", &name)]) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
            let referent = match expand_layout_placeholders(
                referent_template,
                layout,
                &[("component", &name)],
            ) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if validate_owned_path(layout, &dest).is_err()
                || validate_owned_path(layout, &referent).is_err()
            {
                continue;
            }
            expected.insert(dest, referent);
        }
        if expected.is_empty() {
            continue;
        }

        for file in &mut artifact.files {
            if file.kind != OwnedFileKind::File {
                continue;
            }
            let Some(expected_referent) = expected.get(&file.path) else {
                continue;
            };
            let Ok(sym_meta) = fs::symlink_metadata(&file.path) else {
                continue;
            };
            if !sym_meta.file_type().is_symlink() {
                continue;
            }
            let Ok(actual_referent) = fs::read_link(&file.path) else {
                continue;
            };
            if actual_referent != *expected_referent {
                continue;
            }
            let Ok(ref_meta) = fs::symlink_metadata(expected_referent) else {
                continue;
            };
            if ref_meta.file_type().is_symlink() || !ref_meta.is_file() {
                continue;
            }
            if let Some(ref recorded_sha) = file.sha256 {
                if ref_meta.len() > MAX_MIGRATE_PROBE_BYTES {
                    continue;
                }
                let actual = match hash_file_streaming(expected_referent) {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                if actual != *recorded_sha {
                    continue;
                }
            }
            file.kind = OwnedFileKind::Symlink;
            file.referent = Some(expected_referent.clone());
            file.sha256 = None;
            migrated += 1;
        }
    }

    migrated
}

/// Enrich owned-file rows written before config, permission, and capability metadata
/// was persisted in v5 state.
///
/// The exact installed component manifest is the authority: explicit file
/// modes and capability targets are expanded against the active layout, then
/// copied only onto matching ANOLISA-owned rows. This is intentionally
/// in-memory and best-effort so read-only commands never rewrite state and a
/// missing or damaged manifest cannot invent a contract.
///
/// Returns the number of metadata fields populated.
pub fn hydrate_owned_file_contracts(store: &mut StateStore, layout: &FsLayout) -> usize {
    let env = anolisa_env::EnvService::detect();
    let install_mode = match layout.mode {
        anolisa_platform::fs_layout::InstallMode::System => "system",
        anolisa_platform::fs_layout::InstallMode::User => "user",
    };
    let supported = anolisa_core::capability_for_install_mode(install_mode, &env).supported();
    hydrate_owned_file_contracts_with_capability_support(store, layout, supported)
}

/// Hydrate legacy metadata using an already observed capability-support decision.
pub(crate) fn hydrate_owned_file_contracts_with_capability_support(
    store: &mut StateStore,
    layout: &FsLayout,
    capability_probe_supported: bool,
) -> usize {
    use std::collections::HashMap;

    use anolisa_core::expand_layout_placeholders;
    use anolisa_core::manifest::FileKind;
    use anolisa_core::path_safety::validate_owned_path;
    use anolisa_core::state::FileOwner;

    #[derive(Default)]
    struct FileContract {
        // None distinguishes capability-only entries from exact layout mappings.
        config: Option<bool>,
        mode: Option<String>,
        capabilities: Vec<String>,
    }

    fn normalized_mode(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        let octal = trimmed.strip_prefix("0o").unwrap_or(trimmed);
        let mode = u32::from_str_radix(octal, 8).ok()?;
        (mode <= 0o7777).then(|| format!("{mode:04o}"))
    }

    let mut hydrated = 0;
    for installation in &mut store.installations {
        if installation.kind != ObjectKind::Component {
            continue;
        }
        let component = installation.name.clone();
        if validate_component_path_segment(&component, "hydrate").is_err() {
            continue;
        }
        let manifest_path = layout
            .state_dir
            .join(INSTALLED_COMPONENT_MANIFESTS_SUBDIR)
            .join(&component)
            .join(INSTALLED_COMPONENT_MANIFEST_FILE);
        let manifest = match ComponentManifest::from_file(&manifest_path) {
            Ok(manifest) => manifest,
            Err(_) => continue,
        };

        let mut contracts: HashMap<PathBuf, FileContract> = HashMap::new();
        let mut directory_contracts = Vec::new();
        for spec in &manifest.install.files {
            if spec.kind == FileKind::Symlink {
                continue;
            }
            let mode = spec.mode.as_deref().and_then(normalized_mode);
            let Some(template) = spec.install_path() else {
                continue;
            };
            let Ok(dest) =
                expand_layout_placeholders(template, layout, &[("component", &component)])
            else {
                continue;
            };
            if validate_owned_path(layout, &dest).is_err() {
                continue;
            }
            if spec
                .source
                .as_deref()
                .is_some_and(|source| source.ends_with('/'))
            {
                directory_contracts.push((dest, mode, spec.kind == FileKind::Config));
            } else {
                let contract = contracts.entry(dest).or_default();
                contract.mode = mode;
                contract.config = Some(spec.kind == FileKind::Config);
            }
        }
        for spec in &manifest.install.capabilities {
            // Older state cannot prove that a best-effort capability was
            // successfully applied. Required assignments are safe to infer
            // only on hosts where the install backend is actionable.
            if !capability_probe_supported || spec.optional || spec.caps.is_empty() {
                continue;
            }
            let Some(template) = spec.path.as_deref() else {
                continue;
            };
            let Ok(path) =
                expand_layout_placeholders(template, layout, &[("component", &component)])
            else {
                continue;
            };
            if validate_owned_path(layout, &path).is_err() {
                continue;
            }
            let contract = contracts.entry(path).or_default();
            contract.capabilities.extend(spec.caps.iter().cloned());
            contract
                .capabilities
                .sort_by_key(|cap| cap.to_ascii_lowercase());
            contract
                .capabilities
                .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        }

        let ProviderBinding::Owned { artifact } = &mut installation.binding else {
            continue;
        };
        for file in &mut artifact.files {
            if file.owner != FileOwner::Anolisa || file.kind == anolisa_core::OwnedFileKind::Symlink
            {
                continue;
            }
            if let Some(contract) = contracts.get(&file.path) {
                if contract.config == Some(true) && file.kind == anolisa_core::OwnedFileKind::File {
                    file.kind = anolisa_core::OwnedFileKind::Config;
                    hydrated += 1;
                }
                if file.mode.is_none()
                    && let Some(mode) = &contract.mode
                {
                    file.mode = Some(mode.clone());
                    hydrated += 1;
                }
                if file.capabilities.is_empty() && !contract.capabilities.is_empty() {
                    file.capabilities = contract.capabilities.clone();
                    hydrated += 1;
                }
            }
            let mut directory_matches = directory_contracts
                .iter()
                .filter(|(directory, _, _)| file.path.starts_with(directory));
            if let Some((_, mode, config)) = directory_matches.next().filter(|_| {
                contracts
                    .get(&file.path)
                    .is_none_or(|contract| contract.config.is_none())
            }) {
                // Legacy rows lack archive provenance. Only unanimous directory
                // contracts can safely exempt content or reconstruct permissions.
                let mut config = *config;
                let mut mode = mode.as_ref();
                for (_, other_mode, other_config) in directory_matches {
                    config &= *other_config;
                    if mode != other_mode.as_ref() {
                        mode = None;
                    }
                }
                if file.mode.is_none() && mode.is_some() {
                    file.mode = mode.cloned();
                    hydrated += 1;
                }
                if config && file.kind == anolisa_core::OwnedFileKind::File {
                    file.kind = anolisa_core::OwnedFileKind::Config;
                    hydrated += 1;
                }
            }
        }
    }
    hydrated
}

const REDACTED: &str = "<redacted>";

/// Removes known URLs and withholds text containing any unverified URL.
pub(crate) fn redact_known_urls(text: &str, urls: &[String]) -> String {
    let mut out = text.to_string();
    for url in urls {
        out = redact_url_runs(&out, url);
    }
    if out.contains("://") {
        return "the failure text was withheld: it carried a URL that could not be \
                shown to be free of credentials"
            .to_string();
    }
    out
}

fn redact_url_runs(text: &str, url: &str) -> String {
    if url.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(url) {
        out.push_str(&rest[..at]);
        out.push_str(REDACTED);
        let matched = &rest[at..];
        let end = matched
            .find(char::is_whitespace)
            .unwrap_or(matched.len())
            .max(url.len());
        rest = &matched[end..];
    }
    out.push_str(rest);
    out
}

/// Keeps only scheme and authority because paths and queries may carry secrets.
///
/// Fail-closed: a credential with an unencoded '/', '?', or '#' ends the
/// authority early (`https://ci:secret-token/@mirror.invalid/…` leaves
/// `ci:secret-token` where a host belongs), so an authority is shown only
/// when it parses as an unambiguous host or `host:port` — and, when an `@`
/// follows the cut, only when the parser provably did not sever a
/// `user[:password]` span (see [`could_be_severed_credential`]). Anything
/// else is `None` and callers report an opaque label.
pub(crate) fn endpoint_without_credentials(url: &str) -> Option<String> {
    let sep = url.find("://")?;
    let remainder = &url[sep + 3..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let tail = &remainder[authority_end..];
    let (host, stripped_userinfo) = match authority.rfind('@') {
        Some(at) => (&authority[at + 1..], true),
        None => (authority, false),
    };
    if !parses_as_host(host) {
        return None;
    }
    if could_be_severed_credential(tail, stripped_userinfo) {
        return None;
    }
    Some(format!("{}{host}", &url[..sep + 3]))
}

/// True when a fragment that already reads as a legal endpoint could still
/// be a severed credential — and must be withheld.
///
/// An unencoded '/', '?', or '#' inside a credential ends the authority at
/// that separator, and an `@` later in the URL is what the remainder of that
/// credential was attached to: `https://ci:1234/@mirror.invalid/…` cuts the
/// authority to `ci:1234` — exactly a `host:port`, because the password is
/// numeric — and `https://secret-token/@mirror.invalid/…` cuts it to a
/// colon-free `secret-token`, because userinfo can be a bare username.
/// A colon in the fragment therefore cannot be the test: username-only
/// credentials never carry one, and nothing that survives the truncation
/// (colon or not, numeric or not) is evidence the fragment is a host. The
/// fragment's shape plays no part in the decision.
///
/// The one witness that clears the ambiguity is an `@` stripped from *inside*
/// the extracted authority (`stripped_userinfo`): the cut then fell past a
/// complete `user[:password]@host` span, so the parser consumed the whole
/// authority and a later `@` is path or query content. Without that witness
/// — an authority with no `@` of its own ahead of a tail that has one — the
/// trailing `@` cannot be told apart from a severed username, and the URL
/// fails closed. That is deliberately broad: a scoped-package path on a
/// userinfo-free mirror (`https://npm.example/@scope/repo`) is
/// indistinguishable from a severed username and withholds too, while the
/// same scoped path under real userinfo (`https://ci@npm.example/@scope/…`)
/// keeps its label.
fn could_be_severed_credential(tail: &str, stripped_userinfo: bool) -> bool {
    !stripped_userinfo && tail.contains('@')
}

/// True when `candidate` can only be read as a host or `host:port` — never as
/// a truncated `user:password` credential.
///
/// An unencoded separator inside a password leaves the rest of the credential
/// in the port position (`ci:secret-token`), and a password that is itself
/// hostname-shaped is indistinguishable from a host, so validation is on the
/// parts a credential cannot share with a legal endpoint: a non-bracketed
/// host carries at most one ':' and only as a numeric u16 port, and an IPv6
/// literal must sit in brackets with at most a port behind them.
fn parses_as_host(candidate: &str) -> bool {
    if candidate.is_empty() {
        return false;
    }
    if let Some(bracketed) = candidate.strip_prefix('[') {
        let Some(close) = bracketed.find(']') else {
            return false;
        };
        let after = &bracketed[close + 1..];
        return match after.strip_prefix(':') {
            Some(port) => is_valid_port(port),
            None => after.is_empty(),
        };
    }
    match candidate.split_once(':') {
        Some((host, port)) => !host.is_empty() && is_valid_port(port),
        None => true,
    }
}

/// A port is a non-empty run of ASCII digits that fits a u16.
fn is_valid_port(port: &str) -> bool {
    !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok()
}

/// Repository URL as it may appear in user-facing text: the scheme and
/// authority of an http(s) location, with credentials, path, and query
/// dropped. A `file://` location keeps its path — it has no secrets to
/// hide there and the tree it names is what operators need to see — but
/// its authority's userinfo is stripped the same way, since
/// `validate_base_url` accepts a credentialed file rest. An http(s) URL
/// whose authority cannot be isolated is reported as an opaque
/// `<repository>`.
pub(crate) fn repository_url_label(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        endpoint_without_credentials(url).unwrap_or_else(|| "<repository>".to_string())
    } else if url.starts_with("file://") {
        file_url_without_userinfo(url)
    } else {
        url.to_string()
    }
}

/// Strips `user[:pass]@` from a `file://` URL's authority while keeping its
/// path, so the label stays useful without echoing credentials. A file
/// authority is held to the same host[:port] test as an http(s) one — an
/// empty authority names a local tree and stays verbatim, but a non-empty
/// rest that cannot be read as a host is withheld rather than echoed.
fn file_url_without_userinfo(url: &str) -> String {
    let Some(sep) = url.find("://") else {
        return url.to_string();
    };
    let remainder = &url[sep + 3..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let tail = &remainder[authority_end..];
    match authority.rfind('@') {
        // No authority at all (`file:///srv/…`) or a host-shaped one with no
        // severed-credential ambiguity behind it: the location is already
        // safe to show in full.
        None if authority.is_empty()
            || (parses_as_host(authority) && !could_be_severed_credential(tail, false)) =>
        {
            url.to_string()
        }
        // A rest that could hide a `user:password` cannot be labelled safely.
        None => "<repository>".to_string(),
        // Userinfo with no host after it cannot be labelled safely.
        Some(at) if !parses_as_host(&authority[at + 1..]) => "<repository>".to_string(),
        Some(at) => format!(
            "{}{}{}",
            &url[..sep + 3],
            &authority[at + 1..],
            &remainder[authority_end..]
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use anolisa_core::adapter::claim::{AdapterClaim, ClaimStatus, DriverPayload, OpenClawClaim};
    use anolisa_core::state::{InstalledObject, ObjectStatus, Ownership, SubscriptionScope};

    use crate::commands::state_view::{ScopedStateRoot, StateScope, UnavailableStateRoot};

    /// A `file://` authority can carry userinfo just like an http(s) one —
    /// `validate_base_url` accepts any non-empty file rest — so a label that
    /// echoes it leaks the credential into user-facing text.
    #[test]
    fn repository_url_label_does_not_echo_file_url_userinfo() {
        let label = repository_url_label("file://deploy:secret-token@nas.example/share/repo");
        assert_ne!(
            label, "file://deploy:secret-token@nas.example/share/repo",
            "a file URL's userinfo must not be echoed: got `{label}`"
        );
        assert_eq!(label, "file://nas.example/share/repo");
    }

    /// Scoped-package-style paths put the `@` only in the path — and that is
    /// exactly the shape a severed username-only credential leaves behind
    /// (`https://secret-token/@mirror.invalid/…`): a colon-free authority
    /// ahead of an `@` in the tail. Nothing distinguishes the two, so both
    /// fail closed; the witness that keeps a scoped path labelable is real
    /// userinfo in the authority, which proves the parser consumed the whole
    /// authority before the path began.
    #[test]
    fn repository_url_label_fails_closed_when_only_the_path_has_an_at() {
        assert_eq!(
            repository_url_label("https://npm.example/@scope/repo/v1/index.toml"),
            "<repository>",
            "an `@` in the tail without an authority `@` to witness a complete \
             userinfo span is indistinguishable from a severed username"
        );
        assert_eq!(
            repository_url_label("file://nas.example/share/@scope/repo"),
            "<repository>",
            "the file label carries the same ambiguity and fails closed too"
        );
        // The witness: real userinfo inside the authority means the cut fell
        // past a complete `user[:password]@host` span, so the later `@` is
        // path content and the host keeps its label.
        assert_eq!(
            repository_url_label("https://ci@npm.example/@scope/repo/v1/index.toml"),
            "https://npm.example"
        );
        assert_eq!(
            repository_url_label("file://ci@nas.example/share/@scope/repo"),
            "file://nas.example/share/@scope/repo"
        );
    }

    /// A password with an unencoded '/', '?', or '#' ends the authority
    /// early, so the truncated credential sits where a host[:port] belongs
    /// (`https://ci:secret-token/@mirror.invalid/…` parses the authority as
    /// `ci:secret-token`). Dropping the path-wide `@` rejection must not let
    /// that shape through: the authority is shown only when it reads as a
    /// legal host or host:port, and the whole password — and any recognizable
    /// prefix of it — must be absent from the label.
    #[test]
    fn repository_url_label_fails_closed_on_separator_passwords() {
        for url in [
            "https://ci:secret-token/@mirror.invalid/release.toml",
            "https://ci:secret-token?@mirror.invalid/release.toml",
            "https://ci:secret-token#@mirror.invalid/release.toml",
        ] {
            let label = repository_url_label(url);
            assert_eq!(
                label, "<repository>",
                "an authority that cannot be read as a host must stay opaque: {url}"
            );
            assert!(
                !label.contains("secret-token"),
                "the full password leaked into the label for {url}: {label}"
            );
            assert!(
                !label.contains("secret") && !label.contains("token"),
                "a recognizable password prefix leaked into the label for {url}: {label}"
            );
        }
    }

    /// Host:port and IPv6 forms stay labelable: the added authority check
    /// narrows only to shapes a credential can hide in, so an ordinary
    /// endpoint with a port keeps its label.
    #[test]
    fn repository_url_label_keeps_host_port_and_ipv6_shapes() {
        assert_eq!(
            repository_url_label("https://mirror.invalid:8080/release.toml"),
            "https://mirror.invalid:8080"
        );
        assert_eq!(
            repository_url_label("https://[2001:db8::1]/release.toml"),
            "https://[2001:db8::1]"
        );
        assert_eq!(
            repository_url_label("https://[2001:db8::1]:8443/release.toml"),
            "https://[2001:db8::1]:8443"
        );
        // Ports must be numeric u16 values; a password-shaped "port" fails
        // closed just like the separator cases.
        assert_eq!(
            repository_url_label("https://mirror.invalid:notaport/release.toml"),
            "<repository>"
        );
        assert_eq!(
            repository_url_label("https://mirror.invalid:99999/release.toml"),
            "<repository>"
        );
    }

    /// A numeric password truncated at an unencoded separator is shaped
    /// exactly like a legal `host:port` — `https://ci:1234/@mirror.invalid/…`
    /// cuts the authority to `ci:1234` — so the fragment's shape cannot clear
    /// the ambiguity a trailing `@` introduces and the label must stay
    /// opaque: username and password both disappear. An ordinary
    /// `host:port` in front of an `@`-scoped path carries the same
    /// ambiguity (the "port" is indistinguishable from a severed password
    /// prefix) and withholds too.
    #[test]
    fn repository_url_label_fails_closed_on_numeric_separator_passwords() {
        for url in [
            "https://ci:1234/@mirror.invalid/release.toml",
            "https://ci:1234?@mirror.invalid/release.toml",
            "https://ci:1234#@mirror.invalid/release.toml",
            "https://mirror.invalid:8080/@scope/repo/index.toml",
        ] {
            let label = repository_url_label(url);
            assert_eq!(
                label, "<repository>",
                "an authority whose tail carries an '@' must stay opaque: {url}"
            );
            assert!(
                !label.contains("1234") && !label.contains("8080") && !label.contains("ci:1234"),
                "the numeric password leaked into the label for {url}: {label}"
            );
        }
    }

    /// Username-only userinfo carries no colon, so the severed fragment reads
    /// as a bare host (`https://secret-token/@mirror.invalid/…` cuts the
    /// authority to `secret-token`): the presence or absence of a colon in
    /// what remains cannot be the disambiguator, and the label must stay
    /// opaque for the same reason the colon-bearing shapes do.
    #[test]
    fn repository_url_label_fails_closed_on_username_only_tokens() {
        for url in [
            "https://secret-token/@mirror.invalid/release.toml",
            "https://secret-token?@mirror.invalid/release.toml",
            "https://secret-token#@mirror.invalid/release.toml",
        ] {
            let label = repository_url_label(url);
            assert_eq!(
                label, "<repository>",
                "a username-only token cut off by a separator must stay opaque: {url}"
            );
            assert!(
                !label.contains("secret-token")
                    && !label.contains("secret")
                    && !label.contains("token"),
                "the username-only token leaked into the label for {url}: {label}"
            );
        }
    }

    /// The same username-only shape against a `file://` rest: the token reads
    /// as the file authority's host and the raw label would echo the whole
    /// URL — token included — verbatim into raw-backend error messages.
    #[test]
    fn repository_url_label_fails_closed_on_username_only_tokens_in_file_rests() {
        for url in [
            "file://deploy-token/@nas.example/share/repo",
            "file://deploy-token?@nas.example/share/repo",
            "file://deploy-token#@nas.example/share/repo",
        ] {
            let label = repository_url_label(url);
            assert_eq!(
                label, "<repository>",
                "a file rest cut off by a separator must stay opaque: {url}"
            );
            assert!(
                !label.contains("deploy-token")
                    && !label.contains("deploy")
                    && !label.contains("token"),
                "the username-only token leaked into the file label for {url}: {label}"
            );
        }
    }

    /// The same separator trick against a `file://` rest: a numeric
    /// `deploy:1234` reads as `host:port`, so the raw label withholds the
    /// whole URL instead of echoing the severed credential verbatim into
    /// raw-backend error messages.
    #[test]
    fn repository_url_label_fails_closed_on_numeric_separator_passwords_in_file_rests() {
        for url in [
            "file://deploy:1234/@nas.example/share/repo",
            "file://deploy:1234?@nas.example/share/repo",
            "file://deploy:1234#@nas.example/share/repo",
        ] {
            let label = repository_url_label(url);
            assert_eq!(
                label, "<repository>",
                "a file rest whose tail carries an '@' must stay opaque: {url}"
            );
            assert!(
                !label.contains("1234") && !label.contains("deploy"),
                "the numeric password leaked into the file label for {url}: {label}"
            );
        }
        // Covered shapes kept: no authority means a local tree, and a
        // colon-free host names a remote one; both stay verbatim.
        assert_eq!(
            repository_url_label("file:///srv/anolisa/repo"),
            "file:///srv/anolisa/repo"
        );
        assert_eq!(
            repository_url_label("file://nas.example/share/repo"),
            "file://nas.example/share/repo"
        );
    }

    /// The same separator trick against a `file://` rest: a non-empty file
    /// authority that cannot be read as a host is withheld rather than
    /// echoed, while a userinfo-free local path keeps its verbatim label.
    #[test]
    fn repository_url_label_fails_closed_on_separator_passwords_in_file_rests() {
        for url in [
            "file://deploy:secret-token/@nas.example/share/repo",
            "file://deploy:secret-token?@nas.example/share/repo",
            "file://deploy:secret-token#@nas.example/share/repo",
        ] {
            let label = repository_url_label(url);
            assert_eq!(
                label, "<repository>",
                "a file rest that cannot be read as a host must stay opaque: {url}"
            );
            assert!(
                !label.contains("secret-token") && !label.contains("secret"),
                "the password leaked into the file label for {url}: {label}"
            );
        }
        // Covered shape kept: no authority means a local tree, shown in full.
        assert_eq!(
            repository_url_label("file:///srv/anolisa/repo"),
            "file:///srv/anolisa/repo"
        );
    }

    /// The shapes "fix(anolisa): redact raw repository URLs" (9b019aab5)
    /// covered keep their labels: http(s) userinfo reduced to scheme and
    /// authority, an http(s) authority that cannot be isolated reported as an
    /// opaque `<repository>`, and a userinfo-free `file://` path kept intact.
    #[test]
    fn repository_url_label_keeps_covered_shapes() {
        assert_eq!(
            repository_url_label("https://user:secret@repo.example.internal/private/v1/index.toml"),
            "https://repo.example.internal"
        );
        assert_eq!(
            repository_url_label("http://user@repo.example.internal:8080/v1"),
            "http://repo.example.internal:8080"
        );
        assert_eq!(repository_url_label("https://"), "<repository>");
        assert_eq!(
            repository_url_label("file:///srv/anolisa/repo"),
            "file:///srv/anolisa/repo"
        );
    }

    #[test]
    fn remediation_commands_preserve_the_explicit_scope() {
        assert_eq!(
            scoped_component_command(InstallationScope::System, "repair", "cosh"),
            "sudo anolisa --install-mode system repair cosh"
        );
        assert_eq!(
            scoped_component_command(InstallationScope::User { uid: 1000 }, "repair", "cosh"),
            "anolisa --install-mode user repair cosh"
        );
    }

    fn test_component(name: &str) -> InstalledObject {
        InstalledObject {
            kind: ObjectKind::Component,
            name: name.to_string(),
            version: "1.0.0".to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: None,
            raw_package: None,
            install_backend: Some("raw".to_string()),
            ownership: Some(Ownership::RawManaged),
            rpm_metadata: None,
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            last_operation_id: None,
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

    fn state_with_objects(objects: Vec<InstalledObject>) -> StateStore {
        let migration = anolisa_core::state_migration::migrate_state(
            &objects,
            anolisa_core::domain::InstallationScope::System,
        );
        assert!(
            migration.quarantined.is_empty(),
            "fixtures must migrate cleanly"
        );
        let mut store = StateStore::empty();
        store.installations = migration.active;
        store
    }

    fn state_with_quarantined_object(mut object: InstalledObject) -> StateStore {
        object.install_backend = None;
        object.ownership = None;
        object.managed = false;
        let migration = anolisa_core::state_migration::migrate_state(
            &[object],
            anolisa_core::domain::InstallationScope::System,
        );
        assert!(migration.active.is_empty());
        assert_eq!(migration.quarantined.len(), 1);
        let mut store = StateStore::empty();
        store.quarantined = migration.quarantined;
        store
    }

    fn scoped_view(user_state: StateStore, system_state: StateStore) -> StateView {
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

    fn view_with_unavailable_system(user_state: StateStore) -> StateView {
        let mut view = scoped_view(user_state, StateStore::empty());
        let system_root = view.visible_roots.pop().expect("system root");
        view.unavailable_roots.push(UnavailableStateRoot {
            scope: StateScope::System,
            state_path: system_root.state_path,
            reason: "future state schema".to_string(),
        });
        view
    }

    fn test_adapter_claim(component: &str) -> AdapterClaim {
        AdapterClaim {
            claim_schema: 1,
            component: component.to_string(),
            framework: "openclaw".to_string(),
            plugin_id: None,
            adapter_type: None,
            enabled_at: "2026-01-01T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/tmp/adapter-resource"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: 1,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: Vec::new(),
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "state".to_string(),
                plugin_resource: "plugin".to_string(),
                skill_resources: Vec::new(),
                config_resources: Vec::new(),
            }),
        }
    }

    #[test]
    fn reject_visible_non_writable_component_blocks_read_only_system_view() {
        let view = scoped_view(
            StateStore::empty(),
            state_with_objects(vec![test_component("system-tool")]),
        );

        let err = reject_visible_non_writable_component_from_view(
            &view,
            "uninstall system-tool",
            "system-tool",
        )
        .expect_err("system component must be visible but read-only");

        match err {
            CliError::PermissionDenied { reason, hint, .. } => {
                assert!(reason.contains("system-tool"), "reason: {reason}");
                assert!(reason.contains("system-scope"), "reason: {reason}");
                assert!(
                    hint.as_deref()
                        .is_some_and(|h| h.contains("--install-mode system")),
                    "hint: {hint:?}",
                );
            }
            other => panic!("expected permission error, got {other:?}"),
        }
    }

    #[test]
    fn reject_visible_non_writable_component_keeps_missing_targets_unchanged() {
        let view = scoped_view(StateStore::empty(), StateStore::empty());

        reject_visible_non_writable_component_from_view(
            &view,
            "forget missing-tool",
            "missing-tool",
        )
        .expect("missing targets should continue into the existing not-installed path");
    }

    #[test]
    fn install_keeps_system_exact_identity_but_targets_writable_scope() {
        let view = scoped_view(
            state_with_objects(vec![test_component("cosh")]),
            state_with_objects(vec![test_component("legacy-name")]),
        );

        let (target, resolved_view, exact_identity) = resolve_lifecycle_target_from_view(
            "legacy-name",
            "install legacy-name",
            false,
            view,
            || Ok("cosh".to_string()),
        )
        .expect("a system record must not block a user-scope install");

        assert_eq!(target, "legacy-name");
        assert!(exact_identity);
        assert!(
            resolved_view
                .writable
                .state
                .find(ObjectKind::Component, "cosh")
                .is_some(),
            "the existing user alias target remains a separate record",
        );
        assert!(
            resolved_view
                .writable
                .state
                .find(ObjectKind::Component, "legacy-name")
                .is_none(),
            "install planning must see the exact identity as fresh in user scope",
        );
    }

    #[test]
    fn mutation_rejects_system_exact_before_repo_alias() {
        let view = scoped_view(
            state_with_objects(vec![test_component("cosh")]),
            state_with_quarantined_object(test_component("legacy-name")),
        );
        let alias_consulted = std::cell::Cell::new(false);

        let err = resolve_lifecycle_target_from_view(
            "legacy-name",
            "forget legacy-name",
            true,
            view,
            || {
                alias_consulted.set(true);
                Ok("cosh".to_string())
            },
        )
        .expect_err("the read-only system identity must win over a user alias target");

        assert_eq!(err.code(), "PERMISSION_DENIED");
        assert!(
            !alias_consulted.get(),
            "exact identity must skip alias lookup"
        );
        assert!(err.reason().contains("legacy-name"));
        assert!(err.reason().contains("system-scope"));
    }

    #[test]
    fn mutation_rejects_system_package_identity_before_repo_alias() {
        let mut system_component = test_component("legacy-name");
        system_component.raw_package = Some("copilot-shell".to_string());
        let view = scoped_view(
            state_with_objects(vec![test_component("cosh")]),
            state_with_objects(vec![system_component]),
        );

        let result = resolve_lifecycle_target_from_view(
            "copilot-shell",
            "forget copilot-shell",
            true,
            view,
            || Ok("cosh".to_string()),
        );

        let err = result.expect_err("the visible system package identity must win");
        assert_eq!(err.code(), "PERMISSION_DENIED");
    }

    #[test]
    fn mutation_resolves_writable_package_identity_before_repo_alias() {
        let mut package_owner = test_component("user-tool");
        package_owner.raw_package = Some("legacy-name".to_string());
        let view = scoped_view(
            state_with_objects(vec![package_owner, test_component("cosh")]),
            StateStore::empty(),
        );
        let alias_consulted = std::cell::Cell::new(false);

        let (target, _, state_identity) = resolve_lifecycle_target_from_view(
            "legacy-name",
            "forget legacy-name",
            true,
            view,
            || {
                alias_consulted.set(true);
                Ok("cosh".to_string())
            },
        )
        .expect("the writable package owner must be resolved directly");

        assert_eq!(target, "user-tool");
        assert!(state_identity);
        assert!(
            !alias_consulted.get(),
            "state identity must skip repo alias"
        );
    }

    #[test]
    fn mutation_rejects_package_identity_claimed_by_multiple_components() {
        let mut first = test_component("first");
        first.raw_package = Some("shared-package".to_string());
        let mut second = test_component("second");
        second.raw_package = Some("shared-package".to_string());
        let view = scoped_view(state_with_objects(vec![first, second]), StateStore::empty());

        let err = resolve_lifecycle_target_from_view(
            "shared-package",
            "forget shared-package",
            true,
            view,
            || Ok("repo-target".to_string()),
        )
        .expect_err("ambiguous state package identity must fail closed");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("first"));
        assert!(err.reason().contains("second"));
    }

    #[test]
    fn mutation_includes_quarantine_in_package_identity_ambiguity() {
        let mut active = test_component("active");
        active.raw_package = Some("shared-package".to_string());
        let mut quarantine = state_with_quarantined_object(test_component("quarantined-system"));
        quarantine.quarantined[0].record.raw_package = Some("shared-package".to_string());
        let view = scoped_view(state_with_objects(vec![active]), quarantine);

        let err = resolve_lifecycle_target_from_view(
            "shared-package",
            "repair shared-package",
            true,
            view,
            || Ok("repo-target".to_string()),
        )
        .expect_err("quarantined package claims must remain authoritative");

        assert_eq!(err.code(), "INVALID_ARGUMENT");
        assert!(err.reason().contains("active"));
        assert!(err.reason().contains("quarantined-system"));
    }

    #[test]
    fn adapter_target_keeps_visible_system_exact_before_repo_alias() {
        let view = scoped_view(
            state_with_objects(vec![test_component("cosh")]),
            state_with_objects(vec![test_component("legacy-name")]),
        );
        let alias_consulted = std::cell::Cell::new(false);

        let (target, _) = resolve_adapter_target_from_view(
            "legacy-name",
            "adapter enable legacy-name",
            view,
            || {
                alias_consulted.set(true);
                Ok("cosh".to_string())
            },
        )
        .expect("system component must remain a visible adapter source");

        assert_eq!(target, "legacy-name");
        assert!(!alias_consulted.get());
    }

    #[test]
    fn adapter_target_rejects_alias_when_system_visibility_is_incomplete() {
        let view = view_with_unavailable_system(state_with_objects(vec![test_component("cosh")]));

        let err = resolve_adapter_target_from_view(
            "legacy-name",
            "adapter disable legacy-name",
            view,
            || Ok("cosh".to_string()),
        )
        .expect_err("incomplete visibility must block adapter alias inference");

        assert!(err.reason().contains("visible state is incomplete"));
        assert!(err.reason().contains("legacy-name"));
    }

    #[test]
    fn adapter_target_keeps_exact_receipt_when_system_visibility_is_incomplete() {
        let mut user_state = state_with_objects(vec![test_component("cosh")]);
        user_state.upsert_adapter_claim(test_adapter_claim("legacy-name"));
        let view = view_with_unavailable_system(user_state);
        let alias_consulted = std::cell::Cell::new(false);

        let (target, _) = resolve_adapter_target_from_view(
            "legacy-name",
            "adapter disable legacy-name",
            view,
            || {
                alias_consulted.set(true);
                Ok("cosh".to_string())
            },
        )
        .expect("an exact local receipt remains safely addressable");

        assert_eq!(target, "legacy-name");
        assert!(!alias_consulted.get());
    }

    /// Verify that `package_datadir()` is wired into the system-mode
    /// manager: an RPM-installed contract under `{prefix}/usr/share/anolisa`
    /// must be discoverable via scan when the primary datadir is
    /// `{prefix}/usr/local/share/anolisa`.
    ///
    /// This exercises the same wiring path as `build_adapter_manager()`
    /// for system mode without needing a full `CliContext`.
    #[test]
    fn system_mode_wiring_discovers_package_datadir_contract() {
        use anolisa_core::adapter::manager::AdapterManager;
        use anolisa_core::state::{
            InstallMode as StateInstallMode, InstalledObject, InstalledState, ObjectKind,
            ObjectStatus, Ownership, SubscriptionScope,
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let prefix = tmp.path().to_path_buf();
        let layout = FsLayout::system(Some(prefix));

        // Simulate the system-mode wiring from build_adapter_manager().
        let mut manager = AdapterManager::new(
            layout.clone(),
            Some(tmp.path().to_path_buf()),
            "test".into(),
        );
        if let Some(pkg_dd) = layout.package_datadir() {
            manager.push_primary_datadir_root(pkg_dd);
        }

        // Seed state: sec-core adopted.
        let state_dir = &layout.state_dir;
        let mut state = InstalledState {
            install_mode: StateInstallMode::System,
            prefix: layout.prefix.clone(),
            ..InstalledState::default()
        };
        state.objects.push(InstalledObject {
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
            installed_at: "2026-06-23T00:00:00Z".to_string(),
            last_operation_id: None,
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
        });
        std::fs::create_dir_all(state_dir).expect("mkdir state");
        crate::test_support::write_legacy_state(&state, &state_dir.join("installed.toml"))
            .expect("save state");

        // Write contract under the package datadir (NOT local datadir).
        let package_datadir = layout.package_datadir().expect("package_datadir");
        let contract_dir = package_datadir.join("components").join("sec-core");
        std::fs::create_dir_all(&contract_dir).expect("mkdir contract");
        std::fs::write(
            contract_dir.join("component.toml"),
            r#"
[component]
name = "sec-core"
version = "0.1.0"
layer = "runtime"

[[adapters]]
framework = "openclaw"
adapter_type = "plugin"
plugin_id = "sec-core"
dest = "{datadir}/adapters/sec-core/openclaw/"
"#,
        )
        .expect("write contract");

        let report = manager.scan().expect("scan");
        let entry = report
            .entries
            .iter()
            .find(|e| e.component == "sec-core" && e.framework == "openclaw");
        assert!(
            entry.is_some_and(|e| e.declared),
            "system-mode wiring must discover contract under package_datadir; \
             entries: {:?}, warnings: {:?}",
            report
                .entries
                .iter()
                .map(|e| (&e.component, &e.framework, e.declared))
                .collect::<Vec<_>>(),
            report.warnings,
        );
    }

    #[test]
    fn status_is_enabled_excludes_disabled_failed_and_unknown() {
        assert!(status_is_enabled("installed"));
        assert!(status_is_enabled("degraded"));
        assert!(status_is_enabled("adopted"));
        assert!(!status_is_enabled("disabled"));
        assert!(!status_is_enabled("failed"));
        assert!(!status_is_enabled("not_installed"));
        assert!(!status_is_enabled(""));
    }

    mod migrate_v3_symlinks_tests {
        use anolisa_core::state::{
            FileOwner, InstalledObject, InstalledState, ObjectKind, ObjectStatus, OwnedFile,
            OwnedFileKind, Ownership, SubscriptionScope,
        };
        use anolisa_core::state_store::StateStore;
        use anolisa_platform::fs_layout::FsLayout;
        use sha2::{Digest, Sha256};

        fn hex_lower(bytes: &[u8]) -> String {
            bytes.iter().fold(String::new(), |mut s, b| {
                use std::fmt::Write;
                let _ = write!(s, "{b:02x}");
                s
            })
        }

        fn sample_object(name: &str, files: Vec<OwnedFile>) -> InstalledObject {
            InstalledObject {
                kind: ObjectKind::Component,
                name: name.to_string(),
                version: "1.0.0".to_string(),
                status: ObjectStatus::Installed,
                manifest_digest: None,
                distribution_source: None,
                raw_package: None,
                install_backend: None,
                ownership: Some(Ownership::RawManaged),
                rpm_metadata: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
                last_operation_id: None,
                managed: true,
                adopted: false,
                subscription_scope: SubscriptionScope::None,
                enabled_features: Vec::new(),
                component_refs: Vec::new(),
                files,
                external_modified_files: Vec::new(),
                services: Vec::new(),
                health: Vec::new(),
                provisioned_packages: Vec::new(),
            }
        }

        fn v3_state() -> InstalledState {
            InstalledState {
                schema_version: 3,
                ..Default::default()
            }
        }

        /// Persist a v3-shaped legacy state and load it back as the v5 store,
        /// so `migrated_from_legacy()` holds — the gate `migrate_v3_symlinks`
        /// checks in production.
        fn seed_v3_store(layout: &FsLayout, objects: Vec<InstalledObject>) -> StateStore {
            let mut state = v3_state();
            for object in objects {
                state.objects.push(object);
            }
            std::fs::create_dir_all(&layout.state_dir).expect("mkdir state dir");
            let path = layout.state_dir.join("installed.toml");
            crate::test_support::write_legacy_state(&state, &path).expect("seed v3 state");
            StateStore::load(&path, 0).expect("load store")
        }

        fn owned_files<'a>(store: &'a StateStore, name: &str) -> &'a [OwnedFile] {
            match &store
                .find(ObjectKind::Component, name)
                .expect("component in store")
                .binding
            {
                anolisa_core::domain::ProviderBinding::Owned { artifact } => &artifact.files,
                other => panic!("expected owned component, found {other:?}"),
            }
        }

        fn write_manifest(layout: &FsLayout, component: &str, toml: &str) {
            let dir = layout
                .state_dir
                .join(super::INSTALLED_COMPONENT_MANIFESTS_SUBDIR)
                .join(component);
            std::fs::create_dir_all(&dir).expect("mkdir manifest dir");
            std::fs::write(dir.join(super::INSTALLED_COMPONENT_MANIFEST_FILE), toml)
                .expect("write manifest");
        }

        #[test]
        #[cfg(unix)]
        fn migrate_upgrades_manifest_declared_symlink() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            std::fs::create_dir_all(&layout.bin_dir).expect("mkdir bindir");
            std::fs::create_dir_all(&layout.libexec_dir).expect("mkdir libexecdir");

            let referent = layout.libexec_dir.join("tokenless").join("rtk");
            std::fs::create_dir_all(referent.parent().unwrap()).expect("mkdir referent parent");
            let payload = b"binary-payload";
            std::fs::write(&referent, payload).expect("write referent");

            let link = layout.bin_dir.join("rtk");
            std::os::unix::fs::symlink(&referent, &link).expect("symlink");

            let sha = hex_lower(&Sha256::digest(payload));

            write_manifest(
                &layout,
                "tokenless",
                r#"
[component]
name = "tokenless"
version = "1.0.0"
layer = "runtime"

[[install.files]]
source = "{libexecdir}/tokenless/rtk"
dest = "{bindir}/rtk"
type = "symlink"
"#,
            );

            let owned = OwnedFile {
                path: link.clone(),
                owner: FileOwner::Anolisa,
                sha256: Some(sha),
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            };
            let mut store = seed_v3_store(&layout, vec![sample_object("tokenless", vec![owned])]);

            let count = super::migrate_v3_symlinks(&mut store, &layout);
            assert_eq!(count, 1);

            let file = &owned_files(&store, "tokenless")[0];
            assert_eq!(file.kind, OwnedFileKind::Symlink);
            assert_eq!(file.referent.as_deref(), Some(referent.as_path()));
            assert!(file.sha256.is_none());
        }

        #[test]
        #[cfg(unix)]
        fn migrate_skips_when_disk_not_symlink() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            std::fs::create_dir_all(&layout.bin_dir).expect("mkdir bindir");
            std::fs::create_dir_all(&layout.libexec_dir).expect("mkdir libexecdir");

            let referent = layout.libexec_dir.join("tokenless").join("rtk");
            std::fs::create_dir_all(referent.parent().unwrap()).expect("mkdir referent parent");
            std::fs::write(&referent, b"binary").expect("write referent");

            let regular = layout.bin_dir.join("rtk");
            std::fs::write(&regular, b"regular-file").expect("write regular");

            write_manifest(
                &layout,
                "tokenless",
                r#"
[component]
name = "tokenless"
version = "1.0.0"
layer = "runtime"

[[install.files]]
source = "{libexecdir}/tokenless/rtk"
dest = "{bindir}/rtk"
type = "symlink"
"#,
            );

            let owned = OwnedFile {
                path: regular,
                owner: FileOwner::Anolisa,
                sha256: None,
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            };
            let mut store = seed_v3_store(&layout, vec![sample_object("tokenless", vec![owned])]);

            let count = super::migrate_v3_symlinks(&mut store, &layout);
            assert_eq!(count, 0);
            assert_eq!(
                owned_files(&store, "tokenless")[0].kind,
                OwnedFileKind::File
            );
        }

        #[test]
        #[cfg(unix)]
        fn migrate_skips_when_readlink_mismatches() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            std::fs::create_dir_all(&layout.bin_dir).expect("mkdir bindir");
            std::fs::create_dir_all(&layout.libexec_dir).expect("mkdir libexecdir");

            let expected_referent = layout.libexec_dir.join("tokenless").join("rtk");
            std::fs::create_dir_all(expected_referent.parent().unwrap()).expect("mkdir");
            std::fs::write(&expected_referent, b"binary").expect("write expected");

            let wrong_target = layout.libexec_dir.join("attacker").join("evil");
            std::fs::create_dir_all(wrong_target.parent().unwrap()).expect("mkdir");
            std::fs::write(&wrong_target, b"evil").expect("write evil");

            let link = layout.bin_dir.join("rtk");
            std::os::unix::fs::symlink(&wrong_target, &link).expect("symlink");

            write_manifest(
                &layout,
                "tokenless",
                r#"
[component]
name = "tokenless"
version = "1.0.0"
layer = "runtime"

[[install.files]]
source = "{libexecdir}/tokenless/rtk"
dest = "{bindir}/rtk"
type = "symlink"
"#,
            );

            let owned = OwnedFile {
                path: link,
                owner: FileOwner::Anolisa,
                sha256: None,
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            };
            let mut store = seed_v3_store(&layout, vec![sample_object("tokenless", vec![owned])]);

            let count = super::migrate_v3_symlinks(&mut store, &layout);
            assert_eq!(count, 0);
            assert_eq!(
                owned_files(&store, "tokenless")[0].kind,
                OwnedFileKind::File
            );
        }

        #[test]
        #[cfg(unix)]
        fn migrate_skips_when_sha256_mismatches() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            std::fs::create_dir_all(&layout.bin_dir).expect("mkdir bindir");
            std::fs::create_dir_all(&layout.libexec_dir).expect("mkdir libexecdir");

            let referent = layout.libexec_dir.join("tokenless").join("rtk");
            std::fs::create_dir_all(referent.parent().unwrap()).expect("mkdir");
            std::fs::write(&referent, b"correct-payload").expect("write referent");

            let link = layout.bin_dir.join("rtk");
            std::os::unix::fs::symlink(&referent, &link).expect("symlink");

            write_manifest(
                &layout,
                "tokenless",
                r#"
[component]
name = "tokenless"
version = "1.0.0"
layer = "runtime"

[[install.files]]
source = "{libexecdir}/tokenless/rtk"
dest = "{bindir}/rtk"
type = "symlink"
"#,
            );

            let owned = OwnedFile {
                path: link,
                owner: FileOwner::Anolisa,
                sha256: Some("deadbeefdeadbeef".to_string()),
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            };
            let mut store = seed_v3_store(&layout, vec![sample_object("tokenless", vec![owned])]);

            let count = super::migrate_v3_symlinks(&mut store, &layout);
            assert_eq!(count, 0);
            assert_eq!(
                owned_files(&store, "tokenless")[0].kind,
                OwnedFileKind::File
            );
        }

        #[test]
        fn migrate_skips_when_manifest_missing() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            std::fs::create_dir_all(&layout.bin_dir).expect("mkdir bindir");

            let owned = OwnedFile {
                path: layout.bin_dir.join("rtk"),
                owner: FileOwner::Anolisa,
                sha256: None,
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            };
            let mut store = seed_v3_store(&layout, vec![sample_object("tokenless", vec![owned])]);

            let count = super::migrate_v3_symlinks(&mut store, &layout);
            assert_eq!(count, 0);
            assert_eq!(
                owned_files(&store, "tokenless")[0].kind,
                OwnedFileKind::File
            );
        }

        #[test]
        fn migrate_skips_traversal_component_name() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            std::fs::create_dir_all(&layout.bin_dir).expect("mkdir bindir");

            let owned = OwnedFile {
                path: layout.bin_dir.join("rtk"),
                owner: FileOwner::Anolisa,
                sha256: None,
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            };
            let mut store =
                seed_v3_store(&layout, vec![sample_object("../../../etc", vec![owned])]);

            let count = super::migrate_v3_symlinks(&mut store, &layout);
            assert_eq!(count, 0);
        }

        #[test]
        #[cfg(unix)]
        /// A native v5 store (not derived from a legacy file) never runs the
        /// symlink upgrade — its records were written by code that already
        /// records symlinks as symlinks.
        #[allow(clippy::items_after_statements)]
        fn migrate_skips_native_v5_store() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
            std::fs::create_dir_all(&layout.bin_dir).expect("mkdir bindir");
            std::fs::create_dir_all(&layout.libexec_dir).expect("mkdir libexecdir");

            let referent = layout.libexec_dir.join("tokenless").join("rtk");
            std::fs::create_dir_all(referent.parent().unwrap()).expect("mkdir referent parent");
            let payload = b"binary-payload";
            std::fs::write(&referent, payload).expect("write referent");

            let link = layout.bin_dir.join("rtk");
            std::os::unix::fs::symlink(&referent, &link).expect("symlink");

            let sha = hex_lower(&Sha256::digest(payload));

            write_manifest(
                &layout,
                "tokenless",
                r#"
[component]
name = "tokenless"
version = "1.0.0"
layer = "runtime"

[[install.files]]
source = "{libexecdir}/tokenless/rtk"
dest = "{bindir}/rtk"
type = "symlink"
"#,
            );

            let owned = OwnedFile {
                path: link.clone(),
                owner: FileOwner::Anolisa,
                sha256: Some(sha),
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            };
            // Round-trip through a *v5* file: save the migrated store, then
            // reload it — the reload is a native v5 load.
            let store = seed_v3_store(&layout, vec![sample_object("tokenless", vec![owned])]);
            let path = layout.state_dir.join("installed.toml");
            store.save(&path).expect("persist v5 store");
            let mut store = StateStore::load(&path, 0).expect("reload v5 store");
            assert!(!store.migrated_from_legacy());

            let count = super::migrate_v3_symlinks(&mut store, &layout);
            assert_eq!(count, 0);
            assert_eq!(
                owned_files(&store, "tokenless")[0].kind,
                OwnedFileKind::File
            );
        }
    }

    mod persist_policy {
        use super::super::{
            RepoConfigProvisioning, RepoPersistPolicy, enforce_repo_persist_policy,
        };
        use crate::response::CliError;
        use std::path::PathBuf;

        fn persist_failed() -> RepoConfigProvisioning {
            RepoConfigProvisioning::DownloadedPersistFailed {
                url: "https://example.com/repo.toml".to_string(),
                dest: PathBuf::from("/etc/anolisa/repo.toml"),
                reason: "permission denied".to_string(),
            }
        }

        /// Require policy: DownloadedPersistFailed is escalated to CliError::Runtime
        /// carrying the dest path and underlying reason.
        #[test]
        fn require_policy_rejects_persist_failure() {
            let err =
                enforce_repo_persist_policy(&persist_failed(), RepoPersistPolicy::Require, "list")
                    .expect_err("Require must reject persist failure");
            match err {
                CliError::Runtime { command, reason } => {
                    assert_eq!(command, "list");
                    assert!(
                        reason.contains("/etc/anolisa/repo.toml"),
                        "reason should carry dest: {reason}"
                    );
                    assert!(
                        reason.contains("permission denied"),
                        "reason should carry inner cause: {reason}"
                    );
                }
                other => panic!("expected Runtime, got {other:?}"),
            }
        }

        /// BestEffort policy: DownloadedPersistFailed is tolerated (Ok).
        #[test]
        fn best_effort_policy_tolerates_persist_failure() {
            enforce_repo_persist_policy(&persist_failed(), RepoPersistPolicy::BestEffort, "list")
                .expect("BestEffort must accept persist failure");
        }

        /// Neither policy touches non-persist-failure provisioning states.
        #[test]
        fn non_persist_failure_states_are_always_ok() {
            let downloaded = RepoConfigProvisioning::Downloaded {
                url: "https://example.com/repo.toml".to_string(),
                dest: PathBuf::from("/etc/anolisa/repo.toml"),
            };
            let existing = RepoConfigProvisioning::Existing;
            let dry_run = RepoConfigProvisioning::FetchedForDryRun {
                url: "https://example.com/repo.toml".to_string(),
                dest: PathBuf::from("/etc/anolisa/repo.toml"),
            };
            for provisioning in [&downloaded, &existing, &dry_run] {
                enforce_repo_persist_policy(provisioning, RepoPersistPolicy::Require, "install")
                    .expect("Require must pass through non-persist-failure states");
                enforce_repo_persist_policy(provisioning, RepoPersistPolicy::BestEffort, "list")
                    .expect("BestEffort must pass through non-persist-failure states");
            }
        }
    }
}
