//! Cosh (copilot-shell) framework driver.
//!
//! ANOLISA owns the receipt-bound user copy; cosh owns source selection and
//! activation. Native transactions select the delivered identity and release
//! only its selection during cleanup. System extensions remain package-owned.
//!
//! `COSH_HOME` overrides the extension/state root; `COSH_CORE_PATH` selects the native
//! registry executable, while `COSH_BIN` overrides CLI detection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus, CoshClaim,
    DRIVER_SCHEMA_VERSION, DriverPayload,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx, DriverPlan,
    FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable, find_binary_in_path,
};
use super::managed_files::{
    ManagedMatch, MaterializedMapping, copy_materialized_resource, verify_materialized_bundle,
};
use super::util::{bool_status, now_iso8601};

/// Candidate binary names that indicate cosh is installed. `co` and
/// `copilot` are the short/legacy aliases of the `cosh` CLI.
const COSH_BINARIES: &[&str] = &["cosh", "co", "copilot"];

/// Native manifest inside a cosh extension bundle. Its presence is what
/// makes a directory a valid cosh extension.
const COSH_MANIFEST: &str = "cosh-extension.json";

/// Ownership marker ANOLISA drops inside the delivered extension directory.
/// Its presence proves the directory is ANOLISA-managed, so a re-enable may
/// safely replace it and disable may safely remove it — without that proof,
/// enable refuses to overwrite and disable leaves the directory alone, so a
/// user-installed extension of the same name is never destroyed.
const COSH_OWNERSHIP_MARKER: &str = ".anolisa-adapter";

/// Resource id used in Cosh receipts.
const RES_EXTENSION_DIR: &str = "cosh_extension_dir";

/// Cosh driver. Stateless; all per-operation context arrives via
/// [`DriverCtx`].
pub struct CoshDriver;

impl CoshDriver {
    /// Construct the driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CoshDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for CoshDriver {
    // The extension dir carries COSH_OWNERSHIP_MARKER: apply_enable refuses an
    // unmarked existing dir and reclaims a marked orphan itself.
    fn owns_preexisting_materialized_roots(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "cosh"
    }

    fn probe_bundle(&self, resource_root: &Path, declared_entry: Option<&str>) -> bool {
        // Mirrors read_bundle's mandatory check: the native extension
        // manifest (or the contract-declared entry) must be present.
        resource_root
            .join(declared_entry.unwrap_or(COSH_MANIFEST))
            .is_file()
    }

