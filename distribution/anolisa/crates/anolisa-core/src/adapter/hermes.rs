//! Hermes framework driver.
//!
//! Hermes manages plugins and skills under `$HERMES_HOME` (default
//! `~/.hermes`). Plugin adapters place the resource root into
//! `$HERMES_HOME/plugins/<plugin_id>/` and run
//! `hermes plugins enable <plugin_id>`. Skill-only adapters
//! (`adapter_type = "skill_bundle"`) skip plugin placement and only copy
//! declared skills into `$HERMES_HOME/skills/`. Status uses the read-only
//! `hermes plugins list --plain --no-bundled` for plugin adapters. All
//! CLI and filesystem operations go through the Manager's
//! [`AdapterOps`](super::driver::AdapterOps) — the driver never
//! performs direct IO.
//!
//! The CLI env contract: `HERMES_BIN` overrides the executable (used by
//! tests to point at a fake CLI); `HERMES_HOME` overrides the home
//! directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    DRIVER_SCHEMA_VERSION, DriverPayload, HermesClaim, validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx, DriverPlan,
    FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable, find_binary_in_path,
};
use super::managed_files::{MaterializedMapping, copy_materialized_resource};

/// Default timeout for a Hermes CLI invocation.
const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Resource ids used in Hermes receipts. Stable strings referenced from
/// the [`HermesClaim`] payload and condition reports.
const RES_HOME: &str = "hermes_home";
const RES_PLUGIN: &str = "hermes_plugin";

/// Hermes driver. Stateless; all per-operation context arrives via
/// [`DriverCtx`].
pub struct HermesDriver;

