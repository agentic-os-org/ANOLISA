//! Qwen Code framework driver.
//!
//! Qwen Code owns both extension artifacts and activation policy. ANOLISA
//! therefore performs every mutation through `qwen extensions` and treats
//! the native install metadata plus activation projection as postconditions:
//!
//! ```text
//! qwen extensions link <resource_root>
//! qwen extensions enable <plugin>  # only when native policy disables it
//! ```
//!
//! The driver never writes `extension-store/state.json` or
//! `extension-enablement.json`. It reads the latter only because Qwen keeps it
//! as the compatibility projection used by both the legacy and transactional
//! extension stores. A linked extension is considered ANOLISA-owned only when
//! `.qwen-extension-install.json` records `type = "link"` and its source is
//! exactly the receipt's resource root.
//!
//! Env contract: `QWEN_BIN` overrides the executable. `QWEN_HOME` follows
//! Qwen's process-environment and user-level `.env` bootstrap semantics,
//! including tilde expansion and relative paths resolved from the current
//! working directory.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use semver::Version;
use serde::Deserialize;

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    DRIVER_SCHEMA_VERSION, DriverPayload, QwenCodeClaim, validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx, DriverPlan,
    FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable, find_binary_in_path,
};
use super::util::{bool_status, cli_failure_reason, display_command, now_iso8601};

const CLI_TIMEOUT: Duration = Duration::from_secs(60);
// tokenless declares the same floor; earlier Qwen releases expose the
// extension commands but do not honor the QWEN_HOME contract used here.
const MIN_QWEN_VERSION: Version = Version::new(0, 17, 0);
const QWEN_MANIFEST: &str = "qwen-extension.json";
const INSTALL_METADATA: &str = ".qwen-extension-install.json";
const ENABLEMENT_FILE: &str = "extension-enablement.json";

const RES_EXTENSION_DIR: &str = "qwencode_extension_dir";
const RES_PLUGIN: &str = "qwencode_plugin";

/// Qwen Code driver. Stateless; operation state is carried by the receipt.
pub struct QwenCodeDriver;

