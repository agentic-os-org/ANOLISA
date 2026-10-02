// SPDX-License-Identifier: Apache-2.0
//! Workload class enum, policy file schema and policy engine.
//!
//! See the workload class table and policy TOML schema.
//! Evaluation logic returns a [`RuntimeDecision`] that the
//! daemon hands to the [`crate::backend`] selector and the
//! [`crate::lifecycle`] manager.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::backend::BackendKind;
use crate::error::{BlazeError, Result};

// ---------------------------------------------------------------------------
// WorkloadClass
// ---------------------------------------------------------------------------

/// Requests without a declared workload class are rejected; no silent fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkloadClass {
    AgentRl,
    AgentTool,
    Serverless,
    DevShell,
    Untrusted,
    Function,
}

impl WorkloadClass {
    pub const fn as_str(&self) -> &'static str {
        match self {
            WorkloadClass::AgentRl => "agent-rl",
            WorkloadClass::AgentTool => "agent-tool",
            WorkloadClass::Serverless => "serverless",
            WorkloadClass::DevShell => "dev-shell",
            WorkloadClass::Untrusted => "untrusted",
            WorkloadClass::Function => "function",
        }
    }
}

impl fmt::Display for WorkloadClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorkloadClass {
    type Err = BlazeError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "agent-rl" => Ok(WorkloadClass::AgentRl),
            "agent-tool" => Ok(WorkloadClass::AgentTool),
            "serverless" => Ok(WorkloadClass::Serverless),
            "dev-shell" => Ok(WorkloadClass::DevShell),
            "untrusted" => Ok(WorkloadClass::Untrusted),
            "function" => Ok(WorkloadClass::Function),
            other => Err(BlazeError::PolicyEvalError {
                reason: format!("unknown workload class: {other}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Misc enums in policy schema
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FallbackOnMissingHook {
    #[default]
    Fail,
    Degrade,
    Continue,
}

/// Checkpoint strategy selection.
/// NOTE(Phase 3): v0.1 stores strategy in policy config but does NOT invoke
/// kernel syscalls. Real checkpoint/restore via UFFD-WP deferred to Phase 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckpointStrategy {
    #[serde(rename = "uffd-wp-async")]
    UffdWpAsync,
    Criu,
}

// ---------------------------------------------------------------------------
// Policy file schema
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyFile {
    pub manifest_version: u32,
    pub policy_name: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(rename = "match")]
    pub match_: PolicyMatch,
    pub select: PolicySelect,
    /// Legacy policy `[pool]` input retained for package-upgrade compatibility.
    ///
    /// Exact defaults from the two older packaged policies are accepted but ignored.
    /// Serialization omits the section, and any other value fails validation.
    #[serde(default, skip_serializing)]
    pub pool: Option<toml::Value>,
    #[serde(default)]
    pub checkpoint: Option<PolicyCheckpoint>,
    #[serde(default)]
    pub quota: Option<PolicyQuota>,
    #[serde(default)]
    pub hooks: PolicyHooks,
    #[serde(default)]
    pub backend: BackendConfigs,
    #[serde(default)]
    pub vm: Option<VmConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyMatch {
    pub workload_class: WorkloadClass,
    #[serde(default)]
    pub image_labels: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicySelect {
    pub backend_priority: Vec<BackendKind>,
    #[serde(default)]
    pub kernel_hooks: Vec<String>,
    #[serde(default)]
    pub templates: Vec<String>,
    #[serde(default)]
    pub fallback_on_missing_hook: FallbackOnMissingHook,
}

impl PolicyFile {
    /// Validate internal consistency constraints of a policy file.
    pub fn validate(&self) -> Result<()> {
        if let Some(pool) = &self.pool {
            if !is_legacy_packaged_policy_pool(self, pool) {
                return Err(BlazeError::PolicyEvalError {
                    reason: format!(
                        "policy \"{}\": [pool] is not supported because warm pool management is not implemented",
                        self.policy_name
                    ),
                });
            }
        }
        // Validate [vm] vcpus/memory if present.
        if let Some(vm) = &self.vm {
            validate_vm_resource(&self.policy_name, "[vm]", Some(&vm.memory), Some(vm.vcpus))?;
        }

        // Validate [backend.firecracker] override vcpus/memory if present.
        if let Some(fc) = self.backend.firecracker.as_ref() {
            validate_vm_resource(
                &self.policy_name,
                "[backend.firecracker]",
                fc.memory.as_deref(),
                fc.vcpus,
            )?;
        }

        // Validate Firecracker vCPU upper bound (Firecracker limits to 1..=32).
        validate_firecracker_vcpus(self)?;

        // Validate [quota].cpu_shares is at least 1.
        // cgroup v1 cpu.shares has no hard upper bound, so only reject 0 / negative values.
        if let Some(quota) = self.quota.as_ref()
            && let Some(shares) = quota.cpu_shares
            && shares < 1
        {
            return Err(BlazeError::PolicyEvalError {
                reason: format!(
                    "policy \"{policy_name}\": [quota].cpu_shares must be at least 1, got {shares}",
                    policy_name = self.policy_name
                ),
            });
        }

        Ok(())
    }
}

fn is_legacy_packaged_policy_pool(policy: &PolicyFile, value: &toml::Value) -> bool {
    let expected_sizes = match (policy.policy_name.as_str(), policy.match_.workload_class) {
        ("agent-rl-default", WorkloadClass::AgentRl) => (4, 16, 64),
        ("agent-tool-default", WorkloadClass::AgentTool) => (2, 8, 32),
        _ => return false,
    };
    let Some(table) = value.as_table() else {
        return false;
    };
    table.len() == 6
        && table.get("enabled").and_then(toml::Value::as_bool) == Some(true)
        && table.get("min").and_then(toml::Value::as_integer) == Some(expected_sizes.0)
        && table.get("target").and_then(toml::Value::as_integer) == Some(expected_sizes.1)
        && table.get("max").and_then(toml::Value::as_integer) == Some(expected_sizes.2)
        && table.get("warm_ttl").and_then(toml::Value::as_str) == Some("30m")
        && table.get("reset_mode").and_then(toml::Value::as_str) == Some("full-recreate")
}

fn validate_vm_resource(
    policy_name: &str,
    field: &str,
    memory: Option<&str>,
    vcpus: Option<u32>,
) -> Result<()> {
    if let Some(memory) = memory {
        parse_memory_value(memory).map_err(|e| BlazeError::PolicyEvalError {
            reason: format!("policy \"{policy_name}\": {}", format_parse_memory_error(e)),
        })?;
    }
    if let Some(vcpus) = vcpus
        && vcpus == 0
    {
        return Err(BlazeError::PolicyEvalError {
            reason: format!("policy \"{policy_name}\": {field}.vcpus must be ≥ 1, got 0"),
        });
    }
    Ok(())
}

/// Validate that the *resolved* Firecracker vCPU count is within 1..=32.
/// The resolution priority mirrors the spawner fallback chain:
/// `backend.firecracker.vcpus` > `[vm].vcpus` > code default (1).
fn validate_firecracker_vcpus(policy: &PolicyFile) -> Result<()> {
    if !policy
        .select
        .backend_priority
        .contains(&BackendKind::Firecracker)
    {
        return Ok(());
    }
    let resolved = policy
        .backend
        .firecracker
        .as_ref()
        .and_then(|fc| fc.vcpus)
        .or(policy.vm.as_ref().map(|vm| vm.vcpus))
        .unwrap_or(1);
    if resolved > FIRECRACKER_MAX_VCPUS {
        return Err(BlazeError::PolicyEvalError {
            reason: format!(
                "policy \"{}\": Firecracker vCPU count must be 1..={}, got {}",
                policy.policy_name, FIRECRACKER_MAX_VCPUS, resolved
            ),
        });
    }
    Ok(())
}

fn format_parse_memory_error(e: BlazeError) -> String {
    match e {
        BlazeError::PolicyEvalError { reason } => reason,
        other => other.to_string(),
    }
}

/// Parse a duration string such as "30s", "30m", "1h", "2d". Returns `None` for
/// malformed input, including bare numbers like "300".
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.len() < 2 {
        return None;
    }
    let idx = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if idx == 0 || idx == s.len() {
        return None;
    }
    let (num, unit) = s.split_at(idx);
    let n: u64 = num.parse().ok()?;
    if n == 0 {
        return None;
    }
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86_400)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyCheckpoint {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_checkpoint_strategy")]
    pub strategy: CheckpointStrategy,
}

fn default_checkpoint_strategy() -> CheckpointStrategy {
    CheckpointStrategy::UffdWpAsync
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyQuota {
    #[serde(default)]
    pub cpu_shares: Option<u32>,
    #[serde(default)]
    pub memory_high: Option<String>,
    #[serde(default)]
    pub memory_max: Option<String>,
    #[serde(default)]
    pub pids_max: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyHooks {
    #[serde(default)]
    pub on_create: Option<HookSequence>,
    #[serde(default)]
    pub on_reset: Option<HookSequence>,
    #[serde(default)]
    pub on_destroy: Option<HookSequence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookSequence {
    #[serde(default)]
    pub sequence: Vec<String>,
}

// ---------------------------------------------------------------------------
// Backend-specific configuration
// ---------------------------------------------------------------------------

/// Firecracker-specific knobs. Carried inside [`BackendConfigs`] so that
/// non-VM backends can ignore it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirecrackerConfig {
    /// Guest kernel command line.
    #[serde(default = "default_fc_boot_args")]
    pub boot_args: String,
    /// Enable guest operations over virtio-vsock.
    ///
    /// This is disabled by default and requires a compatible guest service.
    #[serde(default)]
    pub enable_vsock: bool,
    /// Create an isolated network namespace, veth uplink, tap, and NAT.
    #[serde(default)]
    pub enable_network: bool,
    /// Capture guest ttyS0 output (Firecracker stdout) to `serial.log`.
    #[serde(default)]
    pub serial_log: bool,
    /// Override [`VmConfig::vcpus`] for this backend only.
    #[serde(default)]
    pub vcpus: Option<u32>,
    /// Override [`VmConfig::memory`] for this backend only.
    #[serde(default, deserialize_with = "deserialize_optional_memory_size")]
    pub memory: Option<String>,
}

impl Default for FirecrackerConfig {
    fn default() -> Self {
        Self {
            boot_args: default_fc_boot_args(),
            enable_vsock: false,
            enable_network: false,
            serial_log: false,
            vcpus: None,
            memory: None,
        }
    }
}

fn default_fc_boot_args() -> String {
    "console=ttyS0 reboot=k panic=1 pci=off".to_string()
}

/// Per-backend configuration tables. Only the backend selected at runtime
/// consumes its own table; others are ignored.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackendConfigs {
    #[serde(default)]
    pub firecracker: Option<FirecrackerConfig>,
}

// ---------------------------------------------------------------------------
// VM generic configuration
// ---------------------------------------------------------------------------

/// VM-class backend generic resource spec. Only consumed by VM backends
/// (Firecracker, Kata-*, Rund); non-VM backends ignore this section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    /// Guest-visible vCPU count.
    #[serde(default = "default_vm_vcpus")]
    pub vcpus: u32,
    /// Guest-visible memory size with a unit suffix (e.g. "256M", "4G", "1Gi").
    /// Validated at deserialization time to reject malformed or sub-MiB values.
    #[serde(
        default = "default_vm_memory",
        deserialize_with = "deserialize_memory_size"
    )]
    pub memory: String,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            vcpus: default_vm_vcpus(),
            memory: default_vm_memory(),
        }
    }
}

