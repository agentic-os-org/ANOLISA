//! Purge previews and shared lifecycle backup primitives.
//!
//! [`LifecyclePlan`] is the data-only purge preview. Uninstall planning and
//! execution use the typed planner/executor pipeline; purge remains plan-only
//! until manifest-driven config/cache/state discovery lands.
//!
//! Only ANOLISA-owned files are removable; external modifications are refused.
//! [`prepare_backup`] is shared with the owned executor's port implementations.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::domain::{InstallationScope, ProviderBinding};
use crate::hooks::HookSpec;
use crate::manifest::ServiceScope;
use crate::state::{
    ExternalModifiedFile, FileOwner as StateFileOwner, ObjectKind, OwnedFile, ServiceRef,
};
use crate::state_store::StateStore;

// ---------------------------------------------------------------------------
// Plan data model
// ---------------------------------------------------------------------------

/// Which teardown verb produced this plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOperation {
    /// Uninstall + drop ANOLISA-owned config / cache / state fragments.
    Purge,
}

impl LifecycleOperation {
    /// Wire label for the verb, used in audit-log records and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Purge => "purge",
        }
    }
}

/// Coarse blast-radius bucket. Used by CLI surfaces to gate confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Destructive cleanup with incomplete rollback coverage.
    High,
}

/// What a single planned phase will actually do at execute time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleMode {
    /// Will run for real on execute.
    Execute,
    /// Intentionally skipped (e.g. nothing to do, or scope-gated off).
    Skip,
}

/// Whether a file is ANOLISA-owned (safe to remove) or external.
///
/// Mirrors the ownership recorded by [`crate::state::FileOwner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOwner {
    /// Path is owned by ANOLISA and can be removed by lifecycle verbs.
    Anolisa,
    /// Path belongs to the user or another package and must be preserved.
    External,
}

impl From<StateFileOwner> for FileOwner {
    fn from(value: StateFileOwner) -> Self {
        match value {
            StateFileOwner::Anolisa => Self::Anolisa,
            StateFileOwner::External => Self::External,
        }
    }
}

/// What the executor is allowed to do with a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileActionKind {
    /// Delete the file. Only valid when `owner ==
    /// FileOwner::Anolisa`.
    Remove,
    /// External modification that cannot be safely removed — the plan
    /// MUST surface it so operators understand the residue.
    Refuse,
}

/// One file slot in the plan, tying a path to its ownership + intended
/// action.
#[derive(Debug, Clone, Serialize)]
pub struct FileAction {
    /// Absolute path the action applies to.
    pub path: PathBuf,
    /// Ownership classification used to decide whether deletion is safe.
    pub owner: FileOwner,
    /// Planned executor behavior for this path.
    pub action: FileActionKind,
    /// Human-facing explanation for skipped or refused actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Service-unit action the plan would take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceActionKind {
    /// Stop the service before removing its files.
    Stop,
}

/// Service-unit action surfaced in a lifecycle plan.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceAction {
    /// Unit name as recorded in installed state.
    pub name: String,
    /// Planned behavior for the unit.
    pub action: ServiceActionKind,
    /// Manager scope, carried from the installed `ServiceRef` so the
    /// uninstall executor can drive user units via `systemctl --user`.
    #[serde(default)]
    pub scope: ServiceScope,
    /// Explanation when a service action is skipped or deferred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Hook (pre/post-uninstall, etc.) recorded in the plan.
#[derive(Debug, Clone, Serialize)]
pub struct HookAction {
    /// Hook phase name shown in the plan.
    pub name: String,
    /// Whether this hook would run or be skipped.
    pub mode: LifecycleMode,
    /// Explanation when the hook does not execute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Per-component slice of the plan.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentLifecyclePlan {
    /// Component this plan slice describes.
    pub name: String,
    /// Service work associated with the component.
    pub services: Vec<ServiceAction>,
    /// Installed file actions for uninstall.
    pub files: Vec<FileAction>,
    /// Configuration / state fragments owned by ANOLISA (e.g. dropins
    /// the component wrote into `etc_dir`). Only populated for `Purge`.
    pub configs: Vec<FileAction>,
    /// Hook phases that would surround the component lifecycle.
    pub hooks: Vec<HookAction>,
}

/// A single ordered phase of the plan, used by the renderer to show
/// the user what will happen and in what order.
#[derive(Debug, Clone, Serialize)]
pub struct LifecyclePhase {
    /// Stable phase identifier (e.g. `"stop_services"`, `"remove_files"`).
    pub name: String,
    /// Human-readable verb (`"stop"`, `"remove"`, `"run_hook"`, ...).
    pub action: String,
    /// What the phase is acting on (component name, file path, etc.).
    pub target: String,
    /// Whether the executor will run or skip the phase.
    pub mode: LifecycleMode,
    /// Operator guidance for recovery if this phase fails mid-flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback_hint: Option<String>,
}

/// Installed-state object vocabulary targeted by a lifecycle plan.
///
/// Components are the only installable object today; the enum stays on
/// the wire as an extension point for future target kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleTargetKind {
    /// Component target used by `anolisa install` / `uninstall`.
    Component,
}

