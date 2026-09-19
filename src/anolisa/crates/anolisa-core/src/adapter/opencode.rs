//! OpenCode local-plugin registration through a receipt-owned symlink.
//!
//! OpenCode loads plugins at startup. Filesystem registration is verifiable;
//! whether an already-running host loaded the plugin is not.

use std::path::{Component, Path, PathBuf};

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    DRIVER_SCHEMA_VERSION, DriverPayload, OpenCodeClaim, validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx, DriverPlan,
    EnableProgress, FrameworkDriver, HostEnv, PreparedEnable, find_binary_in_path, is_executable,
};
use super::util::{bool_status, now_iso8601, symlink_matches};

const RES_LINK: &str = "opencode_plugin_link";

/// Manage local OpenCode plugins without editing the host's JSON configuration.
pub struct OpenCodeDriver;

impl OpenCodeDriver {
    /// Construct the stateless driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenCodeDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for OpenCodeDriver {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn detect(&self, _env: &HostEnv) -> DetectResult {
        let program = std::env::var_os("OPENCODE_BIN")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "opencode".into());
        let path = Path::new(&program);
        let found = if path.is_absolute() {
            (path.is_file() && is_executable(path)).then(|| path.to_path_buf())
        } else {
            program.to_str().and_then(find_binary_in_path)
        };
        DetectResult {
            detected: found.is_some(),
            reason: found.map_or_else(
                || format!("OpenCode CLI not found: {}", path.display()),
                |path| format!("OpenCode CLI found at {}", path.display()),
            ),
        }
    }

    fn probe_bundle(&self, root: &Path, declared_entry: Option<&str>) -> bool {
        let entry = Path::new(declared_entry.unwrap_or("plugin.js"));
        valid_entry(entry) && root.join(entry).is_file()
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        config_dir(ctx.user_home.as_deref()).into_iter().collect()
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        let entry = Path::new(ctx.declared_bundle_entry.as_deref().unwrap_or("plugin.js"));
        if !self.probe_bundle(&ctx.resource_root, ctx.declared_bundle_entry.as_deref()) {
            return Err(AdapterError::BundleInvalid {
                root: ctx.resource_root.clone(),
                reason: format!(
                    "OpenCode entry must be an existing relative .js or .ts file: {}",
                    entry.display()
                ),
            });
        }
        let plugin_id = ctx.declared_plugin_id.as_deref().unwrap_or(&ctx.component);
        validate_plugin_id(plugin_id)?;
        Ok(AdapterBundle {
            resource_root: ctx.resource_root.clone(),
            plugin_id: Some(plugin_id.to_string()),
        })
    }

    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError> {
        let (link, target) = plugin_paths(bundle, ctx)?;
        let action = if symlink_matches(&link, &target)? {
            "adopt"
        } else {
            "create"
        };
        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions: vec![
                format!(
                    "{action} OpenCode plugin link {} -> {} (refuse conflicting paths)",
                    link.display(),
                    target.display()
                ),
                "restart OpenCode to load the plugin".to_string(),
            ],
            register_command: None,
        })
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let (link, target) = plugin_paths(bundle, ctx)?;
        Ok((
            AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: ctx.component.clone(),
                framework: self.name().to_string(),
                plugin_id: bundle.plugin_id.clone(),
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
                    id: RES_LINK.to_string(),
                    purpose: "opencode_local_plugin".to_string(),
                    kind: ClaimResourceKind::Symlink { link, target },
                }],
                driver_payload: DriverPayload::OpenCode(OpenCodeClaim {
                    symlink_resource: RES_LINK.to_string(),
                }),
            },
            PreparedEnable::None,
        ))
    }

    fn plan_reenable_cleanup(
        &self,
        prior: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        let bundle = self.read_bundle(ctx)?;
        let (next_link, next_target) = plugin_paths(&bundle, ctx)?;
        let (link, target) = claimed_link(prior)?;
        if (link, target) != (next_link.as_path(), next_target.as_path()) {
            return Ok(vec![format!(
                "remove prior matching OpenCode plugin link {} -> {}",
                link.display(),
                target.display()
            )]);
        }
        Ok(Vec::new())
    }

    fn cleanup_replaced_claim(
        &self,
        prior: &AdapterClaim,
        next: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        let (old_link, old_target) = claimed_link(prior)?;
        let (next_link, next_target) = claimed_link(next)?;
        if old_link != next_link && !symlink_matches(next_link, next_target)? {
            // A known destination conflict must not remove the working link
            // or replace its receipt. apply_enable still refuses race-created
            // conflicts through create_symlink_new's exclusive creation.
            match std::fs::symlink_metadata(next_link) {
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(AdapterError::Io {
                        path: next_link.to_path_buf(),
                        source,
                    });
                }
                Ok(_) => {
                    return Err(AdapterError::Io {
                        path: next_link.to_path_buf(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "refusing OpenCode migration to a conflicting plugin path",
                        ),
                    });
                }
            }
        }
        if (old_link, old_target) != (next_link, next_target) {
            return self.disable(prior, ctx);
        }
        Ok(DisableReport {
            cleanup_complete: true,
            messages: Vec::new(),
        })
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        _prepared: &PreparedEnable,
        ctx: &DriverCtx,
        _progress: &mut dyn EnableProgress,
    ) -> Result<(), AdapterError> {
        if ctx.dry_run {
            return Ok(());
        }
        let (link, target) = claimed_link(claim)?;
        if !symlink_matches(link, target)? {
            ctx.ops.create_symlink_new(link, target)?;
        }
        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        let (link, target) = claimed_link(claim)?;
        let detected = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        let (status, reason) = match symlink_matches(link, target) {
            Ok(matches) => (
                bool_status(matches),
                format!("{} -> {}", link.display(), target.display()),
            ),
            Err(error) => (ConditionStatus::Unknown, error.to_string()),
        };
        Ok(AdapterStatusReport {
            summary: if !detected.detected || status == ConditionStatus::False {
                AdapterSummary::Degraded
            } else {
                AdapterSummary::Unknown
            },
            conditions: vec![
                AdapterCondition {
                    kind: AdapterConditionKind::FrameworkDetected,
                    status: bool_status(detected.detected),
                    reason: Some(detected.reason),
                    resource: None,
                },
                AdapterCondition {
                    kind: AdapterConditionKind::SymlinkPresent,
                    status,
                    reason: Some(reason),
                    resource: Some(ClaimResourceRef { id: RES_LINK.to_string() }),
                },
                AdapterCondition {
                    kind: AdapterConditionKind::PluginResourcesLoaded,
                    status: ConditionStatus::Unknown,
                    reason: Some("Restart OpenCode to load the plugin; running-host verification is unavailable".to_string()),
                    resource: None,
                },
                AdapterCondition {
                    kind: AdapterConditionKind::VerificationSupported,
                    status: ConditionStatus::False,
                    reason: Some("Only local plugin registration can be verified".to_string()),
                    resource: None,
                },
            ],
        })
    }

    fn disable(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        let (link, target) = claimed_link(claim)?;
        if ctx.dry_run {
            return Ok(DisableReport {
                cleanup_complete: true,
                messages: vec![format!(
                    "would remove matching OpenCode plugin link {}",
                    link.display()
                )],
            });
        }
        let cleanup_complete = ctx.ops.remove_matching_symlink(link, target)?;
        Ok(DisableReport {
            cleanup_complete,
            messages: vec![if cleanup_complete {
                "OpenCode plugin link removed or already absent; restart OpenCode to unload it"
                    .to_string()
            } else {
                format!(
                    "preserving changed path {}; restore the recorded link or remove the conflict, then retry disable",
                    link.display()
                )
            }],
        })
    }
}