fn default_vm_vcpus() -> u32 {
    1
}

fn default_vm_memory() -> String {
    "256Mi".to_string()
}

const MIB: u64 = 1 << 20;

/// Firecracker hard upper bound for vCPU count.
/// See: https://github.com/firecracker-microvm/firecracker/blob/main/src/firecracker/swagger/firecracker.yaml
const FIRECRACKER_MAX_VCPUS: u32 = 32;

fn validate_memory_size(s: &str) -> std::result::Result<(), String> {
    parse_memory_value(s).map_err(|e| format!("invalid memory size \"{s}\": {e}"))?;
    Ok(())
}

/// Deserialize a memory-size string (e.g. "4G", "512Mi") and validate it
/// upfront using [`parse_memory_value`]. Keeps the original human-readable
/// `String` in the struct while ensuring bad values fail at policy load time.
fn deserialize_memory_size<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    validate_memory_size(&s).map_err(serde::de::Error::custom)?;
    Ok(s)
}

/// Deserialize an optional memory-size string and validate it upfront.
fn deserialize_optional_memory_size<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let maybe = Option::<String>::deserialize(deserializer)?;
    if let Some(s) = &maybe {
        validate_memory_size(s).map_err(serde::de::Error::custom)?;
    }
    Ok(maybe)
}

