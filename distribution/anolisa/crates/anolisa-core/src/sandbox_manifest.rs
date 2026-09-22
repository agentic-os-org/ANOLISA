//! TOML-driven sandbox manifest loader.
//!
//! Replaces the hardcoded scenario→package mappings previously scattered across
//! `sandbox_install.rs` and `osbase_install.rs`.  The canonical source of truth
//! is now `sandbox.toml` — a user-editable TOML file deployed by `system setup`
//! to `/etc/anolisa/sandbox.toml`.
//!
//! Load priority (highest wins):
//! 1. `/etc/anolisa/sandbox.toml`  (system-deployed, editable by admin)
//! 2. `$XDG_CONFIG_HOME/anolisa/sandbox.toml` (user override, dev machines)
//! 3. Compiled-in fallback via `include_str!` (always available)

use std::path::{Path, PathBuf};

use serde::Deserialize;

// ─── Public types ────────────────────────────────────────────────────────────

/// A single sandbox scenario parsed from `[[scenario]]` in sandbox.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioConfig {
    /// Scenario identifier (e.g. `"gvisor"`, `"firecracker"`, `"landlock"`).
    pub name: String,
    /// Required packages to install via dnf/yum.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Optional packages — hinted to user but not auto-installed.
    #[serde(default)]
    pub packages_optional: Vec<String>,
    /// Services to enable+start after package install (systemctl enable --now).
    #[serde(default)]
    pub services: Vec<String>,
    /// Post-install verify commands (e.g. `["runc --version", "docker info"]`).
    /// Each entry is a shell-style command string split on whitespace.
    /// If empty, the verify phase falls back to `systemctl is-active` for each service.
    #[serde(default)]
    pub verify_commands: Vec<String>,
    /// Whether KVM (`/dev/kvm`) is required.
    #[serde(default)]
    pub requires_kvm: bool,
    /// Minimum kernel version (e.g. `">=5.10"`).
    #[serde(default = "default_kernel_requirement")]
    pub requires_kernel: String,
}

fn default_kernel_requirement() -> String {
    ">=4.0".to_string()
}

/// Top-level sandbox manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct SandboxManifest {
    /// Schema version for forward-compatibility gating.
    #[serde(default = "default_manifest_version")]
    pub manifest_version: u32,
    /// All registered scenarios.
    #[serde(rename = "scenario")]
    pub scenarios: Vec<ScenarioConfig>,
}

fn default_manifest_version() -> u32 {
    1
}

/// Errors that can occur when loading the sandbox manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to read manifest at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse manifest: {0}")]
    Parse(String),
    #[error("no manifest found in any search path")]
    NotFound,
}

// ─── Compiled-in fallback ────────────────────────────────────────────────────

/// The default manifest shipped with the binary.  Acts as a fallback when
/// no deployed sandbox.toml is found on disk.
const BUILTIN_MANIFEST: &str = include_str!("../../../manifests/sandbox.toml");

// ─── Load logic ──────────────────────────────────────────────────────────────

impl SandboxManifest {
    /// Load the sandbox manifest using the standard priority chain:
    /// 1. `/etc/anolisa/sandbox.toml`
    /// 2. `$XDG_CONFIG_HOME/anolisa/sandbox.toml`
    /// 3. Built-in fallback
    pub fn load() -> Result<Self, ManifestError> {
        Self::load_with_search_paths(&Self::default_search_paths())
    }

    /// Load from explicit search paths (for testing / custom prefix).
    pub fn load_with_search_paths(paths: &[PathBuf]) -> Result<Self, ManifestError> {
        for path in paths {
            if path.is_file() {
                let content = std::fs::read_to_string(path).map_err(|e| ManifestError::Io {
                    path: path.clone(),
                    source: e,
                })?;
                return Self::parse(&content);
            }
        }
        // Fallback to built-in
        Self::parse(BUILTIN_MANIFEST)
    }

    /// Parse from TOML string.
    pub fn parse(content: &str) -> Result<Self, ManifestError> {
        toml::from_str(content).map_err(|e| ManifestError::Parse(e.to_string()))
    }

    /// Find a scenario by name (case-sensitive).
    pub fn find_scenario(&self, name: &str) -> Option<&ScenarioConfig> {
        self.scenarios.iter().find(|s| s.name == name)
    }

    /// Return all available scenario names.
    pub fn scenario_names(&self) -> Vec<&str> {
        self.scenarios.iter().map(|s| s.name.as_str()).collect()
    }

    /// Default search paths in priority order.
    fn default_search_paths() -> Vec<PathBuf> {
        let mut paths = Vec::with_capacity(3);

        // 1. System path
        paths.push(PathBuf::from("/etc/anolisa/sandbox.toml"));

        // 2. XDG user config
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            paths.push(Path::new(&xdg).join("anolisa/sandbox.toml"));
        } else if let Ok(home) = std::env::var("HOME") {
            paths.push(Path::new(&home).join(".config/anolisa/sandbox.toml"));
        }

