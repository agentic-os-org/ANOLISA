"""Shared daemon environment variable names and parsing helpers."""

import os

SOCKET_ENV = "AGENT_SEC_DAEMON_SOCKET"
DAEMON_DISABLED_ENV = "AGENT_SEC_DAEMON_DISABLED"
_TRUE_ENV_VALUES = frozenset({"1", "true", "yes", "on"})


def daemon_disabled() -> bool:
    """Return whether CLI daemon calls are disabled by environment."""
    return _is_truthy_env(os.environ.get(DAEMON_DISABLED_ENV))


def _is_truthy_env(value: str | None) -> bool:
    return value is not None and value.strip().lower() in _TRUE_ENV_VALUES
