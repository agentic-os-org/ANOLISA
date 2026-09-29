//! DeepSeek Harness (`dsh`) native plugin driver.
//!
//! dsh owns profile configuration and plugin registration. ANOLISA therefore
//! treats a bundle as immutable package data (`package.json` plus the
//! `dsh.bundle.patch` file) and delegates every profile mutation to the dsh
//! plugin CLI. A single receipt records all explicitly selected profiles so
//! disable and status never guess an implicit profile.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    DRIVER_SCHEMA_VERSION, DriverPayload, DshClaim, DshProfileClaim, validate_dsh_package_name,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx, DriverPlan,
    FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable, find_binary_in_path,
};
use super::util::{bool_status, cli_failure_reason, display_command, now_iso8601};

const CLI_TIMEOUT: Duration = Duration::from_secs(60);
const PACKAGE_JSON: &str = "package.json";
const HOME_RESOURCE: &str = "dsh_home";
const RES_PREFIX: &str = "dsh_plugin_";

/// dsh native bundle metadata (`package.json` → `dsh.bundle`).
#[derive(Debug, Deserialize)]
struct PackageJson {
    name: String,
    dsh: DshMetadata,
}

#[derive(Debug, Deserialize)]
struct DshMetadata {
    bundle: DshBundleMeta,
}

#[derive(Debug, Deserialize)]
struct DshBundleMeta {
    patch: String,
}

#[derive(Debug, Deserialize)]
struct ProfilePackageJson {
    #[serde(default)]
    dependencies: BTreeMap<String, serde_json::Value>,
    dsh: Option<ProfileDshMetadata>,
}

#[derive(Debug, Deserialize)]
struct ProfileDshMetadata {
    profile: Option<DshProfileMeta>,
}

#[derive(Debug, Deserialize)]
struct DshProfileMeta {
    #[serde(default)]
    bundles: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfilePackageState {
    Registered,
    DependencyOnly,
    BundleOnly,
    Absent,
}

/// Parsed dsh bundle with the package and patch identities required by the
/// driver. The patch entry itself stays package-owned and is never persisted
/// as executable receipt data.
#[derive(Debug, Clone)]
struct DshBundle {
    package_name: String,
    patch: serde_yaml_ng::Value,
}

/// dsh framework driver. Per-operation state is carried by [`DriverCtx`] and
/// the typed [`DshClaim`] receipt.
pub struct DshDriver;

impl DshDriver {
    /// Construct a dsh driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for DshDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for DshDriver {
    fn name(&self) -> &'static str {
        "dsh"
    }

    fn probe_bundle(&self, resource_root: &Path, _declared_entry: Option<&str>) -> bool {
        read_dsh_bundle(resource_root).is_ok()
    }

