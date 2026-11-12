//! Pure translation of the strictly expressible permission subset.

use crate::{DockerConfig, DockerError};
use asc_sandbox_types::{
    ExecutionSpec, FileRight, NetworkMode, RuleEffect, Validate, validate_os_path,
};

/// Process limit applied when the deployment does not choose one.
const DEFAULT_PIDS_LIMIT: u32 = 512;

/// One authorized bind mount, verified against the created container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPlan {
    /// Host path exposed to the container.
    pub source: String,
    /// Container path, identical to the source in this backend.
    pub destination: String,
    /// True unless the rule grants write.
    pub read_only: bool,
}

/// Container placement derived from one execution spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPlan {
    /// Arguments preceding the image in `docker create`.
    pub create_args: Vec<String>,
    /// Mounts that must appear exactly in the created container.
    pub mounts: Vec<MountPlan>,
    /// Docker network mode the created container must report.
    pub network_mode: String,
}

/// Builds the container plan or rejects anything containers cannot express.
///
/// The pinned image rootfs is the trusted base world; default-deny applies to
/// host paths, which appear only through the authorized bind mounts. The
/// target program must be an absolute path inside the image and the working
/// directory must be covered by an allow rule.
///
/// # Errors
/// Returns [`DockerError::Unsupported`] for deny rules, mount-level noexec,
/// reserved or inexpressible paths, duplicate rules and an uncovered working
/// directory; invalid input fails validation.
pub fn translate(
    spec: &ExecutionSpec,
    config: &DockerConfig,
    name: &str,
) -> Result<ContainerPlan, DockerError> {
    spec.validate()?;
    config.validate()?;
    validate_os_path(&spec.launch.program)?;
    if spec.launch.program != spec.launch.resolved_program {
        return Err(DockerError::Unsupported(
            "docker backend executes the program inside the image, not a host resolution".into(),
        ));
    }
    // The granted mode maps exactly: off fences the network, full retains
    // host networking without restriction.
    let network_mode = match spec.permissions.network.mode {
        NetworkMode::Off => "none",
        NetworkMode::Full => "host",
    };
    if !spec.permissions.execution.deny_paths.is_empty() {
        return Err(DockerError::Unsupported(
            "docker bind mounts cannot express mount-level noexec".into(),
        ));
    }
    let mut mounts = Vec::new();
    for rule in &spec.permissions.filesystem.rules {
        if rule.effect != RuleEffect::Allow {
            return Err(DockerError::Unsupported(
                "docker bind mounts cannot deny a subtree inside an allowed mount".into(),
            ));
        }
        if mounts.iter().any(|m: &MountPlan| m.source == rule.path) {
            return Err(DockerError::Unsupported(format!(
                "duplicate filesystem rule path: {}",
                rule.path
            )));
        }
        if reserved(&rule.path) {
            return Err(DockerError::Unsupported(format!(
                "reserved container path cannot be a bind mount: {}",
                rule.path
            )));
        }
        if rule.path.contains([':', ',']) {
            return Err(DockerError::Unsupported(format!(
                "path cannot be expressed as a docker bind mount: {}",
                rule.path
            )));
        }
        mounts.push(MountPlan {
            source: rule.path.clone(),
            destination: rule.path.clone(),
            read_only: !rule.rights.contains(&FileRight::Write),
        });
    }
    let cwd = &spec.launch.cwd;
    let covered = mounts.iter().any(|m| {
        *cwd == m.destination
            || cwd.starts_with(&format!("{}/", m.destination.trim_end_matches('/')))
    });
    if !covered {
        return Err(DockerError::Unsupported(format!(
            "working directory is not covered by an allow rule: {cwd}"
        )));
    }
    let mut args = vec![
        "--name".into(),
        name.into(),
        "--network".into(),
        network_mode.into(),
        "--read-only".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--pids-limit".into(),
        config.pids_limit.unwrap_or(DEFAULT_PIDS_LIMIT).to_string(),
        "--workdir".into(),
        cwd.clone(),
    ];
    if let Some(memory) = &config.memory {
        args.extend(["--memory".into(), memory.clone()]);
    }
    if let Some(cpus) = &config.cpus {
        args.extend(["--cpus".into(), cpus.clone()]);
    }
    for (key, value) in &spec.launch.env {
        args.push("--env".into());
        args.push(format!("{key}={value}"));
    }
    for mount in &mounts {
        args.push("--volume".into());
        let suffix = if mount.read_only { ":ro" } else { "" };
        args.push(format!("{}:{}{suffix}", mount.source, mount.destination));
    }
    Ok(ContainerPlan {
        create_args: args,
        mounts,
        network_mode: network_mode.into(),
    })
}

