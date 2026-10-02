//! Claude Code framework driver.
//!
//! Claude Code v2 installs plugins from a registered marketplace. ANOLISA
//! exposes the component's resource root (which ships
//! `.claude-plugin/marketplace.json` + `.claude-plugin/plugin.json`) as a
//! single-plugin marketplace, then installs the plugin via the official CLI:
//!
//! ```text
//! claude plugin validate <resource_root>
//! claude plugin marketplace add <resource_root>
//! claude plugin install <plugin>@anolisa-<component>
//! ```
//!
//! **Marketplace name is component-scoped** (`anolisa-<component>`), so two
//! ANOLISA components can each register their own Claude Code marketplace
//! without one's disable removing or shadowing another's. Claude Code names
//! a marketplace after the `name` field in the bundle's
//! `.claude-plugin/marketplace.json` (ANOLISA cannot rename it via the CLI),
//! so `read_bundle` enforces that the bundle declares exactly
//! `anolisa-<component>` — a bundle that still uses the bare shared
//! `anolisa` name is rejected with an actionable error.
//!
//! `disable` reverses this with `claude plugin uninstall` /
//! `claude plugin marketplace remove`. ANOLISA never edits
//! `~/.claude/settings.json` directly: when the CLI is unavailable it
//! reports cleanup as incomplete and keeps the receipt, rather than
//! hand-patching a file it does not own.
//!
//! Env contract: `CLAUDE_BIN` overrides the executable (tests point it at a
//! fake CLI).

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    ClaudeCodeClaim, DRIVER_SCHEMA_VERSION, DriverPayload, validate_marketplace_name,
    validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx, DriverPlan,
    FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable, find_binary_in_path,
};
use super::util::{bool_status, cli_failure_reason, display_command, now_iso8601};

/// Default timeout for a Claude Code CLI invocation.
const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Prefix for the component-scoped Claude Code marketplace name. The full
/// name is `anolisa-<component>` (see [`marketplace_name`]), so each
/// component owns a distinct marketplace and disable never touches another
/// component's registration.
const MARKETPLACE_PREFIX: &str = "anolisa";

/// Native marketplace manifest inside a Claude Code plugin bundle.
const MARKETPLACE_MANIFEST: &str = ".claude-plugin/marketplace.json";
/// Native plugin manifest inside a Claude Code plugin bundle.
const PLUGIN_MANIFEST: &str = ".claude-plugin/plugin.json";

/// Resource ids used in Claude Code receipts.
const RES_MARKETPLACE: &str = "claude_code_marketplace";
const RES_PLUGIN: &str = "claude_code_plugin";

/// Claude Code driver. Stateless; all per-operation context arrives via
/// [`DriverCtx`].
pub struct ClaudeCodeDriver;

