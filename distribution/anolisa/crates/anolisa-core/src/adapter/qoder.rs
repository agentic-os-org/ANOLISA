//! Qoder (`qodercli`) framework driver.
//!
//! The driver supports two unambiguous bundle layouts. Native bundles carry
//! `.qoder-plugin/plugin.json` plus `hooks/hooks.json`; their complete
//! lifecycle is delegated to `qodercli plugins`, and registration,
//! activation, and loaded hooks are verified from `plugins list --json`.
//! ANOLISA installs the original resource root without staging or rewriting
//! plugin files and never edits `~/.qoder/settings.json` for this mode.
//!
//! Legacy bundles carry the same manifest plus a root-level `hooks.json`.
//! That branch preserves the original Tokenless integration exactly: it
//! stages a plugin-id-named copy, expands `${QODER_TOKENLESS_HOOKS}`, installs
//! the copy, and atomically merges only its owned hooks and activation entry
//! into `settings.json`. Legacy status remains settings-based because older
//! qodercli inventories could omit freshly installed plugins, and legacy
//! disable prunes only the exact entries persisted in its receipt.
//!
//! Env contract: `QODERCLI_BIN` overrides the executable (tests point it at
//! a fake CLI); otherwise the binary is resolved in the legacy order
//! (highest-versioned `~/.qoder/bin/qodercli/qodercli-*`, then the
//! unversioned binary there, then `qodercli` on `PATH`). `XDG_DATA_HOME`
//! relocates the plugin staging base.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    DRIVER_SCHEMA_VERSION, DriverPayload, QoderClaim, QoderManagedHook, validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx, DriverPlan,
    FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable, find_binary_in_path,
};
use super::util::{bool_status, cli_failure_reason, display_command, now_iso8601};

mod settings;

use settings::{
    SettingsProbe, collect_expected_hook_names, collect_managed_hook_specs,
    load_settings_for_merge, merge_managed, probe_settings, prune_settings_via_ops,
    render_resolved_hooks_text,
};

/// Default timeout for a `qodercli` invocation.
const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Qoder plugin manifest shared by native and legacy layouts.
const QODER_PLUGIN_MANIFEST: &str = ".qoder-plugin/plugin.json";

/// Legacy hook declarations merged into the user's `settings.json`.
const QODER_HOOKS_FILE: &str = "hooks.json";

/// Qoder-native hook declarations auto-discovered by `qodercli`.
const QODER_NATIVE_HOOKS_FILE: &str = "hooks/hooks.json";

/// Native lifecycle mutations are always owned in Qoder's user scope.
const QODER_NATIVE_SCOPE: &str = "user";

/// Placeholder in `hooks.json` for the absolute hook-scripts directory,
/// expanded to `<resource_root>/../common/hooks` before the entries are
/// written into `settings.json` (matching the legacy install script).
const HOOKS_PLACEHOLDER: &str = "${QODER_TOKENLESS_HOOKS}";

/// Resource ids used in Qoder receipts.
const RES_PLUGIN: &str = "qoder_plugin";
const RES_SETTINGS: &str = "qoder_settings";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QoderBundleKind {
    Native,
    Legacy,
}

/// Qoder driver. Stateless; all per-operation context arrives via
/// [`DriverCtx`].
pub struct QoderDriver;