    fn detect(&self, env: &HostEnv) -> DetectResult {
        // A home is a discovery hint; prepare_enable separately requires the
        // native registry before changing any delivered files.
        if let Some(path) = detect_cosh_binary() {
            return DetectResult {
                detected: true,
                reason: format!("cosh CLI found at {}", path.display()),
            };
        }
        match cosh_home(env.user_home.as_deref()).filter(|h| h.exists()) {
            Some(home) => DetectResult {
                detected: true,
                reason: format!(
                    "cosh CLI not on PATH, but home {} exists (extension can still be delivered)",
                    home.display()
                ),
            },
            None => DetectResult {
                detected: false,
                reason: "cosh not detected: no cosh/co/copilot on PATH and no ~/.copilot-shell"
                    .to_string(),
            },
        }
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        // The only external root Cosh writes is its own home directory.
        cosh_home(ctx.user_home.as_deref()).into_iter().collect()
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        let root = &ctx.resource_root;
        if !root.is_dir() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: "resource root does not exist or is not a directory".to_string(),
            });
        }
        let manifest = ctx
            .declared_bundle_entry
            .as_deref()
            .unwrap_or(COSH_MANIFEST);
        if !root.join(manifest).is_file() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!("cosh extension manifest '{manifest}' missing from resource root"),
            });
        }
        // Extension id used for the destination directory name. Falls back
        // to the component name when the manifest declares no plugin id.
        let plugin_id = Some(
            ctx.declared_plugin_id
                .clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| ctx.component.clone()),
        );
        let manifest_path = root.join(manifest);
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|source| AdapterError::Io {
            path: manifest_path,
            source,
        })?;
        let native: Value = serde_json::from_slice(&manifest_bytes).map_err(|error| {
            AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!("invalid cosh extension manifest: {error}"),
            }
        })?;
        if native.get("name").and_then(Value::as_str) != plugin_id.as_deref() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: "cosh extension name does not match adapter plugin id".to_string(),
            });
        }
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
        let dst = extension_dir(bundle, ctx)?;
        let actions = vec![format!(
            "deliver cosh extension from {} to {}",
            bundle.resource_root.display(),
            dst.display(),
        )];
        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions,
            register_command: None,
        })
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        require_native_lifecycle(ctx)?;
        let dst = extension_dir(bundle, ctx)?;
        let resources = vec![ClaimResource {
            id: RES_EXTENSION_DIR.to_string(),
            purpose: "cosh_extension_dir".to_string(),
            kind: ClaimResourceKind::ExternalPath { path: dst },
        }];
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
                resources,
                driver_payload: DriverPayload::Cosh(CoshClaim {
                    extension_dir_resource: RES_EXTENSION_DIR.to_string(),
                }),
            },
            PreparedEnable::None,
        ))
    }

    fn preserve_reenable_facts(
        &self,
        prior: &AdapterClaim,
        _next: &mut AdapterClaim,
    ) -> Result<(), AdapterError> {
        if !prior.materialized_files.is_empty() {
            match verify_existing_files(prior) {
                ManagedMatch::Matched => {}
                ManagedMatch::Changed(reason) | ManagedMatch::Unknown(reason) => {
                    return Err(native_error(format!(
                        "refusing to replace edited cosh extension: {reason}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn plan_reenable_cleanup(
        &self,
        prior: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        let next = self.read_bundle(ctx)?;
        if prior.plugin_id.as_deref().unwrap_or(&prior.component)
            == next.plugin_id.as_deref().unwrap_or(&ctx.component)
            && claim_extension_dir(prior).as_ref() == Some(&extension_dir(&next, ctx)?)
        {
            return Ok(Vec::new());
        }
        Ok(vec![format!(
            "remove prior managed cosh extension '{}' and release its matching source selection",
            prior.plugin_id.as_deref().unwrap_or(&prior.component),
        )])
    }

    fn cleanup_replaced_claim(
        &self,
        prior: &AdapterClaim,
        next: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        if prior.plugin_id.as_deref().unwrap_or(&prior.component)
            == next.plugin_id.as_deref().unwrap_or(&next.component)
            && claim_extension_dir(prior) == claim_extension_dir(next)
        {
            return Ok(DisableReport {
                cleanup_complete: true,
                messages: Vec::new(),
            });
        }
        // Release the old native selection while its receipt is still durable.
        self.disable(prior, ctx)
    }

    fn materialized_mappings(
        &self,
        resource_root: &Path,
        _adapter_type: Option<&str>,
        _declared_skills: &[super::driver::DeclaredSkill],
    ) -> Vec<MaterializedMapping> {
        vec![MaterializedMapping {
            resource_id: RES_EXTENSION_DIR.to_string(),
            source_root: resource_root.to_path_buf(),
            excluded_prefixes: Vec::new(),
        }]
    }

    fn materialized_destination_roots(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<BTreeMap<String, PathBuf>, AdapterError> {
        Ok(BTreeMap::from([(
            RES_EXTENSION_DIR.to_string(),
            extension_dir(bundle, ctx)?,
        )]))
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
        let dst = claim_extension_dir(claim).ok_or_else(|| AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: "cosh receipt has no extension directory resource".to_string(),
        })?;
        // Never clobber a directory ANOLISA does not own. A first enable
        // onto a user-installed extension of the same name would otherwise
        // silently destroy the user's files (and a later disable would
        // remove them as if ANOLISA had created them). Only replace when the
        // directory is empty/absent or carries our ownership marker.
        if dst.exists() && !is_anolisa_owned(&dst) && !dir_is_empty(&dst) {
            return Err(AdapterError::InvalidAdapterInput {
                component: ctx.component.clone(),
                framework: self.name().to_string(),
                reason: format!(
                    "refusing to overwrite existing non-ANOLISA cosh extension at {} (no {COSH_OWNERSHIP_MARKER} marker); remove it manually to enable",
                    dst.display()
                ),
            });
        }
        // Replace any prior ANOLISA contents so a re-enable after a bundle
        // change does not leave stale files behind. The dir is inside the
        // receipt-declared external root, so remove_tree is bounded.
        ctx.ops.remove_tree(&dst)?;
        // Write the ownership marker *before* copying the bundle: if the
        // copy fails partway, the directory is already provably
        // ANOLISA-owned, so a later disable will remove the partial tree
        // instead of leaving it (and dropping the receipt). copy_tree
        // tolerates the pre-existing marker in the destination.
        ctx.ops.write_file(
            &dst.join(COSH_OWNERSHIP_MARKER),
            b"ANOLISA-managed cosh extension\n",
        )?;
        copy_materialized_resource(claim, RES_EXTENSION_DIR, &claim.resource_root, ctx.ops)?;
        let name = claim.plugin_id.as_deref().unwrap_or(&claim.component);
        let identity = canonical_identity(&dst)?;
        let detail = registry(ctx, "detail", json!({"name": name}))?;
        if detail
            .pointer("/available_source_identities/user")
            .and_then(Value::as_str)
            != Some(identity.as_str())
        {
            return Err(native_error(
                "cosh did not discover the delivered user identity",
            ));
        }
        registry(
            ctx,
            "select-source",
            json!({"name": name, "source": "user"}),
        )?;
        registry(ctx, "enable", json!({"name": name}))?;
        let detail = registry(ctx, "detail", json!({"name": name}))?;
        if !configured(&detail, &identity) {
            return Err(native_error(
                "cosh did not enable the delivered extension; inspect extensions detail",
            ));
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

        // 2. Extension tree still present, carrying its manifest AND our
        //    ownership marker? A dir/manifest without the marker is not
        //    ANOLISA-managed (user replaced it, or a marker write failed),
        //    which is a degraded state — not a healthy one. This is a
        //    reliable, read-only filesystem check, so verification is always
        //    supported for cosh.
        let (tree_present, tree_reason) = match claim_extension_dir(claim) {
            Some(dir) if !dir.is_dir() || !dir.join(COSH_MANIFEST).is_file() => (
                false,
                Some("cosh extension directory or manifest missing".to_string()),
            ),
            Some(dir) if !is_anolisa_owned(&dir) => (
                false,
                Some(format!(
                    "cosh extension directory is not ANOLISA-managed ({COSH_OWNERSHIP_MARKER} marker missing)"
                )),
            ),
            Some(_) => (true, None),
            None => (
                false,
                Some("receipt has no extension directory".to_string()),
            ),
        };
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::TreePresent,
            status: bool_status(tree_present),
            reason: tree_reason,
            resource: Some(ClaimResourceRef {
                id: RES_EXTENSION_DIR.to_string(),
            }),
        });
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::VerificationSupported,
            status: ConditionStatus::True,
            reason: None,
            resource: None,
        });

        let native = claim_extension_dir(claim).map(|dir| {
            let identity = canonical_identity(&dir)?;
            let name = claim.plugin_id.as_deref().unwrap_or(&claim.component);
            registry(ctx, "detail", json!({"name": name}))
                .map(|detail| configured(&detail, &identity))
        });
        let (configured_status, reason) = match native {
            Some(Ok(true)) => (ConditionStatus::True, Some("cosh catalog enables this user identity; restart existing cosh sessions to load it".to_string())),
            Some(Ok(false)) => (ConditionStatus::False, Some("cosh reports another source, disabled state, or unhealthy extension".to_string())),
            Some(Err(error)) => (ConditionStatus::Unknown, Some(error.to_string())),
            None => (ConditionStatus::False, Some("receipt has no extension directory".to_string())),
        };
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::ActivationEnabled,
            status: configured_status,
            reason,
            resource: None,
        });
        conditions.push(AdapterCondition { kind: AdapterConditionKind::PluginResourcesLoaded,
            status: ConditionStatus::Unknown,
            reason: Some("standalone registry verifies catalog state; resident cosh session activation is not observable; restart existing sessions".to_string()), resource: None });
        let mut summary = summarize(claim.status, detect.detected, tree_present);
        if summary == AdapterSummary::Healthy {
            summary = if configured_status == ConditionStatus::False {
                AdapterSummary::Degraded
            } else {
                AdapterSummary::Unknown
            };
        }
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
        // Probe before deletion: older cosh cannot release the persisted choice.
        require_native_lifecycle(ctx)?;
        let owned_dir = claim_extension_dir(claim);
        let identity = owned_dir
            .as_ref()
            .map(|dir| canonical_identity(dir))
            .transpose()?;

        match owned_dir {
            Some(dir) if !dir.exists() => {
                messages.push(format!(
                    "cosh extension dir {} already absent",
                    dir.display()
                ));
            }
            // Only remove a directory we can prove ANOLISA created. If the
            // marker is gone (e.g. the user replaced the extension after
            // enable), leave the directory in place rather than deleting
            // files ANOLISA does not own — but do NOT report success: the
            // extension is still on disk (and may still be auto-discovered
            // by cosh), so keep the receipt as cleanup_failed for the
            // operator to resolve manually.
            Some(dir) if !is_anolisa_owned(&dir) => {
                cleanup_complete = false;
                messages.push(format!(
                    "cosh extension dir {} is not ANOLISA-managed ({COSH_OWNERSHIP_MARKER} marker missing); left in place — remove it manually, then re-run disable",
                    dir.display()
                ));
            }
            Some(dir)
                if !claim.materialized_files.is_empty()
                    && verify_existing_files(claim) != ManagedMatch::Matched =>
            {
                cleanup_complete = false;
                messages.push(format!(
                    "cosh extension {} contains changed managed files; left in place",
                    dir.display()
                ));
            }
            Some(dir) => match ctx.ops.remove_tree(&dir) {
                Ok(true) => messages.push(format!("removed cosh extension dir {}", dir.display())),
                Ok(false) => messages.push(format!(
                    "cosh extension dir {} already absent",
                    dir.display()
                )),
                Err(err) => {
                    cleanup_complete = false;
                    messages.push(format!(
                        "failed to remove cosh extension dir {}: {err}",
                        dir.display()
                    ));
                }
            },
            None => messages.push("receipt records no cosh extension directory".to_string()),
        }

        if cleanup_complete && let Some(identity) = identity {
            let name = claim.plugin_id.as_deref().unwrap_or(&claim.component);
            if let Err(error) = registry(
                ctx,
                "clear-user-source",
                json!({"name": name, "source_identity": identity}),
            ) {
                cleanup_complete = false;
                messages.push(format!(
                    "extension files removed; source selection cleanup requires retry: {error}"
                ));
            }
        }
        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }
}

