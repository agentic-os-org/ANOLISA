//! Shared telemetry primitives: configuration, errors, region detection, and
//! product type probing.
//!
//! The common library for the `telemetry` module tree: holds the
//! [`TelemetryConfig`] paths, the shared [`TelemetryError`], the
//! [`RegionProbe`] used to pick the SLS endpoint host, and the
//! [`ProductType`] read from `/etc/anolisa-release`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::telemetry::metadata::MetadataClient;

// ── Product type ─────────────────────────────────────────────────────

/// Product type, read from the `PRODUCT_TYPE` field in `/etc/anolisa-release`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProductType {
    /// Alibaba Cloud ECS
    Ecs,
    /// Simple Application Server (SWAS)
    Swas,
    /// Elastic Desktop Service (EDS)
    Eds,
    /// Unknown / self-hosted environment
    Unknown,
}

impl ProductType {
    pub fn display_name(&self) -> &str {
        match self {
            ProductType::Ecs => "ECS",
            ProductType::Swas => "Simple Application Server",
            ProductType::Eds => "Elastic Desktop Service",
            ProductType::Unknown => "Unknown",
        }
    }
}

impl std::fmt::Display for ProductType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// Parse the non-empty `PRODUCT_TYPE` value from `/etc/anolisa-release` content.
pub(crate) fn find_product_type_in_release(content: &str) -> Option<String> {
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("PRODUCT_TYPE=") {
            let pt = val.trim().to_ascii_lowercase();
            if !pt.is_empty() {
                return Some(pt);
            }
        }
    }
    None
}

/// Shared filesystem paths for telemetry setup and instance probing.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Instance metadata URL (ECS internal network)
    pub metadata_url: String,
    /// Ops directory for component .jsonl files
    pub ops_dir: PathBuf,
    /// logrotate config path for ops .jsonl files
    pub logrotate_config_path: PathBuf,
    /// Instance ID cache path
    pub instance_id_cache_path: PathBuf,
    /// Persisted personal identity (`instance_id` / `uid`) mirrored here when
    /// the operator links named reporting; read by the uploader and erased on
    /// unlink.
    pub identity_cache_path: PathBuf,
    /// Path to `/etc/machine-id` (used as instance ID fallback)
    pub machine_id_path: PathBuf,
    /// Path to `/etc/anolisa-release` (used for product type detection)
    pub release_path: PathBuf,
    /// Path to `/etc/os-release` (used for distro detection)
    pub os_release_path: PathBuf,
    /// Path to `/sys/devices/system/cpu/present` (used for vCPU count)
    pub cpu_present_path: PathBuf,
    /// Path to `/etc/image-id` (used for image ID detection)
    pub image_id_path: PathBuf,
    /// Persistent telemetry id file (UUID generated once, reused across reboots).
    pub telemetry_id_path: PathBuf,
    /// Path to the legacy ilogtail account configuration file.
    ///
    /// This file lists the SLS account ids and ilogtail `users/` directories
    /// that should be cleaned up when migrating to the self-hosted uploader.
    /// Kept out of the source tree so downstream distributions can inject
    /// their own values without forking the code.
    pub legacy_accounts_path: PathBuf,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            metadata_url: "http://100.100.100.200/latest/meta-data/region-id".into(),
            ops_dir: PathBuf::from("/var/log/anolisa/sls/ops"),
            logrotate_config_path: PathBuf::from("/etc/logrotate.d/anolisa"),
            instance_id_cache_path: PathBuf::from("/var/lib/anolisa/instance-id.cache"),
            identity_cache_path: PathBuf::from("/var/lib/anolisa/telemetry/identity.json"),
            machine_id_path: PathBuf::from("/etc/machine-id"),
            release_path: PathBuf::from("/etc/anolisa-release"),
            os_release_path: PathBuf::from("/etc/os-release"),
            cpu_present_path: PathBuf::from("/sys/devices/system/cpu/present"),
            image_id_path: PathBuf::from("/etc/image-id"),
            telemetry_id_path: PathBuf::from("/var/lib/anolisa/telemetry/telemetry-id"),
            legacy_accounts_path: PathBuf::from("/etc/anolisa/legacy-accounts.json"),
        }
    }
}

/// Detect product type from `/etc/anolisa-release`, falling back to `Unknown`.
pub fn detect_product_type(release_path: &Path) -> ProductType {
    fs::read_to_string(release_path)
        .ok()
        .and_then(|content| {
            find_product_type_in_release(&content).map(|pt| match pt.as_str() {
                "ecs" => ProductType::Ecs,
                "swas" => ProductType::Swas,
                "eds" => ProductType::Eds,
                _ => ProductType::Unknown,
            })
        })
        .unwrap_or(ProductType::Unknown)
}