/// Parse a memory string such as "4G", "512Mi" or "4096" into bytes.
///
/// Supported suffixes (case-sensitive):
/// - Decimal: K (10^3), M (10^6), G (10^9), T (10^12)
/// - Binary:  Ki (2^10), Mi (2^20), Gi (2^30), Ti (2^40)
/// - No suffix means bytes.
///
/// Errors if the value is malformed, overflows u64, or resolves to < 1 MiB.
pub fn parse_memory_value(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        return Err(BlazeError::PolicyEvalError {
            reason: "memory value is empty".into(),
        });
    }

    let first_non_digit = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num_str, suffix) = s.split_at(first_non_digit);

    if num_str.is_empty() {
        return Err(BlazeError::PolicyEvalError {
            reason: format!("invalid memory value \"{s}\" — missing digits"),
        });
    }

    let value: u64 = num_str.parse().map_err(|_| BlazeError::PolicyEvalError {
        reason: format!("invalid memory value \"{s}\""),
    })?;

    let multiplier: u64 = match suffix {
        "" => 1,
        "K" => 1_000,
        "M" => 1_000_000,
        "G" => 1_000_000_000,
        "T" => 1_000_000_000_000,
        "Ki" => 1 << 10,
        "Mi" => 1 << 20,
        "Gi" => 1 << 30,
        "Ti" => 1 << 40,
        other => {
            return Err(BlazeError::PolicyEvalError {
                reason: format!("invalid memory value \"{s}\" — unknown suffix \"{other}\""),
            });
        }
    };

    let bytes = value
        .checked_mul(multiplier)
        .ok_or_else(|| BlazeError::PolicyEvalError {
            reason: format!("memory value \"{s}\" overflows u64"),
        })?;

    if bytes < MIB {
        return Err(BlazeError::PolicyEvalError {
            reason: format!("memory \"{s}\" resolves to {bytes} bytes (< 1 MiB minimum)"),
        });
    }

    Ok(bytes)
}