    fn detect(&self, _env: &HostEnv) -> DetectResult {
        match dsh_program_path() {
            Some(path) => DetectResult {
                detected: true,
                reason: format!("dsh CLI found at {}", path.display()),
            },
            None => DetectResult {
                detected: false,
                reason: "dsh CLI not found on PATH".to_string(),
            },
        }
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        // Profile manifests are read for status and idempotent cleanup, while
        // all profile mutations remain delegated to the dsh CLI.
        dsh_home(ctx.user_home.as_deref()).into_iter().collect()
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        let bundle = read_dsh_bundle(&ctx.resource_root)?;
        if let Some(declared) = ctx.declared_plugin_id.as_deref().filter(|v| !v.is_empty()) {
            super::claim::validate_plugin_id(declared).map_err(AdapterError::ClaimValidation)?;
            if !patch_contains_id(&bundle.patch, declared) {
                return Err(AdapterError::BundleInvalid {
                    root: ctx.resource_root.clone(),
                    reason: format!(
                        "dsh patch has no plugin id '{declared}' matching manifest declaration"
                    ),
                });
            }
        }
        Ok(AdapterBundle {
            resource_root: ctx.resource_root.clone(),
            plugin_id: Some(bundle.package_name),
        })
    }

    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError> {
        let package = bundle_package_name(bundle)?;
        let profiles = requested_profiles(ctx)?;
        let home = required_dsh_home(ctx)?;
        let actions = profiles
            .iter()
            .map(|profile| {
                format!(
                    "register dsh plugin '{package}' in profile '{profile}' from {}",
                    bundle.resource_root.display()
                )
            })
            .collect::<Vec<_>>();
        let register_command = match profiles.as_slice() {
            [profile] => Some(display_command(&dsh_command(
                [
                    "plugin",
                    "--profile",
                    profile,
                    "add",
                    &format!("link:{}", bundle.resource_root.display()),
                ],
                &home,
            ))),
            _ => None,
        };
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
        let package = bundle_package_name(bundle)?;
        let home = required_dsh_home(ctx)?;
        let profiles = requested_profiles(ctx)?;
        let profiles = profiles
            .into_iter()
            .enumerate()
            .map(|(index, name)| DshProfileClaim {
                name,
                plugin_resource: format!("{RES_PREFIX}{index}"),
            })
            .collect::<Vec<_>>();
        let mut resources = profiles
            .iter()
            .map(|profile| ClaimResource {
                id: profile.plugin_resource.clone(),
                purpose: format!("dsh_plugin_profile_{}", profile.name),
                kind: ClaimResourceKind::FrameworkPlugin {
                    framework: self.name().to_string(),
                    plugin_id: package.clone(),
                },
            })
            .collect::<Vec<_>>();
        resources.push(ClaimResource {
            id: HOME_RESOURCE.to_string(),
            purpose: "dsh_home".to_string(),
            kind: ClaimResourceKind::ExternalPath { path: home },
        });
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: ctx.component.clone(),
            framework: self.name().to_string(),
            plugin_id: Some(package.clone()),
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
            driver_payload: DriverPayload::Dsh(DshClaim {
                package_name: package,
                home_resource: HOME_RESOURCE.to_string(),
                profiles,
            }),
        };
        Ok((claim, PreparedEnable::None))
    }

    fn plan_reenable_cleanup(
        &self,
        prior: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        let prior_payload = dsh_claim(prior)?;
        validate_dsh_claim(prior, prior_payload)?;
        let current = read_dsh_bundle(&ctx.resource_root)?;
        let package_changed = current.package_name != prior_payload.package_name;
        let source_changed = prior.resource_root != ctx.resource_root;
        let home_changed = dsh_claim_home(prior, prior_payload)? != required_dsh_home(ctx)?;
        let next_profiles = requested_profiles(ctx)?;
        let mut actions = Vec::new();
        for profile in &prior_payload.profiles {
            let retained = next_profiles.iter().any(|name| name == &profile.name);
            if package_changed || source_changed || home_changed || !retained {
                actions.push(format!(
                    "remove prior dsh plugin '{}' from profile '{}'",
                    prior_payload.package_name, profile.name
                ));
            }
        }
        Ok(actions)
    }

    fn cleanup_replaced_claim(
        &self,
        prior: &AdapterClaim,
        next: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        let prior_payload = dsh_claim(prior)?;
        let next_payload = dsh_claim(next)?;
        validate_dsh_claim(prior, prior_payload)?;
        validate_dsh_claim(next, next_payload)?;
        let prior_home = dsh_claim_home(prior, prior_payload)?;
        let home_changed = prior_home != dsh_claim_home(next, next_payload)?;
        let source_changed = prior.resource_root != next.resource_root;
        let package_changed = prior_payload.package_name != next_payload.package_name;
        let mut cleanup_complete = true;
        let mut messages = Vec::new();
        for profile in &prior_payload.profiles {
            let retained = next_payload
                .profiles
                .iter()
                .any(|candidate| candidate.name == profile.name);
            if retained && !source_changed && !package_changed && !home_changed {
                continue;
            }
            if profile_package_state(ctx, prior_home, &profile.name, &prior_payload.package_name)?
                == Some(ProfilePackageState::Absent)
            {
                messages.push(format!(
                    "prior dsh plugin '{}' is already absent from profile '{}'",
                    prior_payload.package_name, profile.name
                ));
                continue;
            }
            let output = ctx.ops.run_framework_cli(dsh_command(
                [
                    "plugin",
                    "--profile",
                    profile.name.as_str(),
                    "remove",
                    prior_payload.package_name.as_str(),
                ],
                prior_home,
            ))?;
            if output.success() {
                messages.push(format!(
                    "removed prior dsh plugin '{}' from profile '{}'",
                    prior_payload.package_name, profile.name
                ));
            } else {
                cleanup_complete = false;
                messages.push(format!(
                    "failed to remove prior dsh plugin '{}' from profile '{}': {}",
                    prior_payload.package_name,
                    profile.name,
                    cli_failure_reason("plugin remove", &output)
                ));
            }
        }
        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        prepared: &PreparedEnable,
        ctx: &DriverCtx,
        _progress: &mut dyn super::driver::EnableProgress,
    ) -> Result<(), AdapterError> {
        if !matches!(prepared, PreparedEnable::None) {
            return Err(AdapterError::FrameworkCli {
                program: dsh_program(),
                reason: "dsh enable received unexpected prepared state".to_string(),
            });
        }
        let payload = dsh_claim(claim)?;
        validate_dsh_claim(claim, payload)?;
        let home = dsh_claim_home(claim, payload)?;
        for profile in &payload.profiles {
            let root = format!("link:{}", claim.resource_root.display());
            let output = ctx.ops.run_framework_cli(dsh_command(
                [
                    "plugin",
                    "--profile",
                    profile.name.as_str(),
                    "add",
                    root.as_str(),
                ],
                home,
            ))?;
            if !output.success() {
                return Err(AdapterError::FrameworkCli {
                    program: dsh_program(),
                    reason: cli_failure_reason("plugin add", &output),
                });
            }
        }
        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        let payload = dsh_claim(claim)?;
        validate_dsh_claim(claim, payload)?;
        let home = dsh_claim_home(claim, payload)?;
        let detect = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        let mut conditions = vec![AdapterCondition {
            kind: AdapterConditionKind::FrameworkDetected,
            status: bool_status(detect.detected),
            reason: Some(detect.reason),
            resource: None,
        }];
        let mut all_registered = true;
        let mut any_unknown = false;
        let mut verification = ConditionStatus::True;
        for profile in &payload.profiles {
            let resource = Some(ClaimResourceRef {
                id: profile.plugin_resource.clone(),
            });
            let (status, reason) =
                match profile_package_state(ctx, home, &profile.name, &payload.package_name)? {
                    Some(ProfilePackageState::Registered) => (ConditionStatus::True, None),
                    Some(_) => {
                        all_registered = false;
                        (
                            ConditionStatus::False,
                            Some(format!(
                                "package '{}' is not registered in profile '{}'",
                                payload.package_name, profile.name
                            )),
                        )
                    }
                    None => {
                        verification = ConditionStatus::Unknown;
                        any_unknown = true;
                        (
                            ConditionStatus::Unknown,
                            Some(format!(
                                "dsh could not read plugin registration in profile '{}'",
                                profile.name
                            )),
                        )
                    }
                };
            all_registered &= status == ConditionStatus::True;
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::PluginRegistered,
                status,
                reason,
                resource,
            });
        }
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::VerificationSupported,
            status: verification,
            reason: (verification != ConditionStatus::True)
                .then(|| "dsh profile registration could not be verified".to_string()),
            resource: None,
        });
        let summary = if claim.status == ClaimStatus::CleanupFailed {
            AdapterSummary::CleanupFailed
        } else if !detect.detected || !all_registered {
            if verification == ConditionStatus::Unknown
                && any_unknown
                && !conditions.iter().any(|condition| {
                    condition.kind == AdapterConditionKind::PluginRegistered
                        && condition.status == ConditionStatus::False
                })
            {
                AdapterSummary::Unknown
            } else {
                AdapterSummary::Degraded
            }
        } else {
            AdapterSummary::Healthy
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
        let payload = dsh_claim(claim)?;
        validate_dsh_claim(claim, payload)?;
        let home = dsh_claim_home(claim, payload)?;
        if dsh_program_path().is_none() {
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![
                    "dsh CLI not found on PATH; receipt kept for cleanup retry".to_string(),
                ],
            });
        }
        let mut messages = Vec::new();
        let mut cleanup_complete = true;
        for profile in &payload.profiles {
            if profile_package_state(ctx, home, &profile.name, &payload.package_name)?
                == Some(ProfilePackageState::Absent)
            {
                messages.push(format!(
                    "dsh plugin '{}' is already absent from profile '{}'",
                    payload.package_name, profile.name
                ));
                continue;
            }
            let output = ctx.ops.run_framework_cli(dsh_command(
                [
                    "plugin",
                    "--profile",
                    profile.name.as_str(),
                    "remove",
                    payload.package_name.as_str(),
                ],
                home,
            ))?;
            if output.success() {
                messages.push(format!(
                    "removed dsh plugin '{}' from profile '{}'",
                    payload.package_name, profile.name
                ));
            } else {
                cleanup_complete = false;
                messages.push(format!(
                    "failed to remove dsh plugin '{}' from profile '{}': {}",
                    payload.package_name,
                    profile.name,
                    cli_failure_reason("plugin remove", &output)
                ));
            }
        }
        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }
}