impl QoderDriver {
    /// Construct the driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for QoderDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for QoderDriver {
    fn name(&self) -> &'static str {
        "qoder"
    }

    fn probe_bundle(&self, resource_root: &Path, declared_entry: Option<&str>) -> bool {
        if declared_entry.is_some_and(|entry| entry != QODER_PLUGIN_MANIFEST) {
            return false;
        }
        resource_root.join(QODER_PLUGIN_MANIFEST).is_file()
            && classify_bundle(resource_root).is_ok()
    }

    fn detect(&self, env: &HostEnv) -> DetectResult {
        match resolve_qodercli(env.user_home.as_deref()) {
            Some(path) => DetectResult {
                detected: true,
                reason: format!("qodercli found at {}", path.display()),
            },
            None => DetectResult {
                detected: false,
                reason: "qodercli not found (checked $QODERCLI_BIN, ~/.qoder/bin/qodercli, PATH)"
                    .to_string(),
            },
        }
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        // Two external roots: the user's `~/.qoder` (where settings.json
        // lives) and ANOLISA's own plugin-staging namespace under the data
        // home (where the install-time symlink is created). Neither is
        // derived from receipt contents. `~/.ssh`, `/etc`, etc. fall outside
        // both, so a forged receipt cannot redirect a write there.
        let mut roots = Vec::new();
        if let Some(home) = ctx.user_home.as_deref() {
            roots.push(qoder_home(home));
        }
        if let Some(staging) = plugin_staging_root(ctx.user_home.as_deref()) {
            roots.push(staging);
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
        if let Some(entry) = ctx.declared_bundle_entry.as_deref()
            && entry != QODER_PLUGIN_MANIFEST
        {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!(
                    "qoder bundle entry must be the native manifest '{QODER_PLUGIN_MANIFEST}', got '{entry}'"
                ),
            });
        }
        let manifest = QODER_PLUGIN_MANIFEST;
        if !root.join(manifest).is_file() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!(
                    "qoder plugin manifest '{manifest}' missing (run: make stamp-adapter-templates)"
                ),
            });
        }
        let kind = classify_bundle(root).map_err(|reason| AdapterError::BundleInvalid {
            root: root.clone(),
            reason,
        })?;
        let plugin_id = match kind {
            QoderBundleKind::Native => read_native_bundle(ctx, manifest)?,
            QoderBundleKind::Legacy => ctx
                .declared_plugin_id
                .clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| ctx.component.clone()),
        };
        // Validate the resolved plugin id (including the component-name
        // default) before it can reach an argv or a staging directory name.
        validate_plugin_id(&plugin_id)?;
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
        let plugin = plugin_name(bundle, ctx);
        let program =
            qodercli_program(ctx.user_home.as_deref()).unwrap_or_else(|| "qodercli".to_string());
        let kind = classify_bundle(&bundle.resource_root).map_err(|reason| {
            AdapterError::BundleInvalid {
                root: bundle.resource_root.clone(),
                reason,
            }
        })?;
        if kind == QoderBundleKind::Native {
            let install_cmd = build_native_install_cmd(&program, &bundle.resource_root);
            return Ok(DriverPlan {
                framework: self.name().to_string(),
                component: ctx.component.clone(),
                actions: vec![
                    "validate the native Qoder plugin bundle and CLI capabilities".to_string(),
                    format!("install and enable native Qoder plugin '{plugin}'"),
                    "verify registration, activation, and loaded hooks via JSON inventory"
                        .to_string(),
                ],
                register_command: Some(display_command(&install_cmd)),
            });
        }
        let staging = staging_symlink(ctx.user_home.as_deref(), &plugin);
        let staging_display = staging
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("<staging>/{plugin}"));
        let settings_display = settings_path(ctx.user_home.as_deref())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.qoder/settings.json".to_string());
        let install_cmd = build_install_cmd(
            &program,
            staging.as_deref().unwrap_or_else(|| Path::new("<staging>")),
        );
        let actions = vec![
            format!(
                "stage qoder plugin copy {staging_display} from {} (hooks.json placeholder expanded)",
                bundle.resource_root.display()
            ),
            format!("register qoder plugin '{plugin}' via `qodercli plugins install`"),
            format!("merge tokenless hooks into {settings_display}"),
            format!(
                "enable plugin '{}' in qoder settings",
                plugin_entry(&plugin)
            ),
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
        validate_plugin_id(&plugin)?;
        let kind = classify_bundle(&bundle.resource_root).map_err(|reason| {
            AdapterError::BundleInvalid {
                root: bundle.resource_root.clone(),
                reason,
            }
        })?;
        if kind == QoderBundleKind::Native {
            let program = require_native_qodercli(ctx)?;
            let validate_capability = ctx
                .ops
                .run_framework_cli(build_native_help_cmd(&program, "validate"))?;
            for subcommand in ["install", "list", "uninstall"] {
                require_cli_success(
                    &program,
                    &format!("plugins {subcommand} --help"),
                    ctx.ops
                        .run_framework_cli(build_native_help_cmd(&program, subcommand))?,
                )?;
            }
            if validate_capability.success() {
                require_cli_success(
                    &program,
                    "plugins validate",
                    ctx.ops.run_framework_cli(build_native_validate_cmd(
                        &program,
                        &bundle.resource_root,
                    ))?,
                )?;
            }
            let inventory = ctx
                .ops
                .run_framework_cli_json(build_native_list_cmd(&program))?;
            require_cli_success(&program, "plugins list --json", inventory.clone())?;
            let expected = plugin_entry(&plugin);
            let matching =
                parse_native_plugin_matches(&inventory.stdout, &expected).map_err(|reason| {
                    AdapterError::FrameworkCli {
                        program: program.clone(),
                        reason: format!(
                            "Qoder native plugin inventory is unavailable ({reason}); upgrade Qoder"
                        ),
                    }
                })?;
            if !matching.non_user_scopes.is_empty() {
                return Err(AdapterError::FrameworkCli {
                    program,
                    reason: format!(
                        "refusing to install {expected} while matching non-user Qoder registration(s) exist in scope(s) {}; Qoder shares the local plugin cache across scopes",
                        matching.non_user_scopes.join(", ")
                    ),
                });
            }
            let plugin_preexisting = matching.user.is_some();

            let claim = AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: ctx.component.clone(),
                framework: self.name().to_string(),
                plugin_id: Some(plugin.clone()),
                adapter_type: ctx.adapter_type.clone(),
                enabled_at: now_iso8601(),
                resource_root: bundle.resource_root.clone(),
                bundle_digest: None,
                source_revision: None,
                materialized_files: Vec::new(),
                driver_schema: DRIVER_SCHEMA_VERSION,
                status: ClaimStatus::Enabled,
                notices: Vec::new(),
                resources: vec![ClaimResource {
                    id: RES_PLUGIN.to_string(),
                    purpose: "qoder_plugin".to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: self.name().to_string(),
                        plugin_id: plugin,
                    },
                }],
                driver_payload: DriverPayload::Qoder(QoderClaim {
                    plugin_resource: RES_PLUGIN.to_string(),
                    settings_resource: None,
                    plugin_preexisting,
                    plugin_install_confirmed: false,
                    managed_hooks: Vec::new(),
                    managed_hook_specs: Vec::new(),
                }),
            };
            return Ok((claim, PreparedEnable::QoderNative { program }));
        }

        let settings =
            settings_path(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
                program: "qodercli".to_string(),
                reason: "cannot resolve ~/.qoder/settings.json (no home directory)".to_string(),
            })?;
        // Persist the exact hook entries we will merge so status/disable do
        // not depend on the resource root still existing later.
        let managed_hooks = collect_expected_hook_names(&bundle.resource_root)?;
        let managed_hook_specs = collect_managed_hook_specs(&bundle.resource_root)?;

        let resources = vec![
            ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: "qoder_plugin".to_string(),
                kind: ClaimResourceKind::FrameworkPlugin {
                    framework: self.name().to_string(),
                    plugin_id: plugin.clone(),
                },
            },
            ClaimResource {
                id: RES_SETTINGS.to_string(),
                purpose: "qoder_settings".to_string(),
                kind: ClaimResourceKind::ExternalPath { path: settings },
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
                driver_payload: DriverPayload::Qoder(QoderClaim {
                    plugin_resource: RES_PLUGIN.to_string(),
                    settings_resource: Some(RES_SETTINGS.to_string()),
                    plugin_preexisting: false,
                    plugin_install_confirmed: false,
                    managed_hooks,
                    managed_hook_specs,
                }),
            },
            PreparedEnable::None,
        ))
    }

    fn preserve_reenable_facts(
        &self,
        prior: &AdapterClaim,
        next: &mut AdapterClaim,
    ) -> Result<(), AdapterError> {
        let prior_native = native_claim(prior)?;
        let next_native = native_claim(next)?;
        if prior_native != next_native {
            let prior_kind = if prior_native { "native" } else { "legacy" };
            let next_kind = if next_native { "native" } else { "legacy" };
            return Err(AdapterError::BundleInvalid {
                root: next.resource_root.clone(),
                reason: format!(
                    "qoder bundle lifecycle changed from {prior_kind} to {next_kind}; disable the existing adapter before re-enabling"
                ),
            });
        }
        if next_native {
            let prior_plugin =
                resolve_plugin(prior).ok_or_else(|| AdapterError::BundleInvalid {
                    root: prior.resource_root.clone(),
                    reason: "existing qoder receipt has no plugin resource".to_string(),
                })?;
            let next_plugin = resolve_plugin(next).ok_or_else(|| AdapterError::BundleInvalid {
                root: next.resource_root.clone(),
                reason: "new qoder receipt has no plugin resource".to_string(),
            })?;
            if prior_plugin != next_plugin {
                return Err(AdapterError::BundleInvalid {
                    root: next.resource_root.clone(),
                    reason: format!(
                        "qoder native plugin id changed from '{prior_plugin}' to '{next_plugin}'; disable the existing adapter before re-enabling"
                    ),
                });
            }
            let prior_payload =
                qoder_payload(prior).ok_or_else(|| AdapterError::BundleInvalid {
                    root: prior.resource_root.clone(),
                    reason: "existing qoder receipt has a non-qoder driver payload".to_string(),
                })?;
            let prior_owned =
                !prior_payload.plugin_preexisting && native_install_confirmed(prior, prior_payload);
            let next_root = next.resource_root.clone();
            let next_payload =
                qoder_payload_mut(next).ok_or_else(|| AdapterError::BundleInvalid {
                    root: next_root,
                    reason: "new qoder receipt has a non-qoder driver payload".to_string(),
                })?;
            // Ownership can cross a same-ID re-enable only while the
            // registration is still present and a prior install checkpoint
            // proved ANOLISA created it. An absent registration starts a new
            // unconfirmed write-ahead lifecycle.
            if next_payload.plugin_preexisting && prior_owned {
                next_payload.plugin_preexisting = false;
                next_payload.plugin_install_confirmed = true;
            }
        }
        Ok(())
    }

    fn validate_prepared_enable(&self, claim: &AdapterClaim) -> Result<(), AdapterError> {
        if native_claim(claim)?
            && qoder_payload(claim).is_some_and(|payload| payload.plugin_preexisting)
        {
            let plugin = resolve_plugin(claim).ok_or_else(|| AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: "qoder receipt has no plugin resource".to_string(),
            })?;
            return Err(AdapterError::FrameworkCli {
                program: "qodercli".to_string(),
                reason: format!(
                    "refusing to replace pre-existing user-scope Qoder plugin '{}'; remove it explicitly before enabling",
                    plugin_entry(&plugin)
                ),
            });
        }
        Ok(())
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        prepared: &PreparedEnable,
        ctx: &DriverCtx,
        progress: &mut dyn super::driver::EnableProgress,
    ) -> Result<(), AdapterError> {
        if native_claim(claim)? {
            let PreparedEnable::QoderNative { program } = prepared else {
                return Err(AdapterError::BundleInvalid {
                    root: claim.resource_root.clone(),
                    reason: "native Qoder enable requires prepared CLI capabilities".to_string(),
                });
            };
            let plugin = resolve_plugin(claim).ok_or_else(|| AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: "qoder receipt has no plugin resource".to_string(),
            })?;
            let output = ctx
                .ops
                .run_framework_cli(build_native_install_cmd(program, &claim.resource_root))?;
            require_cli_success(program, "plugins install", output)?;
            let root = claim.resource_root.clone();
            let payload = qoder_payload_mut(claim).ok_or_else(|| AdapterError::BundleInvalid {
                root,
                reason: "qoder receipt has a non-qoder driver payload".to_string(),
            })?;
            payload.plugin_install_confirmed = true;
            progress.persist_claim(claim)?;
            return verify_native_plugin(ctx, program, &plugin).map_err(|reason| {
                AdapterError::FrameworkCli {
                    program: program.clone(),
                    reason,
                }
            });
        }
        if !matches!(prepared, PreparedEnable::None) {
            return Err(AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: "legacy Qoder enable received native CLI capabilities".to_string(),
            });
        }
        // Resolve plugin + settings strictly from the receipt's payload
        // references (Manager-validated), failing closed on a malformed
        // receipt rather than falling back to ctx-derived defaults.
        let plugin = resolve_plugin(claim).ok_or_else(|| AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: "qoder receipt has no plugin resource".to_string(),
        })?;
        let settings = resolve_settings(claim, ctx.user_home.as_deref()).ok_or_else(|| {
            AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: "qoder receipt settings resource is missing or not ~/.qoder/settings.json"
                    .to_string(),
            }
        })?;
        let managed_hooks =
            managed_hook_specs(claim).ok_or_else(|| AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: "qoder receipt has no managed hook specs".to_string(),
            })?;
        let existing = ctx.ops.read_file(&settings)?;
        let mut root = load_settings_for_merge(existing, &settings)?;
        merge_managed(&mut root, managed_hooks, &plugin_entry(&plugin)).map_err(|reason| {
            AdapterError::SettingsUnparseable {
                path: settings.clone(),
                reason,
            }
        })?;
        let program = qodercli_program(ctx.user_home.as_deref()).ok_or_else(|| {
            AdapterError::FrameworkCli {
                program: "qodercli".to_string(),
                reason: "qodercli not found on PATH or under ~/.qoder/bin".to_string(),
            }
        })?;
        let staging = staging_symlink(ctx.user_home.as_deref(), &plugin).ok_or_else(|| {
            AdapterError::FrameworkCli {
                program: program.clone(),
                reason: "cannot resolve qoder plugin staging dir (no home / XDG_DATA_HOME)"
                    .to_string(),
            }
        })?;

        // 1. Stage a real copy of the bundle named after the plugin id
        //    (qodercli derives the id from the dir name), patching its
        //    hooks.json so the verbatim copy qodercli drops into its
        //    plugin cache is loadable by consumers that never expand the
        //    placeholder. Staging is install-time only — remove it
        //    whether install succeeds or not.
        if let Err(err) = stage_plugin_copy(ctx, &claim.resource_root, &staging) {
            let _ = ctx.ops.remove_tree(&staging);
            return Err(err);
        }
        let install_cmd = build_install_cmd(&program, &staging);
        let cli_program = install_cmd.program.clone();
        let install = ctx.ops.run_framework_cli(install_cmd);
        let _ = ctx.ops.remove_tree(&staging);
        let output = install?;
        if !output.success() {
            return Err(AdapterError::FrameworkCli {
                program: cli_program,
                reason: cli_failure_reason("plugins install", &output),
            });
        }

        // 2. Write the already-validated merged settings.
        let bytes = serde_json::to_vec_pretty(&Value::Object(root)).map_err(|source| {
            AdapterError::SettingsUnparseable {
                path: settings.clone(),
                reason: format!("failed to render merged settings JSON: {source}"),
            }
        })?;
        ctx.ops.write_file(&settings, &bytes)?;
        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        if native_claim(claim)? {
            return native_status(claim, ctx);
        }
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
        // Resolve strictly from the receipt payload; a receipt missing its
        // plugin or settings resource is malformed and must not be treated as
        // healthy or verifiable.
        let (Some(plugin), Some(settings)) = (
            resolve_plugin(claim),
            resolve_settings(claim, ctx.user_home.as_deref()),
        ) else {
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::JsonKeysPresent,
                status: ConditionStatus::False,
                reason: Some("receipt missing plugin or settings resource".to_string()),
                resource: Some(ClaimResourceRef {
                    id: RES_SETTINGS.to_string(),
                }),
            });
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::PluginRegistered,
                status: ConditionStatus::Unknown,
                reason: Some("receipt missing plugin resource".to_string()),
                resource: Some(ClaimResourceRef {
                    id: RES_PLUGIN.to_string(),
                }),
            });
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::VerificationSupported,
                status: ConditionStatus::False,
                reason: Some("receipt missing required resources".to_string()),
                resource: None,
            });
            return Ok(AdapterStatusReport {
                summary: summarize(claim.status, detect.detected, ConditionStatus::False),
                conditions,
            });
        };
        let managed_hooks = managed_hook_specs(claim).unwrap_or(&[]);
        let probe = probe_settings(ctx, &settings, managed_hooks, &plugin_entry(&plugin));
        let (settings_status, settings_reason) = match probe {
            SettingsProbe::Present {
                hooks_present: true,
                plugin_enabled: true,
            } => (ConditionStatus::True, None),
            SettingsProbe::Present {
                hooks_present,
                plugin_enabled,
            } => {
                let mut missing: Vec<String> = Vec::new();
                if !hooks_present {
                    if managed_hooks.is_empty() {
                        missing.push("managed hook spec".to_string());
                    } else {
                        missing.push(format!("managed hooks for '{plugin}'"));
                    }
                }
                if !plugin_enabled {
                    missing.push(format!("'{}'", plugin_entry(&plugin)));
                }
                (
                    ConditionStatus::False,
                    Some(format!("settings.json missing {}", missing.join(" and "))),
                )
            }
            SettingsProbe::Absent => (
                ConditionStatus::False,
                Some("~/.qoder/settings.json absent".to_string()),
            ),
            SettingsProbe::Unverifiable => (
                ConditionStatus::Unknown,
                Some("~/.qoder/settings.json unreadable or unparseable".to_string()),
            ),
        };
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::JsonKeysPresent,
            status: settings_status,
            reason: settings_reason,
            resource: Some(ClaimResourceRef {
                id: RES_SETTINGS.to_string(),
            }),
        });

        // `qodercli plugins list` omits freshly installed plugins, so never
        // report registration as verified — leave it Unknown rather than
        // faking Healthy off an unreliable probe.
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::PluginRegistered,
            status: ConditionStatus::Unknown,
            reason: Some(
                "qodercli plugins list is unreliable; verified via settings.json instead"
                    .to_string(),
            ),
            resource: Some(ClaimResourceRef {
                id: RES_PLUGIN.to_string(),
            }),
        });
        // Settings-based verification does not need the CLI, so it is always
        // supported even when qodercli is absent.
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::VerificationSupported,
            status: ConditionStatus::True,
            reason: None,
            resource: None,
        });

        let summary = summarize(claim.status, detect.detected, settings_status);
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
        if native_claim(claim)? {
            return disable_native(claim, ctx);
        }
        // Framework-side deregistration needs the CLI. Without it, the plugin
        // would stay in qodercli's cache, so keep the receipt for a retry
        // rather than pruning settings and pretending cleanup finished.
        let Some(cli) = resolve_qodercli(ctx.user_home.as_deref()) else {
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![
                    "qodercli not found on PATH or under ~/.qoder/bin; receipt kept for retry"
                        .to_string(),
                ],
            });
        };
        let program = cli.to_string_lossy().into_owned();

        // Fail closed: act only on resources the receipt actually declares.
        // A malformed/forged receipt missing the plugin or settings resource
        // must not drive `plugins uninstall` or a settings write against a
        // ctx-derived default — keep the receipt for manual resolution.
        let Some(plugin) = resolve_plugin(claim) else {
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![
                    "qoder receipt has no plugin resource; receipt kept (nothing safely removable)"
                        .to_string(),
                ],
            });
        };
        let Some(settings) = resolve_settings(claim, ctx.user_home.as_deref()) else {
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![
                    "qoder receipt settings resource is missing or not ~/.qoder/settings.json; \
                     receipt kept (nothing safely removable)"
                        .to_string(),
                ],
            });
        };

        let mut messages = Vec::new();

        // 1. Unregister the plugin. An already-removed plugin exits non-zero,
        //    so treat a CLI failure as clean only when the plugin cache is
        //    confirmed gone; otherwise cleanup is incomplete.
        let out = ctx
            .ops
            .run_framework_cli(build_uninstall_cmd(&program, &plugin))?;
        let plugin_ok = if out.success() {
            messages.push(format!("uninstalled qoder plugin '{plugin}'"));
            true
        } else if !plugin_cache_present(ctx.user_home.as_deref(), &plugin) {
            messages.push(format!("qoder plugin '{plugin}' already absent"));
            true
        } else {
            messages.push(format!(
                "qodercli plugins uninstall failed and plugin still cached: {}",
                cli_failure_reason("plugins uninstall", &out)
            ));
            false
        };

        // 2. Prune only ANOLISA-managed entries from settings.json.
        let settings_ok = prune_settings_via_ops(
            ctx,
            &settings,
            &plugin,
            managed_hook_specs(claim).unwrap_or(&[]),
            &mut messages,
        );

        Ok(DisableReport {
            cleanup_complete: plugin_ok && settings_ok,
            messages,
        })
    }
}