impl QwenCodeDriver {
    /// Construct the driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for QwenCodeDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for QwenCodeDriver {
    fn name(&self) -> &'static str {
        "qwencode"
    }

    fn probe_bundle(&self, resource_root: &Path, _declared_entry: Option<&str>) -> bool {
        // Mirrors read_bundle's mandatory check: the Qwen-native root
        // manifest must be present. The declared entry is ignored here —
        // read_bundle rejects any entry other than the native manifest,
        // so consulting it could only make this probe stricter.
        resource_root.join(QWEN_MANIFEST).is_file()
    }

    fn detect(&self, _env: &HostEnv) -> DetectResult {
        match find_binary_in_path(&qwen_program()) {
            Some(path) => DetectResult {
                detected: true,
                reason: format!("qwen CLI found at {}", path.display()),
            },
            None => DetectResult {
                detected: false,
                reason: "qwen CLI not found on PATH".to_string(),
            },
        }
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        qwen_home(ctx.user_home.as_deref()).into_iter().collect()
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
            && entry != QWEN_MANIFEST
        {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!(
                    "Qwen bundle entry must be the native root manifest '{QWEN_MANIFEST}', got '{entry}'"
                ),
            });
        }
        let manifest = root.join(QWEN_MANIFEST);
        let bytes = std::fs::read(&manifest).map_err(|source| AdapterError::BundleInvalid {
            root: root.clone(),
            reason: format!(
                "Qwen extension manifest '{}' is missing or unreadable: {source}",
                manifest.display()
            ),
        })?;
        let manifest: ExtensionManifest =
            serde_json::from_slice(&bytes).map_err(|source| AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!(
                    "invalid Qwen extension manifest '{}': {source}",
                    manifest.display()
                ),
            })?;
        let plugin = manifest.name;
        validate_plugin_id(&plugin)?;
        if let Some(declared) = ctx
            .declared_plugin_id
            .as_deref()
            .filter(|value| !value.is_empty())
            && declared != plugin
        {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: format!(
                    "Qwen manifest name '{plugin}' does not match declared plugin_id '{declared}'"
                ),
            });
        }

        Ok(AdapterBundle {
            resource_root: root.clone(),
            plugin_id: Some(plugin),
        })
    }

    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError> {
        let plugin = bundle_plugin(bundle)?;
        let home =
            qwen_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
                program: qwen_program(),
                reason: "cannot resolve Qwen home (no HOME and no QWEN_HOME)".to_string(),
            })?;
        let source = bundle.resource_root.display().to_string();
        let link = build_qwen_link_command(&home, &source);
        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions: vec![
                format!(
                    "link Qwen Code extension '{plugin}' from {}",
                    bundle.resource_root.display()
                ),
                format!("enable Qwen Code extension '{plugin}' in user scope"),
                "verify native registration source and activation policy".to_string(),
            ],
            register_command: Some(display_command(&link)),
        })
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let plugin = bundle_plugin(bundle)?;
        let home =
            qwen_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
                program: qwen_program(),
                reason: "cannot resolve Qwen home (no HOME and no QWEN_HOME)".to_string(),
            })?;
        let extension_dir = home.join("extensions").join(&plugin);
        let resources = vec![
            ClaimResource {
                id: RES_EXTENSION_DIR.to_string(),
                purpose: "qwencode_extension_dir".to_string(),
                kind: ClaimResourceKind::ExternalPath {
                    path: extension_dir,
                },
            },
            ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: "qwencode_plugin".to_string(),
                kind: ClaimResourceKind::FrameworkPlugin {
                    framework: self.name().to_string(),
                    plugin_id: plugin.clone(),
                },
            },
        ];

        let claim = AdapterClaim {
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
            driver_payload: DriverPayload::QwenCode(QwenCodeClaim {
                extension_dir_resource: RES_EXTENSION_DIR.to_string(),
                plugin_resource: RES_PLUGIN.to_string(),
            }),
        };
        Ok((claim, PreparedEnable::None))
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        _prepared: &PreparedEnable,
        ctx: &DriverCtx,
        _progress: &mut dyn super::driver::EnableProgress,
    ) -> Result<(), AdapterError> {
        let layout = QwenLayout::from_claim(claim)?;
        ensure_current_home(&layout, ctx)?;
        ensure_supported_version(&layout.home, ctx)?;
        let activation = probe_activation(&layout, ctx)?;
        ensure_activation_readable(&activation)?;

        match probe_registration(&layout, claim, ctx)? {
            RegistrationProbe::Owned => {}
            RegistrationProbe::Absent => {
                let source = claim.resource_root.display().to_string();
                let output = ctx
                    .ops
                    .run_framework_cli(build_qwen_link_command(&layout.home, &source))?;
                if !output.success() {
                    return Err(AdapterError::FrameworkCli {
                        program: qwen_program(),
                        reason: cli_failure_reason("extensions link", &output),
                    });
                }
                match probe_registration(&layout, claim, ctx)? {
                    RegistrationProbe::Owned => {}
                    other => {
                        return Err(AdapterError::FrameworkCli {
                            program: qwen_program(),
                            reason: format!(
                                "'extensions link' exited successfully but registration postcondition failed: {}",
                                other.reason()
                            ),
                        });
                    }
                }
            }
            other => {
                return Err(AdapterError::InvalidAdapterInput {
                    component: ctx.component.clone(),
                    framework: self.name().to_string(),
                    reason: format!(
                        "refusing to replace Qwen extension '{}': {}",
                        layout.plugin,
                        other.reason()
                    ),
                });
            }
        }

        match probe_activation(&layout, ctx)? {
            ActivationProbe::Enabled { .. } => Ok(()),
            ActivationProbe::Disabled(_) => {
                let output = ctx.ops.run_framework_cli(build_qwen_command(
                    &layout.home,
                    ["extensions", "enable", layout.plugin.as_str()],
                ))?;
                if !output.success() {
                    return Err(AdapterError::FrameworkCli {
                        program: qwen_program(),
                        reason: cli_failure_reason("extensions enable", &output),
                    });
                }
                match probe_activation(&layout, ctx)? {
                    ActivationProbe::Enabled { .. } => Ok(()),
                    other => Err(AdapterError::FrameworkCli {
                        program: qwen_program(),
                        reason: format!(
                            "'extensions enable' exited successfully but activation postcondition failed: {}",
                            other.reason()
                        ),
                    }),
                }
            }
            other => Err(AdapterError::FrameworkCli {
                program: qwen_program(),
                reason: format!(
                    "refusing to mutate unreadable Qwen activation state: {}",
                    other.reason()
                ),
            }),
        }
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        let layout = QwenLayout::from_claim(claim)?;
        let detect = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        let registration = probe_registration(&layout, claim, ctx)?;
        let registration_status = registration.status();
        let activation = probe_activation(&layout, ctx)?;
        let activation_status = activation.status();
        let (version_status, version_reason) = if detect.detected {
            match qwen_version(&layout.home, ctx) {
                Ok(version) if version >= MIN_QWEN_VERSION => (ConditionStatus::True, None),
                Ok(version) => (
                    ConditionStatus::False,
                    Some(format!(
                        "Qwen {version} is unsupported; qwencode adapters require >= {MIN_QWEN_VERSION}"
                    )),
                ),
                Err(error) => (ConditionStatus::Unknown, Some(error.to_string())),
            }
        } else {
            (
                ConditionStatus::False,
                Some("qwen CLI is unavailable; version cannot be verified".to_string()),
            )
        };
        let (verification_status, verification_reason) = if version_status != ConditionStatus::True
        {
            (version_status, version_reason)
        } else if registration_status == ConditionStatus::Unknown
            || activation_status == ConditionStatus::Unknown
        {
            (
                ConditionStatus::Unknown,
                Some("Qwen native registration or activation state is unreadable".to_string()),
            )
        } else {
            (ConditionStatus::True, None)
        };

        let conditions = vec![
            AdapterCondition {
                kind: AdapterConditionKind::FrameworkDetected,
                status: bool_status(detect.detected),
                reason: Some(detect.reason),
                resource: None,
            },
            AdapterCondition {
                kind: AdapterConditionKind::PluginRegistered,
                status: registration_status,
                reason: registration.condition_reason(),
                resource: Some(ClaimResourceRef {
                    id: RES_PLUGIN.to_string(),
                }),
            },
            AdapterCondition {
                kind: AdapterConditionKind::ActivationEnabled,
                status: activation_status,
                reason: activation.condition_reason(),
                resource: Some(ClaimResourceRef {
                    id: RES_EXTENSION_DIR.to_string(),
                }),
            },
            AdapterCondition {
                kind: AdapterConditionKind::VerificationSupported,
                status: verification_status,
                reason: verification_reason,
                resource: None,
            },
        ];
        let summary = summarize(
            claim.status,
            detect.detected,
            registration_status,
            activation_status,
            verification_status,
        );
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
        let layout = QwenLayout::from_claim(claim)?;
        if let Err(error) = ensure_current_home(&layout, ctx) {
            return Ok(incomplete(error.to_string()));
        }

        let registration = probe_registration(&layout, claim, ctx)?;
        let activation = probe_activation(&layout, ctx)?;
        if let ActivationProbe::Unknown(reason) = &activation {
            return Ok(incomplete(format!(
                "Qwen activation state is unreadable; refusing native cleanup: {reason}"
            )));
        }
        if matches!(registration, RegistrationProbe::Absent) && activation.is_clean() {
            return Ok(DisableReport {
                cleanup_complete: true,
                messages: vec![format!(
                    "Qwen extension '{}' and its activation policy are already absent",
                    layout.plugin
                )],
            });
        }
        if let RegistrationProbe::Conflict(reason) | RegistrationProbe::Unknown(reason) =
            &registration
        {
            return Ok(incomplete(format!(
                "Qwen extension '{}' is not provably ANOLISA-owned: {reason}",
                layout.plugin
            )));
        }
        if matches!(registration, RegistrationProbe::Absent) {
            return Ok(incomplete(format!(
                "Qwen registration is absent but activation policy remains; refusing to relink '{}' during cleanup",
                layout.plugin
            )));
        }
        match casefold_collision(&layout) {
            Ok(Some(collision)) => {
                return Ok(incomplete(format!(
                    "Qwen extension '{}' has a case-insensitive name collision with '{collision}'; refusing name-only uninstall",
                    layout.plugin
                )));
            }
            Ok(None) => {}
            Err(error) => {
                return Ok(incomplete(format!(
                    "could not verify Qwen extension name uniqueness; refusing name-only uninstall: {error}"
                )));
            }
        }
        if find_binary_in_path(&qwen_program()).is_none() {
            return Ok(incomplete(
                "qwen CLI not found; receipt kept so native cleanup can be retried".to_string(),
            ));
        }
        if let Err(error) = ensure_supported_version(&layout.home, ctx) {
            return Ok(incomplete(format!(
                "Qwen version preflight failed; receipt kept: {error}"
            )));
        }

        let mut messages = Vec::new();
        let output = ctx.ops.run_framework_cli(build_qwen_command(
            &layout.home,
            ["extensions", "uninstall", layout.plugin.as_str()],
        ))?;
        let registration_after = probe_registration(&layout, claim, ctx)?;
        let activation_after = probe_activation(&layout, ctx)?;
        let cleanup_complete =
            matches!(registration_after, RegistrationProbe::Absent) && activation_after.is_clean();
        if cleanup_complete {
            messages.push(format!("uninstalled Qwen extension '{}'", layout.plugin));
        } else {
            messages.push(format!(
                "Qwen uninstall did not satisfy cleanup postconditions: registration={}, activation={}",
                registration_after.reason(),
                activation_after.reason()
            ));
            if !output.success() {
                messages.push(cli_failure_reason("extensions uninstall", &output));
            }
        }

        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }
}

