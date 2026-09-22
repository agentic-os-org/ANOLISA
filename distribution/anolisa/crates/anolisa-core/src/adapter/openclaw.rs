//! OpenClaw framework driver.
//!
//! OpenClaw plugin adapters use the CLI-managed registry: `enable` runs
//! `openclaw plugins install <resource_root>` then `plugins enable <plugin_id>`;
//! `disable` runs `openclaw plugins uninstall <plugin_id>`. Skill-only adapters
//! (`adapter_type = "skill_bundle"`) skip registry operations and only
//! copy declared skills into the OpenClaw skills directory. Status is the
//! read-only `openclaw plugins list` for plugin adapters. All CLI and
//! filesystem operations go through the Manager's helpers — the driver
//! only builds argv arrays from validated data.
//!
//! The CLI env contract mirrors `openclaw/scripts/install.sh`: unset
//! `OPENCLAW_HOME`, set `OPENCLAW_STATE_DIR` to the resolved state directory,
//! and prepend the standard bin dirs to `PATH`. `OPENCLAW_BIN` overrides
//! the executable (used by tests to point at a fake CLI).
//!
//! Before the first framework mutation — during `prepare_enable` (and, for
//! `--dry-run`, `plan_enable`) — `enable` builds a read-only
//! `OpenClawHostProfile` from read-only probes (`openclaw --version`,
//! `plugins install --help`, `plugins enable --help`, `plugins inspect --help`).
//! From it the driver gates on the adapter's declared framework version, chooses
//! version-conditioned config, requires install `--force`, and accepts declared
//! capabilities when each subcommand advertises `--accept-capabilities`. It adds
//! `--dangerously-force-unsafe-install` only when both authorized and
//! advertised as effective by the host, and records the inspect capabilities.
//! The install/verify capabilities flow to `apply_enable` as typed
//! [`PreparedEnable`] state, so
//! apply performs no probe of its own — each probe runs exactly once per
//! enable, all before the first mutation. The host version or argv is never
//! written into the receipt — the receipt stays pure typed data.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    ConfigApplyState, DRIVER_SCHEMA_VERSION, DriverPayload, OpenClawClaim, validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, CliOutput, ConditionStatus, DetectResult, DisableReport, DriverCtx,
    DriverPlan, EnableProgress, FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable,
    find_binary_in_path,
};
use super::managed_files::{MaterializedMapping, copy_materialized_resource};
use crate::manifest::AdapterConfigSetSpec;

/// Default timeout for an OpenClaw CLI invocation.
const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Resource ids used in OpenClaw receipts. Stable strings referenced from
/// the [`OpenClawClaim`] payload and condition reports.
const RES_STATE_DIR: &str = "openclaw_state_dir";
const RES_PLUGIN: &str = "openclaw_plugin";

/// OpenClaw driver. Stateless; all per-operation context arrives via
/// [`DriverCtx`].
pub struct OpenClawDriver;

impl OpenClawDriver {
    /// Construct the driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenClawDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for OpenClawDriver {
    fn name(&self) -> &'static str {
        "openclaw"
    }

    fn detect(&self, env: &HostEnv) -> DetectResult {
        match find_binary_in_path(&openclaw_bin()) {
            Some(path) => DetectResult {
                detected: true,
                reason: format!("openclaw CLI found at {}", path.display()),
            },
            None => {
                // The CLI is what enable/disable need; a bare home dir is
                // not sufficient. Report not-detected but mention the home
                // so a user understands the framework is partially present.
                let home_note = openclaw_home(env.user_home.as_deref())
                    .filter(|h| h.exists())
                    .map(|h| format!(" (home {} exists but CLI is not on PATH)", h.display()))
                    .unwrap_or_default();
                DetectResult {
                    detected: false,
                    reason: format!("openclaw CLI not found on PATH{home_note}"),
                }
            }
        }
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        openclaw_allowed_roots(ctx.user_home.as_deref())
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
            let manifest_file = ctx
                .declared_bundle_entry
                .as_deref()
                .unwrap_or("openclaw.plugin.json");
            ctx.declared_plugin_id
                .clone()
                .or(read_plugin_manifest_id(root, manifest_file)?)
                .or_else(|| Some(ctx.component.clone()))
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
        let mut enable_command = None;
        // Plugin adapters resolve a read-only host profile so the dry-run
        // plan shows the exact install command (including whether the
        // authorized unsafe flag is used) and only the config the host
        // version actually selects — the same decisions a real enable makes.
        let mut selected_config: Vec<(usize, &AdapterConfigSetSpec)> = Vec::new();
        if ctx.is_skill_bundle() {
            // The adapter-level version gate still applies to skill bundles.
            self.gate_skill_bundle_version(ctx)?;
        } else {
            let plugin_id = require_plugin_id(bundle)?;
            validate_plugin_id(&plugin_id)?;
            let preflight = self.plugin_preflight(&bundle.resource_root, ctx)?;
            enable_command = Some(display_command(&build_enable_cmd(
                &plugin_id,
                &home,
                ctx.user_home.as_deref(),
                preflight.supports_enable_accept_capabilities,
            )));
            selected_config = preflight.selected_config;
            register_command = Some(display_command(&preflight.install_cmd));
            actions.push(format!(
                "register openclaw plugin '{plugin_id}' from {}",
                bundle.resource_root.display()
            ));
        }

        for skill in &ctx.declared_skills {
            let src_display = match skill.source {
                Some(ref s) => s.display().to_string(),
                None => format!("{}/skills/{}", bundle.resource_root.display(), skill.name,),
            };
            actions.push(format!(
                "deliver openclaw skill '{}' from {} to {}/skills/{}",
                skill.name,
                src_display,
                home.display(),
                skill.name,
            ));
        }
        for &(i, cfg) in &selected_config {
            actions.push(format!(
                "set openclaw config [{i}] {} = {}",
                cfg.key,
                config_value_display(&cfg.value)
            ));
        }

        if let Some(command) = enable_command {
            actions.push(format!("enable openclaw plugin: {command}"));
        }

        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions,
            register_command,
        })
    }

    fn plan_reenable_cleanup(
        &self,
        prior: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        let prior_home = claim_state_dir(prior)?;
        if prior_home == require_home(ctx)? {
            return Ok(Vec::new());
        }

        let mut actions = Vec::new();
        if let Some(plugin_id) = claim_plugin_id(prior) {
            validate_plugin_id(&plugin_id)?;
            actions.push(format!(
                "unregister prior openclaw plugin '{plugin_id}' from {}",
                prior_home.display()
            ));
        }
        for skill_name in claim_skill_resources(prior) {
            actions.push(format!(
                "remove prior openclaw skill '{skill_name}' from {}",
                prior_home.join("skills").join(&skill_name).display()
            ));
        }
        Ok(actions)
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let home = require_home(ctx)?;

        // Resolve the plugin id and, for plugin adapters, run all read-only
        // probing and gating BEFORE building any resource — so a version
        // mismatch, a missing `--force`/`--json`, or an unsupported authorized
        // unsafe flag fails here, before the Manager persists the receipt. The
        // config the host version selects is computed once and carried as
        // transient intent; apply journals each entry as Pending before its
        // write and confirms it only after success. The install/verify
        // capabilities are carried forward to `apply_enable` as typed
        // `PreparedEnable` so apply never re-probes.
        let plugin_id = if ctx.is_skill_bundle() {
            None
        } else {
            let plugin_id = require_plugin_id(bundle)?;
            validate_plugin_id(&plugin_id)?;
            Some(plugin_id)
        };
        let prepared = if ctx.is_skill_bundle() {
            // Skill bundles run no plugin install, but the adapter-level
            // version gate still applies before the receipt is persisted.
            self.gate_skill_bundle_version(ctx)?;
            PreparedEnable::None
        } else {
            let preflight = self.plugin_preflight(&bundle.resource_root, ctx)?;
            PreparedEnable::OpenClaw {
                supports_accept_capabilities: preflight.supports_accept_capabilities,
                supports_enable_accept_capabilities: preflight.supports_enable_accept_capabilities,
                supports_unsafe_install: preflight.supports_unsafe_install,
                supports_inspect_json: preflight.supports_inspect_json,
                supports_inspect_runtime: preflight.supports_inspect_runtime,
                selected_config_indices: preflight
                    .selected_config
                    .iter()
                    .map(|(index, _)| *index)
                    .collect(),
            }
        };

        let mut resources = vec![ClaimResource {
            id: RES_STATE_DIR.to_string(),
            purpose: "openclaw_state_dir".to_string(),
            kind: ClaimResourceKind::ExternalPath { path: home.clone() },
        }];
        if let Some(plugin_id) = &plugin_id {
            resources.push(ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: "openclaw_plugin".to_string(),
                kind: ClaimResourceKind::FrameworkPlugin {
                    framework: self.name().to_string(),
                    plugin_id: plugin_id.clone(),
                },
            });
        }

        let mut skill_resources = Vec::new();
        for skill in &ctx.declared_skills {
            let res_id = format!("openclaw_skill_{}", skill.name);
            resources.push(ClaimResource {
                id: res_id.clone(),
                purpose: "openclaw_skill".to_string(),
                kind: ClaimResourceKind::ExternalPath {
                    path: home.join("skills").join(&skill.name),
                },
            });
            skill_resources.push(res_id);
        }

        let claim = AdapterClaim {
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
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: RES_STATE_DIR.to_string(),
                plugin_resource: if ctx.is_skill_bundle() {
                    String::new()
                } else {
                    RES_PLUGIN.to_string()
                },
                skill_resources,
                // Pending/applied config resources are journaled during apply;
                // this list references confirmed entries only.
                config_resources: Vec::new(),
            }),
        };
        Ok((claim, prepared))
    }

    fn preserve_reenable_facts(
        &self,
        prior: &AdapterClaim,
        next: &mut AdapterClaim,
    ) -> Result<(), AdapterError> {
        preserve_openclaw_config_facts(prior, next)
    }

    fn materialized_mappings(
        &self,
        resource_root: &Path,
        _adapter_type: Option<&str>,
        declared_skills: &[super::driver::DeclaredSkill],
    ) -> Vec<MaterializedMapping> {
        declared_skills
            .iter()
            .map(|skill| MaterializedMapping {
                resource_id: format!("openclaw_skill_{}", skill.name),
                source_root: skill
                    .source
                    .clone()
                    .unwrap_or_else(|| resource_root.join("skills").join(&skill.name)),
                excluded_prefixes: Vec::new(),
            })
            .collect()
    }

    fn materialized_destination_roots(
        &self,
        _bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<BTreeMap<String, PathBuf>, AdapterError> {
        let home = require_home(ctx)?;
        Ok(ctx
            .declared_skills
            .iter()
            .map(|skill| {
                (
                    format!("openclaw_skill_{}", skill.name),
                    home.join("skills").join(&skill.name),
                )
            })
            .collect())
    }

    fn materialized_verification_applicable(&self, claim: &AdapterClaim) -> bool {
        matches!(
            &claim.driver_payload,
            DriverPayload::OpenClaw(payload) if !payload.skill_resources.is_empty()
        )
    }

    fn cleanup_replaced_claim(
        &self,
        prior: &AdapterClaim,
        next: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        if claim_state_dir(prior)? == claim_state_dir(next)? {
            return Ok(DisableReport {
                cleanup_complete: true,
                messages: Vec::new(),
            });
        }

        // A state-directory migration creates a separate OpenClaw registry
        // and skills tree. Release the prior installation while its validated
        // receipt is still durable, before the Manager replaces ownership.
        self.disable(prior, ctx)
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        prepared: &PreparedEnable,
        ctx: &DriverCtx,
        progress: &mut dyn EnableProgress,
    ) -> Result<(), AdapterError> {
        let home = require_home(ctx)?;
        let user_home = ctx.user_home.as_deref();

        // The install/verify capabilities (unsafe support, inspect `--runtime`)
        // and all gating were resolved and validated by `prepare_enable`, which
        // probed the host once and handed the results forward as `prepared`.
        // `apply_enable` therefore does NOT probe at all: the install argv is
        // rebuilt from the typed `ctx` and prepared consent support; the unsafe
        // flag still requires explicit authorization. Config selection comes from
        // prepared state, and runtime verification uses the prepared
        // `--runtime` capability. Each probe (`--version`, install/enable/inspect
        // `--help`) thus runs exactly once per enable, all in prepare,
        // and no two probe generations are ever mixed.
        //
        // Defense in depth: `PreparedEnable` and this trait are public, so a
        // caller could hand a mismatched value. Validate the (adapter kind,
        // prepared variant) pairing and re-check the capabilities BEFORE the
        // first mutation, failing closed on any mismatch rather than silently
        // degrading (which could skip the `--json` precondition, verify without
        // `--runtime`, or add the unsafe flag on an unverified host).
        let (
            accept_capabilities,
            enable_accept_capabilities,
            host_supports_unsafe,
            verify_with_runtime,
            selected_config_indices,
        ) = if ctx.is_skill_bundle() {
            if !matches!(prepared, PreparedEnable::None) {
                return Err(prepared_state_mismatch(
                    "skill_bundle adapters carry no prepared host capabilities",
                ));
            }
            // Skill bundles run no plugin install and no runtime verification;
            // these values are unused for them.
            (false, false, false, false, Vec::new())
        } else {
            match prepared {
                PreparedEnable::OpenClaw {
                    supports_accept_capabilities,
                    supports_enable_accept_capabilities,
                    supports_unsafe_install,
                    supports_inspect_json,
                    supports_inspect_runtime,
                    selected_config_indices,
                } => {
                    if !supports_inspect_json {
                        return Err(AdapterError::FrameworkCli {
                            program: openclaw_bin(),
                            reason: "`openclaw plugins inspect --help` does not expose --json; \
                                     cannot verify plugin runtime status"
                                .to_string(),
                        });
                    }
                    if ctx.allow_unsafe_plugin_install && !supports_unsafe_install {
                        return Err(AdapterError::FrameworkCli {
                            program: openclaw_bin(),
                            reason: "unsafe plugin install was explicitly authorized but this \
                                     openclaw does not expose --dangerously-force-unsafe-install"
                                .to_string(),
                        });
                    }
                    validate_prepared_config_indices(selected_config_indices, ctx)?;
                    (
                        *supports_accept_capabilities,
                        *supports_enable_accept_capabilities,
                        *supports_unsafe_install,
                        *supports_inspect_runtime,
                        selected_config_indices.clone(),
                    )
                }
                PreparedEnable::None => {
                    return Err(prepared_state_mismatch(
                        "openclaw plugin enable requires prepared host capabilities",
                    ));
                }
                PreparedEnable::QoderNative { .. } => {
                    return Err(prepared_state_mismatch(
                        "openclaw plugin enable received Qoder capabilities",
                    ));
                }
            }
        };
        validate_config_claim_state(claim)?;
        validate_pending_config_selection(claim, &selected_config_indices, ctx)?;

        let plugin = if ctx.is_skill_bundle() {
            None
        } else {
            let plugin_id = claim_plugin_id(claim).ok_or_else(|| AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: "openclaw receipt has no plugin id".to_string(),
            })?;
            validate_plugin_id(&plugin_id)?;
            let cmd = base_cmd(
                install_argv(
                    &claim.resource_root,
                    ctx.allow_unsafe_plugin_install,
                    accept_capabilities,
                ),
                &home,
                user_home,
            );
            let program = cmd.program.clone();
            let output = ctx.ops.run_framework_cli(cmd)?;
            if !output.success() {
                let mut reason = full_failure_reason("plugins install", &output);
                // Point the operator at the explicit, auditable retry only when
                // it could actually help: the host exposes the unsafe flag, the
                // user did not already authorize it, and the failure looks like
                // a plugin-safety rejection. Never retry automatically.
                if install_output_requires_capability_consent(&output) {
                    reason.push_str(
                        "; OpenClaw capability consent was not accepted; inspect the reported \
                         capability requirements and the host's --accept-capabilities support",
                    );
                } else if host_supports_unsafe
                    && !ctx.allow_unsafe_plugin_install
                    && install_output_looks_like_safety_rejection(&output)
                {
                    reason.push_str(
                        "; this looks like an OpenClaw plugin-safety rejection — review the \
                         reported findings and, only if you accept them, re-run enable with \
                         --allow-unsafe-plugin-install",
                    );
                }
                return Err(AdapterError::FrameworkCli { program, reason });
            }
            Some(plugin_id)
        };

        for skill in &ctx.declared_skills {
            let src = skill
                .source
                .clone()
                .unwrap_or_else(|| ctx.resource_root.join("skills").join(&skill.name));
            let resource_id = format!("openclaw_skill_{}", skill.name);
            copy_materialized_resource(claim, &resource_id, &src, ctx.ops)?;
        }

        if let Some(plugin_id) = &plugin {
            // Apply only the entries selected during prepare. A selected
            // entry becomes a durable Pending resource before the command,
            // then transitions to Applied after success. Matching resources
            // from re-enable are reused rather than duplicated.
            for i in selected_config_indices {
                let cfg = &ctx.declared_config[i];
                let resource_id = ensure_config_intent(claim, i, cfg)?;
                // Write-ahead persistence closes the mutation-without-receipt
                // window. On timeout/non-zero exit the entry remains Pending,
                // accurately expressing that host state is uncertain.
                progress.persist_claim(claim)?;
                let cmd = build_config_set_cmd(&cfg.key, &cfg.value, &home, user_home);
                let program = cmd.program.clone();
                let output = ctx.ops.run_framework_cli(cmd)?;
                if !output.success() {
                    return Err(AdapterError::FrameworkCli {
                        program,
                        reason: full_failure_reason("config set", &output),
                    });
                }
                confirm_config_applied(claim, &resource_id, cfg)?;
                progress.persist_claim(claim)?;
            }

            // Install preserves an explicit disabled entry left by uninstall.
            let cmd = build_enable_cmd(plugin_id, &home, user_home, enable_accept_capabilities);
            let program = cmd.program.clone();
            let output = ctx.ops.run_framework_cli(cmd)?;
            if !output.success() {
                return Err(AdapterError::FrameworkCli {
                    program,
                    reason: full_failure_reason("plugins enable", &output),
                });
            }

            // Post-enable runtime verification: the plugin must report
            // loaded. A non-loaded status surfaces the framework diagnostics
            // and, via the Manager's receipt-first model, leaves a
            // cleanup_failed receipt for later disable.
            self.verify_runtime(plugin_id, &home, user_home, ctx, verify_with_runtime)?;
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
                        reason: Some("openclaw CLI unavailable".to_string()),
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
        // Disable must clean the state directory recorded at enable time. In
        // particular, pre-fix receipts can point at the legacy resolver's
        // directory while the caller's active OPENCLAW_STATE_DIR points
        // elsewhere.
        let home = claim_state_dir(claim)?;
        let mut messages = Vec::new();
        let mut cleanup_complete = true;

        if let Some(plugin_id) = claim_plugin_id(claim) {
            validate_plugin_id(&plugin_id)?;
            if find_binary_in_path(&openclaw_bin()).is_none() {
                return Ok(DisableReport {
                    cleanup_complete: false,
                    messages: vec![
                        "openclaw CLI not found on PATH; receipt kept so cleanup can be retried"
                            .to_string(),
                    ],
                });
            }

            let cmd = build_uninstall_cmd(&plugin_id, &home, ctx.user_home.as_deref());
            let output = ctx.ops.run_framework_cli(cmd)?;
            if output.success() {
                messages.push(format!("unregistered openclaw plugin '{plugin_id}'"));
            } else if uninstall_reports_missing_plugin(&output, &plugin_id) {
                messages.push(format!(
                    "openclaw plugin '{plugin_id}' was already unregistered"
                ));
            } else if uninstall_reports_untracked_plugin(&output, &plugin_id) {
                // Missing package ownership does not prove that the plugin is
                // gone. Verify against the same instance as the uninstall.
                let mut cmd = build_list_cmd(&home, ctx.user_home.as_deref());
                cmd.args.push("--json".to_string());
                let verification = ctx.ops.run_framework_cli_json(cmd);
                if !verification
                    .as_ref()
                    .is_ok_and(|output| json_list_confirms_plugin_absent(output, &plugin_id))
                {
                    let reason = match verification {
                        Ok(output) => inspect_diagnostics(&output),
                        Err(err) => err.to_string(),
                    };
                    return Ok(DisableReport {
                        cleanup_complete: false,
                        messages: vec![format!(
                            "openclaw plugin '{plugin_id}' has no tracked package install; \
                             `plugins list --json` could not confirm absence: {reason}; \
                             repair the OpenClaw plugin registry and retry disable"
                        )],
                    });
                }
                messages.push(format!(
                    "openclaw plugin '{plugin_id}' is absent from `plugins list --json`"
                ));
            } else {
                return Ok(DisableReport {
                    cleanup_complete: false,
                    messages: vec![format!(
                        "openclaw plugin uninstall failed: {}",
                        cli_failure_reason("plugins uninstall", &output)
                    )],
                });
            }
        } else {
            messages.push("receipt records no plugin to unregister".to_string());
        }

        let skill_resources = claim_skill_resources(claim);
        for skill_name in &skill_resources {
            let skill_dir = home.join("skills").join(skill_name);
            match ctx.ops.remove_tree(&skill_dir) {
                Ok(true) => messages.push(format!(
                    "removed openclaw skill dir {}",
                    skill_dir.display()
                )),
                Ok(false) => {} // already gone, idempotent
                Err(err) => {
                    messages.push(format!(
                        "failed to remove skill dir {}: {err}",
                        skill_dir.display()
                    ));
                    cleanup_complete = false;
                }
            }
        }

        // 3. Config entries are NOT reversed on disable (framework-wide
        //    config should persist).
        let (applied_config_count, pending_config_count) = claim_config_counts(claim);
        if applied_config_count > 0 {
            let noun = if applied_config_count == 1 {
                "entry"
            } else {
                "entries"
            };
            messages.push(format!(
                "{applied_config_count} confirmed openclaw config {noun} left in place \
                 (not reversed on disable)"
            ));
        }
        if pending_config_count > 0 {
            let noun = if pending_config_count == 1 {
                "entry"
            } else {
                "entries"
            };
            messages.push(format!(
                "{pending_config_count} openclaw config {noun} with an uncertain apply outcome \
                 left in place (not reversed on disable)"
            ));
        }

        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }
}