// ---------------------------------------------------------------------------
// Pure path / identifier helpers
// ---------------------------------------------------------------------------

fn classify_bundle(resource_root: &Path) -> Result<QoderBundleKind, String> {
    let legacy = resource_root.join(QODER_HOOKS_FILE).is_file();
    let native = resource_root.join(QODER_NATIVE_HOOKS_FILE).is_file();
    match (native, legacy) {
        (true, false) => Ok(QoderBundleKind::Native),
        (false, true) => Ok(QoderBundleKind::Legacy),
        (true, true) => Err(format!(
            "qoder bundle is ambiguous: both '{QODER_NATIVE_HOOKS_FILE}' and '{QODER_HOOKS_FILE}' exist"
        )),
        (false, false) => Err(format!(
            "qoder bundle has neither '{QODER_NATIVE_HOOKS_FILE}' nor '{QODER_HOOKS_FILE}'"
        )),
    }
}

fn read_native_bundle(ctx: &DriverCtx, manifest: &str) -> Result<String, AdapterError> {
    let root = &ctx.resource_root;
    let manifest_path = root.join(manifest);
    let bytes = ctx
        .ops
        .read_file(&manifest_path)?
        .ok_or_else(|| AdapterError::BundleInvalid {
            root: root.clone(),
            reason: format!("qoder plugin manifest '{manifest}' is missing"),
        })?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|source| AdapterError::BundleInvalid {
            root: root.clone(),
            reason: format!("failed to parse qoder plugin manifest '{manifest}': {source}"),
        })?;
    let manifest_name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AdapterError::BundleInvalid {
            root: root.clone(),
            reason: format!("qoder plugin manifest '{manifest}' has no non-empty 'name'"),
        })?;
    if let Some(declared) = ctx
        .declared_plugin_id
        .as_deref()
        .filter(|declared| !declared.is_empty())
        && declared != manifest_name
    {
        return Err(AdapterError::BundleInvalid {
            root: root.clone(),
            reason: format!(
                "declared qoder plugin id '{declared}' does not match manifest name '{manifest_name}'"
            ),
        });
    }

    let hooks_path = root.join(QODER_NATIVE_HOOKS_FILE);
    let hooks_bytes =
        ctx.ops
            .read_file(&hooks_path)?
            .ok_or_else(|| AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!("native qoder hooks '{QODER_NATIVE_HOOKS_FILE}' are missing"),
            })?;
    let hooks: Value =
        serde_json::from_slice(&hooks_bytes).map_err(|source| AdapterError::BundleInvalid {
            root: root.clone(),
            reason: format!("failed to parse {QODER_NATIVE_HOOKS_FILE}: {source}"),
        })?;
    if !hooks.get("hooks").is_some_and(Value::is_object) {
        return Err(AdapterError::BundleInvalid {
            root: root.clone(),
            reason: format!("{QODER_NATIVE_HOOKS_FILE} lacks the top-level 'hooks' object"),
        });
    }
    Ok(manifest_name.to_string())
}