fn verify_existing_files(claim: &AdapterClaim) -> ManagedMatch {
    // Partial delivery/removal may omit files; existing user edits remain protected on retries.
    let mut existing = claim.clone();
    existing.materialized_files.retain(|file| {
        let Some(resource) = claim.resource(&file.resource_id) else {
            return true;
        };
        let root = match &resource.kind {
            ClaimResourceKind::ExternalPath { path } => path,
            _ => return true,
        };
        !std::fs::symlink_metadata(root.join(&file.relative_path))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    });
    verify_materialized_bundle(&existing)
}

fn native_error(reason: impl Into<String>) -> AdapterError {
    AdapterError::FrameworkCli {
        program: "cosh-core".to_string(),
        reason: reason.into(),
    }
}

fn require_native_lifecycle(ctx: &DriverCtx) -> Result<(), AdapterError> {
    let capabilities = registry(ctx, "adapter-capabilities", json!({}))?;
    if capabilities
        .get("conditional_clear_user_source")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(native_error(
            "cosh lacks conditional source cleanup; update cosh-ng before enabling or disabling adapters",
        ));
    }
    Ok(())
}

fn canonical_identity(path: &Path) -> Result<String, AdapterError> {
    if path.exists() {
        return path
            .canonicalize()
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|source| AdapterError::Io {
                path: path.to_path_buf(),
                source,
            });
    }
    let parent = path
        .parent()
        .ok_or_else(|| native_error("cosh extension path has no existing ancestor"))?;
    Ok(PathBuf::from(canonical_identity(parent)?)
        .join(path.file_name().unwrap_or_default())
        .to_string_lossy()
        .into_owned())
}