        paths
    }
}

impl ScenarioConfig {
    /// Parse the `requires_kernel` field (e.g. `">=5.10"`) and compare
    /// against the running kernel version.  Returns `Ok(())` if the
    /// requirement is satisfied, `Err(reason)` otherwise.
    pub fn check_kernel(&self, running_kernel: Option<&str>) -> Result<(), String> {
        let requirement = &self.requires_kernel;
        if requirement.is_empty() || requirement == ">=0" || requirement == ">=4.0" {
            return Ok(());
        }

        let running = match running_kernel {
            Some(k) => k,
            None => return Err("cannot determine running kernel version".to_string()),
        };

        // Parse requirement: ">=X.Y" or ">=X.Y.Z"
        let min_version = requirement
            .trim_start_matches(">=")
            .trim_start_matches('>')
            .trim();

        if compare_kernel_versions(running, min_version) {
            Ok(())
        } else {
            Err(format!(
                "kernel {running} does not satisfy requirement {requirement}"
            ))
        }
    }
}

/// Simple kernel version comparison: returns true if `running >= required`.
/// Handles formats like "6.6.30-xxxx" vs "5.10".
fn compare_kernel_versions(running: &str, required: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .take(3)
            .filter_map(|s| s.parse::<u32>().ok())
            .collect()
    };
    let r = parse(running);
    let q = parse(required);
    for (i, &qv) in q.iter().enumerate() {
        let rv = r.get(i).copied().unwrap_or(0);
        if rv > qv {
            return true;
        }
        if rv < qv {
            return false;
        }
    }
    true // equal
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_builtin_manifest() {
        let m = SandboxManifest::parse(BUILTIN_MANIFEST).expect("builtin must parse");
        assert_eq!(m.manifest_version, 2);
        assert_eq!(m.scenarios.len(), 5);
        assert!(m.find_scenario("gvisor").is_some());
        assert!(m.find_scenario("firecracker").is_some());
        assert!(m.find_scenario("landlock").is_some());
        assert!(m.find_scenario("nonexistent").is_none());
    }

    #[test]
    fn scenario_names_returns_all() {
        let m = SandboxManifest::parse(BUILTIN_MANIFEST).unwrap();
        let names = m.scenario_names();
        assert!(names.contains(&"runc"));
        assert!(names.contains(&"rund"));
        assert!(names.contains(&"firecracker"));
        assert!(names.contains(&"gvisor"));
        assert!(names.contains(&"landlock"));
    }

    #[test]
    fn runc_packages_and_services() {
        let m = SandboxManifest::parse(BUILTIN_MANIFEST).unwrap();
        let s = m.find_scenario("runc").unwrap();
        assert_eq!(
            s.packages,
            vec!["runc", "containerd", "docker", "docker-client"]
        );
        assert_eq!(s.packages_optional, vec!["nerdctl"]);
        assert_eq!(s.services, vec!["containerd", "docker"]);
        assert!(!s.requires_kvm);
    }

    #[test]
    fn gvisor_packages() {
        let m = SandboxManifest::parse(BUILTIN_MANIFEST).unwrap();
        let s = m.find_scenario("gvisor").unwrap();
        assert_eq!(s.packages, vec!["gvisor"]);
        assert!(!s.requires_kvm);
    }

    #[test]
    fn firecracker_packages_and_optional() {
        let m = SandboxManifest::parse(BUILTIN_MANIFEST).unwrap();
        let s = m.find_scenario("firecracker").unwrap();
        assert_eq!(s.packages, vec!["firecracker", "firecracker-jailer"]);
        assert_eq!(
            s.packages_optional,
            vec!["firecracker-kernel", "firecracker-rootfs"]
        );
        assert!(s.requires_kvm);
    }

    #[test]
    fn kernel_version_check() {
        let s = ScenarioConfig {
            name: "test".to_string(),
            packages: vec![],
            packages_optional: vec![],
            services: vec![],
            verify_commands: vec![],
            requires_kvm: false,
            requires_kernel: ">=5.10".to_string(),
        };
        assert!(s.check_kernel(Some("6.6.30-custom")).is_ok());
        assert!(s.check_kernel(Some("5.10.0")).is_ok());
        assert!(s.check_kernel(Some("5.9.99")).is_err());
        assert!(s.check_kernel(Some("4.19.0")).is_err());
    }

    #[test]
    fn kernel_version_compare() {
        assert!(compare_kernel_versions("6.6.30", "5.10"));
        assert!(compare_kernel_versions("5.10.0", "5.10"));
        assert!(compare_kernel_versions("5.13.0", "5.13"));
        assert!(!compare_kernel_versions("5.9.99", "5.10"));
        assert!(!compare_kernel_versions("4.4.0", "5.0"));
    }

    #[test]
    fn load_fallback_to_builtin() {
        // With empty search paths, should fallback to builtin
        let m = SandboxManifest::load_with_search_paths(&[]).expect("should fallback to builtin");
        assert_eq!(m.scenarios.len(), 5);
    }
}