/// Probe product type with full fallback chain.
///
/// Resolution order:
/// 1. `/etc/anolisa-release` `PRODUCT_TYPE` field.
/// 2. Metadata API / cloud-init `desktop-id` starting with `ecd` → `eds`.
/// 3. Metadata API / cloud-init `instance/instance-type` starting with `ecs` → `ecs`.
/// 4. Fallback: `unknown`.
///
/// Reuses the caller's [`MetadataClient`] so the short-circuit flag
/// (metadata-unreachable) is shared across all probes.
pub fn probe_product_type(release_path: &Path, client: &MetadataClient) -> String {
    // 1. /etc/anolisa-release PRODUCT_TYPE field
    if let Ok(content) = fs::read_to_string(release_path)
        && let Some(pt) = find_product_type_in_release(&content)
    {
        return pt;
    }

    // 2. EDS detection: desktop-id starts with "ecd"
    if let Some(desktop_id) = client.query("desktop-id")
        && desktop_id.starts_with("ecd")
    {
        return "eds".to_string();
    }

    // 3. ECS detection: instance-type starts with "ecs"
    if let Some(instance_type) = client.query("instance/instance-type")
        && instance_type.starts_with("ecs")
    {
        return "ecs".to_string();
    }

    "unknown".to_string()
}

// ── Error types ───────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── Region detection ──────────────────────────────────────────────────

/// Detection result: region-id + whether to use the internal network.
#[derive(Debug, Clone)]
pub struct RegionInfo {
    pub region_id: String,
    /// true  = use Alibaba Cloud internal network URL (instance metadata API reachable)
    /// false = use public network URL (self-hosted / external network)
    pub use_internal: bool,
}

/// region-id probe.
///
/// Priority:
/// 1. ECS instance metadata API (`http://100.100.100.200/latest/meta-data/region-id`)
///    / `cloud-init query ds` → on success `use_internal = true`.
/// 2. fallback `cn-hangzhou` → `use_internal = false` (use public network).
pub struct RegionProbe {
    client: MetadataClient,
}

impl RegionProbe {
    pub fn new(metadata_url: &str) -> Self {
        Self {
            client: MetadataClient::from_key_url(metadata_url),
        }
    }

    /// Construct from an existing [`MetadataClient`] so the uploader can share
    /// one client (and its unreachable short-circuit flag) between region and
    /// product-type probes.
    pub fn with_client(client: MetadataClient) -> Self {
        Self { client }
    }

    /// Detect region-id and infer network environment to decide internal vs public network.
    ///
    /// # Errors
    ///
    /// Currently infallible (falls back to `cn-hangzhou`); returns `Result` so
    /// future probe strategies can surface hard failures.
    pub fn probe(&self) -> Result<RegionInfo, TelemetryError> {
        // Unified probe: metadata API first, then cloud-init datasource.
        if let Some(region) = self.client.query("region-id") {
            return Ok(RegionInfo {
                region_id: region,
                use_internal: true,
            });
        }
        // Self-hosted: fallback to cn-hangzhou, use public network
        Ok(RegionInfo {
            region_id: "cn-hangzhou".to_string(),
            use_internal: false,
        })
    }
}

// ── Test helpers ───────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) fn test_config(dir: &tempfile::TempDir) -> TelemetryConfig {
    TelemetryConfig {
        metadata_url: "http://127.0.0.1:19999/no-such-endpoint".into(),
        ops_dir: dir.path().join("ops"),
        logrotate_config_path: dir.path().join("logrotate-anolisa"),
        instance_id_cache_path: dir.path().join("instance-id.cache"),
        identity_cache_path: dir.path().join("identity.json"),
        machine_id_path: dir.path().join("machine-id"),
        release_path: dir.path().join("anolisa-release"),
        os_release_path: dir.path().join("os-release"),
        cpu_present_path: dir.path().join("cpu-present"),
        image_id_path: dir.path().join("image-id"),
        telemetry_id_path: dir.path().join("telemetry-id"),
        legacy_accounts_path: dir.path().join("legacy-accounts.json"),
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_product_type_case_insensitive() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("anolisa-release");
        std::fs::write(&path, "PRODUCT_TYPE=ECS\n").unwrap();
        assert_eq!(detect_product_type(&path), ProductType::Ecs);
    }

    #[test]
    fn test_region_fallback_when_both_unavailable() {
        crate::telemetry::metadata::with_cloud_init_disabled(|| {
            let probe = RegionProbe::new("http://127.0.0.1:19999/nope");
            let info = probe.probe().unwrap();
            assert_eq!(info.region_id, "cn-hangzhou");
            assert!(!info.use_internal);
        });
    }
}
