// SPDX-License-Identifier: Apache-2.0
//! Daemon configuration (`/etc/anolisa/blaze/config.toml`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{BlazeError, ConfigErrorSource, Result};
use crate::policy::parse_duration;

/// Top-level daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonConfig {
    #[serde(default)]
    pub daemon: DaemonSection,
    #[serde(default)]
    pub listen: ListenSection,
    /// Backend name → binary path mapping (e.g. `firecracker = "/usr/bin/firecracker"`).
    #[serde(default)]
    pub backends: HashMap<String, PathBuf>,
    #[serde(default)]
    pub policy: PolicySection,
    #[serde(default)]
    pub storage: StorageSection,
    /// Legacy `[pool]` input retained for package-upgrade compatibility.
    ///
    /// The exact defaults shipped by older packages are accepted but ignored.
    /// Serialization omits the section, and any other value fails validation.
    #[serde(default, skip_serializing)]
    pub pool: Option<toml::Value>,
    #[serde(default)]
    pub template: TemplateSection,
    #[serde(default)]
    pub metrics: MetricsSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonSection {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default = "default_socket")]
    pub socket: PathBuf,
}

impl Default for DaemonSection {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
            state_dir: default_state_dir(),
            socket: default_socket(),
        }
    }
}

/// Remote API listener configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListenSection {
    /// TCP address for remote HTTP API (e.g. "0.0.0.0:14159").
    /// Empty string or absent means remote API is disabled.
    #[serde(default)]
    pub http_addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicySection {
    #[serde(default = "default_policy_dir")]
    pub dir: PathBuf,
    #[serde(default = "default_on_load_error")]
    pub on_load_error: PolicyLoadErrorMode,
}

