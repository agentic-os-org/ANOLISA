"""Execution-path fixtures for prompt-scanner e2e tests.

The CLI ``scan-prompt`` command routes through ``security_middleware.invoke``
in-process; it no longer hops through the daemon socket (the daemon stopped
serving ``prompt_scan`` in the scan-prompt-to-rust refactor). The suite
therefore runs against a single direct-middleware execution path.
"""

import os
from dataclasses import dataclass
from pathlib import Path

import pytest

# The env-var names the CLI reads are part of its black-box contract, so they
# are spelled out here rather than imported: the RPM e2e runs on a system
# interpreter that cannot import the package at all (it lives in a private
# site-packages together with its third-party deps).  ``_assert_env_contract``
# below still cross-checks these literals against the package constants
# whenever the package *is* importable, so a rename cannot drift silently.
SOCKET_ENV = "AGENT_SEC_DAEMON_SOCKET"
DAEMON_DISABLED_ENV = "AGENT_SEC_DAEMON_DISABLED"
TELEMETRY_LOG_PATH_ENV = "AGENT_SEC_TELEMETRY_LOG_PATH"
DATA_DIR_ENV = "AGENT_SEC_DATA_DIR"


def _assert_env_contract() -> None:
    """Fail fast when the CLI renamed an env var this suite drives.

    No-op where the package is not importable (installed-artifact runs); the
    dev and CI coverage runs import it and therefore own the drift check.
    """
    try:
        from agent_sec_cli.daemon import env  # noqa: PLC0415 - optional import
        from agent_sec_cli.telemetry import config  # noqa: PLC0415
    except ImportError:
        return
    assert (env.SOCKET_ENV, env.DAEMON_DISABLED_ENV) == (
        SOCKET_ENV,
        DAEMON_DISABLED_ENV,
    ), "daemon env-var contract drifted from this suite"
    assert (
        config.TELEMETRY_LOG_PATH_ENV == TELEMETRY_LOG_PATH_ENV
    ), "telemetry env-var contract drifted from this suite"


@dataclass(frozen=True)
class PromptScanExecutionContext:
    data_dir: Path
    telemetry_path: Path


@pytest.fixture(scope="module", autouse=True)
def prompt_scan_execution_path(
    tmp_path_factory: pytest.TempPathFactory,
) -> PromptScanExecutionContext:
    """Configure the CLI to run scan-prompt through the local middleware.

    Forces the daemon-disabled path so ``agent-sec-cli scan-prompt`` invokes
    ``security_middleware.invoke("prompt_scan", ...)`` directly, without
    attempting to reach a daemon socket.
    """
    tmp_path = tmp_path_factory.mktemp("prompt_scan_middleware")
    _assert_env_contract()
    data_dir = tmp_path / "data"
    telemetry_path = data_dir / "telemetry.jsonl"
    telemetry_path.parent.mkdir(parents=True, exist_ok=True)
    telemetry_path.write_text("", encoding="utf-8")

    saved_env = {
        SOCKET_ENV: os.environ.get(SOCKET_ENV),
        DAEMON_DISABLED_ENV: os.environ.get(DAEMON_DISABLED_ENV),
        DATA_DIR_ENV: os.environ.get(DATA_DIR_ENV),
        TELEMETRY_LOG_PATH_ENV: os.environ.get(TELEMETRY_LOG_PATH_ENV),
    }

    os.environ.pop(SOCKET_ENV, None)
    os.environ[DAEMON_DISABLED_ENV] = "1"
    os.environ[DATA_DIR_ENV] = str(data_dir)
    os.environ[TELEMETRY_LOG_PATH_ENV] = str(telemetry_path)

    yield PromptScanExecutionContext(
        data_dir=data_dir,
        telemetry_path=telemetry_path,
    )

    for key, value in saved_env.items():
        if value is None:
            os.environ.pop(key, None)
        else:
            os.environ[key] = value
