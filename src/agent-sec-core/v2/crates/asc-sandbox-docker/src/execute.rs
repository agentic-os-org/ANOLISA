//! Container lifecycle over the trusted docker CLI, fail closed at every step.

use crate::{DockerConfig, DockerError, cleanup::Cleanup, client::Client, translate};
use asc_pep_runtime::{PepError, SandboxBackend};
use asc_sandbox_types::{
    BackendCapabilities, ExecutionResult, ExecutionSpec, LaunchSpec, OsPermissionSet, PathFact,
    Validate,
};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
#[cfg(test)]
use std::process::Stdio;
use std::time::Duration;

/// Diagnostics carried into errors are bounded independently of CLI output.
const MAX_DIAGNOSTIC_CHARS: usize = 4096;

/// Docker enforcement backend pinned to one operator-owned configuration.
#[derive(Debug, Clone)]
pub struct DockerBackend {
    pub(crate) config: DockerConfig,
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
        #[cfg(target_os = "linux")]
        {
            crate::guard::run(&self.config, spec)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = spec;
            Err(DockerError::Unsupported(
                "Docker sandbox requires Linux".into(),
            ))
        }
    }

    // Tests replace only mount syscalls; the full Docker lifecycle and fence checks still run.
    #[cfg(test)]
    pub(crate) fn run_with_stager<T>(
        &self,
        spec: &ExecutionSpec,
        client: &Client,
        prepare_mounts: impl FnOnce(&ExecutionSpec, &mut crate::ContainerPlan) -> Result<T, DockerError>,
    ) -> Result<ExecutionResult, DockerError> {
        self.run_session(
            spec,
            client,
            &format!("asc-sandbox-{}", uuid::Uuid::new_v4()),
            prepare_mounts,
            |client, name| {
                Ok(client
                    .command(&["start", "--attach", "--interactive", name])
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status()?
                    .code())
            },
        )
    }

    pub(crate) fn run_session<T>(
        &self,
        spec: &ExecutionSpec,
        client: &Client,
        name: &str,
        prepare_mounts: impl FnOnce(&ExecutionSpec, &mut crate::ContainerPlan) -> Result<T, DockerError>,
        attach: impl FnOnce(&Client, &str) -> Result<Option<i32>, DockerError>,
    ) -> Result<ExecutionResult, DockerError> {
        Self::validate_sources(spec)?;
        let mut plan = translate::translate(spec, &self.config, name)?;
        let staging = prepare_mounts(spec, &mut plan)?;
        client.verify_mounts(&plan)?;
        let mut cleanup = Cleanup::new(client, name, &plan, staging)?;
        let outcome = (|| {
            self.create_container(client, spec, &plan)?;
            cleanup.creation_confirmed = true;
            Self::verify_fence(client, name, &plan, &crate::seccomp::network_profile()?)?;
            let status = attach(client, name)?;
            Self::finished(client, name, status)
        })();
        cleanup.finish(outcome)
    }

    fn validate_sources(spec: &ExecutionSpec) -> Result<(), DockerError> {
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
        Ok(())
    }

    fn create_container(
        &self,
        client: &Client,
        spec: &ExecutionSpec,
        plan: &crate::ContainerPlan,
    ) -> Result<(), DockerError> {
        let mut create_args = plan.create_args.clone();
        // network=none does not isolate pathname sockets, so both modes need this filter.
        let profile = crate::seccomp::network_profile()?;
        let mut seccomp = tempfile::NamedTempFile::new_in(&client.directory)?;
        seccomp.write_all(&serde_json::to_vec(&profile)?)?;
        create_args.extend([
            "--security-opt".into(),
            format!("seccomp={}", seccomp.path().display()),
        ]);
        create_args.push(self.config.image.clone());
        create_args.push(spec.launch.program.clone());
        create_args.extend(spec.launch.args.iter().cloned());
        let mut command = client.command(&["create", "--pull", "never"]);
        command.args(&create_args);
        let created = client.output(command, Duration::from_secs(60))?;
        if !created.status.success() {
            return Err(DockerError::Protocol(format!(
                "docker create failed: {}",
                diagnostic(&created.stderr)
            )));
        }
        Ok(())
    }

    fn finished(
        client: &Client,
        name: &str,
        status: Option<i32>,
    ) -> Result<ExecutionResult, DockerError> {
        let mut result = ExecutionResult {
            installed: true,
            ..ExecutionResult::default()
        };
        let inspected = Self::inspect(client, name)?;
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
        if status != Some(0) && result.exit_code != status {
            return Err(DockerError::Protocol(format!(
                "docker start status {:?} disagrees with container exit code {:?}",
                status, result.exit_code
            )));
        }
        Ok(result)
    }

    /// Confirms the created container installs exactly the planned fence.
    fn verify_fence(
        client: &Client,
        name: &str,
        plan: &crate::ContainerPlan,
        profile: &serde_json::Value,
    ) -> Result<(), DockerError> {
        let inspected = Self::inspect(client, name)?;
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
            if mount.read_only && !recursive_readonly_matches(host, mount) {
                return Err(DockerError::Unsupported(format!(
                    "runtime did not retain forced recursive readonly: {}",
                    mount.destination
                )));
            }
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

    fn inspect(client: &Client, name: &str) -> Result<serde_json::Value, DockerError> {
        let output = client.output(client.command(&["inspect", name]), Duration::from_secs(10))?;
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

    pub(crate) fn capabilities_with_client(
        client: &Client,
    ) -> Result<BackendCapabilities, PepError> {
        let output = client
            .output(
                client.command(&["version", "--format", "{{.Server.Version}}"]),
                Duration::from_secs(10),
            )
            .map_err(PepError::from)?;
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
}

fn recursive_readonly_matches(host: &serde_json::Value, mount: &crate::MountPlan) -> bool {
    host["Mounts"].as_array().is_some_and(|mounts| {
        mounts.iter().any(|entry| {
            let options = &entry["BindOptions"];
            entry["Type"].as_str() == Some("bind")
                && entry["Source"].as_str() == Some(mount.source.as_str())
                && entry["Target"].as_str() == Some(mount.destination.as_str())
                && entry["ReadOnly"].as_bool() == Some(true)
                && options["ReadOnlyForceRecursive"].as_bool() == Some(true)
                && options["ReadOnlyNonRecursive"].as_bool() != Some(true)
                && options["NonRecursive"].as_bool() != Some(true)
                && options["Propagation"].as_str() == Some("rprivate")
        })
    })
}

impl SandboxBackend for DockerBackend {
    fn capabilities(&self) -> Result<BackendCapabilities, PepError> {
        let client = Client::new(&self.config).map_err(PepError::from)?;
        Self::capabilities_with_client(&client)
    }

    fn execute_bound(&self, spec: &ExecutionSpec) -> Result<ExecutionResult, PepError> {
        self.run(spec).map_err(PepError::from)
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
            host: "unix:///var/run/docker.sock".into(),
            guard: None,
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
            host: "unix:///var/run/docker.sock".into(),
            guard: None,
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
