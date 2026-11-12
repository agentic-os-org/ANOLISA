//! Container lifecycle over the trusted docker CLI, fail closed at every step.

use crate::{DockerConfig, DockerError, translate};
use asc_pep_runtime::{PepError, SandboxBackend};
use asc_sandbox_types::{
    BackendCapabilities, ExecutionResult, ExecutionSpec, LaunchSpec, OsPermissionSet, PathFact,
    Validate,
};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Diagnostics carried into errors are bounded independently of CLI output.
const MAX_DIAGNOSTIC_CHARS: usize = 4096;

/// Docker enforcement backend pinned to one operator-owned configuration.
#[derive(Debug, Clone)]
pub struct DockerBackend {
    config: DockerConfig,
}

impl DockerBackend {
    /// Validates the deployment configuration before any container is placed.
    ///
    /// # Errors
    /// Rejects an untrusted CLI path, a missing CLI binary, or malformed image
    /// and limit values.
    pub fn new(config: DockerConfig) -> Result<Self, DockerError> {
        config.validate()?;
        // Fail with an actionable message instead of a bare spawn ENOENT at
        // the first container placement.
        if !config.cli.is_file() {
            return Err(DockerError::Unsupported(format!(
                "docker CLI not found at {}; install docker or pass an explicit --docker-cli path",
                config.cli.display()
            )));
        }
        Ok(Self { config })
    }

    /// Runs a command directly; this entrypoint never evaluates or reports to a PDP.
    ///
    /// # Errors
    /// Returns invalid input, unsupported restrictions, or placement errors.
    pub fn execute(
        &self,
        launch: LaunchSpec,
        permissions: OsPermissionSet,
    ) -> Result<ExecutionResult, PepError> {
        self.execute_bound(&ExecutionSpec {
            version: 1,
            launch,
            permissions,
            path_facts: Vec::new(),
            mount_aliases: Vec::new(),
        })
    }

    fn run(&self, spec: &ExecutionSpec) -> Result<ExecutionResult, DockerError> {
        spec.validate()?;
        for fact in &spec.path_facts {
            if path_fact(Path::new(&fact.path))? != *fact {
                return Err(DockerError::Unsupported(format!(
                    "path identity changed: {}",
                    fact.path
                )));
            }
        }
        // Bind sources must exist before placement; the container cannot
        // create them and a missing source would silently change the view.
        for rule in &spec.permissions.filesystem.rules {
            let metadata = fs::metadata(&rule.path).map_err(|error| {
                DockerError::Unsupported(format!("bind source {}: {error}", rule.path))
            })?;
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(DockerError::Unsupported(format!(
                    "bind source must be a regular file or directory, not a socket or special object: {}",
                    rule.path
                )));
            }
        }
        if !fs::metadata(&spec.launch.cwd)
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            return Err(DockerError::Unsupported(format!(
                "working directory is not a host directory: {}",
                spec.launch.cwd
            )));
        }
        let name = format!("asc-sandbox-{}", uuid::Uuid::new_v4());
        let plan = translate::translate(spec, &self.config, &name)?;
        let mut create_args = plan.create_args.clone();
        // network=none does not isolate pathname sockets, so both modes need this filter.
        let profile = crate::seccomp::network_profile()?;
        let mut seccomp = tempfile::NamedTempFile::new()?;
        seccomp.write_all(&serde_json::to_vec(&profile)?)?;
        create_args.extend([
            "--security-opt".into(),
            format!("seccomp={}", seccomp.path().display()),
        ]);
        create_args.push(self.config.image.clone());
        create_args.push(spec.launch.program.clone());
        create_args.extend(spec.launch.args.iter().cloned());
        let created = self
            .cli(&["create"])
            .args(&create_args)
            .output()
            .map_err(DockerError::Io)?;
        if !created.status.success() {
            return Err(DockerError::Protocol(format!(
                "docker create failed: {}",
                diagnostic(&created.stderr)
            )));
        }
        let mut cleanup = Cleanup {
            cli: &self.config.cli,
            name: name.clone(),
            armed: true,
        };
        let mut result = ExecutionResult::default();
        self.verify_fence(&name, &plan, &profile)?;
        result.installed = true;
        // Attach keeps the target's stdio interactive; the CLI exit status is
        // cross-checked against the inspected state below.
        let started = self
            .cli(&["start", "--attach", "--interactive", &name])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(DockerError::Io)?;
        let inspected = self.inspect(&name)?;
        let state = &inspected["State"];
        result.started = state["StartedAt"]
            .as_str()
            .is_some_and(|at| !at.is_empty() && !at.starts_with("0001-01-01"));
        if !result.started {
            return Err(DockerError::Protocol(format!(
                "container never started: {}",
                diagnostic(state["Error"].as_str().unwrap_or_default().as_bytes())
            )));
        }
        result.exit_code = state["ExitCode"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok());
        if result.exit_code.is_none() {
            return Err(DockerError::Protocol(
                "container state lacks an exit code".into(),
            ));
        }
        if !started.success() && result.exit_code != started.code() {
            return Err(DockerError::Protocol(format!(
                "docker start status {:?} disagrees with container exit code {:?}",
                started.code(),
                result.exit_code
            )));
        }
        cleanup.disarm();
        self.remove(&name)?;
        Ok(result)
    }

    /// Confirms the created container installs exactly the planned fence.
    fn verify_fence(
        &self,
        name: &str,
        plan: &crate::ContainerPlan,
        profile: &serde_json::Value,
    ) -> Result<(), DockerError> {
        let inspected = self.inspect(name)?;
        let host = &inspected["HostConfig"];
        if host["NetworkMode"].as_str() != Some(plan.network_mode.as_str())
            || host["ReadonlyRootfs"].as_bool() != Some(true)
            || !host["CapDrop"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|cap| cap.as_str() == Some("ALL")))
        {
            return Err(DockerError::Protocol(
                "created container does not match the planned network/rootfs/capability fence"
                    .into(),
            ));
        }
        if !seccomp_matches(host, profile) {
            return Err(DockerError::Protocol(
                "container lacks the exact AF_UNIX seccomp fence".into(),
            ));
        }
        let actual = inspected["Mounts"]
            .as_array()
            .ok_or_else(|| DockerError::Protocol("container inspect lacks mounts".into()))?;
        for mount in &plan.mounts {
            let matched = actual.iter().any(|entry| {
                entry["Source"].as_str() == Some(mount.source.as_str())
                    && entry["Destination"].as_str() == Some(mount.destination.as_str())
                    && entry["RW"].as_bool() == Some(!mount.read_only)
            });
            if !matched {
                return Err(DockerError::Protocol(format!(
                    "created container lacks the planned mount: {}",
                    mount.destination
                )));
            }
        }
        Ok(())
    }

    fn inspect(&self, name: &str) -> Result<serde_json::Value, DockerError> {
        let output = self.cli(&["inspect", name]).output()?;
        if !output.status.success() {
            return Err(DockerError::Protocol(format!(
                "docker inspect failed: {}",
                diagnostic(&output.stderr)
            )));
        }
        let mut list: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
        if list.len() != 1 {
            return Err(DockerError::Protocol(
                "docker inspect returned an unexpected container count".into(),
            ));
        }
        Ok(list.remove(0))
    }

    fn remove(&self, name: &str) -> Result<(), DockerError> {
        let output = self.cli(&["rm", "-f", name]).output()?;
        if !output.status.success() {
            return Err(DockerError::Protocol(format!(
                "docker cleanup failed: {}",
                diagnostic(&output.stderr)
            )));
        }
        Ok(())
    }

    /// Every CLI call runs without ambient environment or credentials.
    fn cli(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.config.cli);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

