//! Shared telemetry primitives: configuration, errors, region detection, and
//! product type probing.
//!
//! The common library for the `telemetry` module tree: holds the
//! [`TelemetryConfig`] paths, the shared [`TelemetryError`], the
//! [`RegionProbe`] used to pick the SLS endpoint host, and the
//! [`ProductType`] read from `/etc/anolisa-release`.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::cell::RefCell;

use serde::{Deserialize, Serialize};

use crate::telemetry::metadata::MetadataClient;

// ── Product type ─────────────────────────────────────────────────────

/// Product type resolved through the shared probe chain
/// (release file → container detection → metadata API → unknown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProductType {
    /// Alibaba Cloud ECS
    Ecs,
    /// Simple Application Server (SWAS)
    Swas,
    /// Elastic Desktop Service (EDS)
    Eds,
    /// Any container runtime flavor (docker, podman, kubepods, ...): the
    /// process runs inside a container, which counts as its own statistical
    /// device rather than inheriting the host's cloud product label.
    Container,
    /// Unknown / self-hosted environment
    Unknown,
}

impl ProductType {
    pub fn display_name(&self) -> &str {
        match self {
            ProductType::Ecs => "ECS",
            ProductType::Swas => "Simple Application Server",
            ProductType::Eds => "Elastic Desktop Service",
            ProductType::Container => "Container",
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

// Test-only pin of the container flavor probe result:
// `Some(Some(flavor))` forces that flavor, `Some(None)` forces a
// bare-metal answer, `None` means "use the real probe".
#[cfg(test)]
thread_local! {
    static CONTAINER_FLAVOR_OVERRIDE: RefCell<Option<Option<String>>> =
        const { RefCell::new(None) };
}

/// Resolve the container runtime flavor, honouring the test override.
///
/// Delegates to `anolisa_env::detect_container` (marker files + cgroup
/// heuristics); the override lets unit tests pin the result so assertions
/// do not flip when the suite itself runs inside a container. Shared by
/// product-type probing and the instance snapshot so both see one answer.
pub(crate) fn container_flavor() -> Option<String> {
    #[cfg(test)]
    if let Some(flavor) = CONTAINER_FLAVOR_OVERRIDE.with(|o| o.borrow().clone()) {
        return flavor;
    }
    anolisa_env::detect_container()
}

/// Test helper: run `f` with the container flavor probe pinned.
///
/// `Some("docker")` forces a container flavor, `None` forces a bare-metal
/// answer, regardless of the host the suite runs on. Thread-local, so
/// parallel tests on other threads are unaffected.
#[cfg(test)]
pub(crate) fn with_container_flavor<F, R>(flavor: Option<&str>, f: F) -> R
where
    F: FnOnce() -> R,
{
    CONTAINER_FLAVOR_OVERRIDE.with(|o| {
        *o.borrow_mut() = Some(flavor.map(|s| s.to_string()));
    });
    let result = f();
    CONTAINER_FLAVOR_OVERRIDE.with(|o| {
        *o.borrow_mut() = None;
    });
    result
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

/// Detect the product type through the shared probe chain.
///
/// Wraps [`probe_product_type`] so register status and telemetry reporting
/// agree on one resolution order (release file → container → metadata →
/// unknown). Custom `PRODUCT_TYPE` values from downstream distributions
/// collapse to [`ProductType::Unknown`] here while [`probe_product_type`]
/// passes them through verbatim; extend this mapping when a value needs
/// first-class CLI display.
pub fn detect_product_type(release_path: &Path, client: &MetadataClient) -> ProductType {
    match probe_product_type(release_path, client).as_str() {
        "ecs" => ProductType::Ecs,
        "swas" => ProductType::Swas,
        "eds" => ProductType::Eds,
        "container" => ProductType::Container,
        _ => ProductType::Unknown,
    }
}

/// Probe product type with full fallback chain.
///
/// Resolution order:
/// 1. `/etc/anolisa-release` `PRODUCT_TYPE` field, passed through verbatim
///    (custom values included).
/// 2. Container detection: any runtime flavor → `container`.
/// 3. Metadata API / cloud-init `desktop-id` starting with `ecd` → `eds`.
/// 4. Metadata API / cloud-init `instance/instance-type` starting with `ecs`
///    → `ecs`.
/// 5. Fallback: `unknown`.
///
/// The container check deliberately precedes the metadata probes: from
/// inside a container the metadata endpoint usually remains reachable but
/// answers for the *host*, which would mislabel the container — a separate
/// statistical device — as its host's cloud product. Consequently, container
/// images built from VM images must remove or refresh
/// `/etc/anolisa-release`; a stale file left in an image layer wins over
/// container detection.
///
/// Reuses the caller's [`MetadataClient`] so the short-circuit flag
/// (metadata-unreachable) is shared across all probes.
pub fn probe_product_type(release_path: &Path, client: &MetadataClient) -> String {
    // 1. /etc/anolisa-release PRODUCT_TYPE field — an explicit declaration
    //    always wins over probing.
    if let Ok(content) = fs::read_to_string(release_path)
        && let Some(pt) = find_product_type_in_release(&content)
    {
        return pt;
    }

    // 2. Container detection — local and deterministic. Inside a container
    //    the metadata API typically stays reachable but answers for the
    //    host, so this check must come first for the container device to be
    //    labelled "container" rather than its host's cloud product.
    if container_flavor().is_some() {
        return "container".to_string();
    }

    // 3. EDS detection: desktop-id starts with "ecd"
    if let Some(desktop_id) = client.query("desktop-id")
        && desktop_id.starts_with("ecd")
    {
        return "eds".to_string();
    }

    // 4. ECS detection: instance-type starts with "ecs"
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
    use crate::telemetry::metadata::MetadataClient;

    fn dead_client() -> MetadataClient {
        MetadataClient::new("http://127.0.0.1:19999/no-such-endpoint")
    }

    #[test]
    fn test_detect_product_type_case_insensitive() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("anolisa-release");
        std::fs::write(&path, "PRODUCT_TYPE=ECS\n").unwrap();
        crate::telemetry::metadata::with_cloud_init_disabled(|| {
            with_container_flavor(None, || {
                assert_eq!(detect_product_type(&path, &dead_client()), ProductType::Ecs);
            })
        });
    }

    #[test]
    fn test_probe_product_type_container_when_no_release() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("anolisa-release");
        crate::telemetry::metadata::with_cloud_init_disabled(|| {
            with_container_flavor(Some("docker"), || {
                assert_eq!(probe_product_type(&path, &dead_client()), "container");
            })
        });
    }

    #[test]
    fn test_probe_product_type_release_wins_over_container() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("anolisa-release");
        std::fs::write(&path, "PRODUCT_TYPE=ecs\n").unwrap();
        with_container_flavor(Some("docker"), || {
            assert_eq!(probe_product_type(&path, &dead_client()), "ecs");
        });
    }

    #[test]
    fn test_probe_product_type_container_wins_over_metadata() {
        // cloud-init would answer "ecs" for instance-type; the container
        // layer must short-circuit before any metadata query runs.
        crate::telemetry::metadata::with_cloud_init_responses(
            &[("ds.meta_data.instance.instance-type", "ecs.c7.xlarge")],
            || {
                with_container_flavor(Some("docker"), || {
                    let dir = tempfile::TempDir::new().unwrap();
                    let path = dir.path().join("anolisa-release");
                    assert_eq!(probe_product_type(&path, &dead_client()), "container");
                });
            },
        );
    }

    #[test]
    fn test_detect_product_type_maps_container() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("anolisa-release");
        crate::telemetry::metadata::with_cloud_init_disabled(|| {
            with_container_flavor(Some("docker"), || {
                assert_eq!(
                    detect_product_type(&path, &dead_client()),
                    ProductType::Container
                );
            })
        });
    }

    #[test]
    fn test_detect_product_type_custom_release_collapses_to_unknown() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("anolisa-release");
        std::fs::write(&path, "PRODUCT_TYPE=mycloud\n").unwrap();
        crate::telemetry::metadata::with_cloud_init_disabled(|| {
            with_container_flavor(None, || {
                // The String path passes custom values through; the enum
                // path deliberately collapses them (documented divergence).
                assert_eq!(probe_product_type(&path, &dead_client()), "mycloud");
                assert_eq!(
                    detect_product_type(&path, &dead_client()),
                    ProductType::Unknown
                );
            })
        });
    }

    #[test]
    fn test_product_type_container_serde_roundtrip() {
        assert_eq!(
            serde_json::to_string(&ProductType::Container).unwrap(),
            "\"container\""
        );
        assert_eq!(
            serde_json::from_str::<ProductType>("\"container\"").unwrap(),
            ProductType::Container
        );
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