impl HermesDriver {
    /// Construct the driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for HermesDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for HermesDriver {
    fn name(&self) -> &'static str {
        "hermes"
    }

    fn detect(&self, env: &HostEnv) -> DetectResult {
        match find_binary_in_path(&hermes_bin()) {
            Some(path) => DetectResult {
                detected: true,
                reason: format!("hermes CLI found at {}", path.display()),
            },
            None => {
                // The CLI is what enable/disable need; a bare home dir is
                // not sufficient. Report not-detected but mention the home
                // so a user understands the framework is partially present.
                let home_note = hermes_home(env.user_home.as_deref())
                    .filter(|h| h.exists())
                    .map(|h| format!(" (home {} exists but CLI is not on PATH)", h.display()))
                    .unwrap_or_default();
                DetectResult {
                    detected: false,
                    reason: format!("hermes CLI not found on PATH{home_note}"),
                }
            }
        }
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        // The only external root Hermes writes is its own home dir.
        hermes_home(ctx.user_home.as_deref()).into_iter().collect()
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        let root = &ctx.resource_root;
        if !root.is_dir() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: "resource root does not exist or is not a directory".to_string(),
            });
        }
        let is_empty = root
            .read_dir()
            .map_err(|source| AdapterError::Io {
                path: root.clone(),
                source,
            })?
            .next()
            .is_none();
        if is_empty {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: "resource root is empty".to_string(),
            });
        }

        let plugin_id = if ctx.is_skill_bundle() {
            None
        } else {
            match ctx.declared_plugin_id.clone().filter(|id| !id.is_empty()) {
                Some(id) => Some(id),
                None => read_plugin_manifest_id(root, ctx.declared_bundle_entry.as_deref())?
                    .or_else(|| Some(ctx.component.clone())),
            }
        };

        Ok(AdapterBundle {
            resource_root: root.clone(),
            plugin_id,
        })
    }

    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError> {
        let home = require_home(ctx)?;
        let mut actions = Vec::new();
        let mut register_command = None;
        if !ctx.is_skill_bundle() {
            let plugin_id = require_plugin_id(bundle)?;
            validate_plugin_id(&plugin_id)?;
            let enable_cmd = build_enable_cmd(&plugin_id);
            register_command = Some(display_command(&enable_cmd));
            actions.push(format!(
                "place hermes plugin '{plugin_id}' from {} to {}/plugins/{plugin_id}",
                bundle.resource_root.display(),
                home.display(),
            ));
            actions.push(format!("enable hermes plugin '{plugin_id}'"));
        }

        for skill in &ctx.declared_skills {
            let src_display = match skill.source {
                Some(ref s) => s.display().to_string(),
                None => format!("{}/skills/{}", bundle.resource_root.display(), skill.name,),
            };
            actions.push(format!(
                "deliver skill '{}' from {} to {}/skills/{}",
                skill.name,
                src_display,
                home.display(),
                skill.name,
            ));
        }

        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions,
            register_command,
        })
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let home = require_home(ctx)?;
        let mut resources = vec![ClaimResource {
            id: RES_HOME.to_string(),
            purpose: "hermes_home".to_string(),
            kind: ClaimResourceKind::ExternalPath { path: home.clone() },
        }];
        let plugin_id = if ctx.is_skill_bundle() {
            None
        } else {
            let plugin_id = require_plugin_id(bundle)?;
            validate_plugin_id(&plugin_id)?;
            resources.push(ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: "hermes_plugin_dir".to_string(),
                kind: ClaimResourceKind::ExternalPath {
                    path: home.join("plugins").join(&plugin_id),
                },
            });
            Some(plugin_id)
        };

        let mut skill_resource_ids = Vec::new();
        for skill in &ctx.declared_skills {
            let res_id = format!("hermes_skill_{}", skill.name);
            resources.push(ClaimResource {
                id: res_id.clone(),
                purpose: "hermes_skill".to_string(),
                kind: ClaimResourceKind::ExternalPath {
                    path: home.join("skills").join(&skill.name),
                },
            });
            skill_resource_ids.push(res_id);
        }

        Ok((
            AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: ctx.component.clone(),
                framework: self.name().to_string(),
                plugin_id,
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
                driver_payload: DriverPayload::Hermes(HermesClaim {
                    home_resource: RES_HOME.to_string(),
                    plugin_resource: if ctx.is_skill_bundle() {
                        String::new()
                    } else {
                        RES_PLUGIN.to_string()
                    },
                    skill_resources: skill_resource_ids,
                }),
            },
            PreparedEnable::None,
        ))
    }

    fn materialized_mappings(
        &self,
        resource_root: &Path,
        adapter_type: Option<&str>,
        declared_skills: &[super::driver::DeclaredSkill],
    ) -> Vec<MaterializedMapping> {
        let mut mappings = Vec::new();
        if adapter_type != Some("skill_bundle") {
            mappings.push(MaterializedMapping {
                resource_id: RES_PLUGIN.to_string(),
                source_root: resource_root.to_path_buf(),
                excluded_prefixes: vec![PathBuf::from("skills")],
            });
        }
        for skill in declared_skills {
            mappings.push(MaterializedMapping {
                resource_id: format!("hermes_skill_{}", skill.name),
                source_root: skill
                    .source
                    .clone()
                    .unwrap_or_else(|| resource_root.join("skills").join(&skill.name)),
                excluded_prefixes: Vec::new(),
            });
        }
        mappings
    }

    fn materialized_destination_roots(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<BTreeMap<String, PathBuf>, AdapterError> {
        let home = require_home(ctx)?;
        let mut roots = BTreeMap::new();
        if !ctx.is_skill_bundle() {
            let plugin_id = require_plugin_id(bundle)?;
            validate_plugin_id(&plugin_id)?;
            roots.insert(RES_PLUGIN.to_string(), home.join("plugins").join(plugin_id));
        }
        for skill in &ctx.declared_skills {
            roots.insert(
                format!("hermes_skill_{}", skill.name),
                home.join("skills").join(&skill.name),
            );
        }
        Ok(roots)
    }

    fn materialized_verification_applicable(&self, _claim: &AdapterClaim) -> bool {
        true
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        _prepared: &PreparedEnable,
        ctx: &DriverCtx,
        _progress: &mut dyn super::driver::EnableProgress,
    ) -> Result<(), AdapterError> {
        require_home(ctx)?;

        if !ctx.is_skill_bundle() {
            let plugin_id = claim_plugin_id(claim).ok_or_else(|| AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: "hermes receipt has no plugin id".to_string(),
            })?;
            validate_plugin_id(&plugin_id)?;

            copy_materialized_resource(claim, RES_PLUGIN, &claim.resource_root, ctx.ops)?;

            let enable_cmd = build_enable_cmd(&plugin_id);
            let program = enable_cmd.program.clone();
            let output = ctx.ops.run_framework_cli(enable_cmd)?;
            if !output.success() {
                return Err(AdapterError::FrameworkCli {
                    program,
                    reason: cli_failure_reason("plugins enable", &output),
                });
            }
        }

        for skill in &ctx.declared_skills {
            let src = skill
                .source
                .clone()
                .unwrap_or_else(|| claim.resource_root.join("skills").join(&skill.name));
            let resource_id = format!("hermes_skill_{}", skill.name);
            copy_materialized_resource(claim, &resource_id, &src, ctx.ops)?;
        }

        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        let mut conditions = Vec::new();

        // 1. Framework detectable?
        let detect = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::FrameworkDetected,
            status: bool_status(detect.detected),
            reason: Some(detect.reason.clone()),
            resource: None,
        });

        // 2. Plugin still registered? Skill-only receipts have no plugin
        //    registry entry by design, so status does not require one.
        let plugin_registered = if claim.is_skill_bundle() {
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::VerificationSupported,
                status: bool_status(detect.detected),
                reason: Some("skill_bundle has no plugin registry entry".to_string()),
                resource: None,
            });
            ConditionStatus::True
        } else {
            let plugin_id = claim_plugin_id(claim);
            let (plugin_cond, verify_cond, plugin_registered) = if !detect.detected {
                (
                    AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: ConditionStatus::Unknown,
                        reason: Some("framework not detected; cannot verify".to_string()),
                        resource: plugin_id.as_ref().map(|_| ClaimResourceRef {
                            id: RES_PLUGIN.to_string(),
                        }),
                    },
                    AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::False,
                        reason: Some("hermes CLI unavailable".to_string()),
                        resource: None,
                    },
                    ConditionStatus::Unknown,
                )
            } else if let Some(pid) = &plugin_id {
                self.plugin_registered_condition(pid, ctx)
            } else {
                (
                    AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: ConditionStatus::Unknown,
                        reason: Some("receipt has no plugin id".to_string()),
                        resource: None,
                    },
                    AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::True,
                        reason: None,
                        resource: None,
                    },
                    ConditionStatus::Unknown,
                )
            };
            conditions.push(plugin_cond);
            conditions.push(verify_cond);
            plugin_registered
        };

        let summary = summarize(claim.status, detect.detected, plugin_registered);
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
        let mut messages = Vec::new();
        let mut cleanup_complete = true;
        let home = require_home(ctx)?;

        if let Some(plugin_id) = claim_plugin_id(claim) {
            validate_plugin_id(&plugin_id)?;
            if find_binary_in_path(&hermes_bin()).is_none() {
                return Ok(DisableReport {
                    cleanup_complete: false,
                    messages: vec![
                        "hermes CLI not found on PATH; receipt kept so cleanup can be retried"
                            .to_string(),
                    ],
                });
            }

            let disable_cmd = build_disable_cmd(&plugin_id);
            let _ = ctx.ops.run_framework_cli(disable_cmd);

            let plugin_dir = home.join("plugins").join(&plugin_id);
            match ctx.ops.remove_tree(&plugin_dir) {
                Ok(true) => messages.push(format!(
                    "removed hermes plugin directory {}",
                    plugin_dir.display()
                )),
                Ok(false) => messages.push(format!(
                    "hermes plugin directory {} already absent",
                    plugin_dir.display()
                )),
                Err(err) => {
                    cleanup_complete = false;
                    messages.push(format!(
                        "failed to remove hermes plugin directory {}: {err}",
                        plugin_dir.display()
                    ));
                }
            }
        } else {
            messages.push("receipt records no plugin to unregister".to_string());
        }

        if let DriverPayload::Hermes(ref hermes) = claim.driver_payload {
            for skill_res_id in &hermes.skill_resources {
                if let Some(resource) = claim.resource(skill_res_id)
                    && let ClaimResourceKind::ExternalPath { path } = &resource.kind
                {
                    match ctx.ops.remove_tree(path) {
                        Ok(true) => {
                            messages.push(format!("removed skill dir {}", path.display()));
                        }
                        Ok(false) => {} // already gone
                        Err(err) => {
                            cleanup_complete = false;
                            messages.push(format!(
                                "failed to remove skill dir {}: {err}",
                                path.display()
                            ));
                        }
                    }
                }
            }
        }

        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }
}