/// Convert bytes to MiB using ceiling division so the VM gets at least the
/// requested amount of memory.
pub fn to_mib_ceil(bytes: u64) -> u64 {
    bytes.div_ceil(MIB)
}

// ---------------------------------------------------------------------------
// Decision + image metadata
// ---------------------------------------------------------------------------

/// Subset of OCI image metadata that policy evaluation reads.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageMetadata {
    pub digest: String,
    #[serde(default)]
    pub workload_class: Option<WorkloadClass>,
    #[serde(default)]
    pub kernel_version: Option<String>,
}

/// Result of evaluating a request against the policy library for one sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeDecision {
    pub policy_name: String,
    pub workload_class: WorkloadClass,
    pub backend_priority: Vec<BackendKind>,
    pub kernel_hooks: Vec<String>,
    pub templates: Vec<String>,
    pub fallback_on_missing_hook: FallbackOnMissingHook,
    pub checkpoint: Option<PolicyCheckpoint>,
    pub quota: Option<PolicyQuota>,
    pub hooks: PolicyHooks,
    pub backend: BackendConfigs,
    pub vm: Option<VmConfig>,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// In-memory store of policy files, sorted by descending `priority`.
#[derive(Debug, Default, Clone)]
pub struct PolicyEngine {
    policies: Vec<PolicyFile>,
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self {
            policies: Vec::new(),
        }
    }

    /// Construct an engine pre-populated with `policies`. Sorted so that
    /// higher priority is evaluated first.
    pub fn with_policies(mut policies: Vec<PolicyFile>) -> Self {
        policies.sort_by_key(|p| std::cmp::Reverse(p.priority));
        Self { policies }
    }

    /// Load every `*.toml` file under `dir` into a single engine. Files
    /// whose `manifest_version` is unsupported, or whose schema fails to
    /// parse, are wrapped in [`BlazeError::PolicyLoadError`] (caller can
    /// decide between `fail`/`warn` per [`crate::config::PolicyLoadErrorMode`]).
    pub fn load_dir(dir: &Path) -> Result<Self> {
        let mut policies = Vec::new();
        let entries = fs::read_dir(dir).map_err(|e| BlazeError::PolicyLoadError {
            path: dir.to_path_buf(),
            source: Box::new(BlazeError::IoError { source: e }),
        })?;
        for entry in entries {
            let entry = entry.map_err(BlazeError::from)?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let policy = load_one(&path)?;
            policy.validate()?;
            if policy.pool.is_some() {
                tracing::warn!(
                    path = %path.display(),
                    policy_name = %policy.policy_name,
                    workload_class = %policy.match_.workload_class,
                    "ignoring legacy packaged policy [pool] defaults; remove this section because reusable-instance management is unavailable"
                );
            }
            warn_vm_config(&policy);
            tracing::info!(
                policy_name = %policy.policy_name,
                priority = policy.priority,
                path = %path.display(),
                "loaded policy"
            );
            policies.push(policy);
        }
        Ok(Self::with_policies(policies))
    }

    pub fn policies(&self) -> &[PolicyFile] {
        &self.policies
    }

    /// Walk the policy library top-down by priority and return the first
    /// policy whose `match` block matches the request.
    pub fn evaluate(
        &self,
        labels: &HashMap<String, String>,
        image_metadata: &ImageMetadata,
    ) -> Result<RuntimeDecision> {
        let class = image_metadata
            .workload_class
            .ok_or_else(|| BlazeError::PolicyEvalError {
                reason: "image_metadata.workload_class is required (no silent fallback)".into(),
            })?;

        for policy in &self.policies {
            if policy.match_.workload_class != class {
                continue;
            }
            if !labels_match(&policy.match_.image_labels, labels) {
                continue;
            }
            tracing::info!(
                policy = %policy.policy_name,
                class = %class,
                "policy matched"
            );
            return Ok(build_decision(policy));
        }

        Err(BlazeError::PolicyEvalError {
            reason: format!("no policy matched workload_class={class}"),
        })
    }
}