impl ClaudeCodeDriver {
    /// Construct the driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeCodeDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for ClaudeCodeDriver {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn probe_bundle(&self, resource_root: &Path, _declared_entry: Option<&str>) -> bool {
        // Mirrors read_bundle's mandatory checks: a Claude Code bundle
        // requires both the marketplace and the plugin manifest,
        // regardless of any contract-declared entry.
        resource_root.join(MARKETPLACE_MANIFEST).is_file()
            && resource_root.join(PLUGIN_MANIFEST).is_file()
    }

    fn detect(&self, _env: &HostEnv) -> DetectResult {
        match find_binary_in_path(&claude_bin()) {
            Some(path) => DetectResult {
                detected: true,
                reason: format!("claude CLI found at {}", path.display()),
            },
            None => DetectResult {
                detected: false,
                reason: "claude CLI not found on PATH".to_string(),
            },
        }
    }

    fn allowed_external_roots(&self, _ctx: &DriverCtx) -> Vec<PathBuf> {
        // Claude Code owns its own registry and settings; ANOLISA writes no
        // external filesystem paths of its own (the marketplace source is
        // the shared resource root, added by reference via the CLI).
        Vec::new()
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        let root = &ctx.resource_root;
        if !root.is_dir() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: "resource root does not exist or is not a directory".to_string(),
            });
        }
        if !root.join(MARKETPLACE_MANIFEST).is_file() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!(
                    "Claude Code marketplace manifest '{MARKETPLACE_MANIFEST}' missing"
                ),
            });
        }
        if !root.join(PLUGIN_MANIFEST).is_file() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!(
                    "Claude Code plugin manifest '{PLUGIN_MANIFEST}' missing (run: make stamp-adapter-templates)"
                ),
            });
        }
        // Claude Code names the marketplace after the manifest's `name`
        // field (ANOLISA cannot rename it via the CLI), so the bundle MUST
        // declare the component-scoped name; otherwise two components would
        // collide on a shared marketplace and one's disable would remove the
        // other's. Enforce it here so the mismatch fails before any state is
        // written.
        let expected = marketplace_name(&ctx.component);
        match read_marketplace_manifest_name(root)? {
            Some(name) if name == expected => {}
            Some(name) => {
                return Err(AdapterError::BundleInvalid {
                    root: root.clone(),
                    reason: format!(
                        "Claude Code marketplace name '{name}' in {MARKETPLACE_MANIFEST} must be component-scoped as '{expected}'"
                    ),
                });
            }
            None => {
                return Err(AdapterError::BundleInvalid {
                    root: root.clone(),
                    reason: format!(
                        "Claude Code {MARKETPLACE_MANIFEST} declares no 'name'; expected '{expected}'"
                    ),
                });
            }
        }
        let plugin_id = Some(
            ctx.declared_plugin_id
                .clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| ctx.component.clone()),
        );
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
        let plugin = plugin_name(bundle, ctx);
        let marketplace = marketplace_name(&ctx.component);
        let plugin_ref = plugin_ref(&plugin, &marketplace);
        let install_cmd = build_plugin_install_cmd(&plugin_ref);
        let actions = vec![
            format!(
                "validate Claude Code plugin at {}",
                bundle.resource_root.display()
            ),
            format!(
                "register Claude Code marketplace '{marketplace}' from {}",
                bundle.resource_root.display()
            ),
            format!("install Claude Code plugin '{plugin_ref}'"),
        ];
        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions,
            register_command: Some(display_command(&install_cmd)),
        })
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let plugin = plugin_name(bundle, ctx);
        let marketplace = marketplace_name(&ctx.component);
        validate_plugin_id(&plugin)?;
        validate_marketplace_name(&marketplace)?;

        let resources = vec![
            ClaimResource {
                id: RES_MARKETPLACE.to_string(),
                purpose: "claude_code_marketplace".to_string(),
                kind: ClaimResourceKind::FrameworkMarketplace {
                    framework: self.name().to_string(),
                    marketplace: marketplace.clone(),
                },
            },
            ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: "claude_code_plugin".to_string(),
                kind: ClaimResourceKind::FrameworkPlugin {
                    framework: self.name().to_string(),
                    plugin_id: plugin.clone(),
                },
            },
        ];

        Ok((
            AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: ctx.component.clone(),
                framework: self.name().to_string(),
                plugin_id: Some(plugin),
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
                driver_payload: DriverPayload::ClaudeCode(ClaudeCodeClaim {
                    marketplace_resource: RES_MARKETPLACE.to_string(),
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
        let plugin = claim_plugin(claim).ok_or_else(|| AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: "claude-code receipt has no plugin resource".to_string(),
        })?;
        // Read the marketplace strictly from the receipt's validated
        // resource — never fall back to a name derived from ctx, so a
        // malformed/forged receipt missing the marketplace resource cannot
        // drive `marketplace add` / `plugin install`.
        let marketplace = claim_marketplace(claim).ok_or_else(|| AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: "claude-code receipt has no marketplace resource".to_string(),
        })?;

        // 1. Validate the bundle via the official CLI — same gate `install`
        //    hits, but surfaces a cleaner error here.
        let validate_cmd = build_validate_cmd(&claim.resource_root);
        let program = validate_cmd.program.clone();
        let output = ctx.ops.run_framework_cli(validate_cmd)?;
        if !output.success() {
            return Err(AdapterError::FrameworkCli {
                program,
                reason: cli_failure_reason("plugin validate", &output),
            });
        }

        // 2. Register the marketplace (idempotent: skip when already listed
        //    so a re-enable does not error on a duplicate source).
        if !marketplace_registered(&marketplace, ctx) {
            let add_cmd = build_marketplace_add_cmd(&claim.resource_root);
            let program = add_cmd.program.clone();
            let output = ctx.ops.run_framework_cli(add_cmd)?;
            if !output.success() {
                return Err(AdapterError::FrameworkCli {
                    program,
                    reason: cli_failure_reason("plugin marketplace add", &output),
                });
            }
        }

        // 3. Install the plugin from the marketplace.
        let install_cmd = build_plugin_install_cmd(&plugin_ref(&plugin, &marketplace));
        let program = install_cmd.program.clone();
        let output = ctx.ops.run_framework_cli(install_cmd)?;
        if !output.success() {
            return Err(AdapterError::FrameworkCli {
                program,
                reason: cli_failure_reason("plugin install", &output),
            });
        }
        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        let mut conditions = Vec::new();
        let detect = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::FrameworkDetected,
            status: bool_status(detect.detected),
            reason: Some(detect.reason.clone()),
            resource: None,
        });
        let plugin = claim_plugin(claim);
        // Read strictly from the receipt; a receipt with no marketplace
        // resource is malformed and must not be treated as healthy.
        let marketplace = claim_marketplace(claim);
        let (mkt_status, plugin_status) = if !detect.detected {
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::MarketplaceRegistered,
                status: ConditionStatus::Unknown,
                reason: Some("claude CLI unavailable; cannot verify".to_string()),
                resource: Some(ClaimResourceRef {
                    id: RES_MARKETPLACE.to_string(),
                }),
            });
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::PluginRegistered,
                status: ConditionStatus::Unknown,
                reason: Some("claude CLI unavailable; cannot verify".to_string()),
                resource: Some(ClaimResourceRef {
                    id: RES_PLUGIN.to_string(),
                }),
            });
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::VerificationSupported,
                status: ConditionStatus::False,
                reason: Some("claude CLI unavailable".to_string()),
                resource: None,
            });
            (ConditionStatus::Unknown, ConditionStatus::Unknown)
        } else if let Some(marketplace) = marketplace {
            let mkt = marketplace_registered(&marketplace, ctx);
            let plugin_ok = plugin
                .as_deref()
                .map(|p| plugin_registered(p, &marketplace, ctx))
                .unwrap_or(false);
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::MarketplaceRegistered,
                status: bool_status(mkt),
                reason: (!mkt).then(|| "marketplace not in `plugin marketplace list`".to_string()),
                resource: Some(ClaimResourceRef {
                    id: RES_MARKETPLACE.to_string(),
                }),
            });
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::PluginRegistered,
                status: bool_status(plugin_ok),
                reason: (!plugin_ok).then(|| "plugin not in `plugin list`".to_string()),
                resource: Some(ClaimResourceRef {
                    id: RES_PLUGIN.to_string(),
                }),
            });
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::VerificationSupported,
                status: ConditionStatus::True,
                reason: None,
                resource: None,
            });
            (bool_status(mkt), bool_status(plugin_ok))
        } else {
            // Receipt has no marketplace resource: malformed. Do not run the
            // CLI; report a degraded, unverifiable state.
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::MarketplaceRegistered,
                status: ConditionStatus::False,
                reason: Some("receipt has no marketplace resource".to_string()),
                resource: Some(ClaimResourceRef {
                    id: RES_MARKETPLACE.to_string(),
                }),
            });
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::PluginRegistered,
                status: ConditionStatus::Unknown,
                reason: Some("cannot verify plugin without a marketplace resource".to_string()),
                resource: Some(ClaimResourceRef {
                    id: RES_PLUGIN.to_string(),
                }),
            });
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::VerificationSupported,
                status: ConditionStatus::False,
                reason: Some("receipt has no marketplace resource".to_string()),
                resource: None,
            });
            (ConditionStatus::False, ConditionStatus::Unknown)
        };

        let summary = summarize(claim.status, detect.detected, mkt_status, plugin_status);
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
        // Deregistration is only possible through the CLI. ANOLISA must not
        // hand-edit ~/.claude/settings.json, so without the CLI we report
        // cleanup as incomplete and keep the receipt for a later retry.
        if find_binary_in_path(&claude_bin()).is_none() {
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![
                    "claude CLI not found on PATH; receipt kept (ANOLISA will not hand-edit settings.json)"
                        .to_string(),
                ],
            });
        }

        // Fail closed: act only on the marketplace the receipt recorded. A
        // malformed/forged receipt without a marketplace resource must not
        // trigger `plugin uninstall` / `marketplace remove` against a name
        // derived from ctx — keep the receipt for manual resolution instead.
        let Some(marketplace) = claim_marketplace(claim) else {
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![
                    "claude-code receipt has no marketplace resource; receipt kept (nothing safely removable)"
                        .to_string(),
                ],
            });
        };

        let mut messages = Vec::new();
        let mut cleanup_complete = true;

        if let Some(plugin) = claim_plugin(claim) {
            let plugin_ref = plugin_ref(&plugin, &marketplace);
            let cmd = build_plugin_uninstall_cmd(&plugin_ref);
            let output = ctx.ops.run_framework_cli(cmd)?;
            if output.success() {
                messages.push(format!("uninstalled claude-code plugin '{plugin_ref}'"));
            } else {
                cleanup_complete = false;
                messages.push(format!(
                    "claude plugin uninstall failed: {}",
                    cli_failure_reason("plugin uninstall", &output)
                ));
            }
        } else {
            messages.push("receipt records no plugin to uninstall".to_string());
        }

        let cmd = build_marketplace_remove_cmd(&marketplace);
        let output = ctx.ops.run_framework_cli(cmd)?;
        if output.success() {
            messages.push(format!("removed claude-code marketplace '{marketplace}'"));
        } else {
            cleanup_complete = false;
            messages.push(format!(
                "claude plugin marketplace remove failed: {}",
                cli_failure_reason("plugin marketplace remove", &output)
            ));
        }

        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// `CLAUDE_BIN` override, else `claude`.