fn read_dsh_bundle(root: &Path) -> Result<DshBundle, AdapterError> {
    if !root.is_dir() {
        return Err(bundle_error(
            root,
            "resource root does not exist or is not a directory",
        ));
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|source| {
        bundle_error(
            root,
            format!("cannot resolve bundle root '{}': {source}", root.display()),
        )
    })?;
    let package_path = resolve_bundle_file(
        root,
        &canonical_root,
        Path::new(PACKAGE_JSON),
        "package manifest",
    )?;
    let bytes = std::fs::read(&package_path).map_err(|source| {
        bundle_error(
            root,
            format!("cannot read '{}': {source}", package_path.display()),
        )
    })?;
    let package: PackageJson = serde_json::from_slice(&bytes).map_err(|source| {
        bundle_error(
            root,
            format!("invalid '{}': {source}", package_path.display()),
        )
    })?;
    validate_dsh_package_name(&package.name).map_err(AdapterError::ClaimValidation)?;
    let patch_rel = validate_relative_bundle_path(root, &package.dsh.bundle.patch, "patch")?;
    let patch = resolve_bundle_file(root, &canonical_root, &patch_rel, "patch")?;
    let patch_text = std::fs::read_to_string(&patch).map_err(|source| {
        bundle_error(
            root,
            format!(
                "cannot read dsh bundle patch '{}': {source}",
                patch.display()
            ),
        )
    })?;
    let patch = parse_patch_list(root, &patch_text)?;
    validate_patch_entries(root, &patch)?;
    Ok(DshBundle {
        package_name: package.name,
        patch,
    })
}

fn parse_patch_list(root: &Path, patch: &str) -> Result<serde_yaml_ng::Value, AdapterError> {
    let parsed = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(patch)
        .map_err(|source| bundle_error(root, format!("invalid dsh bundle patch YAML: {source}")))?;
    let Some(entries) = parsed.as_sequence() else {
        return Err(bundle_error(
            root,
            "dsh.bundle.patch must be a top-level YAML array",
        ));
    };
    if entries.iter().any(|entry| entry.as_mapping().is_none()) {
        return Err(bundle_error(
            root,
            "every dsh bundle patch entry must be a YAML mapping",
        ));
    }
    Ok(parsed)
}

fn patch_contains_id(patch: &serde_yaml_ng::Value, expected: &str) -> bool {
    patch.as_sequence().is_some_and(|entries| {
        entries.iter().any(|entry| {
            let Some(mapping) = entry.as_mapping() else {
                return false;
            };
            yaml_string(mapping, "id") == Some(expected)
                || yaml_sequence(mapping, "insert").is_some_and(|inserted| {
                    inserted.iter().any(|row| {
                        row.as_mapping()
                            .and_then(|mapping| yaml_string(mapping, "id"))
                            == Some(expected)
                    })
                })
        })
    })
}

