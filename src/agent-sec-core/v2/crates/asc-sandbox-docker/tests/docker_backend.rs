//! Lifecycle checks against a scripted docker CLI; no daemon is required.
#![cfg(target_os = "linux")]

use crate::{DockerBackend, DockerConfig};
use asc_sandbox_types::{ExecutionSpec, LaunchSpec, NetworkMode, OsPermissionSet};
use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

// Crate unit tests replace privileged staging without exporting an API that bypasses mount pinning.
struct ScriptedBackend(DockerBackend);

impl ScriptedBackend {
    fn execute_bound(
        &self,
        spec: &ExecutionSpec,
    ) -> Result<asc_sandbox_types::ExecutionResult, asc_pep_runtime::PepError> {
        self.0
            .run_with_stager(
                spec,
                &crate::client::Client::scripted(&self.0.config),
                |_, _| Ok(()),
            )
            .map_err(Into::into)
    }

    fn execute(
        &self,
        launch: LaunchSpec,
        permissions: OsPermissionSet,
    ) -> Result<asc_sandbox_types::ExecutionResult, asc_pep_runtime::PepError> {
        self.execute_bound(&ExecutionSpec {
            version: 1,
            launch,
            permissions,
            path_facts: vec![],
            mount_aliases: vec![],
        })
    }

    fn capabilities(
        &self,
    ) -> Result<asc_sandbox_types::BackendCapabilities, asc_pep_runtime::PepError> {
        DockerBackend::capabilities_with_client(&crate::client::Client::scripted(&self.0.config))
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    state: PathBuf,
    backend: ScriptedBackend,
    work: PathBuf,
    data: PathBuf,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let work = directory.path().join("work");
    let data = directory.path().join("data");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let cli = directory.path().join("docker");
    std::fs::write(
        &cli,
        format!(
            r#"#!/bin/bash
state='{state}'
[ "$1" = --config ] || exit 65
[ -d "$2" ] || exit 65
[ "$3" = --host ] || exit 65
[ "$4" = unix:///var/run/docker.sock ] || exit 65
printf '%s\n' "$1" "$2" "$3" "$4" >> "$state/client.calls"
shift 4
case "$1" in
  version)
    [ -f "$state/fail-version" ] && exit 1
    echo 27.0.0 ;;
  create)
    shift; printf '%s\n' "$@" > "$state/create.args"; echo fake-id ;;
  inspect)
    if [ -f "$state/create.args" ] && grep -q '^seccomp=' "$state/create.args" \
       && [ ! -f "$state/strip-seccomp" ]; then
      # Mimic the daemon expanding the staged profile file into SecurityOpt;
      # the profile is echoed as a JSON string, so escape its quotes.
      p=$(grep '^seccomp=' "$state/create.args" | head -n1 | cut -d= -f2-)
      esc=$(sed 's/"/\\\\"/g' "$p")
      sed "s|\"HostConfig\":{{|\"HostConfig\":{{\"SecurityOpt\":[\"no-new-privileges\",\"seccomp=$esc\"],|" "$state/inspect.json"
    else
      cat "$state/inspect.json"
    fi ;;
  start)
    printf '%s\n' "$@" > "$state/start.args"
    code=$(cat "$state/start.code" 2>/dev/null || echo 7)
    exit "$code" ;;
  rm)
    shift; printf '%s\n' "$@" >> "$state/rm.calls" ;;
  *) exit 64 ;;
esac
"#,
            state = state.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let backend = DockerBackend::new(DockerConfig {
        cli,
        host: "unix:///var/run/docker.sock".into(),
        image: "agent@sha256:abc123".into(),
        memory: None,
        cpus: None,
        pids_limit: None,
    })
    .unwrap();
    Fixture {
        directory,
        state,
        backend: ScriptedBackend(backend),
        work,
        data,
    }
}

impl Fixture {
    fn write_inspect(&self, network: &str) {
        let inspect = serde_json::json!([{
            "HostConfig": {
                "NetworkMode": network,
                "ReadonlyRootfs": true,
                "CapDrop": ["ALL"],
                "Mounts": [{"Type": "bind", "Source": self.work, "Target": self.work,
                    "ReadOnly": true, "BindOptions": {
                        "ReadOnlyForceRecursive": true, "Propagation": "rprivate"
                    }}]
            },
            "Mounts": [
                {"Source": self.work, "Destination": self.work, "RW": false},
                {"Source": self.data, "Destination": self.data, "RW": true}
            ],
            "State": {
                "Running": false,
                "ExitCode": 7,
                "StartedAt": "2026-01-01T00:00:00Z",
                "Error": ""
            }
        }]);
        std::fs::write(
            self.state.join("inspect.json"),
            serde_json::to_vec(&inspect).unwrap(),
        )
        .unwrap();
    }

