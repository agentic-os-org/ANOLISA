"""Telemetry path and component metadata."""

import errno
import os
from os import lstat as _lstat
from os import stat as _stat
from pathlib import Path

from agent_sec_cli import __version__

COMPONENT_NAME = "agent-sec-core"
COMPONENT_AGENT_NAME = ""
DEFAULT_TELEMETRY_LOG_PATH = "/var/log/anolisa/sls/ops/agent-sec-core.jsonl"
TELEMETRY_LOG_PATH_ENV = "AGENT_SEC_TELEMETRY_LOG_PATH"
TELEMETRY_DISABLED_SENTINEL = "/etc/anolisa/.telemetry_disabled"
TELEMETRY_LINKED_SENTINEL = "/etc/anolisa/.telemetry_linked"


def get_telemetry_log_path() -> Path:
    """Return the configured Agentic OS telemetry JSONL path."""
    override = os.environ.get(TELEMETRY_LOG_PATH_ENV)
    if override:
        return Path(override).expanduser()
    return Path(DEFAULT_TELEMETRY_LOG_PATH)


def telemetry_log_path_exists() -> bool:
    """Return whether the configured telemetry JSONL file exists."""
    return get_telemetry_log_path().is_file()


def is_l1_telemetry_allowed() -> bool:
    """Return whether anonymous L1 telemetry is allowed for this write.

    The disabled sentinel is checked on every call.  Any stat failure other
    than an absent path fails closed so a permissions or filesystem problem
    cannot accidentally enable telemetry.
    """
    try:
        _lstat(TELEMETRY_DISABLED_SENTINEL)
    except OSError as exc:
        return exc.errno == errno.ENOENT
    return False


def is_l3_telemetry_linked() -> bool:
    """Return whether the current installation is linked for approved L3 data."""
    try:
        _stat(TELEMETRY_LINKED_SENTINEL)
    except OSError:
        return False
    return True


def get_component_fields() -> dict[str, str]:
    """Return fixed Agentic OS component fields for telemetry records."""
    return {
        "component.name": COMPONENT_NAME,
        "component.version": __version__,
        "component.agent_name": COMPONENT_AGENT_NAME,
    }
