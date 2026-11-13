//! Opt-in real-daemon regressions that touch only resources created by these tests.
#![cfg(target_os = "linux")]

use asc_pep_runtime::SandboxBackend;
use asc_sandbox_docker::{DockerBackend, DockerConfig};
use asc_sandbox_types::{ExecutionSpec, NetworkMode};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

// Unmount before cleanup so TempDir cannot recursively traverse a mounted directory.
struct Mounted(std::path::PathBuf);
impl Drop for Mounted {
    fn drop(&mut self) {
        rustix::mount::unmount(&self.0, rustix::mount::UnmountFlags::DETACH).unwrap();
    }
}

fn backend() -> DockerBackend {
    DockerBackend::new(DockerConfig {
        cli: "/usr/bin/docker".into(),
        host: "unix:///var/run/docker.sock".into(),
        guard: Some(env!("CARGO_BIN_EXE_asc-sandbox-docker-guard").into()),
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

// Derive only test-owned container config; do not build, access the network or alter user images.
struct ImageFixture {
    directory: tempfile::TempDir,
    seed: String,
    image: String,
}

impl ImageFixture {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("/usr/bin/docker");
        command
            .env_clear()
            .args(["--host", "unix:///var/run/docker.sock", "--config"])
            .arg(self.directory.path())
            .args(args);
        command
    }

    fn entrypoint_image() -> Self {
        let mut fixture = Self {
            directory: tempfile::tempdir().unwrap(),
            seed: format!("asc-entrypoint-test-{}", uuid::Uuid::new_v4()),
            image: String::new(),
        };
        std::fs::write(fixture.directory.path().join("config.json"), "{}").unwrap();
        let base = std::env::var("ASC_DOCKER_TEST_IMAGE").unwrap();
        let created = fixture
            .command(&["create", "--pull", "never", "--name", &fixture.seed, &base])
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
        let committed = fixture
            .command(&[
                "commit",
                "--change",
                "ENTRYPOINT [\"/bin/false\"]",
                "--change",
                "CMD [\"inherited-image-argument\"]",
                "--change",
                "USER 65534",
                &fixture.seed,
            ])
            .output()
            .unwrap();
        assert!(
            committed.status.success(),
            "{}",
            String::from_utf8_lossy(&committed.stderr)
        );
        String::from_utf8(committed.stdout)
            .unwrap()
            .trim()
            .clone_into(&mut fixture.image);
        assert!(fixture.image.starts_with("sha256:"));
        fixture
    }
}

impl Drop for ImageFixture {
    fn drop(&mut self) {
        let removed = self
            .command(&["rm", "-f", "--volumes", &self.seed])
            .output()
            .unwrap();
        assert!(removed.status.success(), "test seed cleanup failed");
        if !self.image.is_empty() {
            let removed = self
                .command(&["image", "rm", &self.image])
                .output()
                .unwrap();
            assert!(
                removed.status.success(),
                "test image cleanup failed: {}",
                String::from_utf8_lossy(&removed.stderr)
            );
        }
    }
}

#[test]
#[ignore = "requires host mount privileges, local Docker and pinned Python image"]
fn requested_program_overrides_image_entrypoint_and_cmd() {
    let image = ImageFixture::entrypoint_image();
    let work = tempfile::tempdir().unwrap();
    std::fs::set_permissions(work.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let backend = DockerBackend::new(DockerConfig {
        cli: "/usr/bin/docker".into(),
        host: "unix:///var/run/docker.sock".into(),
        guard: Some(env!("CARGO_BIN_EXE_asc-sandbox-docker-guard").into()),
        image: image.image.clone(),
        memory: None,
        cpus: None,
        pids_limit: Some(64),
    })
    .unwrap();
    let mut request = spec(
        work.path(),
        "import os, sys; from pathlib import Path; assert os.getuid() == 65534; assert sys.argv[1:] == ['a b', '', '--literal']; Path('executed').write_text(os.getcwd()); sys.exit(7)",
    );
    request
        .launch
        .args
        .extend(["a b".into(), String::new(), "--literal".into()]);
    assert_eq!(backend.execute_bound(&request).unwrap().exit_code, Some(7));
    assert_eq!(
        std::fs::read_to_string(work.path().join("executed")).unwrap(),
        work.path().to_str().unwrap()
    );
    // Empty args must not inherit image CMD; the custom script fails on any extra argument.
    let executable = work.path().join("no-args");
    std::fs::write(&executable, "#!/bin/sh\n[ \"$#\" = 0 ]\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    request.launch.program = executable.to_str().unwrap().into();
    request
        .launch
        .resolved_program
        .clone_from(&request.launch.program);
    request.launch.args.clear();
    assert_eq!(backend.execute_bound(&request).unwrap().exit_code, Some(0));
}

fn wait_for(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for lifecycle transition"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "requires host mount privileges, local Docker and pinned Python image"]
fn supervisor_exit_removes_container() {
    if let Ok(work) = std::env::var("ASC_DOCKER_CHILD_WORK") {
        let backend = DockerBackend::new(DockerConfig {
            cli: Path::new(&work).join("docker"),
            host: "unix:///var/run/docker.sock".into(),
            guard: Some(env!("CARGO_BIN_EXE_asc-sandbox-docker-guard").into()),
            image: std::env::var("ASC_DOCKER_TEST_IMAGE").unwrap(),
            memory: None,
            cpus: None,
            pids_limit: Some(64),
        })
        .unwrap();
        let _ = backend.execute_bound(&spec(
            Path::new(&work),
            "from pathlib import Path; import time; Path('running').touch(); time.sleep(3600)",
        ));
        return;
    }
    for (signal, group) in [
        (rustix::process::Signal::TERM, false),
        (rustix::process::Signal::HUP, false),
        (rustix::process::Signal::INT, false),
        (rustix::process::Signal::TERM, true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let work = directory.path();
        let cli = work.join("docker");
        std::fs::write(&cli, format!("#!/bin/sh\nif [ \"$5\" = start ]; then\n printf '%s' \"$8\" > '{}/name'\n printf '%s' \"$2\" > '{}/client'\nfi\nexec /usr/bin/docker \"$@\"\n", work.display(), work.display())).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "supervisor_exit_removes_container"])
            .env("ASC_DOCKER_CHILD_WORK", work)
            .process_group(0)
            .spawn()
            .unwrap();
        wait_for(|| work.join("running").exists());
        let name = std::fs::read_to_string(work.join("name")).unwrap();
        let client = std::fs::read_to_string(work.join("client")).unwrap();
        let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        if group {
            rustix::process::kill_process_group(pid, signal).unwrap();
        } else {
            rustix::process::kill_process(pid, signal).unwrap();
        }
        assert!(!child.wait().unwrap().success());
        wait_for(|| !Path::new(&client).exists());
        let result = Command::new("/usr/bin/docker")
            .args([
                "--config",
                work.to_str().unwrap(),
                "--host",
                "unix:///var/run/docker.sock",
                "ps",
                "--all",
                "--filter",
                &format!("name=^/{name}$"),
                "--format",
                "{{.ID}}",
            ])
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(
            result.stdout.is_empty(),
            "container survived supervisor death"
        );
    }
}

#[test]
#[ignore = "requires host mount privileges, local Docker and pinned Python image"]
fn guard_eof_waits_for_inflight_create() {
    use std::io::{BufRead as _, Write as _};
    let directory = tempfile::tempdir().unwrap();
    let work = directory.path();
    let cli = work.join("docker");
    std::fs::write(&cli, format!("#!/bin/sh\nif [ \"$5\" = create ]; then\n touch '{}/creating'\n sleep 1\nfi\nexec /usr/bin/docker \"$@\"\n", work.display())).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = DockerConfig {
        cli,
        host: "unix:///var/run/docker.sock".into(),
        guard: Some(env!("CARGO_BIN_EXE_asc-sandbox-docker-guard").into()),
        image: std::env::var("ASC_DOCKER_TEST_IMAGE").unwrap(),
        memory: None,
        cpus: None,
        pids_limit: Some(64),
    };
    let name = format!("asc-sandbox-{}", uuid::Uuid::new_v4());
    let request = serde_json::json!({"config": config, "spec": spec(work, "pass"), "name": name});
    let mut guard = Command::new(env!("CARGO_BIN_EXE_asc-sandbox-docker-guard"))
        .process_group(0)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = guard.stdin.take().unwrap();
    writeln!(input, "{request}").unwrap();
    let mut output = std::io::BufReader::new(guard.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let owned: serde_json::Value = serde_json::from_str(&line).unwrap();
    let client = owned["Owned"]["directory"].as_str().unwrap();
    wait_for(|| work.join("creating").exists());
    drop(input);
    assert!(guard.wait().unwrap().success());
    assert!(!Path::new(client).exists());
    let result = Command::new("/usr/bin/docker")
        .args([
            "--config",
            work.to_str().unwrap(),
            "--host",
            "unix:///var/run/docker.sock",
            "ps",
            "--all",
            "--filter",
            &format!("name=^/{name}$"),
            "--format",
            "{{.ID}}",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(result.stdout.is_empty(), "late create leaked a container");
}

#[test]
#[ignore = "requires host root, CAP_SYS_ADMIN, a local Docker daemon and pinned Python image"]
fn source_replacement_at_start_keeps_authorized_inode() {
    for bound in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let work = directory.path().join("work");
        let old = directory.path().join("old");
        let secret = directory.path().join("secret");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(work.join("value"), "authorized").unwrap();
        std::fs::write(secret.join("value"), "secret").unwrap();
        let cli = directory.path().join("docker");
        std::fs::write(&cli, format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n if [ \"$arg\" = start ]; then\n /usr/bin/mv '{}' '{}'\n /usr/bin/ln -s '{}' '{}'\n fi\ndone\nexec /usr/bin/docker \"$@\"\n",
            work.display(), old.display(), secret.display(), work.display()
        )).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let backend = DockerBackend::new(DockerConfig {
            cli,
            host: "unix:///var/run/docker.sock".into(),
            guard: Some(env!("CARGO_BIN_EXE_asc-sandbox-docker-guard").into()),
            image: std::env::var("ASC_DOCKER_TEST_IMAGE").unwrap(),
            memory: None,
            cpus: None,
            pids_limit: Some(64),
        })
        .unwrap();
        let mut request = spec(
            &work,
            "from pathlib import Path; assert Path('value').read_text() == 'authorized'; Path('value').write_text('changed')",
        );
        if bound {
            let metadata = std::fs::metadata(&work).unwrap();
            request.path_facts.push(asc_sandbox_types::PathFact {
                path: work.to_str().unwrap().into(),
                canonical_path: work.to_str().unwrap().into(),
                device: metadata.dev(),
                inode: metadata.ino(),
                is_directory: true,
            });
        }
        let result = backend.execute_bound(&request).unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(
            std::fs::read_to_string(old.join("value")).unwrap(),
            "changed"
        );
        assert_eq!(
            std::fs::read_to_string(secret.join("value")).unwrap(),
            "secret"
        );
    }
}

#[test]
#[ignore = "requires host mount privileges, local Docker and pinned Python image"]
fn personal_context_and_proxy_config_are_isolated() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join(".docker");
    std::fs::create_dir(&config).unwrap();
    let contents = br#"{"currentContext":"missing-remote-context","proxies":{"default":{"httpProxy":"http://test.invalid:1"}}}"#;
    std::fs::write(config.join("config.json"), contents).unwrap();
    let cli = directory.path().join("docker");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nexport HOME='{}' DOCKER_CONFIG='{}'\nexec /usr/bin/docker \"$@\"\n",
            directory.path().display(),
            config.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let backend = DockerBackend::new(DockerConfig {
        cli,
        host: "unix:///var/run/docker.sock".into(),
        guard: Some(env!("CARGO_BIN_EXE_asc-sandbox-docker-guard").into()),
        image: std::env::var("ASC_DOCKER_TEST_IMAGE").unwrap(),
        memory: None,
        cpus: None,
        pids_limit: Some(64),
    })
    .unwrap();
    let request = spec(
        directory.path(),
        "import os; assert not any(k.lower().endswith('_proxy') for k in os.environ)",
    );
    assert_eq!(backend.execute_bound(&request).unwrap().exit_code, Some(0));
    assert_eq!(std::fs::read(config.join("config.json")).unwrap(), contents);
}

#[test]
#[ignore = "requires host mount privileges, local Docker and pinned Python image"]
fn recursive_readonly_blocks_writable_submount() {
    let directory = tempfile::tempdir().unwrap();
    let work = directory.path().join("work");
    let nested = work.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let data = directory.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(data.join("value"), "original").unwrap();
    rustix::mount::mount_bind(&data, &nested).unwrap();
    let _mounted = Mounted(nested);
    let mut request = spec(
        &work,
        r"
import errno, os
from pathlib import Path
assert Path('nested/value').read_text() == 'original'
for path in ('top', 'nested/value'):
    try:
        Path(path).write_text('changed')
    except OSError as error:
        assert error.errno == errno.EROFS, error
    else:
        raise AssertionError('read-only write succeeded')
assert os.statvfs('nested').f_flag & os.ST_RDONLY
",
    );
    request.permissions.filesystem.rules[0].rights = vec![asc_sandbox_types::FileRight::Read];
    let result = backend().execute_bound(&request).unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(
        std::fs::read_to_string(data.join("value")).unwrap(),
        "original"
    );
    std::fs::write(data.join("value"), "host-still-writable").unwrap();
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