fn claude_bin() -> String {
    std::env::var("CLAUDE_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude".to_string())
}

/// Component-scoped marketplace name: `anolisa-<component>`.
fn marketplace_name(component: &str) -> String {
    format!("{MARKETPLACE_PREFIX}-{component}")
}

/// Plugin name for the receipt: declared plugin id, else component.
fn plugin_name(bundle: &AdapterBundle, ctx: &DriverCtx) -> String {
    bundle
        .plugin_id
        .clone()
        .unwrap_or_else(|| ctx.component.clone())
}

/// `<plugin>@<marketplace>` argument for `claude plugin install/uninstall`.
/// Both halves are validated separately (no `@` inside either token), so the
/// composed ref cannot smuggle a metacharacter into the argv.
fn plugin_ref(plugin: &str, marketplace: &str) -> String {
    format!("{plugin}@{marketplace}")
}

/// Read the `name` field from the bundle's `.claude-plugin/marketplace.json`.
/// Returns `None` when the field is absent/empty.
///
/// # Errors
///
/// [`AdapterError::BundleInvalid`] when the manifest cannot be parsed;
/// [`AdapterError::Io`] on a read failure other than not-found.
fn read_marketplace_manifest_name(root: &Path) -> Result<Option<String>, AdapterError> {
    #[derive(serde::Deserialize)]
    struct MarketplaceManifest {
        name: Option<String>,
    }
    let path = root.join(MARKETPLACE_MANIFEST);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(AdapterError::Io { path, source }),
    };
    let manifest: MarketplaceManifest =
        serde_json::from_slice(&bytes).map_err(|source| AdapterError::BundleInvalid {
            root: root.to_path_buf(),
            reason: format!(
                "failed to parse {} as a Claude Code marketplace manifest: {source}",
                path.display()
            ),
        })?;
    Ok(manifest.name.filter(|n| !n.is_empty()))
}

