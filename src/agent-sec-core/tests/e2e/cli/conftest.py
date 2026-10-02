"""Shared fixtures and helpers for CLI e2e tests.

This module provides common test infrastructure used across all CLI e2e
test files.  It isolates each test with a fresh SQLite database and
provides helper functions for invoking the CLI and parsing outputs.
"""

import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from typing import Any

import pytest

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# Use the same Python interpreter that runs pytest to invoke the CLI module.
# This works in all environments: local venv, CI (uv run), tox, etc.
PYTHON = sys.executable

# ---------------------------------------------------------------------------
# CLI resolution — supports both installed and dev-mode environments
# ---------------------------------------------------------------------------

_CLI_BIN = shutil.which("agent-sec-cli")
_CLI_MODE = "binary" if _CLI_BIN else "python -m"

# Check if loongshield is available
LOONGSHIELD_AVAILABLE = shutil.which("loongshield") is not None


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def isolated_data_dir(tmp_path):
    """Create a function-scoped temp directory for security event data.

    Each test gets a completely fresh SQLite DB — no cross-test pollution,
    no ordering dependency, no cascade failures.

    Sets AGENT_SEC_DATA_DIR env var to isolate the SQLite store.
    """
    data_dir = tmp_path / "agent-sec-e2e"
    data_dir.mkdir()
    os.environ["AGENT_SEC_DATA_DIR"] = str(data_dir)
    yield
    os.environ.pop("AGENT_SEC_DATA_DIR", None)


# ---------------------------------------------------------------------------
# Helper functions
# ---------------------------------------------------------------------------


def run_cli(
    *args: str,
    check: bool = False,
    input_text: str | None = None,
) -> subprocess.CompletedProcess:
    """Run agent-sec-cli command and return CompletedProcess.

    Automatically detects whether to use the installed binary or
    `python -m agent_sec_cli` based on environment.

    Args:
        *args: CLI arguments to pass to the command.
        check: If True, raise CalledProcessError on non-zero exit code.
        input_text: Optional text to pass to the command's stdin.

    Returns:
        subprocess.CompletedProcess with stdout, stderr, and returncode.
    """
    if _CLI_MODE == "binary":
        cmd = [_CLI_BIN, *args]
    else:
        cmd = [PYTHON, "-m", "agent_sec_cli", *args]

    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        input=input_text,
        check=check,
        timeout=30,
        env=os.environ.copy(),  # inherits AGENT_SEC_DATA_DIR
    )


def iso_now() -> str:
    """Return current UTC time as ISO-8601 string."""
    return datetime.now(timezone.utc).isoformat()


def parse_events_json(stdout: str) -> list[dict[str, Any]]:
    """Parse events JSON output and return list of event dicts.

    Args:
        stdout: CLI stdout output (JSON array).

    Returns:
        List of event dictionaries.
    """
    events = json.loads(stdout)
    assert isinstance(events, list), f"Expected JSON array, got {type(events)}"
    return events


def parse_table_output(stdout: str) -> list[str]:
    """Parse table format output and return non-empty lines.

    Args:
        stdout: CLI stdout output (table format).

    Returns:
        List of non-empty lines from the table.
    """
    return [line for line in stdout.strip().split("\n") if line.strip()]


def assert_event_structure(event: dict[str, Any]) -> None:
    """Validate that an event dict has all required fields.

    Args:
        event: Event dictionary from JSON output.

    Raises:
        AssertionError if any required field is missing.
    """
    required_fields = [
        "event_id",
        "event_type",
        "category",
        "result",
        "timestamp",
        "trace_id",
        "details",
    ]
    for field in required_fields:
        assert field in event, f"Missing required field: {field}"


# ---------------------------------------------------------------------------
# Loongshield availability check
# ---------------------------------------------------------------------------


def require_loongshield():
    """Skip test if loongshield is not installed.

    Use this at the beginning of tests that require actual loongshield
    execution to produce meaningful result data (passed/failed/total fields).

    Example:
        def test_compliance_with_real_data():
            require_loongshield()
            # ... test code that needs loongshield output ...
    """
    if not LOONGSHIELD_AVAILABLE:
        pytest.skip("loongshield not installed")