fn validate_patch_entries(root: &Path, patch: &serde_yaml_ng::Value) -> Result<(), AdapterError> {
    for entry in patch.as_sequence().into_iter().flatten() {
        let Some(mapping) = entry.as_mapping() else {
            continue;
        };
        let Some(insert) = yaml_value(mapping, "insert") else {
            continue;
        };
        let Some(rows) = insert.as_sequence() else {
            return Err(bundle_error(
                root,
                "dsh patch 'insert' must be a YAML array",
            ));
        };
        for row in rows {
            let Some(row) = row.as_mapping() else {
                return Err(bundle_error(
                    root,
                    "every dsh patch 'insert' row must be a YAML mapping",
                ));
            };
            let Some(name) = yaml_value(row, "name") else {
                continue;
            };
            let Some(name) = name.as_str() else {
                return Err(bundle_error(
                    root,
                    "dsh patch plugin 'name' must be a string",
                ));
            };
            if Path::new(name).is_absolute()
                || name.starts_with("./")
                || name.starts_with("../")
                || name.starts_with(".\\")
                || name.starts_with("..\\")
            {
                return Err(bundle_error(
                    root,
                    format!("dsh patch plugin name '{name}' must use its installed package name"),
                ));
            }
        }
    }
    Ok(())
}

fn yaml_value<'a>(
    mapping: &'a serde_yaml_ng::Mapping,
    key: &str,
) -> Option<&'a serde_yaml_ng::Value> {
    mapping.get(serde_yaml_ng::Value::String(key.to_string()))
}

fn yaml_string<'a>(mapping: &'a serde_yaml_ng::Mapping, key: &str) -> Option<&'a str> {
    yaml_value(mapping, key).and_then(serde_yaml_ng::Value::as_str)
}

fn yaml_sequence<'a>(
    mapping: &'a serde_yaml_ng::Mapping,
    key: &str,
) -> Option<&'a Vec<serde_yaml_ng::Value>> {
    yaml_value(mapping, key).and_then(serde_yaml_ng::Value::as_sequence)
}

fn resolve_bundle_file(
    root: &Path,
    canonical_root: &Path,
    relative: &Path,
    role: &str,
) -> Result<PathBuf, AdapterError> {
    let path = root.join(relative);
    let resolved = std::fs::canonicalize(&path).map_err(|source| {
        bundle_error(
            root,
            format!("cannot resolve dsh {role} '{}': {source}", path.display()),
        )
    })?;
    if !resolved.starts_with(canonical_root) {
        return Err(bundle_error(
            root,
            format!(
                "dsh {role} '{}' resolves outside the bundle root",
                path.display()
            ),
        ));
    }
    let metadata = std::fs::metadata(&resolved).map_err(|source| {
        bundle_error(
            root,
            format!("cannot inspect dsh {role} '{}': {source}", path.display()),
        )
    })?;
    if !metadata.is_file() {
        return Err(bundle_error(
            root,
            format!("dsh {role} '{}' is not a regular file", path.display()),
        ));
    }
    Ok(resolved)
}

fn validate_relative_bundle_path(
    root: &Path,
    value: &str,
    role: &str,
) -> Result<PathBuf, AdapterError> {
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(bundle_error(
            root,
            format!("dsh {role} must be a non-empty relative path"),
        ));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(bundle_error(
            root,
            format!("dsh {role} '{}' escapes the bundle root", path.display()),
        ));
    }
    Ok(path)
}

fn bundle_error(root: &Path, reason: impl Into<String>) -> AdapterError {
    AdapterError::BundleInvalid {
        root: root.to_path_buf(),
        reason: reason.into(),
    }
}

fn bundle_package_name(bundle: &AdapterBundle) -> Result<String, AdapterError> {
    let package = bundle
        .plugin_id
        .clone()
        .ok_or_else(|| bundle_error(&bundle.resource_root, "dsh bundle has no package name"))?;
    validate_dsh_package_name(&package).map_err(AdapterError::ClaimValidation)?;
    Ok(package)
}

fn requested_profiles(ctx: &DriverCtx) -> Result<Vec<String>, AdapterError> {
    if ctx.requested_profiles.is_empty() {
        return Err(AdapterError::InvalidAdapterInput {
            component: ctx.component.clone(),
            framework: "dsh".to_string(),
            reason: "dsh adapter enable requires at least one explicit --profile".to_string(),
        });
    }
    let mut profiles = ctx.requested_profiles.clone();
    profiles.sort();
    profiles.dedup();
    for profile in &profiles {
        validate_profile_name(profile).map_err(|reason| AdapterError::InvalidAdapterInput {
            component: ctx.component.clone(),
            framework: "dsh".to_string(),
            reason,
        })?;
    }
    Ok(profiles)
}

fn validate_profile_name(profile: &str) -> Result<(), String> {
    if profile.is_empty()
        || matches!(profile, "." | ".." | "node_modules")
        || profile.starts_with('-')
    {
        return Err(format!("invalid dsh profile '{profile}'"));
    }
    if !profile
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "invalid dsh profile '{profile}': use only letters, digits, '.', '_' and '-'"
        ));
    }
    Ok(())
}

fn dsh_claim(claim: &AdapterClaim) -> Result<&DshClaim, AdapterError> {
    match &claim.driver_payload {
        DriverPayload::Dsh(payload) => Ok(payload),
        _ => Err(bundle_error(
            &claim.resource_root,
            "receipt payload is not a dsh claim",
        )),
    }
}

