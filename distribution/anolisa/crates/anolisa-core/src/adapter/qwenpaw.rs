//! QwenPaw framework driver.
//!
//! QwenPaw keeps plugins under `<working dir>/plugins/<plugin_id>/` and owns
//! their lifecycle through its own CLI: `qwenpaw plugin install <dir>`
//! copies the bundle, validates the entry module, installs the bundle's
//! `requirements.txt` into QwenPaw's Python environment and hot-loads the
//! plugin when a QwenPaw server is running. ANOLISA therefore never copies
//! the bundle itself; it hands the resource root to the CLI and verifies the
//! post-condition (the installed `plugin.json`), because the CLI reports
//! failures on stdout and still exits 0.
//!
//! The working directory mirrors QwenPaw's own resolution:
//! `QWENPAW_WORKING_DIR`, else `COPAW_WORKING_DIR`, else `~/.copaw` when it
//! exists, else `~/.qwenpaw`. `QWENPAW_BIN` overrides the executable (used
//! by tests to point at a fake CLI). The installer's `bin/qwenpaw` wrapper
//! lives under `${QWENPAW_HOME:-~/.qwenpaw}/bin`, which is rarely on a
//! non-login PATH, so detection also probes that directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    DRIVER_SCHEMA_VERSION, DriverPayload, QwenPawClaim, validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterOps, AdapterStatusReport,
    AdapterSummary, ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx,
    DriverPlan, FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable, find_binary_in_path,
    is_executable,
};
use super::util::{bool_status, cli_failure_reason, display_command, now_iso8601};

/// `plugin install` copies the bundle and pip-installs its requirements
/// (QwenPaw allows 300s offline, 120s for a hot install over HTTP).
const INSTALL_TIMEOUT: Duration = Duration::from_secs(420);
/// Timeout for `plugin uninstall`.
const CLI_TIMEOUT: Duration = Duration::from_secs(120);

/// Resource ids used in QwenPaw receipts.
const RES_HOME: &str = "qwenpaw_home";
const RES_PLUGIN: &str = "qwenpaw_plugin";

/// QwenPaw-native plugin manifest inside the bundle and the installed
/// plugin directory.
const MANIFEST: &str = "plugin.json";

/// QwenPaw driver. Stateless; all per-operation context arrives via
/// [`DriverCtx`].
pub struct QwenPawDriver;