/// The full lifecycle plan for one installed object invocation.
#[derive(Debug, Clone, Serialize)]
pub struct LifecyclePlan {
    /// Lifecycle verb requested by the user.
    pub operation: LifecycleOperation,
    /// Installed-state object kind this plan targets.
    pub target_kind: LifecycleTargetKind,
    /// Component name the plan targets.
    pub component: String,
    /// Per-component plan slices.
    pub components: Vec<ComponentLifecyclePlan>,
    /// Ordered phases shown by dry-run renderers.
    pub phases: Vec<LifecyclePhase>,
    /// Confirmation bucket for the overall plan.
    pub risk: RiskLevel,
    /// `true` when executing the plan needs elevated privileges.
    pub requires_privilege: bool,
    /// Non-fatal planning warnings for the user.
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Planner constructors
// ---------------------------------------------------------------------------

impl LifecyclePlan {
    /// Build a `Purge` plan: `Uninstall` + remove ANOLISA-owned
    /// `etc_dir` / `cache_dir` / `state_dir` fragments. External
    /// modifications stay [`FileActionKind::Refuse`]. Execution remains
    /// gated by the purge guard.
    pub fn for_component_purge(component: &str, store: &StateStore) -> Self {
        let target = component;
        let target_obj = store.find(ObjectKind::Component, target);
        let target_scope = target_obj.map(|installation| installation.scope);

        let mut components: Vec<ComponentLifecyclePlan> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        if let Some(installation) = target_obj {
            // Only owned artifacts carry files/services the plan can act
            // on; a delegated record contributes nothing but its hooks.
            let (owned_files, external_files, service_refs): (
                &[OwnedFile],
                &[ExternalModifiedFile],
                &[ServiceRef],
            ) = match &installation.binding {
                ProviderBinding::Owned { artifact } => (
                    &artifact.files,
                    &artifact.external_modified_files,
                    &artifact.services,
                ),
                ProviderBinding::Delegated { .. } => (&[], &[], &[]),
            };
            let mut files: Vec<FileAction> = plan_owned_files(owned_files);
            files.extend(plan_external_files(external_files));
            let configs = plan_purge_configs(owned_files);
            components.push(ComponentLifecyclePlan {
                name: target.to_string(),
                services: plan_services(service_refs),
                files,
                configs,
                // Hook execution is deferred to lifecycle teardown; record
                // the intent so audit / preview is honest.
                hooks: purge_hooks(),
            });
        } else {
            warnings.push(format!(
                "component '{target}' is not installed — plan is empty"
            ));
        }

        let phases = build_phases(target, target_scope, &components);

        let requires_privilege = components
            .iter()
            .any(|c| c.files.iter().any(|f| f.action == FileActionKind::Remove));

        Self {
            operation: LifecycleOperation::Purge,
            target_kind: LifecycleTargetKind::Component,
            component: target.to_string(),
            components,
            phases,
            risk: RiskLevel::High,
            requires_privilege,
            warnings,
        }
    }
}

fn plan_owned_files(files: &[OwnedFile]) -> Vec<FileAction> {
    files
        .iter()
        .map(|f| {
            let owner: FileOwner = f.owner.into();
            let (action, reason) = match owner {
                FileOwner::Anolisa => (FileActionKind::Remove, None),
                FileOwner::External => (
                    FileActionKind::Refuse,
                    Some("file marked external in state".to_string()),
                ),
            };
            FileAction {
                path: f.path.clone(),
                owner,
                action,
                reason,
            }
        })
        .collect()
}

fn plan_external_files(files: &[ExternalModifiedFile]) -> Vec<FileAction> {
    files
        .iter()
        .map(|f| FileAction {
            path: f.path.clone(),
            owner: FileOwner::External,
            // Uninstall / Purge refuse external modifications — the user
            // (or a future restore command) owns the cleanup decision.
            action: FileActionKind::Refuse,
            reason: Some("external modification recorded in state".to_string()),
        })
        .collect()
}

fn plan_services(services: &[ServiceRef]) -> Vec<ServiceAction> {
    services
        .iter()
        .map(|s| ServiceAction {
            name: s.name.clone(),
            action: ServiceActionKind::Stop,
            scope: s.scope,
            reason: Some(
                "stops and disables via systemd; user-scope units via `systemctl --user`; skipped on non-linux/container hosts"
                    .to_string(),
            ),
        })
        .collect()
}

/// Configuration fragments to drop on `Purge`. Today we only purge the
/// ANOLISA-owned files that already live under a state/etc/cache root —
/// the manifest schema work for separate config drop-ins is deferred,
/// so we surface the existing files via the `Remove` action and rely on
/// the executor to enforce ownership.
fn plan_purge_configs(files: &[OwnedFile]) -> Vec<FileAction> {
    files
        .iter()
        .filter(|f| f.owner == StateFileOwner::Anolisa)
        .filter(|f| is_config_or_state_path(&f.path))
        .map(|f| FileAction {
            path: f.path.clone(),
            owner: FileOwner::Anolisa,
            action: FileActionKind::Remove,
            reason: Some("ANOLISA-owned config/state fragment".to_string()),
        })
        .collect()
}

fn is_config_or_state_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    // Conservative match — only the ANOLISA-owned roots that
    // `install_runner` writes into qualify.
    s.contains("/etc/anolisa")
        || s.contains("/var/lib/anolisa")
        || s.contains("/var/cache/anolisa")
        || s.contains("/.config/anolisa")
        || s.contains("/.local/state/anolisa")
        || s.contains("/.cache/anolisa")
}