fn validate_dsh_claim<'a>(
    claim: &AdapterClaim,
    payload: &'a DshClaim,
) -> Result<&'a DshClaim, AdapterError> {
    validate_dsh_package_name(&payload.package_name).map_err(AdapterError::ClaimValidation)?;
    if claim.plugin_id.as_deref() != Some(payload.package_name.as_str()) {
        return Err(AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: "dsh receipt package name disagrees with plugin_id".to_string(),
        });
    }
    if payload.profiles.is_empty() {
        return Err(AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: "dsh receipt contains no profiles".to_string(),
        });
    }
    let _ = dsh_claim_home(claim, payload)?;
    let mut names = BTreeMap::new();
    let mut resources = BTreeMap::new();
    for profile in &payload.profiles {
        validate_profile_name(&profile.name).map_err(|reason| AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason,
        })?;
        if names.insert(profile.name.clone(), ()).is_some() {
            return Err(AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: format!("dsh receipt contains duplicate profile '{}'", profile.name),
            });
        }
        if resources
            .insert(profile.plugin_resource.clone(), ())
            .is_some()
        {
            return Err(AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: format!(
                    "dsh receipt reuses plugin resource '{}'",
                    profile.plugin_resource
                ),
            });
        }
        let Some(resource) = claim.resource(&profile.plugin_resource) else {
            return Err(AdapterError::BundleInvalid {
                root: claim.resource_root.clone(),
                reason: format!(
                    "dsh receipt references unknown resource '{}'",
                    profile.plugin_resource
                ),
            });
        };
        match &resource.kind {
            ClaimResourceKind::FrameworkPlugin {
                framework,
                plugin_id,
            } if framework == "dsh" && plugin_id == &payload.package_name => {}
            _ => {
                return Err(AdapterError::BundleInvalid {
                    root: claim.resource_root.clone(),
                    reason: format!(
                        "dsh resource '{}' is not its package plugin",
                        profile.plugin_resource
                    ),
                });
            }
        }
    }
    Ok(payload)
}

fn dsh_claim_home<'a>(
    claim: &'a AdapterClaim,
    payload: &DshClaim,
) -> Result<&'a Path, AdapterError> {
    let Some(resource) = claim.resource(&payload.home_resource) else {
        return Err(AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: format!(
                "dsh receipt references unknown home resource '{}'",
                payload.home_resource
            ),
        });
    };
    match &resource.kind {
        ClaimResourceKind::ExternalPath { path }
            if path.is_absolute()
                && path.to_str().is_some()
                && !path.components().any(|component| {
                    matches!(component, Component::CurDir | Component::ParentDir)
                }) =>
        {
            Ok(path)
        }
        _ => Err(AdapterError::BundleInvalid {
            root: claim.resource_root.clone(),
            reason: format!(
                "dsh home resource '{}' is not a normalized absolute external path",
                payload.home_resource
            ),
        }),
    }
}

fn dsh_program() -> String {
    std::env::var("DSH_BIN").unwrap_or_else(|_| "dsh".to_string())
}

fn dsh_program_path() -> Option<PathBuf> {
    let program = dsh_program();
    let candidate = PathBuf::from(&program);
    if candidate.components().count() > 1 {
        return candidate.is_file().then_some(candidate);
    }
    find_binary_in_path(&program)
}

fn dsh_home(user_home: Option<&Path>) -> Option<PathBuf> {
    let configured = std::env::var_os("DSH_HOME");
    let cwd = std::env::current_dir().ok();
    resolve_dsh_home(configured.as_deref(), user_home, cwd.as_deref())
}

fn resolve_dsh_home(
    configured: Option<&OsStr>,
    user_home: Option<&Path>,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    let configured = configured.filter(|value| !value.to_string_lossy().trim().is_empty());
    let path = match configured {
        Some(value) => {
            let display = value.to_string_lossy();
            if display == "~" {
                user_home?.to_path_buf()
            } else if let Some(rest) = display
                .strip_prefix("~/")
                .or_else(|| display.strip_prefix("~\\"))
            {
                user_home?.join(rest)
            } else {
                PathBuf::from(value)
            }
        }
        None => user_home?.join(".dsh"),
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        cwd?.join(path)
    };
    Some(normalize_lexically(&absolute))
}

fn required_dsh_home(ctx: &DriverCtx) -> Result<PathBuf, AdapterError> {
    let home =
        dsh_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::InvalidAdapterInput {
            component: ctx.component.clone(),
            framework: "dsh".to_string(),
            reason: "cannot resolve an absolute dsh home from DSH_HOME or the user home"
                .to_string(),
        })?;
    if home.to_str().is_none() {
        return Err(AdapterError::InvalidAdapterInput {
            component: ctx.component.clone(),
            framework: "dsh".to_string(),
            reason: "resolved dsh home is not valid UTF-8 and cannot be passed as DSH_HOME"
                .to_string(),
        });
    }
    Ok(home)
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

fn dsh_command<const N: usize>(args: [&str; N], home: &Path) -> FrameworkCommand {
    // Both receipt validation and enable-time resolution reject non-UTF-8
    // homes before command construction, so this branch is unreachable.
    let Some(home) = home.to_str() else {
        unreachable!("validated dsh home must be UTF-8")
    };
    FrameworkCommand {
        program: dsh_program(),
        args: args.into_iter().map(str::to_string).collect(),
        stdin: None,
        env_set: vec![("DSH_HOME".to_string(), home.to_string())],
        env_remove: Vec::new(),
        path_prepend: Vec::new(),
        timeout: CLI_TIMEOUT,
    }
}

