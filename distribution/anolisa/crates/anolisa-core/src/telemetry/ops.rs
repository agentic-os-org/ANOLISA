//! Ops channel on-disk layout.
//!
//! Owns the filesystem side of the ops telemetry channel:
//! - Create `/var/log/anolisa/sls/ops/` directory
//! - Pre-create component `.jsonl` files
//! - Configure / remove logrotate for the ops `.jsonl` files
//!
//! Deliberately pure filesystem: instance-snapshot writing lives in
//! [`crate::telemetry::instance`] and region probing in
//! [`crate::telemetry::common`], so this module has no metadata dependencies.

use std::fs;

use crate::telemetry::{TelemetryConfig, TelemetryError};

/// Component `.jsonl` files to pre-create in the ops directory.
const OPS_LOG_FILES: &[&str] = &[
    "instance",
    "agentsight",
    "agent-sec-core",
    "cosh",
    "tokenless",
    "ws-ckpt",
    "skillfs",
];

/// Provisions the ops directory layout: the directory itself, the pre-created
/// component `.jsonl` files, and the logrotate policy.
pub struct OpsLayout<'a> {
    config: &'a TelemetryConfig,
}

impl<'a> OpsLayout<'a> {
    pub fn new(config: &'a TelemetryConfig) -> Self {
        Self { config }
    }

    /// Create `/var/log/anolisa/sls/ops/` with mode 755.
    pub fn create_ops_dir(&self) -> Result<(), TelemetryError> {
        let ops_dir = &self.config.ops_dir;
        if !ops_dir.exists() {
            fs::create_dir_all(ops_dir)?;

            #[cfg(target_os = "linux")]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(ops_dir, fs::Permissions::from_mode(0o755))?;
            }
        }

        Ok(())
    }

    /// Pre-create empty `.jsonl` files with mode 666 in the ops directory.
    pub fn create_ops_jsonl_files(&self) -> Result<(), TelemetryError> {
        let ops_dir = &self.config.ops_dir;
        for name in OPS_LOG_FILES {
            let file_path = ops_dir.join(format!("{name}.jsonl"));
            if !file_path.exists() {
                fs::write(&file_path, "")?;

                #[cfg(target_os = "linux")]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o666))?;
                }
            }
        }
        Ok(())
    }

    /// Write logrotate config for ops `.jsonl` files.
    ///
    /// Creates `/etc/logrotate.d/anolisa` with a `size 30M / rotate 1` policy
    /// using rename-mode rotation so the uploader's inode offsets stay valid
    /// across a rotation (see [`crate::telemetry::uploader`]).
    pub fn setup_logrotate(&self) -> Result<(), TelemetryError> {
        let config_path = &self.config.logrotate_config_path;
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let glob = self.config.ops_dir.join("*.jsonl");
        let content = format!(
            "{glob} {{\n    size 30M\n    rotate 1\n    missingok\n    notifempty\n    create 0666 root root\n}}\n",
            glob = glob.display()
        );
        fs::write(config_path, content)?;

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(config_path, fs::Permissions::from_mode(0o644))?;
        }

        Ok(())
    }

    /// Remove logrotate config for ops `.jsonl` files.
    pub fn remove_logrotate(&self) -> Result<(), TelemetryError> {
        let config_path = &self.config.logrotate_config_path;
        if config_path.exists() {
            fs::remove_file(config_path)?;
        }
        Ok(())
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::test_config;
    use tempfile::TempDir;

    #[test]
    fn test_create_ops_dir() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        let ops = OpsLayout::new(&cfg);
        ops.create_ops_dir().unwrap();

        let ops_dir = dir.path().join("ops");
        assert!(ops_dir.exists());
        assert!(ops_dir.is_dir());
    }

    #[test]
    fn test_create_ops_jsonl_files() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        let ops = OpsLayout::new(&cfg);
        ops.create_ops_dir().unwrap();
        ops.create_ops_jsonl_files().unwrap();

        let ops_dir = dir.path().join("ops");
        for name in OPS_LOG_FILES {
            let file_path = ops_dir.join(format!("{name}.jsonl"));
            assert!(file_path.exists(), "expected {name}.jsonl to exist");
        }
    }

    #[test]
    fn test_ops_jsonl_files_idempotent() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        let ops = OpsLayout::new(&cfg);
        ops.create_ops_dir().unwrap();
        ops.create_ops_jsonl_files().unwrap();
        ops.create_ops_jsonl_files().unwrap(); // second call should not fail

        let ops_dir = dir.path().join("ops");
        let instance_file = ops_dir.join("instance.jsonl");
        assert!(instance_file.exists());
    }

    #[test]
    fn test_setup_logrotate_creates_config() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        let ops = OpsLayout::new(&cfg);
        ops.setup_logrotate().unwrap();

        let content = fs::read_to_string(&cfg.logrotate_config_path).unwrap();
        assert!(content.contains("size 30M"));
        assert!(content.contains("rotate 1"));
        assert!(content.contains("create 0666 root root"));
        assert!(content.contains("*.jsonl"));
    }

    #[test]
    fn test_setup_logrotate_idempotent() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        let ops = OpsLayout::new(&cfg);
        ops.setup_logrotate().unwrap();
        ops.setup_logrotate().unwrap();

        let content = fs::read_to_string(&cfg.logrotate_config_path).unwrap();
        assert_eq!(content.matches("size 30M").count(), 1);
    }

    #[test]
    fn test_remove_logrotate_noop_when_absent() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        let ops = OpsLayout::new(&cfg);
        assert!(ops.remove_logrotate().is_ok());
    }

    #[test]
    fn test_remove_logrotate_deletes_config() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        let ops = OpsLayout::new(&cfg);
        ops.setup_logrotate().unwrap();
        assert!(cfg.logrotate_config_path.exists());

        ops.remove_logrotate().unwrap();
        assert!(!cfg.logrotate_config_path.exists());
    }
}
