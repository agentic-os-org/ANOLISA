//! Environment detection for SkillFS compiler.
//!
//! Detects OS, available commands, and selected environment variables to enable
//! environment-aware skill compilation.

use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// OsKind
// ---------------------------------------------------------------------------

/// The operating system kind, used in `@if os == ...` expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsKind {
    Darwin,
    Linux,
    Windows,
    Unknown(String),
}

impl OsKind {
    /// Detect the current operating system.
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => OsKind::Darwin,
            "linux" => OsKind::Linux,
            "windows" => OsKind::Windows,
            other => OsKind::Unknown(other.to_string()),
        }
    }

    /// Return the canonical string representation used in skill conditions.
    pub fn as_str(&self) -> &str {
        match self {
            OsKind::Darwin => "darwin",
            OsKind::Linux => "linux",
            OsKind::Windows => "windows",
            OsKind::Unknown(s) => s.as_str(),
        }
    }
}

// ---------------------------------------------------------------------------
// EnvironmentProfile
// ---------------------------------------------------------------------------

/// Commands to probe for availability.
const COMMAND_WHITELIST: &[&str] = &[
    "uv",
    "pip",
    "pip3",
    "python",
    "python3",
    "npm",
    "yarn",
    "pnpm",
    "node",
    "docker",
    "docker-compose",
    "kubectl",
    "cargo",
    "rustc",
    "go",
    "git",
    "sh",
    "bash",
    "zsh",
    "fish",
    "jq",
    "curl",
    "wget",
];

/// Environment variables to capture (selective whitelist).
const ENV_VAR_WHITELIST: &[&str] = &[
    "HOME",
    "PATH",
    "SHELL",
    "VIRTUAL_ENV",
    "CONDA_DEFAULT_ENV",
    "NODE_ENV",
    "GOPATH",
    "CARGO_HOME",
    "UV_CACHE_DIR",
    "PYENV_ROOT",
];

/// Complete runtime environment snapshot used for skill compilation and filtering.
#[derive(Debug, Clone)]
pub struct EnvironmentProfile {
    pub os: OsKind,
    /// Commands available on PATH (from `COMMAND_WHITELIST`).
    pub available_commands: HashSet<String>,
    /// Selected environment variables (from `ENV_VAR_WHITELIST`).
    pub env_vars: HashMap<String, String>,
}

impl EnvironmentProfile {
    pub fn detect() -> Self {
        let os = OsKind::current();
        let available_commands = probe_commands();
        let env_vars = capture_env_vars();
        Self {
            os,
            available_commands,
            env_vars,
        }
    }

    /// Returns `true` if `cmd` was found on PATH.
    pub fn has_command(&self, cmd: &str) -> bool {
        self.available_commands.contains(cmd)
    }

    /// Returns `true` if environment variable `var` is set.
    pub fn has_env(&self, var: &str) -> bool {
        self.env_vars.contains_key(var)
    }
}

// ---------------------------------------------------------------------------
// Probe helpers
// ---------------------------------------------------------------------------

fn probe_commands() -> HashSet<String> {
    let mut found = HashSet::new();
    for &cmd in COMMAND_WHITELIST {
        if command_exists(cmd) {
            found.insert(cmd.to_string());
        }
    }
    found
}

fn command_exists(cmd: &str) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("which")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("where")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn capture_env_vars() -> HashMap<String, String> {
    ENV_VAR_WHITELIST
        .iter()
        .filter_map(|&key| std::env::var(key).ok().map(|val| (key.to_string(), val)))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_os_kind_current_is_valid() {
        let os = OsKind::current();
        let s = os.as_str();
        assert!(!s.is_empty());
    }

    #[test]
    fn test_os_kind_as_str() {
        assert_eq!(OsKind::Darwin.as_str(), "darwin");
        assert_eq!(OsKind::Linux.as_str(), "linux");
        assert_eq!(OsKind::Windows.as_str(), "windows");
        assert_eq!(OsKind::Unknown("freebsd".to_string()).as_str(), "freebsd");
    }

    #[test]
    fn test_has_command_nonexistent() {
        let profile = EnvironmentProfile::detect();
        assert!(!profile.has_command("this-command-does-not-exist-99999"));
    }

    #[test]
    #[cfg(unix)]
    fn test_has_command_sh_exists() {
        let profile = EnvironmentProfile::detect();
        assert!(
            profile.has_command("sh"),
            "sh should always be available on Unix"
        );
    }
}
