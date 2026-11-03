"""CLI wrapper and snapshot management for the ws-ckpt Hermes plugin.

Wraps all `ws-ckpt` CLI invocations using subprocess.run.
Each method constructs the appropriate CLI arguments, executes the command,
and returns structured results.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional

from .config import HermesPluginConfig, MSG_TRUNCATE_LEN, load_config

# Allows the daemon's 180 s bounded reclamation plus RPC overhead.
DEFAULT_TIMEOUT_S = 240

WS_CKPT_BIN = "ws-ckpt"


@dataclass
class CommandOutput:
    """Structured output from a CLI invocation."""

    exit_code: int
    stdout: str
    stderr: str


@dataclass
class CheckpointResult:
    """Result of a checkpoint creation attempt."""

    success: bool
    message: str
    snapshot: str = ""
    skipped: bool = False
    reason: Optional[str] = None


def map_error_to_message(stderr: str, context: Optional[Dict[str, Any]] = None) -> str:
    """Map CLI stderr to a user-friendly error message.

    Follows the OpenClaw mapErrorToLLMMessage pattern.
    """
    ctx_str = ""
    if context:
        ctx_str = f" (context: {json.dumps(context)})"

    lowered = stderr.lower()

    # A filename may itself contain words such as "timeout" or "active write".
    if "cannot be represented as utf-8" in lowered:
        diagnostic = re.sub(r"\x1b\[[0-9;]*m", "", stderr).strip()
        return (
            "A workspace path cannot be represented as UTF-8. "
            "This is NOT retryable until the filename is fixed. "
            "Use a valid UTF-8 name for the affected file or directory. "
            "The operation and escaped path token are shown in this diagnostic:\n"
            f"{diagnostic}{ctx_str}"
        )

    # CLI environment issues take priority over generic "not found" (snapshot)
    if "binary not found" in lowered or "not found on path" in lowered:
        return f"ws-ckpt CLI not found on PATH.{ctx_str}"
    if "timeout" in lowered:
        return f"Command timed out.{ctx_str}"
    if "already exists" in lowered:
        return f"Snapshot already exists in this workspace. Use a different ID.{ctx_str}"
    if "active write" in lowered or "write operations" in lowered:
        return f"Workspace has active write operations. Wait a moment and retry.{ctx_str}"
    if "insufficient" in lowered:
        return f"Insufficient disk space for snapshot. Delete old snapshots to free space.{ctx_str}"
    # daemon flattens anyhow chains via `format!("{:#}", e)`, so inner io errors leak generic substrings.
    if "cwd scan failed" in lowered:
        return (
            "ws-ckpt could not scan /proc to verify workspace occupants "
            "(typically a transient /proc or canonicalize race). "
            "This is retryable — wait a moment and try again."
            f"{ctx_str}"
        )
    if "have cwd inside workspace" in lowered:
        return (
            "Other processes have their working directory inside the workspace. "
            "ws-ckpt cannot proceed because the symlink swap would break those processes. "
            "This is NOT retryable. The user must move affected processes out of the workspace."
            f"{ctx_str}"
        )
    if "daemon is not running" in lowered or "daemon is starting up" in lowered:
        return f"ws-ckpt daemon is not responding. Is it running?{ctx_str}"
    if "not found" in lowered and "snapshot" in lowered:
        return f"Snapshot not found.{ctx_str}"
    if "not found" in lowered and "workspace" in lowered:
        return f"Workspace not found.{ctx_str}"

    return f"ws-ckpt error: {stderr.strip()}{ctx_str}"


class CheckpointManager:
    """Manages ws-ckpt CLI operations.

    Provides synchronous methods for initializing the workspace and creating
    checkpoints. The plugin does not maintain an in-memory snapshot cache —
    `ws-ckpt list` is the single source of truth, queried on demand by tools.
    """

    def __init__(self, config: HermesPluginConfig) -> None:
        self._config = config
        self._turn_count: int = 0
        # Registrations restored this turn; consumed together at turn end.
        self.skip_auto_checkpoint_workspaces: set[str] = set()

    @property
    def config(self) -> HermesPluginConfig:
        """Expose the plugin config for hooks and tool handlers."""
        return self._config

    def set_workspace(self, workspace: str) -> None:
        """Update the in-process workspace path."""
        self._config.workspace = workspace

    def set_auto_checkpoint(self, enabled: bool) -> None:
        """Update the in-process auto-checkpoint flag."""
        self._config.auto_checkpoint = enabled

    def advance_turn(self) -> int:
        """Increment and return the turn counter."""
        self._turn_count += 1
        return self._turn_count

    # ------------------------------------------------------------------
    # CLI execution
    # ------------------------------------------------------------------

    def _run(self, args: List[str]) -> CommandOutput:
        """Execute a ws-ckpt CLI command and return structured output."""
        try:
            result = subprocess.run(
                [WS_CKPT_BIN, *args],
                capture_output=True,
                text=True,
                timeout=DEFAULT_TIMEOUT_S,
                env={**os.environ, "WS_CKPT_AGENT_NAME": "hermes"},
            )
            return CommandOutput(
                exit_code=result.returncode,
                stdout=result.stdout,
                stderr=result.stderr,
            )
        except subprocess.TimeoutExpired:
            return CommandOutput(
                exit_code=1,
                stdout="",
                stderr=f"Command timed out after {DEFAULT_TIMEOUT_S} seconds",
            )
        except FileNotFoundError:
            return CommandOutput(
                exit_code=127,
                stdout="",
                stderr=f"{WS_CKPT_BIN} binary not found on PATH",
            )
        except Exception as e:
            return CommandOutput(
                exit_code=1,
                stdout="",
                stderr=str(e),
            )

    # ------------------------------------------------------------------
    # High-level operations
    # ------------------------------------------------------------------

    def init_workspace(self) -> CommandOutput:
        """Initialize a workspace for ws-ckpt management.

        Equivalent to: ws-ckpt init --workspace <ws>
        """
        return self._run(["init", "--workspace", self._config.workspace])

    def create_checkpoint(
        self,
        snapshot_id: str,
        message: str = "",
        metadata: Optional[Dict[str, Any]] = None,
    ) -> CheckpointResult:
        """Create a checkpoint (snapshot) of the workspace.

        Equivalent to:
            ws-ckpt checkpoint --workspace <ws> --snapshot <id> [--message <msg>] [--metadata <json>]
        """
        args = [
            "checkpoint",
            "--workspace", self._config.workspace,
            "--snapshot", snapshot_id,
        ]

        if message:
            args.extend(["--message", message[:MSG_TRUNCATE_LEN]])

        if metadata:
            args.extend(["--metadata", json.dumps(metadata)])

        output = self._run(args)

        if output.exit_code != 0:
            return CheckpointResult(
                success=False,
                message=map_error_to_message(output.stderr, {"id": snapshot_id}),
            )

        # CheckpointSkipped is a successful CLI response reported on stderr.
        combined_output = f"{output.stdout}\n{output.stderr}"
        if "Empty workspace, no snapshot created." in combined_output:
            return CheckpointResult(
                success=True,
                skipped=True,
                reason="Empty workspace, no snapshot created.",
                message="Empty workspace, no snapshot created.",
            )

        return CheckpointResult(
            success=True,
            message=f"Checkpoint created: {snapshot_id}",
            snapshot=snapshot_id,
        )


# ---------------------------------------------------------------------------
# Singleton & workspace helpers (shared by __init__ and tools)
# ---------------------------------------------------------------------------

_manager: Optional[CheckpointManager] = None


def get_manager() -> CheckpointManager:
    """Return (or create) the singleton CheckpointManager."""
    global _manager
    if _manager is None:
        config = load_config()
        _manager = CheckpointManager(config)
        print("[ws-ckpt] Plugin initialized", flush=True)
    return _manager


def resolve_workspace_path(workspace: str) -> str:
    """Resolve bare selectors through the daemon, preserving its registered path."""
    if workspace in (".", "..") or "/" in workspace or "\\" in workspace:
        return workspace
    try:
        output = get_manager()._run(["status", "--workspace", workspace, "--format", "json"])
        if output.exit_code:
            raise ValueError(output.stderr or output.stdout)
        report = json.loads(output.stdout)
        entries = report.get("workspaces") if isinstance(report, dict) else None
        if (
            not isinstance(entries, list)
            or len(entries) != 1
            or not isinstance(entries[0], dict)
            or not isinstance(entries[0].get("path"), str)
            or not Path(entries[0]["path"]).is_absolute()
        ):
            raise ValueError("expected one registered workspace with an absolute path")
        return entries[0]["path"]
    except (ValueError, TypeError) as error:
        raise ValueError(
            f"Cannot resolve workspace {workspace!r}: {error}. "
            "Check daemon status or use the registered absolute path."
        ) from error


def workspace_registration_path(workspace: str) -> str:
    """Resolve parent aliases while preserving the final workspace symlink."""
    path = Path(workspace)
    try:
        # The daemon replaces the final symlink during rollback. Only its
        # parent belongs in canonical identity; resolving the whole path
        # would make that identity depend on the currently live snapshot.
        if path.name == "..":
            return str(path.resolve(strict=True))
        return str(path.parent.resolve(strict=True) / path.name)
    except (OSError, RuntimeError):
        # Keep an unavailable registration addressable by its exact spelling;
        # do not invent an alias by cancelling unresolved '..' components.
        return str(path.absolute())


def cwd_inside_workspace(workspace: str) -> tuple[bool, str]:
    """Return (inside, cwd) — whether the current cwd is the workspace or a descendant."""
    try:
        cwd = Path(os.getcwd()).resolve()
    except (FileNotFoundError, OSError):
        return False, ""
    try:
        ws_path = Path(workspace).resolve()
    except (FileNotFoundError, OSError):
        return False, str(cwd)
    return cwd == ws_path or ws_path in cwd.parents, str(cwd)


def cwd_inside_workspace_reason(cwd: str, workspace: str) -> str:
    return (
        f"Refused: cwd={cwd} is inside workspace={workspace}. "
        "ws-ckpt replaces the workspace inode during init/checkpoint/rollback, "
        "which would invalidate the process cwd. "
        "The user must launch the session from outside the workspace directory."
    )
