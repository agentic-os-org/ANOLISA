"""E2E tests for the agent-sec daemon systemd service unit.

Verifies that the installed systemd user unit is valid and that the daemon
can be managed via systemctl (start, health check, stop).  Tests are
skipped automatically when systemd is not available (e.g. containers without
a user session, or CI runners without dbus).
"""

import os
import shutil
import subprocess
import time
from pathlib import Path

import pytest

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_SYSTEMD_USER_UNIT = Path("/usr/lib/systemd/user/agent-sec-core.service")
_DAEMON_BIN = "/usr/bin/agent-sec-daemon"


def _systemd_available() -> bool:
    """Return True if a systemd --user session is live and managing units."""
    try:
        result = subprocess.run(
            ["systemctl", "--user", "is-system-running"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        # Only treat specific states as "available".  Exit code 1 covers
        # both "degraded" (usable) and "offline" (manager not running),
        # so we check stdout rather than the return code.
        state = result.stdout.strip()
        return state in ("running", "degraded", "starting")
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


def _systemctl_user(*args: str, timeout: int = 30) -> subprocess.CompletedProcess[str]:
    """Run a systemctl --user command and return the result."""
    return subprocess.run(
        ["systemctl", "--user", *args],
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def _wait_for_service_active(service: str, timeout_sec: int = 15) -> bool:
    """Poll until the service reaches 'active' state or timeout."""
    deadline = time.monotonic() + timeout_sec
    while time.monotonic() < deadline:
        result = _systemctl_user("is-active", service)
        if result.stdout.strip() == "active":
            return True
        time.sleep(0.5)
    return False


def _wait_for_service_stopped(service: str, timeout_sec: int = 15) -> bool:
    """Poll until the service is fully stopped or timeout."""
    deadline = time.monotonic() + timeout_sec
    while time.monotonic() < deadline:
        result = _systemctl_user("is-active", service)
        if result.stdout.strip() == "inactive":
            return True
        time.sleep(0.5)
    return False


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

requires_systemd = pytest.mark.skipif(
    not _systemd_available(),
    reason="systemd --user session not available in this environment",
)

requires_installed = pytest.mark.skipif(
    not _SYSTEMD_USER_UNIT.exists(),
    reason="agent-sec-core.service not installed (RPM mode required)",
)


@pytest.fixture
def daemon_bin() -> str:
    """Return the installed daemon binary path, or skip if absent."""
    path = shutil.which("agent-sec-daemon") or _DAEMON_BIN
    if not Path(path).exists():
        pytest.skip("agent-sec-daemon binary not installed")
    return path


@pytest.fixture
def managed_service(daemon_bin: str) -> str:
    """Start the systemd service, yield its name, then ensure cleanup."""
    service = "agent-sec-core.service"

    # Ensure a clean starting state
    _systemctl_user("stop", service)
    _wait_for_service_stopped(service)

    result = _systemctl_user("start", service)
    assert result.returncode == 0, f"systemctl --user start failed: {result.stderr}"
    yield service

    # Teardown: always stop the service
    _systemctl_user("stop", service)
    _wait_for_service_stopped(service)


# ---------------------------------------------------------------------------
# Tests: static validation (no systemd runtime required)
# ---------------------------------------------------------------------------


class TestServiceFileValidation:
    """Tests that validate the service file without starting the service."""

    def test_service_file_installed(self) -> None:
        """The systemd user unit must exist at the expected path."""
        if not _SYSTEMD_USER_UNIT.exists():
            pytest.skip("agent-sec-core.service not installed (RPM mode required)")
        assert _SYSTEMD_USER_UNIT.is_file()
        # Unit files should be readable by all users (mode 0644)
        mode = _SYSTEMD_USER_UNIT.stat().st_mode & 0o777
        assert mode == 0o644, f"Expected mode 0644, got {oct(mode)}"

    def test_service_file_syntax_valid(self) -> None:
        """systemd-analyze verify should pass on a well-formed unit file."""
        if not _SYSTEMD_USER_UNIT.exists():
            pytest.skip("agent-sec-core.service not installed (RPM mode required)")
        if not shutil.which("systemd-analyze"):
            pytest.skip("systemd-analyze not available")

        result = subprocess.run(
            ["systemd-analyze", "verify", str(_SYSTEMD_USER_UNIT)],
            capture_output=True,
            text=True,
            timeout=15,
        )
        # systemd-analyze verify exits 0 on success; non-zero on errors
        assert result.returncode == 0, (
            f"systemd-analyze verify failed:\nstdout: {result.stdout}\n"
            f"stderr: {result.stderr}"
        )

    def test_daemon_binary_help(self, daemon_bin: str) -> None:
        """agent-sec-daemon --help should succeed (basic RPM smoke test)."""
        result = subprocess.run(
            [daemon_bin, "--help"],
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert (
            result.returncode == 0
        ), f"agent-sec-daemon --help failed: {result.stderr}"
        assert (
            "agent-sec-daemon" in result.stdout.lower()
            or "usage" in result.stdout.lower()
        )


# ---------------------------------------------------------------------------
# Tests: systemd-managed lifecycle (requires running systemd user session)
# ---------------------------------------------------------------------------


@requires_systemd
@requires_installed
class TestSystemdManagedDaemon:
    """Tests that exercise the daemon via systemctl --user."""

    def test_service_starts_and_becomes_active(self, managed_service: str) -> None:
        """The service should reach 'active' state after systemctl start."""
        assert _wait_for_service_active(
            managed_service
        ), f"Service {managed_service} did not become active within timeout"

    def test_service_status_reports_running(self, managed_service: str) -> None:
        """systemctl status should show the service as running."""
        if not _wait_for_service_active(managed_service):
            pytest.skip("Service did not reach active state")

        result = _systemctl_user("status", managed_service)
        # Active services show 'active (running)' in status output
        assert "active" in result.stdout.lower()
        assert "running" in result.stdout.lower()

    def test_service_stops_cleanly(self, managed_service: str) -> None:
        """The service should stop cleanly when systemctl stop is issued."""
        if not _wait_for_service_active(managed_service):
            pytest.skip("Service did not reach active state")

        result = _systemctl_user("stop", managed_service)
        assert result.returncode == 0, f"systemctl --user stop failed: {result.stderr}"
        assert _wait_for_service_stopped(
            managed_service
        ), "Service did not stop within timeout"

    def test_service_restarts_on_failure(self, managed_service: str) -> None:
        """Restart=on-failure should bring the service back after a crash."""
        if not _wait_for_service_active(managed_service):
            pytest.skip("Service did not reach active state")

        # Simulate a crash by sending SIGKILL to the daemon process
        result = _systemctl_user("show", managed_service, "--property=MainPID")
        pid_line = result.stdout.strip()
        if "=" not in pid_line:
            pytest.skip("Could not determine MainPID")
        pid = int(pid_line.split("=", 1)[1])
        if pid == 0:
            pytest.skip("MainPID is 0, service may not have started")

        os.kill(pid, 9)

        # Service should restart automatically and become active again
        assert _wait_for_service_active(
            managed_service, timeout_sec=20
        ), "Service did not auto-restart after SIGKILL within 20s"