impl HermesDriver {
    /// Run `hermes plugins list` and decide whether `plugin_id` is still
    /// registered. Returns `(plugin_condition, verification_condition,
    /// plugin_registered_status)`.
    fn plugin_registered_condition(
        &self,
        plugin_id: &str,
        ctx: &DriverCtx,
    ) -> (AdapterCondition, AdapterCondition, ConditionStatus) {
        let plugin_ref = Some(ClaimResourceRef {
            id: RES_PLUGIN.to_string(),
        });
        let cmd = build_list_cmd();
        match ctx.ops.run_framework_cli(cmd) {
            Ok(output) if output.success() => {
                let registered = list_contains_plugin(&output.stdout, plugin_id);
                (
                    AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: bool_status(registered),
                        reason: (!registered)
                            .then(|| "plugin not present in `plugins list`".to_string()),
                        resource: plugin_ref,
                    },
                    AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::True,
                        reason: None,
                        resource: None,
                    },
                    bool_status(registered),
                )
            }
            // The list probe ran but failed, or could not spawn: we cannot
            // verify. Report Unknown, never a faked healthy/absent.
            Ok(_) | Err(_) => (
                AdapterCondition {
                    kind: AdapterConditionKind::PluginRegistered,
                    status: ConditionStatus::Unknown,
                    reason: Some("`plugins list` did not return a usable result".to_string()),
                    resource: plugin_ref,
                },
                AdapterCondition {
                    kind: AdapterConditionKind::VerificationSupported,
                    status: ConditionStatus::False,
                    reason: Some("`plugins list` unavailable".to_string()),
                    resource: None,
                },
                ConditionStatus::Unknown,
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (no spawning) — unit-testable
// ---------------------------------------------------------------------------

/// `HERMES_BIN` override, else `hermes`.
fn hermes_bin() -> String {
    std::env::var("HERMES_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "hermes".to_string())
}

/// Resolve the Hermes home directory: `HERMES_HOME`, else
/// `<user_home>/.hermes`. Trailing slashes are trimmed.
fn hermes_home(user_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("HERMES_HOME") {
        let s = h.to_string_lossy();
        let trimmed = s.trim_end_matches('/');
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    user_home.map(|h| h.join(".hermes"))
}

/// Build a bare Hermes command with the given args. Hermes has a simpler
/// env contract than OpenClaw: no state-dir rewriting, no PATH prepend.
fn base_cmd(args: Vec<String>) -> FrameworkCommand {
    FrameworkCommand {
        program: hermes_bin(),
        args,
        stdin: None,
        env_set: Vec::new(),
        env_remove: Vec::new(),
        path_prepend: Vec::new(),
        timeout: CLI_TIMEOUT,
    }
}

/// Build `hermes plugins enable <plugin_id>`.
fn build_enable_cmd(plugin_id: &str) -> FrameworkCommand {
    base_cmd(vec![
        "plugins".to_string(),
        "enable".to_string(),
        plugin_id.to_string(),
    ])
}

/// Build `hermes plugins disable <plugin_id>`.
fn build_disable_cmd(plugin_id: &str) -> FrameworkCommand {
    base_cmd(vec![
        "plugins".to_string(),
        "disable".to_string(),
        plugin_id.to_string(),
    ])
}

/// Build the read-only `hermes plugins list --plain --no-bundled`.
///
/// `--plain` avoids rich table formatting that breaks token-based
/// parsing. `--no-bundled` excludes built-in plugins so the output
/// reflects only explicitly installed entries.
fn build_list_cmd() -> FrameworkCommand {
    base_cmd(vec![
        "plugins".to_string(),
        "list".to_string(),
        "--plain".to_string(),
        "--no-bundled".to_string(),
    ])
}

/// Plugin id declared by a Hermes-native manifest, when present.
///
/// When `declared_entry` is given, uses only that file; otherwise tries
/// `hermes.plugin.json` then `hermes.manifest.yaml`. Falls back to `None`.
///
/// File format is determined by extension: `.yaml`/`.yml` are parsed with
/// a simple line scan for `id: <value>`; `.json` (and the default
/// fallback `hermes.plugin.json`) are parsed as JSON.
fn read_plugin_manifest_id(
    root: &Path,
    declared_entry: Option<&str>,
) -> Result<Option<String>, AdapterError> {
    if let Some(entry) = declared_entry {
        let lower = entry.to_ascii_lowercase();
        if lower.ends_with(".yaml") || lower.ends_with(".yml") {
            return read_yaml_id(root, entry);
        }
        return read_json_id(root, entry);
    }

    // No declared entry: try JSON then YAML fallback.
    if let Some(id) = read_json_id(root, "hermes.plugin.json")? {
        return Ok(Some(id));
    }
    read_yaml_id(root, "hermes.manifest.yaml")
}

/// Read the `id` field from a JSON manifest file.
fn read_json_id(root: &Path, filename: &str) -> Result<Option<String>, AdapterError> {
    #[derive(serde::Deserialize)]
    struct PluginManifest {
        id: Option<String>,
    }

    let path = root.join(filename);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(AdapterError::Io { path, source }),
    };
    let manifest: PluginManifest =
        serde_json::from_slice(&bytes).map_err(|source| AdapterError::BundleInvalid {
            root: root.to_path_buf(),
            reason: format!(
                "failed to parse {} as Hermes plugin manifest: {source}",
                path.display()
            ),
        })?;
    Ok(manifest.id.filter(|id| !id.is_empty()))
}

/// Read the `id` field from a YAML manifest via a minimal line scan.
fn read_yaml_id(root: &Path, filename: &str) -> Result<Option<String>, AdapterError> {
    let path = root.join(filename);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(AdapterError::Io { path, source }),
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("id:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Ok(Some(value.to_string()));
            }
        }
    }
    Ok(None)
}

/// Human-readable form of a command for dry-run/preview output. Display
/// only — never parsed back into an argv.
fn display_command(cmd: &FrameworkCommand) -> String {
    let mut s = String::new();
    for (k, v) in &cmd.env_set {
        s.push_str(&format!("{k}={v} "));
    }
    s.push_str(&cmd.program);
    for a in &cmd.args {
        s.push(' ');
        s.push_str(a);
    }
    s
}

/// True when `plugin_id` appears as a whole whitespace-delimited token on
/// any line of `plugins list` output. Tolerant of decoration like
/// `- agent-sec (v1.2)`.
fn list_contains_plugin(stdout: &str, plugin_id: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.split_whitespace().any(|tok| tok == plugin_id))
}