fn configured(detail: &Value, identity: &str) -> bool {
    detail.get("source_identity").and_then(Value::as_str) == Some(identity)
        && detail
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| {
                canonical_identity(Path::new(path)).is_ok_and(|actual| actual == identity)
            })
        && detail.get("desired_state").and_then(Value::as_str) == Some("enabled")
        && matches!(
            detail.get("health").and_then(Value::as_str),
            Some("healthy" | "degraded")
        )
}

fn registry(ctx: &DriverCtx, action: &str, params: Value) -> Result<Value, AdapterError> {
    let request = json!({"type": "registry_request", "request_id": "anolisa-adapter", "domain": "extensions", "action": action, "params": params});
    let mut env_set = Vec::new();
    if let Some(home) = cosh_home(ctx.user_home.as_deref()) {
        env_set.push(("COSH_HOME".to_string(), home.to_string_lossy().into_owned()));
    }
    if let Some(home) = &ctx.user_home {
        env_set.push(("HOME".to_string(), home.to_string_lossy().into_owned()));
    }
    let output = ctx.ops.run_framework_cli_json(FrameworkCommand {
        program: resolve_core(ctx),
        args: vec!["--registry".to_string()],
        stdin: Some(format!("{request}\n").into_bytes()),
        env_set,
        env_remove: Vec::new(),
        path_prepend: Vec::new(),
        timeout: Duration::from_secs(30),
    })?;
    if !output.success() {
        return Err(native_error(format!(
            "cosh registry {action} failed: {}",
            super::util::cli_failure_reason(action, &output)
        )));
    }
    let response: Value = serde_json::from_str(output.stdout.trim()).map_err(|error| {
        native_error(format!("invalid cosh registry {action} response: {error}"))
    })?;
    if response.get("type").and_then(Value::as_str) != Some("registry_response")
        || response.get("request_id").and_then(Value::as_str) != Some("anolisa-adapter")
        || response.get("success").and_then(Value::as_bool) != Some(true)
    {
        return Err(native_error(format!(
            "cosh registry {action}: {}; update cosh-ng if this action is unsupported",
            response.get("error").unwrap_or(&Value::Null)
        )));
    }
    response
        .get("data")
        .cloned()
        .ok_or_else(|| native_error(format!("cosh registry {action} omitted data")))
}