/// Paths owned by the container runtime or exposing the whole host.
fn reserved(path: &str) -> bool {
    path == "/"
        || [
            "/proc",
            "/dev",
            "/sys",
            "/run",
            "/var/run",
            "/var/lib/docker",
        ]
        .iter()
        .any(|base| path == *base || path.starts_with(&format!("{base}/")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_sandbox_types::ExecutionSpec;
    use std::path::PathBuf;

    fn config() -> DockerConfig {
        DockerConfig {
            cli: PathBuf::from("/usr/bin/docker"),
            image: "agent@sha256:abc123".into(),
            memory: None,
            cpus: None,
            pids_limit: None,
        }
    }

    fn spec(rules: &serde_json::Value, cwd: &str) -> ExecutionSpec {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "launch": {
                "program": "/usr/bin/python3",
                "resolved_program": "/usr/bin/python3",
                "args": ["-c", "print(1)"],
                "cwd": cwd,
                "env": {"FOO": "bar"}
            },
            "permissions": {
                "version": 1,
                "filesystem": {"default": "deny", "rules": rules},
                "execution": {"deny_paths": []},
                "network": {"mode": "off"}
            },
            "path_facts": []
        }))
        .unwrap()
    }

    fn allow(path: &str, rights: &[&str]) -> serde_json::Value {
        serde_json::json!({"effect": "allow", "path": path, "rights": rights})
    }

    #[test]
    fn translates_the_expressible_subset_exactly() {
        let spec = spec(
            &serde_json::json!([
                allow("/work", &["read"]),
                allow("/data", &["read", "write"])
            ]),
            "/work",
        );
        let plan = translate(&spec, &config(), "asc-sandbox-test").unwrap();
        for expected in [
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "no-new-privileges",
            "--pids-limit",
            "512",
            "--workdir",
            "/work",
            "--env",
            "FOO=bar",
            "/work:/work:ro",
            "/data:/data",
        ] {
            assert!(
                plan.create_args.iter().any(|arg| arg == expected),
                "missing {expected}: {:?}",
                plan.create_args
            );
        }
        assert_eq!(plan.network_mode, "none");
        assert_eq!(
            plan.mounts,
            vec![
                MountPlan {
                    source: "/work".into(),
                    destination: "/work".into(),
                    read_only: true
                },
                MountPlan {
                    source: "/data".into(),
                    destination: "/data".into(),
                    read_only: false
                }
            ]
        );
    }

    #[test]
    fn full_network_permission_maps_to_host_networking() {
        let mut spec = spec(&serde_json::json!([allow("/work", &["read"])]), "/work");
        spec.permissions.network.mode = NetworkMode::Full;
        let plan = translate(&spec, &config(), "asc-sandbox-test").unwrap();
        let position = plan
            .create_args
            .iter()
            .position(|arg| arg == "--network")
            .unwrap();
        assert_eq!(plan.create_args[position + 1], "host");
        assert_eq!(plan.network_mode, "host");
    }

    #[test]
    fn applies_operator_resource_limits() {
        let mut config = config();
        config.memory = Some("512m".into());
        config.cpus = Some("2.0".into());
        config.pids_limit = Some(64);
        let spec = spec(&serde_json::json!([allow("/work", &["read"])]), "/work");
        let plan = translate(&spec, &config, "asc-sandbox-test").unwrap();
        for pair in [
            ("--memory", "512m"),
            ("--cpus", "2.0"),
            ("--pids-limit", "64"),
        ] {
            let position = plan
                .create_args
                .iter()
                .position(|arg| arg == pair.0)
                .unwrap();
            assert_eq!(plan.create_args[position + 1], pair.1);
        }
    }

    #[test]
    fn rejects_every_inexpressible_restriction() {
        let deny = serde_json::json!([
            allow("/work", &["read"]),
            {"effect": "deny", "path": "/work/secret", "rights": ["read"]}
        ]);
        let cases = vec![
            (spec(&deny, "/work"), "deny"),
            (
                {
                    let mut s = spec(&serde_json::json!([allow("/work", &["read"])]), "/work");
                    s.permissions.execution.deny_paths = vec!["/work".into()];
                    s
                },
                "noexec",
            ),
            (
                spec(
                    &serde_json::json!([allow("/work", &["read"]), allow("/work", &["write"])]),
                    "/work",
                ),
                "duplicate",
            ),
            (
                spec(&serde_json::json!([allow("/", &["read"])]), "/"),
                "reserved",
            ),
            (
                spec(&serde_json::json!([allow("/proc/1", &["read"])]), "/work"),
                "reserved",
            ),
            (
                spec(&serde_json::json!([allow("/dev", &["read"])]), "/work"),
                "reserved",
            ),
            (
                spec(
                    &serde_json::json!([allow("/work/a:b", &["read"])]),
                    "/work/a:b",
                ),
                "bind mount",
            ),
            (
                spec(
                    &serde_json::json!([allow("/work", &["read"])]),
                    "/elsewhere",
                ),
                "working directory",
            ),
        ];
        for (spec, fragment) in cases {
            let error = translate(&spec, &config(), "asc-sandbox-test").unwrap_err();
            assert!(
                matches!(error, DockerError::Unsupported(_)),
                "{fragment}: {error}"
            );
            assert!(error.to_string().contains(fragment), "{fragment}: {error}");
        }
    }

    #[test]
    fn rejects_host_resolved_programs() {
        let mut spec = spec(&serde_json::json!([allow("/work", &["read"])]), "/work");
        spec.launch.resolved_program = "/host/usr/bin/python3".into();
        assert!(translate(&spec, &config(), "asc-sandbox-test").is_err());
    }
}
