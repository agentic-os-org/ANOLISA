"""DPROC process lifecycle and secured runtime namespace fixtures.

Application persistence and health endpoints are separate acceptance gates.
"""

import json
import os
import pwd
import signal
import socket
import stat
import subprocess
import tempfile
import time
from pathlib import Path

import pytest


def test_bound_socket_allows_ordinary_users(daemon):
    mode = stat.S_IMODE(os.stat(daemon.socket_path).st_mode)
    assert mode == 0o666, f"socket mode is {oct(mode)}, expected 0o666"


@pytest.mark.skipif(os.geteuid() != 0, reason="cross-UID UDS test requires root")
def test_ordinary_uid_connects_but_cannot_administer_or_replace_socket(daemon_bin):
    client = pwd.getpwnam("nobody")
    with tempfile.TemporaryDirectory(prefix="asc-public-uds-") as directory:
        runtime = Path(directory)
        runtime.chmod(0o755)
        endpoint = runtime / "daemon.sock"
        process = subprocess.Popen([daemon_bin, "serve", "--socket", str(endpoint)])
        try:
            deadline = time.monotonic() + 5
            while (
                not endpoint.exists() or stat.S_IMODE(endpoint.stat().st_mode) != 0o666
            ):
                assert process.poll() is None, "daemon exited during startup"
                assert time.monotonic() < deadline, "daemon did not bind"
                time.sleep(0.02)
            result = subprocess.run(
                [
                    "/usr/bin/python3",
                    "-c",
                    """import json, os, socket, sys
endpoint = sys.argv[1]
with socket.socket(socket.AF_UNIX) as stream:
    stream.settimeout(5)
    stream.connect(endpoint)
    stream.sendall(b'{"method":"policy.templates.list","params":{"limit":1,"offset":0}}\\n')
    data = b""
    while not data.endswith(b"\\n"):
        chunk = stream.recv(4096)
        assert chunk and len(data) + len(chunk) < 65536
        data += chunk
    response = json.loads(data)
    assert set(response) == {"requestId", "error"}, response
    assert response["error"]["code"] == "permission_denied", response
try:
    os.unlink(endpoint)
except PermissionError:
    pass
else:
    raise AssertionError("ordinary UID could unlink the socket")
try:
    with open(os.path.join(os.path.dirname(endpoint), "daemon.lock"), "rb"):
        pass
except PermissionError:
    pass
else:
    raise AssertionError("ordinary UID could read the private lock")
""",
                    str(endpoint),
                ],
                user=client.pw_uid,
                group=client.pw_gid,
                extra_groups=[],
                check=False,
                capture_output=True,
                text=True,
                timeout=10,
            )
            assert result.returncode == 0, result.stderr
        finally:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
                raise


def test_sigterm_cooperatively_exits_and_removes_socket(start_daemon):
    handle = start_daemon()
    socket_path = handle.socket_path
    assert socket_path.exists()
    socket_inode = os.stat(socket_path).st_ino

    handle.process.send_signal(signal.SIGTERM)
    handle.process.communicate(timeout=5.0)

    assert handle.process.returncode == 0
    # The daemon owns its socket and unlinks exactly the inode it bound.
    assert not socket_path.exists(), f"socket inode {socket_inode} was left behind"


def test_relative_socket_fails(daemon_bin):
    result = subprocess.run(
        [daemon_bin, "--socket", "relative.sock"],
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )
    assert result.returncode == 2
    assert "absolute" in result.stderr


def run_rejected(daemon_bin: str, path: Path) -> str:
    result = subprocess.run(
        [daemon_bin, "--socket", str(path)],
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )
    assert result.returncode != 0
    return result.stderr


def test_dproc_014_second_socket_in_same_runtime_is_rejected(daemon, daemon_bin):
    lock = daemon.socket_path.parent / "daemon.lock"
    before = lock.stat().st_ino
    stderr = run_rejected(daemon_bin, daemon.socket_path.with_name("second.sock"))
    assert "already running" in stderr
    assert lock.stat().st_ino == before
    assert daemon.request("policy", "list")["total"] == 0


def test_dproc_003_sigkill_restart_reuses_lock_and_reclaims_socket(start_daemon):
    first = start_daemon()
    lock = first.socket_path.parent / "daemon.lock"
    inode = lock.stat().st_ino
    first.process.kill()
    first.process.communicate(timeout=5)
    assert first.socket_path.exists()
    # The factory waits for bind; remove no runtime files before restarting.
    second = start_daemon()
    assert second.process.pid != first.process.pid
    assert lock.stat().st_ino == inode
    assert second.request("policy", "list")["total"] == 0


@pytest.mark.parametrize("signal_number", [signal.SIGTERM, signal.SIGINT])
def test_dproc_003_stop_with_incomplete_request(start_daemon, signal_number):
    daemon = start_daemon()
    with socket.socket(socket.AF_UNIX) as stream:
        stream.connect(str(daemon.socket_path))
        stream.sendall(b'{"method":')
        before = time.monotonic()
        daemon.process.send_signal(signal_number)
        daemon.process.communicate(timeout=5)
        assert time.monotonic() - before < 5
        assert daemon.process.returncode == 0
        assert not daemon.socket_path.exists()
        stream.settimeout(1)
        response = b""
        try:
            while chunk := stream.recv(4096):
                response += chunk
                assert len(response) < 65536
        except ConnectionResetError:
            pass
        if response:
            # Accept may race shutdown: a queued connection gets a bounded rejection.
            payload = json.loads(response)
            assert set(payload) == {"requestId", "error"}
            assert isinstance(payload["requestId"], str) and payload["requestId"]
            assert payload["error"]["code"] == "unavailable"
            assert payload["error"]["message"] == "daemon is shutting down"