/// Plugin name for the receipt: the bundle's resolved id, else component.
fn plugin_name(bundle: &AdapterBundle, ctx: &DriverCtx) -> String {
    bundle
        .plugin_id
        .clone()
        .unwrap_or_else(|| ctx.component.clone())
}

/// Managed plugin entry in `plugins.enabled` (`<plugin>@local`).
fn plugin_entry(plugin: &str) -> String {
    format!("{plugin}@local")
}

/// `<user_home>/.qoder`.
fn qoder_home(user_home: &Path) -> PathBuf {
    user_home.join(".qoder")
}

/// `<user_home>/.qoder/settings.json`, when a home directory is known.
fn settings_path(user_home: Option<&Path>) -> Option<PathBuf> {
    user_home.map(|h| qoder_home(h).join("settings.json"))
}

/// ANOLISA data-home base: `${XDG_DATA_HOME:-<home>/.local/share}/anolisa`.
/// Mirrors the Codex driver so both stage under the same namespace.
fn anolisa_data_base(user_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let s = xdg.to_string_lossy();
        let trimmed = s.trim_end_matches('/');
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("anolisa"));
        }
    }
    user_home.map(|h| h.join(".local").join("share").join("anolisa"))
}

/// Plugin staging root: `<data base>/qoder-plugins`.
fn plugin_staging_root(user_home: Option<&Path>) -> Option<PathBuf> {
    anolisa_data_base(user_home).map(|base| base.join("qoder-plugins"))
}

/// Install-time staging directory: `<staging root>/<plugin>`.
fn staging_symlink(user_home: Option<&Path>, plugin: &str) -> Option<PathBuf> {
    plugin_staging_root(user_home).map(|root| root.join(plugin))
}

/// Stage a real copy of the bundle at `staging`, patching the copied
/// `hooks.json` so the placeholder expands to the absolute sibling
/// `common/hooks` dir — the same resolution the `settings.json` merge
/// applies. A symlink to the raw bundle would let qodercli cache the
/// unexpanded placeholder, which the Qoder IDE (sharing `~/.qoder` with
/// qodercli) then loads verbatim, breaking every matching tool call.
/// Any pre-existing staging tree from an interrupted run is removed
/// first so the staged content always matches the current bundle.
fn stage_plugin_copy(
    ctx: &DriverCtx,
    resource_root: &Path,
    staging: &Path,
) -> Result<(), AdapterError> {
    ctx.ops.remove_tree(staging)?;
    ctx.ops.copy_tree(resource_root, staging)?;
    let resolved = render_resolved_hooks_text(resource_root)?;
    ctx.ops
        .write_file(&staging.join(QODER_HOOKS_FILE), resolved.as_bytes())
}

/// Absolute hook-scripts directory the [`HOOKS_PLACEHOLDER`] resolves to:
/// `<resource_root>/../common/hooks` (the sibling `common` bundle).
fn common_hooks_dir(resource_root: &Path) -> PathBuf {
    resource_root
        .parent()
        .unwrap_or(resource_root)
        .join("common")
        .join("hooks")
}

/// Whether qodercli's plugin cache holds `<plugin>` (or the target-suffixed
/// `<plugin>-qoder` variant the legacy scripts also accept).
fn plugin_cache_present(user_home: Option<&Path>, plugin: &str) -> bool {
    let Some(home) = user_home else {
        return false;
    };
    let base = qoder_home(home).join("plugins").join("cache").join("local");
    base.join(plugin).is_dir() || base.join(format!("{plugin}-qoder")).is_dir()
}

/// The Qoder-specific payload of a receipt, when it is one.
fn qoder_payload(claim: &AdapterClaim) -> Option<&QoderClaim> {
    match &claim.driver_payload {
        DriverPayload::Qoder(q) => Some(q),
        _ => None,
    }
}

fn qoder_payload_mut(claim: &mut AdapterClaim) -> Option<&mut QoderClaim> {
    match &mut claim.driver_payload {
        DriverPayload::Qoder(qoder) => Some(qoder),
        _ => None,
    }
}