    fn spec(&self) -> ExecutionSpec {
        let permissions: OsPermissionSet = serde_json::from_value(serde_json::json!({
            "version": 1,
            "filesystem": {"default": "deny", "rules": [
                {"effect": "allow", "path": self.work, "rights": ["read"]},
                {"effect": "allow", "path": self.data, "rights": ["read", "write"]}
            ]},
            "execution": {"deny_paths": []},
            "network": {"mode": "off"}
        }))
        .unwrap();
        ExecutionSpec {
            version: 1,
            launch: LaunchSpec {
                program: "/usr/bin/python3".into(),
                resolved_program: "/usr/bin/python3".into(),
                args: vec!["-c".into(), "print(1)".into()],
                cwd: self.work.to_str().unwrap().into(),
                env: BTreeMap::new(),
            },
            permissions,
            path_facts: Vec::new(),
            mount_aliases: Vec::new(),
        }
    }

    fn read_calls(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.state.join(name))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

#[test]
fn successful_run_maps_the_container_lifecycle() {
    let fixture = fixture();
    fixture.write_inspect("none");
    let result = fixture.backend.execute_bound(&fixture.spec()).unwrap();
    assert!(result.installed);
    assert!(result.started);
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.signal, None);
    assert_eq!(result.error, None);
    let create = fixture.read_calls("create.args");
    for expected in ["--network", "none", "--read-only", "--cap-drop", "ALL"] {
        assert!(create.iter().any(|arg| arg == expected), "{expected}");
    }
    assert!(create.iter().any(|arg| arg == "agent@sha256:abc123"));
    assert_eq!(fixture.read_calls("rm.calls").len(), 2);
}

#[test]
fn client_endpoint_and_config_remain_fixed_for_lifecycle() {
    let fixture = fixture();
    fixture.write_inspect("none");
    fixture.backend.execute_bound(&fixture.spec()).unwrap();
    let calls = fixture.read_calls("client.calls");
    assert_eq!(calls.len(), 20);
    for invocation in calls.chunks_exact(4) {
        assert_eq!(invocation, &calls[..4]);
    }
    assert!(!Path::new(&calls[1]).exists());
    let create = fixture.read_calls("create.args");
    assert_eq!(&create[..2], ["--pull", "never"]);
    let mut config = fixture.backend.0.config.clone();
    for host in [
        "tcp://remote:2375",
        "ssh://remote",
        "unix://relative",
        "unix:///run/../sock",
    ] {
        config.host = host.into();
        assert!(DockerBackend::new(config.clone()).is_err());
    }
}

#[test]
fn full_network_permission_runs_with_host_networking() {
    let fixture = fixture();
    fixture.write_inspect("host");
    let mut spec = fixture.spec();
    spec.permissions.network.mode = NetworkMode::Full;
    let result = fixture.backend.execute_bound(&spec).unwrap();
    assert!(result.installed);
    assert!(result.started);
    let create = fixture.read_calls("create.args");
    let position = create.iter().position(|arg| arg == "--network").unwrap();
    assert_eq!(create[position + 1], "host");
    assert!(
        create.iter().any(|arg| arg.starts_with("seccomp=")),
        "host networking must install the AF_UNIX seccomp fence"
    );
}

#[test]
fn host_network_without_seccomp_fence_aborts_and_cleans_up() {
    let fixture = fixture();
    fixture.write_inspect("host");
    // Make the fake daemon drop the profile so the fence check must abort.
    std::fs::write(fixture.state.join("strip-seccomp"), "").unwrap();
    let mut spec = fixture.spec();
    spec.permissions.network.mode = NetworkMode::Full;
    let error = fixture.backend.execute_bound(&spec).unwrap_err();
    assert!(error.to_string().contains("AF_UNIX"), "{error}");
    assert!(!fixture.state.join("start.args").exists());
    assert_eq!(fixture.read_calls("rm.calls").len(), 2);
}