impl OpenClawDriver {
    /// Run `openclaw plugins list` and decide whether `plugin_id` is still
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
        let home = match openclaw_home(ctx.user_home.as_deref()) {
            Some(h) => h,
            None => {
                return (
                    AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: ConditionStatus::Unknown,
                        reason: Some("cannot resolve openclaw home".to_string()),
                        resource: plugin_ref,
                    },
                    AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::False,
                        reason: Some("openclaw home unresolved".to_string()),
                        resource: None,
                    },
                    ConditionStatus::Unknown,
                );
            }
        };
        let cmd = build_list_cmd(&home, ctx.user_home.as_deref());
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
// Host profile (read-only probing) and enable gating
// ---------------------------------------------------------------------------

/// Read-only pre-install facts about the installed OpenClaw CLI, gathered by
/// [`OpenClawDriver::host_profile`] from version and install/enable/inspect
/// help probes before the first mutation. Never persisted into a receipt.
#[derive(Debug, Clone)]
struct OpenClawHostProfile {
    /// Parsed `openclaw --version`, when the output was recognizable.
    version: Option<OpenClawVersion>,
    /// Trimmed raw `--version` output, for diagnostics.
    version_display: String,
    /// `openclaw plugins install --help` exposes `--force`.
    supports_install_force: bool,
    /// `openclaw plugins install --help` exposes `--accept-capabilities`.
    supports_accept_capabilities: bool,
    /// `plugins enable --help` exposes `--accept-capabilities`.
    supports_enable_accept_capabilities: bool,
    /// `openclaw plugins install --help` exposes
    /// `--dangerously-force-unsafe-install`, including whether the advertised
    /// option is still effective or has become a deprecated no-op.
    unsafe_install_support: UnsafeInstallSupport,
    /// `openclaw plugins inspect --help` exposes `--json`.
    supports_inspect_json: bool,
    /// `openclaw plugins inspect --help` exposes `--runtime`.
    supports_inspect_runtime: bool,
}

/// Effective semantics of OpenClaw's unsafe-install compatibility option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsafeInstallSupport {
    Unsupported,
    Effective,
    DeprecatedNoOp,
}

impl UnsafeInstallSupport {
    fn is_effective(self) -> bool {
        self == Self::Effective
    }
}

/// A plugin adapter's resolved read-only preflight, derived before the first
/// framework mutation and consumed by `prepare_enable`/`plan_enable`: the
/// selected config, the single install command, and the install/verify
/// capabilities to hand forward as [`PreparedEnable`].
struct PluginPreflight<'a> {
    /// The host's installer supports accepting declared plugin capabilities.
    supports_accept_capabilities: bool,
    /// `plugins enable --help` exposes `--accept-capabilities`.
    supports_enable_accept_capabilities: bool,
    /// The host's `plugins install --help` exposes the unsafe flag.
    supports_unsafe_install: bool,
    /// The host's `plugins inspect --help` exposes `--json`.
    supports_inspect_json: bool,
    /// The host's `plugins inspect --help` exposes `--runtime`.
    supports_inspect_runtime: bool,
    /// Selected config entries as `(original manifest index, spec)`. The
    /// index anchors the receipt's `openclaw_config_<i>` resource id so
    /// `apply_enable` can execute exactly the selected entries even when two
    /// entries share a config key.
    selected_config: Vec<(usize, &'a AdapterConfigSetSpec)>,
    install_cmd: FrameworkCommand,
}