fn load_one(path: &Path) -> Result<PolicyFile> {
    let raw = fs::read_to_string(path).map_err(|e| BlazeError::PolicyLoadError {
        path: path.to_path_buf(),
        source: Box::new(BlazeError::IoError { source: e }),
    })?;
    let policy: PolicyFile = toml::from_str(&raw).map_err(|e| BlazeError::PolicyLoadError {
        path: path.to_path_buf(),
        source: Box::new(BlazeError::from(e)),
    })?;
    if policy.manifest_version != 1 {
        return Err(BlazeError::PolicyLoadError {
            path: path.to_path_buf(),
            source: Box::new(BlazeError::PolicyEvalError {
                reason: format!(
                    "unsupported manifest_version {} (only 1 is supported)",
                    policy.manifest_version
                ),
            }),
        });
    }
    Ok(policy)
}

fn labels_match(required: &HashMap<String, String>, provided: &HashMap<String, String>) -> bool {
    required
        .iter()
        .all(|(k, v)| provided.get(k).map(|got| got == v).unwrap_or(false))
}

fn is_vm_class_backend(kind: BackendKind) -> bool {
    matches!(
        kind,
        BackendKind::Firecracker
            | BackendKind::KataFc
            | BackendKind::KataClh
            | BackendKind::KataQemu
            | BackendKind::Rund
    )
}

fn backend_has_vm_override(policy: &PolicyFile, kind: BackendKind) -> bool {
    match kind {
        BackendKind::Firecracker => policy
            .backend
            .firecracker
            .as_ref()
            .map(|fc| fc.vcpus.is_some() || fc.memory.is_some())
            .unwrap_or(false),
        // Phase 1: only Firecracker has a [backend.*] override section. Other
        // VM-class backends always return false here so users are not warned
        // about a missing override they cannot yet provide.
        _ => false,
    }
}

fn warn_vm_config(policy: &PolicyFile) {
    let has_vm = policy.vm.is_some();
    let vm_backends: Vec<_> = policy
        .select
        .backend_priority
        .iter()
        .copied()
        .filter(|b| is_vm_class_backend(*b))
        .collect();
    let has_vm_backend = !vm_backends.is_empty();

    if has_vm && !has_vm_backend {
        tracing::warn!(
            policy = %policy.policy_name,
            "[vm] defined but backend_priority contains no VM-class backend — [vm] section will be ignored"
        );
    }

    if has_vm_backend && !has_vm {
        let missing: Vec<_> = vm_backends
            .into_iter()
            .filter(|kind| !backend_has_vm_override(policy, *kind))
            .map(|kind| kind.as_str())
            .collect();

        if !missing.is_empty() {
            tracing::warn!(
                policy = %policy.policy_name,
                backends = %missing.join(", "),
                "VM backends in priority but no [vm] or backend override for vcpus/memory defined — using defaults (vcpus=1, memory=\"256Mi\")"
            );
        }
    }
}