/// Extract the plugin name from a receipt's plugin resource.
fn claim_plugin(claim: &AdapterClaim) -> Option<String> {
    claim
        .resource(RES_PLUGIN)
        .and_then(|r| match &r.kind {
            ClaimResourceKind::FrameworkPlugin { plugin_id, .. } => Some(plugin_id.clone()),
            _ => None,
        })
        .or_else(|| claim.plugin_id.clone())
}

/// Extract the marketplace name from a receipt's marketplace resource.
fn claim_marketplace(claim: &AdapterClaim) -> Option<String> {
    claim.resource(RES_MARKETPLACE).and_then(|r| match &r.kind {
        ClaimResourceKind::FrameworkMarketplace { marketplace, .. } => Some(marketplace.clone()),
        _ => None,
    })
}

fn base_cmd(args: Vec<String>) -> FrameworkCommand {
    FrameworkCommand {
        program: claude_bin(),
        args,
        stdin: None,
        env_set: Vec::new(),
        env_remove: Vec::new(),
        path_prepend: Vec::new(),
        timeout: CLI_TIMEOUT,
    }
}

fn build_validate_cmd(resource_root: &Path) -> FrameworkCommand {
    base_cmd(vec![
        "plugin".to_string(),
        "validate".to_string(),
        resource_root.to_string_lossy().into_owned(),
    ])
}