impl OpenClawDriver {
    /// Run one read-only probe command, failing closed on timeout **or a
    /// non-zero exit**, and return its combined stdout/stderr on success. A
    /// failed probe must never be mistaken for a capability answer (e.g. an
    /// error message mentioning `--force`), so the exit status is checked
    /// before the output is interpreted; the diagnostics are carried in the
    /// error.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the probe cannot spawn, times out,
    /// or exits non-zero.
    fn run_read_probe(
        &self,
        ctx: &DriverCtx,
        cmd: FrameworkCommand,
        label: &str,
    ) -> Result<String, AdapterError> {
        let out = ctx.ops.run_framework_cli(cmd)?;
        if out.timed_out || !out.success() {
            return Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: full_failure_reason(label, &out),
            });
        }
        Ok(combine_output(&out))
    }

    /// Probe `openclaw --version` read-only, failing closed on timeout or a
    /// non-zero exit. The version may still be unparseable (`None`); callers
    /// that require it enforce that separately.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the probe cannot spawn, times out,
    /// or exits non-zero.
    fn probe_version(
        &self,
        ctx: &DriverCtx,
    ) -> Result<(Option<OpenClawVersion>, String), AdapterError> {
        let home = require_home(ctx)?;
        let text = self.run_read_probe(
            ctx,
            build_version_cmd(&home, ctx.user_home.as_deref()),
            "openclaw --version",
        )?;
        let version = parse_openclaw_version_output(&text);
        // Keep the full trimmed output (bounded by the Manager's capture cap)
        // so a diagnostic that precedes the version line does not hide the
        // real, unparseable version text in the error.
        Ok((version, text.trim().to_string()))
    }

    /// Probe the host CLI read-only for every pre-install fact: version,
    /// `plugins install/enable/inspect --help`. All probing
    /// happens here — before the first mutation — and the inspect
    /// capabilities are carried forward to `apply_enable` as
    /// [`PreparedEnable`] so apply never re-probes. Fails closed when a probe
    /// times out or exits non-zero.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when a probe command cannot be spawned,
    /// times out, or exits non-zero.
    fn host_profile(&self, ctx: &DriverCtx) -> Result<OpenClawHostProfile, AdapterError> {
        let home = require_home(ctx)?;
        let user_home = ctx.user_home.as_deref();

        let (version, version_display) = self.probe_version(ctx)?;

        let install_help = self.run_read_probe(
            ctx,
            build_install_help_cmd(&home, user_home),
            "openclaw plugins install --help",
        )?;
        let supports_install_force = help_lists_flag(&install_help, "--force");
        let supports_accept_capabilities = help_lists_flag(&install_help, "--accept-capabilities");
        let unsafe_install_support = unsafe_install_support(&install_help);

        let enable_help = self.run_read_probe(
            ctx,
            build_enable_cmd("--help", &home, user_home, false),
            "openclaw plugins enable --help",
        )?;
        let supports_enable_accept_capabilities =
            help_lists_flag(&enable_help, "--accept-capabilities");

        let inspect_help = self.run_read_probe(
            ctx,
            build_inspect_help_cmd(&home, user_home),
            "openclaw plugins inspect --help",
        )?;
        let supports_inspect_json = help_lists_flag(&inspect_help, "--json");
        let supports_inspect_runtime = help_lists_flag(&inspect_help, "--runtime");

        Ok(OpenClawHostProfile {
            version,
            version_display,
            supports_install_force,
            supports_accept_capabilities,
            supports_enable_accept_capabilities,
            unsafe_install_support,
            supports_inspect_json,
            supports_inspect_runtime,
        })
    }

    /// Enforce the adapter-level framework version requirement against a
    /// probed version. A `None` requirement is a no-op. When set, the host
    /// version must be known and satisfy the constraint. Applies to every
    /// OpenClaw adapter (plugin and skill_bundle alike).
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the version cannot be determined;
    /// [`AdapterError::InvalidAdapterInput`] when the requirement is malformed
    /// (a manifest bug); [`AdapterError::FrameworkVersionMismatch`] when the
    /// detected version does not satisfy the requirement.
    fn enforce_version_gate(
        &self,
        ctx: &DriverCtx,
        version: Option<&OpenClawVersion>,
        version_display: &str,
    ) -> Result<(), AdapterError> {
        // A missing field (`None`) means "no requirement". OpenClaw owns the
        // validity check for its own adapters (the framework-agnostic Manager
        // does not gate other frameworks on this), so a present-but-empty
        // requirement is a declaration error here, not a silent no-op.
        let Some(raw) = ctx.framework_version_req.as_deref() else {
            return Ok(());
        };
        let req = raw.trim();
        if req.is_empty() {
            return Err(AdapterError::InvalidAdapterInput {
                component: ctx.component.clone(),
                framework: ctx.framework.clone(),
                reason: "adapter framework_version requirement is present but empty".to_string(),
            });
        }
        let version = version.ok_or_else(|| AdapterError::FrameworkCli {
            program: openclaw_bin(),
            reason: format!(
                "cannot determine openclaw version (from `openclaw --version`: {version_display:?}) to check adapter requirement '{req}'"
            ),
        })?;
        match openclaw_version_req_satisfied(req, version) {
            Ok(true) => Ok(()),
            Ok(false) => Err(AdapterError::FrameworkVersionMismatch {
                framework: self.name().to_string(),
                detected: version.to_string(),
                required: req.to_string(),
            }),
            Err(reason) => Err(AdapterError::InvalidAdapterInput {
                component: ctx.component.clone(),
                framework: ctx.framework.clone(),
                reason: format!("invalid adapter framework_version requirement: {reason}"),
            }),
        }
    }

    /// Probe the version and enforce the adapter-level requirement for a
    /// skill_bundle adapter. Only probes when a requirement is declared, so a
    /// requirement-free skill bundle keeps its previous no-CLI behavior.
    ///
    /// # Errors
    ///
    /// As [`Self::enforce_version_gate`].
    fn gate_skill_bundle_version(&self, ctx: &DriverCtx) -> Result<(), AdapterError> {
        // Only probe when a requirement is declared, so a requirement-free
        // skill bundle keeps its previous no-CLI behavior. `enforce_version_gate`
        // still validates a present-but-empty requirement.
        if ctx.framework_version_req.is_none() {
            return Ok(());
        }
        let (version, display) = self.probe_version(ctx)?;
        self.enforce_version_gate(ctx, version.as_ref(), &display)
    }

    /// Resolve the full plugin preflight before any mutation: profile (all
    /// read-only probes), version precondition, `--json` inspect precondition,
    /// version gate, install command, and selected config. Fails closed — the
    /// version must be parseable, the host must expose machine-readable
    /// (`--json`) inspect output for verification, and the `--force` /
    /// unsafe-flag capabilities must hold (see [`build_install_cmd`]).
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] / [`AdapterError::FrameworkVersionMismatch`]
    /// / [`AdapterError::InvalidAdapterInput`] for any failed probe,
    /// precondition, gate, or malformed condition.
    fn plugin_preflight<'a>(
        &self,
        resource_root: &Path,
        ctx: &'a DriverCtx,
    ) -> Result<PluginPreflight<'a>, AdapterError> {
        let home = require_home(ctx)?;
        let profile = self.host_profile(ctx)?;
        // Fail closed before the first write: the version must be known even
        // when the manifest declares no version condition, so an unreadable
        // `--version` never silently proceeds to an install.
        if profile.version.is_none() {
            return Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "cannot determine openclaw version from `openclaw --version` (output: {:?})",
                    profile.version_display
                ),
            });
        }
        // Runtime verification relies on machine-readable inspect output, so a
        // host without `--json` is rejected before install, not after.
        if !profile.supports_inspect_json {
            return Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: "`openclaw plugins inspect --help` does not expose --json; \
                         cannot verify plugin runtime status"
                    .to_string(),
            });
        }
        self.enforce_version_gate(ctx, profile.version.as_ref(), &profile.version_display)?;
        let install_cmd = build_install_cmd(
            resource_root,
            &home,
            ctx.user_home.as_deref(),
            &profile,
            ctx.allow_unsafe_plugin_install,
        )?;
        let selected_config = self.select_config(ctx, &profile)?;
        Ok(PluginPreflight {
            supports_accept_capabilities: profile.supports_accept_capabilities,
            supports_enable_accept_capabilities: profile.supports_enable_accept_capabilities,
            supports_unsafe_install: profile.unsafe_install_support.is_effective(),
            supports_inspect_json: profile.supports_inspect_json,
            supports_inspect_runtime: profile.supports_inspect_runtime,
            selected_config,
            install_cmd,
        })
    }

    /// Select the declared config entries whose optional `framework_version`
    /// condition the host version satisfies, paired with their original
    /// manifest index. A missing condition (`None`) always selects; a
    /// condition the host does not satisfy is skipped (left out of the
    /// receipt). A present-but-empty or malformed condition is a manifest bug.
    ///
    /// # Errors
    ///
    /// [`AdapterError::InvalidAdapterInput`] when a condition is present but
    /// empty or malformed; [`AdapterError::FrameworkCli`] when a valid
    /// condition cannot be evaluated because the host version is unknown.
    fn select_config<'a>(
        &self,
        ctx: &'a DriverCtx,
        profile: &OpenClawHostProfile,
    ) -> Result<Vec<(usize, &'a AdapterConfigSetSpec)>, AdapterError> {
        let mut selected = Vec::new();
        for (i, cfg) in ctx.declared_config.iter().enumerate() {
            // A missing field means "always apply"; an explicit empty value is
            // a declaration error, not an implicit unconditional apply.
            let Some(raw) = cfg.framework_version.as_deref() else {
                selected.push((i, cfg));
                continue;
            };
            let req = raw.trim();
            if req.is_empty() {
                return Err(AdapterError::InvalidAdapterInput {
                    component: ctx.component.clone(),
                    framework: ctx.framework.clone(),
                    reason: format!(
                        "config '{}' declares an empty framework_version condition",
                        cfg.key
                    ),
                });
            }
            let version = profile.version.as_ref().ok_or_else(|| AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "cannot evaluate config version condition '{req}' for key '{}': openclaw version unknown",
                    cfg.key
                ),
            })?;
            match openclaw_version_req_satisfied(req, version) {
                Ok(true) => selected.push((i, cfg)),
                Ok(false) => {}
                Err(reason) => {
                    return Err(AdapterError::InvalidAdapterInput {
                        component: ctx.component.clone(),
                        framework: ctx.framework.clone(),
                        reason: format!(
                            "config '{}' has an invalid framework_version condition: {reason}",
                            cfg.key
                        ),
                    });
                }
            }
        }
        Ok(selected)
    }

    /// Verify the plugin reports `loaded` after install/skill/config apply.
    ///
    /// Uses `plugins inspect <id> --runtime --json` when `with_runtime` (the
    /// host's `--runtime` support, resolved during prepare and passed via
    /// [`PreparedEnable`]), else `plugins inspect <id> --json`. No inspect-help
    /// probe runs here — it already ran during prepare. The JSON is parsed in
    /// Rust (legacy diagnostic lines before it are tolerated) and
    /// `.plugin.status` must equal `"loaded"`.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] carrying the OpenClaw diagnostics when
    /// the command fails, the JSON is missing/invalid, or the status is not
    /// `loaded`.
    fn verify_runtime(
        &self,
        plugin_id: &str,
        home: &Path,
        user_home: Option<&Path>,
        ctx: &DriverCtx,
        with_runtime: bool,
    ) -> Result<(), AdapterError> {
        let cmd = build_inspect_cmd(plugin_id, home, user_home, with_runtime);
        let output = ctx.ops.run_framework_cli(cmd)?;
        if !output.success() {
            return Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "runtime verification of plugin '{plugin_id}' failed: {}",
                    inspect_diagnostics(&output)
                ),
            });
        }
        let value =
            extract_trailing_json(&output.stdout).ok_or_else(|| AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "could not parse `plugins inspect` JSON for plugin '{plugin_id}'; diagnostics: {}",
                    inspect_diagnostics(&output)
                ),
            })?;
        let status = value
            .get("plugin")
            .and_then(|p| p.get("status"))
            .and_then(|s| s.as_str());
        match status {
            Some("loaded") => Ok(()),
            other => Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "plugin '{plugin_id}' runtime status is {} (expected \"loaded\"); diagnostics: {}",
                    other
                        .map(|s| format!("\"{s}\""))
                        .unwrap_or_else(|| "absent".to_string()),
                    inspect_diagnostics(&output)
                ),
            }),
        }
    }
}

/// Merge a command's stdout and stderr into one searchable string. Help and
/// version output land on either stream across CLI implementations.
fn combine_output(output: &CliOutput) -> String {
    let mut s = output.stdout.clone();
    if !output.stderr.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(&output.stderr);
    }
    s
}

/// Compose a compact diagnostics string from an inspect command's output,
/// preserving the framework's own messages for the operator.
fn inspect_diagnostics(output: &CliOutput) -> String {
    let mut parts = Vec::new();
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        parts.push(stdout.to_string());
    }
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        parts.push(format!("stderr: {stderr}"));
    }
    if output.timed_out {
        parts.push("timed out".to_string());
    }
    if parts.is_empty() {
        "<no output>".to_string()
    } else {
        parts.join("; ")
    }
}

/// Parse a JSON object from `stdout`, tolerating legacy diagnostic lines
/// printed before it. Tries the whole trimmed output first, then each `{`
/// boundary in turn — OpenClaw prints the JSON last, so the first prefix
/// that parses cleanly is the intended value.
fn extract_trailing_json(stdout: &str) -> Option<serde_json::Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(value);
    }
    for (idx, _) in stdout.char_indices().filter(|&(_, c)| c == '{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout[idx..].trim()) {
            return Some(value);
        }
    }
    None
}

/// Validate prepared config indices before the first mutation.
fn validate_prepared_config_indices(
    indices: &[usize],
    ctx: &DriverCtx,
) -> Result<(), AdapterError> {
    let mut previous = None;
    for &index in indices {
        if index >= ctx.declared_config.len() {
            return Err(prepared_state_mismatch(&format!(
                "config index {index} is outside the declared config"
            )));
        }
        if previous.is_some_and(|value| value >= index) {
            return Err(prepared_state_mismatch(
                "selected config indices must be unique and in manifest order",
            ));
        }
        previous = Some(index);
    }
    Ok(())
}

/// Validate the OpenClaw payload's applied-resource references and the
/// write-ahead state of every config resource.
fn validate_config_claim_state(claim: &AdapterClaim) -> Result<(), AdapterError> {
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        return Err(invalid_config_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };

    let mut applied_ids = HashSet::new();
    for resource_id in &payload.config_resources {
        if !applied_ids.insert(resource_id.as_str()) {
            return Err(invalid_config_claim(
                claim,
                &format!("applied config resource '{resource_id}' is referenced more than once"),
            ));
        }
    }

    let mut resource_ids = HashSet::new();
    let mut config_keys = HashSet::new();
    let mut has_pending = false;
    for resource in &claim.resources {
        if !resource_ids.insert(resource.id.as_str()) {
            return Err(invalid_config_claim(
                claim,
                &format!("resource id '{}' is duplicated", resource.id),
            ));
        }
        let ClaimResourceKind::FrameworkConfig { key, state, .. } = &resource.kind else {
            continue;
        };
        if !config_keys.insert(key.as_str()) {
            return Err(invalid_config_claim(
                claim,
                &format!("config key '{key}' has more than one receipt resource"),
            ));
        }
        let referenced_as_applied = applied_ids.contains(resource.id.as_str());
        match (*state, referenced_as_applied) {
            (ConfigApplyState::Applied, true) | (ConfigApplyState::Pending, false) => {}
            (ConfigApplyState::Applied, false) => {
                return Err(invalid_config_claim(
                    claim,
                    &format!(
                        "applied config resource '{}' is missing from the OpenClaw payload",
                        resource.id
                    ),
                ));
            }
            (ConfigApplyState::Pending, true) => {
                return Err(invalid_config_claim(
                    claim,
                    &format!(
                        "pending config resource '{}' is listed as applied",
                        resource.id
                    ),
                ));
            }
        }
        has_pending |= *state == ConfigApplyState::Pending;
    }

    for resource_id in applied_ids {
        match claim.resource(resource_id).map(|resource| &resource.kind) {
            Some(ClaimResourceKind::FrameworkConfig {
                state: ConfigApplyState::Applied,
                ..
            }) => {}
            Some(_) => {
                return Err(invalid_config_claim(
                    claim,
                    &format!(
                        "applied config resource '{resource_id}' does not reference confirmed config"
                    ),
                ));
            }
            None => {
                return Err(invalid_config_claim(
                    claim,
                    &format!("applied config resource '{resource_id}' does not exist"),
                ));
            }
        }
    }
    if has_pending && claim.status != ClaimStatus::CleanupFailed {
        return Err(invalid_config_claim(
            claim,
            "pending config resources require cleanup_failed receipt status",
        ));
    }
    Ok(())
}

/// Ensure every uncertain prior config can be replayed by this enable.
///
/// A removed or version-incompatible key cannot be confirmed or superseded.
/// Reject it before another framework mutation so enable never reports success
/// while retaining an indefinitely pending receipt.
fn validate_pending_config_selection(
    claim: &AdapterClaim,
    selected_indices: &[usize],
    ctx: &DriverCtx,
) -> Result<(), AdapterError> {
    let selected_keys: HashSet<&str> = selected_indices
        .iter()
        .map(|&index| ctx.declared_config[index].key.as_str())
        .collect();
    let unmatched_keys: Vec<&str> = claim
        .resources
        .iter()
        .filter_map(|resource| match &resource.kind {
            ClaimResourceKind::FrameworkConfig {
                key,
                state: ConfigApplyState::Pending,
                ..
            } if !selected_keys.contains(key.as_str()) => Some(key.as_str()),
            _ => None,
        })
        .collect();
    if unmatched_keys.is_empty() {
        return Ok(());
    }

    Err(AdapterError::InvalidAdapterInput {
        component: claim.component.clone(),
        framework: claim.framework.clone(),
        reason: format!(
            "pending OpenClaw config key(s) [{}] are not selected by the current manifest and \
             host version; restore matching config entries and re-run enable to reconcile them, \
             or disable the adapter to acknowledge they are left in place",
            unmatched_keys.join(", ")
        ),
    })
}

