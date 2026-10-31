//! Converts explicit permissions to a strict backend invocation without a PDP.

use crate::PepError;
use asc_policy_types::{
    ExecutionResult, ExecutionSpec, LaunchSpec, OsPermissionSet, PathFact, PepCapabilities,
    Validate,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// Explicit trusted helper location, chosen by the embedding application.
#[derive(Debug, Clone)]
pub struct SandboxBackend {
    helper: PathBuf,
}

impl SandboxBackend {
    /// Uses a specific installed helper, never the target's PATH or environment.
    pub fn new(helper: PathBuf) -> Self {
        Self { helper }
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
    pub fn capabilities(&self) -> Result<PepCapabilities, PepError> {
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
        let facts = prepare_path_facts(&launch, &permissions, &[])?;
        self.execute_bound(&ExecutionSpec {
            version: 1,
            launch,
            permissions,
            path_facts: facts,
        })
    }

    /// Executes a previously prepared invocation without refreshing its identity.
    ///
    /// # Errors
    /// Rejects malformed input, changed paths, and unsupported helpers.
    pub fn execute_bound(&self, spec: &ExecutionSpec) -> Result<ExecutionResult, PepError> {
        spec.validate()?;
        for fact in &spec.path_facts {
            if path_fact(Path::new(&fact.path))? != *fact {
                return Err(PepError::Authorization(format!(
                    "path identity changed: {}",
                    fact.path
                )));
            }
        }
        crate::supervisor::run(&self.helper, spec)
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
    SandboxBackend::installed()?.execute(launch, permissions)
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
    let cwd = fs::canonicalize(cwd)?;
    if !cwd.is_dir() {
        return Err(PepError::Unsupported("cwd is not a directory".into()));
    }
    let resolved = if program.contains('/') {
        let path = Path::new(program);
        fs::canonicalize(if path.is_absolute() {
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
            .map(|rule| rule.path.clone()),
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
                        return Err(PepError::Authorization(format!(
                            "path identity changed during preparation: {}",
                            fact.path
                        )));
                    }
                }
            }
        }
    }
    Ok(facts.into_values().collect())
}

fn path_fact(path: &Path) -> Result<PathFact, PepError> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    Ok(PathFact {
        path: path_string(path)?,
        canonical_path: path_string(&canonical)?,
        is_directory: metadata.is_dir(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn path_string(path: &Path) -> Result<String, PepError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| PepError::Unsupported("non-UTF-8 execution path".into()))
}