/// Extract the validated plugin id from a claim's resources, falling back
/// to the top-level `plugin_id` field.
fn claim_plugin_id(claim: &AdapterClaim) -> Option<String> {
    // Hermes uses ExternalPath for the plugin dir; the plugin id is in the
    // top-level field.
    claim.plugin_id.clone()
}

/// Plugin id from a bundle, or [`AdapterError::BundleInvalid`] when none
/// is resolvable.
fn require_plugin_id(bundle: &AdapterBundle) -> Result<String, AdapterError> {
    bundle
        .plugin_id
        .clone()
        .ok_or_else(|| AdapterError::BundleInvalid {
            root: bundle.resource_root.clone(),
            reason: "no plugin id declared in manifest and none discoverable".to_string(),
        })
}

/// Hermes home, or [`AdapterError::FrameworkCli`] when `$HOME` is
/// unresolvable (no `user_home`, no `HERMES_HOME`).
fn require_home(ctx: &DriverCtx) -> Result<PathBuf, AdapterError> {
    hermes_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
        program: hermes_bin(),
        reason: "cannot resolve Hermes home (no $HOME and no HERMES_HOME)".to_string(),
    })
}

/// Compose a failure reason string from a non-success [`CliOutput`].
fn cli_failure_reason(verb: &str, output: &super::driver::CliOutput) -> String {
    if output.timed_out {
        return format!("'{verb}' timed out");
    }
    let code = output
        .status
        .map(|c| c.to_string())
        .unwrap_or_else(|| "killed".to_string());
    let mut reason = format!("'{verb}' exited with {code}");
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        reason.push_str(": ");
        reason.push_str(stderr);
    }
    reason
}