/// Carry confirmed and uncertain config facts into a replacement receipt so a
/// re-enable failure cannot erase host state that the new attempt did not
/// supersede.
fn preserve_openclaw_config_facts(
    prior: &AdapterClaim,
    next: &mut AdapterClaim,
) -> Result<(), AdapterError> {
    validate_config_claim_state(prior)?;
    validate_config_claim_state(next)?;

    let prior_resources: Vec<ClaimResource> = prior
        .resources
        .iter()
        .filter(|resource| matches!(resource.kind, ClaimResourceKind::FrameworkConfig { .. }))
        .cloned()
        .collect();
    for resource in prior_resources {
        match next.resource(&resource.id) {
            Some(existing) if existing == &resource => {}
            Some(_) => {
                return Err(invalid_config_claim(
                    next,
                    &format!(
                        "prior config resource id '{}' collides with the replacement receipt",
                        resource.id
                    ),
                ));
            }
            None => next.resources.push(resource),
        }
    }

    let DriverPayload::OpenClaw(prior_payload) = &prior.driver_payload else {
        return Err(invalid_config_claim(
            prior,
            "receipt payload is not OpenClaw",
        ));
    };
    let DriverPayload::OpenClaw(next_payload) = &mut next.driver_payload else {
        return Err(invalid_config_claim(
            next,
            "receipt payload is not OpenClaw",
        ));
    };
    for resource_id in &prior_payload.config_resources {
        if !next_payload.config_resources.contains(resource_id) {
            next_payload.config_resources.push(resource_id.clone());
        }
    }
    if next.resources.iter().any(|resource| {
        matches!(
            resource.kind,
            ClaimResourceKind::FrameworkConfig {
                state: ConfigApplyState::Pending,
                ..
            }
        )
    }) {
        next.status = ClaimStatus::CleanupFailed;
    }
    validate_config_claim_state(next)
}

/// Ensure one selected config key has a durable typed intent resource.
///
/// Existing matching applied or pending resources are reused, which keeps
/// repeated apply idempotent. When a manifest index collides with a preserved
/// resource for another key, a deterministic numeric suffix avoids erasing the
/// prior fact.
fn ensure_config_intent(
    claim: &mut AdapterClaim,
    index: usize,
    config: &AdapterConfigSetSpec,
) -> Result<String, AdapterError> {
    validate_config_claim_state(claim)?;

    let existing_id = claim.resources.iter().find_map(|resource| {
        matches!(
            &resource.kind,
            ClaimResourceKind::FrameworkConfig { framework, key, .. }
                if framework == &claim.framework && key == &config.key
        )
        .then(|| resource.id.clone())
    });

    let resource_id = match existing_id {
        Some(resource_id) => resource_id,
        None => {
            let base_id = format!("openclaw_config_{index}");
            let resource_id = if claim.resource(&base_id).is_none() {
                base_id
            } else {
                let mut suffix = 1usize;
                loop {
                    let candidate = format!("{base_id}_{suffix}");
                    if claim.resource(&candidate).is_none() {
                        break candidate;
                    }
                    suffix += 1;
                }
            };
            let framework = claim.framework.clone();
            claim.resources.push(ClaimResource {
                id: resource_id.clone(),
                purpose: "openclaw_config".to_string(),
                kind: ClaimResourceKind::FrameworkConfig {
                    framework,
                    key: config.key.clone(),
                    state: ConfigApplyState::Pending,
                },
            });
            resource_id
        }
    };

    let Some(resource) = claim
        .resources
        .iter_mut()
        .find(|resource| resource.id == resource_id)
    else {
        return Err(invalid_config_claim(
            claim,
            &format!("config resource '{resource_id}' disappeared before apply"),
        ));
    };
    match &mut resource.kind {
        ClaimResourceKind::FrameworkConfig { state, .. } => {
            *state = ConfigApplyState::Pending;
        }
        _ => {
            return Err(invalid_config_claim(
                claim,
                &format!("config resource '{resource_id}' is not framework config"),
            ));
        }
    }
    let DriverPayload::OpenClaw(payload) = &mut claim.driver_payload else {
        return Err(invalid_config_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };
    payload
        .config_resources
        .retain(|existing| existing != &resource_id);
    claim.status = ClaimStatus::CleanupFailed;
    validate_config_claim_state(claim)?;
    Ok(resource_id)
}

/// Promote a matching pending resource to a confirmed applied fact.
fn confirm_config_applied(
    claim: &mut AdapterClaim,
    resource_id: &str,
    config: &AdapterConfigSetSpec,
) -> Result<(), AdapterError> {
    let framework = claim.framework.clone();
    let Some(resource_index) = claim
        .resources
        .iter()
        .position(|resource| resource.id == resource_id)
    else {
        return Err(invalid_config_claim(
            claim,
            &format!("config resource '{resource_id}' disappeared during apply"),
        ));
    };
    let resource = &mut claim.resources[resource_index];
    match &mut resource.kind {
        ClaimResourceKind::FrameworkConfig {
            framework: resource_framework,
            key,
            state,
        } if resource_framework == &framework && key == &config.key => {
            *state = ConfigApplyState::Applied;
        }
        _ => {
            return Err(invalid_config_claim(
                claim,
                &format!(
                    "config resource '{resource_id}' does not match '{}'",
                    config.key
                ),
            ));
        }
    }

    let DriverPayload::OpenClaw(payload) = &mut claim.driver_payload else {
        return Err(invalid_config_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };
    if !payload
        .config_resources
        .iter()
        .any(|existing| existing == resource_id)
    {
        payload.config_resources.push(resource_id.to_string());
    }
    claim.status = if claim.resources.iter().any(|resource| {
        matches!(
            resource.kind,
            ClaimResourceKind::FrameworkConfig {
                state: ConfigApplyState::Pending,
                ..
            }
        )
    }) {
        ClaimStatus::CleanupFailed
    } else {
        ClaimStatus::Enabled
    };
    validate_config_claim_state(claim)?;
    Ok(())
}

fn invalid_config_claim(claim: &AdapterClaim, reason: &str) -> AdapterError {
    AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: format!("invalid OpenClaw config receipt: {reason}"),
    }
}

/// Whether an install command's output looks like an OpenClaw plugin-safety
/// rejection (as opposed to a generic failure). Used only to decide whether
/// to surface the explicit-retry hint — never to auto-retry.
fn install_output_looks_like_safety_rejection(output: &CliOutput) -> bool {
    let haystack = format!("{}\n{}", output.stdout, output.stderr).to_lowercase();
    ["unsafe", "safety", "dangerous"]
        .iter()
        .any(|marker| haystack.contains(marker))
}

fn install_output_requires_capability_consent(output: &CliOutput) -> bool {
    format!(
        "{}\n{}",
        strip_ansi(&output.stdout),
        strip_ansi(&output.stderr)
    )
    .to_ascii_lowercase()
    .contains("capability consent")
}

// ---------------------------------------------------------------------------
// OpenClaw version parsing and comparison
// ---------------------------------------------------------------------------

/// A parsed OpenClaw version.
///
/// OpenClaw ships calendar-style versions (`2026.4.14`) that may carry a
/// purely-numeric *correction* suffix (`2026.5.3-1`, a rebuild of the same
/// release), an alphabetic prerelease (`2026.4.14-beta.1`, `-rc.2`), and/or
/// `+build` metadata (ignored for ordering).
///
/// The deliberate departure from plain semver: a numeric-only suffix is a
/// correction that sorts **above** the base version, not a prerelease that
/// sorts below. Comparing with a stock semver `Version` would wrongly rank
/// `2026.5.3-1 < 2026.5.3`, so this type implements its own ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenClawVersion {
    core: [u64; 3],
    suffix: VersionSuffix,
}

/// The suffix of an [`OpenClawVersion`], ranked prerelease < release <
/// correction.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionSuffix {
    /// Alphabetic prerelease identifiers (`beta.1`); sorts below the release.
    Prerelease(Vec<PreId>),
    /// No suffix.
    Release,
    /// Numeric-only correction identifiers (`1`, `2.1`); sorts above release.
    Correction(Vec<u64>),
}

/// One prerelease identifier, numeric or textual, compared semver-style.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreId {
    /// Purely-numeric identifier, compared by value.
    Num(u64),
    /// Textual identifier, compared lexically.
    Text(String),
}

impl OpenClawVersion {
    /// Parse an OpenClaw version string. Returns `None` for any malformed
    /// input — a non-numeric or >3-component core, an empty or invalid
    /// `-suffix`, or empty/invalid `+build` metadata — so a bad constraint
    /// like `>=2026.4.14-` is rejected rather than silently treated as
    /// `>=2026.4.14`. Build metadata is validated for well-formedness, then
    /// discarded (it does not affect identity or ordering).
    fn parse(input: &str) -> Option<OpenClawVersion> {
        let s = input.trim();
        if s.is_empty() {
            return None;
        }
        // Separate optional `+build`; validate then drop it. An empty or
        // invalid build metadata segment is a malformed version.
        let s = match s.split_once('+') {
            Some((before, build)) => {
                if !is_valid_dot_identifiers(build) {
                    return None;
                }
                before
            }
            None => s,
        };
        // Separate optional `-suffix`. A trailing `-` with no suffix is
        // malformed, not a plain release.
        let (core_str, pre_str) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };

        let mut core = [0u64; 3];
        let mut count = 0usize;
        for (i, part) in core_str.split('.').enumerate() {
            if i >= 3 {
                return None;
            }
            core[i] = part.parse().ok()?;
            count = i + 1;
        }
        if count == 0 {
            return None;
        }

        let suffix = match pre_str {
            None => VersionSuffix::Release,
            Some(pre) => {
                // Every identifier must be non-empty and drawn from the
                // semver alphabet (`[0-9A-Za-z-]`); an empty `-` or an illegal
                // character is rejected rather than accepted as text.
                if !is_valid_dot_identifiers(pre) {
                    return None;
                }
                let ids: Vec<&str> = pre.split('.').collect();
                let all_numeric = ids.iter().all(|id| id.bytes().all(|b| b.is_ascii_digit()));
                if all_numeric {
                    let nums = ids
                        .iter()
                        .map(|id| id.parse::<u64>().ok())
                        .collect::<Option<Vec<_>>>()?;
                    VersionSuffix::Correction(nums)
                } else {
                    let parsed = ids
                        .iter()
                        .map(|id| {
                            if id.bytes().all(|b| b.is_ascii_digit()) {
                                id.parse::<u64>().ok().map(PreId::Num)
                            } else {
                                Some(PreId::Text((*id).to_string()))
                            }
                        })
                        .collect::<Option<Vec<_>>>()?;
                    VersionSuffix::Prerelease(parsed)
                }
            }
        };
        Some(OpenClawVersion { core, suffix })
    }
}

/// Whether `s` is one or more dot-separated identifiers, each non-empty and
/// composed only of ASCII alphanumerics and `-` (the semver
/// prerelease/build alphabet). Rejects empty input and empty identifiers
/// (leading/trailing/double dots).
fn is_valid_dot_identifiers(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.split('.')
        .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

impl std::fmt::Display for OpenClawVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.core[0], self.core[1], self.core[2])?;
        match &self.suffix {
            VersionSuffix::Release => Ok(()),
            VersionSuffix::Correction(nums) => {
                let joined = nums
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(".");
                write!(f, "-{joined}")
            }
            VersionSuffix::Prerelease(ids) => {
                let joined = ids
                    .iter()
                    .map(|id| match id {
                        PreId::Num(n) => n.to_string(),
                        PreId::Text(t) => t.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                write!(f, "-{joined}")
            }
        }
    }
}

impl Ord for OpenClawVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| self.suffix.cmp(&other.suffix))
    }
}

impl PartialOrd for OpenClawVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl VersionSuffix {
    /// Ordering rank: prerelease (0) < release (1) < correction (2).
    fn rank(&self) -> u8 {
        match self {
            VersionSuffix::Prerelease(_) => 0,
            VersionSuffix::Release => 1,
            VersionSuffix::Correction(_) => 2,
        }
    }
}

