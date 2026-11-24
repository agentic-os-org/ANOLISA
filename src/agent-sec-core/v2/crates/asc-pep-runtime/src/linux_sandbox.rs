//! Converts explicit permissions to a strict backend invocation without a PDP.

use crate::PepError;
use asc_sandbox_types::{
    BackendCapabilities, BackendSelection, ExecutionResult, ExecutionSpec, LaunchSpec,
    LinuxSandboxOptions, OsPermissionSet, PathFact, Validate, ValidationError,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// Explicit trusted helper location, chosen by the embedding application.
#[derive(Debug, Clone)]
pub struct LinuxSandboxBackend {
    helper: PathBuf,
    options: LinuxSandboxOptions,
}

impl LinuxSandboxBackend {
    fn check_landlock(&self, mode: asc_sandbox_types::LandlockMode) -> Result<(), PepError> {
        use asc_sandbox_types::LandlockMode;
        let mode = match mode {
            LandlockMode::Off => return Ok(()),
            LandlockMode::Auto => "auto",
            LandlockMode::Required => "required",
        };
        let reply = std::process::Command::new(&self.helper)
            .args(["--check-landlock", mode])
            .env_clear()
            .output()?;
        if !reply.status.success() || reply.stdout != b"landlock-write-v1\n" {
            return Err(PepError::Unsupported(format!(
                "Landlock {mode} preflight failed: {}",
                String::from_utf8_lossy(&reply.stderr).trim()
            )));
        }
        Ok(())
    }

    /// Uses a specific installed helper, never the target's PATH or environment.
    pub fn new(helper: PathBuf) -> Self {
        Self {
            helper,
            options: LinuxSandboxOptions::default(),
        }
    }

    /// Pin the deployment settings for both preflight and execution.
    #[must_use]
    pub fn with_options(mut self, options: LinuxSandboxOptions) -> Self {
        self.options = options;
        self
    }

    /// Resolves the sibling helper installed with the CLI.
    ///
    /// # Errors
    /// Returns an error when the application executable cannot be located.
    pub fn installed() -> Result<Self, PepError> {
        let executable = std::env::current_exe()?;
        let parent = executable.parent().ok_or_else(|| {
            PepError::Unsupported("application has no installation directory".into())
        })?;
        Ok(Self::new(parent.join("linux-sandbox")))
    }

    /// Probes the helper without passing target environment or executing a target.
    ///
    /// # Errors
    /// Rejects unavailable helpers and malformed capability replies.
    pub fn capabilities(&self) -> Result<BackendCapabilities, PepError> {
        let output = std::process::Command::new(&self.helper)
            .arg("--capabilities")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()?;
        if !output.status.success() {
            return Err(PepError::Unsupported(
                "strict backend capability probe failed".into(),
            ));
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    /// Installs exactly the supplied permissions and waits for the target tree.
    ///
    /// # Errors
    /// Returns preparation or helper launch errors; installed execution failures
    /// are retained in the returned result.
    pub fn execute(
        &self,
        launch: LaunchSpec,
        permissions: OsPermissionSet,
    ) -> Result<ExecutionResult, PepError> {
        self.execute_with_provider(launch, &crate::FixedPermissions(permissions))
    }

    /// Resolves a permission source and binds its path constraints before direct execution.
    ///
    /// # Errors
    /// Rejects invalid sources, changed path types and backend preparation failures.
    pub fn execute_with_provider(
        &self,
        launch: LaunchSpec,
        provider: &dyn crate::PermissionProvider,
    ) -> Result<ExecutionResult, PepError> {
        let prepared = provider.prepare(&launch)?;
        self.execute_bound(&ExecutionSpec { launch, ..prepared })
    }

    /// Executes a previously prepared invocation without refreshing its identity.
    ///
    /// # Errors
    /// Rejects malformed input, changed paths, and unsupported helpers.
    pub fn execute_bound(&self, spec: &ExecutionSpec) -> Result<ExecutionResult, PepError> {
        spec.validate()?;
        self.check_landlock(self.options.landlock)?;
        for fact in &spec.path_facts {
            if path_fact(Path::new(&fact.path))? != *fact {
                return Err(ValidationError::new(
                    "path_facts",
                    format!("path identity changed: {}", fact.path),
                )
                .into());
            }
        }
        crate::supervisor::run_with_options(&self.helper, spec, self.options)
    }
}

impl crate::SandboxBackend for LinuxSandboxBackend {
    fn selection(&self) -> BackendSelection {
        BackendSelection::LinuxSandbox(self.options)
    }

    fn capabilities(&self) -> Result<BackendCapabilities, PepError> {
        Self::capabilities(self)
    }

    fn execute_bound(&self, spec: &ExecutionSpec) -> Result<ExecutionResult, PepError> {
        Self::execute_bound(self, spec)
    }

    fn preflight(&self, spec: &ExecutionSpec) -> Result<(), PepError> {
        self.check_landlock(self.options.landlock)?;
        #[cfg(target_os = "linux")]
        if self.options.landlock == asc_sandbox_types::LandlockMode::Required {
            return crate::supervisor::preflight_landlock(&self.helper, spec, self.options);
        }
        // The bwrap mount plan is backend-specific; other backends validate
        // their own placement inside execute_bound.
        #[cfg(target_os = "linux")]
        linux_sandbox::runtime::check_mount_plan(spec)
            .map_err(|e| PepError::Unsupported(e.to_string()))?;
        #[cfg(not(target_os = "linux"))]
        let _ = spec;
        Ok(())
    }
}

/// Runs a command directly; this entrypoint never evaluates or reports to a PDP.
///
/// # Errors
/// Returns invalid input, unsupported backend, or preparation errors.
pub fn execute(
    launch: LaunchSpec,
    permissions: OsPermissionSet,
) -> Result<ExecutionResult, PepError> {
    LinuxSandboxBackend::installed()?.execute(launch, permissions)
}

/// Resolves a command using the target's PATH and cwd, preserving original argv.
///
/// # Errors
/// Rejects unresolved programs, invalid environment, and non-directory cwd.
pub fn prepare_launch(
    program: &str,
    args: Vec<String>,
    cwd: &Path,
    env: BTreeMap<String, String>,
) -> Result<LaunchSpec, PepError> {
    let cwd = canonicalize(cwd)?;
    if !cwd.is_dir() {
        return Err(PepError::Unsupported("cwd is not a directory".into()));
    }
    let resolved = if program.contains('/') {
        let path = Path::new(program);
        canonicalize(&if path.is_absolute() {
            path.to_owned()
        } else {
            cwd.join(path)
        })?
    } else {
        let path = env.get("PATH").ok_or_else(|| {
            PepError::Unsupported("bare program requires an explicit PATH".into())
        })?;
        path.split(':')
            .map(|part| {
                let directory = Path::new(part);
                if directory.is_absolute() {
                    directory.join(program)
                } else {
                    cwd.join(directory).join(program)
                }
            })
            .find_map(|candidate| {
                let canonical = fs::canonicalize(candidate).ok()?;
                let meta = fs::metadata(&canonical).ok()?;
                (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(canonical)
            })
            .ok_or_else(|| {
                PepError::Unsupported(format!("program not executable in target PATH: {program}"))
            })?
    };
    let metadata = fs::metadata(&resolved)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(PepError::Unsupported(format!(
            "program is not executable: {}",
            resolved.display()
        )));
    }
    let launch = LaunchSpec {
        program: program.into(),
        resolved_program: path_string(&resolved)?,
        args,
        cwd: path_string(&cwd)?,
        env,
    };
    launch.validate()?;
    Ok(launch)
}

/// Captures canonical path identities without granting implicit dependencies.
///
/// # Errors
/// Missing objects, non-UTF-8 paths and unsupported input are rejected.
pub fn prepare_path_facts(
    launch: &LaunchSpec,
    permissions: &OsPermissionSet,
    additional: &[String],
) -> Result<Vec<PathFact>, PepError> {
    launch.validate()?;
    permissions.validate()?;
    let mut paths = BTreeSet::new();
    paths.insert(launch.resolved_program.clone());
    paths.insert(launch.cwd.clone());
    paths.extend(
        permissions
            .filesystem
            .rules
            .iter()
            .flat_map(|rule| std::iter::once(&rule.path).chain(&rule.excluded_paths))
            .cloned(),
    );
    paths.extend(permissions.execution.deny_paths.iter().cloned());
    paths.extend(additional.iter().cloned());
    let mut facts = BTreeMap::new();
    for path in paths {
        let fact = path_fact(Path::new(&path))?;
        let canonical = PathFact {
            path: fact.canonical_path.clone(),
            ..fact.clone()
        };
        for fact in [fact, canonical] {
            match facts.entry(fact.path.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(fact);
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    if entry.get() != &fact {
                        return Err(ValidationError::new(
                            "path_facts",
                            format!("path identity changed during preparation: {}", fact.path),
                        )
                        .into());
                    }
                }
            }
        }
    }
    for rule in &permissions.filesystem.rules {
        if rule.excluded_paths.is_empty() {
            continue;
        }
        for path in std::iter::once(&rule.path).chain(&rule.excluded_paths) {
            let fact = &facts[path];
            if !fact.is_directory || fact.path != fact.canonical_path {
                return Err(PepError::Unsupported(format!(
                    "subtree exclusion boundaries must be canonical directories: {path}"
                )));
            }
        }
    }
    Ok(facts.into_values().collect())
}

fn path_fact(path: &Path) -> Result<PathFact, PepError> {
    let canonical = canonicalize(path)?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        PepError::Unsupported(format!("cannot inspect {}: {error}", canonical.display()))
    })?;
    Ok(PathFact {
        path: path_string(path)?,
        canonical_path: path_string(&canonical)?,
        is_directory: metadata.is_dir(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn canonicalize(path: &Path) -> Result<PathBuf, PepError> {
    fs::canonicalize(path).map_err(|error| {
        PepError::Unsupported(format!("cannot resolve {}: {error}", path.display()))
    })
}

fn path_string(path: &Path) -> Result<String, PepError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| PepError::Unsupported("non-UTF-8 execution path".into()))
}
