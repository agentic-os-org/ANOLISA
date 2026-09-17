"""Execution-path fixtures for prompt-scanner e2e tests.

The suite is agnostic to how the resolved ``agent-sec-cli`` executes a scan:
in-process — where security events and telemetry land in
``AGENT_SEC_DATA_DIR`` / the telemetry log — or as a thin daemon client that
writes nothing itself.  The fixture therefore only isolates filesystem
writes into a temp directory and leaves daemon discovery
(``AGENT_SEC_DAEMON_SOCKET``) untouched: lanes that provide a daemon rely on
it, and a daemon-backed CLI has no in-process mode to force.
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
TELEMETRY_LOG_PATH_ENV = "AGENT_SEC_TELEMETRY_LOG_PATH"
DATA_DIR_ENV = "AGENT_SEC_DATA_DIR"


def _assert_env_contract() -> None:
    """Fail fast when the CLI renamed an env var this suite drives.

    No-op where the package is not importable (installed-artifact runs); the
    dev and CI coverage runs import it and therefore own the drift check.
    Only the telemetry var has a package constant to compare against — the
    security-events store reads ``AGENT_SEC_DATA_DIR`` as a bare literal, so
    no cross-check exists for it.
    """
    try:
        from agent_sec_cli.telemetry import config  # noqa: PLC0415
    except ImportError:
        return
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
    """Isolate the CLI's filesystem writes for the whole module.

    ``AGENT_SEC_DATA_DIR`` and the telemetry log path point into a temp
    directory so in-process executions cannot pollute real user data.  A
    thin-client CLI ignores both and writes nothing itself; the daemon it
    talks to keeps its own configured paths.
    """
    tmp_path = tmp_path_factory.mktemp("prompt_scan_execution")
    _assert_env_contract()
    data_dir = tmp_path / "data"
    telemetry_path = data_dir / "telemetry.jsonl"
    telemetry_path.parent.mkdir(parents=True, exist_ok=True)
    telemetry_path.write_text("", encoding="utf-8")

    saved_env = {
        DATA_DIR_ENV: os.environ.get(DATA_DIR_ENV),
        TELEMETRY_LOG_PATH_ENV: os.environ.get(TELEMETRY_LOG_PATH_ENV),
    }
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
