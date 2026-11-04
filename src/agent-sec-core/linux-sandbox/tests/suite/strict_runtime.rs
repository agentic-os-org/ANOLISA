//! Real isolation checks; dedicated Linux CI must enable ASC_SANDBOX_INTEGRATION.
#![cfg(target_os = "linux")]

use asc_sandbox_types::{BackendStatus, ExecutionSpec, PathFact};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn enabled() -> bool {
    std::env::var_os("ASC_SANDBOX_INTEGRATION").is_some()
}

fn fact(path: &Path) -> PathFact {
    let canonical = path.canonicalize().unwrap();
    let metadata = canonical.metadata().unwrap();
    PathFact {
        path: path.to_str().unwrap().into(),
        canonical_path: canonical.to_str().unwrap().into(),
        is_directory: metadata.is_dir(),
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn spec(work: &Path, shell: &str) -> ExecutionSpec {
    let program = Path::new("/bin/sh").canonicalize().unwrap();
    serde_json::from_value(serde_json::json!({
        "version":1,
        "launch":{"program":"sh","resolved_program":program,"args":["-c",shell],"cwd":work,"env":{"PATH":"/usr/bin:/bin"}},
        "permissions":{"version":1,"filesystem":{"default":"deny","rules":[
            {"effect":"allow","path":"/","rights":["read"]},
            {"effect":"allow","path":work,"rights":["read","write"]}
        ]},"execution":{"deny_paths":[]},"network":{"mode":"off"}},
        "path_facts":[fact(Path::new("/")),fact(work),fact(&program)]
    })).unwrap()
}

fn run(spec: &ExecutionSpec) -> (Output, Vec<BackendStatus>) {
    run_with(spec, None, &[])
}

fn run_with(
    spec: &ExecutionSpec,
    stdin: Option<Stdio>,
    extra_fds: &[RawFd],
) -> (Output, Vec<BackendStatus>) {
    run_with_stdio(
        spec,
        [
            stdin.unwrap_or_else(Stdio::null),
            Stdio::piped(),
            Stdio::piped(),
        ],
        extra_fds,
    )
}

fn run_with_stdio(
    spec: &ExecutionSpec,
    [stdin, stdout, stderr]: [Stdio; 3],
    extra_fds: &[RawFd],
) -> (Output, Vec<BackendStatus>) {
    let mut input = tempfile::tempfile().unwrap();
    serde_json::to_writer(&mut input, spec).unwrap();
    input.flush().unwrap();
    input.seek(SeekFrom::Start(0)).unwrap();
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    let mut reader = unsafe { File::from_raw_fd(fds[0]) };
    let writer = unsafe { File::from_raw_fd(fds[1]) };
    let input_fd = input.as_raw_fd();
    let status_fd = writer.as_raw_fd();
    let mut command = Command::new(env!("CARGO_BIN_EXE_linux-sandbox"));
    command.args([
        "--execution-spec-fd",
        &input_fd.to_string(),
        "--status-fd",
        &status_fd.to_string(),
    ]);
    command.stdin(stdin).stdout(stdout).stderr(stderr);
    let mut inherited = vec![input_fd, status_fd];
    inherited.extend_from_slice(extra_fds);
    let output = linux_sandbox::runtime::spawn_with_fds(&mut command, &inherited)
        .unwrap()
        .wait_with_output()
        .unwrap();
    drop(writer);
    let mut statuses = String::new();
    reader.read_to_string(&mut statuses).unwrap();
    let statuses: Vec<BackendStatus> = statuses
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    #[cfg(coverage)]
    collect_inner_profiles(spec, &statuses);
    (output, statuses)
}

#[cfg(coverage)]
fn collect_inner_profiles(spec: &ExecutionSpec, statuses: &[BackendStatus]) {
    let Some(profile_pattern) = std::env::var_os("LLVM_PROFILE_FILE") else {
        return;
    };
    let profile_directory = Path::new(&profile_pattern).parent().unwrap();
    let mut collected = 0;
    // The inner helper clears its environment and uses LLVM's default cwd output.
    // Preserve it before the test's writable temporary directory is removed.
    for entry in std::fs::read_dir(&spec.launch.cwd).unwrap() {
        let path = entry.unwrap().path();
        if !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("default_")
            || path
                .extension()
                .is_none_or(|extension| extension != "profraw")
        {
            continue;
        }
        let profile = tempfile::Builder::new()
            .prefix("strict-inner-")
            .suffix(".profraw")
            .tempfile_in(profile_directory)
            .unwrap();
        assert!(std::fs::copy(&path, profile.path()).unwrap() > 0);
        profile.keep().unwrap();
        std::fs::remove_file(path).unwrap();
        collected += 1;
    }
    if statuses.contains(&BackendStatus::Installed) {
        assert!(
            collected > 0,
            "inner helper coverage profile was not collected"
        );
    }
}

#[test]
fn strict_runtime_enforces_write_and_read_carveouts() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let key = work.path().join("key");
    std::fs::write(&key, "secret").unwrap();
    let readonly = tempfile::tempdir().unwrap();
    let readonly_file = readonly.path().join("unchanged");
    std::fs::write(&readonly_file, "original").unwrap();
    let command = format!(
        "echo ok > result && ! cat key && test -f result && ! echo overwritten > '{}'",
        readonly_file.display()
    );
    let mut spec = spec(work.path(), &command);
    spec.permissions.filesystem.rules.push(
        serde_json::from_value(serde_json::json!({"effect":"deny","path":key,"rights":["read"]}))
            .unwrap(),
    );
    spec.path_facts.push(fact(&key));
    let (output, statuses) = run(&spec);
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(statuses.contains(&BackendStatus::Installed));
    assert!(statuses.contains(&BackendStatus::Started));
    assert_eq!(std::fs::read_to_string(readonly_file).unwrap(), "original");
    assert_eq!(
        std::fs::read_to_string(work.path().join("result")).unwrap(),
        "ok\n"
    );
}

#[test]
fn strict_runtime_rejects_reserved_aliases_with_unmounted_parent() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let aliases = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("data"), "allowed").unwrap();
    let alias = aliases.path().join("source");
    let marker = work.path().join("started");
    let mut base = spec(work.path(), "printf ran > started");
    // Leave the alias parent unmounted so the destination is created in the empty root.
    base.permissions.filesystem.rules.remove(0);
    for path in ["/usr", "/lib", "/lib64"] {
        let path = Path::new(path);
        if path.exists() {
            base.permissions.filesystem.rules.push(
                serde_json::from_value(serde_json::json!({
                    "effect":"allow", "path":path, "rights":["read"]
                }))
                .unwrap(),
            );
            base.path_facts.push(fact(path));
        }
    }
    for source in [
        data.path(),
        Path::new("/proc"),
        Path::new("/proc/sys"),
        Path::new("/dev"),
        Path::new("/dev/pts"),
    ] {
        std::os::unix::fs::symlink(source, &alias).unwrap();
        let mut request = base.clone();
        request.permissions.filesystem.rules.push(
            serde_json::from_value(serde_json::json!({
                "effect":"allow", "path":alias, "rights":["read"]
            }))
            .unwrap(),
        );
        request.path_facts.push(fact(&alias));
        if source == data.path() {
            request.launch.args[1] =
                format!("test -r '{}/data' && printf ran > started", alias.display());
        }
        let (output, statuses) = run(&request);
        if source == data.path() {
            assert!(
                output.status.success(),
                "{} {statuses:?}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(statuses.contains(&BackendStatus::Started));
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "ran");
            std::fs::remove_file(&marker).unwrap();
        } else {
            assert_eq!(output.status.code(), Some(125));
            assert!(
                matches!(statuses.as_slice(), [BackendStatus::Failed { message }] if message.contains("reserved sandbox infrastructure path")),
                "{statuses:?}"
            );
            assert!(
                !marker.exists(),
                "target started for reserved source {}",
                source.display()
            );
        }
        std::fs::remove_file(&alias).unwrap();
    }
}