fn build_decision(policy: &PolicyFile) -> RuntimeDecision {
    RuntimeDecision {
        policy_name: policy.policy_name.clone(),
        workload_class: policy.match_.workload_class,
        backend_priority: policy.select.backend_priority.clone(),
        kernel_hooks: policy.select.kernel_hooks.clone(),
        templates: policy.select.templates.clone(),
        fallback_on_missing_hook: policy.select.fallback_on_missing_hook,
        checkpoint: policy.checkpoint.clone(),
        quota: policy.quota.clone(),
        hooks: policy.hooks.clone(),
        backend: policy.backend.clone(),
        vm: policy.vm.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample_toml() -> &'static str {
        r#"
manifest_version = 1
policy_name = "agent-rl-default"
priority = 100

[match]
workload_class = "agent-rl"
image_labels = { "ai.anolisa.workload" = "rl-rollout" }

[select]
backend_priority = ["kata-fc", "kata-clh", "rund"]
kernel_hooks = ["mm-template", "uffd-wp"]
templates = ["mm-template"]
fallback_on_missing_hook = "fail"

[checkpoint]
enabled = true
strategy = "uffd-wp-async"

[quota]
cpu_shares = 1024
memory_high = "2G"
memory_max = "4G"
pids_max = 4096

[vm]
vcpus = 4
memory = "4G"

[backend.firecracker]
boot_args = "console=ttyS0 reboot=k panic=1 pci=off"
enable_network = true
serial_log = true

[hooks.on_create]
sequence = ["template-reg:bind-mm-template"]
"#
    }

    #[test]
    fn parses_full_schema() {
        let pf: PolicyFile = toml::from_str(sample_toml()).expect("parse");
        assert_eq!(pf.policy_name, "agent-rl-default");
        assert_eq!(pf.match_.workload_class, WorkloadClass::AgentRl);
        assert_eq!(pf.select.backend_priority[0], BackendKind::KataFc);
        assert!(
            pf.backend
                .firecracker
                .as_ref()
                .expect("firecracker")
                .enable_network
        );
    }

    #[test]
    fn evaluate_picks_highest_priority_match() {
        let p1: PolicyFile = toml::from_str(sample_toml()).expect("parse");
        let mut p2 = p1.clone();
        p2.policy_name = "agent-rl-override".into();
        p2.priority = 200;
        p2.select.backend_priority = vec![BackendKind::Rund];

        let engine = PolicyEngine::with_policies(vec![p1, p2]);

        let labels = HashMap::from([("ai.anolisa.workload".into(), "rl-rollout".into())]);
        let img = ImageMetadata {
            digest: "sha256:abc".into(),
            workload_class: Some(WorkloadClass::AgentRl),
            kernel_version: None,
        };
        let decision = engine.evaluate(&labels, &img).expect("matches");
        assert_eq!(decision.policy_name, "agent-rl-override");
        assert_eq!(decision.backend_priority, vec![BackendKind::Rund]);
    }

    #[test]
    fn evaluate_rejects_when_no_class() {
        let engine = PolicyEngine::new();
        let img = ImageMetadata::default();
        let err = engine
            .evaluate(&HashMap::new(), &img)
            .expect_err("no class");
        assert!(matches!(err, BlazeError::PolicyEvalError { .. }));
    }

    #[test]
    fn evaluate_rejects_when_no_match() {
        let p: PolicyFile = toml::from_str(sample_toml()).expect("parse");
        let engine = PolicyEngine::with_policies(vec![p]);
        let img = ImageMetadata {
            digest: "sha256:abc".into(),
            workload_class: Some(WorkloadClass::Function),
            kernel_version: None,
        };
        let err = engine
            .evaluate(&HashMap::new(), &img)
            .expect_err("no match");
        assert!(matches!(err, BlazeError::PolicyEvalError { .. }));
    }

    #[test]
    fn load_dir_reads_toml_files() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("agent-rl.toml");
        let mut f = fs::File::create(&path).expect("create");
        f.write_all(sample_toml().as_bytes()).expect("write");
        // a non-toml file should be skipped silently.
        fs::write(tmp.path().join("README"), b"ignore me").expect("write");

        let engine = PolicyEngine::load_dir(tmp.path()).expect("load");
        assert_eq!(engine.policies().len(), 1);
    }

    #[test]
    fn load_dir_reads_vm_config() {
        let tmp = tempfile::tempdir().expect("tmp");
        let raw = r#"
manifest_version = 1
policy_name = "vm-test"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[vm]
vcpus = 4
memory = "4G"

[backend.firecracker]
vcpus = 2
memory = "2G"
"#;
        fs::write(tmp.path().join("vm-policy.toml"), raw).expect("write");

        let engine = PolicyEngine::load_dir(tmp.path()).expect("load");
        assert_eq!(engine.policies().len(), 1);
        let policy = &engine.policies()[0];
        let vm = policy.vm.as_ref().expect("vm config");
        assert_eq!(vm.vcpus, 4);
        assert_eq!(vm.memory, "4G");
        let fc = policy
            .backend
            .firecracker
            .as_ref()
            .expect("firecracker config");
        assert_eq!(fc.vcpus, Some(2));
        assert_eq!(fc.memory, Some("2G".to_string()));
        assert!(!fc.enable_network);
    }

    #[test]
    fn parse_memory_value_accepts_units() {
        assert_eq!(parse_memory_value("1048576").unwrap(), 1 << 20);
        assert_eq!(parse_memory_value("2048K").unwrap(), 2_048_000);
        assert_eq!(parse_memory_value("2M").unwrap(), 2_000_000);
        assert_eq!(parse_memory_value("1G").unwrap(), 1_000_000_000);
        assert_eq!(parse_memory_value("1024Ki").unwrap(), 1 << 20);
        assert_eq!(parse_memory_value("1Mi").unwrap(), 1 << 20);
        assert_eq!(parse_memory_value("1Gi").unwrap(), 1 << 30);
    }

    #[test]
    fn parse_memory_value_rejects_invalid() {
        assert!(parse_memory_value("").is_err());
        assert!(parse_memory_value("4g").is_err());
        assert!(parse_memory_value("1.5G").is_err());
        assert!(parse_memory_value("512K").is_err()); // < 1 MiB
        assert!(parse_memory_value("0").is_err()); // < 1 MiB
    }

    #[test]
    fn to_mib_ceil_rounds_up() {
        assert_eq!(to_mib_ceil(1 << 20), 1);
        assert_eq!(to_mib_ceil((1 << 20) + 1), 2);
        assert_eq!(to_mib_ceil(1_500_000_000), 1431); // 1500M -> ceil to MiB
    }

    #[test]
    fn vm_config_parses_and_validates() {
        let raw = r#"
manifest_version = 1
policy_name = "test"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[vm]
vcpus = 4
memory = "4G"
"#;
        let pf: PolicyFile = toml::from_str(raw).expect("parse");
        let vm = pf.vm.as_ref().expect("vm");
        assert_eq!(vm.vcpus, 4);
        assert_eq!(vm.memory, "4G");
        pf.validate().expect("valid");
    }

    #[test]
    fn validate_rejects_zero_vcpus() {
        let raw = r#"
manifest_version = 1
policy_name = "test"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[vm]
vcpus = 0
memory = "4G"
"#;
        let pf: PolicyFile = toml::from_str(raw).expect("parse");
        assert!(pf.validate().is_err());
    }

    #[test]
    fn deserialize_rejects_too_small_memory() {
        let raw = r#"
manifest_version = 1
policy_name = "test"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[vm]
vcpus = 1
memory = "512K"
"#;
        let err = toml::from_str::<PolicyFile>(raw).expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("512K") && msg.contains("< 1 MiB"),
            "error should mention sub-MiB memory: {msg}"
        );
    }

    #[test]
    fn validate_error_includes_policy_name() {
        let raw = r#"
manifest_version = 1
policy_name = "named-policy"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[vm]
vcpus = 0
memory = "4G"
"#;
        let pf: PolicyFile = toml::from_str(raw).expect("parse");
        let err = pf.validate().expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains(r#"policy "named-policy":"#),
            "error should include policy name: {msg}"
        );
    }

    #[test]
    fn validate_rejects_cpu_shares_below_one() {
        let raw = r#"
manifest_version = 1
policy_name = "test"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[quota]
cpu_shares = 0
"#;
        let pf: PolicyFile = toml::from_str(raw).expect("parse");
        assert!(pf.validate().is_err());
    }

    #[test]
    fn validate_rejects_unsupported_pool_section() {
        let raw = r#"
manifest_version = 1
policy_name = "test"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[pool]
enabled = false
"#;
        let pf: PolicyFile = toml::from_str(raw).expect("parse");
        let error = pf.validate().expect_err("unsupported pool section");
        assert!(
            error
                .to_string()
                .contains("warm pool management is not implemented"),
            "{error}"
        );
    }

    fn packaged_pool_policy_toml(
        policy_name: &str,
        workload_class: &str,
        min: i64,
        target: i64,
        max: i64,
    ) -> String {
        format!(
            r#"
manifest_version = 1
policy_name = "{policy_name}"
priority = 777

[match]
workload_class = "{workload_class}"

[select]
backend_priority = ["firecracker"]

[pool]
enabled = true
min = {min}
target = {target}
max = {max}
warm_ttl = "30m"
reset_mode = "full-recreate"
"#
        )
    }

    #[test]
    fn validate_accepts_only_packaged_policy_pool_defaults_without_serializing_them() {
        for (policy_name, workload_class, min, target, max) in [
            ("agent-rl-default", "agent-rl", 4, 16, 64),
            ("agent-tool-default", "agent-tool", 2, 8, 32),
        ] {
            let raw = packaged_pool_policy_toml(policy_name, workload_class, min, target, max);
            let policy: PolicyFile = toml::from_str(&raw).expect("packaged policy parses");

            policy
                .validate()
                .expect("packaged policy remains upgrade-compatible");
            let serialized = toml::to_string(&policy).expect("serialize policy");
            assert!(!serialized.contains("[pool]"));
            assert!(!serialized.contains("warm_ttl"));
            assert!(!serialized.contains("reset_mode"));
        }
    }

    #[test]
    fn validate_rejects_modified_packaged_policy_pool_defaults() {
        let raw = packaged_pool_policy_toml("agent-tool-default", "agent-tool", 2, 8, 32);
        let base: PolicyFile = toml::from_str(&raw).expect("packaged policy parses");
        let mut modified = Vec::new();

        let mut wrong_name = base.clone();
        wrong_name.policy_name = "renamed-agent-tool".to_string();
        modified.push(wrong_name);

        let mut wrong_class = base.clone();
        wrong_class.match_.workload_class = WorkloadClass::AgentRl;
        modified.push(wrong_class);

        let rl_raw = packaged_pool_policy_toml("agent-rl-default", "agent-rl", 4, 16, 64);
        let mut wrong_rl_class: PolicyFile =
            toml::from_str(&rl_raw).expect("packaged policy parses");
        wrong_rl_class.match_.workload_class = WorkloadClass::AgentTool;
        modified.push(wrong_rl_class);

        let mut missing = base.clone();
        missing
            .pool
            .as_mut()
            .and_then(toml::Value::as_table_mut)
            .expect("pool table")
            .remove("max");
        modified.push(missing);

        let mut extra = base.clone();
        extra
            .pool
            .as_mut()
            .and_then(toml::Value::as_table_mut)
            .expect("pool table")
            .insert("extra".to_string(), toml::Value::Boolean(true));
        modified.push(extra);

        for (key, value) in [
            ("enabled", toml::Value::Boolean(false)),
            ("min", toml::Value::Integer(3)),
            ("target", toml::Value::Integer(9)),
            ("max", toml::Value::Integer(33)),
            ("warm_ttl", toml::Value::String("31m".to_string())),
            (
                "reset_mode",
                toml::Value::String("reuse-runtime".to_string()),
            ),
            ("min", toml::Value::String("2".to_string())),
        ] {
            let mut policy = base.clone();
            policy
                .pool
                .as_mut()
                .and_then(toml::Value::as_table_mut)
                .expect("pool table")
                .insert(key.to_string(), value);
            modified.push(policy);
        }

        for policy in modified {
            policy
                .validate()
                .expect_err("modified pool settings remain unsupported");
        }
    }

    #[test]
    fn load_dir_accepts_both_preserved_packaged_policy_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("agent-rl.toml"),
            packaged_pool_policy_toml("agent-rl-default", "agent-rl", 4, 16, 64),
        )
        .expect("write agent-rl policy");
        fs::write(
            tmp.path().join("agent-tool.toml"),
            packaged_pool_policy_toml("agent-tool-default", "agent-tool", 2, 8, 32),
        )
        .expect("write agent-tool policy");

        let engine = PolicyEngine::load_dir(tmp.path()).expect("load preserved policies");

        assert_eq!(engine.policies().len(), 2);
        assert!(engine.policies().iter().all(|policy| policy.pool.is_some()));
    }

    #[test]
    fn validate_rejects_firecracker_vcpus_above_32() {
        let raw = r#"
manifest_version = 1
policy_name = "test-fc-vcpu-limit"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[vm]
vcpus = 33
memory = "4G"
"#;
        let pf: PolicyFile = toml::from_str(raw).expect("parse");
        let err = pf.validate().expect_err("should reject vcpus > 32");
        let msg = err.to_string();
        assert!(
            msg.contains("1..=32") && msg.contains("33"),
            "error should mention the limit and actual value: {msg}"
        );
    }

    #[test]
    fn validate_accepts_firecracker_vcpus_at_boundary() {
        let raw = r#"
manifest_version = 1
policy_name = "test-fc-vcpu-ok"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[vm]
vcpus = 32
memory = "4G"
"#;
        let pf: PolicyFile = toml::from_str(raw).expect("parse");
        pf.validate().expect("vcpus=32 should be valid");
    }

    #[test]
    fn validate_rejects_firecracker_override_vcpus_above_32() {
        let raw = r#"
manifest_version = 1
policy_name = "test-fc-override"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["firecracker"]

[vm]
vcpus = 4
memory = "4G"

[backend.firecracker]
vcpus = 64
"#;
        let pf: PolicyFile = toml::from_str(raw).expect("parse");
        let err = pf
            .validate()
            .expect_err("should reject override vcpus > 32");
        let msg = err.to_string();
        assert!(
            msg.contains("1..=32") && msg.contains("64"),
            "error should mention the limit and actual value: {msg}"
        );
    }

    #[test]
    fn parse_duration_accepts_common_units() {
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(30 * 60));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(
            parse_duration("2d").unwrap(),
            Duration::from_secs(2 * 86_400)
        );
        assert!(parse_duration("300").is_none());
        assert!(parse_duration("").is_none());
        assert!(parse_duration("1x").is_none());
        assert!(
            parse_duration("0s").is_none(),
            "zero duration should be rejected"
        );
        assert!(
            parse_duration("5秒").is_none(),
            "multi-byte suffix should be rejected without panicking"
        );
    }
}