/// Only the explicit schema-v3 checkpoint proves ANOLISA installed a Native
/// plugin. Older receipts were written as `Enabled` before mutation, so
/// their status cannot safely imply ownership.
fn native_install_confirmed(claim: &AdapterClaim, payload: &QoderClaim) -> bool {
    claim.driver_schema >= DRIVER_SCHEMA_VERSION && payload.plugin_install_confirmed
}

/// Native receipts omit legacy settings ownership and hook specs.
fn native_claim(claim: &AdapterClaim) -> Result<bool, AdapterError> {
    let payload = qoder_payload(claim).ok_or_else(|| AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: "qoder receipt has a non-qoder driver payload".to_string(),
    })?;
    if payload.settings_resource.is_none()
        && (!payload.managed_hooks.is_empty() || !payload.managed_hook_specs.is_empty())
    {
        return Err(AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: "qoder receipt is inconsistent: settings resource is absent while legacy managed hook facts remain"
                .to_string(),
        });
    }
    if payload.plugin_preexisting && payload.plugin_install_confirmed {
        return Err(AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: "qoder receipt is inconsistent: a pre-existing plugin cannot have an ANOLISA install confirmation"
                .to_string(),
        });
    }
    Ok(payload.settings_resource.is_none())
}

/// Resolve the plugin name strictly from the payload's `plugin_resource`
/// reference. Returns `None` (fail closed) when the payload is not Qoder's,
/// the referenced resource is missing, or it is not a `FrameworkPlugin`.
///
/// [`AdapterClaim::validate`] only checks the resources that *exist*, not
/// that payload references resolve, so a forged/malformed receipt can drop a
/// key resource yet still parse. Resolving strictly here — with no fallback
/// to `claim.plugin_id`/`ctx.component` — ensures such a receipt cannot drive
/// the CLI off an unvalidated name.
fn resolve_plugin(claim: &AdapterClaim) -> Option<String> {
    let payload = qoder_payload(claim)?;
    claim
        .resource(&payload.plugin_resource)
        .and_then(|r| match &r.kind {
            ClaimResourceKind::FrameworkPlugin { plugin_id, .. } => Some(plugin_id.clone()),
            _ => None,
        })
}

/// Resolve the settings path strictly from the payload's `settings_resource`
/// reference, requiring it to equal the canonical `~/.qoder/settings.json`
/// recomputed from `user_home`.
///
/// The Manager only validates the recorded `ExternalPath` against the
/// driver's allowed roots, and the driver's allowed root is the *whole*
/// `~/.qoder` — so root-level validation alone would let a forged receipt
/// redirect the write to another file under it (e.g.
/// `~/.qoder/other.json`). Pinning the path to exactly `settings.json`
/// closes that redirect: a mismatch returns `None` (fail closed), never the
/// recorded path. Returns `None` when the reference is missing, is not an
/// `ExternalPath`, or `user_home` is unknown.
fn resolve_settings(claim: &AdapterClaim, user_home: Option<&Path>) -> Option<PathBuf> {
    let payload = qoder_payload(claim)?;
    let resource_id = payload.settings_resource.as_deref()?;
    let recorded = claim.resource(resource_id).and_then(|r| match &r.kind {
        ClaimResourceKind::ExternalPath { path } => Some(path.clone()),
        _ => None,
    })?;
    let expected = settings_path(user_home)?;
    (recorded == expected).then_some(recorded)
}