fn valid_entry(entry: &Path) -> bool {
    entry
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
        && matches!(
            entry.extension().and_then(|s| s.to_str()),
            Some("js" | "ts")
        )
}

fn config_dir(home: Option<&Path>) -> Option<PathBuf> {
    let path = std::env::var_os("OPENCODE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(|p| PathBuf::from(p).join("opencode"))
        })
        .or_else(|| home.map(|p| p.join(".config/opencode")))?;
    // Relative config roots would make receipt authority depend on the CWD.
    path.is_absolute().then_some(path)
}

fn plugin_paths(
    bundle: &AdapterBundle,
    ctx: &DriverCtx,
) -> Result<(PathBuf, PathBuf), AdapterError> {
    let config =
        config_dir(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::InvalidAdapterInput {
            component: ctx.component.clone(),
            framework: "opencode".to_string(),
            reason:
                "OpenCode requires an absolute OPENCODE_CONFIG_DIR, XDG_CONFIG_HOME, or user home"
                    .to_string(),
        })?;
    let entry = Path::new(ctx.declared_bundle_entry.as_deref().unwrap_or("plugin.js"));
    let plugin_id = bundle.plugin_id.as_deref().unwrap_or(&ctx.component);
    validate_plugin_id(plugin_id)?;
    let extension = entry
        .extension()
        .and_then(|s| s.to_str())
        .filter(|_| valid_entry(entry))
        .ok_or_else(|| AdapterError::BundleInvalid {
            root: bundle.resource_root.clone(),
            reason: "OpenCode entry must be a relative .js or .ts path".to_string(),
        })?;
    Ok((
        config
            .join("plugins")
            .join(format!("{plugin_id}.{extension}")),
        bundle.resource_root.join(entry),
    ))
}

/// Resolve the receipt's validated link and target for lifecycle and trust persistence.
pub(crate) fn claimed_link(claim: &AdapterClaim) -> Result<(&Path, &Path), AdapterError> {
    if let DriverPayload::OpenCode(payload) = &claim.driver_payload
        && let Some(resource) = claim.resource(&payload.symlink_resource)
        && let ClaimResourceKind::Symlink { link, target } = &resource.kind
    {
        return Ok((link, target));
    }
    Err(AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: "OpenCode receipt is missing its plugin symlink resource".to_string(),
    })
}