#[test]
fn strict_runtime_enforces_file_and_directory_noexec() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let executable = work.path().join("payload");
    std::fs::copy("/bin/true", &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let command = format!(
        "! ./payload && test -r ./payload && ! /proc/1/root{}/payload",
        work.path().display()
    );
    for path in [executable.as_path(), work.path()] {
        let mut spec = spec(work.path(), &command);
        spec.permissions.execution.deny_paths = vec![path.to_str().unwrap().into()];
        if path != work.path() {
            spec.path_facts.push(fact(path));
        }
        let (output, statuses) = run(&spec);
        assert!(
            output.status.success(),
            "{} {statuses:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(statuses.contains(&BackendStatus::Installed));
    }
}

#[test]
fn strict_runtime_drops_caps_and_applies_baseline_in_full_network() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let mut spec = spec(
        work.path(),
        "grep -q '^CapEff:[[:space:]]*0000000000000000$' /proc/self/status && grep -q '^NoNewPrivs:[[:space:]]*1$' /proc/self/status && ! unshare -Ur true",
    );
    spec.permissions.network.mode = asc_sandbox_types::NetworkMode::Full;
    let (output, statuses) = run(&spec);
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn strict_runtime_distinguishes_exec_failure_from_nonzero_exit() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let spec = spec(work.path(), "exit 23");
    let (output, statuses) = run(&spec);
    assert_eq!(output.status.code(), Some(23));
    assert!(statuses.contains(&BackendStatus::Exited {
        exit_code: Some(23),
        signal: None
    }));
    let mut invalid = spec;
    let text = work.path().join("not-executable");
    std::fs::write(&text, "not executable").unwrap();
    invalid.launch.resolved_program = text.to_str().unwrap().into();
    invalid.path_facts.push(fact(&text));
    let (_output, statuses) = run(&invalid);
    assert!(!statuses.contains(&BackendStatus::Started));
    assert!(
        statuses
            .iter()
            .any(|event| matches!(event, BackendStatus::Failed { .. }))
    );
}

