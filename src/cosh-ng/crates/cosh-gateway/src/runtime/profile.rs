//! Allowlisted launch profiles for locally installed ACP v1 adapters.

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::{
    AcpV1AdapterProfile, AcpV1BridgeError, AcpV1ClientConfig, AcpV1RuntimeBridge, PinnedDirectory,
    PinnedExecutable, RuntimeLaunchSpec,
};

const COMMON_ENVIRONMENT: &[&str] = &[
    "HOME",
    "PATH",
    "TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
];
const PROXY_ENVIRONMENT: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];
const CODEX_ENVIRONMENT: &[&str] = &[
    "CODEX_API_KEY",
    "CODEX_HOME",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_ORGANIZATION",
    "OPENAI_PROJECT",
];
const CLAUDE_ENVIRONMENT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CONFIG_DIR",
];

/// Stable identity of an ACP adapter supported by the first COSH profile set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AcpRuntimeProfileId {
    /// Official ACP adapter backed by the Codex app server.
    Codex,
    /// Official ACP adapter backed by the Claude Agent SDK.
    ClaudeCode,
}

/// Immutable metadata for one built-in ACP adapter launch profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpRuntimeProfile {
    id: AcpRuntimeProfileId,
    display_name: &'static str,
    executable_name: &'static str,
    arguments: &'static [&'static str],
    provider_environment: &'static [&'static str],
}

const CODEX_PROFILE: AcpRuntimeProfile = AcpRuntimeProfile {
    id: AcpRuntimeProfileId::Codex,
    display_name: "Codex ACP adapter",
    executable_name: "codex-acp",
    arguments: &[],
    provider_environment: CODEX_ENVIRONMENT,
};
const CLAUDE_PROFILE: AcpRuntimeProfile = AcpRuntimeProfile {
    id: AcpRuntimeProfileId::ClaudeCode,
    display_name: "Claude Agent ACP adapter",
    executable_name: "claude-agent-acp",
    arguments: &[],
    provider_environment: CLAUDE_ENVIRONMENT,
};
const BUILT_IN_PROFILES: &[AcpRuntimeProfile] = &[CODEX_PROFILE, CLAUDE_PROFILE];

/// Returns the complete, fixed Phase-1 ACP runtime profile set.
#[must_use]
pub fn built_in_acp_runtime_profiles() -> &'static [AcpRuntimeProfile] {
    BUILT_IN_PROFILES
}

impl AcpRuntimeProfileId {
    /// Returns the immutable launch policy associated with this identity.
    #[must_use]
    pub fn profile(self) -> &'static AcpRuntimeProfile {
        match self {
            Self::Codex => &CODEX_PROFILE,
            Self::ClaudeCode => &CLAUDE_PROFILE,
        }
    }
}

impl AcpRuntimeProfile {
    /// Returns the stable profile identity.
    #[must_use]
    pub fn id(self) -> AcpRuntimeProfileId {
        self.id
    }

    /// Returns the human-readable adapter name.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        self.display_name
    }

    /// Returns the only accepted executable basename for this profile.
    #[must_use]
    pub fn executable_name(self) -> &'static str {
        self.executable_name
    }

    /// Returns fixed adapter arguments. Prompts can never add process arguments.
    #[must_use]
    pub fn arguments(self) -> &'static [&'static str] {
        self.arguments
    }

    /// Returns names that may cross the cleared-environment boundary.
    pub fn allowed_environment_names(self) -> impl Iterator<Item = &'static str> {
        COMMON_ENVIRONMENT
            .iter()
            .chain(PROXY_ENVIRONMENT.iter())
            .chain(self.provider_environment.iter())
            .copied()
    }
}

/// Inputs used to resolve one allowlisted local adapter process.
#[derive(Clone)]
pub struct AcpRuntimeProfileRequest {
    /// Selected built-in adapter profile.
    pub profile: AcpRuntimeProfileId,
    /// Optional trusted local adapter path. The basename must match the profile.
    ///
    /// npm-style symlinks are accepted and pinned to their canonical target.
    pub executable: Option<PathBuf>,
    /// Workspace to canonicalize and bind as the child working directory.
    pub workspace: PathBuf,
    /// Source environment filtered through the profile allowlist.
    pub environment: BTreeMap<OsString, OsString>,
}

impl AcpRuntimeProfileRequest {
    /// Captures the current process environment for later allowlist filtering.
    #[must_use]
    pub fn from_current_environment(
        profile: AcpRuntimeProfileId,
        executable: Option<PathBuf>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            profile,
            executable,
            workspace: workspace.into(),
            environment: env::vars_os().collect(),
        }
    }
}

impl fmt::Debug for AcpRuntimeProfileRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpRuntimeProfileRequest")
            .field("profile", &self.profile)
            .field("executable", &self.executable)
            .field("workspace", &self.workspace)
            .field("environment_names", &self.environment.keys())
            .finish()
    }
}