#[derive(Debug)]
struct QwenLayout {
    home: PathBuf,
    extension_dir: PathBuf,
    enablement_file: PathBuf,
    plugin: String,
}

impl QwenLayout {
    fn from_claim(claim: &AdapterClaim) -> Result<Self, AdapterError> {
        let payload = match &claim.driver_payload {
            DriverPayload::QwenCode(payload) => payload,
            _ => return Err(malformed_claim(claim, "driver payload is not qwencode")),
        };
        let extension_dir =
            external_path(claim, &payload.extension_dir_resource).ok_or_else(|| {
                malformed_claim(
                    claim,
                    "extension directory resource is missing or not a path",
                )
            })?;
        let plugin = framework_plugin(claim, &payload.plugin_resource).ok_or_else(|| {
            malformed_claim(
                claim,
                "plugin resource is missing or targets another framework",
            )
        })?;
        validate_plugin_id(&plugin)?;
        if claim
            .plugin_id
            .as_deref()
            .is_some_and(|value| value != plugin)
        {
            return Err(malformed_claim(
                claim,
                "top-level plugin id does not match plugin resource",
            ));
        }
        let Some(extensions_dir) = extension_dir.parent() else {
            return Err(malformed_claim(claim, "extension directory has no parent"));
        };
        if extensions_dir.file_name() != Some(OsStr::new("extensions")) {
            return Err(malformed_claim(
                claim,
                "extension directory is not under a Qwen extensions directory",
            ));
        }
        let Some(home) = extensions_dir.parent() else {
            return Err(malformed_claim(
                claim,
                "Qwen extensions directory has no parent",
            ));
        };
        if extension_dir != home.join("extensions").join(&plugin) {
            return Err(malformed_claim(
                claim,
                "extension directory does not match home/extensions/<plugin>",
            ));
        }
        let home = home.to_path_buf();
        Ok(Self {
            enablement_file: home.join("extensions").join(ENABLEMENT_FILE),
            home,
            extension_dir,
            plugin,
        })
    }
}

#[derive(Debug)]
enum RegistrationProbe {
    Owned,
    Absent,
    Conflict(String),
    Unknown(String),
}

impl RegistrationProbe {
    fn status(&self) -> ConditionStatus {
        match self {
            Self::Owned => ConditionStatus::True,
            Self::Absent | Self::Conflict(_) => ConditionStatus::False,
            Self::Unknown(_) => ConditionStatus::Unknown,
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Owned => "registered from the receipt resource root".to_string(),
            Self::Absent => "extension entry is absent".to_string(),
            Self::Conflict(reason) | Self::Unknown(reason) => reason.clone(),
        }
    }

    fn condition_reason(&self) -> Option<String> {
        match self {
            Self::Owned => None,
            _ => Some(self.reason()),
        }
    }
}

#[derive(Debug)]
enum ActivationProbe {
    Enabled { policy_present: bool },
    Disabled(String),
    Unknown(String),
}

impl ActivationProbe {
    fn status(&self) -> ConditionStatus {
        match self {
            Self::Enabled { .. } => ConditionStatus::True,
            Self::Disabled(_) => ConditionStatus::False,
            Self::Unknown(_) => ConditionStatus::Unknown,
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Enabled {
                policy_present: false,
            } => "enabled by Qwen's default policy".to_string(),
            Self::Enabled {
                policy_present: true,
            } => "enabled by Qwen's activation policy".to_string(),
            Self::Disabled(reason) | Self::Unknown(reason) => reason.clone(),
        }
    }

    fn condition_reason(&self) -> Option<String> {
        match self {
            Self::Enabled { .. } => None,
            _ => Some(self.reason()),
        }
    }

    fn is_clean(&self) -> bool {
        matches!(
            self,
            Self::Enabled {
                policy_present: false
            }
        )
    }
}

#[derive(Deserialize)]
struct ExtensionManifest {
    name: String,
}

#[derive(Deserialize)]
struct InstallMetadata {
    source: PathBuf,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Default, Deserialize)]
struct EnablementEntry {
    #[serde(default)]
    overrides: Vec<String>,
}

fn probe_registration(
    layout: &QwenLayout,
    claim: &AdapterClaim,
    ctx: &DriverCtx,
) -> Result<RegistrationProbe, AdapterError> {
    if !layout.extension_dir.exists() {
        return Ok(RegistrationProbe::Absent);
    }
    if !layout.extension_dir.is_dir() {
        return Ok(RegistrationProbe::Conflict(format!(
            "{} exists but is not a directory",
            layout.extension_dir.display()
        )));
    }
    let metadata_path = layout.extension_dir.join(INSTALL_METADATA);
    let Some(bytes) = ctx.ops.read_file(&metadata_path)? else {
        return Ok(RegistrationProbe::Conflict(format!(
            "{} has no Qwen install metadata",
            layout.extension_dir.display()
        )));
    };
    let metadata: InstallMetadata = match serde_json::from_slice(&bytes) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Ok(RegistrationProbe::Unknown(format!(
                "cannot parse {}: {error}",
                metadata_path.display()
            )));
        }
    };
    if metadata.kind != "link" {
        return Ok(RegistrationProbe::Conflict(
            "same-name extension was not installed as a link".to_string(),
        ));
    }
    if !paths_equivalent(&metadata.source, &claim.resource_root) {
        return Ok(RegistrationProbe::Conflict(format!(
            "same-name Qwen extension is linked from {}, not {}",
            metadata.source.display(),
            claim.resource_root.display()
        )));
    }
    Ok(RegistrationProbe::Owned)
}