#[test]
fn strict_runtime_blocks_ip_off_and_host_unix_sockets_in_full_mode() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let script = r#"python3 - <<'PYTHON'
import socket
for family in (socket.AF_INET, socket.AF_INET6):
    try:
        socket.socket(family, socket.SOCK_STREAM)
    except PermissionError:
        continue
    raise SystemExit('IP socket unexpectedly allowed')
PYTHON"#;
    let (output, statuses) = run(&spec(work.path(), script));
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let script = r#"python3 - <<'PYTHON'
import socket
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
    s.bind(('127.0.0.1', 0))
    s.listen()
try:
    socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
except PermissionError:
    pass
else:
    raise SystemExit('host Unix socket unexpectedly allowed')
PYTHON"#;
    let mut spec = spec(work.path(), script);
    spec.permissions.network.mode = asc_sandbox_types::NetworkMode::Full;
    let (output, statuses) = run(&spec);
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn strict_runtime_reports_signals_and_reaps_background_descendants() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let (_output, statuses) = run(&spec(work.path(), "kill -TERM $$"));
    assert!(
        statuses.contains(&BackendStatus::Exited {
            exit_code: None,
            signal: Some(libc::SIGTERM)
        }),
        "{statuses:?}"
    );
    let (output, statuses) = run(&spec(
        work.path(),
        "(sleep 0.3; echo escaped > descendant) & exit 0",
    ));
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
        !work.path().join("descendant").exists(),
        "background process escaped PID namespace cleanup"
    );
}

#[test]
fn strict_runtime_does_not_reopen_denied_parent_via_rename() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let parent = work.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let key = parent.join("key");
    std::fs::write(&key, "secret").unwrap();
    let mut spec = spec(work.path(), "! mv parent moved && ! cat parent/key");
    spec.permissions.filesystem.rules.push(
        serde_json::from_value(serde_json::json!({"effect":"deny","path":key,"rights":["read"]}))
            .unwrap(),
    );
    spec.path_facts.push(fact(&key));
    let (output, statuses) = run(&spec);
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn strict_runtime_hides_helper_status_descriptors() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let script = r#"python3 - <<'PYTHON'
import os
# The trusted setup/wait helper is the PID namespace's init process.
try:
    os.listdir('/proc/1/fd')
except PermissionError:
    pass
else:
    raise SystemExit('target can inspect trusted helper descriptors')
PYTHON"#;
    let (output, statuses) = run(&spec(work.path(), script));
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn strict_runtime_closes_unlisted_inherited_descriptors() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let secret = tempfile::tempfile().unwrap();
    let fd = secret.as_raw_fd();
    let spec = spec(work.path(), &format!("test ! -e /proc/self/fd/{fd}"));
    let (output, statuses) = run_with(&spec, None, &[fd]);
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(statuses.contains(&BackendStatus::Started));
}

#[test]
fn strict_runtime_preserves_explicit_file_input_but_rejects_socket_input() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let mut input = tempfile::tempfile().unwrap();
    input.write_all(b"explicit input\n").unwrap();
    input.seek(SeekFrom::Start(0)).unwrap();
    let request = spec(work.path(), "cat");
    let (output, statuses) = run_with(&request, Some(Stdio::from(input)), &[]);
    assert!(output.status.success(), "{statuses:?}");
    assert_eq!(output.stdout, b"explicit input\n");

    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let socket: OwnedFd = socket.into();
    let (output, statuses) = run_with(&request, Some(Stdio::from(socket)), &[]);
    assert_eq!(output.status.code(), Some(125));
    assert!(!statuses.contains(&BackendStatus::Installed));
    assert!(statuses.iter().any(|status| matches!(status,
        BackendStatus::Failed { message } if message.contains("socket standard I/O")
    )));
}