impl SandboxBackend for DockerBackend {
    fn capabilities(&self) -> Result<BackendCapabilities, PepError> {
        // strict_filesystem refers to host-path isolation: only authorized
        // bind mounts expose host data; the pinned image is the base world.
        let output = self
            .cli(&["version", "--format", "{{.Server.Version}}"])
            .output()?;
        if !output.status.success() {
            return Err(PepError::Unsupported(
                "docker daemon capability probe failed".into(),
            ));
        }
        Ok(BackendCapabilities {
            strict_filesystem: true,
            noexec: false,
            network_off: true,
        })
    }

    fn execute_bound(&self, spec: &ExecutionSpec) -> Result<ExecutionResult, PepError> {
        self.run(spec).map_err(PepError::from)
    }
}

/// Best-effort container removal when a run aborts before explicit cleanup.
struct Cleanup<'a> {
    cli: &'a Path,
    name: String,
    armed: bool,
}

impl Cleanup<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = Command::new(self.cli)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .args(["rm", "-f", &self.name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output();
        }
    }
}

// Docker expands the file to JSON; compare the full policy, not permissive heuristics.
fn seccomp_matches(host: &serde_json::Value, expected: &serde_json::Value) -> bool {
    let Some(options) = host["SecurityOpt"].as_array() else {
        return false;
    };
    let profiles: Vec<_> = options
        .iter()
        .filter_map(|option| {
            option
                .as_str()
                .and_then(|text| text.strip_prefix("seccomp="))
        })
        .collect();
    profiles.len() == 1
        && serde_json::from_str::<serde_json::Value>(profiles[0])
            .is_ok_and(|actual| actual == *expected)
}

fn path_fact(path: &Path) -> Result<PathFact, DockerError> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    let text = |value: &Path| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| DockerError::Unsupported("non-UTF-8 execution path".into()))
    };
    Ok(PathFact {
        path: text(path)?,
        canonical_path: text(&canonical)?,
        is_directory: metadata.is_dir(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn diagnostic(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.chars().take(MAX_DIAGNOSTIC_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_is_object_safe() {
        let cli = tempfile::NamedTempFile::new().unwrap();
        let config = DockerConfig {
            cli: cli.path().to_owned(),
            image: "agent:1.0".into(),
            memory: None,
            cpus: None,
            pids_limit: None,
        };
        let backend = DockerBackend::new(config).unwrap();
        let _: &dyn SandboxBackend = &backend;
    }

    #[test]
    fn missing_cli_is_rejected_at_construction() {
        let directory = tempfile::tempdir().unwrap();
        let error = DockerBackend::new(DockerConfig {
            cli: directory.path().join("absent-docker"),
            image: "agent:1.0".into(),
            memory: None,
            cpus: None,
            pids_limit: None,
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("docker CLI not found"),
            "{error}"
        );
    }
}