fn profile_package_state(
    ctx: &DriverCtx,
    home: &Path,
    profile: &str,
    package: &str,
) -> Result<Option<ProfilePackageState>, AdapterError> {
    let manifest_path = home.join("profiles").join(profile).join(PACKAGE_JSON);
    let Some(bytes) = ctx.ops.read_file(&manifest_path)? else {
        return Ok(Some(ProfilePackageState::Absent));
    };
    Ok(profile_package_state_from_manifest(&bytes, package))
}

fn profile_package_state_from_manifest(bytes: &[u8], package: &str) -> Option<ProfilePackageState> {
    let manifest = serde_json::from_slice::<ProfilePackageJson>(bytes).ok()?;
    let dependency = manifest.dependencies.contains_key(package);
    let bundle = manifest
        .dsh
        .and_then(|dsh| dsh.profile)
        .is_some_and(|profile| profile.bundles.iter().any(|candidate| candidate == package));
    Some(match (dependency, bundle) {
        (true, true) => ProfilePackageState::Registered,
        (true, false) => ProfilePackageState::DependencyOnly,
        (false, true) => ProfilePackageState::BundleOnly,
        (false, false) => ProfilePackageState::Absent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::driver::{AdapterOps, CliOutput};
    use anolisa_platform::fs_layout::FsLayout;
    use std::sync::{Arc, Mutex};

    struct RecordingOps {
        commands: Arc<Mutex<Vec<FrameworkCommand>>>,
        reads: Arc<Mutex<Vec<PathBuf>>>,
        output: CliOutput,
    }

    impl AdapterOps for RecordingOps {
        fn run_framework_cli(&self, command: FrameworkCommand) -> Result<CliOutput, AdapterError> {
            self.commands.lock().unwrap().push(command);
            Ok(self.output.clone())
        }
        fn copy_tree(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
            unreachable!("dsh never copies files")
        }
        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
            unreachable!("dsh never copies files")
        }
        fn remove_tree(&self, _: &Path) -> Result<bool, AdapterError> {
            unreachable!("dsh never removes files")
        }
        fn write_file(&self, _: &Path, _: &[u8]) -> Result<(), AdapterError> {
            unreachable!("dsh never writes files")
        }
        fn create_symlink(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
            unreachable!("dsh never creates symlinks")
        }
        fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
            self.reads.lock().unwrap().push(path.to_path_buf());
            Ok(Some(
                br#"{"dependencies":{"@anolisa/dsh-tokenless":"link:/bundle"},"dsh":{"profile":{"bundles":["@anolisa/dsh-tokenless"]}}}"#
                    .to_vec(),
            ))
        }
    }

    fn ctx<'a>(root: &'a Path, ops: &'a RecordingOps, profiles: Vec<String>) -> DriverCtx<'a> {
        ctx_with_user_home(root, root, ops, profiles)
    }

    fn ctx_with_user_home<'a>(
        root: &'a Path,
        user_home: &Path,
        ops: &'a RecordingOps,
        profiles: Vec<String>,
    ) -> DriverCtx<'a> {
        let layout = Box::leak(Box::new(FsLayout::user(root.to_path_buf())));
        DriverCtx {
            component: "tokenless".to_string(),
            framework: "dsh".to_string(),
            layout,
            resource_root: root.to_path_buf(),
            user_home: Some(user_home.to_path_buf()),
            declared_plugin_id: Some("anolisa-tokenless".to_string()),
            requested_profiles: profiles,
            adapter_type: Some("plugin".to_string()),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: false,
            ops,
        }
    }

    fn write_bundle(root: &Path, entry: &str) {
        std::fs::create_dir_all(root.join("dist")).unwrap();
        std::fs::write(
            root.join(PACKAGE_JSON),
            r#"{"name":"@anolisa/dsh-tokenless","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("cordis.patch.yml"),
            format!("- insert:\n    - id: anolisa-tokenless\n      name: {entry}\n"),
        )
        .unwrap();
        std::fs::write(root.join("dist/index.js"), "export {}\n").unwrap();
    }

    #[test]
    fn bundle_requires_package_patch_and_package_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let bundle = read_dsh_bundle(dir.path()).unwrap();
        assert_eq!(bundle.package_name, "@anolisa/dsh-tokenless");
        assert!(read_dsh_bundle(dir.path()).is_ok());
        std::fs::write(
            dir.path().join("cordis.patch.yml"),
            "- insert:\n    - id: anolisa-tokenless\n      name: ../escape.js\n",
        )
        .unwrap();
        assert!(read_dsh_bundle(dir.path()).is_err());
    }

    #[test]
    fn bundle_rejects_malformed_patch_before_registration() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        std::fs::write(
            dir.path().join("cordis.patch.yml"),
            "- insert:\n    - id: anolisa-tokenless\n      name: '@anolisa/dsh-tokenless'\n  broken\n",
        )
        .unwrap();

        assert!(read_dsh_bundle(dir.path()).is_err());
    }

    #[test]
    fn bundle_patch_accepts_dsh_js_values() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        std::fs::write(
            dir.path().join("cordis.patch.yml"),
            "- insert:\n    - id: anolisa-tokenless\n      name: '@anolisa/dsh-tokenless'\n      config:\n        mode: !!js process.env.DSH_TOOLS_MODE\n",
        )
        .unwrap();

        assert!(read_dsh_bundle(dir.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn bundle_rejects_patch_symlink_that_resolves_outside_root() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let outside_patch = outside.path().join("cordis.patch.yml");
        std::fs::write(&outside_patch, "[]\n").unwrap();
        std::fs::remove_file(dir.path().join("cordis.patch.yml")).unwrap();
        symlink(&outside_patch, dir.path().join("cordis.patch.yml")).unwrap();

        let err = read_dsh_bundle(dir.path()).expect_err("external patch must be rejected");
        assert!(err.to_string().contains("outside the bundle root"));
    }

    #[test]
    fn enable_records_all_profiles_and_uses_native_add() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(0),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        };
        let driver = DshDriver::new();
        let context = ctx(
            dir.path(),
            &ops,
            vec!["headless".to_string(), "web".to_string()],
        );
        let bundle = driver.read_bundle(&context).unwrap();
        let plan = driver.plan_enable(&bundle, &context).unwrap();
        assert_eq!(plan.actions.len(), 2);
        assert!(
            plan.register_command.is_none(),
            "a singular command must not hide additional profile mutations"
        );
        let (mut claim, prepared) = driver.prepare_enable(&bundle, &context).unwrap();
        driver
            .apply_enable(&mut claim, &prepared, &context, &mut ())
            .unwrap();
        let DriverPayload::Dsh(payload) = claim.driver_payload else {
            panic!("expected dsh payload")
        };
        assert_eq!(payload.package_name, "@anolisa/dsh-tokenless");
        assert_eq!(
            payload
                .profiles
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["headless", "web"]
        );
        let commands = ops.commands.lock().unwrap();
        assert_eq!(
            commands[0].args,
            vec![
                "plugin",
                "--profile",
                "headless",
                "add",
                &format!("link:{}", dir.path().display())
            ]
        );
        assert_eq!(
            commands[0].env_set,
            [(
                "DSH_HOME".to_string(),
                dir.path().join(".dsh").display().to_string()
            )]
        );
    }

    #[test]
    fn single_profile_plan_exposes_its_native_command() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(0),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        };
        let driver = DshDriver::new();
        let context = ctx(dir.path(), &ops, vec!["web".to_string()]);
        let bundle = driver.read_bundle(&context).unwrap();

        let plan = driver.plan_enable(&bundle, &context).unwrap();

        assert!(plan.register_command.is_some());
    }

    #[test]
    fn no_profile_is_rejected_instead_of_defaulting() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(0),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        };
        let context = ctx(dir.path(), &ops, Vec::new());
        let err = DshDriver::new()
            .prepare_enable(&DshDriver::new().read_bundle(&context).unwrap(), &context)
            .expect_err("profile selection is mandatory");
        assert!(err.to_string().contains("--profile"));
    }

    #[test]
    fn profile_validation_matches_dsh_reserved_name() {
        assert!(validate_profile_name("node_modules").is_err());
        assert!(validate_profile_name("--flag").is_err());
        assert!(validate_profile_name("custom-profile").is_ok());
    }

    #[test]
    fn relative_dsh_home_resolution_is_anchored_to_enable_cwd() {
        let first_cwd = Path::new("/work/first");
        let second_cwd = Path::new("/work/second");
        let configured = OsStr::new("state/dsh");

        assert_eq!(
            resolve_dsh_home(
                Some(configured),
                Some(Path::new("/home/user")),
                Some(first_cwd),
            ),
            Some(first_cwd.join("state/dsh"))
        );
        assert_eq!(
            resolve_dsh_home(
                Some(configured),
                Some(Path::new("/home/user")),
                Some(second_cwd),
            ),
            Some(second_cwd.join("state/dsh"))
        );
        assert_eq!(
            resolve_dsh_home(Some(OsStr::new("/var/lib/dsh")), None, None),
            Some(PathBuf::from("/var/lib/dsh")),
            "an absolute DSH_HOME must not depend on a readable cwd"
        );
    }

    #[test]
    fn receipt_rejects_unvalidated_dsh_home_resource() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(0),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        };
        let driver = DshDriver::new();
        let context = ctx(dir.path(), &ops, vec!["web".to_string()]);
        let bundle = driver.read_bundle(&context).unwrap();
        let (mut claim, _) = driver.prepare_enable(&bundle, &context).unwrap();
        let allowed_home = dir.path().join(".dsh");
        claim
            .validate(context.layout, std::slice::from_ref(&allowed_home))
            .expect("enable-time dsh home must validate against its resolved boundary");
        let payload = dsh_claim(&claim).unwrap().clone();
        let resource = claim
            .resources
            .iter_mut()
            .find(|resource| resource.id == payload.home_resource)
            .unwrap();
        resource.kind = ClaimResourceKind::ExternalPath {
            path: PathBuf::from("relative/dsh"),
        };

        assert!(
            claim
                .validate(context.layout, std::slice::from_ref(&allowed_home))
                .is_err(),
            "Manager validation must reject a receipt-derived relative home"
        );
        let err = validate_dsh_claim(&claim, &payload).expect_err("relative home must fail");
        assert!(
            err.to_string()
                .contains("normalized absolute external path")
        );
    }

    #[test]
    fn reenable_plan_fails_closed_when_bundle_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(0),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        };
        let driver = DshDriver::new();
        let context = ctx(dir.path(), &ops, vec!["web".to_string()]);
        let bundle = driver.read_bundle(&context).unwrap();
        let (prior, _) = driver.prepare_enable(&bundle, &context).unwrap();
        std::fs::write(dir.path().join(PACKAGE_JSON), "not json\n").unwrap();

        assert!(driver.plan_reenable_cleanup(&prior, &context).is_err());
        assert!(ops.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn reenable_removes_only_profiles_no_longer_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let first_home = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(0),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        };
        let driver = DshDriver::new();
        let prior_ctx = ctx_with_user_home(
            dir.path(),
            first_home.path(),
            &ops,
            vec!["retained".to_string(), "stale".to_string()],
        );
        let bundle = driver.read_bundle(&prior_ctx).unwrap();
        let (prior, _) = driver.prepare_enable(&bundle, &prior_ctx).unwrap();
        let next_ctx = ctx_with_user_home(
            dir.path(),
            first_home.path(),
            &ops,
            vec!["retained".to_string()],
        );
        let (next, _) = driver.prepare_enable(&bundle, &next_ctx).unwrap();

        let report = driver
            .cleanup_replaced_claim(&prior, &next, &next_ctx)
            .unwrap();

        assert!(report.cleanup_complete);
        assert_eq!(
            ops.commands.lock().unwrap()[0].args,
            [
                "plugin",
                "--profile",
                "stale",
                "remove",
                "@anolisa/dsh-tokenless",
            ]
        );
        assert_eq!(
            ops.commands.lock().unwrap()[0].env_set,
            [(
                "DSH_HOME".to_string(),
                first_home.path().join(".dsh").display().to_string(),
            )]
        );
        assert_eq!(
            ops.reads.lock().unwrap()[0],
            first_home.path().join(".dsh/profiles/stale/package.json")
        );
    }

    #[test]
    fn reenable_migrates_retained_profiles_when_home_changes() {
        let dir = tempfile::tempdir().unwrap();
        let first_home = tempfile::tempdir().unwrap();
        let later_home = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(0),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        };
        let driver = DshDriver::new();
        let prior_ctx = ctx_with_user_home(
            dir.path(),
            first_home.path(),
            &ops,
            vec!["retained".to_string()],
        );
        let bundle = driver.read_bundle(&prior_ctx).unwrap();
        let (prior, _) = driver.prepare_enable(&bundle, &prior_ctx).unwrap();
        let next_ctx = ctx_with_user_home(
            dir.path(),
            later_home.path(),
            &ops,
            vec!["retained".to_string()],
        );

        assert_eq!(
            driver.plan_reenable_cleanup(&prior, &next_ctx).unwrap(),
            ["remove prior dsh plugin '@anolisa/dsh-tokenless' from profile 'retained'"]
        );

        let (next, _) = driver.prepare_enable(&bundle, &next_ctx).unwrap();
        let report = driver
            .cleanup_replaced_claim(&prior, &next, &next_ctx)
            .unwrap();

        assert!(report.cleanup_complete);
        let commands = ops.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].args,
            [
                "plugin",
                "--profile",
                "retained",
                "remove",
                "@anolisa/dsh-tokenless",
            ]
        );
        assert_eq!(
            commands[0].env_set,
            [(
                "DSH_HOME".to_string(),
                first_home.path().join(".dsh").display().to_string(),
            )]
        );
        assert_eq!(
            ops.reads.lock().unwrap()[0],
            first_home
                .path()
                .join(".dsh/profiles/retained/package.json")
        );
    }

    #[test]
    fn cleanup_retains_receipt_on_pnpm_infrastructure_failure() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(127),
                timed_out: false,
                stdout: String::new(),
                stderr: "dsh: pnpm not found on PATH".to_string(),
            },
        };
        let driver = DshDriver::new();
        let prior_ctx = ctx(dir.path(), &ops, vec!["stale".to_string()]);
        let bundle = driver.read_bundle(&prior_ctx).unwrap();
        let (prior, _) = driver.prepare_enable(&bundle, &prior_ctx).unwrap();
        let next_ctx = ctx(dir.path(), &ops, vec!["retained".to_string()]);
        let (next, _) = driver.prepare_enable(&bundle, &next_ctx).unwrap();

        let report = driver
            .cleanup_replaced_claim(&prior, &next, &next_ctx)
            .unwrap();

        assert!(!report.cleanup_complete);
        assert!(report.messages[0].contains("pnpm not found"));
    }

    #[test]
    fn status_reads_profile_manifest_without_invoking_dsh_plugin() {
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), "'@anolisa/dsh-tokenless'");
        let ops = RecordingOps {
            commands: Arc::new(Mutex::new(Vec::new())),
            reads: Arc::new(Mutex::new(Vec::new())),
            output: CliOutput {
                status: Some(0),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        };
        let driver = DshDriver::new();
        let context = ctx(dir.path(), &ops, vec!["web".to_string()]);
        let bundle = driver.read_bundle(&context).unwrap();
        let (claim, _) = driver.prepare_enable(&bundle, &context).unwrap();

        driver.status(&claim, &context).unwrap();

        assert!(ops.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn profile_manifest_package_match_is_exact() {
        let package = "@anolisa/dsh-tokenless";
        let missing = br#"{"dependencies":{"@anolisa/dsh-tokenless-extra":"1"},"dsh":{"profile":{"bundles":["@anolisa/dsh-tokenless-extra"]}}}"#;
        assert_eq!(
            profile_package_state_from_manifest(missing, package),
            Some(ProfilePackageState::Absent)
        );
        let present = br#"{"dependencies":{"@anolisa/dsh-tokenless":"1"},"dsh":{"profile":{"bundles":["@anolisa/dsh-tokenless"]}}}"#;
        assert_eq!(
            profile_package_state_from_manifest(present, package),
            Some(ProfilePackageState::Registered)
        );
    }
}