#[test]
fn strict_runtime_rejects_directory_on_every_standard_descriptor() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let secret = tempfile::tempdir().unwrap();
    std::fs::write(secret.path().join("key"), "must stay outside").unwrap();
    for fd in 0..=2 {
        let script = format!(
            "printf ran > started; python3 -c 'import os; f=os.open(\"key\", os.O_RDONLY, dir_fd={fd}); open(\"leaked\", \"wb\").write(os.read(f, 100))'"
        );
        let mut request = spec(work.path(), &script);
        request.permissions.filesystem.rules.push(
            serde_json::from_value(serde_json::json!({
                "effect":"deny", "path":secret.path(), "rights":["read"]
            }))
            .unwrap(),
        );
        request.path_facts.push(fact(secret.path()));
        let mut stdio = [Stdio::null(), Stdio::piped(), Stdio::piped()];
        stdio[fd] = Stdio::from(File::open(secret.path()).unwrap());
        let (output, statuses) = run_with_stdio(&request, stdio, &[]);
        assert_eq!(output.status.code(), Some(125), "fd {fd}: {statuses:?}");
        assert!(
            matches!(statuses.as_slice(), [BackendStatus::Failed { message }] if message.contains("directory standard I/O")),
            "fd {fd}: {statuses:?}"
        );
        assert!(!work.path().join("started").exists());
        assert!(!work.path().join("leaked").exists());
    }
}

#[test]
fn strict_runtime_allows_connected_ipc_but_blocks_datagram_socketpairs() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let script = r#"python3 - <<'PYTHON'
import socket
for kind in (socket.SOCK_STREAM, socket.SOCK_SEQPACKET):
    for flags in (0, socket.SOCK_CLOEXEC, socket.SOCK_NONBLOCK,
                  socket.SOCK_CLOEXEC | socket.SOCK_NONBLOCK):
        left, right = socket.socketpair(socket.AF_UNIX, kind | flags)
        left.send(b'local')
        assert right.recv(5) == b'local'
        left.close()
        right.close()
try:
    socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
except PermissionError:
    pass
else:
    raise SystemExit('datagram socketpair can reach host Unix endpoints')
PYTHON"#;
    for mode in [
        asc_sandbox_types::NetworkMode::Off,
        asc_sandbox_types::NetworkMode::Full,
    ] {
        let mut spec = spec(work.path(), script);
        spec.permissions.network.mode = mode;
        let (output, statuses) = run(&spec);
        assert!(
            output.status.success(),
            "{} {statuses:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn strict_runtime_cannot_redirect_stream_pairs_to_a_host_unix_listener() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let listener = std::os::unix::net::UnixListener::bind(work.path().join("host.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let script = r#"python3 - <<'PYTHON'
import errno
import socket
left, right = socket.socketpair()
try:
    left.connect('host.sock')
except OSError as error:
    assert error.errno == errno.EISCONN
else:
    raise SystemExit('stream pair reconnected to a host Unix endpoint')
try:
    left.sendmsg([b'local'], [], 0, 'host.sock')
except OSError as error:
    assert error.errno == errno.EISCONN
else:
    assert right.recv(5) == b'local'
left.close()
right.close()
PYTHON"#;
    let mut spec = spec(work.path(), script);
    spec.permissions.network.mode = asc_sandbox_types::NetworkMode::Full;
    let (output, statuses) = run(&spec);
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
#[cfg(target_arch = "x86_64")]
fn strict_runtime_rejects_x32_syscall_abi_bypass() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let script = r#"python3 - <<'PYTHON'
import ctypes
import errno
import socket
libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long
for number in (39, 41):
    result = libc.syscall(ctypes.c_long(0x40000000 | number),
                          socket.AF_UNIX, socket.SOCK_STREAM, 0)
    assert result == -1 and ctypes.get_errno() == errno.EPERM, (result, ctypes.get_errno())
PYTHON"#;
    let mut spec = spec(work.path(), script);
    spec.permissions.network.mode = asc_sandbox_types::NetworkMode::Full;
    let (output, statuses) = run(&spec);
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn strict_runtime_filters_terminal_injection_ioctls_only() {
    if !enabled() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let script = r#"python3 - <<'PYTHON'
import ctypes
import errno
libc = ctypes.CDLL(None, use_errno=True)
for request in (0x5412, 0x541c):
    result = libc.ioctl(-1, ctypes.c_ulong(request), ctypes.c_void_p())
    assert result == -1 and ctypes.get_errno() == errno.EPERM, (request, ctypes.get_errno())
# TIOCGWINSZ must reach the kernel and fail for this invalid descriptor.
result = libc.ioctl(-1, ctypes.c_ulong(0x5413), ctypes.c_void_p())
assert result == -1 and ctypes.get_errno() == errno.EBADF, ctypes.get_errno()
PYTHON"#;
    let (output, statuses) = run(&spec(work.path(), script));
    assert!(
        output.status.success(),
        "{} {statuses:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}
