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
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let Some(path) = std::env::var_os("PATH").or_else(default_search_path) else {
            return false;
        };
        // Probe the executable itself: a restricted PATH need not contain
        // `which`. Do not run any of the commands while detecting them.
        std::env::split_paths(&path).any(|directory| {
            let candidate = directory.join(cmd);
            if !candidate.is_file() {
                return false;
            }
            let Ok(candidate) = CString::new(candidate.as_os_str().as_bytes()) else {
                return false;
            };
            // SAFETY: candidate owns a NUL-terminated path for the call.
            // AT_EACCESS checks the effective identity used to execute it.
            unsafe {
                libc::faccessat(
                    libc::AT_FDCWD,
                    candidate.as_ptr(),
                    libc::X_OK,
                    libc::AT_EACCESS,
                ) == 0
            }
        })
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

// Match the platform's default executable search path when PATH is absent.
#[cfg(unix)]
fn default_search_path() -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;

    // SAFETY: a null buffer with zero length queries the required capacity.
    let size = unsafe { libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0) };
    if size == 0 {
        return None;
    }
    let mut path = vec![0u8; size];
    // SAFETY: path has size writable bytes; confstr writes a terminating NUL.
    let written = unsafe { libc::confstr(libc::_CS_PATH, path.as_mut_ptr().cast(), size) };
    if written == 0 || written > size {
        return None;
    }
    path.truncate(written - 1);
    Some(std::ffi::OsString::from_vec(path))
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