fn build_marketplace_add_cmd(resource_root: &Path) -> FrameworkCommand {
    base_cmd(vec![
        "plugin".to_string(),
        "marketplace".to_string(),
        "add".to_string(),
        resource_root.to_string_lossy().into_owned(),
    ])
}

fn build_marketplace_remove_cmd(marketplace: &str) -> FrameworkCommand {
    base_cmd(vec![
        "plugin".to_string(),
        "marketplace".to_string(),
        "remove".to_string(),
        marketplace.to_string(),
    ])
}

fn build_marketplace_list_cmd() -> FrameworkCommand {
    base_cmd(vec![
        "plugin".to_string(),
        "marketplace".to_string(),
        "list".to_string(),
    ])
}

fn build_plugin_install_cmd(plugin_ref: &str) -> FrameworkCommand {
    base_cmd(vec![
        "plugin".to_string(),
        "install".to_string(),
        plugin_ref.to_string(),
    ])
}

fn build_plugin_uninstall_cmd(plugin_ref: &str) -> FrameworkCommand {
    base_cmd(vec![
        "plugin".to_string(),
        "uninstall".to_string(),
        plugin_ref.to_string(),
    ])
}

fn build_plugin_list_cmd() -> FrameworkCommand {
    base_cmd(vec!["plugin".to_string(), "list".to_string()])
}

/// True when `claude plugin marketplace list` reports `marketplace`.
fn marketplace_registered(marketplace: &str, ctx: &DriverCtx) -> bool {
    match ctx.ops.run_framework_cli(build_marketplace_list_cmd()) {
        Ok(output) if output.success() => list_contains_token(&output.stdout, marketplace),
        _ => false,
    }
}

/// True when `claude plugin list` reports the plugin (bare name or
/// `plugin@<marketplace>` ref).
fn plugin_registered(plugin: &str, marketplace: &str, ctx: &DriverCtx) -> bool {
    match ctx.ops.run_framework_cli(build_plugin_list_cmd()) {
        Ok(output) if output.success() => {
            list_contains_token(&output.stdout, plugin)
                || list_contains_token(&output.stdout, &plugin_ref(plugin, marketplace))
        }
        _ => false,
    }
}

/// True when `token` appears as a whole whitespace-delimited word on any
/// line of `stdout`.
fn list_contains_token(stdout: &str, token: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.split_whitespace().any(|t| t == token))
}