impl Default for PolicySection {
    fn default() -> Self {
        Self {
            dir: default_policy_dir(),
            on_load_error: default_on_load_error(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolicyLoadErrorMode {
    Fail,
    Warn,
}

/// Published template catalog and its local import boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateSection {
    /// Directory containing atomically published template artifact sets.
    #[serde(default = "default_template_dir")]
    pub dir: PathBuf,
    /// Optional root containing operator-prepared import sources.
    ///
    /// Imports are disabled when this value is absent. API callers provide a
    /// relative path below this root rather than an arbitrary daemon path.
    #[serde(default)]
    pub import_root: Option<PathBuf>,
    /// Maximum files in one published entry, including generated `template.json`.
    #[serde(default = "default_runtime_template_max_files")]
    pub max_files: usize,
    /// Maximum final artifact and generated metadata bytes for one import.
    #[serde(default = "default_runtime_template_max_bytes")]
    pub max_bytes: u64,
    /// Maximum serialized size of one published `template.json`.
    #[serde(default = "default_runtime_template_max_metadata_bytes")]
    pub max_metadata_bytes: u64,
    /// Maximum committed catalog bytes plus concurrent import reservations.
    #[serde(default = "default_runtime_template_max_total_bytes")]
    pub max_total_bytes: u64,
    /// Maximum committed entries plus concurrent import reservations.
    #[serde(default = "default_runtime_template_max_entries")]
    pub max_entries: usize,
}

impl Default for TemplateSection {
    fn default() -> Self {
        Self {
            dir: default_template_dir(),
            import_root: None,
            max_files: default_runtime_template_max_files(),
            max_bytes: default_runtime_template_max_bytes(),
            max_metadata_bytes: default_runtime_template_max_metadata_bytes(),
            max_total_bytes: default_runtime_template_max_total_bytes(),
            max_entries: default_runtime_template_max_entries(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSection {
    #[serde(default = "default_prometheus_socket")]
    pub prometheus_socket: PathBuf,
}

impl Default for MetricsSection {
    fn default() -> Self {
        Self {
            prometheus_socket: default_prometheus_socket(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSection {
    /// Primary directory for vmlinux, rootfs base images, memfile bases.
    /// All runtime image files are looked up here by default.
    #[serde(default = "default_images_dir")]
    pub images_dir: PathBuf,

    /// Provider-owned runtime slots. This must not be the image directory.
    #[serde(default = "default_instances_dir")]
    pub instances_dir: PathBuf,

    /// Storage provider backend name (e.g. "file", "btrfs", "zfs").
    #[serde(default = "default_storage_provider")]
    pub provider: String,

    /// Legacy `storage.pool_size` input retained for an explicit validation error.
    ///
    /// Serialization omits the value, and validation fails whenever it is present.
    #[serde(default, skip_serializing)]
    pub pool_size: Option<usize>,

    /// Legacy `storage.prefork` input retained for an explicit validation error.
    ///
    /// Serialization omits the value, and validation fails whenever it is present.
    #[serde(default, skip_serializing)]
    pub prefork: Option<bool>,

    /// Interval for persisting already-written provider-owned artifacts.
    ///
    /// The literal `disabled` turns off periodic synchronization.
    #[serde(default = "default_sync_interval", alias = "flush_interval")]
    pub sync_interval: String,

    /// Maximum time the scheduler waits for one artifact synchronization attempt.
    #[serde(default = "default_sync_timeout")]
    pub sync_timeout: String,

    /// Logical size of file-provider root filesystem slots.
    #[serde(default = "default_rootfs_size")]
    pub rootfs_size: u64,

    /// Logical size of file-provider guest memory slots.
    #[serde(default = "default_mem_size")]
    pub mem_size: u64,
}

impl Default for StorageSection {
    fn default() -> Self {
        Self {
            images_dir: default_images_dir(),
            instances_dir: default_instances_dir(),
            provider: default_storage_provider(),
            pool_size: None,
            prefork: None,
            sync_interval: default_sync_interval(),
            sync_timeout: default_sync_timeout(),
            rootfs_size: default_rootfs_size(),
            mem_size: default_mem_size(),
        }
    }
}

/// Parsed periodic storage-artifact synchronization policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageSyncSchedule {
    /// Do not run periodic storage-artifact synchronization.
    Disabled,
    /// Run one sweep after every configured interval.
    Every(Duration),
}

impl StorageSection {
    /// Parse the periodic storage-artifact synchronization setting.
    pub fn sync_schedule(&self) -> Result<StorageSyncSchedule> {
        if self.sync_interval == "disabled" {
            return Ok(StorageSyncSchedule::Disabled);
        }
        let duration = parse_duration(&self.sync_interval)
            .ok_or_else(|| invalid_storage_duration("sync_interval", &self.sync_interval, true))?;
        validate_storage_clock_duration("sync_interval", &self.sync_interval, duration)?;
        Ok(StorageSyncSchedule::Every(duration))
    }

    /// Parse the maximum duration of one artifact synchronization attempt.
    pub fn sync_timeout_duration(&self) -> Result<Duration> {
        let duration = parse_duration(&self.sync_timeout)
            .ok_or_else(|| invalid_storage_duration("sync_timeout", &self.sync_timeout, false))?;
        validate_storage_clock_duration("sync_timeout", &self.sync_timeout, duration)?;
        Ok(duration)
    }
}

impl DaemonConfig {
    /// Load and parse a daemon configuration file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        let cfg: DaemonConfig = toml::from_str(&raw)?;
        cfg.validate()?;
        if cfg.pool.is_some() {
            tracing::warn!(
                path = %path.display(),
                "ignoring legacy packaged [pool] defaults; remove this section because reusable-instance management is unavailable"
            );
        }
        tracing::info!(path = %path.display(), "loaded blaze daemon config");
        Ok(cfg)
    }

    /// Validate cross-field invariants that serde cannot express.
    pub fn validate(&self) -> Result<()> {
        if let Some(pool) = &self.pool {
            if !is_legacy_packaged_pool_defaults(pool) {
                return Err(unsupported_pool_config("[pool]"));
            }
        }
        if self.storage.pool_size.is_some() {
            return Err(unsupported_pool_config("storage.pool_size"));
        }
        if self.storage.prefork.is_some() {
            return Err(unsupported_pool_config("storage.prefork"));
        }
        validate_storage_paths(&self.storage.images_dir, &self.storage.instances_dir)?;
        self.storage.sync_schedule()?;
        self.storage.sync_timeout_duration()?;
        let template_boundaries = [
            ("storage.images_dir", self.storage.images_dir.as_path()),
            (
                "storage.instances_dir",
                self.storage.instances_dir.as_path(),
            ),
            ("policy.dir", self.policy.dir.as_path()),
        ];
        validate_template_paths(
            &self.template.dir,
            self.template.import_root.as_deref(),
            &template_boundaries,
            &self.daemon.state_dir,
            &self.daemon.socket,
        )?;
        if self.template.max_files < 4 {
            return Err(BlazeError::ConfigError {
                source: ConfigErrorSource::InvalidValue(
                    "template.max_files must be at least 4 for the three required \
                     artifacts and generated template.json"
                        .to_string(),
                ),
            });
        }
        if self.template.max_bytes == 0 {
            return Err(BlazeError::ConfigError {
                source: ConfigErrorSource::InvalidValue(
                    "template.max_bytes must be greater than zero".to_string(),
                ),
            });
        }
        if self.template.max_metadata_bytes == 0
            || self.template.max_metadata_bytes > self.template.max_bytes
        {
            return Err(BlazeError::ConfigError {
                source: ConfigErrorSource::InvalidValue(
                    "template.max_metadata_bytes must be greater than zero and no \
                     larger than max_bytes"
                        .to_string(),
                ),
            });
        }
        if self.template.max_total_bytes == 0 {
            return Err(BlazeError::ConfigError {
                source: ConfigErrorSource::InvalidValue(
                    "template.max_total_bytes must be greater than zero".to_string(),
                ),
            });
        }
        if self.template.max_entries == 0 {
            return Err(BlazeError::ConfigError {
                source: ConfigErrorSource::InvalidValue(
                    "template.max_entries must be greater than zero".to_string(),
                ),
            });
        }
        Ok(())
    }
}

fn is_legacy_packaged_pool_defaults(value: &toml::Value) -> bool {
    let Some(table) = value.as_table() else {
        return false;
    };
    table.len() == 2
        && table.get("default_warm_ttl").and_then(toml::Value::as_str) == Some("30m")
        && table.get("gc_interval").and_then(toml::Value::as_str) == Some("5m")
}

fn unsupported_pool_config(field: &str) -> BlazeError {
    BlazeError::ConfigError {
        source: ConfigErrorSource::InvalidValue(format!(
            "{field} is not supported because warm pool management is not implemented"
        )),
    }
}

fn invalid_storage_duration(name: &str, value: &str, allow_disabled: bool) -> BlazeError {
    let expected = if allow_disabled {
        "a positive duration or \"disabled\""
    } else {
        "a positive duration"
    };
    BlazeError::ConfigError {
        source: ConfigErrorSource::InvalidValue(format!(
            "storage.{name} ({value:?}) must be {expected}"
        )),
    }
}

fn validate_storage_clock_duration(name: &str, value: &str, duration: Duration) -> Result<()> {
    if std::time::Instant::now().checked_add(duration).is_none() {
        return Err(BlazeError::ConfigError {
            source: ConfigErrorSource::InvalidValue(format!(
                "storage.{name} ({value:?}) exceeds the monotonic clock range"
            )),
        });
    }
    Ok(())
}

/// Reject storage roots whose ownership domains overlap.
pub fn validate_storage_paths(images_dir: &Path, instances_dir: &Path) -> Result<()> {
    if images_dir == instances_dir
        || images_dir.starts_with(instances_dir)
        || instances_dir.starts_with(images_dir)
    {
        return Err(BlazeError::ConfigError {
            source: ConfigErrorSource::InvalidValue(format!(
                "storage.images_dir ({}) and storage.instances_dir ({}) must be disjoint",
                images_dir.display(),
                instances_dir.display()
            )),
        });
    }
    Ok(())
}

fn validate_template_paths(
    dir: &Path,
    import_root: Option<&Path>,
    owned_roots: &[(&str, &Path)],
    state_dir: &Path,
    socket_path: &Path,
) -> Result<()> {
    validate_absolute_root(dir, "template.dir")?;
    if let Some(import_root) = import_root {
        validate_absolute_root(import_root, "template.import_root")?;
    }
    validate_state_boundary(dir, "template.dir", state_dir)?;
    if let Some(import_root) = import_root {
        validate_state_boundary(import_root, "template.import_root", state_dir)?;
    }
    if paths_overlap(dir, socket_path) {
        return Err(BlazeError::ConfigError {
            source: ConfigErrorSource::InvalidValue(format!(
                "template.dir ({}) and daemon.socket ({}) must be disjoint",
                dir.display(),
                socket_path.display()
            )),
        });
    }
    if let Some(import_root) = import_root
        && paths_overlap(import_root, socket_path)
    {
        return Err(BlazeError::ConfigError {
            source: ConfigErrorSource::InvalidValue(format!(
                "template.import_root ({}) and daemon.socket ({}) must be disjoint",
                import_root.display(),
                socket_path.display()
            )),
        });
    }

    for &(label, root) in owned_roots {
        if paths_overlap(dir, root) {
            return Err(BlazeError::ConfigError {
                source: ConfigErrorSource::InvalidValue(format!(
                    "template.dir ({}) and {label} ({}) must be disjoint",
                    dir.display(),
                    root.display()
                )),
            });
        }
    }

    if let Some(import_root) = import_root {
        if paths_overlap(dir, import_root) {
            return Err(BlazeError::ConfigError {
                source: ConfigErrorSource::InvalidValue(format!(
                    "template.dir ({}) and template.import_root ({}) must be disjoint",
                    dir.display(),
                    import_root.display()
                )),
            });
        }
        for &(label, root) in owned_roots {
            if paths_overlap(import_root, root) {
                return Err(BlazeError::ConfigError {
                    source: ConfigErrorSource::InvalidValue(format!(
                        "template.import_root ({}) and {label} ({}) must be disjoint",
                        import_root.display(),
                        root.display()
                    )),
                });
            }
        }
    }
    Ok(())
}

fn validate_absolute_root(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(BlazeError::ConfigError {
            source: ConfigErrorSource::InvalidValue(format!(
                "{label} ({}) must be an absolute path without parent components",
                path.display()
            )),
        });
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn validate_state_boundary(path: &Path, label: &str, state_dir: &Path) -> Result<()> {
    let enters_lifecycle_entry = path
        .strip_prefix(state_dir)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| match component {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .is_some_and(|name| Uuid::parse_str(name).is_ok());
    if path == state_dir || state_dir.starts_with(path) || enters_lifecycle_entry {
        return Err(BlazeError::ConfigError {
            source: ConfigErrorSource::InvalidValue(format!(
                "{label} ({}) must not own daemon.state_dir ({}) or a sandbox UUID subtree",
                path.display(),
                state_dir.display()
            )),
        });
    }
    Ok(())
}

// ----- defaults -----

fn default_log_level() -> String {
    "info".to_string()
}
fn default_state_dir() -> PathBuf {
    PathBuf::from("/var/lib/blaze")
}
fn default_socket() -> PathBuf {
    PathBuf::from("/run/blaze/api.sock")
}
fn default_policy_dir() -> PathBuf {
    PathBuf::from("/etc/anolisa/blaze/policies")
}
fn default_on_load_error() -> PolicyLoadErrorMode {
    PolicyLoadErrorMode::Fail
}
fn default_template_dir() -> PathBuf {
    PathBuf::from("/var/lib/blaze/templates")
}
fn default_runtime_template_max_files() -> usize {
    32
}
fn default_runtime_template_max_bytes() -> u64 {
    256 * 1024 * 1024 * 1024
}
fn default_runtime_template_max_metadata_bytes() -> u64 {
    1024 * 1024
}
fn default_runtime_template_max_total_bytes() -> u64 {
    1024 * 1024 * 1024 * 1024
}
fn default_runtime_template_max_entries() -> usize {
    128
}
fn default_prometheus_socket() -> PathBuf {
    PathBuf::from("/run/blaze/metrics.sock")
}
fn default_images_dir() -> PathBuf {
    PathBuf::from("/var/lib/blaze/images")
}
fn default_instances_dir() -> PathBuf {
    PathBuf::from("/var/lib/blaze/instances")
}
fn default_storage_provider() -> String {
    "file".to_string()
}
fn default_sync_interval() -> String {
    "disabled".to_string()
}
fn default_sync_timeout() -> String {
    "30s".to_string()
}
fn default_rootfs_size() -> u64 {
    8 * 1024 * 1024 * 1024
}
fn default_mem_size() -> u64 {
    4 * 1024 * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let cfg: DaemonConfig = toml::from_str("").expect("empty parses to defaults");
        assert_eq!(cfg.daemon.log_level, "info");
        assert_eq!(cfg.policy.on_load_error, PolicyLoadErrorMode::Fail);
        assert!(cfg.backends.is_empty());
        assert_ne!(cfg.storage.images_dir, cfg.storage.instances_dir);
        assert_eq!(
            cfg.storage.sync_schedule().expect("sync schedule"),
            StorageSyncSchedule::Disabled
        );
        assert!(cfg.template.import_root.is_none());
        assert_eq!(cfg.template.max_entries, 128);
    }

    #[test]
    fn parses_full_example() {
        let toml_str = r#"
            [daemon]
            log_level = "debug"
            state_dir = "/tmp/blaze"
            socket = "/tmp/blaze/api.sock"

            [backends]
            firecracker = "/usr/bin/firecracker"
            linux-sandbox = "/usr/bin/anolisa-linux-sandbox"

            [policy]
            dir = "/etc/anolisa/blaze/policies"
            on_load_error = "warn"
        "#;
        let cfg: DaemonConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.daemon.log_level, "debug");
        assert_eq!(cfg.policy.on_load_error, PolicyLoadErrorMode::Warn);
        assert_eq!(cfg.backends.len(), 2);
    }

    #[test]
    fn rejects_unsupported_pool_configuration() {
        for input in [
            "[pool]\n",
            "[pool]\ndefault_warm_ttl = \"30m\"\n",
            "[pool]\ngc_interval = \"5m\"\n",
            "[pool]\ndefault_warm_ttl = \"31m\"\ngc_interval = \"5m\"\n",
            "[pool]\ndefault_warm_ttl = \"30m\"\ngc_interval = \"6m\"\n",
            "[pool]\ndefault_warm_ttl = 30\ngc_interval = \"5m\"\n",
            "[pool]\ndefault_warm_ttl = \"30m\"\ngc_interval = \"5m\"\nextra = true\n",
            "[storage]\npool_size = 0\n",
            "[storage]\nprefork = false\n",
        ] {
            let cfg: DaemonConfig = toml::from_str(input).expect("compatibility parse");
            let error = cfg.validate().expect_err("unsupported pool setting");
            assert!(
                error
                    .to_string()
                    .contains("warm pool management is not implemented"),
                "{error}"
            );
        }
    }

    #[test]
    fn accepts_only_the_legacy_packaged_pool_defaults_without_serializing_them() {
        let cfg: DaemonConfig =
            toml::from_str("[pool]\ndefault_warm_ttl = \"30m\"\ngc_interval = \"5m\"\n")
                .expect("legacy packaged configuration parses");

        cfg.validate()
            .expect("legacy packaged defaults remain upgrade-compatible");
        let serialized = toml::to_string(&cfg).expect("serialize configuration");
        assert!(!serialized.contains("[pool]"));
        assert!(!serialized.contains("default_warm_ttl"));
        assert!(!serialized.contains("gc_interval"));
    }

    #[test]
    fn load_accepts_a_preserved_packaged_pool_section() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        std::fs::write(
            &path,
            "[daemon]\nlog_level = \"debug\"\n\n[pool]\ndefault_warm_ttl = \"30m\"\ngc_interval = \"5m\"\n",
        )
        .expect("write legacy packaged configuration");

        let cfg = DaemonConfig::load(&path).expect("load preserved packaged configuration");

        assert!(cfg.pool.is_some());
        assert_eq!(cfg.daemon.log_level, "debug");
    }

    #[test]
    fn rejects_equal_or_nested_storage_roots() {
        for (images, instances) in [
            ("/var/lib/blaze/data", "/var/lib/blaze/data"),
            ("/var/lib/blaze/data", "/var/lib/blaze/data/instances"),
            ("/var/lib/blaze/images/base", "/var/lib/blaze/images"),
        ] {
            let mut cfg = DaemonConfig::default();
            cfg.storage.images_dir = PathBuf::from(images);
            cfg.storage.instances_dir = PathBuf::from(instances);
            let error = cfg.validate().expect_err("overlapping paths");
            assert!(error.to_string().contains("must be disjoint"));
        }
    }
    #[test]
    fn storage_sync_schedule_accepts_disabled_or_positive_duration() {
        let mut cfg = DaemonConfig::default();
        cfg.storage.sync_interval = "disabled".into();
        cfg.validate().expect("disabled schedule");
        assert_eq!(
            cfg.storage.sync_schedule().expect("schedule"),
            StorageSyncSchedule::Disabled
        );

        cfg.storage.sync_interval = "15s".into();
        cfg.validate().expect("positive schedule");
        assert_eq!(
            cfg.storage.sync_schedule().expect("schedule"),
            StorageSyncSchedule::Every(Duration::from_secs(15))
        );
    }

    #[test]
    fn storage_sync_interval_accepts_legacy_input_alias() {
        let cfg: DaemonConfig = toml::from_str(
            r#"
                [storage]
                flush_interval = "15s"
            "#,
        )
        .expect("legacy interval key parses");

        assert_eq!(cfg.storage.sync_interval, "15s");
        assert_eq!(
            cfg.storage.sync_schedule().expect("schedule"),
            StorageSyncSchedule::Every(Duration::from_secs(15))
        );
    }

    #[test]
    fn storage_sync_interval_rejects_duplicate_aliases() {
        let error = toml::from_str::<DaemonConfig>(
            r#"
                [storage]
                flush_interval = "15s"
                sync_interval = "30s"
            "#,
        )
        .expect_err("legacy and canonical interval keys must not coexist");

        assert!(error.to_string().contains("duplicate field"), "{error}");
        assert!(error.to_string().contains("sync_interval"), "{error}");
    }

    #[test]
    fn storage_sync_schedule_rejects_invalid_values() {
        for interval in ["0s", "not-a-duration"] {
            let mut cfg = DaemonConfig::default();
            cfg.storage.sync_interval = interval.into();
            let error = cfg.validate().expect_err("invalid sync interval");
            assert!(
                error.to_string().contains("storage.sync_interval"),
                "{error}"
            );
        }

        for timeout in ["0s", "disabled", "not-a-duration"] {
            let mut cfg = DaemonConfig::default();
            cfg.storage.sync_timeout = timeout.into();
            let error = cfg.validate().expect_err("invalid sync timeout");
            assert!(
                error.to_string().contains("storage.sync_timeout"),
                "{error}"
            );
        }

        for (field, value) in [
            ("sync_interval", "18446744073709551615s"),
            ("sync_timeout", "18446744073709551615s"),
        ] {
            let mut cfg = DaemonConfig::default();
            if field == "sync_interval" {
                cfg.storage.sync_interval = value.into();
            } else {
                cfg.storage.sync_timeout = value.into();
            }
            let error = cfg.validate().expect_err("clock range overflow");
            assert!(error.to_string().contains(field), "{error}");
            assert!(
                error.to_string().contains("monotonic clock range"),
                "{error}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_runtime_template_boundaries() {
        let mut relative = DaemonConfig::default();
        relative.template.dir = PathBuf::from("templates");
        assert!(relative.validate().is_err());

        let mut parent = DaemonConfig::default();
        parent.template.dir = PathBuf::from("/var/lib/blaze/../templates");
        assert!(parent.validate().is_err());

        let mut overlapping = DaemonConfig::default();
        overlapping.template.import_root = Some(PathBuf::from("/var/lib/blaze/templates/imports"));
        assert!(overlapping.validate().is_err());

        for owned_root in [
            "/var/lib/blaze/images/catalog",
            "/var/lib/blaze/instances/catalog",
        ] {
            let mut config = DaemonConfig::default();
            config.template.dir = PathBuf::from(owned_root);
            assert!(config.validate().is_err());
        }

        let mut source_overlap = DaemonConfig::default();
        source_overlap.template.import_root = Some(PathBuf::from("/var/lib/blaze/images/imports"));
        assert!(source_overlap.validate().is_err());

        for policy_dir in [
            "/var/lib/blaze/templates",
            "/var/lib/blaze/templates/policies",
            "/var/lib/blaze",
        ] {
            let mut policy_overlap = DaemonConfig::default();
            policy_overlap.policy.dir = PathBuf::from(policy_dir);
            let error = policy_overlap
                .validate()
                .expect_err("catalog and policy roots must be disjoint");
            assert!(error.to_string().contains("policy.dir"));
        }

        for policy_dir in [
            "/srv/runtime-template-imports",
            "/srv/runtime-template-imports/policies",
            "/srv",
        ] {
            let mut policy_overlap = DaemonConfig::default();
            policy_overlap.template.import_root =
                Some(PathBuf::from("/srv/runtime-template-imports"));
            policy_overlap.policy.dir = PathBuf::from(policy_dir);
            let error = policy_overlap
                .validate()
                .expect_err("import and policy roots must be disjoint");
            assert!(error.to_string().contains("policy.dir"));
        }

        let mut unbounded = DaemonConfig::default();
        unbounded.template.max_files = 3;
        assert!(unbounded.validate().is_err());

        let mut state_owner = DaemonConfig::default();
        state_owner.storage.images_dir = PathBuf::from("/srv/blaze/images");
        state_owner.storage.instances_dir = PathBuf::from("/srv/blaze/instances");
        state_owner.template.dir = state_owner.daemon.state_dir.clone();
        assert!(state_owner.validate().is_err());

        let mut sandbox_owner = DaemonConfig::default();
        sandbox_owner.template.dir = sandbox_owner
            .daemon
            .state_dir
            .join("86b59faf-3b91-46e4-9db0-2468b8336eb6")
            .join("catalog");
        assert!(sandbox_owner.validate().is_err());

        let mut entries = DaemonConfig::default();
        entries.template.max_entries = 0;
        assert!(entries.validate().is_err());

        for catalog in [
            "/run/blaze",
            "/run/blaze/api.sock",
            "/run/blaze/api.sock/catalog",
        ] {
            let mut socket_overlap = DaemonConfig::default();
            socket_overlap.template.dir = PathBuf::from(catalog);
            let error = socket_overlap
                .validate()
                .expect_err("catalog and daemon socket must be disjoint");
            assert!(error.to_string().contains("daemon.socket"));
        }

        for import_root in [
            "/run/blaze",
            "/run/blaze/api.sock",
            "/run/blaze/api.sock/imports",
        ] {
            let mut socket_overlap = DaemonConfig::default();
            socket_overlap.template.import_root = Some(PathBuf::from(import_root));
            let error = socket_overlap
                .validate()
                .expect_err("import root and daemon socket must be disjoint");
            assert!(error.to_string().contains("daemon.socket"));
        }

        let mut metadata = DaemonConfig::default();
        metadata.template.max_metadata_bytes = metadata.template.max_bytes + 1;
        assert!(metadata.validate().is_err());

        let mut total = DaemonConfig::default();
        total.template.max_total_bytes = 0;
        assert!(total.validate().is_err());
    }
}
