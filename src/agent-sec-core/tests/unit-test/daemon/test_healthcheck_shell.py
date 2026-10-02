"""Tests for the lightweight daemon health probe."""

import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[3]
_PROBE = _REPO_ROOT / "tools" / "agent-sec-daemon-health.sh"
_REQUIRED_COMMANDS = ("curl", "jq")

pytestmark = pytest.mark.skipif(
    any(shutil.which(command) is None for command in _REQUIRED_COMMANDS),
    reason="shell health probe runtime commands are unavailable",
)


def _daemon_environment(tmp_path: Path) -> tuple[dict[str, str], Path]:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    environment = os.environ.copy()
    environment.update(
        {
            "AGENT_SEC_DAEMON_SOCKET": str(socket_path),
            "AGENT_SEC_DATA_DIR": str(tmp_path / "events"),
            "XDG_CONFIG_HOME": str(tmp_path / "config"),
            "XDG_DATA_HOME": str(tmp_path / "data"),
        }
    )
    return environment, socket_path


def _start_daemon_at(
    environment: dict[str, str], socket_path: Path
) -> subprocess.Popen[str]:
    process = subprocess.Popen(
        [sys.executable, "-m", "agent_sec_cli.daemon.server", "serve"],
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if socket_path.is_socket():
            return process
        if process.poll() is not None:
            _stdout, stderr = process.communicate()
            pytest.fail(f"daemon exited before creating its socket: {stderr}")
        time.sleep(0.02)

    process.terminate()
    _stdout, stderr = process.communicate(timeout=5)
    pytest.fail(f"daemon did not create its socket: {stderr}")


def _start_daemon(tmp_path: Path) -> tuple[subprocess.Popen[str], dict[str, str], Path]:
    environment, socket_path = _daemon_environment(tmp_path)
    process = _start_daemon_at(environment, socket_path)
    return process, environment, socket_path


def _stop_process(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.communicate(timeout=5)


def test_shell_probe_accepts_the_real_daemon(tmp_path: Path) -> None:
    daemon, environment, _socket_path = _start_daemon(tmp_path)
    try:
        result = subprocess.run(
            [str(_PROBE)],
            env=environment,
            capture_output=True,
            text=True,
            timeout=3,
            check=False,
        )
    finally:
        _stop_process(daemon)

    assert result.returncode == 0, result.stderr
    assert result.stdout == ""


def test_shell_probe_uses_daemon_xdg_socket_default(tmp_path: Path) -> None:
    environment, _explicit_socket = _daemon_environment(tmp_path)
    environment.pop("AGENT_SEC_DAEMON_SOCKET")
    environment["XDG_RUNTIME_DIR"] = str(tmp_path / "xdg-runtime")
    socket_path = (
        Path(environment["XDG_RUNTIME_DIR"]) / "agent-sec-core" / "daemon.sock"
    )
    daemon = _start_daemon_at(environment, socket_path)
    try:
        result = subprocess.run(
            [str(_PROBE)],
            env=environment,
            capture_output=True,
            text=True,
            timeout=3,
            check=False,
        )
    finally:
        _stop_process(daemon)

    assert result.returncode == 0, result.stderr
    assert result.stdout == ""


@pytest.mark.parametrize("option", ["--require-model", "--require-same-cgroup"])
def test_shell_probe_rejects_obsolete_options(option: str) -> None:
    result = subprocess.run(
        [str(_PROBE), option],
        capture_output=True,
        text=True,
        timeout=3,
        check=False,
    )

    assert result.returncode == 1
    assert f"unknown argument: {option}" in result.stderr


def test_shell_probe_rejects_unhealthy_daemon_status(tmp_path: Path) -> None:
    environment, socket_path = _daemon_environment(tmp_path)
    fake_server_code = r"""
import json
import socket
import sys
from pathlib import Path

socket_path = Path(sys.argv[1])
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
socket_path.parent.mkdir(parents=True, exist_ok=True)
server.bind(str(socket_path))
server.listen()
print("ready", flush=True)
connection, _ = server.accept()
with connection:
    request = b""
    while b"\n" not in request:
        chunk = connection.recv(4096)
        if not chunk:
            break
        request += chunk
    response = {
        "request_id": "test",
        "ok": True,
        "data": {"status": "stopping"},
    }
    connection.sendall(json.dumps(response).encode("utf-8") + b"\n")
"""
    fake_server = subprocess.Popen(
        [sys.executable, "-c", fake_server_code, str(socket_path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    try:
        assert fake_server.stdout is not None
        ready = fake_server.stdout.readline().strip()
        if ready != "ready":
            _stdout, stderr = fake_server.communicate(timeout=3)
            pytest.fail(f"fake health server failed to start: {stderr}")
        result = subprocess.run(
            [str(_PROBE)],
            env=environment,
            capture_output=True,
            text=True,
            timeout=3,
            check=False,
        )
    finally:
        _stop_process(fake_server)

    assert result.returncode == 1
    assert "daemon returned an invalid or unhealthy response" in result.stderr


def test_shell_probe_short_circuits_when_socket_is_missing(tmp_path: Path) -> None:
    environment, socket_path = _daemon_environment(tmp_path)

    result = subprocess.run(
        [str(_PROBE)],
        env=environment,
        capture_output=True,
        text=True,
        timeout=3,
        check=False,
    )

    assert result.returncode == 1
    assert f"socket is missing or is not a Unix socket: {socket_path}" in result.stderr