impl Ord for VersionSuffix {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (VersionSuffix::Prerelease(a), VersionSuffix::Prerelease(b)) => cmp_pre_ids(a, b),
            (VersionSuffix::Correction(a), VersionSuffix::Correction(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for VersionSuffix {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compare prerelease identifier lists semver-style: numeric identifiers
/// compare by value, numeric sorts below textual, textual compares lexically,
/// and a shorter list sorts below a longer one when the shared prefix is
/// equal.
fn cmp_pre_ids(a: &[PreId], b: &[PreId]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = match (x, y) {
            (PreId::Num(m), PreId::Num(n)) => m.cmp(n),
            (PreId::Text(m), PreId::Text(n)) => m.cmp(n),
            (PreId::Num(_), PreId::Text(_)) => Ordering::Less,
            (PreId::Text(_), PreId::Num(_)) => Ordering::Greater,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// Comparison operators supported in an OpenClaw version requirement.
#[derive(Debug, Clone, Copy)]
enum ReqOp {
    /// `>=`
    Gte,
    /// `>`
    Gt,
    /// `<=`
    Lte,
    /// `<`
    Lt,
    /// `=` / `==`
    Eq,
}

/// Split a single requirement clause into its operator and version text. A
/// bare version (no operator) is treated as a minimum (`>=`), matching how
/// these fields express a lowest supported framework version.
fn split_req_clause(clause: &str) -> (ReqOp, &str) {
    const OPS: [(&str, ReqOp); 6] = [
        (">=", ReqOp::Gte),
        ("<=", ReqOp::Lte),
        ("==", ReqOp::Eq),
        (">", ReqOp::Gt),
        ("<", ReqOp::Lt),
        ("=", ReqOp::Eq),
    ];
    for (prefix, op) in OPS {
        if let Some(rest) = clause.strip_prefix(prefix) {
            return (op, rest.trim());
        }
    }
    (ReqOp::Gte, clause)
}

/// Evaluate a comma-separated requirement (`>=2026.4.14, <2027.0.0`) against
/// a version. All clauses must hold.
///
/// # Errors
///
/// Returns `Err(reason)` when the requirement is empty, contains an empty
/// clause (a leading/trailing/double comma), or a clause's version cannot be
/// parsed — all manifest bugs, distinct from a non-match.
fn openclaw_version_req_satisfied(req: &str, version: &OpenClawVersion) -> Result<bool, String> {
    if req.trim().is_empty() {
        return Err(format!("empty version requirement '{req}'"));
    }
    let mut clauses = Vec::new();
    for clause in req.split(',') {
        let clause = clause.trim();
        // An empty clause (from `>=X,,<Y` or a trailing/leading comma) is a
        // malformed requirement, not something to silently skip.
        if clause.is_empty() {
            return Err(format!("empty clause in version requirement '{req}'"));
        }
        let (op, ver_str) = split_req_clause(clause);
        let required = OpenClawVersion::parse(ver_str)
            .ok_or_else(|| format!("unparseable version '{ver_str}' in requirement '{req}'"))?;
        clauses.push((op, required));
    }

    Ok(clauses.into_iter().all(|(op, required)| {
        let ord = version.cmp(&required);
        match op {
            ReqOp::Gte => ord != Ordering::Less,
            ReqOp::Gt => ord == Ordering::Greater,
            ReqOp::Lte => ord != Ordering::Greater,
            ReqOp::Lt => ord == Ordering::Less,
            ReqOp::Eq => ord == Ordering::Equal,
        }
    }))
}

/// Extract an [`OpenClawVersion`] from `openclaw --version` output.
///
/// Only an unambiguous version line is accepted: either a bare calendar-shaped
/// version or a version following an explicit `OpenClaw`, `OpenClaw version`,
/// or `OpenClaw CLI version` label. Calendar-shaped tokens embedded in warning
/// or diagnostic lines are ignored. Zero or multiple candidates return `None`
/// so callers fail closed instead of guessing which number belongs to the CLI.
fn parse_openclaw_version_output(output: &str) -> Option<OpenClawVersion> {
    let candidates: Vec<OpenClawVersion> = output
        .lines()
        .filter_map(parse_openclaw_version_line)
        .collect();
    if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }
}

fn parse_openclaw_version_line(line: &str) -> Option<OpenClawVersion> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let (version_token, trailing) = match tokens.as_slice() {
        [version] => (*version, &[][..]),
        [label, cli, keyword, version, trailing @ ..]
            if is_openclaw_label(label)
                && cli.eq_ignore_ascii_case("cli")
                && keyword.eq_ignore_ascii_case("version") =>
        {
            (*version, trailing)
        }
        [label, keyword, version, trailing @ ..]
            if is_openclaw_label(label) && keyword.eq_ignore_ascii_case("version") =>
        {
            (*version, trailing)
        }
        [label, version, trailing @ ..] if is_openclaw_label(label) => (*version, trailing),
        _ => return None,
    };
    if !trailing_version_annotation_is_known(trailing) {
        return None;
    }
    let version_token = version_token
        .strip_prefix('v')
        .or_else(|| version_token.strip_prefix('V'))
        .unwrap_or(version_token);
    is_calendar_shaped_token(version_token)
        .then(|| OpenClawVersion::parse(version_token))
        .flatten()
}

fn is_openclaw_label(token: &str) -> bool {
    token.trim_end_matches(':').eq_ignore_ascii_case("openclaw")
}

fn trailing_version_annotation_is_known(tokens: &[&str]) -> bool {
    tokens.is_empty()
        || (tokens.first().is_some_and(|token| token.starts_with('('))
            && tokens.last().is_some_and(|token| token.ends_with(')')))
}

/// Whether `token` has an OpenClaw calendar-shaped core: exactly three
/// dot-separated numeric components with a 4-digit leading year, ignoring any
/// `-`/`+` suffix. `2026.4.14` and `2026.5.3-1` pass; `22.14.0` and `2026.4`
/// (a short version, valid only in a *requirement*) do not.
fn is_calendar_shaped_token(token: &str) -> bool {
    let core = token.split(['-', '+']).next().unwrap_or(token);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let year_ok = parts[0].len() == 4 && parts[0].bytes().all(|b| b.is_ascii_digit());
    year_ok
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

// ---------------------------------------------------------------------------
// Pure helpers (no spawning) — unit-testable
// ---------------------------------------------------------------------------

/// `OPENCLAW_BIN` override, else `openclaw`.
fn openclaw_bin() -> String {
    std::env::var("OPENCLAW_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "openclaw".to_string())
}

/// Resolve the OpenClaw state directory using the runtime's whitespace,
/// tilde-expansion, and absolute-path semantics. `OPENCLAW_HOME` remains a
/// direct state-directory fallback for compatibility with earlier releases.
fn openclaw_home(user_home: Option<&Path>) -> Option<PathBuf> {
    let home_override = std::env::var_os("OPENCLAW_HOME")
        .and_then(|value| resolve_openclaw_path(&value, user_home));
    let tilde_home = home_override.as_deref().or(user_home);

    if let Some(state_dir) = std::env::var_os("OPENCLAW_STATE_DIR")
        && let Some(state_dir) = resolve_openclaw_path(&state_dir, tilde_home)
    {
        return Some(state_dir);
    }

    home_override
        .or_else(|| user_home.and_then(|home| absolute_normalized_path(home.join(".openclaw"))))
}

/// External roots accepted for current receipts plus the exact root the
/// pre-fix resolver could have persisted. The compatibility root is rebuilt
/// from trusted process/user context, never from receipt contents.
fn openclaw_allowed_roots(user_home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(current) = openclaw_home(user_home) {
        roots.push(current);
    }
    if let Some(legacy) = legacy_openclaw_home(user_home)
        && !roots.contains(&legacy)
    {
        roots.push(legacy);
    }
    roots
}

/// Reproduce the old resolver only to validate receipts it created. New CLI
/// operations and receipts always use [`openclaw_home`].
fn legacy_openclaw_home(user_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("OPENCLAW_HOME") {
        let home = home.to_string_lossy();
        let trimmed = home.trim_end_matches('/');
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    user_home.map(|h| h.join(".openclaw"))
}

/// Match OpenClaw's `resolveUserPath`: trim, expand a leading `~`, resolve
/// relative paths from the current directory, and normalize lexically.
fn resolve_openclaw_path(value: &OsStr, tilde_home: Option<&Path>) -> Option<PathBuf> {
    let value = value.to_string_lossy();
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let path = if value == "~" {
        tilde_home
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())?
    } else if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        tilde_home
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())?
            .join(rest)
    } else {
        PathBuf::from(value)
    };
    absolute_normalized_path(path)
}

fn absolute_normalized_path(path: PathBuf) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    Some(normalize_lexically(&absolute))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// PATH prefix dirs, mirroring `install.sh`:
/// `<user_home>/.local/bin`, `<home>/bin`, `/usr/local/bin`.
fn path_prepend(home: &Path, user_home: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(uh) = user_home {
        v.push(uh.join(".local/bin"));
    }
    v.push(home.join("bin"));
    v.push(PathBuf::from("/usr/local/bin"));
    v
}

/// Keep every OpenClaw invocation on the resolved state directory while
/// suppressing the legacy home override.
fn base_cmd(args: Vec<String>, home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    FrameworkCommand {
        program: openclaw_bin(),
        args,
        stdin: None,
        env_set: vec![(
            "OPENCLAW_STATE_DIR".to_string(),
            home.to_string_lossy().into_owned(),
        )],
        env_remove: vec!["OPENCLAW_HOME".to_string()],
        path_prepend: path_prepend(home, user_home),
        timeout: CLI_TIMEOUT,
    }
}

/// Build a host-compatible `openclaw plugins install` command.
///
/// `--force` is a required capability of the driver contract; if the host's
/// install help does not expose it, this fails before any mutation. The
/// unsafe flag is appended only when the caller both authorized it
/// (`allow_unsafe`) and the host's help describes it as effective. An
/// authorized request fails when the option is absent or advertised as a
/// deprecated no-op, and a normal install never carries it.
/// Enabling a plugin accepts its declared capabilities, so the consent flag
/// is included whenever the installer advertises it.
///
/// # Errors
///
/// [`AdapterError::FrameworkCli`] when `--force` is unsupported, or when an
/// authorized unsafe install is requested but the host does not expose an
/// effective unsafe flag.
fn build_install_cmd(
    resource_root: &Path,
    home: &Path,
    user_home: Option<&Path>,
    profile: &OpenClawHostProfile,
    allow_unsafe: bool,
) -> Result<FrameworkCommand, AdapterError> {
    if !profile.supports_install_force {
        return Err(AdapterError::FrameworkCli {
            program: openclaw_bin(),
            reason: "`openclaw plugins install --help` does not expose the required \
                     --force flag; cannot install non-interactively"
                .to_string(),
        });
    }
    if allow_unsafe {
        match profile.unsafe_install_support {
            UnsafeInstallSupport::Effective => {}
            UnsafeInstallSupport::Unsupported => {
                return Err(AdapterError::FrameworkCli {
                    program: openclaw_bin(),
                    reason: "unsafe plugin install was explicitly authorized but this openclaw \
                             does not expose --dangerously-force-unsafe-install"
                        .to_string(),
                });
            }
            UnsafeInstallSupport::DeprecatedNoOp => {
                return Err(AdapterError::FrameworkCli {
                    program: openclaw_bin(),
                    reason: "unsafe plugin install was explicitly authorized, but this openclaw \
                             advertises --dangerously-force-unsafe-install as a deprecated no-op; \
                             configure the operator-owned security.installPolicy instead"
                        .to_string(),
                });
            }
        }
    }
    Ok(base_cmd(
        install_argv(
            resource_root,
            allow_unsafe,
            profile.supports_accept_capabilities,
        ),
        home,
        user_home,
    ))
}

/// Build the `plugins install <root> --force [--dangerously-force-unsafe-install]`
/// argv, accepting declared capabilities when the host supports consent.
/// `--force` is always present (a required capability); the unsafe flag
/// is appended iff `allow_unsafe`. Capability support is the caller's concern
/// ([`build_install_cmd`] verifies it during preflight); `apply_enable` builds
/// this directly from the authorized decision without re-probing.
fn install_argv(
    resource_root: &Path,
    allow_unsafe: bool,
    accept_capabilities: bool,
) -> Vec<String> {
    let mut args = vec![
        "plugins".to_string(),
        "install".to_string(),
        resource_root.to_string_lossy().into_owned(),
        "--force".to_string(),
    ];
    if allow_unsafe {
        args.push("--dangerously-force-unsafe-install".to_string());
    }
    if accept_capabilities {
        args.push("--accept-capabilities".to_string());
    }
    args
}

/// Build explicit activation, or its read-only `--help` probe.
fn build_enable_cmd(
    plugin_id: &str,
    home: &Path,
    user_home: Option<&Path>,
    accept_capabilities: bool,
) -> FrameworkCommand {
    let mut args = vec![
        "plugins".to_string(),
        "enable".to_string(),
        plugin_id.to_string(),
    ];
    if accept_capabilities {
        args.push("--accept-capabilities".to_string());
    }
    base_cmd(args, home, user_home)
}

/// Whether `help` lists `flag` as a standalone option token — not merely as a
/// prefix of a longer flag. A flag token continues through any character that
/// can appear inside an option name (ASCII alphanumeric, `-`, `_`, `.`), so a
/// near-miss like `--force-color`, `--json-file`, `--json_file`, or
/// `--runtime.mode` stays a single token and never matches `--force`/`--json`/
/// `--runtime`, while genuine boundaries (`--json`, `--json=<path>`,
/// `--json,`) do.
fn help_lists_flag(help: &str, flag: &str) -> bool {
    help.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .any(|token| token == flag)
}

/// Classify the unsafe option by its advertised help semantics. Current
/// OpenClaw releases retain the token as a deprecated no-op, so token presence
/// alone must not authorize an ineffective bypass or produce invalid retry
/// advice.
fn unsafe_install_support(help: &str) -> UnsafeInstallSupport {
    const FLAG: &str = "--dangerously-force-unsafe-install";
    let Some(option_line) = help.lines().find(|line| help_lists_flag(line, FLAG)) else {
        return UnsafeInstallSupport::Unsupported;
    };
    let description = option_line.to_ascii_lowercase();
    if description.contains("no-op") || description.contains("no op") {
        UnsafeInstallSupport::DeprecatedNoOp
    } else {
        UnsafeInstallSupport::Effective
    }
}

/// Build the read-only `openclaw --version` probe.
fn build_version_cmd(home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(vec!["--version".to_string()], home, user_home)
}

/// Build the read-only `openclaw plugins install --help` probe.
fn build_install_help_cmd(home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec![
            "plugins".to_string(),
            "install".to_string(),
            "--help".to_string(),
        ],
        home,
        user_home,
    )
}

/// Build the read-only `openclaw plugins inspect --help` probe.
fn build_inspect_help_cmd(home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec![
            "plugins".to_string(),
            "inspect".to_string(),
            "--help".to_string(),
        ],
        home,
        user_home,
    )
}

/// Build `openclaw plugins inspect <plugin_id> [--runtime] --json` for
/// post-install runtime verification. `--runtime` is included only when the
/// host's inspect help exposes it.
fn build_inspect_cmd(
    plugin_id: &str,
    home: &Path,
    user_home: Option<&Path>,
    with_runtime: bool,
) -> FrameworkCommand {
    let mut args = vec![
        "plugins".to_string(),
        "inspect".to_string(),
        plugin_id.to_string(),
    ];
    if with_runtime {
        args.push("--runtime".to_string());
    }
    args.push("--json".to_string());
    base_cmd(args, home, user_home)
}

/// Build `openclaw plugins uninstall <plugin_id> --force`.
///
/// `--force` skips OpenClaw's interactive confirmation — ANOLISA drives
/// the CLI non-interactively. `plugin_id` is validated by the caller.
fn build_uninstall_cmd(plugin_id: &str, home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec![
            "plugins".to_string(),
            "uninstall".to_string(),
            plugin_id.to_string(),
            "--force".to_string(),
        ],
        home,
        user_home,
    )
}

/// Build the read-only `openclaw plugins list`.
fn build_list_cmd(home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec!["plugins".to_string(), "list".to_string()],
        home,
        user_home,
    )
}

/// Plugin id declared by the OpenClaw-native plugin manifest, when present.
fn read_plugin_manifest_id(root: &Path, filename: &str) -> Result<Option<String>, AdapterError> {
    #[derive(serde::Deserialize)]
    struct PluginManifest {
        id: Option<String>,
    }

    let path = root.join(filename);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(AdapterError::Io { path, source }),
    };
    let manifest: PluginManifest =
        serde_json::from_slice(&bytes).map_err(|source| AdapterError::BundleInvalid {
            root: root.to_path_buf(),
            reason: format!(
                "failed to parse {} as OpenClaw plugin manifest: {source}",
                path.display()
            ),
        })?;
    let id =
        manifest
            .id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AdapterError::BundleInvalid {
                root: root.to_path_buf(),
                reason: format!("{} does not declare a non-empty id", path.display()),
            })?;
    Ok(Some(id))
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