/// Map a bool to a [`ConditionStatus`] (`true` -> `True`, `false` -> `False`).
fn bool_status(b: bool) -> ConditionStatus {
    if b {
        ConditionStatus::True
    } else {
        ConditionStatus::False
    }
}

/// Roll the framework-detect and plugin-registration signals into a
/// summary, honoring a `cleanup_failed` receipt.
fn summarize(
    claim_status: ClaimStatus,
    framework_detected: bool,
    plugin_registered: ConditionStatus,
) -> AdapterSummary {
    if claim_status == ClaimStatus::CleanupFailed {
        return AdapterSummary::CleanupFailed;
    }
    if !framework_detected {
        return AdapterSummary::Degraded;
    }
    match plugin_registered {
        ConditionStatus::True => AdapterSummary::Healthy,
        ConditionStatus::False => AdapterSummary::Degraded,
        ConditionStatus::Unknown => AdapterSummary::Unknown,
    }
}

/// ISO 8601 UTC timestamp, second precision.
fn now_iso8601() -> String {
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, MutexGuard};

    const HERMES_ENV_KEYS: [&str; 2] = ["HERMES_BIN", "HERMES_HOME"];
    static HERMES_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct HermesEnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: [(&'static str, Option<OsString>); 2],
    }

    impl HermesEnvGuard {
        fn acquire() -> Self {
            let lock = HERMES_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let saved = HERMES_ENV_KEYS.map(|key| (key, std::env::var_os(key)));
            // SAFETY: every Hermes unit test that reads or mutates these
            // process-wide variables holds HERMES_ENV_LOCK.
            unsafe {
                for key in HERMES_ENV_KEYS {
                    std::env::remove_var(key);
                }
            }
            Self { _lock: lock, saved }
        }

        fn set(&self, key: &'static str, value: impl AsRef<OsStr>) {
            assert!(HERMES_ENV_KEYS.contains(&key));
            // SAFETY: this guard holds HERMES_ENV_LOCK.
            unsafe { std::env::set_var(key, value) }
        }

        fn remove(&self, key: &'static str) {
            assert!(HERMES_ENV_KEYS.contains(&key));
            // SAFETY: this guard holds HERMES_ENV_LOCK.
            unsafe { std::env::remove_var(key) }
        }
    }

    impl Drop for HermesEnvGuard {
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
    fn hermes_home_resolution() {
        let env = HermesEnvGuard::acquire();
        // With env var set.
        env.set("HERMES_HOME", "/opt/hermes");
        assert_eq!(
            hermes_home(Some(Path::new("/home/alice"))),
            Some(PathBuf::from("/opt/hermes"))
        );
        // Trailing slashes are stripped.
        env.set("HERMES_HOME", "/opt/hermes///");
        assert_eq!(
            hermes_home(Some(Path::new("/home/alice"))),
            Some(PathBuf::from("/opt/hermes"))
        );
        // Empty env var falls back to user_home.
        env.set("HERMES_HOME", "");
        assert_eq!(
            hermes_home(Some(Path::new("/home/alice"))),
            Some(PathBuf::from("/home/alice/.hermes"))
        );
        // No env var, no user_home.
        env.remove("HERMES_HOME");
        assert_eq!(hermes_home(None), None);
        // No env var, with user_home.
        assert_eq!(
            hermes_home(Some(Path::new("/home/bob"))),
            Some(PathBuf::from("/home/bob/.hermes"))
        );
    }

    #[test]
    fn list_contains_plugin_matches_whole_token() {
        assert!(list_contains_plugin("agent-sec\nother\n", "agent-sec"));
        assert!(list_contains_plugin("- agent-sec (v1.2)\n", "agent-sec"));
        assert!(!list_contains_plugin("agent-sec-extra\n", "agent-sec"));
        assert!(!list_contains_plugin("", "agent-sec"));
    }

    #[test]
    fn list_contains_plugin_plain_output() {
        let plain = "agent-sec-core-hermes-plugin\nanother-plugin\n";
        assert!(list_contains_plugin(plain, "agent-sec-core-hermes-plugin"));
        assert!(list_contains_plugin(plain, "another-plugin"));
        assert!(!list_contains_plugin(plain, "not-here"));
    }

    #[test]
    fn list_cmd_uses_plain_and_no_bundled() {
        let _env = HermesEnvGuard::acquire();
        let cmd = build_list_cmd();
        assert_eq!(cmd.program, "hermes");
        assert_eq!(cmd.args, vec!["plugins", "list", "--plain", "--no-bundled"]);
    }

    #[test]
    fn enable_cmd_shape() {
        let _env = HermesEnvGuard::acquire();
        let cmd = build_enable_cmd("agent-sec");
        assert_eq!(cmd.program, "hermes");
        assert_eq!(cmd.args, vec!["plugins", "enable", "agent-sec"]);
    }

    #[test]
    fn disable_cmd_shape() {
        let _env = HermesEnvGuard::acquire();
        let cmd = build_disable_cmd("agent-sec");
        assert_eq!(cmd.args, vec!["plugins", "disable", "agent-sec"]);
    }

    #[test]
    fn summarize_prioritizes_cleanup_failed() {
        assert_eq!(
            summarize(ClaimStatus::CleanupFailed, true, ConditionStatus::True),
            AdapterSummary::CleanupFailed
        );
    }

    #[test]
    fn summarize_healthy_only_when_detected_and_registered() {
        assert_eq!(
            summarize(ClaimStatus::Enabled, true, ConditionStatus::True),
            AdapterSummary::Healthy
        );
        assert_eq!(
            summarize(ClaimStatus::Enabled, false, ConditionStatus::True),
            AdapterSummary::Degraded
        );
        assert_eq!(
            summarize(ClaimStatus::Enabled, true, ConditionStatus::False),
            AdapterSummary::Degraded
        );
        assert_eq!(
            summarize(ClaimStatus::Enabled, true, ConditionStatus::Unknown),
            AdapterSummary::Unknown
        );
    }

    // -- review fix coverage: lazy plugin_id, YAML entry, skills allowlist --

    use crate::adapter::claim::{DriverPayload, HermesClaim};
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

    #[test]
    fn declared_plugin_id_skips_manifest_parse() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Invalid content that would fail if read_plugin_manifest_id parsed it.
        std::fs::write(dir.path().join("hermes.plugin.json"), b"NOT JSON {{").expect("write");

        let driver = HermesDriver::new();
        let ops = StubOps;
        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/test-home-a"));
        let ctx = DriverCtx {
            component: "test-comp".to_string(),
            framework: "hermes".to_string(),
            layout: &layout,
            resource_root: dir.path().to_path_buf(),
            user_home: Some(PathBuf::from("/tmp/test-home-a")),
            declared_plugin_id: Some("agent-sec".to_string()),
            requested_profiles: Vec::new(),
            adapter_type: None,
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: true,
            ops: &ops,
        };

        let bundle = driver
            .read_bundle(&ctx)
            .expect("must succeed without parsing manifest");
        assert_eq!(bundle.plugin_id.as_deref(), Some("agent-sec"));
    }

    #[test]
    fn materialized_mappings_separate_plugin_and_declared_skills() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = anolisa_platform::fs_layout::FsLayout::user(dir.path().join("home"));
        let ops = StubOps;
        let ctx = DriverCtx {
            component: "agent-sec-core".into(),
            framework: "hermes".into(),
            layout: &layout,
            resource_root: dir.path().join("bundle"),
            user_home: Some(dir.path().join("home")),
            declared_plugin_id: Some("agent-sec".into()),
            requested_profiles: Vec::new(),
            adapter_type: Some("plugin".into()),
            declared_skills: vec![super::super::driver::DeclaredSkill {
                name: "sec-audit".into(),
                source: None,
            }],
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: false,
            ops: &ops,
        };
        let mappings = HermesDriver::new().materialized_mappings(
            &ctx.resource_root,
            ctx.adapter_type.as_deref(),
            &ctx.declared_skills,
        );
        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings[0].resource_id, RES_PLUGIN);
        assert_eq!(mappings[0].source_root, ctx.resource_root);
        assert_eq!(mappings[0].excluded_prefixes, vec![PathBuf::from("skills")]);
        assert_eq!(mappings[1].resource_id, "hermes_skill_sec-audit");
        assert_eq!(
            mappings[1].source_root,
            ctx.resource_root.join("skills/sec-audit")
        );
    }

    #[test]
    fn yaml_bundle_entry_parses_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("hermes.manifest.yaml"),
            "id: agent-sec\nname: Agent Security\n",
        )
        .expect("write");

        let id = read_plugin_manifest_id(dir.path(), Some("hermes.manifest.yaml"))
            .expect("parse must succeed")
            .expect("id must be present");
        assert_eq!(id, "agent-sec");
    }

    #[test]
    fn only_declared_skills_are_planned_and_claimed() {
        use crate::adapter::driver::DeclaredSkill;
        let _env = HermesEnvGuard::acquire();
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("skills/sec-audit")).expect("mkdir");
        std::fs::create_dir_all(root.join("skills/undeclared-extra")).expect("mkdir");
        std::fs::write(root.join("dummy.txt"), b"x").expect("write");

        let driver = HermesDriver::new();
        let ops = StubOps;
        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/test-home-c"));
        let ctx = DriverCtx {
            component: "test-comp".to_string(),
            framework: "hermes".to_string(),
            layout: &layout,
            resource_root: root.to_path_buf(),
            user_home: Some(PathBuf::from("/tmp/test-home-c")),
            declared_plugin_id: Some("test-plugin".to_string()),
            requested_profiles: Vec::new(),
            adapter_type: None,
            declared_skills: vec![DeclaredSkill {
                name: "sec-audit".to_string(),
                source: None,
            }],
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: true,
            ops: &ops,
        };

        let bundle = AdapterBundle {
            resource_root: root.to_path_buf(),
            plugin_id: Some("test-plugin".to_string()),
        };

        let plan = driver.plan_enable(&bundle, &ctx).expect("plan_enable");
        assert!(
            plan.actions.iter().any(|a| a.contains("sec-audit")),
            "declared skill must appear in plan"
        );
        assert!(
            !plan.actions.iter().any(|a| a.contains("undeclared-extra")),
            "undeclared skill must not appear in plan"
        );

        let (claim, _prepared) = driver
            .prepare_enable(&bundle, &ctx)
            .expect("prepare_enable");
        let skill_resources: Vec<&str> = claim
            .resources
            .iter()
            .filter(|r| r.purpose == "hermes_skill")
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(skill_resources, vec!["hermes_skill_sec-audit"]);
        if let DriverPayload::Hermes(HermesClaim {
            ref skill_resources,
            ..
        }) = claim.driver_payload
        {
            assert_eq!(skill_resources, &["hermes_skill_sec-audit"]);
        } else {
            panic!("expected Hermes driver payload");
        }
    }

    #[test]
    fn skill_bundle_plan_and_claim_skip_plugin_enable() {
        use crate::adapter::driver::DeclaredSkill;
        let env = HermesEnvGuard::acquire();

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("marker"), b"x").expect("write");

        let driver = HermesDriver::new();
        let ops = StubOps;
        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/test-home-d"));
        let ctx = DriverCtx {
            component: "os-skills".to_string(),
            framework: "hermes".to_string(),
            layout: &layout,
            resource_root: dir.path().to_path_buf(),
            user_home: Some(PathBuf::from("/tmp/test-home-d")),
            declared_plugin_id: None,
            requested_profiles: Vec::new(),
            adapter_type: Some("skill_bundle".to_string()),
            declared_skills: vec![DeclaredSkill {
                name: "install-hermes".to_string(),
                source: Some(PathBuf::from("/usr/share/anolisa/skills/install-hermes")),
            }],
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: true,
            ops: &ops,
        };
        let bundle = driver.read_bundle(&ctx).expect("read bundle");
        assert!(bundle.plugin_id.is_none());

        let plan = driver.plan_enable(&bundle, &ctx).expect("plan");
        assert!(plan.register_command.is_none());
        assert!(
            plan.actions
                .iter()
                .all(|action| !action.contains("enable hermes plugin")),
        );

        let (claim, _prepared) = driver.prepare_enable(&bundle, &ctx).expect("claim");
        assert!(claim.plugin_id.is_none());
        assert_eq!(claim.adapter_type.as_deref(), Some("skill_bundle"));
        let plugin_paths = claim
            .resources
            .iter()
            .filter(|resource| resource.purpose == "hermes_plugin_dir")
            .count();
        assert_eq!(plugin_paths, 0);
        if let DriverPayload::Hermes(HermesClaim {
            ref skill_resources,
            ..
        }) = claim.driver_payload
        {
            assert_eq!(skill_resources, &["hermes_skill_install-hermes"]);
        } else {
            panic!("expected Hermes driver payload");
        }

        env.set("HERMES_BIN", "/bin/sh");
        let report = driver.status(&claim, &ctx).expect("status");

        assert_eq!(report.summary, AdapterSummary::Healthy);
        assert!(
            report
                .conditions
                .iter()
                .all(|condition| condition.kind != AdapterConditionKind::PluginRegistered),
            "skill_bundle status must not require plugin registration"
        );
        assert!(report.conditions.iter().any(|condition| {
            condition.kind == AdapterConditionKind::VerificationSupported
                && condition.status == ConditionStatus::True
        }));
    }
}