fn casefold_collision(layout: &QwenLayout) -> Result<Option<String>, AdapterError> {
    let extensions_dir =
        layout
            .extension_dir
            .parent()
            .ok_or_else(|| AdapterError::BundleInvalid {
                root: layout.extension_dir.clone(),
                reason: "Qwen extension directory has no parent".to_string(),
            })?;
    let entries = std::fs::read_dir(extensions_dir).map_err(|source| AdapterError::Io {
        path: extensions_dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| AdapterError::Io {
            path: extensions_dir.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| AdapterError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_casefold_collision(&layout.plugin, name) {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

fn is_casefold_collision(plugin: &str, candidate: &str) -> bool {
    candidate != plugin && candidate.eq_ignore_ascii_case(plugin)
}

fn probe_activation(layout: &QwenLayout, ctx: &DriverCtx) -> Result<ActivationProbe, AdapterError> {
    let Some(bytes) = ctx.ops.read_file(&layout.enablement_file)? else {
        return Ok(ActivationProbe::Enabled {
            policy_present: false,
        });
    };
    let config: BTreeMap<String, EnablementEntry> = match serde_json::from_slice(&bytes) {
        Ok(config) => config,
        Err(error) => {
            return Ok(ActivationProbe::Unknown(format!(
                "cannot parse {}: {error}",
                layout.enablement_file.display()
            )));
        }
    };
    let Some(entry) = config.get(&layout.plugin) else {
        return Ok(ActivationProbe::Enabled {
            policy_present: false,
        });
    };
    let Some(user_home) = ctx.user_home.as_deref() else {
        return Ok(ActivationProbe::Unknown(
            "cannot evaluate Qwen user activation without HOME".to_string(),
        ));
    };
    let candidates = activation_candidates(user_home);
    let mut enabled = true;
    for rule in &entry.overrides {
        let Some((is_disable, matches)) = evaluate_rule(rule, &candidates) else {
            return Ok(ActivationProbe::Unknown(format!(
                "activation rule '{rule}' uses an unsupported glob"
            )));
        };
        if matches {
            enabled = !is_disable;
        }
    }
    if enabled {
        Ok(ActivationProbe::Enabled {
            policy_present: true,
        })
    } else {
        Ok(ActivationProbe::Disabled(format!(
            "Qwen user activation policy disables '{}'",
            layout.plugin
        )))
    }
}

fn ensure_activation_readable(activation: &ActivationProbe) -> Result<(), AdapterError> {
    if let ActivationProbe::Unknown(reason) = activation {
        return Err(AdapterError::FrameworkCli {
            program: qwen_program(),
            reason: format!("refusing to mutate unreadable Qwen activation state: {reason}"),
        });
    }
    Ok(())
}

fn ensure_supported_version(home: &Path, ctx: &DriverCtx) -> Result<(), AdapterError> {
    let version = qwen_version(home, ctx)?;
    if version < MIN_QWEN_VERSION {
        return Err(AdapterError::FrameworkCli {
            program: qwen_program(),
            reason: format!(
                "Qwen {version} is unsupported; qwencode adapters require >= {MIN_QWEN_VERSION}"
            ),
        });
    }
    Ok(())
}

fn qwen_version(home: &Path, ctx: &DriverCtx) -> Result<Version, AdapterError> {
    let output = ctx
        .ops
        .run_framework_cli(build_qwen_command(home, ["--version"]))?;
    if !output.success() {
        return Err(AdapterError::FrameworkCli {
            program: qwen_program(),
            reason: cli_failure_reason("--version", &output),
        });
    }
    let version_output = format!("{} {}", output.stdout, output.stderr);
    let version =
        parse_qwen_version(&version_output).ok_or_else(|| AdapterError::FrameworkCli {
            program: qwen_program(),
            reason: format!(
                "cannot parse Qwen version from output '{}'",
                version_output.trim()
            ),
        })?;
    Ok(version)
}

fn ensure_current_home(layout: &QwenLayout, ctx: &DriverCtx) -> Result<(), AdapterError> {
    let current =
        qwen_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
            program: qwen_program(),
            reason: "cannot resolve current Qwen home".to_string(),
        })?;
    if !paths_equivalent(&current, &layout.home) {
        return Err(AdapterError::BundleInvalid {
            root: ctx.resource_root.clone(),
            reason: format!(
                "invalid qwencode receipt: current QWEN_HOME {} differs from receipt home {}",
                current.display(),
                layout.home.display()
            ),
        });
    }
    Ok(())
}

fn malformed_claim(claim: &AdapterClaim, reason: &str) -> AdapterError {
    AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: format!("invalid qwencode receipt: {reason}"),
    }
}

fn incomplete(message: String) -> DisableReport {
    DisableReport {
        cleanup_complete: false,
        messages: vec![message],
    }
}

fn external_path(claim: &AdapterClaim, id: &str) -> Option<PathBuf> {
    claim
        .resource(id)
        .and_then(|resource| match &resource.kind {
            ClaimResourceKind::ExternalPath { path } => Some(path.clone()),
            _ => None,
        })
}

fn framework_plugin(claim: &AdapterClaim, id: &str) -> Option<String> {
    claim
        .resource(id)
        .and_then(|resource| match &resource.kind {
            ClaimResourceKind::FrameworkPlugin {
                framework,
                plugin_id,
            } if framework == "qwencode" => Some(plugin_id.clone()),
            _ => None,
        })
}

fn bundle_plugin(bundle: &AdapterBundle) -> Result<String, AdapterError> {
    let plugin = bundle
        .plugin_id
        .clone()
        .ok_or_else(|| AdapterError::BundleInvalid {
            root: bundle.resource_root.clone(),
            reason: "Qwen extension manifest has no name".to_string(),
        })?;
    validate_plugin_id(&plugin).map_err(AdapterError::from)?;
    Ok(plugin)
}

fn build_qwen_command<I, S>(home: &Path, args: I) -> FrameworkCommand
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    FrameworkCommand {
        program: qwen_program(),
        args: args.into_iter().map(Into::into).collect(),
        stdin: None,
        env_set: vec![
            ("QWEN_HOME".to_string(), home.display().to_string()),
            ("NO_COLOR".to_string(), "1".to_string()),
        ],
        env_remove: Vec::new(),
        path_prepend: Vec::new(),
        timeout: CLI_TIMEOUT,
    }
}

fn build_qwen_link_command(home: &Path, source: &str) -> FrameworkCommand {
    let mut command = build_qwen_command(home, ["extensions", "link", source]);
    // `qwen extensions link` has no consent flag in Qwen 0.17. ANOLISA's
    // explicit enable operation supplies the native prompt response directly.
    command.stdin = Some(b"y\n".to_vec());
    command
}

fn qwen_program() -> String {
    std::env::var("QWEN_BIN")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "qwen".to_string())
}

fn qwen_home(user_home: Option<&Path>) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let configured = match std::env::var_os("QWEN_HOME") {
        Some(value) => Some(value),
        None => user_home.and_then(qwen_home_from_user_env),
    };
    resolve_qwen_home(user_home, &cwd, configured.as_deref())
}