/// Fully pinned adapter launch resolved from one built-in profile.
pub struct ResolvedAcpRuntimeProfile {
    profile: AcpRuntimeProfileId,
    executable: PinnedExecutable,
    workspace: PinnedDirectory,
    environment: BTreeMap<OsString, OsString>,
}

impl ResolvedAcpRuntimeProfile {
    /// Returns the selected built-in profile identity.
    #[must_use]
    pub fn profile(&self) -> AcpRuntimeProfileId {
        self.profile
    }

    /// Returns the canonical absolute adapter executable.
    #[must_use]
    pub fn executable(&self) -> &Path {
        self.executable.canonical_path()
    }

    /// Returns the canonical absolute workspace.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        self.workspace.canonical_path()
    }

    pub(crate) fn pinned_workspace(&self) -> &PinnedDirectory {
        &self.workspace
    }

    /// Returns allowed environment names without exposing their values.
    pub fn environment_names(&self) -> impl Iterator<Item = &OsStr> {
        self.environment.keys().map(OsString::as_os_str)
    }

    /// Builds a fresh supervised launch specification for this pinned profile.
    #[must_use]
    pub fn launch_spec(&self) -> RuntimeLaunchSpec {
        self.launch_spec_in(self.workspace.clone())
    }

    pub(crate) fn launch_spec_in(&self, workspace: PinnedDirectory) -> RuntimeLaunchSpec {
        let profile = self.profile.profile();
        let mut spec = RuntimeLaunchSpec::from_pinned_script(self.executable.clone(), workspace);
        spec.arguments = profile.arguments.iter().map(OsString::from).collect();
        spec.environment.clone_from(&self.environment);
        spec
    }

    /// Binds protocol extensions to the selected profile without inspecting
    /// executable contents or restricting user-selected package versions.
    #[must_use]
    pub fn bind_client_config(&self, client: AcpV1ClientConfig) -> AcpV1ClientConfig {
        let adapter_profile = match self.profile {
            AcpRuntimeProfileId::Codex => AcpV1AdapterProfile::Codex162,
            AcpRuntimeProfileId::ClaudeCode => AcpV1AdapterProfile::Generic,
        };
        client.adapter_profile(adapter_profile)
    }

    /// Launches the pinned adapter with the ACP v1 runtime bridge.
    ///
    /// # Errors
    ///
    /// Returns ACP client configuration or supervised process launch failures.
    pub fn launch(
        &self,
        client: AcpV1ClientConfig,
    ) -> Result<AcpV1RuntimeBridge, AcpRuntimeProfileLaunchError> {
        AcpV1RuntimeBridge::launch(&self.launch_spec(), self.bind_client_config(client))
            .map_err(Into::into)
    }
}

impl fmt::Debug for ResolvedAcpRuntimeProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedAcpRuntimeProfile")
            .field("profile", &self.profile)
            .field("executable", &self.executable.canonical_path())
            .field("workspace", &self.workspace.canonical_path())
            .field("environment_names", &self.environment.keys())
            .finish()
    }
}

/// Resolves allowlisted adapter profiles into immutable launch specifications.
#[derive(Debug, Default, Clone, Copy)]
pub struct AcpRuntimeProfileResolver;

impl AcpRuntimeProfileResolver {
    /// Resolves and validates one trusted local adapter and workspace without spawning.
    ///
    /// An explicit executable must be absolute. Without one, only absolute
    /// directories in the supplied `PATH` are searched. Resolution never
    /// downloads, installs, invokes a shell, or delegates to a package runner.
    ///
    /// # Errors
    ///
    /// Rejects missing, mismatched, non-regular, or non-executable adapter
    /// targets and unavailable/non-directory workspaces. This resolver bounds
    /// launch shape; it does not attest local package provenance.
    pub fn resolve(
        request: AcpRuntimeProfileRequest,
    ) -> Result<ResolvedAcpRuntimeProfile, AcpRuntimeProfileResolveError> {
        let profile = request.profile.profile();
        let executable = match request.executable {
            Some(path) => resolve_explicit_executable(profile, &path)?,
            None => resolve_from_path(profile, request.environment.get(OsStr::new("PATH")))?,
        };
        let workspace = canonical_directory(&request.workspace)?;
        let mut environment: BTreeMap<OsString, OsString> = request
            .environment
            .into_iter()
            .filter(|(name, _)| {
                name.to_str()
                    .is_some_and(|name| profile.allowed_environment_names().any(|v| v == name))
            })
            .collect();
        if let Some(path) = environment.get_mut(OsStr::new("PATH")) {
            *path = absolute_path_entries(path);
        }

        Ok(ResolvedAcpRuntimeProfile {
            profile: request.profile,
            executable,
            workspace,
            environment,
        })
    }
}

fn absolute_path_entries(path: &OsStr) -> OsString {
    let entries = env::split_paths(path)
        .filter(|entry| entry.is_absolute())
        .collect::<Vec<_>>();
    env::join_paths(entries).unwrap_or_default()
}