#[test]
fn fence_mismatch_aborts_before_start_and_cleans_up() {
    let fixture = fixture();
    fixture.write_inspect("bridge");
    let error = fixture.backend.execute_bound(&fixture.spec()).unwrap_err();
    assert!(error.to_string().contains("fence"), "{error}");
    assert!(!fixture.state.join("start.args").exists());
    assert_eq!(fixture.read_calls("rm.calls").len(), 2);
}

#[test]
fn recursive_readonly_downgrade_aborts_before_start() {
    let fixture = fixture();
    fixture.write_inspect("none");
    let path = fixture.state.join("inspect.json");
    let mut inspect: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    inspect[0]["HostConfig"]["Mounts"][0]["BindOptions"]["ReadOnlyForceRecursive"] = false.into();
    std::fs::write(path, serde_json::to_vec(&inspect).unwrap()).unwrap();
    let error = fixture.backend.execute_bound(&fixture.spec()).unwrap_err();
    assert!(error.to_string().contains("recursive readonly"), "{error}");
    assert!(!fixture.state.join("start.args").exists());
    let args = fixture.read_calls("create.args");
    assert!(
        args.iter()
            .any(|arg| arg.ends_with(",readonly,bind-recursive=readonly"))
    );
    assert!(!args.iter().any(|arg| arg == "--volume"));
    assert_eq!(fixture.read_calls("rm.calls").len(), 2);
}

#[test]
fn missing_bind_source_fails_before_create() {
    let fixture = fixture();
    fixture.write_inspect("none");
    std::fs::remove_dir_all(&fixture.data).unwrap();
    let error = fixture.backend.execute_bound(&fixture.spec()).unwrap_err();
    assert!(error.to_string().contains("bind source"), "{error}");
    assert!(!fixture.state.join("create.args").exists());
}

#[test]
fn capability_probe_requires_a_reachable_daemon() {
    let fixture = fixture();
    let capabilities = fixture.backend.capabilities().unwrap();
    assert!(capabilities.strict_filesystem);
    assert!(!capabilities.noexec);
    assert!(capabilities.network_off);
    std::fs::write(fixture.state.join("fail-version"), "").unwrap();
    assert!(fixture.backend.capabilities().is_err());
}

#[test]
fn start_status_must_agree_with_inspected_exit_code() {
    let fixture = fixture();
    fixture.write_inspect("none");
    std::fs::write(fixture.state.join("start.code"), "9").unwrap();
    let error = fixture.backend.execute_bound(&fixture.spec()).unwrap_err();
    assert!(error.to_string().contains("disagrees"), "{error}");
    assert_eq!(fixture.read_calls("rm.calls").len(), 2);
}

#[test]
fn changed_path_facts_are_rejected() {
    let fixture = fixture();
    fixture.write_inspect("none");
    let mut spec = fixture.spec();
    let target = fixture.directory.path().join("target");
    std::fs::write(&target, "first").unwrap();
    let meta = std::fs::metadata(&target).unwrap();
    spec.path_facts = vec![asc_sandbox_types::PathFact {
        path: target.to_str().unwrap().into(),
        canonical_path: target.to_str().unwrap().into(),
        is_directory: false,
        device: meta.dev(),
        inode: meta.ino(),
    }];
    std::fs::rename(&target, fixture.directory.path().join("previous-target")).unwrap();
    std::fs::write(&target, "replaced").unwrap();
    let error = fixture.backend.execute_bound(&spec).unwrap_err();
    assert!(
        error.to_string().contains("path identity changed"),
        "{error}"
    );
}

#[test]
fn direct_execute_convenience_matches_bound_execution() {
    let fixture = fixture();
    fixture.write_inspect("none");
    let spec = fixture.spec();
    let result = fixture
        .backend
        .execute(spec.launch, spec.permissions)
        .unwrap();
    assert_eq!(result.exit_code, Some(7));
}

#[test]
fn relative_cli_path_is_rejected_at_construction() {
    let error = DockerBackend::new(DockerConfig {
        cli: Path::new("docker").to_owned(),
        host: "unix:///var/run/docker.sock".into(),
        image: "agent:1.0".into(),
        memory: None,
        cpus: None,
        pids_limit: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("absolute path"), "{error}");
}