impl QwenPawDriver {
    /// Construct the driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for QwenPawDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for QwenPawDriver {
    fn name(&self) -> &'static str {
        "qwenpaw"
    }

    fn detect(&self, env: &HostEnv) -> DetectResult {
        match locate_qwenpaw(env.user_home.as_deref()) {
            Some(path) => DetectResult {
                detected: true,
                reason: format!("qwenpaw CLI found at {}", path.display()),
            },
            None => {
                let home_note = qwenpaw_home(env.user_home.as_deref())
                    .filter(|h| h.exists())
                    .map(|h| {
                        format!(
                            " (working dir {} exists but CLI is not on PATH)",
                            h.display()
                        )
                    })
                    .unwrap_or_default();
                DetectResult {
                    detected: false,
                    reason: format!("qwenpaw CLI not found on PATH{home_note}"),
                }
            }
        }
    }

    fn probe_bundle(&self, resource_root: &Path, declared_entry: Option<&str>) -> bool {
        resource_root
            .join(declared_entry.unwrap_or(MANIFEST))
            .is_file()
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        // The CLI writes only under the working directory. Also allow both
        // default locations so a receipt stays verifiable after `~/.copaw`
        // appears or disappears.
        let mut roots: Vec<PathBuf> = qwenpaw_home(ctx.user_home.as_deref()).into_iter().collect();
        if let Some(home) = ctx.user_home.as_deref() {
            for default in [".qwenpaw", ".copaw"] {
                let path = home.join(default);
                if !roots.contains(&path) {
                    roots.push(path);
                }
            }
        }
        roots
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        let root = &ctx.resource_root;
        if !root.is_dir() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: "resource root does not exist or is not a directory".to_string(),
            });
        }
        // QwenPaw installs the plugin under the id written in plugin.json,
        // so the receipt must carry that id — never a guessed one.
        let manifest = root.join(ctx.declared_bundle_entry.as_deref().unwrap_or(MANIFEST));
        let bytes = std::fs::read(&manifest).map_err(|source| AdapterError::Io {
            path: manifest.clone(),
            source,
        })?;
        let plugin_id = parse_manifest_id(&bytes).ok_or_else(|| AdapterError::BundleInvalid {
            root: root.clone(),
            reason: format!(
                "{} is not a QwenPaw plugin manifest with a non-empty \"id\"",
                manifest.display()
            ),
        })?;
        if let Some(declared) = ctx
            .declared_plugin_id
            .as_deref()
            .filter(|id| !id.is_empty())
            && declared != plugin_id
        {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!(
                    "manifest declares plugin_id '{declared}' but {} has id '{plugin_id}'",
                    manifest.display()
                ),
            });
        }
        Ok(AdapterBundle {
            resource_root: root.clone(),
            plugin_id: Some(plugin_id),
        })
    }

    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError> {
        let home = require_home(ctx)?;
        let plugin_id = require_plugin_id(bundle)?;
        validate_plugin_id(&plugin_id)?;
        let install_cmd = build_install_cmd(&home, ctx.user_home.as_deref(), &bundle.resource_root);
        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions: vec![format!(
                "install qwenpaw plugin '{plugin_id}' from {} into {}/plugins/{plugin_id} via the qwenpaw CLI",
                bundle.resource_root.display(),
                home.display(),
            )],
            register_command: Some(display_command(&install_cmd)),
        })
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let home = require_home(ctx)?;
        let plugin_id = require_plugin_id(bundle)?;
        validate_plugin_id(&plugin_id)?;
        let resources = vec![
            ClaimResource {
                id: RES_HOME.to_string(),
                purpose: "qwenpaw_home".to_string(),
                kind: ClaimResourceKind::ExternalPath { path: home.clone() },
            },
            ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: "qwenpaw_plugin_dir".to_string(),
                kind: ClaimResourceKind::ExternalPath {
                    path: home.join("plugins").join(&plugin_id),
                },
            },
        ];
        Ok((
            AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: ctx.component.clone(),
                framework: self.name().to_string(),
                plugin_id: Some(plugin_id),
                adapter_type: ctx.adapter_type.clone(),
                enabled_at: now_iso8601(),
                resource_root: bundle.resource_root.clone(),
                bundle_digest: None,
                source_revision: None,
                materialized_files: Vec::new(),
                driver_schema: DRIVER_SCHEMA_VERSION,
                status: ClaimStatus::Enabled,
                notices: Vec::new(),
                resources,
                driver_payload: DriverPayload::QwenPaw(QwenPawClaim {
                    home_resource: RES_HOME.to_string(),
                    plugin_resource: RES_PLUGIN.to_string(),
                }),
            },
            PreparedEnable::None,
        ))
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        _prepared: &PreparedEnable,
        ctx: &DriverCtx,
        _progress: &mut dyn super::driver::EnableProgress,
    ) -> Result<(), AdapterError> {
        let home = require_home(ctx)?;
        let plugin_id = claim
            .plugin_id
            .clone()
            .ok_or_else(|| AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: "qwenpaw receipt has no plugin id".to_string(),
            })?;
        validate_plugin_id(&plugin_id)?;

        let install_cmd = build_install_cmd(&home, ctx.user_home.as_deref(), &claim.resource_root);
        let program = install_cmd.program.clone();
        let output = ctx.ops.run_framework_cli(install_cmd)?;
        if !output.success() {
            return Err(AdapterError::FrameworkCli {
                program,
                reason: cli_failure_reason("plugin install", &output),
            });
        }
        // The CLI echoes failures and exits 0, and a failed reinstall leaves
        // the previous bundle in place: only installed files that match the
        // source bundle prove the install happened.
        let installed_dir = home.join("plugins").join(&plugin_id);
        if let Some(reason) =
            installed_bundle_mismatch(ctx.ops, &claim.resource_root, &installed_dir, &plugin_id)?
        {
            let hint = output
                .stderr
                .lines()
                .chain(output.stdout.lines())
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .unwrap_or("no output")
                .to_string();
            return Err(AdapterError::FrameworkCli {
                program,
                reason: format!("'plugin install' exited 0 but {reason}: {hint}"),
            });
        }
        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        let detect = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        let mut conditions = vec![AdapterCondition {
            kind: AdapterConditionKind::FrameworkDetected,
            status: bool_status(detect.detected),
            reason: Some(detect.reason.clone()),
            resource: None,
        }];

        let plugin_ref = Some(ClaimResourceRef {
            id: RES_PLUGIN.to_string(),
        });
        // The installed manifest is the registry: QwenPaw scans
        // <working dir>/plugins at startup and `plugin list` is the same
        // directory scan, so status does not need the CLI.
        let plugin_registered = match (claim.plugin_id.as_deref(), plugin_dir(claim)) {
            (Some(plugin_id), Some(dir)) => match installed_id(ctx.ops, &dir.join(MANIFEST)) {
                Ok(installed) => {
                    let registered = installed.as_deref() == Some(plugin_id);
                    conditions.push(AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: bool_status(registered),
                        reason: (!registered).then(|| match installed {
                            Some(other) => format!("{} belongs to plugin '{other}'", dir.display()),
                            None => format!("{} has no plugin manifest", dir.display()),
                        }),
                        resource: plugin_ref,
                    });
                    conditions.push(AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::True,
                        reason: None,
                        resource: None,
                    });
                    bool_status(registered)
                }
                Err(err) => {
                    conditions.push(AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: ConditionStatus::Unknown,
                        reason: Some(format!("cannot read {}: {err}", dir.display())),
                        resource: plugin_ref,
                    });
                    conditions.push(AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::False,
                        reason: Some("installed plugin manifest unreadable".to_string()),
                        resource: None,
                    });
                    ConditionStatus::Unknown
                }
            },
            _ => {
                conditions.push(AdapterCondition {
                    kind: AdapterConditionKind::PluginRegistered,
                    status: ConditionStatus::Unknown,
                    reason: Some("receipt has no plugin id or plugin resource".to_string()),
                    resource: None,
                });
                conditions.push(AdapterCondition {
                    kind: AdapterConditionKind::VerificationSupported,
                    status: ConditionStatus::False,
                    reason: Some("receipt is incomplete".to_string()),
                    resource: None,
                });
                ConditionStatus::Unknown
            }
        };

        let summary = if claim.status == ClaimStatus::CleanupFailed {
            AdapterSummary::CleanupFailed
        } else if !detect.detected {
            AdapterSummary::Degraded
        } else {
            match plugin_registered {
                ConditionStatus::True => AdapterSummary::Healthy,
                ConditionStatus::False => AdapterSummary::Degraded,
                ConditionStatus::Unknown => AdapterSummary::Unknown,
            }
        };
        Ok(AdapterStatusReport {
            summary,
            conditions,
        })
    }

    fn disable(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        let Some(plugin_id) = claim.plugin_id.clone() else {
            return Ok(DisableReport {
                cleanup_complete: true,
                messages: vec!["receipt records no plugin to uninstall".to_string()],
            });
        };
        // prepare_enable records both paths, so a receipt without them is
        // damaged: keep it rather than orphan an installed plugin.
        let (Some(home), Some(dir)) = (claim_home(claim), plugin_dir(claim)) else {
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![
                    "receipt does not record the qwenpaw working and plugin directories; \
                     receipt kept for inspection"
                        .to_string(),
                ],
            });
        };
        validate_plugin_id(&plugin_id)?;
        if locate_qwenpaw(ctx.user_home.as_deref()).is_none() {
            // Deleting the directory under a running QwenPaw would leave the
            // plugin loaded; keep the receipt so cleanup can be retried.
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![
                    "qwenpaw CLI not found on PATH; receipt kept so cleanup can be retried"
                        .to_string(),
                ],
            });
        }

        let mut messages = Vec::new();
        let uninstall_cmd = build_uninstall_cmd(&home, ctx.user_home.as_deref(), &plugin_id);
        match ctx.ops.run_framework_cli(uninstall_cmd) {
            Ok(output) if output.success() => {}
            Ok(output) => messages.push(cli_failure_reason("plugin uninstall", &output)),
            Err(err) => messages.push(format!("'plugin uninstall' could not run: {err}")),
        }
        // The CLI reports failures on stdout and still exits 0, so only the
        // manifest being gone proves QwenPaw unloaded the plugin; otherwise a
        // running QwenPaw may still have it loaded, so keep the directory and
        // the receipt for a retry.
        if installed_id(ctx.ops, &dir.join(MANIFEST))?.is_some() {
            messages.push(format!(
                "qwenpaw plugin directory {} kept; receipt kept so cleanup can be retried",
                dir.display()
            ));
            return Ok(DisableReport {
                cleanup_complete: false,
                messages,
            });
        }
        messages.push(format!("uninstalled qwenpaw plugin '{plugin_id}'"));
        let mut cleanup_complete = true;
        match ctx.ops.remove_tree(&dir) {
            Ok(true) => messages.push(format!(
                "removed qwenpaw plugin directory {}",
                dir.display()
            )),
            Ok(false) => messages.push(format!(
                "qwenpaw plugin directory {} already absent",
                dir.display()
            )),
            Err(err) => {
                cleanup_complete = false;
                messages.push(format!(
                    "failed to remove qwenpaw plugin directory {}: {err}",
                    dir.display()
                ));
            }
        }
        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (no spawning) — unit-testable
