"""Log path configuration for security events."""

import os
import re
import stat
from pathlib import Path

PRIMARY_LOG_PATH = "/var/log/agent-sec/security-events.jsonl"
FALLBACK_LOG_PATH = str(Path.home() / ".agent-sec-core" / "security-events.jsonl")
DEFAULT_SECURITY_STREAM = "security-events"
_STREAM_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]*$")


def _safe_tmp_dir() -> Path:
    """Return a validated per-user tmp directory, raising on symlink/ownership issues."""
    uid = os.getuid()
    safe_dir = Path("/tmp") / f"agent-sec-{uid}"
    safe_dir.mkdir(mode=0o700, exist_ok=True)
    st = safe_dir.lstat()
    if stat.S_ISLNK(st.st_mode):
        raise OSError(f"{safe_dir} is a symlink — refusing to use")
    if st.st_uid != uid:
        raise OSError(f"{safe_dir} not owned by uid {uid}")
    safe_dir.chmod(0o700)
    return safe_dir


def _resolve_data_dir() -> Path:
    """Resolve the data directory using a 3-tier fallback strategy.

    Override: set ``AGENT_SEC_DATA_DIR`` to force a specific directory.
    This is primarily useful for tests that need an isolated DB/log path.

    Security: All directories are created with mode 0o700 (owner-only access)
    to protect sensitive security event data.
    """
    # Env-var override — highest priority
    override = os.environ.get("AGENT_SEC_DATA_DIR")
    if override:
        p = Path(override)
        p.mkdir(parents=True, exist_ok=True, mode=0o700)
        p.chmod(0o700)  # Explicit permission guarantee (defensive against umask)
        return p

    # Tier 1: system-wide directory
    primary_dir = Path(PRIMARY_LOG_PATH).parent
    try:
        primary_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        primary_dir.chmod(0o700)  # Explicit permission guarantee
        if primary_dir.is_dir() and os.access(primary_dir, os.W_OK):
            return primary_dir
    except OSError:
        pass

    # Tier 2: user home directory
    fallback_dir = Path(FALLBACK_LOG_PATH).parent
    try:
        fallback_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        fallback_dir.chmod(0o700)  # Explicit permission guarantee
        return fallback_dir
    except OSError:
        pass

    # Tier 3: secure per-user tmp directory
    try:
        return _safe_tmp_dir()
    except OSError:
        pass

    # Last resort: return tmp path anyway
    return Path("/tmp") / f"agent-sec-{os.getuid()}"


def get_data_dir() -> Path:
    """Return the directory used for local agent-sec data files."""
    return _resolve_data_dir()


def _validate_stream_name(stream: str) -> str:
    """Validate a logical local-event stream name."""
    if not _STREAM_NAME_RE.match(stream):
        raise ValueError(f"Invalid stream name: {stream!r}")
    return stream


def get_stream_log_path(stream: str) -> str:
    """Return the JSONL path for a logical local-event stream."""
    stream = _validate_stream_name(stream)
    return str(_resolve_data_dir() / f"{stream}.jsonl")


def get_stream_db_path(stream: str) -> str:
    """Return the SQLite path for a logical local-event stream."""
    stream = _validate_stream_name(stream)
    return str(_resolve_data_dir() / f"{stream}.db")


def get_log_path(stream: str = DEFAULT_SECURITY_STREAM) -> str:
    """Return the path for the security-events JSONL log file."""
    return get_stream_log_path(stream)


def get_db_path(stream: str = DEFAULT_SECURITY_STREAM) -> str:
    """Return the path for the security-events SQLite database."""
    return get_stream_db_path(stream)
