"""XDG Base Directory resolution for skill-ledger.

Provides ``get_data_dir()`` and ``get_config_dir()`` so that every module can
resolve paths without pulling in unrelated dependencies.

All skill-ledger paths live under the ``agent-sec`` vendor namespace so that
every agent-sec-core sub-module shares a common top-level directory
(e.g. ``~/.local/share/agent-sec/skill-ledger/``).

Testing strategy: set ``XDG_DATA_HOME`` / ``XDG_CONFIG_HOME`` env vars to a
``tempfile.mkdtemp()`` directory — all key and config I/O is automatically
redirected.
"""

import os
from pathlib import Path

_APP_NAME = Path("agent-sec") / "skill-ledger"


def get_data_dir() -> Path:
    """Return the skill-ledger data directory (XDG_DATA_HOME).

    Default: ``~/.local/share/agent-sec/skill-ledger/``
    """
    base = os.environ.get("XDG_DATA_HOME", "")
    if not base:
        base = str(Path.home() / ".local" / "share")
    return Path(base) / _APP_NAME


def get_config_dir() -> Path:
    """Return the skill-ledger config directory (XDG_CONFIG_HOME).

    Default: ``~/.config/agent-sec/skill-ledger/``
    """
    base = os.environ.get("XDG_CONFIG_HOME", "")
    if not base:
        base = str(Path.home() / ".config")
    return Path(base) / _APP_NAME


def valid_anolisa_data_home(value: str | None) -> bool:
    """Match ANOLISA's absolute data-root syntax without resolving the filesystem."""
    return (
        bool(value)
        and Path(value).is_absolute()
        and not any(segment in (".", "..") for segment in value.split("/"))
    )


def get_anolisa_skill_dir() -> Path:
    """Return the raw user skill root using ANOLISA's XDG fallback rules."""
    value = os.environ.get("XDG_DATA_HOME")
    # Match ANOLISA's root spelling; pathlib otherwise preserves exactly two slashes.
    data_home = (
        Path("/" + value.lstrip("/"))
        if valid_anolisa_data_home(value)
        else Path.home() / ".local/share"
    )
    return data_home / "anolisa/skills"