/// True when `plugin_id` appears in the `plugins list` output.
///
/// Handles three output shapes:
/// 1. Plain text — each line has whitespace-delimited tokens.
/// 2. Rich table without wrapping — tokens appear between │ delimiters.
/// 3. Rich table with wrapping — a cell value is split across
///    consecutive physical lines within the same column.
///
/// ANSI escape codes are stripped before any matching.
fn list_contains_plugin(stdout: &str, plugin_id: &str) -> bool {
    let stripped = strip_ansi(stdout);

    // Fast path: exact whitespace-token match on lines that are NOT
    // table data lines. Table data lines (containing │/┃/║) must go
    // through the table parser, because a wrapped cell fragment can
    // look like a complete token on a single physical line.
    if stripped.lines().any(|line| {
        !line.contains(|c: char| is_cell_delimiter(c))
            && line.split_whitespace().any(|tok| tok == plugin_id)
    }) {
        return true;
    }

    // Table-aware path: parse rows, concatenate wrapped cell text per
    // column, then search each concatenated cell.
    table_contains_token(&stripped, plugin_id)
}

/// Strip ANSI escape sequences (CSI and OSC) from `s`.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: consume until a final byte (0x40..=0x7E).
                    for c in chars.by_ref() {
                        if matches!(c, '\x40'..='\x7e') {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: consume until BEL or ST.
                    for c in chars.by_ref() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn is_cell_delimiter(c: char) -> bool {
    matches!(c, '│' | '┃' | '║')
}

fn is_border_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|c| {
            is_cell_delimiter(c)
                || matches!(
                    c,
                    '─' | '━'
                        | '═'
                        | '┌'
                        | '┐'
                        | '└'
                        | '┘'
                        | '├'
                        | '┤'
                        | '┬'
                        | '┴'
                        | '┼'
                        | '┏'
                        | '┓'
                        | '┗'
                        | '┛'
                        | '┣'
                        | '┫'
                        | '┳'
                        | '┻'
                        | '╋'
                        | '┡'
                        | '┩'
                        | '╇'
                        | '╔'
                        | '╗'
                        | '╚'
                        | '╝'
                        | '╠'
                        | '╣'
                        | '╦'
                        | '╩'
                        | '╬'
                        | ' '
                )
        })
}

/// Extract cell text from a line delimited by │/┃/║. Returns `None`
/// when the line has no cell delimiters (not a table data line).
fn extract_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains(|c: char| is_cell_delimiter(c)) {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(|c: char| is_cell_delimiter(c)).collect();
    if parts.len() < 3 {
        return None;
    }
    // Skip the empty segments before the first and after the last │.
    let cells: Vec<String> = parts[1..parts.len() - 1]
        .iter()
        .map(|cell| cell.trim().to_string())
        .collect();
    Some(cells)
}

/// Parse rich-table output into logical rows (merging physical
/// continuation lines), then check whether any cell in any row
/// matches `token` as a whitespace-delimited word.
///
/// A continuation line is detected by the *last* column being empty.
/// In `plugins list` tables the last column is typically Status
/// (`enabled`/`disabled`), which is always populated on the first
/// physical line of a row but empty on continuation lines. This
/// correctly handles the case where *both* Name and ID wrap.
fn table_contains_token(text: &str, token: &str) -> bool {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current: Option<Vec<String>> = None;

    for line in text.lines() {
        if is_border_line(line) {
            if let Some(cells) = current.take() {
                rows.push(cells);
            }
            continue;
        }

        if let Some(cells) = extract_cells(line) {
            let is_continuation = current.is_some() && cells.last().is_some_and(|c| c.is_empty());

            if is_continuation {
                if let Some(cur) = current.as_mut() {
                    for (i, cell) in cells.into_iter().enumerate() {
                        if i < cur.len() && !cell.is_empty() {
                            cur[i].push_str(&cell);
                        }
                    }
                }
            } else {
                if let Some(prev) = current.take() {
                    rows.push(prev);
                }
                current = Some(cells);
            }
        }
    }
    if let Some(cells) = current {
        rows.push(cells);
    }

    rows.iter().any(|row| {
        row.iter().any(|cell| {
            let trimmed = cell.trim();
            trimmed == token || trimmed.split_whitespace().any(|t| t == token)
        })
    })
}

/// Extract the validated plugin id from a claim's `FrameworkPlugin`
/// resource, falling back to the top-level `plugin_id` field.
fn claim_plugin_id(claim: &AdapterClaim) -> Option<String> {
    for res in &claim.resources {
        if let ClaimResourceKind::FrameworkPlugin { plugin_id, .. } = &res.kind {
            return Some(plugin_id.clone());
        }
    }
    claim.plugin_id.clone()
}

/// Plugin id from a bundle, or [`AdapterError::BundleInvalid`] when none is
/// resolvable.
fn require_plugin_id(bundle: &AdapterBundle) -> Result<String, AdapterError> {
    bundle
        .plugin_id
        .clone()
        .ok_or_else(|| AdapterError::BundleInvalid {
            root: bundle.resource_root.clone(),
            reason: "no plugin id declared in manifest and none discoverable".to_string(),
        })
}

/// OpenClaw state directory, or [`AdapterError::FrameworkCli`] when no
/// explicit state/home override or user home is available.
fn require_home(ctx: &DriverCtx) -> Result<PathBuf, AdapterError> {
    openclaw_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
        program: openclaw_bin(),
        reason: "cannot resolve OpenClaw state directory (no OPENCLAW_STATE_DIR, OPENCLAW_HOME, or $HOME)"
            .to_string(),
    })
}

/// Fail-closed error for a `PreparedEnable` that does not match the adapter
/// being applied. Signals a driver-contract misuse (a caller handed the wrong
/// prepared state); raised before any mutation.
fn prepared_state_mismatch(reason: &str) -> AdapterError {
    AdapterError::FrameworkCli {
        program: openclaw_bin(),
        reason: format!("prepared enable state does not match the adapter: {reason}"),
    }
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

/// Compose a failure reason keeping the exit/timeout status **and both**
/// stdout and stderr.
///
/// Used on every enable-path command failure (probes, install, config set):
/// OpenClaw may report plugin-safety findings on stdout, and a timeout must
/// not drop stderr — the plain [`cli_failure_reason`] omits both. Both streams
/// are already bounded by the Manager's capture cap.
fn full_failure_reason(verb: &str, output: &super::driver::CliOutput) -> String {
    let mut reason = if output.timed_out {
        format!("'{verb}' timed out")
    } else {
        let code = output
            .status
            .map(|c| c.to_string())
            .unwrap_or_else(|| "killed".to_string());
        format!("'{verb}' exited with {code}")
    };
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        reason.push_str("; stderr: ");
        reason.push_str(stderr);
    }
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        reason.push_str("; stdout: ");
        reason.push_str(stdout);
    }
    reason
}

/// Map a bool to a [`ConditionStatus`] (`true`→`True`, `false`→`False`).
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

/// Build `openclaw config set <key> <value>`.
fn build_config_set_cmd(
    key: &str,
    value: &toml::Value,
    home: &Path,
    user_home: Option<&Path>,
) -> FrameworkCommand {
    base_cmd(
        vec![
            "config".to_string(),
            "set".to_string(),
            key.to_string(),
            config_value_to_cli_string(value),
        ],
        home,
        user_home,
    )
}

/// Convert a TOML value to a string suitable for the `openclaw config set`
/// CLI argument. Strings are passed bare (no quotes); other types use the
/// TOML display representation.
fn config_value_to_cli_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Human-readable display of a config value for plan output.
fn config_value_display(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

/// Resolve the state directory from a manager-validated OpenClaw receipt.
fn claim_state_dir(claim: &AdapterClaim) -> Result<PathBuf, AdapterError> {
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        return Err(invalid_state_dir_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };
    let resource = claim.resource(&payload.state_dir_resource).ok_or_else(|| {
        invalid_state_dir_claim(
            claim,
            &format!(
                "state directory resource '{}' is missing",
                payload.state_dir_resource
            ),
        )
    })?;
    match &resource.kind {
        ClaimResourceKind::ExternalPath { path } => Ok(path.clone()),
        _ => Err(invalid_state_dir_claim(
            claim,
            &format!(
                "state directory resource '{}' is not an external path",
                payload.state_dir_resource
            ),
        )),
    }
}

fn invalid_state_dir_claim(claim: &AdapterClaim, reason: &str) -> AdapterError {
    AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: format!("invalid OpenClaw state directory receipt: {reason}"),
    }
}

/// True only for OpenClaw's exact idempotent-uninstall failure. Other
/// non-zero exits must keep the receipt so cleanup can be retried.
fn uninstall_reports_missing_plugin(output: &CliOutput, plugin_id: &str) -> bool {
    if output.timed_out {
        return false;
    }
    let expected = format!("plugin not found: {}", plugin_id.to_ascii_lowercase());
    let combined = format!(
        "{}\n{}",
        strip_ansi(&output.stdout),
        strip_ansi(&output.stderr)
    );
    let mut lines = combined
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_ascii_lowercase);
    matches!(lines.next(), Some(line) if line == expected) && lines.next().is_none()
}

fn uninstall_reports_untracked_plugin(output: &CliOutput, plugin_id: &str) -> bool {
    if output.timed_out {
        return false;
    }
    let expected = format!(
        "plugin \"{}\" is not associated with a tracked package install.",
        plugin_id.to_ascii_lowercase()
    );
    let combined = format!(
        "{}\n{}",
        strip_ansi(&output.stdout),
        strip_ansi(&output.stderr)
    );
    let mut lines = combined
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_ascii_lowercase);
    matches!(lines.next(), Some(line) if line == expected || line.starts_with(&format!("{expected} ")))
        && lines.next().is_none()
}

fn json_list_confirms_plugin_absent(output: &CliOutput, plugin_id: &str) -> bool {
    if !output.success() || !output.stderr.trim().is_empty() {
        return false;
    }
    // A text/table miss or a partial discovery report cannot authorize
    // deleting cleanup ownership. Require JSON IDs and clean diagnostics.
    let Ok(report) = serde_json::from_str::<serde_json::Value>(&output.stdout) else {
        return false;
    };
    let Some(plugins) = report.get("plugins").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let mut diagnostics = vec![report.get("diagnostics")];
    if let Some(registry) = report.get("registry") {
        diagnostics.push(registry.get("diagnostics"));
    }
    plugins.iter().all(|plugin| {
        plugin
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.is_empty() && id != plugin_id)
    }) && diagnostics.into_iter().all(|value| {
        value
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().all(|item| item["level"] == "info"))
    })
}

/// Extract skill names from a claim's `skill_resources` by parsing the
/// resource ids. Each id has the form `openclaw_skill_<name>`, and we
/// extract `<name>` as the directory name under `<home>/skills/`.
fn claim_skill_resources(claim: &AdapterClaim) -> Vec<String> {
    let payload = match &claim.driver_payload {
        DriverPayload::OpenClaw(oc) => oc,
        _ => return Vec::new(),
    };
    payload
        .skill_resources
        .iter()
        .filter_map(|id| id.strip_prefix("openclaw_skill_"))
        .map(str::to_string)
        .collect()
}

/// Count confirmed and uncertain OpenClaw config facts.
fn claim_config_counts(claim: &AdapterClaim) -> (usize, usize) {
    claim
        .resources
        .iter()
        .fold((0, 0), |(applied, pending), resource| {
            match &resource.kind {
                ClaimResourceKind::FrameworkConfig {
                    state: ConfigApplyState::Applied,
                    ..
                } => (applied + 1, pending),
                ClaimResourceKind::FrameworkConfig {
                    state: ConfigApplyState::Pending,
                    ..
                } => (applied, pending + 1),
                _ => (applied, pending),
            }
        })
}