/// Exact Qoder hook entries ANOLISA owns, persisted in the receipt payload.
fn managed_hook_specs(claim: &AdapterClaim) -> Option<&[QoderManagedHook]> {
    qoder_payload(claim).map(|q| q.managed_hook_specs.as_slice())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NativePluginInventory {
    enabled: bool,
    hooks_loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativePluginMatches {
    user: Option<NativePluginInventory>,
    non_user_scopes: Vec<String>,
}

fn parse_plugin_items(text: &str) -> Result<Vec<Value>, String> {
    let root: Value =
        serde_json::from_str(text).map_err(|source| format!("invalid JSON: {source}"))?;
    if let Some(items) = root.as_array() {
        return Ok(items.clone());
    }
    ["plugins", "installed", "items"]
        .iter()
        .find_map(|key| root.get(key).and_then(Value::as_array))
        .cloned()
        .ok_or_else(|| "plugin inventory root is not an array".to_string())
}

fn parse_native_plugin(
    text: &str,
    expected_id: &str,
) -> Result<Option<NativePluginInventory>, String> {
    Ok(parse_native_plugin_matches(text, expected_id)?.user)
}

fn parse_native_plugin_matches(
    text: &str,
    expected_id: &str,
) -> Result<NativePluginMatches, String> {
    let items = parse_plugin_items(text)?;
    let matching = items
        .iter()
        .filter(|item| {
            item.get("id").and_then(Value::as_str) == Some(expected_id)
                || item.get("pluginId").and_then(Value::as_str) == Some(expected_id)
        })
        .map(|item| {
            let scope = item
                .get("scope")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{expected_id} has no string 'scope' provenance"))?;
            Ok((scope, item))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut non_user_scopes = matching
        .iter()
        .filter(|&(scope, _)| *scope != QODER_NATIVE_SCOPE)
        .map(|(scope, _)| (*scope).to_string())
        .collect::<Vec<_>>();
    non_user_scopes.sort();
    non_user_scopes.dedup();
    let mut user_plugins = matching
        .iter()
        .filter_map(|(scope, item)| (*scope == QODER_NATIVE_SCOPE).then_some(*item));
    let Some(plugin) = user_plugins.next() else {
        return Ok(NativePluginMatches {
            user: None,
            non_user_scopes,
        });
    };
    if user_plugins.next().is_some() {
        return Err(format!(
            "{expected_id} has multiple '{QODER_NATIVE_SCOPE}'-scope inventory entries"
        ));
    }
    let enabled = plugin
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{expected_id} has no boolean 'enabled' state"))?;
    let hooks_loaded = plugin
        .get("resources")
        .and_then(Value::as_object)
        .and_then(|resources| resources.get("hooks"))
        .and_then(Value::as_array)
        .is_some_and(|hooks| !hooks.is_empty());
    Ok(NativePluginMatches {
        user: Some(NativePluginInventory {
            enabled,
            hooks_loaded,
        }),
        non_user_scopes,
    })
}

fn require_cli_success(
    program: &str,
    action: &str,
    output: super::driver::CliOutput,
) -> Result<(), AdapterError> {
    if output.success() {
        Ok(())
    } else {
        Err(AdapterError::FrameworkCli {
            program: program.to_string(),
            reason: cli_failure_reason(action, &output),
        })
    }
}

fn require_native_qodercli(ctx: &DriverCtx) -> Result<String, AdapterError> {
    qodercli_program(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
        program: "qodercli".to_string(),
        reason: "qodercli not found on PATH or under ~/.qoder/bin".to_string(),
    })
}

fn verify_native_plugin(ctx: &DriverCtx, program: &str, plugin: &str) -> Result<(), String> {
    let output = ctx
        .ops
        .run_framework_cli_json(build_native_list_cmd(program))
        .map_err(|error| error.to_string())?;
    if !output.success() {
        return Err(cli_failure_reason("plugins list --json", &output));
    }
    let expected = plugin_entry(plugin);
    let inventory = parse_native_plugin(&output.stdout, &expected)?
        .ok_or_else(|| format!("{expected} is absent after install"))?;
    if !inventory.enabled {
        return Err(format!("{expected} is installed but disabled"));
    }
    if !inventory.hooks_loaded {
        return Err(format!("{expected} reports no loaded hooks"));
    }
    Ok(())
}

fn native_status(
    claim: &AdapterClaim,
    ctx: &DriverCtx,
) -> Result<AdapterStatusReport, AdapterError> {
    let detect = QoderDriver.detect(&HostEnv {
        user_home: ctx.user_home.clone(),
    });
    let mut conditions = vec![AdapterCondition {
        kind: AdapterConditionKind::FrameworkDetected,
        status: bool_status(detect.detected),
        reason: Some(detect.reason),
        resource: None,
    }];
    let Some(plugin) = resolve_plugin(claim) else {
        push_native_conditions(
            &mut conditions,
            ConditionStatus::False,
            ConditionStatus::False,
            ConditionStatus::False,
            ConditionStatus::False,
            Some("receipt has no Qoder plugin resource".to_string()),
        );
        return Ok(AdapterStatusReport {
            summary: summarize_native(claim.status, false, ConditionStatus::False),
            conditions,
        });
    };
    let Some(program) = qodercli_program(ctx.user_home.as_deref()) else {
        push_native_conditions(
            &mut conditions,
            ConditionStatus::Unknown,
            ConditionStatus::Unknown,
            ConditionStatus::Unknown,
            ConditionStatus::False,
            Some("qodercli unavailable; plugin inventory cannot be read".to_string()),
        );
        return Ok(AdapterStatusReport {
            summary: summarize_native(claim.status, false, ConditionStatus::False),
            conditions,
        });
    };
    let output = match ctx
        .ops
        .run_framework_cli_json(build_native_list_cmd(&program))
    {
        Ok(output) => output,
        Err(error) => {
            push_native_conditions(
                &mut conditions,
                ConditionStatus::Unknown,
                ConditionStatus::Unknown,
                ConditionStatus::Unknown,
                ConditionStatus::False,
                Some(format!("cannot read Qoder plugin inventory: {error}")),
            );
            return Ok(AdapterStatusReport {
                summary: summarize_native(claim.status, true, ConditionStatus::Unknown),
                conditions,
            });
        }
    };
    if !output.success() {
        push_native_conditions(
            &mut conditions,
            ConditionStatus::Unknown,
            ConditionStatus::Unknown,
            ConditionStatus::Unknown,
            ConditionStatus::False,
            Some(cli_failure_reason("plugins list --json", &output)),
        );
        return Ok(AdapterStatusReport {
            summary: summarize_native(claim.status, true, ConditionStatus::Unknown),
            conditions,
        });
    }
    let expected = plugin_entry(&plugin);
    let inventory = match parse_native_plugin(&output.stdout, &expected) {
        Ok(inventory) => inventory,
        Err(reason) => {
            push_native_conditions(
                &mut conditions,
                ConditionStatus::Unknown,
                ConditionStatus::Unknown,
                ConditionStatus::Unknown,
                ConditionStatus::False,
                Some(reason),
            );
            return Ok(AdapterStatusReport {
                summary: summarize_native(claim.status, true, ConditionStatus::Unknown),
                conditions,
            });
        }
    };
    let (registered, enabled, hooks, reason) = match inventory {
        Some(inventory) => (
            ConditionStatus::True,
            bool_status(inventory.enabled),
            bool_status(inventory.hooks_loaded),
            (!inventory.hooks_loaded).then(|| format!("{expected} reports no loaded hooks")),
        ),
        None => (
            ConditionStatus::False,
            ConditionStatus::False,
            ConditionStatus::False,
            Some(format!("{expected} is absent")),
        ),
    };
    push_native_conditions(
        &mut conditions,
        registered,
        enabled,
        hooks,
        ConditionStatus::True,
        reason,
    );
    let health = if registered == ConditionStatus::True
        && enabled == ConditionStatus::True
        && hooks == ConditionStatus::True
    {
        ConditionStatus::True
    } else {
        ConditionStatus::False
    };
    Ok(AdapterStatusReport {
        summary: summarize_native(claim.status, true, health),
        conditions,
    })
}

fn push_native_conditions(
    conditions: &mut Vec<AdapterCondition>,
    registered: ConditionStatus,
    enabled: ConditionStatus,
    hooks: ConditionStatus,
    verification: ConditionStatus,
    reason: Option<String>,
) {
    for (kind, status) in [
        (AdapterConditionKind::PluginRegistered, registered),
        (AdapterConditionKind::ActivationEnabled, enabled),
        (AdapterConditionKind::PluginResourcesLoaded, hooks),
        (AdapterConditionKind::VerificationSupported, verification),
    ] {
        conditions.push(AdapterCondition {
            kind,
            status,
            reason: reason.clone(),
            resource: (kind != AdapterConditionKind::VerificationSupported).then(|| {
                ClaimResourceRef {
                    id: RES_PLUGIN.to_string(),
                }
            }),
        });
    }
}

fn summarize_native(
    claim_status: ClaimStatus,
    detected: bool,
    health: ConditionStatus,
) -> AdapterSummary {
    if claim_status == ClaimStatus::CleanupFailed {
        return AdapterSummary::CleanupFailed;
    }
    if !detected || health == ConditionStatus::False {
        return AdapterSummary::Degraded;
    }
    match health {
        ConditionStatus::True => AdapterSummary::Healthy,
        ConditionStatus::Unknown => AdapterSummary::Unknown,
        ConditionStatus::False => AdapterSummary::Degraded,
    }
}

fn disable_native(claim: &AdapterClaim, ctx: &DriverCtx) -> Result<DisableReport, AdapterError> {
    let Some(payload) = qoder_payload(claim) else {
        return Ok(DisableReport {
            cleanup_complete: false,
            messages: vec![
                "qoder receipt has a non-qoder driver payload; receipt kept".to_string(),
            ],
        });
    };
    let Some(plugin) = resolve_plugin(claim) else {
        return Ok(DisableReport {
            cleanup_complete: false,
            messages: vec!["qoder receipt has no plugin resource; receipt kept".to_string()],
        });
    };
    if payload.plugin_preexisting {
        return Ok(DisableReport {
            cleanup_complete: true,
            messages: vec![format!(
                "retained pre-existing native Qoder plugin '{}'; ANOLISA never owned this registration",
                plugin_entry(&plugin)
            )],
        });
    }
    let Some(program) = qodercli_program(ctx.user_home.as_deref()) else {
        return Ok(DisableReport {
            cleanup_complete: false,
            messages: vec!["qodercli unavailable; receipt kept for retry".to_string()],
        });
    };
    let expected = plugin_entry(&plugin);
    if !native_install_confirmed(claim, payload) {
        let (cleanup_complete, message) = match ctx
            .ops
            .run_framework_cli_json(build_native_list_cmd(&program))
        {
            Ok(output) if output.success() => {
                match parse_native_plugin(&output.stdout, &expected) {
                    Ok(None) => (
                        true,
                        format!(
                            "verified {expected} is absent; removed unconfirmed native Qoder receipt"
                        ),
                    ),
                    Ok(Some(_)) => (
                        false,
                        format!(
                            "{expected} is registered but ANOLISA install ownership was never confirmed; left it untouched and kept receipt"
                        ),
                    ),
                    Err(reason) => (
                        false,
                        format!("cannot verify unconfirmed Qoder receipt: {reason}"),
                    ),
                }
            }
            Ok(output) => (false, cli_failure_reason("plugins list --json", &output)),
            Err(error) => (false, format!("Qoder inventory could not run: {error}")),
        };
        return Ok(DisableReport {
            cleanup_complete,
            messages: vec![message],
        });
    }
    let mut messages = Vec::new();
    match ctx
        .ops
        .run_framework_cli(build_native_uninstall_cmd(&program, &plugin))
    {
        Ok(output) if output.success() => {
            messages.push(format!("uninstalled native Qoder plugin '{plugin}'"));
        }
        Ok(output) => messages.push(cli_failure_reason("plugins uninstall", &output)),
        Err(error) => messages.push(format!("plugins uninstall could not run: {error}")),
    }

    let cleanup_complete = match ctx
        .ops
        .run_framework_cli_json(build_native_list_cmd(&program))
    {
        Ok(output) if output.success() => match parse_native_plugin(&output.stdout, &expected) {
            Ok(None) => {
                messages.push(format!("verified {expected} is absent"));
                true
            }
            Ok(Some(_)) => {
                messages.push(format!("{expected} remains registered after uninstall"));
                false
            }
            Err(reason) => {
                messages.push(format!("cannot verify Qoder uninstall: {reason}"));
                false
            }
        },
        Ok(output) => {
            messages.push(cli_failure_reason("plugins list --json", &output));
            false
        }
        Err(error) => {
            messages.push(format!("cannot verify Qoder uninstall: {error}"));
            false
        }
    };
    Ok(DisableReport {
        cleanup_complete,
        messages,
    })
}

// ---------------------------------------------------------------------------
// qodercli resolution
// ---------------------------------------------------------------------------

/// Resolve the qodercli binary in the legacy search order, honoring the
/// `QODERCLI_BIN` override first.
fn resolve_qodercli(user_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("QODERCLI_BIN") {
        let s = bin.to_string_lossy();
        if !s.is_empty() {
            let p = PathBuf::from(s.as_ref());
            if is_executable_file(&p) {
                return Some(p);
            }
            // A bare name override resolves via PATH.
            return find_binary_in_path(&s);
        }
    }
    if let Some(home) = user_home {
        let dir = qoder_home(home).join("bin").join("qodercli");
        if let Some(versioned) = highest_versioned_qodercli(&dir) {
            return Some(versioned);
        }
        let unversioned = dir.join("qodercli");
        if is_executable_file(&unversioned) {
            return Some(unversioned);
        }
    }
    find_binary_in_path("qodercli")
}

/// Program string for a [`FrameworkCommand`] built from [`resolve_qodercli`].
fn qodercli_program(user_home: Option<&Path>) -> Option<String> {
    resolve_qodercli(user_home).map(|p| p.to_string_lossy().into_owned())
}

/// Highest-versioned `qodercli-X.Y.Z` under `dir`.
///
/// Numeric components sort semver-ish (`10 > 9`), and a stable suffix wins
/// over a prerelease with the same numeric core (`1.0.0 > 1.0.0-rc1`).
fn highest_versioned_qodercli(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(Vec<u64>, bool, String, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(suffix) = name.strip_prefix("qodercli-") else {
            continue;
        };
        if suffix.is_empty() {
            continue;
        }
        let path = entry.path();
        if !is_executable_file(&path) {
            continue;
        }
        let key = version_key(suffix);
        let stable = is_stable_version_suffix(suffix);
        let better = match &best {
            None => true,
            Some((bk, bstable, bs, _)) => {
                key > *bk
                    || (key == *bk && stable && !*bstable)
                    || (key == *bk && stable == *bstable && suffix > bs.as_str())
            }
        };
        if better {
            best = Some((key, stable, suffix.to_string(), path));
        }
    }
    best.map(|(_, _, _, p)| p)
}

/// Numeric components of the stable core of a version suffix.
fn version_key(suffix: &str) -> Vec<u64> {
    let core = suffix
        .split_once('-')
        .map(|(core, _)| core)
        .unwrap_or(suffix);
    core.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u64>().ok())
        .collect()
}

fn is_stable_version_suffix(suffix: &str) -> bool {
    suffix.chars().all(|c| c.is_ascii_digit() || c == '.')
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

// ---------------------------------------------------------------------------
// Command builders
// ---------------------------------------------------------------------------

fn base_cmd(program: &str, args: Vec<String>) -> FrameworkCommand {
    FrameworkCommand {
        program: program.to_string(),
        args,
        stdin: None,
        env_set: Vec::new(),
        env_remove: Vec::new(),
        path_prepend: Vec::new(),
        timeout: CLI_TIMEOUT,
    }
}

fn build_install_cmd(program: &str, staging: &Path) -> FrameworkCommand {
    base_cmd(
        program,
        vec![
            "plugins".to_string(),
            "install".to_string(),
            staging.to_string_lossy().into_owned(),
        ],
    )
}

fn build_uninstall_cmd(program: &str, plugin: &str) -> FrameworkCommand {
    base_cmd(
        program,
        vec![
            "plugins".to_string(),
            "uninstall".to_string(),
            plugin.to_string(),
        ],
    )
}

fn build_native_help_cmd(program: &str, subcommand: &str) -> FrameworkCommand {
    base_cmd(
        program,
        vec![
            "plugins".to_string(),
            subcommand.to_string(),
            "--help".to_string(),
        ],
    )
}

fn build_native_validate_cmd(program: &str, root: &Path) -> FrameworkCommand {
    base_cmd(
        program,
        vec![
            "plugins".to_string(),
            "validate".to_string(),
            root.to_string_lossy().into_owned(),
        ],
    )
}

fn build_native_install_cmd(program: &str, root: &Path) -> FrameworkCommand {
    base_cmd(
        program,
        vec![
            "plugins".to_string(),
            "install".to_string(),
            root.to_string_lossy().into_owned(),
            "--scope".to_string(),
            "user".to_string(),
        ],
    )
}

fn build_native_list_cmd(program: &str) -> FrameworkCommand {
    base_cmd(
        program,
        vec![
            "plugins".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ],
    )
}

fn build_native_uninstall_cmd(program: &str, plugin: &str) -> FrameworkCommand {
    base_cmd(
        program,
        vec![
            "plugins".to_string(),
            "uninstall".to_string(),
            plugin.to_string(),
            "--scope".to_string(),
            "user".to_string(),
        ],
    )
}

// ---------------------------------------------------------------------------
// Status assembly
// ---------------------------------------------------------------------------

/// Roll signals into a summary. Healthy requires the framework detected and
/// our managed settings entries verified present. Plugin registration is
/// deliberately excluded (qodercli's list is unreliable).
fn summarize(
    claim_status: ClaimStatus,
    detected: bool,
    settings: ConditionStatus,
) -> AdapterSummary {
    if claim_status == ClaimStatus::CleanupFailed {
        return AdapterSummary::CleanupFailed;
    }
    if !detected {
        return AdapterSummary::Degraded;
    }
    match settings {
        ConditionStatus::True => AdapterSummary::Healthy,
        ConditionStatus::False => AdapterSummary::Degraded,
        ConditionStatus::Unknown => AdapterSummary::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_plugin_scoped() {
        assert_eq!(plugin_entry("tokenless"), "tokenless@local");
    }

    #[test]
    fn install_and_uninstall_cmd_shapes() {
        let install = build_install_cmd("qodercli", Path::new("/data/qoder-plugins/tokenless"));
        assert_eq!(install.program, "qodercli");
        assert_eq!(
            install.args,
            vec!["plugins", "install", "/data/qoder-plugins/tokenless"]
        );
        let uninstall = build_uninstall_cmd("qodercli", "tokenless");
        assert_eq!(uninstall.args, vec!["plugins", "uninstall", "tokenless"]);

        let root = Path::new("/data/adapters/tokenless/qoder");
        assert_eq!(
            build_native_validate_cmd("qodercli", root).args,
            vec!["plugins", "validate", "/data/adapters/tokenless/qoder"]
        );
        assert_eq!(
            build_native_install_cmd("qodercli", root).args,
            vec![
                "plugins",
                "install",
                "/data/adapters/tokenless/qoder",
                "--scope",
                "user"
            ]
        );
        assert_eq!(
            build_native_list_cmd("qodercli").args,
            vec!["plugins", "list", "--json"]
        );
        assert_eq!(
            build_native_uninstall_cmd("qodercli", "tokenless").args,
            vec!["plugins", "uninstall", "tokenless", "--scope", "user"]
        );
    }

    #[test]
    fn native_inventory_requires_enabled_plugin_with_hooks() {
        let valid = r#"[{"id":"tokenless@local","scope":"user","enabled":true,"resources":{"hooks":[{}]}}]"#;
        assert_eq!(
            parse_native_plugin(valid, "tokenless@local"),
            Ok(Some(NativePluginInventory {
                enabled: true,
                hooks_loaded: true,
            }))
        );
        assert_eq!(
            parse_native_plugin(r#"{"plugins":[]}"#, "tokenless@local"),
            Ok(None)
        );
        assert!(parse_native_plugin("not-json", "tokenless@local").is_err());
        assert!(
            parse_native_plugin(
                r#"[{"id":"tokenless@local","scope":"user","resources":{"hooks":[{}]}}]"#,
                "tokenless@local"
            )
            .is_err()
        );
        assert_eq!(
            parse_native_plugin(
                r#"[{"pluginId":"tokenless@local","scope":"user","enabled":false,"resources":{"hooks":[]}}]"#,
                "tokenless@local"
            ),
            Ok(Some(NativePluginInventory {
                enabled: false,
                hooks_loaded: false,
            }))
        );
    }

    #[test]
    fn native_inventory_preserves_cross_scope_conflicts() {
        let mixed = r#"[
            {"id":"tokenless@local","scope":"project","enabled":false,"resources":{"hooks":[]}},
            {"id":"tokenless@local","scope":"user","enabled":true,"resources":{"hooks":[{}]}}
        ]"#;
        assert_eq!(
            parse_native_plugin(mixed, "tokenless@local"),
            Ok(Some(NativePluginInventory {
                enabled: true,
                hooks_loaded: true,
            }))
        );
        assert_eq!(
            parse_native_plugin_matches(mixed, "tokenless@local"),
            Ok(NativePluginMatches {
                user: Some(NativePluginInventory {
                    enabled: true,
                    hooks_loaded: true,
                }),
                non_user_scopes: vec!["project".to_string()],
            })
        );
        assert_eq!(
            parse_native_plugin(
                r#"[{"id":"tokenless@local","scope":"project","enabled":true,"resources":{"hooks":[{}]}}]"#,
                "tokenless@local"
            ),
            Ok(None)
        );
        assert_eq!(
            parse_native_plugin_matches(
                r#"[
                    {"id":"tokenless@local","scope":"project"},
                    {"id":"tokenless@local","scope":"local"},
                    {"id":"tokenless@local","scope":"project"}
                ]"#,
                "tokenless@local"
            ),
            Ok(NativePluginMatches {
                user: None,
                non_user_scopes: vec!["local".to_string(), "project".to_string()],
            })
        );
        assert!(
            parse_native_plugin(
                r#"[{"id":"tokenless@local","enabled":true,"resources":{"hooks":[{}]}}]"#,
                "tokenless@local"
            )
            .is_err()
        );
        assert!(
            parse_native_plugin(
                r#"[
                    {"id":"tokenless@local","scope":"user","enabled":true,"resources":{"hooks":[{}]}},
                    {"id":"tokenless@local","scope":"user","enabled":true,"resources":{"hooks":[{}]}}
                ]"#,
                "tokenless@local"
            )
            .is_err()
        );
    }

    #[test]
    fn version_key_orders_semver_numerically() {
        assert!(version_key("10.0.0") > version_key("9.9.9"));
        assert!(version_key("1.2.0") > version_key("1.1.9"));
        assert_eq!(version_key("1.0.0-rc1"), version_key("1.0.0"));
        assert!(is_stable_version_suffix("1.0.0"));
        assert!(!is_stable_version_suffix("1.0.0-rc1"));
    }

    #[cfg(unix)]
    #[test]
    fn highest_versioned_qodercli_prefers_stable_over_prerelease() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["qodercli-1.0.0-rc1", "qodercli-1.0.0", "qodercli-0.9.9"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"#!/bin/sh\n").expect("write fake cli");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake cli");
        }
        assert_eq!(
            highest_versioned_qodercli(dir.path()),
            Some(dir.path().join("qodercli-1.0.0"))
        );
    }

    #[test]
    fn common_hooks_dir_is_sibling_of_resource_root() {
        assert_eq!(
            common_hooks_dir(Path::new("/data/adapters/tokenless/qoder")),
            PathBuf::from("/data/adapters/tokenless/common/hooks")
        );
    }

    #[test]
    fn read_bundle_requires_manifest_and_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("qoder");
        std::fs::create_dir_all(root.join(".qoder-plugin")).expect("mkdir");
        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/qoder-home"));

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
            fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
                Ok(std::fs::read(path).ok())
            }
        }
        let ops = StubOps;
        let mk_ctx = |root: &Path| DriverCtx {
            component: "tokenless".to_string(),
            framework: "qoder".to_string(),
            layout: &layout,
            resource_root: root.to_path_buf(),
            user_home: Some(PathBuf::from("/tmp/qoder-home")),
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
        let driver = QoderDriver::new();

        // plugin.json only -> hooks.json missing.
        std::fs::write(root.join(QODER_PLUGIN_MANIFEST), br#"{"name":"tokenless"}"#)
            .expect("write manifest");
        let err = driver
            .read_bundle(&mk_ctx(&root))
            .expect_err("hooks.json missing must fail");
        assert!(matches!(err, AdapterError::BundleInvalid { .. }));

        // Legacy root hooks only -> ok.
        std::fs::write(root.join(QODER_HOOKS_FILE), b"{}").expect("write hooks");
        let bundle = driver.read_bundle(&mk_ctx(&root)).expect("legacy bundle");
        assert_eq!(bundle.plugin_id.as_deref(), Some("tokenless"));

        // Qoder CLI ignores alternate manifest paths, so the driver must
        // reject a contract that declares one even when the bundle is
        // otherwise complete.
        std::fs::write(root.join("custom.json"), br#"{"name":"tokenless"}"#)
            .expect("write alternate manifest");
        let mut alternate_ctx = mk_ctx(&root);
        alternate_ctx.declared_bundle_entry = Some("custom.json".to_string());
        assert!(!driver.probe_bundle(&root, Some("custom.json")));
        let err = driver
            .read_bundle(&alternate_ctx)
            .expect_err("alternate manifest must be rejected");
        assert!(matches!(err, AdapterError::BundleInvalid { .. }));

        // Native nested hooks only -> manifest name is authoritative.
        std::fs::remove_file(root.join(QODER_HOOKS_FILE)).expect("remove legacy hooks");
        std::fs::create_dir_all(root.join("hooks")).expect("mkdir native hooks");
        std::fs::write(
            root.join(QODER_NATIVE_HOOKS_FILE),
            br#"{"hooks":{"PreToolUse":[]}}"#,
        )
        .expect("write native hooks");
        let bundle = driver.read_bundle(&mk_ctx(&root)).expect("native bundle");
        assert_eq!(bundle.plugin_id.as_deref(), Some("tokenless"));

        // Both layouts are ambiguous and fail closed in probe and read.
        std::fs::write(root.join(QODER_HOOKS_FILE), b"{}").expect("write legacy hooks");
        assert!(!driver.probe_bundle(&root, None));
        assert!(matches!(
            driver.read_bundle(&mk_ctx(&root)),
            Err(AdapterError::BundleInvalid { .. })
        ));
        std::fs::remove_file(root.join(QODER_HOOKS_FILE)).expect("remove legacy hooks");

        // Native hook JSON must parse and expose a top-level hooks object.
        std::fs::write(root.join(QODER_NATIVE_HOOKS_FILE), b"not-json")
            .expect("write invalid hooks");
        assert!(matches!(
            driver.read_bundle(&mk_ctx(&root)),
            Err(AdapterError::BundleInvalid { .. })
        ));
        std::fs::write(root.join(QODER_NATIVE_HOOKS_FILE), br#"{"hooks":[]}"#)
            .expect("write wrong hook shape");
        assert!(matches!(
            driver.read_bundle(&mk_ctx(&root)),
            Err(AdapterError::BundleInvalid { .. })
        ));

        // Contract plugin_id and native manifest name must agree.
        std::fs::write(
            root.join(QODER_NATIVE_HOOKS_FILE),
            br#"{"hooks":{"PreToolUse":[]}}"#,
        )
        .expect("restore native hooks");
        std::fs::write(root.join(QODER_PLUGIN_MANIFEST), br#"{"name":"sec-core"}"#)
            .expect("write mismatched manifest");
        assert!(matches!(
            driver.read_bundle(&mk_ctx(&root)),
            Err(AdapterError::BundleInvalid { .. })
        ));
    }
}