fn qwen_home_from_user_env(user_home: &Path) -> Option<OsString> {
    // Match Qwen's preResolveHomeEnvOverrides order. It ignores unreadable
    // files and keeps the first file's non-empty QWEN_HOME value.
    [user_home.join(".qwen").join(".env"), user_home.join(".env")]
        .into_iter()
        .find_map(|path| {
            let contents = std::fs::read_to_string(path).ok()?;
            parse_qwen_home_env(&contents).map(OsString::from)
        })
}

fn parse_qwen_home_env(contents: &str) -> Option<String> {
    let mut configured = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(str::trim_start)
            .unwrap_or(line);
        let Some((key, raw_value)) = dotenv_assignment(line) else {
            continue;
        };
        if key.trim() != "QWEN_HOME" {
            continue;
        }
        if let Some(value) = parse_dotenv_value(raw_value) {
            configured = Some(value);
        }
    }
    configured.filter(|value| !value.is_empty())
}

fn dotenv_assignment(line: &str) -> Option<(&str, &str)> {
    let key_end = line
        .find(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-'))
        })
        .unwrap_or(line.len());
    if key_end == 0 {
        return None;
    }
    let (key, rest) = line.split_at(key_end);
    if let Some(value) = rest.strip_prefix(':') {
        return value
            .starts_with(char::is_whitespace)
            .then(|| (key, value.trim_start()));
    }
    let value = rest.trim_start().strip_prefix('=')?;
    Some((key, value.trim_start()))
}