/// Failure while pinning an ACP adapter profile before process launch.
#[derive(Debug, Error)]
pub enum AcpRuntimeProfileResolveError {
    /// Explicit adapter paths cannot depend on a daemon working directory.
    #[error("ACP adapter path must be absolute: {0}")]
    ExecutableNotAbsolute(PathBuf),
    /// A profile cannot be redirected to a different command.
    #[error("ACP adapter basename {actual:?} does not match required {expected:?}")]
    ExecutableNameMismatch {
        /// Profile-pinned executable basename.
        expected: &'static str,
        /// Rejected configured basename.
        actual: OsString,
    },
    /// The profile executable was not found in an explicit absolute `PATH` entry.
    #[error("ACP adapter executable {name:?} was not found in absolute PATH entries")]
    ExecutableNotFound {
        /// Profile-pinned executable basename.
        name: &'static str,
    },
    /// Filesystem metadata or canonicalization failed.
    #[error("ACP adapter is unavailable at {path}: {source}")]
    ExecutableUnavailable {
        /// Adapter path that could not be inspected.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The resolved target must be a regular file.
    #[error("ACP adapter is not a regular file: {0}")]
    ExecutableNotRegular(PathBuf),
    /// Unix requires at least one executable permission bit.
    #[error("ACP adapter is not executable: {0}")]
    ExecutableNotExecutable(PathBuf),
    /// Workspace canonicalization or metadata inspection failed.
    #[error("ACP workspace is unavailable at {path}: {source}")]
    WorkspaceUnavailable {
        /// Workspace path that could not be inspected.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// ACP sessions require a directory workspace.
    #[error("ACP workspace is not a directory: {0}")]
    WorkspaceNotDirectory(PathBuf),
}

/// Failure returned after a profile has resolved and launch is attempted.
#[derive(Debug, Error)]
pub enum AcpRuntimeProfileLaunchError {
    /// ACP bridge initialization or supervised process launch failed.
    #[error(transparent)]
    Bridge(#[from] AcpV1BridgeError),
}

fn resolve_explicit_executable(
    profile: &AcpRuntimeProfile,
    path: &Path,
) -> Result<PinnedExecutable, AcpRuntimeProfileResolveError> {
    if !path.is_absolute() {
        return Err(AcpRuntimeProfileResolveError::ExecutableNotAbsolute(
            path.to_path_buf(),
        ));
    }
    if path.file_name() != Some(OsStr::new(profile.executable_name)) {
        return Err(AcpRuntimeProfileResolveError::ExecutableNameMismatch {
            expected: profile.executable_name,
            actual: path.file_name().unwrap_or_default().to_os_string(),
        });
    }
    canonical_executable(path)
}

fn resolve_from_path(
    profile: &AcpRuntimeProfile,
    path: Option<&OsString>,
) -> Result<PinnedExecutable, AcpRuntimeProfileResolveError> {
    let Some(path) = path else {
        return Err(AcpRuntimeProfileResolveError::ExecutableNotFound {
            name: profile.executable_name,
        });
    };
    for directory in env::split_paths(path) {
        if !directory.is_absolute() {
            continue;
        }
        let candidate = directory.join(profile.executable_name);
        match fs::symlink_metadata(&candidate) {
            Ok(_) => return canonical_executable(&candidate),
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(AcpRuntimeProfileResolveError::ExecutableUnavailable {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Err(AcpRuntimeProfileResolveError::ExecutableNotFound {
        name: profile.executable_name,
    })
}

fn canonical_executable(path: &Path) -> Result<PinnedExecutable, AcpRuntimeProfileResolveError> {
    // Opening the configured path is the successful admission linearization
    // point. npm-style final symlinks are followed by that single open, while
    // later launches remain bound to the resulting descriptor.
    match PinnedExecutable::pin(path) {
        Ok(executable) => Ok(executable),
        Err(source) => {
            // Classification after a failed pin is diagnostic only. A racing
            // replacement can at worst produce the generic unavailable error;
            // it can never turn this failed admission into a launch handle.
            let diagnostic_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            match fs::metadata(path) {
                Ok(metadata) if !metadata.is_file() => Err(
                    AcpRuntimeProfileResolveError::ExecutableNotRegular(diagnostic_path),
                ),
                Ok(metadata) if !is_executable(&metadata) => Err(
                    AcpRuntimeProfileResolveError::ExecutableNotExecutable(diagnostic_path),
                ),
                _ => Err(AcpRuntimeProfileResolveError::ExecutableUnavailable {
                    path: diagnostic_path,
                    source,
                }),
            }
        }
    }
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

fn canonical_directory(path: &Path) -> Result<PinnedDirectory, AcpRuntimeProfileResolveError> {
    PinnedDirectory::pin(path).map_err(|source| {
        if source.kind() == io::ErrorKind::InvalidInput && path.is_absolute() {
            AcpRuntimeProfileResolveError::WorkspaceNotDirectory(path.to_path_buf())
        } else {
            AcpRuntimeProfileResolveError::WorkspaceUnavailable {
                path: path.to_path_buf(),
                source,
            }
        }
    })
}

#[cfg(test)]
#[path = "profile/tests.rs"]
mod tests;