/// ISO 8601 UTC timestamp, second precision.
fn now_iso8601() -> String {
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    static OPENCLAW_BIN_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct OpenClawBinEnvGuard {
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl OpenClawBinEnvGuard {
        fn unset() -> Self {
            Self::apply(None)
        }

        fn set(value: &str) -> Self {
            Self::apply(Some(value))
        }

        fn apply(value: Option<&str>) -> Self {
            let lock = OPENCLAW_BIN_ENV_LOCK.lock().expect("openclaw env lock");
            let previous = std::env::var_os("OPENCLAW_BIN");
            // SAFETY: these tests serialize every OPENCLAW_BIN mutation and
            // every command-builder read behind the same process-wide lock.
            unsafe {
                if let Some(value) = value {
                    std::env::set_var("OPENCLAW_BIN", value);
                } else {
                    std::env::remove_var("OPENCLAW_BIN");
                }
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for OpenClawBinEnvGuard {
        fn drop(&mut self) {
            // SAFETY: the lock is still held while restoring the process
            // environment, so no sibling OpenClaw test can observe a partial
            // OPENCLAW_BIN transition.
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var("OPENCLAW_BIN", previous);
                } else {
                    std::env::remove_var("OPENCLAW_BIN");
                }
            }
        }
    }

    #[test]
    fn list_contains_plugin_matches_whole_token() {
        assert!(list_contains_plugin("tokenless\nother\n", "tokenless"));
        assert!(list_contains_plugin("- tokenless (v1.2)\n", "tokenless"));
        assert!(!list_contains_plugin("tokenless-extra\n", "tokenless"));
        assert!(!list_contains_plugin("", "tokenless"));
    }

    #[test]
    fn list_contains_plugin_strips_ansi() {
        let ansi_output = "\x1b[1m\x1b[32magent-sec\x1b[0m\nother\n";
        assert!(list_contains_plugin(ansi_output, "agent-sec"));
        assert!(!list_contains_plugin(ansi_output, "not-here"));
    }

    #[test]
    fn list_contains_plugin_rich_table_no_wrap() {
        let table = "\
┏━━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name               ┃ ID          ┃ Status  ┃
┡━━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━╇━━━━━━━━━┩
│ Agent Security     │ agent-sec   │ enabled │
└────────────────────┴─────────────┴─────────┘
";
        assert!(list_contains_plugin(table, "agent-sec"));
        assert!(!list_contains_plugin(table, "not-here"));
        assert!(!list_contains_plugin(table, "agent-sec-extra"));
    }

    #[test]
    fn list_contains_plugin_rich_table_wrapped() {
        let table = "\
┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name            ┃ ID                ┃ Status  ┃
┡━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩
│ Agent Security  │ agent-sec-core-op │ enabled │
│                 │ enclaw-plugin     │         │
└─────────────────┴───────────────────┴─────────┘
";
        assert!(list_contains_plugin(
            table,
            "agent-sec-core-openclaw-plugin"
        ));
        assert!(!list_contains_plugin(table, "agent-sec"));
    }

    #[test]
    fn list_contains_plugin_rich_table_ansi_wrapped() {
        let table = "\
\x1b[1m┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓\x1b[0m
\x1b[1m┃\x1b[0m Name            \x1b[1m┃\x1b[0m ID                \x1b[1m┃\x1b[0m Status  \x1b[1m┃\x1b[0m
\x1b[1m┡━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩\x1b[0m
│ Agent Security  │ agent-sec-core-op │ enabled │
│                 │ enclaw-plugin     │         │
\x1b[1m└─────────────────┴───────────────────┴─────────┘\x1b[0m
";
        assert!(list_contains_plugin(
            table,
            "agent-sec-core-openclaw-plugin"
        ));
    }

    #[test]
    fn strip_ansi_removes_sgr_and_osc() {
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[0m"), "bold");
        assert_eq!(strip_ansi("\x1b[32mgreen\x1b[0m text"), "green text");
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }

    #[test]
    fn missing_plugin_uninstall_is_idempotent_only_for_exact_error() {
        let missing = CliOutput {
            status: Some(1),
            timed_out: false,
            stdout: String::new(),
            stderr: "\x1b[31mPlugin not found: tokenless\x1b[0m\n".to_string(),
        };
        assert!(uninstall_reports_missing_plugin(&missing, "tokenless"));

        let additional_error = CliOutput {
            stderr: "Plugin not found: tokenless\nUnable to update registry\n".to_string(),
            ..missing.clone()
        };
        assert!(!uninstall_reports_missing_plugin(
            &additional_error,
            "tokenless"
        ));

        let wrong_plugin = CliOutput {
            stderr: "Plugin not found: other-plugin\n".to_string(),
            ..missing.clone()
        };
        assert!(!uninstall_reports_missing_plugin(
            &wrong_plugin,
            "tokenless"
        ));

        let timed_out = CliOutput {
            timed_out: true,
            ..missing
        };
        assert!(!uninstall_reports_missing_plugin(&timed_out, "tokenless"));
    }

    #[test]
    fn untracked_uninstall_requires_exact_plugin_and_completed_output() {
        let output = CliOutput {
            status: Some(1),
            timed_out: false,
            stdout: String::new(),
            stderr: "\x1b[31mPlugin \"tokenless\" is not associated with a tracked package install.\x1b[0m\n".to_string(),
        };
        assert!(uninstall_reports_untracked_plugin(&output, "tokenless"));
        assert!(!uninstall_reports_missing_plugin(&output, "tokenless"));
        assert!(!uninstall_reports_untracked_plugin(&output, "token"));
        for stderr in [
            "Plugin \"tokenless-other\" is not associated with a tracked package install.",
            "Plugin \"tokenless\" is not associated with a tracked package installer.",
            "Plugin \"tokenless\" is not associated with a tracked package install.\nUnable to update registry",
        ] {
            assert!(!uninstall_reports_untracked_plugin(
                &CliOutput {
                    stderr: stderr.to_string(),
                    ..output.clone()
                },
                "tokenless"
            ));
        }
        assert!(!uninstall_reports_untracked_plugin(
            &CliOutput {
                timed_out: true,
                ..output
            },
            "tokenless"
        ));
    }

    #[test]
    fn json_absence_requires_success_and_usable_diagnostics() {
        let output = CliOutput {
            status: Some(0),
            timed_out: false,
            stdout:
                r#"{"plugins":[],"diagnostics":[{"level":"info"}],"registry":{"diagnostics":[]}}"#
                    .to_string(),
            stderr: String::new(),
        };
        assert!(json_list_confirms_plugin_absent(&output, "tokenless"));
        for failed in [
            CliOutput {
                timed_out: true,
                ..output.clone()
            },
            CliOutput {
                status: Some(1),
                ..output.clone()
            },
            CliOutput {
                status: None,
                ..output.clone()
            },
            CliOutput {
                stderr: "discovery failed".to_string(),
                ..output.clone()
            },
        ] {
            assert!(!json_list_confirms_plugin_absent(&failed, "tokenless"));
        }
        for stdout in [
            r#"{"plugins":[]}"#,
            r#"{"plugins":[],"diagnostics":null}"#,
            r#"{"plugins":[],"diagnostics":[{}]}"#,
            r#"{"plugins":[],"diagnostics":[],"registry":{}}"#,
            r#"{"plugins":[{"id":""}],"diagnostics":[]}"#,
            r#"{"plugins":[{"id":"tokenless","status":"error"}],"diagnostics":[]}"#,
        ] {
            assert!(
                !json_list_confirms_plugin_absent(
                    &CliOutput {
                        stdout: stdout.to_string(),
                        ..output.clone()
                    },
                    "tokenless"
                ),
                "{stdout}"
            );
        }
    }

    #[test]
    fn is_border_line_identifies_borders() {
        assert!(is_border_line("┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓"));
        assert!(is_border_line("├──────┼──────────┤"));
        assert!(is_border_line("└──────┴──────────┘"));
        assert!(!is_border_line("│ agent-sec │ enabled │"));
        assert!(!is_border_line("plain text"));
        assert!(!is_border_line(""));
    }

    #[test]
    fn extract_cells_splits_data_line() {
        let cells = extract_cells("│ agent-sec   │ enabled │").unwrap();
        assert_eq!(cells, vec!["agent-sec", "enabled"]);
    }

    #[test]
    fn extract_cells_returns_none_for_plain_text() {
        assert!(extract_cells("plain text").is_none());
    }

    #[test]
    fn list_contains_plugin_rich_table_name_and_id_both_wrap() {
        let table = "\
┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name            ┃ ID                ┃ Status  ┃
┡━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩
│ Agent Security  │ agent-sec-core-op │ enabled │
│ Core Plugin     │ enclaw-plugin     │         │
└─────────────────┴───────────────────┴─────────┘
";
        assert!(list_contains_plugin(
            table,
            "agent-sec-core-openclaw-plugin"
        ));
        assert!(!list_contains_plugin(table, "agent-sec-core-op"));
    }

    #[test]
    fn list_contains_plugin_no_false_positive_across_rows() {
        let table = "\
┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name            ┃ ID                ┃ Status  ┃
┡━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩
│ Plugin A        │ agent-sec-core-op │ enabled │
│ Plugin B        │ enclaw-plugin     │ enabled │
└─────────────────┴───────────────────┴─────────┘
";
        assert!(
            !list_contains_plugin(table, "agent-sec-core-openclaw-plugin"),
            "must not merge IDs from independent rows into a false match"
        );
        assert!(list_contains_plugin(table, "agent-sec-core-op"));
        assert!(list_contains_plugin(table, "enclaw-plugin"));
    }

    /// Build a host profile with the given install capabilities; version
    /// parsed from `"2026.4.14"` and inspect `--json`/`--runtime` supported by
    /// default (the fields `build_install_cmd` does not read).
    fn profile(force: bool, unsafe_install: bool) -> OpenClawHostProfile {
        OpenClawHostProfile {
            version: OpenClawVersion::parse("2026.4.14"),
            version_display: "2026.4.14".to_string(),
            supports_install_force: force,
            supports_accept_capabilities: false,
            supports_enable_accept_capabilities: false,
            unsafe_install_support: if unsafe_install {
                UnsafeInstallSupport::Effective
            } else {
                UnsafeInstallSupport::Unsupported
            },
            supports_inspect_json: true,
            supports_inspect_runtime: true,
        }
    }

    #[test]
    fn install_cmd_default_uses_force_only_no_unsafe() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &profile(true, true),
            false,
        )
        .expect("force-capable host builds an install command");
        assert_eq!(cmd.program, "openclaw");
        assert_eq!(
            cmd.args,
            vec![
                "plugins",
                "install",
                "/data/adapters/tokenless/openclaw",
                "--force",
            ],
            "a normal install must never carry the unsafe flag"
        );
        assert!(cmd.env_remove.contains(&"OPENCLAW_HOME".to_string()));
        assert_eq!(
            cmd.env_set,
            vec![(
                "OPENCLAW_STATE_DIR".to_string(),
                "/home/u/.openclaw".to_string()
            )]
        );
        assert_eq!(cmd.path_prepend[0], PathBuf::from("/home/u/.local/bin"));
    }

    #[test]
    fn install_cmd_missing_force_capability_fails() {
        let _env = OpenClawBinEnvGuard::unset();
        let err = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &profile(false, true),
            false,
        )
        .expect_err("no --force must fail before mutation");
        assert!(matches!(err, AdapterError::FrameworkCli { .. }));
    }

    #[test]
    fn install_cmd_authorized_unsafe_supported_appends_flag() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &profile(true, true),
            true,
        )
        .expect("authorized + supported unsafe install builds a command");
        assert_eq!(
            cmd.args,
            vec![
                "plugins",
                "install",
                "/data/adapters/tokenless/openclaw",
                "--force",
                "--dangerously-force-unsafe-install",
            ],
            "a single install argv carries the unsafe flag exactly once"
        );
    }

    #[test]
    fn install_cmd_authorized_unsafe_unsupported_fails() {
        let _env = OpenClawBinEnvGuard::unset();
        let err = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &profile(true, false),
            true,
        )
        .expect_err("authorized but unsupported unsafe must fail before mutation");
        assert!(matches!(err, AdapterError::FrameworkCli { .. }));
    }

    #[test]
    fn install_cmd_authorized_unsafe_deprecated_noop_fails() {
        let _env = OpenClawBinEnvGuard::unset();
        let mut host = profile(true, false);
        host.unsafe_install_support = UnsafeInstallSupport::DeprecatedNoOp;
        let err = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &host,
            true,
        )
        .expect_err("a deprecated no-op must not be treated as an unsafe bypass");
        match err {
            AdapterError::FrameworkCli { reason, .. } => {
                assert!(reason.contains("deprecated no-op"), "{reason}");
                assert!(reason.contains("security.installPolicy"), "{reason}");
            }
            other => panic!("expected FrameworkCli, got {other:?}"),
        }
    }

    #[test]
    fn inspect_cmd_uses_runtime_when_supported() {
        let _env = OpenClawBinEnvGuard::unset();
        let with_runtime = build_inspect_cmd(
            "agent-sec",
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            true,
        );
        assert_eq!(
            with_runtime.args,
            vec!["plugins", "inspect", "agent-sec", "--runtime", "--json"]
        );
        let without_runtime = build_inspect_cmd(
            "agent-sec",
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            false,
        );
        assert_eq!(
            without_runtime.args,
            vec!["plugins", "inspect", "agent-sec", "--json"]
        );
    }

    // -- version parsing / comparison ------------------------------------

    #[test]
    fn version_parses_core_and_variants() {
        let base = OpenClawVersion::parse("2026.4.14").expect("core");
        assert_eq!(base.core, [2026, 4, 14]);
        assert_eq!(base.suffix, VersionSuffix::Release);

        // Build metadata is ignored for identity.
        let build = OpenClawVersion::parse("2026.4.14+build.5").expect("build meta");
        assert_eq!(build, base);

        // Alphabetic prerelease.
        let beta = OpenClawVersion::parse("2026.4.14-beta.1").expect("beta");
        assert!(matches!(beta.suffix, VersionSuffix::Prerelease(_)));

        // Numeric correction.
        let corr = OpenClawVersion::parse("2026.5.3-1").expect("correction");
        assert_eq!(corr.suffix, VersionSuffix::Correction(vec![1]));

        // Two-component core pads the patch to zero.
        let short = OpenClawVersion::parse("2026.4").expect("short");
        assert_eq!(short.core, [2026, 4, 0]);

        // Non-numeric core is rejected.
        assert!(OpenClawVersion::parse("not.a.version").is_none());
        assert!(OpenClawVersion::parse("").is_none());
    }

    #[test]
    fn version_rejects_malformed_suffixes() {
        // An empty `-suffix` must not be treated as a plain release.
        assert!(OpenClawVersion::parse("2026.4.14-").is_none());
        // Empty build metadata must be rejected, not silently dropped.
        assert!(OpenClawVersion::parse("2026.4.14+").is_none());
        // Empty prerelease/build identifiers (double/leading/trailing dots).
        assert!(OpenClawVersion::parse("2026.4.14-beta..1").is_none());
        assert!(OpenClawVersion::parse("2026.4.14-.1").is_none());
        assert!(OpenClawVersion::parse("2026.4.14+build.").is_none());
        // Illegal characters in a prerelease identifier are rejected, not
        // accepted as free text.
        assert!(OpenClawVersion::parse("2026.4.14-beta_1").is_none());
        assert!(OpenClawVersion::parse("2026.4.14-beta!1").is_none());
        // Well-formed suffixes still parse (incl. hyphen inside identifiers
        // and validated-then-dropped build metadata).
        assert!(OpenClawVersion::parse("2026.4.14-rc-1").is_some());
        assert_eq!(
            OpenClawVersion::parse("2026.4.14-beta.1+build.5"),
            OpenClawVersion::parse("2026.4.14-beta.1")
        );
    }

    #[test]
    fn version_ordering_ranks_correction_above_and_prerelease_below() {
        let base = OpenClawVersion::parse("2026.5.3").unwrap();
        let corr = OpenClawVersion::parse("2026.5.3-1").unwrap();
        let beta = OpenClawVersion::parse("2026.5.3-beta.1").unwrap();
        let rc = OpenClawVersion::parse("2026.5.3-rc.2").unwrap();

        // The key departure from stock semver: numeric correction > base.
        assert!(corr > base, "numeric correction must sort above the base");
        assert!(
            beta < base,
            "alphabetic prerelease must sort below the base"
        );
        assert!(beta < rc, "beta precedes rc");
        assert!(rc < base, "prerelease precedes the release");

        // Core precedence still dominates the suffix.
        let newer = OpenClawVersion::parse("2026.5.4").unwrap();
        assert!(newer > corr);
        assert!(newer > base);

        // A larger correction number sorts above a smaller one.
        let corr2 = OpenClawVersion::parse("2026.5.3-2").unwrap();
        assert!(corr2 > corr);
    }

    #[test]
    fn version_req_satisfaction_covers_operators_and_correction() {
        let v = OpenClawVersion::parse("2026.4.24").unwrap();
        assert_eq!(openclaw_version_req_satisfied(">=2026.4.14", &v), Ok(true));
        assert_eq!(openclaw_version_req_satisfied(">=2026.4.24", &v), Ok(true));
        assert_eq!(openclaw_version_req_satisfied(">=2026.5.0", &v), Ok(false));
        assert_eq!(openclaw_version_req_satisfied(">2026.4.24", &v), Ok(false));
        assert_eq!(openclaw_version_req_satisfied("<2026.5.0", &v), Ok(true));
        assert_eq!(
            openclaw_version_req_satisfied(">=2026.4.14, <2026.5.0", &v),
            Ok(true)
        );
        // Bare version behaves as a minimum.
        assert_eq!(openclaw_version_req_satisfied("2026.4.14", &v), Ok(true));

        // A numeric-correction host satisfies a `>=` on the base release.
        let corr = OpenClawVersion::parse("2026.5.3-1").unwrap();
        assert_eq!(
            openclaw_version_req_satisfied(">=2026.5.3", &corr),
            Ok(true)
        );

        // Malformed requirement is an error, not a silent false.
        assert!(openclaw_version_req_satisfied(">=not.a.version", &v).is_err());
        assert!(
            openclaw_version_req_satisfied(">=2027.0.0, >=not.a.version", &v).is_err(),
            "every clause must be validated before a non-match is returned"
        );
        assert!(openclaw_version_req_satisfied("", &v).is_err());
        // A malformed suffix in the constraint is an error, not a match.
        assert!(openclaw_version_req_satisfied(">=2026.4.14-", &v).is_err());
        // Empty clauses (double/leading/trailing comma) are errors.
        assert!(openclaw_version_req_satisfied(">=2026.4.14,,<2027.0.0", &v).is_err());
        assert!(openclaw_version_req_satisfied(">=2026.4.14,", &v).is_err());
        assert!(openclaw_version_req_satisfied(",>=2026.4.14", &v).is_err());
    }

    #[test]
    fn version_output_parsing_extracts_token() {
        assert_eq!(
            parse_openclaw_version_output("openclaw 2026.4.14"),
            OpenClawVersion::parse("2026.4.14")
        );
        assert_eq!(
            parse_openclaw_version_output("OpenClaw CLI version v2026.4.14 (abcdef)"),
            OpenClawVersion::parse("2026.4.14")
        );
        assert_eq!(
            parse_openclaw_version_output("2026.5.3-1\n"),
            OpenClawVersion::parse("2026.5.3-1")
        );
        assert!(parse_openclaw_version_output("no version here").is_none());
    }

    #[test]
    fn version_output_only_accepts_calendar_shape() {
        // An unrelated dependency/runtime version must not be mistaken for
        // OpenClaw's own version, and a non-calendar token is ignored.
        assert!(
            parse_openclaw_version_output(
                "warning: node 22.14.0 is unsupported\nopenclaw nightly-build"
            )
            .is_none(),
            "22.14.0 is not calendar-shaped and nightly-build is not a version"
        );
        // A leading warning number does not derail extraction of the real one.
        assert_eq!(
            parse_openclaw_version_output("note: 3 plugins loaded\nopenclaw 2026.4.14"),
            OpenClawVersion::parse("2026.4.14")
        );
        assert_eq!(
            parse_openclaw_version_output(
                "warning: certificate expires on 2099.1.1\nopenclaw 2026.4.14"
            ),
            OpenClawVersion::parse("2026.4.14"),
            "a calendar-shaped warning token must not outrank the explicit version line"
        );
        assert!(
            parse_openclaw_version_output("warning: retry after 2099.1.1").is_none(),
            "a diagnostic date is not an OpenClaw version"
        );
        assert!(
            parse_openclaw_version_output("openclaw 2026.4.14\n2026.5.0").is_none(),
            "multiple plausible version lines are ambiguous"
        );
        // A two-component version is not accepted as a host version.
        assert!(parse_openclaw_version_output("openclaw 2026.4").is_none());
    }

    #[test]
    fn help_lists_flag_requires_whole_token() {
        assert!(help_lists_flag("  --force    overwrite", "--force"));
        assert!(help_lists_flag("--json=<path>  machine readable", "--json"));
        assert!(help_lists_flag("use --json, or --yaml", "--json"));
        assert!(help_lists_flag(
            "  --dangerously-force-unsafe-install  bypass",
            "--dangerously-force-unsafe-install"
        ));
        // Similar-prefixed flags must not be mistaken for the target flag.
        assert!(!help_lists_flag("--force-color   colorize", "--force"));
        assert!(!help_lists_flag("--json-file <p>  write json", "--json"));
        assert!(!help_lists_flag(
            "--runtime-only   skip static",
            "--runtime"
        ));
    }

    #[test]
    fn unsafe_install_help_distinguishes_effective_from_noop() {
        assert_eq!(
            unsafe_install_support(
                "  --dangerously-force-unsafe-install  bypass plugin safety checks"
            ),
            UnsafeInstallSupport::Effective
        );
        assert_eq!(
            unsafe_install_support(
                "  --dangerously-force-unsafe-install  Deprecated no-op; security.installPolicy may still block"
            ),
            UnsafeInstallSupport::DeprecatedNoOp
        );
        assert_eq!(
            unsafe_install_support("  --force  overwrite an existing plugin"),
            UnsafeInstallSupport::Unsupported
        );
    }

    #[test]
    fn extract_trailing_json_tolerates_leading_diagnostics() {
        let clean = r#"{"plugin":{"status":"loaded"}}"#;
        assert_eq!(
            extract_trailing_json(clean).and_then(|v| v
                .get("plugin")?
                .get("status")?
                .as_str()
                .map(str::to_string)),
            Some("loaded".to_string())
        );

        let with_diag = "warning: legacy diagnostic line\nreading registry...\n{\"plugin\":{\"status\":\"loaded\"}}\n";
        let value = extract_trailing_json(with_diag).expect("json after diagnostics");
        assert_eq!(
            value
                .get("plugin")
                .and_then(|p| p.get("status"))
                .and_then(|s| s.as_str()),
            Some("loaded")
        );

        assert!(extract_trailing_json("not json at all").is_none());
        assert!(extract_trailing_json("").is_none());
    }

    #[test]
    fn uninstall_cmd_uses_force() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_uninstall_cmd(
            "tokenless",
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(
            cmd.args,
            vec!["plugins", "uninstall", "tokenless", "--force"]
        );
    }

    #[test]
    fn plugin_manifest_id_is_read_from_real_openclaw_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("openclaw.plugin.json"),
            br#"{"id":"tokenless","name":"Tokenless"}"#,
        )
        .expect("write manifest");

        assert_eq!(
            read_plugin_manifest_id(dir.path(), "openclaw.plugin.json").expect("read"),
            Some("tokenless".to_string())
        );
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

    // -- config set cmd -------------------------------------------------

    #[test]
    fn config_set_cmd_string_value() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_config_set_cmd(
            "plugins.entries.sec.enabled",
            &toml::Value::String("true".to_string()),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(cmd.program, "openclaw");
        assert_eq!(
            cmd.args,
            vec!["config", "set", "plugins.entries.sec.enabled", "true"]
        );
    }

    #[test]
    fn config_set_cmd_boolean_value() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_config_set_cmd(
            "debug.enabled",
            &toml::Value::Boolean(true),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(cmd.args, vec!["config", "set", "debug.enabled", "true"]);
    }

    #[test]
    fn config_set_cmd_integer_value() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_config_set_cmd(
            "limits.max_plugins",
            &toml::Value::Integer(42),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(cmd.args, vec!["config", "set", "limits.max_plugins", "42"]);
    }

    #[test]
    fn config_value_to_cli_string_covers_types() {
        assert_eq!(
            config_value_to_cli_string(&toml::Value::String("hello".into())),
            "hello"
        );
        assert_eq!(config_value_to_cli_string(&toml::Value::Integer(7)), "7");
        assert_eq!(config_value_to_cli_string(&toml::Value::Float(2.5)), "2.5");
        assert_eq!(
            config_value_to_cli_string(&toml::Value::Boolean(false)),
            "false"
        );
    }

    // -- claim_skill_resources / claim_config_counts --------------------

    #[test]
    fn claim_skill_resources_extracts_names() {
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "test".to_string(),
            framework: "openclaw".to_string(),
            plugin_id: None,
            adapter_type: None,
            enabled_at: "2026-01-01T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/tmp"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::CleanupFailed,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "openclaw_config_0".to_string(),
                    purpose: "openclaw_config".to_string(),
                    kind: ClaimResourceKind::FrameworkConfig {
                        framework: "openclaw".to_string(),
                        key: "applied.key".to_string(),
                        state: ConfigApplyState::Applied,
                    },
                },
                ClaimResource {
                    id: "openclaw_config_1".to_string(),
                    purpose: "openclaw_config".to_string(),
                    kind: ClaimResourceKind::FrameworkConfig {
                        framework: "openclaw".to_string(),
                        key: "pending.key".to_string(),
                        state: ConfigApplyState::Pending,
                    },
                },
            ],
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "s".to_string(),
                plugin_resource: "p".to_string(),
                skill_resources: vec![
                    "openclaw_skill_sec-audit".to_string(),
                    "openclaw_skill_cred-scan".to_string(),
                ],
                config_resources: vec!["openclaw_config_0".to_string()],
            }),
        };
        let skills = claim_skill_resources(&claim);
        assert_eq!(skills, vec!["sec-audit", "cred-scan"]);
        assert_eq!(claim_config_counts(&claim), (1, 1));
    }

    #[test]
    fn skill_bundle_plan_and_claim_skip_plugin_registration() {
        use crate::adapter::driver::{AdapterOps, CliOutput, DeclaredSkill};

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

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("marker"), b"x").expect("write");
        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/test-home"));
        let ops = StubOps;
        let ctx = DriverCtx {
            component: "os-skills".to_string(),
            framework: "openclaw".to_string(),
            layout: &layout,
            resource_root: dir.path().to_path_buf(),
            user_home: Some(PathBuf::from("/tmp/test-home")),
            declared_plugin_id: None,
            requested_profiles: Vec::new(),
            adapter_type: Some("skill_bundle".to_string()),
            declared_skills: vec![DeclaredSkill {
                name: "install-openclaw".to_string(),
                source: Some(PathBuf::from("/usr/share/anolisa/skills/install-openclaw")),
            }],
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: true,
            ops: &ops,
        };
        let driver = OpenClawDriver::new();
        let bundle = driver.read_bundle(&ctx).expect("read bundle");
        assert!(bundle.plugin_id.is_none());

        let plan = driver.plan_enable(&bundle, &ctx).expect("plan");
        assert!(plan.register_command.is_none());
        assert!(
            plan.actions
                .iter()
                .all(|action| !action.contains("register openclaw plugin")),
        );

        let (claim, _prepared) = driver.prepare_enable(&bundle, &ctx).expect("claim");
        assert!(claim.plugin_id.is_none());
        assert_eq!(claim.adapter_type.as_deref(), Some("skill_bundle"));
        assert!(
            claim.resources.iter().all(|resource| !matches!(
                resource.kind,
                ClaimResourceKind::FrameworkPlugin { .. }
            )),
        );
        assert_eq!(claim_skill_resources(&claim), vec!["install-openclaw"]);

        let _env = OpenClawBinEnvGuard::set("/bin/sh");
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

    /// A mismatched (or under-capable) `PreparedEnable` must fail closed in
    /// `apply_enable` **before** any framework CLI runs. The ops handle panics
    /// on `run_framework_cli`, so any test reaching this line proves no
    /// mutation was attempted.
    #[test]
    fn apply_enable_rejects_mismatched_prepared_state() {
        use crate::adapter::driver::{AdapterOps, CliOutput};

        struct PanicOps;
        impl AdapterOps for PanicOps {
            fn run_framework_cli(&self, _: FrameworkCommand) -> Result<CliOutput, AdapterError> {
                panic!("apply must fail closed before running any framework CLI");
            }
            fn copy_tree(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                panic!("no tree copy before validation");
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

        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/test-home"));
        let ops = PanicOps;
        let mk_ctx = |adapter_type: Option<&str>, allow_unsafe: bool| DriverCtx {
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            layout: &layout,
            resource_root: PathBuf::from("/tmp/test-home/resource"),
            user_home: Some(PathBuf::from("/tmp/test-home")),
            declared_plugin_id: None,
            requested_profiles: Vec::new(),
            adapter_type: adapter_type.map(str::to_string),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: allow_unsafe,
            dry_run: false,
            ops: &ops,
        };
        let mut plugin_claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: None,
            enabled_at: "2026-01-01T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/tmp/test-home/resource"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: "openclaw_plugin".to_string(),
                kind: ClaimResourceKind::FrameworkPlugin {
                    framework: "openclaw".to_string(),
                    plugin_id: "tokenless".to_string(),
                },
            }],
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: RES_STATE_DIR.to_string(),
                plugin_resource: RES_PLUGIN.to_string(),
                skill_resources: Vec::new(),
                config_resources: Vec::new(),
            }),
        };
        let mut skill_claim = plugin_claim.clone();
        skill_claim.adapter_type = Some("skill_bundle".to_string());
        skill_claim.plugin_id = None;

        let driver = OpenClawDriver::new();
        let _env = OpenClawBinEnvGuard::unset();

        // Plugin adapter but no prepared capabilities → reject.
        assert!(matches!(
            driver.apply_enable(
                &mut plugin_claim,
                &PreparedEnable::None,
                &mk_ctx(None, false),
                &mut (),
            ),
            Err(AdapterError::FrameworkCli { .. })
        ));

        // Skill bundle but plugin capabilities supplied → reject.
        assert!(matches!(
            driver.apply_enable(
                &mut skill_claim,
                &PreparedEnable::OpenClaw {
                    supports_accept_capabilities: false,
                    supports_enable_accept_capabilities: false,
                    supports_unsafe_install: true,
                    supports_inspect_json: true,
                    supports_inspect_runtime: true,
                    selected_config_indices: Vec::new(),
                },
                &mk_ctx(Some("skill_bundle"), false),
                &mut (),
            ),
            Err(AdapterError::FrameworkCli { .. })
        ));

        // Plugin adapter but the host cannot produce JSON inspect → reject.
        assert!(matches!(
            driver.apply_enable(
                &mut plugin_claim,
                &PreparedEnable::OpenClaw {
                    supports_accept_capabilities: false,
                    supports_enable_accept_capabilities: false,
                    supports_unsafe_install: true,
                    supports_inspect_json: false,
                    supports_inspect_runtime: false,
                    selected_config_indices: Vec::new(),
                },
                &mk_ctx(None, false),
                &mut (),
            ),
            Err(AdapterError::FrameworkCli { .. })
        ));

        // Unsafe authorized but the prepared state says the host lacks the
        // flag → reject (never add the dangerous flag on an unverified host).
        assert!(matches!(
            driver.apply_enable(
                &mut plugin_claim,
                &PreparedEnable::OpenClaw {
                    supports_accept_capabilities: false,
                    supports_enable_accept_capabilities: false,
                    supports_unsafe_install: false,
                    supports_inspect_json: true,
                    supports_inspect_runtime: false,
                    selected_config_indices: Vec::new(),
                },
                &mk_ctx(None, true),
                &mut (),
            ),
            Err(AdapterError::FrameworkCli { .. })
        ));
    }
}