fn purge_hooks() -> Vec<HookAction> {
    ["pre_uninstall", "post_uninstall", "post_purge"]
        .iter()
        .map(|n| HookAction {
            // The plan is built from installed state, which does not carry
            // the component contract, so the plan cannot tell whether a
            // script is declared for this phase — the executor resolves that
            // from the installed manifest at run time. Preview it as Execute
            // with a reason that names the condition.
            name: (*n).to_string(),
            mode: LifecycleMode::Execute,
            reason: Some(
                "runs the contract [[component.hooks]] script for this phase when declared"
                    .to_string(),
            ),
        })
        .collect()
}

fn build_phases(
    component: &str,
    scope: Option<InstallationScope>,
    components: &[ComponentLifecyclePlan],
) -> Vec<LifecyclePhase> {
    let mut phases: Vec<LifecyclePhase> = Vec::new();

    // Hook phases (intent only).
    for c in components {
        for h in &c.hooks {
            phases.push(LifecyclePhase {
                name: format!("hook_{}", h.name),
                action: "run_hook".to_string(),
                target: format!("{}:{}", c.name, h.name),
                mode: h.mode,
                rollback_hint: None,
            });
        }
    }

    // Service phases retain their existing preview labels.
    for c in components {
        for s in &c.services {
            phases.push(LifecyclePhase {
                name: "stop_service".to_string(),
                action: match s.action {
                    ServiceActionKind::Stop => "stop",
                }
                .to_string(),
                target: s.name.clone(),
                mode: LifecycleMode::Execute,
                rollback_hint: None,
            });
        }
    }

    // File phases.
    for c in components {
        for f in &c.files {
            phases.push(LifecyclePhase {
                name: "remove_file".to_string(),
                action: match f.action {
                    FileActionKind::Remove => "remove",
                    FileActionKind::Refuse => "refuse",
                }
                .to_string(),
                target: f.path.display().to_string(),
                mode: match f.action {
                    FileActionKind::Remove => LifecycleMode::Execute,
                    _ => LifecycleMode::Skip,
                },
                rollback_hint: match f.action {
                    FileActionKind::Remove => {
                        scope.map(|scope| scoped_lifecycle_command(scope, "repair", &c.name))
                    }
                    _ => None,
                },
            });
        }
        for f in &c.configs {
            phases.push(LifecyclePhase {
                name: "remove_config".to_string(),
                action: "remove".to_string(),
                target: f.path.display().to_string(),
                mode: LifecycleMode::Execute,
                rollback_hint: None,
            });
        }
    }
    // State-record removal is the one phase every *installed* target ends
    // with. When the target is absent, `components` is empty and the plan is
    // genuinely empty (see the "not installed — plan is empty" warning in
    // the planner); appending `remove_state` here would report a phantom removal
    // that contradicts that warning, so gate it on a present component.
    if !components.is_empty() {
        phases.push(LifecyclePhase {
            name: "remove_state".to_string(),
            action: "remove_object".to_string(),
            target: component.to_string(),
            mode: LifecycleMode::Execute,
            rollback_hint: scope.map(|scope| scoped_lifecycle_command(scope, "install", component)),
        });
    }

    phases
}

fn scoped_lifecycle_command(scope: InstallationScope, operation: &str, component: &str) -> String {
    match scope {
        InstallationScope::System => {
            format!("sudo anolisa --install-mode system {operation} {component}")
        }
        InstallationScope::User { .. } => {
            format!("anolisa --install-mode user {operation} {component}")
        }
    }
}

/// Failure surface for lifecycle planning and backup primitives.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// Filesystem mutation failed while deleting or restoring a path.
    #[error("filesystem io failed for {path}: {source}")]
    Filesystem {
        /// Path involved in the failed filesystem operation.
        path: PathBuf,
        /// Original I/O error from the OS.
        #[source]
        source: std::io::Error,
    },
}

/// Contract-driven lifecycle hooks the caller pre-resolved from the
/// installed component manifest, grouped by phase.
///
/// The executor takes these as input rather than discovering them itself:
/// the CLI layer owns the installed-manifest path convention and reads back
/// each component's `[[component.hooks]]` (placeholder expansion + the real
/// `strict`/`timeout` already applied by
/// [`resolve_manifest_hooks`](crate::hooks::resolve_manifest_hooks)). A
/// caller with no manifest snapshot (older installs, RPM-delegated paths)
/// passes the [`Default`] empty value and the uninstall simply runs no
/// hooks.
#[derive(Debug, Default)]
pub struct ResolvedLifecycleHooks {
    /// Hooks to run before service-stop and file removal. A `strict = true`
    /// hook that fails aborts the uninstall and rolls back; `strict = false`
    /// (e.g. ws-ckpt's recover) only warns.
    pub pre_uninstall: Vec<HookSpec>,
    /// Hooks to run after the lock is released and removal has committed.
    /// Always best-effort — failures only warn.
    pub post_uninstall: Vec<HookSpec>,
}

/// What [`prepare_backup`] wrote at the backup path.
#[derive(Debug)]
pub enum BackupArtifact {
    /// Regular file copied byte-for-byte; sha256 of those bytes.
    File {
        /// Content hash recorded on the rollback action.
        sha256: String,
        /// Permission bits observed on the source, so rollback can put the
        /// file back executable if it was executable. The full word
        /// including setuid/setgid/sticky is reported; what a restore may
        /// safely reproduce is the restore's decision, not the backup's.
        mode: Option<u32>,
    },
    /// Symlink reproduced as an identical link. The referent is never
    /// read through, so there is no byte hash to verify on restore.
    Symlink,
}

