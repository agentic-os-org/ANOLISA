"""Shared fixtures for the V2 policy-CLI end-to-end suite.

These tests drive the real ``agent-sec-cli`` and ``agent-sec-daemon`` binaries over a
Unix domain socket, with either V2 build outputs or RPM-installed binaries on ``PATH``. A missing binary fails the run
instead of skipping it: skipping would let a broken package slip through the
gate silently.

The suite lives under ``tests/v2/`` rather than ``tests/e2e/`` so the V1
``test-e2e-rpm`` target (which globs ``tests/e2e/``) never collects it.
"""

import json
import os
import shutil
import signal
import socket
import subprocess
import time
from pathlib import Path

import pytest

CLI_BIN = "agent-sec-cli"
DAEMON_BIN = "agent-sec-daemon"

# The socket appears shortly after the daemon binds; five seconds is generous
# for a cold binary start under container I/O without masking a real hang.
_SOCKET_WAIT_SECONDS = 5.0
# SIGTERM is cooperative: the daemon drains in-flight work before unlinking the
# socket, so allow a small margin beyond the daemon's own drain timeout.
_SHUTDOWN_WAIT_SECONDS = 5.0
_POLL_INTERVAL = 0.02


def _require(binary: str) -> str:
    """Resolves an executable on PATH, failing the test if it is absent."""
    resolved = shutil.which(binary)
    if resolved is None:
        pytest.fail(
            f"{binary} not found on PATH; install the agent-sec-core V2 RPM "
            "before running the V2 e2e suite"
        )
    return resolved


def run_agent_sec_cli(
    *args: str,
    timeout: float = 30.0,
    input_text: str | None = None,
) -> subprocess.CompletedProcess:
    """Runs ``agent-sec-cli`` with the given argv and no implicit ``--socket``.

    Used by cases that must not reach a daemon (``--help`` / ``--version`` and
    usage errors), where injecting a socket would change the parse outcome.
    """
    return subprocess.run(
        [_require(CLI_BIN), *args],
        capture_output=True,
        text=True,
        input=input_text,
        timeout=timeout,
        check=False,
    )


class DaemonHandle:
    """A running ``agent-sec-daemon`` process bound to a known socket path."""

    def __init__(self, process: subprocess.Popen, socket_path: Path) -> None:
        self.process = process
        self.socket_path = socket_path

    def cli(self, *args: str, timeout: float = 30.0) -> subprocess.CompletedProcess:
        """Invokes ``agent-sec-cli --socket <this daemon> <args>``."""
        return subprocess.run(
            [_require(CLI_BIN), "--socket", str(self.socket_path), *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )

    def request(self, *args: str, timeout: float = 30.0) -> dict:
        """Runs a CLI command expected to succeed and returns parsed stdout JSON."""
        result = self.cli(*args, timeout=timeout)
        assert (
            result.returncode == 0
        ), f"agent-sec-cli {args} failed (rc={result.returncode}): {result.stderr}"
        assert result.stderr == "", f"unexpected stderr for {args}: {result.stderr}"
        return json.loads(result.stdout)


def _start_daemon(socket_path: Path, admin_uids: list[int]) -> subprocess.Popen:
    """Starts a foreground daemon and waits for a complete protocol response."""
    argv = [_require(DAEMON_BIN), "--socket", str(socket_path)]
    for uid in admin_uids:
        argv += ["--policy-admin-uid", str(uid)]
    process = subprocess.Popen(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    deadline = time.monotonic() + _SOCKET_WAIT_SECONDS
    while time.monotonic() < deadline:
        if socket_path.exists():
            try:
                with socket.socket(socket.AF_UNIX) as probe:
                    probe.settimeout(0.2)
                    probe.connect(str(socket_path))
                    probe.sendall(
                        b'{"method":"policy.templates.list","params":{"limit":1,"offset":0}}\n'
                    )
                    if probe.recv(4096).endswith(b"\n"):
                        return process
            except (OSError, TimeoutError):
                pass
        if process.poll() is not None:
            _, stderr = process.communicate()
            raise AssertionError(
                f"agent-sec-daemon exited early (rc={process.returncode}): {stderr}"
            )
        time.sleep(_POLL_INTERVAL)
    stderr = _terminate(process)
    raise AssertionError(f"agent-sec-daemon did not create {socket_path}: {stderr}")


def _terminate(process: subprocess.Popen) -> str:
    """Sends SIGTERM, waits for cooperative exit, returns captured stderr."""
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
    try:
        _, stderr = process.communicate(timeout=_SHUTDOWN_WAIT_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        _, stderr = process.communicate()
        raise AssertionError("agent-sec-daemon did not exit within the shutdown window")
    return stderr or ""


@pytest.fixture
def cli():
    """Returns a runner for ``agent-sec-cli`` with no implicit ``--socket``."""
    return run_agent_sec_cli


@pytest.fixture
def daemon_bin() -> str:
    """Resolves ``agent-sec-daemon``, failing if the RPM is not installed."""
    return _require(DAEMON_BIN)


@pytest.fixture
def start_daemon(tmp_path: Path):
    """Returns a factory that starts a daemon and tears it down after the test.

    The factory does not assert on shutdown; tests that care about cooperative
    exit and socket cleanup drive SIGTERM themselves."""
    started: list[subprocess.Popen] = []

    def _factory(
        admin_uids: list[int] | None = None, name: str = "daemon.sock"
    ) -> DaemonHandle:
        socket_path = tmp_path / name
        uids = admin_uids if admin_uids is not None else [os.getuid()]
        process = _start_daemon(socket_path, uids)
        started.append(process)
        return DaemonHandle(process, socket_path)

    yield _factory
    for process in started:
        _terminate(process)


@pytest.fixture
def daemon(tmp_path: Path):
    """Yields an authorized daemon: the current uid is a policy administrator.

    Teardown asserts cooperative shutdown — the process must exit on SIGTERM and
    unlink its own socket.
    """
    socket_path = tmp_path / "daemon.sock"
    process = _start_daemon(socket_path, [os.getuid()])
    handle = DaemonHandle(process, socket_path)
    try:
        yield handle
    finally:
        _terminate(process)
        assert process.returncode is not None
        assert not socket_path.exists(), "daemon left its socket behind on shutdown"


@pytest.fixture
def unauthorized_daemon(tmp_path: Path):
    """Yields a daemon whose admin set excludes the caller.

    Only root is authorized, so a non-root caller acts as a plain local user.
    Root is unconditionally authorized by design, so this scenario is
    unreachable when the suite itself runs as root.
    """
    if os.getuid() == 0:
        pytest.skip(
            "root is unconditionally authorized; cannot exercise denial as root"
        )
    socket_path = tmp_path / "daemon.sock"
    process = _start_daemon(socket_path, [])
    handle = DaemonHandle(process, socket_path)
    try:
        yield handle
    finally:
        _terminate(process)