def test_sighup_does_not_reload_or_stop(daemon):
    daemon.process.send_signal(signal.SIGHUP)
    assert daemon.request("policy", "list")["total"] == 0
    assert daemon.process.poll() is None


@pytest.mark.parametrize("kind", ["symlink", "hardlink", "fifo", "directory", "mode"])
def test_dproc_012_unsafe_lock_is_rejected(daemon_bin, tmp_path, kind):
    lock = tmp_path / "daemon.lock"
    original = tmp_path / "original"
    original.write_text("must survive")
    original.chmod(0o600)
    if kind == "symlink":
        lock.symlink_to(original)
    elif kind == "hardlink":
        os.link(original, lock)
    elif kind == "fifo":
        os.mkfifo(lock, 0o600)
    elif kind == "directory":
        lock.mkdir()
    else:
        lock.write_text("unsafe mode")
        lock.chmod(0o666)
    run_rejected(daemon_bin, tmp_path / "daemon.sock")
    assert original.read_text() == "must survive"
    assert not (tmp_path / "daemon.sock").exists()


@pytest.mark.parametrize("mode", [0o1700, 0o2700, 0o4700, 0o2750, 0o2755])
def test_dproc_012_directory_special_bits_allow_safe_access(
    tmp_path, start_daemon, mode
):
    runtime = tmp_path / "runtime"
    runtime.mkdir(mode=0o700)
    runtime.chmod(mode)
    assert stat.S_IMODE(runtime.stat().st_mode) == mode
    daemon = start_daemon(name="runtime/daemon.sock")
    assert daemon.request("policy", "list")["total"] == 0


@pytest.mark.parametrize(
    "kind", ["symlink", "writable", "world_writable", "setgid_writable", "ancestor"]
)
def test_dproc_012_unsafe_directory_is_rejected(daemon_bin, tmp_path, kind):
    runtime = tmp_path / "runtime"
    runtime.mkdir(mode=0o700)
    if kind == "symlink":
        link = tmp_path / "link"
        link.symlink_to(runtime, target_is_directory=True)
        path = link / "daemon.sock"
    elif kind == "ancestor":
        runtime.chmod(0o777)
        child = runtime / "private"
        child.mkdir(mode=0o700)
        path = child / "daemon.sock"
    elif kind == "setgid_writable":
        runtime.chmod(0o2775)
        path = runtime / "daemon.sock"
    else:
        runtime.chmod(0o770 if kind == "writable" else 0o777)
        path = runtime / "daemon.sock"
    run_rejected(daemon_bin, path)
    assert not (path.parent / "daemon.lock").exists()


@pytest.mark.parametrize("kind", ["file", "symlink", "live", "mode"])
def test_dproc_012_existing_socket_is_not_blindly_unlinked(daemon_bin, tmp_path, kind):
    path = tmp_path / "daemon.sock"
    with socket.socket(socket.AF_UNIX) as listener:
        if kind == "file":
            path.write_text("keep")
        elif kind == "symlink":
            path.symlink_to(tmp_path / "missing")
        else:
            listener.bind(str(path))
            path.chmod(0o666 if kind == "live" else 0o667)
            listener.listen(1)
        inode = path.lstat().st_ino
        run_rejected(daemon_bin, path)
        assert path.lstat().st_ino == inode


@pytest.mark.parametrize("explicit", [False, True])
def test_socket_environment_and_explicit_precedence(daemon_bin, tmp_path, explicit):
    endpoint = tmp_path / "env.sock"
    env = dict(
        os.environ,
        AGENT_SEC_DAEMON_SOCKET="relative.sock" if explicit else str(endpoint),
    )
    socket_args = ["--socket", str(endpoint)] if explicit else []
    process = subprocess.Popen(
        [daemon_bin, "serve", *socket_args, "--policy-admin-uid", str(os.getuid())],
        env=env,
    )
    try:
        deadline = time.monotonic() + 5
        while not endpoint.exists():
            assert process.poll() is None
            assert time.monotonic() < deadline
            time.sleep(0.02)
        result = subprocess.run(
            ["agent-sec-cli", *socket_args, "policy", "list"],
            env=env,
            capture_output=True,
            text=True,
            check=False,
            timeout=5,
        )
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)["total"] == 0
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
            raise


@pytest.mark.parametrize(
    "binary,args",
    [("agent-sec-cli", ["policy", "list"]), ("agent-sec-daemon", ["serve"])],
)
def test_relative_socket_environment_is_rejected(binary, args):
    result = subprocess.run(
        [binary, *args],
        env=dict(os.environ, AGENT_SEC_DAEMON_SOCKET="relative.sock"),
        capture_output=True,
        text=True,
        check=False,
        timeout=5,
    )
    assert result.returncode == 2
    assert "absolute" in result.stderr