impl BackupArtifact {
    /// Hash to record on the rollback action; `None` for symlinks.
    pub fn into_sha256(self) -> Option<String> {
        match self {
            Self::File { sha256, .. } => Some(sha256),
            Self::Symlink => None,
        }
    }

    /// Permission bits observed on the source; `None` for symlinks (a
    /// link's own mode is not meaningful on Linux).
    pub fn mode(&self) -> Option<u32> {
        match self {
            Self::File { mode, .. } => *mode,
            Self::Symlink => None,
        }
    }
}

/// Copy `src` to `backup` while streaming sha256 over the bytes.
///
/// The backup path is the rollback's single source of truth — every
/// `RestoreFile` step replays bytes from here, so this write must be at
/// least as hardened as install:
///
///   * A symlink at `src` (a managed `FileKind::Symlink` entry) is backed
///     up as a *link*: the referent path is reproduced, never read
///     through — bytes behind a link must not be copied as if they
///     belonged to the owned file. Regular files still open with
///     `O_NOFOLLOW` so a link racing in after the metadata check fails
///     the open instead of being followed.
///   * Backup leaf opened with `create_new` (+ `O_NOFOLLOW` on Unix) so
///     a pre-placed symlink or stale file at the backup path fails the
///     open instead of being followed or overwritten (`symlink(2)` gives
///     the same EEXIST guarantee on the link branch).
///   * The source's permission bits are *reported*, so rollback can put
///     the file back with the mode it had instead of whatever the umask
///     gives a fresh file. They are deliberately not reproduced on the
///     copy: the backup tree is operation scratch that a failed plan
///     leaves on disk for forensics, so a copy of a `4755` helper must not
///     itself be a setuid binary sitting under the backup root, and a copy
///     of a `0111` run-only file must stay readable to the process that
///     has to read it back. The copy is owner-only instead — strictly
///     tighter than the `0666 & ~umask` it used to get.
///   * Streaming read+hash so a multi-GB owned file does not have to fit
///     in RAM, and so the on-disk bytes match the recorded sha exactly.
///
/// Returns `Ok(None)` only if `src` is `NotFound`; other errors are
/// surfaced as [`LifecycleError::Filesystem`].
pub fn prepare_backup(src: &Path, backup: &Path) -> Result<Option<BackupArtifact>, LifecycleError> {
    use std::io::{Read, Write};

    match fs::symlink_metadata(src) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let referent = fs::read_link(src).map_err(|source| LifecycleError::Filesystem {
                path: src.to_path_buf(),
                source,
            })?;
            if let Some(parent) = backup.parent()
                && !parent.as_os_str().is_empty()
                && let Err(source) = fs::create_dir_all(parent)
            {
                return Err(LifecycleError::Filesystem {
                    path: parent.to_path_buf(),
                    source,
                });
            }
            std::os::unix::fs::symlink(&referent, backup).map_err(|source| {
                LifecycleError::Filesystem {
                    path: backup.to_path_buf(),
                    source,
                }
            })?;
            return Ok(Some(BackupArtifact::Symlink));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(LifecycleError::Filesystem {
                path: src.to_path_buf(),
                source,
            });
        }
    }

    let mut src_opts = fs::OpenOptions::new();
    src_opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        src_opts.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let mut src_f = match src_opts.open(src) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(LifecycleError::Filesystem {
                path: src.to_path_buf(),
                source,
            });
        }
    };

    if let Some(parent) = backup.parent()
        && !parent.as_os_str().is_empty()
        && let Err(source) = fs::create_dir_all(parent)
    {
        return Err(LifecycleError::Filesystem {
            path: parent.to_path_buf(),
            source,
        });
    }

    // Read the mode off the open descriptor, not the path: the O_NOFOLLOW
    // open above already pinned the inode, so this cannot be raced onto a
    // different file between the check and the copy.
    let src_mode = {
        use std::os::unix::fs::PermissionsExt;
        let meta = src_f
            .metadata()
            .map_err(|source| LifecycleError::Filesystem {
                path: src.to_path_buf(),
                source,
            })?;
        Some(meta.permissions().mode() & 0o7777)
    };

    let mut backup_opts = fs::OpenOptions::new();
    backup_opts.write(true).create_new(true);
    {
        use std::os::unix::fs::OpenOptionsExt;
        backup_opts.custom_flags(nix::libc::O_NOFOLLOW);
        // Owner-only: the copy is scratch, not a mirror. It never needs to
        // be executable, and a private source must not become readable to
        // anyone else just because it passed through the backup tree.
        backup_opts.mode(0o600);
    }
    let mut backup_f = match backup_opts.open(backup) {
        Ok(f) => f,
        Err(source) => {
            return Err(LifecycleError::Filesystem {
                path: backup.to_path_buf(),
                source,
            });
        }
    };
    {
        // `mode` above is only the `open(2)` creation mask, which the umask
        // subtracts from: under `umask 0400` the copy would land `0200` and
        // rollback could not read back the very backup it just wrote.
        // `fchmod` is not umask-filtered, so it pins the mode exactly.
        use std::os::unix::fs::PermissionsExt;
        let chmod_error = backup_f
            .set_permissions(fs::Permissions::from_mode(0o600))
            .err();

        // Fail closed, unlike the restore path. This runs *before* the
        // destructive steps, so nothing has been overwritten yet and a
        // backup the rollback could not read back is worth refusing the
        // operation over — registering it as compensatable would let the
        // executor destroy the original against a promise it cannot keep.
        // Restore is the mirror case: the damage is already done there, so
        // the mode is best-effort and a warning.
        //
        // The gate is the resulting mode rather than whether `fchmod`
        // reported success: a filesystem that cannot chmod but creates
        // readable files is perfectly usable, and only an unreadable copy
        // disqualifies the backup.
        match backup_f
            .metadata()
            .map(|meta| meta.permissions().mode() & 0o400 != 0)
        {
            Ok(true) => {}
            other => {
                let _ = fs::remove_file(backup);
                let source = chmod_error.or_else(|| other.err()).unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "backup copy is not readable by its owner, so a rollback could not \
                             restore from it",
                    )
                });
                return Err(LifecycleError::Filesystem {
                    path: backup.to_path_buf(),
                    source,
                });
            }
        }
    }

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match src_f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(source) => {
                let _ = fs::remove_file(backup);
                return Err(LifecycleError::Filesystem {
                    path: src.to_path_buf(),
                    source,
                });
            }
        };
        if let Err(source) = backup_f.write_all(&buf[..n]) {
            let _ = fs::remove_file(backup);
            return Err(LifecycleError::Filesystem {
                path: backup.to_path_buf(),
                source,
            });
        }
        hasher.update(&buf[..n]);
    }
    if let Err(source) = backup_f.sync_all() {
        let _ = fs::remove_file(backup);
        return Err(LifecycleError::Filesystem {
            path: backup.to_path_buf(),
            source,
        });
    }

    let out = hasher.finalize();
    let mut sha = String::with_capacity(64);
    for b in out {
        sha.push_str(&format!("{b:02x}"));
    }
    Ok(Some(BackupArtifact::File {
        sha256: sha,
        mode: src_mode,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::domain::InstallationScope;
    use crate::state::{
        ExternalModifiedFile, FileOwner as StateFileOwner, InstalledObject, InstalledState,
        ObjectKind, ObjectStatus, OwnedFile, OwnedFileKind, ServiceRef,
    };
    use anolisa_platform::fs_layout::FsLayout;
    use std::fs as std_fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn fixture_layout(prefix: &Path) -> FsLayout {
        FsLayout::system(Some(prefix.to_path_buf()))
    }

    /// Legacy fixture kept as migration coverage: seed a v4 object and
    /// migrate it into a v5 store the planner consumes.
    fn seed_state_with_two_files(
        layout: &FsLayout,
        component: &str,
        owned_path: &Path,
        external_path: &Path,
    ) -> StateStore {
        std_fs::create_dir_all(&layout.state_dir).expect("mkdir state");
        let mut state = InstalledState::default();
        state.objects.push(InstalledObject {
            kind: ObjectKind::Component,
            name: component.to_string(),
            version: "0.2.0".to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: None,
            distribution_source: Some("file:///fake".to_string()),
            raw_package: None,
            install_backend: Some("raw".to_string()),
            ownership: None,
            rpm_metadata: None,
            installed_at: "2026-06-01T10:00:00Z".to_string(),
            last_operation_id: Some("op-prior".to_string()),
            managed: true,
            adopted: false,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            component_refs: Vec::new(),
            files: vec![OwnedFile {
                path: owned_path.to_path_buf(),
                owner: StateFileOwner::Anolisa,
                sha256: Some("0".repeat(64)),
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            }],
            external_modified_files: vec![ExternalModifiedFile {
                path: external_path.to_path_buf(),
                owner: StateFileOwner::External,
                backup_id: "backup-prior".to_string(),
                sha256_before: Some("a".repeat(64)),
                sha256_after: Some("b".repeat(64)),
            }],
            services: vec![ServiceRef {
                name: format!("{component}.service"),
                manager: "systemd".to_string(),
                restartable: true,
                enabled: false,
                scope: ServiceScope::System,
            }],
            health: Vec::new(),
            provisioned_packages: Vec::new(),
        });
        crate::state::write_legacy_fixture(&state, &layout.state_dir.join("installed.toml"))
            .expect("seed state save");
        let migration =
            crate::state_migration::migrate_state(&state.objects, InstallationScope::System);
        assert!(
            migration.quarantined.is_empty(),
            "fixtures must migrate cleanly"
        );
        let mut store = StateStore::empty();
        store.installations = migration.active;
        store
    }

    #[test]
    fn plan_services_carries_scope_from_service_ref() {
        let refs = vec![
            ServiceRef {
                name: "agentsight.service".to_string(),
                manager: "systemd".to_string(),
                restartable: true,
                enabled: true,
                scope: ServiceScope::System,
            },
            ServiceRef {
                name: "anolisa-memory@alice.service".to_string(),
                manager: "systemd-user".to_string(),
                restartable: false,
                enabled: false,
                scope: ServiceScope::User,
            },
        ];
        let actions = plan_services(&refs);
        assert!(matches!(actions[0].scope, ServiceScope::System));
        assert!(matches!(actions[1].scope, ServiceScope::User));
    }

    #[test]
    fn purge_plan_remove_anolisa_refuse_external() {
        let root = tempdir().expect("tempdir");
        let layout = fixture_layout(root.path());
        let owned = layout.bin_dir.join("agentsight");
        let external = layout.etc_dir.join("third-party.toml");
        let state = seed_state_with_two_files(&layout, "agentsight", &owned, &external);

        let plan = LifecyclePlan::for_component_purge("agentsight", &state);
        assert_eq!(plan.operation, LifecycleOperation::Purge);
        assert_eq!(plan.risk, RiskLevel::High);
        assert_eq!(
            plan.components[0]
                .hooks
                .iter()
                .map(|hook| hook.name.as_str())
                .collect::<Vec<_>>(),
            ["pre_uninstall", "post_uninstall", "post_purge"],
        );
        // Service phases recorded as Stop (executed best-effort by the
        // ServiceManager; degrades to a quiet skip on unsupported hosts).
        for s in &plan.components[0].services {
            assert_eq!(s.action, ServiceActionKind::Stop);
        }
        let comp = &plan.components[0];
        let owned_action = comp
            .files
            .iter()
            .find(|f| f.path == owned)
            .expect("owned file in plan");
        assert_eq!(owned_action.action, FileActionKind::Remove);
        assert_eq!(owned_action.owner, FileOwner::Anolisa);
        let ext_action = comp
            .files
            .iter()
            .find(|f| f.path == external)
            .expect("external file in plan");
        assert_eq!(ext_action.action, FileActionKind::Refuse);
        assert_eq!(ext_action.owner, FileOwner::External);

        let wire = serde_json::to_value(&plan).expect("serialize purge plan");
        assert_eq!(wire["operation"], "purge");
        assert_eq!(wire["target_kind"], "component");
        assert_eq!(wire["risk"], "high");
        assert_eq!(wire["components"][0]["services"][0]["action"], "stop");
        assert_eq!(wire["components"][0]["files"][0]["owner"], "anolisa");
        assert_eq!(wire["components"][0]["files"][0]["action"], "remove");
        assert_eq!(wire["components"][0]["files"][1]["owner"], "external");
        assert_eq!(wire["components"][0]["files"][1]["action"], "refuse");
        let phases: Vec<_> = wire["phases"]
            .as_array()
            .expect("phases")
            .iter()
            .map(|phase| (phase["action"].as_str(), phase["mode"].as_str()))
            .collect();
        assert_eq!(
            phases,
            vec![
                (Some("run_hook"), Some("execute")),
                (Some("run_hook"), Some("execute")),
                (Some("run_hook"), Some("execute")),
                (Some("stop"), Some("execute")),
                (Some("remove"), Some("execute")),
                (Some("refuse"), Some("skip")),
                (Some("remove_object"), Some("execute")),
            ],
        );

        let remove_file = plan
            .phases
            .iter()
            .find(|phase| phase.name == "remove_file")
            .expect("remove_file phase");
        assert_eq!(
            remove_file.rollback_hint.as_deref(),
            Some("sudo anolisa --install-mode system repair agentsight"),
        );
        let remove_state = plan
            .phases
            .iter()
            .find(|phase| phase.name == "remove_state")
            .expect("remove_state phase");
        assert_eq!(
            remove_state.rollback_hint.as_deref(),
            Some("sudo anolisa --install-mode system install agentsight"),
        );
    }

    #[test]
    fn recovery_hint_preserves_user_scope() {
        assert_eq!(
            scoped_lifecycle_command(InstallationScope::User { uid: 1000 }, "repair", "cosh",),
            "anolisa --install-mode user repair cosh",
        );
    }

    #[test]
    fn purge_dry_run_does_not_mutate_anything() {
        // "dry-run" is a CLI-level concept: the executor is never
        // invoked. Here we exercise the planner-only path and confirm
        // no IO occurs.
        let root = tempdir().expect("tempdir");
        let layout = fixture_layout(root.path());
        std_fs::create_dir_all(&layout.bin_dir).expect("mkdir bin");
        let owned = layout.bin_dir.join("agentsight");
        std_fs::write(&owned, b"keep me").expect("write owned");
        let external = layout.etc_dir.join("third.toml");
        let state = seed_state_with_two_files(&layout, "agentsight", &owned, &external);

        let plan = LifecyclePlan::for_component_purge("agentsight", &state);
        assert!(!plan.components.is_empty());
        assert!(
            owned.exists(),
            "dry-run planner must not touch the filesystem",
        );
        assert!(!layout.central_log.exists());
    }

    /// #1471: an absent target must yield a *genuinely* empty plan — the
    /// "not installed" warning present, and neither a component slice nor
    /// any phase emitted. Guards the self-contradiction where the warning
    /// said "plan is empty" while a phantom `remove_state` phase remained.
    #[test]
    fn purge_absent_component_yields_empty_components_and_phases() {
        let empty = StateStore::empty();
        let plan = LifecyclePlan::for_component_purge("agentsight", &empty);

        assert!(
            plan.components.is_empty(),
            "absent component must produce no component slice",
        );
        assert!(
            plan.phases.is_empty(),
            "absent component must produce no phases (not even remove_state): {:?}",
            plan.phases,
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("is not installed") && w.contains("plan is empty")),
            "the not-installed warning must be retained: {:?}",
            plan.warnings,
        );
    }

    /// `prepare_backup` must refuse to overwrite a pre-existing file at
    /// the backup leaf — `O_CREAT|O_EXCL` is what makes the backup the
    /// rollback's single source of truth, so a stale or hostile file
    /// already sitting at `<backup_root>/<idx>.bak` must fail the open
    /// rather than be silently replaced.
    #[test]
    fn prepare_backup_refuses_existing_backup_leaf() {
        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        std_fs::write(&src, b"payload").expect("write src");
        let backup = tmp.path().join("backup.bak");
        std_fs::write(&backup, b"stale").expect("write stale backup");

        let err = prepare_backup(&src, &backup).expect_err("must refuse existing backup leaf");
        assert!(
            matches!(err, LifecycleError::Filesystem { ref path, .. } if path == &backup),
            "expected Filesystem error pointing at backup leaf, got {err:?}",
        );
        // Existing bytes preserved — we did not silently overwrite.
        let after = std_fs::read(&backup).expect("read backup");
        assert_eq!(after, b"stale");
    }

    /// A symlink planted at the backup leaf must fail the open instead
    /// of being followed. Without `O_NOFOLLOW`, an attacker who can
    /// write inside the backup root could redirect the backup writes
    /// onto an arbitrary file.
    #[test]
    #[cfg(unix)]
    fn prepare_backup_refuses_symlink_at_backup_leaf() {
        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        std_fs::write(&src, b"payload").expect("write src");
        let victim = tmp.path().join("victim");
        std_fs::write(&victim, b"untouched").expect("write victim");
        let backup = tmp.path().join("backup.bak");
        std::os::unix::fs::symlink(&victim, &backup).expect("plant symlink");

        let err = prepare_backup(&src, &backup).expect_err("must refuse symlink at backup leaf");
        assert!(
            matches!(err, LifecycleError::Filesystem { ref path, .. } if path == &backup),
            "expected Filesystem error pointing at backup leaf, got {err:?}",
        );
        // Victim must NOT have been written to via the symlink.
        assert_eq!(std_fs::read(&victim).expect("read victim"), b"untouched");
    }

    /// A symlink at the source path is backed up as a *link* — the
    /// referent path is reproduced and its bytes are never read through,
    /// so a link pointing at content outside the owned roots cannot leak
    /// those bytes into the backup as if they belonged to the owned file.
    #[test]
    #[cfg(unix)]
    fn prepare_backup_copies_symlink_as_link() {
        let tmp = tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        std_fs::write(&target, b"target bytes").expect("write target");
        let src = tmp.path().join("src");
        std::os::unix::fs::symlink(&target, &src).expect("plant src symlink");
        let backup = tmp.path().join("backup.bak");

        let artifact = prepare_backup(&src, &backup)
            .expect("backup ok")
            .expect("src exists");
        assert!(
            artifact.into_sha256().is_none(),
            "symlink backup must not record a byte hash"
        );
        let meta = std_fs::symlink_metadata(&backup).expect("backup exists");
        assert!(meta.file_type().is_symlink(), "backup must be a link");
        assert_eq!(std_fs::read_link(&backup).expect("read_link"), target);
    }

    /// The source's permission bits are reported so rollback can put the
    /// file back executable; a backup that only carried bytes is the whole
    /// of the restore-drops-the-executable-bit bug.
    #[test]
    #[cfg(unix)]
    fn prepare_backup_reports_the_source_mode() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("bin/tool");
        std_fs::create_dir_all(src.parent().expect("parent")).expect("mkdir");
        std_fs::write(&src, b"tool bytes").expect("write src");
        std_fs::set_permissions(&src, std_fs::Permissions::from_mode(0o755)).expect("chmod src");
        let backup = tmp.path().join("backup/0.bak");

        let artifact = prepare_backup(&src, &backup)
            .expect("backup ok")
            .expect("src exists");

        assert_eq!(artifact.mode(), Some(0o755));
    }

    /// The setuid bit is reported verbatim — deciding what may safely be
    /// replayed belongs to the restore, which knows it cannot reproduce the
    /// original owner. The copy itself must never become that setuid binary:
    /// it sits in the backup tree, which a failed plan leaves on disk.
    #[test]
    #[cfg(unix)]
    fn prepare_backup_reports_setuid_without_reproducing_it() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("libexec/helper");
        std_fs::create_dir_all(src.parent().expect("parent")).expect("mkdir");
        std_fs::write(&src, b"helper bytes").expect("write src");
        std_fs::set_permissions(&src, std_fs::Permissions::from_mode(0o4755)).expect("chmod src");
        let backup = tmp.path().join("backup/0.bak");

        let artifact = prepare_backup(&src, &backup)
            .expect("backup ok")
            .expect("src exists");

        assert_eq!(artifact.mode(), Some(0o4755));
        let copy_mode = std_fs::metadata(&backup)
            .expect("stat backup")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            copy_mode & 0o7000,
            0,
            "a backup copy must never be setuid/setgid: {copy_mode:04o}"
        );
        assert_eq!(
            copy_mode & 0o111,
            0,
            "a backup copy is inert data, not an executable: {copy_mode:04o}"
        );
    }

    /// Whatever the source mode and whatever the umask, the copy is
    /// owner-only: backing up a `0600` secret must not widen it, and
    /// backing up a `0644` config must not leave a second readable copy
    /// under the backup root.
    #[test]
    #[cfg(unix)]
    fn prepare_backup_copies_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        for (name, mode) in [("secret.toml", 0o600), ("config.toml", 0o644)] {
            let src = tmp.path().join("etc").join(name);
            std_fs::create_dir_all(src.parent().expect("parent")).expect("mkdir");
            std_fs::write(&src, b"token = \"s3cr3t\"").expect("write src");
            std_fs::set_permissions(&src, std_fs::Permissions::from_mode(mode)).expect("chmod src");
            let backup = tmp.path().join("backup").join(name);

            prepare_backup(&src, &backup)
                .expect("backup ok")
                .expect("src exists");

            assert_eq!(
                std_fs::metadata(&backup)
                    .expect("stat backup")
                    .permissions()
                    .mode()
                    & 0o7777,
                0o600,
                "the copy of a {mode:o} source must be owner-only"
            );
        }
    }

    /// The backup copy must stay owner-*readable* whatever umask the caller
    /// runs under. `OpenOptions::mode` is only the `open(2)` creation mask,
    /// which the umask subtracts from, so `umask 0400` turns a `0600` request
    /// into a write-only `0200` copy — and then `restore_backup_file` cannot
    /// read back the backup it just wrote, turning the compensation this
    /// module exists to make whole into a partial one.
    ///
    /// umask is process-global, so the hostile value is set in a child
    /// process running only this test; mutating it in-process would leak
    /// into whatever tests happen to run beside it.
    #[test]
    #[cfg(unix)]
    fn prepare_backup_copies_stay_readable_under_a_hostile_umask() {
        const CHILD: &str = "ANOLISA_PREPARE_BACKUP_UMASK_CHILD";
        const TEST: &str =
            "lifecycle::tests::prepare_backup_copies_stay_readable_under_a_hostile_umask";

        if std::env::var_os(CHILD).is_none() {
            let out = std::process::Command::new(
                std::env::current_exe().expect("locate the test binary"),
            )
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .expect("re-exec the test binary");
            let report = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            // A filter that matches nothing also exits 0, which would make
            // this test vacuous; require the child to say it ran one test.
            assert!(
                report.contains("1 passed"),
                "the child must actually run {TEST}: {report}"
            );
            assert!(out.status.success(), "child failed: {report}");
            return;
        }

        use std::os::unix::fs::PermissionsExt;

        // Lay the fixture down first: the source is what an earlier install
        // left behind under a normal umask, so only the backup itself should
        // meet the hostile one.
        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("etc/secret.toml");
        std_fs::create_dir_all(src.parent().expect("parent")).expect("mkdir");
        std_fs::write(&src, b"token = \"s3cr3t\"").expect("write src");
        std_fs::set_permissions(&src, std_fs::Permissions::from_mode(0o600)).expect("chmod src");
        let backup = tmp.path().join("backup/0.bak");

        // Only this test runs in the child, so the global umask is ours.
        unsafe { nix::libc::umask(0o400) };

        prepare_backup(&src, &backup)
            .expect("backup ok")
            .expect("src exists");

        let mode = std_fs::metadata(&backup)
            .expect("stat backup")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            mode, 0o600,
            "umask must not narrow the copy out of the owner's reach: {mode:04o}"
        );
        assert_eq!(
            std_fs::read(&backup).expect("the rollback has to be able to read its own backup"),
            b"token = \"s3cr3t\""
        );
    }

    /// A pre-placed file at the backup leaf must fail the symlink backup
    /// the same way `create_new` protects the regular-file branch.
    #[test]
    #[cfg(unix)]
    fn prepare_backup_symlink_refuses_existing_backup_leaf() {
        let tmp = tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        std_fs::write(&target, b"target bytes").expect("write target");
        let src = tmp.path().join("src");
        std::os::unix::fs::symlink(&target, &src).expect("plant src symlink");
        let backup = tmp.path().join("backup.bak");
        std_fs::write(&backup, b"stale").expect("write stale backup");

        let err = prepare_backup(&src, &backup).expect_err("must refuse existing backup leaf");
        assert!(
            matches!(err, LifecycleError::Filesystem { ref path, .. } if path == &backup),
            "expected Filesystem error pointing at backup leaf, got {err:?}",
        );
        assert_eq!(std_fs::read(&backup).expect("read backup"), b"stale");
    }

    /// Streaming-hash sanity: a multi-chunk file's recorded sha matches
    /// the canonical sha256 of its bytes, and the backup contents are
    /// byte-identical to the source. Guards against off-by-one read
    /// loops.
    #[test]
    fn prepare_backup_streams_large_file_with_correct_sha() {
        let tmp = tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        // Bigger than one read buffer (64 KiB) to exercise the loop.
        let payload: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        std_fs::write(&src, &payload).expect("write src");
        let backup = tmp.path().join("nested").join("backup.bak");

        let sha = prepare_backup(&src, &backup)
            .expect("backup ok")
            .expect("expected sha for existing src")
            .into_sha256()
            .expect("regular file backup records a sha");

        let mut hasher = Sha256::new();
        hasher.update(&payload);
        let expected: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(sha, expected);
        assert_eq!(std_fs::read(&backup).expect("read backup"), payload);
    }
}