fn parse_dotenv_value(raw: &str) -> Option<String> {
    let raw = raw.trim_start();
    let Some(quote) = raw
        .chars()
        .next()
        .filter(|value| matches!(value, '\'' | '"' | '`'))
    else {
        return Some(
            raw.split('#')
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_string(),
        );
    };

    let mut value = String::new();
    let mut escaped = false;
    for character in raw[quote.len_utf8()..].chars() {
        if escaped {
            match (quote, character) {
                ('"', 'n') => value.push('\n'),
                ('"', 'r') => value.push('\r'),
                (_, value_character) if value_character == quote => value.push(value_character),
                (_, value_character) => {
                    value.push('\\');
                    value.push(value_character);
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(value);
        } else {
            value.push(character);
        }
    }
    None
}

fn resolve_qwen_home(
    user_home: Option<&Path>,
    cwd: &Path,
    configured: Option<&OsStr>,
) -> Option<PathBuf> {
    let configured = configured.filter(|value| !value.is_empty());
    let path = match configured {
        Some(value) => {
            let text = value.to_string_lossy();
            if text == "~" {
                user_home?.to_path_buf()
            } else if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\"))
            {
                user_home?.join(rest)
            } else {
                let configured = PathBuf::from(value);
                if configured.is_absolute() {
                    configured
                } else {
                    cwd.join(configured)
                }
            }
        }
        None => user_home?.join(".qwen"),
    };
    Some(normalize_lexically(&path))
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

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    let left = normalize_lexically(left);
    let right = normalize_lexically(right);
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(&left), std::fs::canonicalize(&right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn activation_candidates(user_home: &Path) -> Vec<String> {
    let mut candidates = vec![normalize_rule_path(user_home)];
    if let Ok(canonical) = std::fs::canonicalize(user_home) {
        let canonical = normalize_rule_path(&canonical);
        if !candidates.contains(&canonical) {
            candidates.push(canonical);
        }
    }
    candidates
}

fn normalize_rule_path(path: &Path) -> String {
    let mut result = path.to_string_lossy().replace('\\', "/");
    if !result.starts_with('/') {
        result.insert(0, '/');
    }
    if !result.ends_with('/') {
        result.push('/');
    }
    result
}

fn evaluate_rule(rule: &str, candidates: &[String]) -> Option<(bool, bool)> {
    let is_disable = rule.starts_with('!');
    let pattern = rule.strip_prefix('!').unwrap_or(rule);
    let include_subdirs = pattern.ends_with('*');
    let base = pattern.strip_suffix('*').unwrap_or(pattern);
    if base.contains('*') {
        return None;
    }
    let matches = candidates.iter().any(|candidate| {
        if include_subdirs {
            candidate.starts_with(base)
        } else {
            candidate == base
        }
    });
    Some((is_disable, matches))
}

fn parse_qwen_version(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(|token| {
        let trimmed = token.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | '-' | '+')
        });
        let candidate = trimmed.strip_prefix('v').unwrap_or(trimmed);
        Version::parse(candidate).ok()
    })
}

fn summarize(
    claim_status: ClaimStatus,
    framework_detected: bool,
    registration: ConditionStatus,
    activation: ConditionStatus,
    verification: ConditionStatus,
) -> AdapterSummary {
    if claim_status == ClaimStatus::CleanupFailed {
        return AdapterSummary::CleanupFailed;
    }
    if !framework_detected
        || registration == ConditionStatus::False
        || activation == ConditionStatus::False
        || verification == ConditionStatus::False
    {
        return AdapterSummary::Degraded;
    }
    if registration == ConditionStatus::Unknown
        || activation == ConditionStatus::Unknown
        || verification == ConditionStatus::Unknown
    {
        return AdapterSummary::Unknown;
    }
    AdapterSummary::Healthy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::driver::{AdapterOps, CliOutput};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved_bin: Option<std::ffi::OsString>,
        saved_home: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn acquire() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
            let guard = Self {
                _lock: lock,
                saved_bin: std::env::var_os("QWEN_BIN"),
                saved_home: std::env::var_os("QWEN_HOME"),
            };
            // SAFETY: this module serializes its Qwen environment mutations.
            unsafe {
                std::env::remove_var("QWEN_BIN");
                std::env::remove_var("QWEN_HOME");
            }
            guard
        }

        fn set_bin(&self, path: &Path) {
            // SAFETY: this module serializes its Qwen environment mutations.
            unsafe { std::env::set_var("QWEN_BIN", path) }
        }

        fn set_bin_absent(&self) {
            // SAFETY: this module serializes its Qwen environment mutations.
            unsafe { std::env::set_var("QWEN_BIN", "qwen-missing-anolisa-test") }
        }

        fn set_home(&self, value: &OsStr) {
            // SAFETY: this module serializes its Qwen environment mutations.
            unsafe { std::env::set_var("QWEN_HOME", value) }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: the guard holds ENV_LOCK until restoration finishes.
            unsafe {
                match &self.saved_bin {
                    Some(value) => std::env::set_var("QWEN_BIN", value),
                    None => std::env::remove_var("QWEN_BIN"),
                }
                match &self.saved_home {
                    Some(value) => std::env::set_var("QWEN_HOME", value),
                    None => std::env::remove_var("QWEN_HOME"),
                }
            }
        }
    }

    struct SimOps {
        home: PathBuf,
        user_home: PathBuf,
        version: String,
        link_effect: bool,
        enable_effect: bool,
        uninstall_effect: bool,
        commands: Mutex<Vec<Vec<String>>>,
    }

    impl SimOps {
        fn new(home: PathBuf, user_home: PathBuf) -> Self {
            Self {
                home,
                user_home,
                version: "0.17.0".to_string(),
                link_effect: true,
                enable_effect: true,
                uninstall_effect: true,
                commands: Mutex::new(Vec::new()),
            }
        }

        fn commands(&self) -> Vec<Vec<String>> {
            self.commands
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }

        fn success(&self, stdout: String) -> CliOutput {
            CliOutput {
                status: Some(0),
                timed_out: false,
                stdout,
                stderr: String::new(),
            }
        }

        fn write_enablement(&self, present: bool) -> Result<(), AdapterError> {
            let file = self.home.join("extensions").join(ENABLEMENT_FILE);
            std::fs::create_dir_all(file.parent().unwrap_or(&self.home)).map_err(|source| {
                AdapterError::Io {
                    path: file.clone(),
                    source,
                }
            })?;
            let value = if present {
                serde_json::json!({
                    "tokenless": {
                        "overrides": [format!("{}*", normalize_rule_path(&self.user_home))]
                    }
                })
            } else {
                serde_json::json!({})
            };
            std::fs::write(
                &file,
                serde_json::to_vec_pretty(&value).map_err(|source| {
                    AdapterError::BundleInvalid {
                        root: file.clone(),
                        reason: source.to_string(),
                    }
                })?,
            )
            .map_err(|source| AdapterError::Io { path: file, source })
        }
    }

    impl AdapterOps for SimOps {
        fn run_framework_cli(&self, command: FrameworkCommand) -> Result<CliOutput, AdapterError> {
            self.commands
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(command.args.clone());
            match command.args.as_slice() {
                [version] if version == "--version" => Ok(self.success(self.version.clone())),
                [extensions, link, source] if extensions == "extensions" && link == "link" => {
                    assert_eq!(
                        command.stdin.as_deref(),
                        Some(&b"y\n"[..]),
                        "Qwen link must receive explicit native consent"
                    );
                    if self.link_effect {
                        let extension_dir = self.home.join("extensions").join("tokenless");
                        std::fs::create_dir_all(&extension_dir).map_err(|source| {
                            AdapterError::Io {
                                path: extension_dir.clone(),
                                source,
                            }
                        })?;
                        std::fs::write(
                            extension_dir.join(INSTALL_METADATA),
                            serde_json::to_vec_pretty(&serde_json::json!({
                                "source": source,
                                "type": "link"
                            }))
                            .map_err(|error| {
                                AdapterError::BundleInvalid {
                                    root: extension_dir.clone(),
                                    reason: error.to_string(),
                                }
                            })?,
                        )
                        .map_err(|source| AdapterError::Io {
                            path: extension_dir,
                            source,
                        })?;
                    }
                    Ok(self.success(String::new()))
                }
                [extensions, enable, plugin]
                    if extensions == "extensions"
                        && enable == "enable"
                        && plugin == "tokenless" =>
                {
                    if self.enable_effect {
                        self.write_enablement(true)?;
                    }
                    Ok(self.success(String::new()))
                }
                [extensions, uninstall, plugin]
                    if extensions == "extensions"
                        && uninstall == "uninstall"
                        && plugin == "tokenless" =>
                {
                    if self.uninstall_effect {
                        let extension_dir = self.home.join("extensions").join("tokenless");
                        if extension_dir.exists() {
                            std::fs::remove_dir_all(&extension_dir).map_err(|source| {
                                AdapterError::Io {
                                    path: extension_dir,
                                    source,
                                }
                            })?;
                        }
                        self.write_enablement(false)?;
                    }
                    Ok(self.success(String::new()))
                }
                _ => Ok(self.success(String::new())),
            }
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
            match std::fs::read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(source) => Err(AdapterError::Io {
                    path: path.to_path_buf(),
                    source,
                }),
            }
        }
    }

    fn fake_qwen(dir: &Path) -> PathBuf {
        let path = dir.join("qwen");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write fake qwen");
        let mut permissions = std::fs::metadata(&path)
            .expect("fake qwen metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod fake qwen");
        path
    }

    fn resource_root(dir: &Path) -> PathBuf {
        let root = dir.join("qwencode");
        std::fs::create_dir_all(root.join("hooks")).expect("mkdir bundle");
        std::fs::write(
            root.join(QWEN_MANIFEST),
            br#"{"name":"tokenless","version":"1.0.0"}"#,
        )
        .expect("write manifest");
        std::fs::write(root.join("hooks/run-hook.sh"), b"#!/bin/sh\n").expect("write hook");
        root
    }

    fn ctx<'a>(
        resource_root: &Path,
        user_home: &Path,
        ops: &'a dyn AdapterOps,
        layout: &'a anolisa_platform::fs_layout::FsLayout,
    ) -> DriverCtx<'a> {
        DriverCtx {
            component: "tokenless".to_string(),
            framework: "qwencode".to_string(),
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
            dry_run: false,
            ops,
        }
    }

    fn prepare_claim(
        driver: &QwenCodeDriver,
        ctx: &DriverCtx,
    ) -> Result<AdapterClaim, AdapterError> {
        let bundle = driver.read_bundle(ctx)?;
        driver
            .prepare_enable(&bundle, ctx)
            .map(|(claim, _prepared)| claim)
    }

    fn seed_owned_link(home: &Path, source: &Path) {
        let extension_dir = home.join("extensions").join("tokenless");
        std::fs::create_dir_all(&extension_dir).expect("mkdir extension");
        std::fs::write(
            extension_dir.join(INSTALL_METADATA),
            serde_json::to_vec_pretty(&serde_json::json!({
                "source": source,
                "type": "link"
            }))
            .expect("metadata json"),
        )
        .expect("metadata");
    }

    #[test]
    fn enable_links_enables_and_verifies_native_state() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        let ops = SimOps::new(home, user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let mut claim = prepare_claim(&driver, &ctx).expect("claim");

        driver
            .apply_enable(&mut claim, &PreparedEnable::None, &ctx, &mut ())
            .expect("enable");
        assert_eq!(
            ops.commands(),
            vec![
                vec!["--version".to_string()],
                vec![
                    "extensions".to_string(),
                    "link".to_string(),
                    resource.display().to_string()
                ]
            ]
        );
        let report = driver.status(&claim, &ctx).expect("status");
        assert_eq!(report.summary, AdapterSummary::Healthy);
        assert!(report.conditions.iter().any(|condition| {
            condition.kind == AdapterConditionKind::ActivationEnabled
                && condition.status == ConditionStatus::True
        }));
    }

    #[test]
    fn reenable_repairs_disabled_policy_without_relinking() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        seed_owned_link(&home, &resource);
        let enablement = home.join("extensions").join(ENABLEMENT_FILE);
        std::fs::write(
            &enablement,
            serde_json::to_vec_pretty(&serde_json::json!({
                "tokenless": {
                    "overrides": [format!("!{}*", normalize_rule_path(&user_home))]
                }
            }))
            .expect("enablement json"),
        )
        .expect("enablement");
        let ops = SimOps::new(home, user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let mut claim = prepare_claim(&driver, &ctx).expect("claim");

        driver
            .apply_enable(&mut claim, &PreparedEnable::None, &ctx, &mut ())
            .expect("re-enable");
        assert_eq!(
            ops.commands(),
            vec![
                vec!["--version".to_string()],
                vec![
                    "extensions".to_string(),
                    "enable".to_string(),
                    "tokenless".to_string()
                ]
            ]
        );
        assert_eq!(
            driver.status(&claim, &ctx).expect("status").summary,
            AdapterSummary::Healthy
        );
    }

    #[test]
    fn enable_rejects_exit_zero_without_registration_postcondition() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let resource = resource_root(tmp.path());
        let mut ops = SimOps::new(user_home.join(".qwen"), user_home.clone());
        ops.link_effect = false;
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let mut claim = prepare_claim(&driver, &ctx).expect("claim");

        let error = driver
            .apply_enable(&mut claim, &PreparedEnable::None, &ctx, &mut ())
            .expect_err("missing postcondition must fail");
        assert!(matches!(error, AdapterError::FrameworkCli { .. }));
        assert_eq!(
            ops.commands().len(),
            2,
            "enable must not run after failed link"
        );
    }

    #[test]
    fn enable_refuses_foreign_same_name_extension() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        seed_owned_link(&home, &tmp.path().join("foreign-source"));
        let ops = SimOps::new(home, user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let mut claim = prepare_claim(&driver, &ctx).expect("claim");

        let error = driver
            .apply_enable(&mut claim, &PreparedEnable::None, &ctx, &mut ())
            .expect_err("foreign extension must be preserved");
        assert!(matches!(error, AdapterError::InvalidAdapterInput { .. }));
        assert_eq!(ops.commands(), vec![vec!["--version".to_string()]]);
    }

    #[test]
    fn enable_refuses_unreadable_activation_before_mutation() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        let enablement = home.join("extensions").join(ENABLEMENT_FILE);
        std::fs::create_dir_all(enablement.parent().expect("extensions dir"))
            .expect("mkdir extensions");
        std::fs::write(&enablement, b"{not-json").expect("malformed enablement");
        let ops = SimOps::new(home.clone(), user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let mut claim = prepare_claim(&driver, &ctx).expect("claim");

        let error = driver
            .apply_enable(&mut claim, &PreparedEnable::None, &ctx, &mut ())
            .expect_err("malformed shared state must fail closed");
        assert!(matches!(error, AdapterError::FrameworkCli { .. }));
        assert_eq!(ops.commands(), vec![vec!["--version".to_string()]]);
        assert!(!home.join("extensions").join("tokenless").exists());
    }

    #[test]
    fn disable_uninstalls_owned_registration_and_policy() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        seed_owned_link(&home, &resource);
        let ops = SimOps::new(home.clone(), user_home.clone());
        ops.write_enablement(true).expect("enablement");
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let claim = prepare_claim(&driver, &ctx).expect("claim");

        let report = driver.disable(&claim, &ctx).expect("disable");
        assert!(report.cleanup_complete, "{:?}", report.messages);
        assert!(!home.join("extensions").join("tokenless").exists());
        assert_eq!(
            ops.commands(),
            vec![
                vec!["--version".to_string()],
                vec![
                    "extensions".to_string(),
                    "uninstall".to_string(),
                    "tokenless".to_string()
                ]
            ]
        );
    }

    #[test]
    fn disable_refuses_to_relink_when_only_policy_remains() {
        let _guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        let ops = SimOps::new(home.clone(), user_home.clone());
        ops.write_enablement(true).expect("enablement");
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let claim = prepare_claim(&driver, &ctx).expect("claim");
        std::fs::write(
            resource.join(QWEN_MANIFEST),
            br#"{"name":"tokenless-v2","version":"2.0.0"}"#,
        )
        .expect("replace manifest");

        let report = driver.disable(&claim, &ctx).expect("disable");
        assert!(!report.cleanup_complete);
        assert!(
            report
                .messages
                .iter()
                .any(|message| message.contains("refusing to relink"))
        );
        assert!(ops.commands().is_empty());
        assert!(!home.join("extensions").join("tokenless").exists());
        assert!(!home.join("extensions").join("tokenless-v2").exists());
    }

    #[test]
    fn disable_refuses_case_insensitive_name_collision() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        seed_owned_link(&home, &resource);
        let foreign = home.join("extensions").join("TokenLess");
        std::fs::create_dir_all(&foreign).expect("foreign extension");
        if paths_equivalent(&foreign, &home.join("extensions").join("tokenless")) {
            assert!(is_casefold_collision("tokenless", "TokenLess"));
            return;
        }
        std::fs::write(
            foreign.join(QWEN_MANIFEST),
            br#"{"name":"TokenLess","version":"1.0.0"}"#,
        )
        .expect("foreign manifest");
        let ops = SimOps::new(home.clone(), user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let claim = prepare_claim(&driver, &ctx).expect("claim");

        let report = driver.disable(&claim, &ctx).expect("disable");
        assert!(!report.cleanup_complete);
        assert!(
            report
                .messages
                .iter()
                .any(|message| message.contains("case-insensitive name collision"))
        );
        assert!(ops.commands().is_empty());
        assert!(home.join("extensions").join("tokenless").exists());
        assert!(foreign.exists());
    }

    #[test]
    fn disable_keeps_receipt_when_cli_is_missing() {
        let guard = EnvGuard::acquire();
        guard.set_bin_absent();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        seed_owned_link(&home, &resource);
        let ops = SimOps::new(home, user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let claim = prepare_claim(&driver, &ctx).expect("claim");

        let report = driver.disable(&claim, &ctx).expect("disable");
        assert!(!report.cleanup_complete);
        assert!(ops.commands().is_empty());
    }

    #[test]
    fn disable_refuses_unreadable_activation_before_mutation() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        seed_owned_link(&home, &resource);
        std::fs::write(home.join("extensions").join(ENABLEMENT_FILE), b"{not-json")
            .expect("malformed enablement");
        let ops = SimOps::new(home.clone(), user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let claim = prepare_claim(&driver, &ctx).expect("claim");

        let report = driver.disable(&claim, &ctx).expect("disable");
        assert!(!report.cleanup_complete);
        assert!(ops.commands().is_empty());
        assert!(home.join("extensions").join("tokenless").exists());
    }

    #[test]
    fn unsupported_version_fails_before_mutation() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let resource = resource_root(tmp.path());
        let mut ops = SimOps::new(user_home.join(".qwen"), user_home.clone());
        ops.version = "0.16.9".to_string();
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let mut claim = prepare_claim(&driver, &ctx).expect("claim");

        let error = driver
            .apply_enable(&mut claim, &PreparedEnable::None, &ctx, &mut ())
            .expect_err("old Qwen must fail");
        assert!(matches!(error, AdapterError::FrameworkCli { .. }));
        assert_eq!(ops.commands(), vec![vec!["--version".to_string()]]);
    }

    #[test]
    fn status_degrades_when_qwen_version_is_unsupported() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let home = user_home.join(".qwen");
        let resource = resource_root(tmp.path());
        seed_owned_link(&home, &resource);
        let mut ops = SimOps::new(home, user_home.clone());
        ops.version = "0.16.9".to_string();
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let claim = prepare_claim(&driver, &ctx).expect("claim");

        let report = driver.status(&claim, &ctx).expect("status");
        assert_eq!(report.summary, AdapterSummary::Degraded);
        assert!(report.conditions.iter().any(|condition| {
            condition.kind == AdapterConditionKind::VerificationSupported
                && condition.status == ConditionStatus::False
        }));
    }

    #[test]
    fn qwen_home_matches_qwen_path_resolution() {
        let user_home = Path::new("/home/alice");
        let cwd = Path::new("/work/project");
        assert_eq!(
            resolve_qwen_home(Some(user_home), cwd, Some(OsStr::new("~/qwen-home"))),
            Some(PathBuf::from("/home/alice/qwen-home"))
        );
        assert_eq!(
            resolve_qwen_home(Some(user_home), cwd, Some(OsStr::new("../qwen-home"))),
            Some(PathBuf::from("/work/qwen-home"))
        );
        assert_eq!(
            resolve_qwen_home(Some(user_home), cwd, Some(OsStr::new(""))),
            Some(PathBuf::from("/home/alice/.qwen"))
        );
    }

    #[test]
    fn qwen_home_matches_user_env_bootstrap_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(user_home.join(".qwen")).expect("qwen home");
        std::fs::write(
            user_home.join(".qwen").join(".env"),
            "QWEN_HOME=\"~/from-qwen-env\"\n",
        )
        .expect("qwen env");
        std::fs::write(
            user_home.join(".env"),
            "export QWEN_HOME=~/from-home-env # lower priority\n",
        )
        .expect("home env");

        let configured = qwen_home_from_user_env(&user_home).expect("configured home");
        assert_eq!(
            resolve_qwen_home(
                Some(&user_home),
                Path::new("/work/project"),
                Some(&configured)
            ),
            Some(user_home.join("from-qwen-env"))
        );

        std::fs::write(user_home.join(".qwen").join(".env"), "OTHER=value\n")
            .expect("qwen env without home");
        let configured = qwen_home_from_user_env(&user_home).expect("fallback home");
        assert_eq!(configured, OsString::from("~/from-home-env"));
    }

    #[test]
    fn qwen_home_preserves_explicit_empty_process_value() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(user_home.join(".qwen")).expect("qwen home");
        std::fs::write(user_home.join(".qwen").join(".env"), "QWEN_HOME=/custom\n")
            .expect("qwen env");
        guard.set_home(OsStr::new(""));

        assert_eq!(qwen_home(Some(&user_home)), Some(user_home.join(".qwen")));
    }

    #[test]
    fn qwen_home_parser_matches_dotenv_assignment_precedence() {
        assert_eq!(
            parse_qwen_home_env("QWEN_HOME: /from-colon\n"),
            Some("/from-colon".to_string())
        );
        assert_eq!(
            parse_qwen_home_env("QWEN_HOME=/first\nQWEN_HOME=/last\n"),
            Some("/last".to_string())
        );
        assert_eq!(parse_qwen_home_env("QWEN_HOME=/first\nQWEN_HOME=\n"), None);
    }

    #[test]
    fn read_bundle_rejects_non_native_manifest_entry() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let resource = resource_root(tmp.path());
        let ops = SimOps::new(user_home.join(".qwen"), user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let mut ctx = ctx(&resource, &user_home, &ops, &layout);
        ctx.declared_bundle_entry = Some("alternate.json".to_string());

        let error = QwenCodeDriver::new()
            .read_bundle(&ctx)
            .expect_err("alternate manifest must be rejected");
        assert!(matches!(error, AdapterError::BundleInvalid { .. }));
        assert!(ops.commands().is_empty());
    }

    #[test]
    fn malformed_payload_reference_is_rejected_before_cli() {
        let guard = EnvGuard::acquire();
        let tmp = tempfile::tempdir().expect("tempdir");
        let user_home = tmp.path().join("home");
        std::fs::create_dir_all(&user_home).expect("home");
        guard.set_bin(&fake_qwen(tmp.path()));
        let resource = resource_root(tmp.path());
        let ops = SimOps::new(user_home.join(".qwen"), user_home.clone());
        let layout = anolisa_platform::fs_layout::FsLayout::user(user_home.clone());
        let ctx = ctx(&resource, &user_home, &ops, &layout);
        let driver = QwenCodeDriver::new();
        let mut claim = prepare_claim(&driver, &ctx).expect("claim");
        if let DriverPayload::QwenCode(payload) = &mut claim.driver_payload {
            payload.extension_dir_resource = RES_PLUGIN.to_string();
        }

        let error = driver
            .apply_enable(&mut claim, &PreparedEnable::None, &ctx, &mut ())
            .expect_err("malformed payload must fail");
        assert!(matches!(error, AdapterError::BundleInvalid { .. }));
        assert!(ops.commands().is_empty());
    }

    #[test]
    fn parses_common_qwen_version_output() {
        assert_eq!(
            parse_qwen_version("qwen-code v0.17.2"),
            Some(Version::new(0, 17, 2))
        );
        assert_eq!(
            parse_qwen_version("0.18.0-beta.1"),
            Some(Version::parse("0.18.0-beta.1").expect("semver"))
        );
    }
}