fn resolve_core(ctx: &DriverCtx) -> String {
    if let Some(program) = std::env::var_os("COSH_CORE_PATH").filter(|value| !value.is_empty()) {
        return program.to_string_lossy().into_owned();
    }
    let mut candidates = vec![ctx.layout.libexec_dir.join("cosh-ng/cosh-core")];
    if let Some(bin) = detect_cosh_binary().and_then(|bin| bin.canonicalize().ok())
        && let Some(parent) = bin.parent()
    {
        candidates.push(parent.join("cosh-core"));
        if let Some(prefix) = parent.parent() {
            candidates.push(prefix.join("libexec/anolisa/cosh-ng/cosh-core"));
            candidates.push(prefix.join("lib/anolisa/libexec/cosh-ng/cosh-core"));
        }
    }
    candidates.extend([
        PathBuf::from("/usr/local/libexec/anolisa/cosh-ng/cosh-core"),
        PathBuf::from("/usr/libexec/anolisa/cosh-ng/cosh-core"),
    ]);
    candidates
        .into_iter()
        .find(|path| super::driver::is_executable(path))
        .or_else(|| find_binary_in_path("cosh-core"))
        .unwrap_or_else(|| PathBuf::from("cosh-core"))
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// First cosh-family binary found on PATH, honoring the `COSH_BIN`
/// override (which may name any executable, not only the defaults).
fn detect_cosh_binary() -> Option<PathBuf> {
    if let Some(bin) = std::env::var("COSH_BIN").ok().filter(|s| !s.is_empty()) {
        return find_binary_in_path(&bin);
    }
    COSH_BINARIES
        .iter()
        .find_map(|name| find_binary_in_path(name))
}

/// True when `dir` carries ANOLISA's ownership marker, proving ANOLISA
/// created (and may replace/remove) it.
fn is_anolisa_owned(dir: &Path) -> bool {
    dir.join(COSH_OWNERSHIP_MARKER).is_file()
}

/// True when `dir` has no entries (a fresh, safe-to-replace destination).
/// A read error is treated as "not empty" so we fail closed and refuse to
/// clobber a directory we could not inspect.
fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

/// Resolve the cosh home directory from `COSH_HOME` or the caller's home.
fn cosh_home(user_home: Option<&Path>) -> Option<PathBuf> {
    std::env::var_os("COSH_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| user_home.map(|home| home.join(".copilot-shell")))
}

/// Destination extension directory `<cosh_home>/extensions/<plugin_id>`.
fn extension_dir(bundle: &AdapterBundle, ctx: &DriverCtx) -> Result<PathBuf, AdapterError> {
    let home = cosh_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
        program: "cosh".to_string(),
        reason: "cannot resolve cosh home (no $HOME and no COSH_HOME)".to_string(),
    })?;
    let id = bundle
        .plugin_id
        .clone()
        .unwrap_or_else(|| ctx.component.clone());
    Ok(home.join("extensions").join(id))
}

/// Extract the extension directory path from a receipt's external-path
/// resource.
fn claim_extension_dir(claim: &AdapterClaim) -> Option<PathBuf> {
    let id = match &claim.driver_payload {
        DriverPayload::Cosh(c) => c.extension_dir_resource.as_str(),
        _ => return None,
    };
    claim.resource(id).and_then(|res| match &res.kind {
        ClaimResourceKind::ExternalPath { path } => Some(path.clone()),
        _ => None,
    })
}

