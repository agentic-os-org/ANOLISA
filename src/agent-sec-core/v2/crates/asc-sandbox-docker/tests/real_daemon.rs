//! Opt-in real-daemon regressions that touch only resources created by these tests.
#![cfg(target_os = "linux")]

use asc_pep_runtime::SandboxBackend;
use asc_sandbox_docker::{DockerBackend, DockerConfig};
use asc_sandbox_types::{ExecutionSpec, NetworkMode};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;

fn backend() -> DockerBackend {
    DockerBackend::new(DockerConfig {
        cli: "/usr/bin/docker".into(),
        image: std::env::var("ASC_DOCKER_TEST_IMAGE")
            .expect("set ASC_DOCKER_TEST_IMAGE to a locally available pinned Python image"),
        memory: None,
        cpus: None,
        pids_limit: Some(64),
    })
    .unwrap()
}

fn spec(work: &Path, code: &str) -> ExecutionSpec {
    serde_json::from_value(serde_json::json!({
        "version": 1,
        "launch": {"program": "/usr/local/bin/python3", "resolved_program": "/usr/local/bin/python3",
            "args": ["-c", code], "cwd": work, "env": {}},
        "permissions": {"version": 1,
            "filesystem": {"default": "deny", "rules": [
                {"effect": "allow", "path": work, "rights": ["read", "write"]}
            ]}, "execution": {"deny_paths": []}, "network": {"mode": "off"}},
        "path_facts": []
    })).unwrap()
}

#[test]
#[ignore = "requires a local Docker daemon, pinned Python image and native cc"]
fn unix_channels_are_closed_in_both_network_modes() {
    let directory = tempfile::tempdir().unwrap();
    let work = directory.path();
    let socket = work.join("control.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    // Build a libc-free compat socketcall probe; run it on the host first to confirm ABI support.
    if cfg!(target_arch = "x86_64") {
        let source = work.join("compat.S");
        std::fs::write(&source, ".global _start\n.text\n_start:\n push $0\n push $2\n push $1\n mov %esp,%ecx\n mov $1,%ebx\n mov $102,%eax\n int $0x80\n test %eax,%eax\n js fail\n xor %ebx,%ebx\n jmp done\nfail:\n mov $1,%ebx\ndone:\n mov $1,%eax\n int $0x80\n").unwrap();
        assert!(
            Command::new("cc")
                .args(["-m32", "-nostdlib", "-static", "-o"])
                .arg(work.join("compat"))
                .arg(source)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new(work.join("compat"))
                .status()
                .unwrap()
                .success(),
            "host compat ABI unavailable; do not claim compat coverage"
        );
    }
    let code = r"
import errno, os, signal, socket, subprocess
for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
    try:
        client = socket.socket(socket.AF_UNIX, kind)
        client.connect('control.sock')
    except OSError as error:
        assert error.errno == errno.EPERM, error
    else:
        raise AssertionError('external Unix socket allowed')
try:
    socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
except OSError as error:
    assert error.errno == errno.EPERM, error
else:
    raise AssertionError('datagram pair allowed')
for kind in (socket.SOCK_STREAM, socket.SOCK_SEQPACKET):
    left, right = socket.socketpair(socket.AF_UNIX, kind)
    left.send(b'local')
    assert right.recv(5) == b'local'
    left.close(); right.close()
if os.path.exists('compat'):
    result = subprocess.run(['./compat'], check=False)
    assert result.returncode == -signal.SIGSYS, result.returncode
";
    let backend = backend();
    for mode in [NetworkMode::Off, NetworkMode::Full] {
        let mut request = spec(work, code);
        request.permissions.network.mode = mode;
        let result = backend.execute_bound(&request).unwrap();
        assert_eq!(result.exit_code, Some(0));
    }
    let mut request = spec(work, "pass");
    request.permissions.filesystem.rules[0].path = socket.to_str().unwrap().into();
    let error = backend.execute_bound(&request).unwrap_err();
    assert!(error.to_string().contains("special object"), "{error}");
}