// ---------------------------------------------------------------------------

/// `QWENPAW_BIN` override, else `qwenpaw`.
fn qwenpaw_bin() -> String {
    std::env::var("QWENPAW_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "qwenpaw".to_string())
}

/// Non-empty value of an environment variable with trailing slashes trimmed.
fn env_path(key: &str) -> Option<PathBuf> {
    let value = std::env::var_os(key)?;
    let value = value.to_string_lossy();
    let trimmed = value.trim_end_matches('/');
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// QwenPaw working directory, resolved exactly like QwenPaw's `constant.py`:
/// `QWENPAW_WORKING_DIR`, else `COPAW_WORKING_DIR`, else `~/.copaw` when it
/// is a directory, else `~/.qwenpaw`.
fn qwenpaw_home(user_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = env_path("QWENPAW_WORKING_DIR").or_else(|| env_path("COPAW_WORKING_DIR")) {
        return Some(dir);
    }
    let home = user_home?;
    let copaw = home.join(".copaw");
    Some(if copaw.is_dir() {
        copaw
    } else {
        home.join(".qwenpaw")
    })
}

/// Directories the installer's `bin/qwenpaw` wrapper commonly lives in.
fn path_prepend(user_home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = env_path("QWENPAW_HOME") {
        dirs.push(dir.join("bin"));
    } else if let Some(home) = user_home {
        dirs.push(home.join(".qwenpaw").join("bin"));
    }
    if let Some(home) = user_home {
        dirs.push(home.join(".local").join("bin"));
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// Locate the CLI on `PATH` or in one of the [`path_prepend`] directories.
fn locate_qwenpaw(user_home: Option<&Path>) -> Option<PathBuf> {
    let bin = qwenpaw_bin();
    find_binary_in_path(&bin).or_else(|| {
        path_prepend(user_home)
            .into_iter()
            .map(|dir| dir.join(&bin))
            .find(|candidate| candidate.is_file() && is_executable(candidate))
    })
}

/// Build a `qwenpaw` command pinned to `home`. `COPAW_WORKING_DIR` is
/// dropped so the child cannot resolve a different working directory than
/// the one the receipt claims.
fn base_cmd(
    home: &Path,
    user_home: Option<&Path>,
    args: Vec<String>,
    timeout: Duration,
) -> FrameworkCommand {
    FrameworkCommand {
        program: qwenpaw_bin(),
        args,
        stdin: None,
        env_set: vec![(
            "QWENPAW_WORKING_DIR".to_string(),
            home.to_string_lossy().into_owned(),
        )],
        env_remove: vec!["COPAW_WORKING_DIR".to_string()],
        path_prepend: path_prepend(user_home),
        timeout,
    }
}

/// Build `qwenpaw plugin install <resource_root> --force`.
fn build_install_cmd(
    home: &Path,
    user_home: Option<&Path>,
    resource_root: &Path,
) -> FrameworkCommand {
    base_cmd(
        home,
        user_home,
        vec![
            "plugin".to_string(),
            "install".to_string(),
            resource_root.to_string_lossy().into_owned(),
            "--force".to_string(),
        ],
        INSTALL_TIMEOUT,
    )
}

/// Build `qwenpaw plugin uninstall <plugin_id>`, answering its confirmation
/// prompt on stdin.
fn build_uninstall_cmd(home: &Path, user_home: Option<&Path>, plugin_id: &str) -> FrameworkCommand {
    let mut cmd = base_cmd(
        home,
        user_home,
        vec![
            "plugin".to_string(),
            "uninstall".to_string(),
            plugin_id.to_string(),
        ],
        CLI_TIMEOUT,
    );
    cmd.stdin = Some(b"y\n".to_vec());
    cmd
}

/// Non-empty `id` of a QwenPaw `plugin.json`.
fn parse_manifest_id(bytes: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct PluginManifest {
        id: Option<String>,
    }
    serde_json::from_slice::<PluginManifest>(bytes)
        .ok()?
        .id
        .filter(|id| !id.is_empty())
}

/// Plugin id recorded by an installed manifest, `None` when the manifest is
/// absent or not a QwenPaw plugin manifest.
fn installed_id(ops: &dyn AdapterOps, manifest: &Path) -> Result<Option<String>, AdapterError> {
    Ok(ops
        .read_file(manifest)?
        .and_then(|bytes| parse_manifest_id(&bytes)))
}

/// Files `qwenpaw plugin install` copies verbatim from the bundle. Verified
/// against QwenPaw 2.2.0, which copies the bundle directory as-is; re-verify
/// when a QwenPaw release starts rewriting any of them. Subdirectories are
/// not compared.
const BUNDLE_FILES: [&str; 3] = [MANIFEST, "plugin.py", "requirements.txt"];

/// Why the installed directory does not hold the source bundle, if it does not.
fn installed_bundle_mismatch(
    ops: &dyn AdapterOps,
    source_root: &Path,
    installed_dir: &Path,
    plugin_id: &str,
) -> Result<Option<String>, AdapterError> {
    let manifest = installed_dir.join(MANIFEST);
    if installed_id(ops, &manifest)?.as_deref() != Some(plugin_id) {
        return Ok(Some(format!(
            "{} does not describe plugin '{plugin_id}'",
            manifest.display()
        )));
    }
    for file in BUNDLE_FILES {
        if ops.read_file(&source_root.join(file))? != ops.read_file(&installed_dir.join(file))? {
            return Ok(Some(format!(
                "{} does not match the source bundle",
                installed_dir.join(file).display()
            )));
        }
    }
    Ok(None)
}

/// External path recorded in the receipt under `resource`.
fn external_path(claim: &AdapterClaim, resource: &str) -> Option<PathBuf> {
    match &claim.resource(resource)?.kind {
        ClaimResourceKind::ExternalPath { path } => Some(path.clone()),
        _ => None,
    }
}

/// Installed plugin directory recorded in the receipt.
fn plugin_dir(claim: &AdapterClaim) -> Option<PathBuf> {
    let DriverPayload::QwenPaw(payload) = &claim.driver_payload else {
        return None;
    };
    external_path(claim, &payload.plugin_resource)
}

/// Working directory recorded in the receipt. The ambient resolution follows
/// the environment and whether `~/.copaw` exists, both of which may have
/// changed since enable, so disable must not re-resolve it.
fn claim_home(claim: &AdapterClaim) -> Option<PathBuf> {
    let DriverPayload::QwenPaw(payload) = &claim.driver_payload else {
        return None;
    };
    external_path(claim, &payload.home_resource)
}

/// Plugin id from a bundle, or [`AdapterError::BundleInvalid`] when none
/// is resolvable.
fn require_plugin_id(bundle: &AdapterBundle) -> Result<String, AdapterError> {
    bundle
        .plugin_id
        .clone()
        .ok_or_else(|| AdapterError::BundleInvalid {
            root: bundle.resource_root.clone(),
            reason: "no plugin id in plugin.json".to_string(),
        })
}

/// QwenPaw working directory, or [`AdapterError::FrameworkCli`] when `$HOME`
/// is unresolvable and no working-dir variable is set.
fn require_home(ctx: &DriverCtx) -> Result<PathBuf, AdapterError> {
    qwenpaw_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
        program: qwenpaw_bin(),
        reason: "cannot resolve QwenPaw working directory (no $HOME and no QWENPAW_WORKING_DIR)"
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, MutexGuard};

    const ENV_KEYS: [&str; 4] = [
        "QWENPAW_BIN",
        "QWENPAW_WORKING_DIR",
        "COPAW_WORKING_DIR",
        "QWENPAW_HOME",
    ];
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: [(&'static str, Option<OsString>); 4],
    }

    impl EnvGuard {
        fn acquire() -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let saved = ENV_KEYS.map(|key| (key, std::env::var_os(key)));
            // SAFETY: every QwenPaw unit test that reads or mutates these
            // process-wide variables holds ENV_LOCK.
            unsafe {
                for key in ENV_KEYS {
                    std::env::remove_var(key);
                }
            }
            Self { _lock: lock, saved }
        }

        fn set(&self, key: &'static str, value: impl AsRef<OsStr>) {
            assert!(ENV_KEYS.contains(&key));
            // SAFETY: this guard holds ENV_LOCK.
            unsafe { std::env::set_var(key, value) }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: the lock remains held while restoring the original
            // process environment for the next test.
            unsafe {
                for (key, value) in &self.saved {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    #[test]
    fn working_dir_resolution_matches_qwenpaw() {
        let env = EnvGuard::acquire();
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            qwenpaw_home(Some(home.path())),
            Some(home.path().join(".qwenpaw"))
        );
        std::fs::create_dir(home.path().join(".copaw")).expect("mkdir");
        assert_eq!(
            qwenpaw_home(Some(home.path())),
            Some(home.path().join(".copaw"))
        );
        env.set("COPAW_WORKING_DIR", "/opt/copaw/");
        assert_eq!(
            qwenpaw_home(Some(home.path())),
            Some(PathBuf::from("/opt/copaw"))
        );
        env.set("QWENPAW_WORKING_DIR", "/opt/qwenpaw");
        assert_eq!(
            qwenpaw_home(Some(home.path())),
            Some(PathBuf::from("/opt/qwenpaw"))
        );
        env.set("QWENPAW_WORKING_DIR", "");
        assert_eq!(
            qwenpaw_home(Some(home.path())),
            Some(PathBuf::from("/opt/copaw"))
        );
        env.set("COPAW_WORKING_DIR", "");
        assert_eq!(qwenpaw_home(None), None);
    }

    #[test]
    fn install_cmd_pins_working_dir_and_forces_overwrite() {
        let env = EnvGuard::acquire();
        env.set("QWENPAW_HOME", "/opt/qwenpaw-venv");
        let cmd = build_install_cmd(
            Path::new("/home/alice/.qwenpaw"),
            Some(Path::new("/home/alice")),
            Path::new("/usr/share/anolisa/adapters/tokenless/qwenpaw"),
        );
        assert_eq!(cmd.program, "qwenpaw");
        assert_eq!(
            cmd.args,
            vec![
                "plugin",
                "install",
                "/usr/share/anolisa/adapters/tokenless/qwenpaw",
                "--force"
            ]
        );
        assert_eq!(
            cmd.env_set,
            vec![(
                "QWENPAW_WORKING_DIR".to_string(),
                "/home/alice/.qwenpaw".to_string()
            )]
        );
        assert_eq!(cmd.env_remove, vec!["COPAW_WORKING_DIR"]);
        assert_eq!(
            cmd.path_prepend,
            vec![
                PathBuf::from("/opt/qwenpaw-venv/bin"),
                PathBuf::from("/home/alice/.local/bin"),
                PathBuf::from("/usr/local/bin"),
            ]
        );
        assert_eq!(cmd.timeout, INSTALL_TIMEOUT);
        assert!(cmd.stdin.is_none());
    }

    #[test]
    fn uninstall_cmd_answers_confirmation() {
        let env = EnvGuard::acquire();
        env.set("QWENPAW_BIN", "/opt/bin/qwenpaw");
        let cmd = build_uninstall_cmd(
            Path::new("/home/alice/.qwenpaw"),
            Some(Path::new("/home/alice")),
            "tokenless",
        );
        assert_eq!(cmd.program, "/opt/bin/qwenpaw");
        assert_eq!(cmd.args, vec!["plugin", "uninstall", "tokenless"]);
        assert_eq!(cmd.stdin.as_deref(), Some(&b"y\n"[..]));
        assert_eq!(
            cmd.path_prepend[0],
            PathBuf::from("/home/alice/.qwenpaw/bin")
        );
    }

    #[test]
    fn manifest_id_requires_non_empty_id() {
        assert_eq!(
            parse_manifest_id(br#"{"id":"tokenless","version":"1.0.0"}"#).as_deref(),
            Some("tokenless")
        );
        assert_eq!(parse_manifest_id(br#"{"id":""}"#), None);
        assert_eq!(parse_manifest_id(br#"{"name":"x"}"#), None);
        assert_eq!(parse_manifest_id(b"not json"), None);
    }

    #[test]
    fn read_bundle_rejects_declared_id_mismatch() {
        use crate::adapter::driver::{AdapterOps, CliOutput};

        struct StubOps;
        impl AdapterOps for StubOps {
            fn run_framework_cli(&self, _: FrameworkCommand) -> Result<CliOutput, AdapterError> {
                unimplemented!()
            }
            fn copy_tree(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn copy_file(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn remove_tree(&self, _: &Path) -> Result<bool, AdapterError> {
                unimplemented!()
            }
            fn write_file(&self, _: &Path, _: &[u8]) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn create_symlink(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn read_file(&self, _: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
                unimplemented!()
            }
        }

        let _env = EnvGuard::acquire();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(MANIFEST), br#"{"id":"tokenless"}"#).expect("write");
        let layout = anolisa_platform::fs_layout::FsLayout::user(dir.path().join("home"));
        let ops = StubOps;
        let mut ctx = DriverCtx {
            component: "tokenless".to_string(),
            framework: "qwenpaw".to_string(),
            layout: &layout,
            resource_root: dir.path().to_path_buf(),
            user_home: Some(dir.path().join("home")),
            declared_plugin_id: Some("other".to_string()),
            requested_profiles: Vec::new(),
            adapter_type: Some("plugin".to_string()),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: Some(MANIFEST.to_string()),
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: true,
            ops: &ops,
        };
        let err = QwenPawDriver::new()
            .read_bundle(&ctx)
            .expect_err("mismatch must be rejected");
        assert!(matches!(err, AdapterError::BundleInvalid { .. }), "{err:?}");

        ctx.declared_plugin_id = Some("tokenless".to_string());
        let bundle = QwenPawDriver::new().read_bundle(&ctx).expect("bundle");
        assert_eq!(bundle.plugin_id.as_deref(), Some("tokenless"));
        let plan = QwenPawDriver::new()
            .plan_enable(&bundle, &ctx)
            .expect("plan");
        let expected_home = dir.path().join("home").join(".qwenpaw");
        assert_eq!(
            plan.register_command.as_deref(),
            Some(
                format!(
                    "QWENPAW_WORKING_DIR={} qwenpaw plugin install {} --force",
                    expected_home.display(),
                    dir.path().display()
                )
                .as_str()
            )
        );
        let (claim, _) = QwenPawDriver::new()
            .prepare_enable(&bundle, &ctx)
            .expect("claim");
        assert_eq!(
            plugin_dir(&claim),
            Some(expected_home.join("plugins").join("tokenless"))
        );
    }
}