/// Roll signals into a summary. Healthy requires the framework detected and
/// both marketplace and plugin verified present.
fn summarize(
    claim_status: ClaimStatus,
    detected: bool,
    marketplace: ConditionStatus,
    plugin: ConditionStatus,
) -> AdapterSummary {
    if claim_status == ClaimStatus::CleanupFailed {
        return AdapterSummary::CleanupFailed;
    }
    if !detected {
        return AdapterSummary::Degraded;
    }
    match (marketplace, plugin) {
        (ConditionStatus::True, ConditionStatus::True) => AdapterSummary::Healthy,
        (ConditionStatus::False, _) | (_, ConditionStatus::False) => AdapterSummary::Degraded,
        _ => AdapterSummary::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_name_and_plugin_ref_are_component_scoped() {
        assert_eq!(marketplace_name("tokenless"), "anolisa-tokenless");
        assert_eq!(
            plugin_ref("tokenless", "anolisa-tokenless"),
            "tokenless@anolisa-tokenless"
        );
    }

    #[test]
    fn cmd_shapes() {
        assert_eq!(
            build_validate_cmd(Path::new("/data/cc")).args,
            vec!["plugin", "validate", "/data/cc"]
        );
        assert_eq!(
            build_marketplace_add_cmd(Path::new("/data/cc")).args,
            vec!["plugin", "marketplace", "add", "/data/cc"]
        );
        assert_eq!(
            build_plugin_install_cmd("tokenless@anolisa-tokenless").args,
            vec!["plugin", "install", "tokenless@anolisa-tokenless"]
        );
        assert_eq!(
            build_plugin_uninstall_cmd("tokenless@anolisa-tokenless").args,
            vec!["plugin", "uninstall", "tokenless@anolisa-tokenless"]
        );
        assert_eq!(
            build_marketplace_remove_cmd("anolisa-tokenless").args,
            vec!["plugin", "marketplace", "remove", "anolisa-tokenless"]
        );
    }

    #[test]
    fn read_bundle_requires_both_manifests() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("claude-code");
        std::fs::create_dir_all(root.join(".claude-plugin")).expect("mkdir");
        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/cc-home"));

        struct StubOps;
        impl super::super::driver::AdapterOps for StubOps {
            fn run_framework_cli(
                &self,
                _: FrameworkCommand,
            ) -> Result<super::super::driver::CliOutput, AdapterError> {
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
        let ops = StubOps;
        let mk_ctx = |root: &Path| DriverCtx {
            component: "tokenless".to_string(),
            framework: "claude-code".to_string(),
            layout: &layout,
            resource_root: root.to_path_buf(),
            user_home: Some(PathBuf::from("/tmp/cc-home")),
            declared_plugin_id: Some("tokenless".to_string()),
            requested_profiles: Vec::new(),
            adapter_type: Some("plugin".to_string()),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: true,
            ops: &ops,
        };
        let driver = ClaudeCodeDriver::new();

        // marketplace.json only -> still missing plugin.json.
        std::fs::write(
            root.join(MARKETPLACE_MANIFEST),
            br#"{"name":"anolisa-tokenless"}"#,
        )
        .expect("write");
        let err = driver
            .read_bundle(&mk_ctx(&root))
            .expect_err("plugin.json missing must fail");
        assert!(matches!(err, AdapterError::BundleInvalid { .. }));

        // Both present, component-scoped marketplace name -> ok.
        std::fs::write(root.join(PLUGIN_MANIFEST), b"{}").expect("write");
        let bundle = driver.read_bundle(&mk_ctx(&root)).expect("both present");
        assert_eq!(bundle.plugin_id.as_deref(), Some("tokenless"));
    }

    #[test]
    fn read_bundle_rejects_shared_marketplace_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("claude-code");
        std::fs::create_dir_all(root.join(".claude-plugin")).expect("mkdir");
        // Bare shared "anolisa" name (the pre-scoping value) must be rejected.
        std::fs::write(root.join(MARKETPLACE_MANIFEST), br#"{"name":"anolisa"}"#).expect("write");
        std::fs::write(root.join(PLUGIN_MANIFEST), b"{}").expect("write");
        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/cc-home2"));

        struct StubOps;
        impl super::super::driver::AdapterOps for StubOps {
            fn run_framework_cli(
                &self,
                _: FrameworkCommand,
            ) -> Result<super::super::driver::CliOutput, AdapterError> {
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
        let ops = StubOps;
        let ctx = DriverCtx {
            component: "tokenless".to_string(),
            framework: "claude-code".to_string(),
            layout: &layout,
            resource_root: root.clone(),
            user_home: Some(PathBuf::from("/tmp/cc-home2")),
            declared_plugin_id: Some("tokenless".to_string()),
            requested_profiles: Vec::new(),
            adapter_type: Some("plugin".to_string()),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: true,
            ops: &ops,
        };
        let err = ClaudeCodeDriver::new()
            .read_bundle(&ctx)
            .expect_err("bare 'anolisa' marketplace name must be rejected");
        match err {
            AdapterError::BundleInvalid { reason, .. } => {
                assert!(reason.contains("anolisa-tokenless"), "reason: {reason}");
            }
            other => panic!("expected BundleInvalid, got {other:?}"),
        }
    }
}
