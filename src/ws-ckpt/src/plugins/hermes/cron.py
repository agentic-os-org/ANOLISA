"""Crontab management for scheduled ws-ckpt snapshots."""

from __future__ import annotations

import fcntl
import os
import re
import subprocess
import tempfile
from typing import List, Optional

_LOCK_PATH = os.path.join(tempfile.gettempdir(), "ws-ckpt-cron.lock")

# Match: ws-ckpt checkpoint ... -w '<path>' or -w <path>
_CRON_RE = re.compile(r"^\S+\s+\S+\s+\S+\s+\S+\s+\S+$")
_MARKER_RE = re.compile(r"ws-ckpt\s+checkpoint\s+.*-w\s+'([^']+)'")
_MARKER_RE_UNQUOTED = re.compile(r"ws-ckpt\s+checkpoint\s+.*-w\s+(\S+)")


def _build_cron_line(workspace: str, schedule: str) -> str:
    quoted_ws = "'" + workspace.replace("'", "'\\''") + "'"
    return (
        f"{schedule} /usr/local/bin/ws-ckpt checkpoint -w {quoted_ws}"
        f' -s "cron-$(date +\\%s)"'
        f' -m "scheduled snapshot"'
        f" --metadata '{{\"auto\":true,\"type\":\"cron\"}}'"
        f" >/dev/null 2>&1"
    )


def _read_crontab() -> Optional[List[str]]:
    """Return current crontab lines, or None on failure."""
    try:
        result = subprocess.run(
            ["crontab", "-l"],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode != 0:
            # "no crontab for <user>" is normal (exit 1 on first use)
            if result.returncode == 1 and "no crontab for" in (result.stderr or ""):
                return []
            return None
        return [l for l in result.stdout.splitlines() if l]
    except Exception:
        return None


def _write_crontab(lines: List[str]) -> bool:
    content = "\n".join(lines)
    if content and not content.endswith("\n"):
        content += "\n"
    try:
        result = subprocess.run(
            ["crontab", "-"],
            input=content, capture_output=True, text=True, timeout=10,
        )
        return result.returncode == 0
    except Exception:
        return False


def _extract_workspace(line: str) -> Optional[str]:
    m = _MARKER_RE.search(line)
    if m:
        return m.group(1)
    m = _MARKER_RE_UNQUOTED.search(line)
    if m:
        return m.group(1)
    return None


def _match_workspace(line: str, workspace: str) -> bool:
    return _extract_workspace(line) == workspace


def validate_cron_expr(expr: str) -> bool:
    """Return True if expr looks like a valid 5-field cron expression."""
    return bool(_CRON_RE.match(expr.strip()))


def parse_schedules_update(value: str, current: List[str]) -> tuple[Optional[List[str]], Optional[str]]:
    """Parse a cronSchedules sub-command and apply it to current list.

    Returns (new_list, None) on success or (None, error_message) on failure.
    """
    import json as _json

    sub_action, _, sub_val = value.strip().partition(" ")
    sub_action = sub_action.lower()
    sub_val = sub_val.strip()
    if len(sub_val) >= 2 and sub_val[0] == sub_val[-1] and sub_val[0] in ('"', "'"):
        sub_val = sub_val[1:-1]

    result = list(current)
    if sub_action == "add":
        if not sub_val:
            return None, "add requires a cron expression"
        if not validate_cron_expr(sub_val):
            return None, f'Invalid cron expression: "{sub_val}". Expected 5 fields, e.g. "0 * * * *"'
        if sub_val not in result:
            result.append(sub_val)
    elif sub_action == "remove":
        if not sub_val:
            return None, "remove requires a cron expression"
        if sub_val in result:
            result.remove(sub_val)
        else:
            return None, f'"{sub_val}" not found in current schedules'
    elif sub_action == "set":
        try:
            parsed = _json.loads(sub_val)
            if not isinstance(parsed, list):
                raise ValueError
            for e in parsed:
                if not validate_cron_expr(str(e)):
                    return None, f'Invalid cron expression in array: "{e}"'
            result = [str(e) for e in parsed]
        except (ValueError, _json.JSONDecodeError):
            return None, "set requires a JSON array, e.g. '[\"0 * * * *\"]'"
    else:
        return None, (
            f'Unknown cronSchedules sub-action: {sub_action}. '
            'Use: add "EXPR", remove "EXPR", or set \'["EXPR"]\''
        )
    return result, None


class CrontabManager:
    """Manage crontab entries for ws-ckpt scheduled snapshots."""

    @staticmethod
    def _with_lock(fn):
        """Execute fn while holding an exclusive flock to prevent TOCTOU races."""
        fd = os.open(_LOCK_PATH, os.O_CREAT | os.O_RDWR, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            return fn()
        finally:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)

    @staticmethod
    def sync(workspace: str, schedules: List[str]) -> bool:
        def _do():
            lines = _read_crontab()
            if lines is None:
                return False
            kept = [l for l in lines if not _match_workspace(l, workspace)]
            for s in schedules:
                kept.append(_build_cron_line(workspace, s))
            return _write_crontab(kept)
        return CrontabManager._with_lock(_do)

    @staticmethod
    def remove(workspace: str) -> bool:
        return CrontabManager.sync(workspace, [])

    @staticmethod
    def sync_with_retry(workspace: str, schedules: List[str], retries: int = 3) -> bool:
        for _ in range(retries):
            if CrontabManager.sync(workspace, schedules):
                return True
        return False

    @staticmethod
    def remove_with_retry(workspace: str, retries: int = 3) -> bool:
        return CrontabManager.sync_with_retry(workspace, [], retries)

    @staticmethod
    def migrate(old_workspace: str, new_workspace: str, schedules: List[str]) -> List[str]:
        """Remove old crontab entries and install under new workspace. Returns warnings."""
        warnings: List[str] = []
        if old_workspace and old_workspace != new_workspace:
            if not CrontabManager.remove_with_retry(old_workspace):
                warnings.append(
                    f"WARNING: Failed to remove cron entries for old workspace {old_workspace}. "
                    f"Run `crontab -e` to manually remove lines containing -w '{old_workspace}'."
                )
        if schedules:
            if not CrontabManager.sync_with_retry(new_workspace, schedules):
                warnings.append(
                    f"WARNING: Failed to install cron entries for {new_workspace}. "
                    f"Cron snapshots will not run until next session start or manual retry."
                )
        return warnings

    @staticmethod
    def list_installed(workspace: str) -> List[str]:
        lines = _read_crontab()
        if lines is None:
            return []
        result: List[str] = []
        for line in lines:
            if not _match_workspace(line, workspace):
                continue
            parts = line.split()
            if len(parts) >= 5:
                result.append(" ".join(parts[:5]))
        return result