/// Roll the detect and tree-present signals into a summary, honoring a
/// `cleanup_failed` receipt.
fn summarize(
    claim_status: ClaimStatus,
    framework_detected: bool,
    tree_present: bool,
) -> AdapterSummary {
    if claim_status == ClaimStatus::CleanupFailed {
        return AdapterSummary::CleanupFailed;
    }
    if !tree_present {
        return AdapterSummary::Degraded;
    }
    if !framework_detected {
        return AdapterSummary::Degraded;
    }
    AdapterSummary::Healthy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::driver::{AdapterOps, CliOutput, FrameworkCommand};
    use crate::adapter::managed_files::{
        ManagedFile, ManagedInventory, ManagedInventoryKind, materialized_files, source_revision,
    };
    use sha2::{Digest, Sha256};
    use std::sync::{Mutex, MutexGuard};

    /// Serializes cosh env mutation across tests in this module.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved_bin: Option<std::ffi::OsString>,
        saved_home: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn acquire() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let g = Self {
                _lock: lock,
                saved_bin: std::env::var_os("COSH_BIN"),
                saved_home: std::env::var_os("COSH_HOME"),
            };
            // SAFETY: guard holds ENV_LOCK for the test's duration.
            unsafe {
                std::env::remove_var("COSH_BIN");
                std::env::remove_var("COSH_HOME");
            }
            g
        }
        fn set_home(&self, home: &Path) {
            // SAFETY: guard holds ENV_LOCK.
            unsafe { std::env::set_var("COSH_HOME", home) }
        }
        /// Force binary detection to miss, isolating the test from any real
        /// cosh-family CLI on the host PATH.
        fn set_bin_absent(&self) {
            // SAFETY: guard holds ENV_LOCK.
            unsafe { std::env::set_var("COSH_BIN", "cosh-does-not-exist-xyz-123") }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: guard holds ENV_LOCK until restore completes.
            unsafe {
                match &self.saved_bin {
                    Some(v) => std::env::set_var("COSH_BIN", v),
                    None => std::env::remove_var("COSH_BIN"),
                }
                match &self.saved_home {
                    Some(v) => std::env::set_var("COSH_HOME", v),
                    None => std::env::remove_var("COSH_HOME"),
                }
            }
        }
    }

    /// Filesystem effects plus a deterministic native registry fixture.
    #[derive(Default)]
    struct FsOps {
        fail_action: Option<&'static str>,
        calls: std::cell::RefCell<Vec<String>>,
    }
    impl AdapterOps for FsOps {
        fn run_framework_cli(&self, cmd: FrameworkCommand) -> Result<CliOutput, AdapterError> {
            let request: Value = serde_json::from_slice(cmd.stdin.as_deref().unwrap()).unwrap();
            let action = request["action"].as_str().unwrap();
            self.calls.borrow_mut().push(action.to_string());
            if self.fail_action == Some(action) {
                return Ok(CliOutput { status: Some(0), timed_out: false, stderr: String::new(),
                    stdout: json!({"type":"registry_response", "request_id":"anolisa-adapter", "success":false,"error":"injected registry failure"}).to_string() });
            }
            let name = request["params"]["name"].as_str().unwrap_or("tokenless");
            let home = cmd
                .env_set
                .iter()
                .find(|(name, _)| name == "COSH_HOME")
                .unwrap()
                .1
                .clone();
            let path = PathBuf::from(home).join("extensions").join(name);
            let data = if request["action"] == "adapter-capabilities" {
                json!({"conditional_clear_user_source": true})
            } else {
                json!({"name": name, "available_source_identities": {"user": path}, "source_identity": path,
                    "path": path, "desired_state": "enabled", "health": "healthy"})
            };
            Ok(CliOutput { status: Some(0), timed_out: false, stderr: String::new(),
                stdout: json!({"type":"registry_response", "request_id":"anolisa-adapter", "success":true,"data":data}).to_string() })
        }
        fn copy_tree(&self, src: &Path, dst: &Path) -> Result<(), AdapterError> {
            copy_dir(src, dst).map_err(|source| AdapterError::Io {
                path: dst.to_path_buf(),
                source,
            })
        }
        fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), AdapterError> {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|source| AdapterError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::copy(src, dst)
                .map(|_| ())
                .map_err(|source| AdapterError::Io {
                    path: dst.to_path_buf(),
                    source,
                })
        }
        fn remove_tree(&self, path: &Path) -> Result<bool, AdapterError> {
            if !path.exists() {
                return Ok(false);
            }
            std::fs::remove_dir_all(path).map_err(|source| AdapterError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            Ok(true)
        }
        fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), AdapterError> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| AdapterError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(path, contents).map_err(|source| AdapterError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
        fn create_symlink(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
            panic!("cosh driver must not create symlinks");
        }
        fn read_file(&self, _: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
            unimplemented!()
        }
    }

    fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let s = entry.path();
            let d = dst.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&s, &d)?;
            } else {
                std::fs::copy(&s, &d)?;
            }
        }
        Ok(())
    }

    fn staged_resource_root(dir: &Path) -> PathBuf {
        let root = dir.join("common");
        std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
        std::fs::write(root.join(COSH_MANIFEST), br#"{"name":"tokenless"}"#).expect("manifest");
        std::fs::write(root.join("hooks/run-hook.sh"), b"#!/bin/sh\n").expect("hook");
        root
    }

    fn attach_materialized_inventory(claim: &mut AdapterClaim, root: &Path) {
        let managed = [root.join(COSH_MANIFEST), root.join("hooks/run-hook.sh")]
            .into_iter()
            .map(|path| ManagedFile {
                sha256: Some(format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(&path).expect("managed fixture bytes"))
                )),
                path,
                kind: ManagedInventoryKind::File,
                symlink_target: None,
            })
            .collect();
        let inventory = ManagedInventory { files: managed };
        let mappings = vec![MaterializedMapping {
            resource_id: RES_EXTENSION_DIR.to_string(),
            source_root: root.to_path_buf(),
            excluded_prefixes: Vec::new(),
        }];
        claim.source_revision =
            Some(source_revision(&inventory, root, &mappings).expect("revision"));
        claim.materialized_files =
            materialized_files(&inventory, &mappings).expect("materialized inventory");
    }

    fn ctx<'a>(
        resource_root: &Path,
        user_home: &Path,
        ops: &'a FsOps,
        layout: &'a anolisa_platform::fs_layout::FsLayout,
    ) -> DriverCtx<'a> {
        DriverCtx {
            component: "tokenless".to_string(),
            framework: "cosh".to_string(),
            layout,
            resource_root: resource_root.to_path_buf(),
            user_home: Some(user_home.to_path_buf()),
            declared_plugin_id: Some("tokenless".to_string()),
            requested_profiles: Vec::new(),
            adapter_type: Some("extension".to_string()),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            no_accept_capabilities: false,
            dry_run: false,
            ops,
        }
    }

    #[test]
    fn native_failure_prevents_delivery_and_activation_failure_is_retryable() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let user_home = tmp.path().join("home");
        let cosh = tmp.path().join("cosh-home");
        guard.set_home(&cosh);
        let resource_root = staged_resource_root(tmp.path());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let old_core = FsOps {
            fail_action: Some("adapter-capabilities"),
            ..FsOps::default()
        };
        let context = ctx(&resource_root, &user_home, &old_core, &layout);
        let driver = CoshDriver::new();
        let bundle = driver.read_bundle(&context).unwrap();
        assert!(driver.prepare_enable(&bundle, &context).is_err());
        assert!(!cosh.exists(), "unsupported core must fail before delivery");

        let ops = FsOps {
            fail_action: Some("select-source"),
            ..FsOps::default()
        };
        let context = ctx(&resource_root, &user_home, &ops, &layout);
        let (mut claim, prepared) = driver.prepare_enable(&bundle, &context).unwrap();
        attach_materialized_inventory(&mut claim, &resource_root);
        assert!(
            driver
                .apply_enable(&mut claim, &prepared, &context, &mut ())
                .is_err()
        );
        assert!(!ops.calls.borrow().iter().any(|action| action == "enable"));
        claim.status = ClaimStatus::CleanupFailed;
        let recovered = driver.disable(&claim, &context).unwrap();
        assert!(recovered.cleanup_complete);
        assert!(!cosh.join("extensions/tokenless").exists());
        assert_eq!(ops.calls.borrow().last().unwrap(), "clear-user-source");
        std::fs::remove_dir_all(cosh.join("extensions")).unwrap();
        assert!(driver.disable(&claim, &context).unwrap().cleanup_complete);
    }

    #[test]
    fn canonical_identity_survives_symlink_home_and_missing_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let extension = alias.join("extensions/example");
        let expected = real
            .join("extensions/example")
            .to_string_lossy()
            .into_owned();
        assert_eq!(canonical_identity(&extension).unwrap(), expected);
        std::fs::create_dir_all(&extension).unwrap();
        assert!(configured(
            &json!({"source_identity":expected, "path":extension, "desired_state":"enabled", "health":"degraded"}),
            &expected
        ));
        assert!(!configured(
            &json!({"source_identity":expected, "path":extension, "desired_state":"enabled", "health":"conflict"}),
            &expected
        ));
    }

    #[test]
    fn enable_status_disable_copies_and_removes_only_extension_dir() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        let cosh = tmp.path().join("cosh-home");
        guard.set_home(&cosh);
        // A pre-existing sibling extension must survive disable.
        let sibling = cosh.join("extensions").join("other");
        std::fs::create_dir_all(&sibling).expect("sibling");
        std::fs::write(sibling.join("keep.txt"), b"keep").expect("keep");

        let resource_root = staged_resource_root(tmp.path());
        let ops = FsOps::default();
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource_root, &user_home, &ops, &layout);
        let driver = CoshDriver::new();

        let bundle = driver.read_bundle(&ctx).expect("read bundle");
        assert_eq!(bundle.plugin_id.as_deref(), Some("tokenless"));

        let (mut claim, prepared) = driver.prepare_enable(&bundle, &ctx).expect("claim");
        attach_materialized_inventory(&mut claim, &resource_root);
        driver
            .apply_enable(&mut claim, &prepared, &ctx, &mut ())
            .expect("apply");

        let ext_dir = cosh.join("extensions").join("tokenless");
        assert!(ext_dir.join(COSH_MANIFEST).is_file(), "manifest copied");
        assert!(ext_dir.join("hooks/run-hook.sh").is_file(), "tree copied");

        let mut legacy_claim = claim.clone();
        legacy_claim.plugin_id = None;
        assert!(
            driver
                .plan_reenable_cleanup(&legacy_claim, &ctx)
                .unwrap()
                .is_empty()
        );
        assert!(
            driver
                .cleanup_replaced_claim(&legacy_claim, &claim, &ctx)
                .unwrap()
                .cleanup_complete
        );
        assert!(
            ext_dir.is_dir(),
            "equivalent legacy identity must not be removed"
        );
        assert!(
            !ops.calls
                .borrow()
                .iter()
                .any(|action| action == "clear-user-source")
        );

        let report = driver.status(&claim, &ctx).expect("status");
        assert_eq!(report.summary, AdapterSummary::Unknown);
        assert!(
            report
                .conditions
                .iter()
                .any(|c| c.kind == AdapterConditionKind::TreePresent
                    && c.status == ConditionStatus::True)
        );

        let disabled = driver.disable(&claim, &ctx).expect("disable");
        assert!(disabled.cleanup_complete);
        assert!(!ext_dir.exists(), "extension dir removed");
        assert!(
            sibling.join("keep.txt").is_file(),
            "disable must not touch other extensions"
        );
    }

    #[test]
    fn enable_refuses_to_overwrite_non_anolisa_extension() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        let cosh = tmp.path().join("cosh-home");
        guard.set_home(&cosh);
        // A user-installed extension of the same name, WITHOUT our marker.
        let ext_dir = cosh.join("extensions").join("tokenless");
        std::fs::create_dir_all(&ext_dir).expect("ext dir");
        std::fs::write(ext_dir.join("user-file"), b"precious user data").expect("user file");

        let resource_root = staged_resource_root(tmp.path());
        let ops = FsOps::default();
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource_root, &user_home, &ops, &layout);
        let driver = CoshDriver::new();

        let bundle = driver.read_bundle(&ctx).expect("read bundle");
        let (mut claim, prepared) = driver.prepare_enable(&bundle, &ctx).expect("claim");
        let err = driver
            .apply_enable(&mut claim, &prepared, &ctx, &mut ())
            .expect_err("must refuse to clobber non-ANOLISA extension");
        assert!(
            matches!(err, AdapterError::InvalidAdapterInput { .. }),
            "got {err:?}"
        );
        // The user's file must be untouched.
        assert_eq!(
            std::fs::read_to_string(ext_dir.join("user-file")).expect("user file kept"),
            "precious user data"
        );
    }

    #[test]
    fn read_bundle_rejects_missing_manifest() {
        let _guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("common");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("stray.txt"), b"x").expect("write");
        let user_home = tmp.path().join("home");
        let ops = FsOps::default();
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&root, &user_home, &ops, &layout);
        let err = CoshDriver::new()
            .read_bundle(&ctx)
            .expect_err("missing manifest must fail");
        assert!(matches!(err, AdapterError::BundleInvalid { .. }));
    }

    #[test]
    fn detect_uses_home_when_cli_absent() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join(".copilot-shell");
        std::fs::create_dir_all(&home).expect("home");
        guard.set_home(&home);
        guard.set_bin_absent();
        let result = CoshDriver::new().detect(&HostEnv {
            user_home: Some(tmp.path().to_path_buf()),
        });
        assert!(result.detected, "existing home is a weak detect signal");
    }

    #[test]
    fn detect_false_without_cli_or_home() {
        let guard = EnvGuard::acquire();
        guard.set_bin_absent();
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = CoshDriver::new().detect(&HostEnv {
            user_home: Some(tmp.path().join("nonexistent-home")),
        });
        assert!(!result.detected);
    }
}
