"""Agent-facing tools for the ws-ckpt Hermes plugin.

Tool surface mirrors the OpenClaw plugin (`ws-ckpt-*`):

  ws-ckpt-config     — view or update plugin/daemon configuration
  ws-ckpt-checkpoint — create a new snapshot
  ws-ckpt-rollback   — rollback to a specific snapshot
  ws-ckpt-list       — list snapshots for the workspace
  ws-ckpt-diff       — show file changes between two snapshots
  ws-ckpt-delete     — delete a snapshot
  ws-ckpt-status     — show workspace checkpoint status
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from typing import Any, Dict, Optional, Tuple

from .config import load_config
from .checkpoint_manager import (
    DEFAULT_TIMEOUT_S,
    cwd_inside_workspace,
    cwd_inside_workspace_reason,
    get_manager,
)


# Cached once per process: ws-ckpt is a system-installed binary, so a path
# lookup at first call survives the rest of the session.
_ws_ckpt_available: Optional[bool] = None


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


_NO_WORKSPACE_MSG = "No workspace configured. Tell me the workspace path and I'll set it up."


def _require_workspace() -> Tuple[str, Optional[str]]:
    """Resolve and validate workspace. Returns (workspace, None) or ("", error_json)."""
    ws = get_manager().config.workspace
    if not ws:
        return "", _err(_NO_WORKSPACE_MSG)
    return ws, None


def _reject_if_cwd_inside_workspace(workspace: str) -> Optional[str]:
    """Return a serialized error response when cwd is inside workspace, else None."""
    inside, cwd = cwd_inside_workspace(workspace)
    if inside:
        return _json({
            "success": False,
            "error": cwd_inside_workspace_reason(cwd, workspace),
            "retryable": False,
        })
    return None


def _run_ws_ckpt_cmd(cmd: list) -> Tuple[bool, str]:
    """Execute a ws-ckpt CLI command and return (success, output)."""
    try:
        result = subprocess.run(
            cmd, capture_output=True, text=True, timeout=DEFAULT_TIMEOUT_S,
            env={**os.environ, "WS_CKPT_AGENT_NAME": "hermes"},
        )
        return result.returncode == 0, result.stdout.strip() or result.stderr.strip()
    except subprocess.TimeoutExpired:
        return False, f"Command timed out ({DEFAULT_TIMEOUT_S}s)"
    except FileNotFoundError:
        return False, "ws-ckpt not found. Is it installed and in PATH?"
    except Exception as e:
        return False, str(e)


# Schema version the plugin understands. ws-ckpt --format json carries this
# tag; mismatch means a daemon bump and the plugin should NOT re-interpret.
_POLICY_JSON_SCHEMA = "ws-ckpt-policy/v1"


def _parse_workspace_policy_json(stdout: str) -> Dict[str, Any]:
    """Parse `ws-ckpt config -w <ws> --format json` stdout.

    Returns one of:
        {"kind": "parse-error", "reason": str}
        {"kind": "disabled"}              ← daemon pre-computed is_disabled=true
        {"kind": "count",    "num": int}
        {"kind": "age",      "duration": str}

    Mirrors the openclaw discriminated union (config.ts WorkspaceCleanupParsed)
    so display logic can tell a real "disabled" from a parse failure —
    treating a parse miss as "disabled" was openclaw's original bug.
    """
    try:
        doc = json.loads(stdout)
    except json.JSONDecodeError as e:
        return {"kind": "parse-error", "reason": f"not valid JSON: {e}"}
    if not isinstance(doc, dict):
        return {"kind": "parse-error", "reason": "JSON root is not an object"}
    if doc.get("schema") != _POLICY_JSON_SCHEMA:
        return {
            "kind": "parse-error",
            "reason": f"unknown schema: {doc.get('schema')!r} (expected {_POLICY_JSON_SCHEMA!r})",
        }
    eff = doc.get("effective")
    if not isinstance(eff, dict):
        return {"kind": "parse-error", "reason": "missing `effective`"}
    # Trust daemon's pre-computed is_disabled (covers auto_cleanup=false AND
    # Count(0) / Age{secs:0}); re-deriving consumer-side is what bit openclaw.
    if eff.get("is_disabled") is True:
        return {"kind": "disabled"}
    keep = eff.get("auto_cleanup_keep")
    if not isinstance(keep, dict):
        return {"kind": "parse-error", "reason": "missing `effective.auto_cleanup_keep`"}
    mode = keep.get("mode")
    if mode == "count":
        n = keep.get("count")
        # bool is a subclass of int in Python — exclude explicitly so {"count": true} doesn't slip through.
        if isinstance(n, bool) or not isinstance(n, int) or n < 0:
            return {"kind": "parse-error", "reason": f"`count` must be a non-negative int, got {n!r}"}
        return {"kind": "count", "num": n}
    if mode == "age":
        raw = keep.get("raw")
        if not isinstance(raw, str):
            return {"kind": "parse-error", "reason": "`raw` is not a string"}
        return {"kind": "age", "duration": raw}
    return {"kind": "parse-error", "reason": f"unknown auto_cleanup_keep.mode: {mode!r}"}


def _render_workspace_policy(stdout: str, ws: str) -> list:
    """Render a parsed JSON policy as human-readable lines for `view`.

    A parse failure is reported explicitly, never silently rendered as
    "auto-cleanup disabled" (openclaw's original bug).
    """
    parsed = _parse_workspace_policy_json(stdout)
    kind = parsed["kind"]
    if kind == "parse-error":
        return [
            f"  (daemon response did not match expected schema: {parsed['reason']}; "
            f"raw stdout follows)",
            stdout,
        ]
    if kind == "disabled":
        return [
            "  maxSnapshotsNum:      not set - auto-cleanup disabled",
            "  maxSnapshotsDuration: not set - auto-cleanup disabled",
        ]
    if kind == "count":
        return [
            f"  maxSnapshotsNum:      {parsed['num']}",
            "  maxSnapshotsDuration: not set",
        ]
    # kind == "age"
    return [
        "  maxSnapshotsNum:      not set",
        f"  maxSnapshotsDuration: {parsed['duration']}",
    ]


def _json(obj: Any) -> str:
    return json.dumps(obj, ensure_ascii=False)


def _ok(output: str) -> str:
    return _json({"success": True, "output": output})


def _err(msg: str) -> str:
    return _json({"success": False, "error": msg})


# ---------------------------------------------------------------------------
# Runtime gate
# ---------------------------------------------------------------------------


def check_ws_ckpt_available() -> bool:
    """Return True when ws-ckpt CLI is on PATH.

    Hermes' registry caches check_fn results for 30s, but we cache for the
    full process lifetime: ws-ckpt is a system-installed binary and a PATH
    lookup is enough — no need to fork `ws-ckpt --version` on every gate.
    """
    global _ws_ckpt_available
    if _ws_ckpt_available is None:
        _ws_ckpt_available = shutil.which("ws-ckpt") is not None
    return _ws_ckpt_available


# ---------------------------------------------------------------------------
# Schemas (OpenAI Function Calling format)
# ---------------------------------------------------------------------------

WS_CKPT_CONFIG_SCHEMA: Dict[str, Any] = {
    "name": "ws-ckpt-config",
    "description": (
        "View or update ws-ckpt configuration. "
        "Configurable keys: "
        "autoCheckpoint (whether to auto-snapshot at the end of each conversation turn), "
        "workspace (default workspace absolute path; used by every command without -w. "
        "If the path is a symlink, use the link itself — do NOT replace it with the "
        "resolved real path; the daemon registers and matches by the exact string you pass), "
        "cronSchedules (scheduled cron snapshots using standard 5-field cron expressions; "
        "value format: 'add \"CRON_EXPR\"', 'remove \"CRON_EXPR\"', or 'set [\"CRON_EXPR\"]'; "
        "operates on the current workspace; "
        "if the user's scheduling intent cannot be exactly expressed as a cron expression, "
        "do NOT write an approximate/degraded schedule — present the closest option and await confirmation), "
        "maxSnapshotsNum (number of snapshots to keep when auto-cleanup is by count), "
        "maxSnapshotsDuration (duration to keep when auto-cleanup is by time, e.g. \"7d\"/\"24h\"). "
        "Only update the specific key requested by the user."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "description": 'Action to perform: "view" (default) or "update"',
            },
            "key": {
                "type": "string",
                "description": (
                    "Config key to update: autoCheckpoint, workspace, "
                    "cronSchedules, maxSnapshotsNum, maxSnapshotsDuration"
                ),
            },
            "value": {
                "type": "string",
                "description": (
                    "New value as a string. Formats: "
                    "autoCheckpoint = \"true\"/\"false\"; "
                    "workspace = absolute path; "
                    "cronSchedules = 'add \"CRON_EXPR\"' / 'remove \"CRON_EXPR\"' / 'set [\"CRON_EXPR\"]'; "
                    "maxSnapshotsNum = positive integer (or \"unset\" to restore inherit-global); "
                    "maxSnapshotsDuration = e.g. \"7d\"/\"24h\" (or \"unset\" to restore inherit-global)."
                ),
            },
        },
        "additionalProperties": False,
    },
}

WS_CKPT_CHECKPOINT_SCHEMA: Dict[str, Any] = {
    "name": "ws-ckpt-checkpoint",
    "description": (
        "Create a checkpoint of the default or specified workspace. Use this "
        "to save the current state before making significant changes, so you "
        "can rollback if needed."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "id": {
                "type": "string",
                "description": "Required: caller-provided snapshot identifier",
            },
            "message": {
                "type": "string",
                "description": "Optional message describing the checkpoint",
            },
            "workspace": {
                "type": "string",
                "description": (
                    "Optional: workspace absolute path. Defaults to the "
                    "configured workspace. If the path is a symlink, use the "
                    "link itself — do NOT replace it with the resolved real path."
                ),
            },
        },
        "required": ["id"],
        "additionalProperties": False,
    },
}

WS_CKPT_ROLLBACK_SCHEMA: Dict[str, Any] = {
    "name": "ws-ckpt-rollback",
    "description": (
        "Preview or roll back the workspace to a specific checkpoint or N ancestors back. "
        "Set preview=true to inspect file changes without modifying the workspace. "
        "Use ws-ckpt-list first to see available snapshots."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "target": {
                "type": "string",
                "description": (
                    "Snapshot id to roll back to (mutually exclusive with "
                    "num_ancestors)."
                ),
            },
            "num_ancestors": {
                "type": "integer",
                "description": (
                    "Number of steps to go back "
                    "(>=1, mutually exclusive with target). "
                    "1 = undo last turn, 2 = undo last two turns."
                ),
            },
            "workspace": {
                "type": "string",
                "description": (
                    "Optional: workspace absolute path. Defaults to the "
                    "configured workspace. If the path is a symlink, use the "
                    "link itself — do NOT replace it with the resolved real path."
                ),
            },
            "preview": {
                "type": "boolean",
                "description": (
                    "Optional: preview the file changes without modifying the workspace."
                ),
            },
        },
        "additionalProperties": False,
    },
}

WS_CKPT_LIST_SCHEMA: Dict[str, Any] = {
    "name": "ws-ckpt-list",
    "description": (
        "List all snapshots managed by ws-ckpt. Always display the FULL "
        "untruncated result to the user."
    ),
    "parameters": {
        "type": "object",
        "properties": {},
        "additionalProperties": False,
    },
}

WS_CKPT_DIFF_SCHEMA: Dict[str, Any] = {
    "name": "ws-ckpt-diff",
    "description": (
        "Compare file changes between two snapshots, or between a snapshot "
        "and the current workspace state. Omit 'to' to diff against the "
        "current workspace. Always display the FULL untruncated result to "
        "the user. Do NOT re-interpret or contradict the tool output."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "from": {
                "type": "string",
                "description": "Source snapshot id",
            },
            "to": {
                "type": "string",
                "description": (
                    "Target snapshot id or name. Omit to diff against current workspace state."
                ),
            },
        },
        "required": ["from"],
        "additionalProperties": False,
    },
}

WS_CKPT_DELETE_SCHEMA: Dict[str, Any] = {
    "name": "ws-ckpt-delete",
    "description": (
        "Delete a specific snapshot. Use ws-ckpt-list to see available "
        "snapshots before deleting."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "snapshot": {
                "type": "string",
                "description": "Required: snapshot ID to delete",
            },
            "workspace": {
                "type": "string",
                "description": (
                    "Optional: workspace absolute path. Defaults to the "
                    "configured workspace. If the path is a symlink, use the "
                    "link itself — do NOT replace it with the resolved real path."
                ),
            },
        },
        "required": ["snapshot"],
        "additionalProperties": False,
    },
}

WS_CKPT_STATUS_SCHEMA: Dict[str, Any] = {
    "name": "ws-ckpt-status",
    "description": (
        "Show ws-ckpt service status and workspace information. Returns the "
        "complete status from ws-ckpt daemon — no additional CLI or exec "
        "verification needed."
    ),
    "parameters": {
        "type": "object",
        "properties": {},
        "additionalProperties": False,
    },
}


# ---------------------------------------------------------------------------
# Handlers
# ---------------------------------------------------------------------------


def handle_ws_ckpt_config(args: Dict[str, Any], **_kwargs) -> str:
    """Handle ws-ckpt-config tool call.

    view  → print plugin config + transparently dump
        `ws-ckpt config -w <workspace>` stdout (3-column effective/local/global view).
    update autoCheckpoint / workspace  → mutate the in-process manager
        config; persistence requires editing ~/.hermes/config.yaml.
    update maxSnapshotsNum / maxSnapshotsDuration → shell out to
        `ws-ckpt config -w <workspace> --enable-auto-cleanup --auto-cleanup-keep <v>`.

    Scope is **per-workspace** (the workspace this plugin manages): a hermes
    user changing snapshot retention shouldn't accidentally rewrite the
    daemon-wide default that other workspaces / users share. ws-ckpt v0.3.3
    introduced explicit scope flags (`-g` global / `-w` per-ws); this plugin
    always uses `-w`.
    """
    action = (args.get("action") or "view").strip().lower()

    if action == "view":
        ws, ws_err = _require_workspace()
        if ws_err:
            return ws_err
        cfg = load_config()
        lines = [
            "Current ws-ckpt plugin configuration:",
            f"  autoCheckpoint: {cfg.auto_checkpoint}",
            f"  workspace:      {cfg.workspace}",
        ]
        if cfg.cron_schedules:
            lines.append("  cronSchedules:")
            for expr in cfg.cron_schedules:
                lines.append(f"    - {expr}")
        else:
            lines.append("  cronSchedules:  (disabled)")
        lines.append("")
        lines.append(f"Workspace policy (from `ws-ckpt config -w {ws} --format json`):")
        # Use `--format json`, not raw daemon text: text isn't a contract,
        # while the JSON schema is versioned and lets us tell parse-error
        # from a real "disabled" (the openclaw bug we avoid here).
        success, output = _run_ws_ckpt_cmd(
            ["ws-ckpt", "config", "-w", ws, "--format", "json"]
        )
        if not success:
            lines.append(f"(failed to query daemon: {output or 'no output'})")
            return _ok("\n".join(lines))
        lines.extend(_render_workspace_policy(output, ws))
        return _ok("\n".join(lines))

    if action not in ("update", "set"):
        return _err(f'Unknown action: {action}. Use "view" or "update".')

    key = (args.get("key") or "").strip()
    value = args.get("value")
    if not key:
        return _err(
            "Usage: ws-ckpt-config update <key> <value>. "
            "Available keys: autoCheckpoint, workspace, "
            "cronSchedules, maxSnapshotsNum, maxSnapshotsDuration."
        )

    # Persist via `ws-ckpt config -w <workspace>` so the change scopes to a
    # per-ws policy.toml override, not the shared daemon-wide default.
    if key in ("maxSnapshotsNum", "maxSnapshotsDuration"):
        ws, ws_err = _require_workspace()
        if ws_err:
            return ws_err

        if value is None:
            return _err(
                f"{key} requires a value (or \"unset\" to restore inherit-global)"
            )
        value = str(value).strip()

        if value == "unset":
            # unset = restore default (delete policy.toml) so admin's later global toggle wins.
            success, output = _run_ws_ckpt_cmd(
                ["ws-ckpt", "config", "-w", ws, "--reset"]
            )
            if not success:
                return _err(f"Failed to reset workspace policy for {ws}: {output}")
            return _ok(
                f"Cleared: {key} unset — workspace {ws} now inherits global auto-cleanup."
            )

        if key == "maxSnapshotsNum":
            try:
                num = int(value)
                if num < 1:
                    raise ValueError
            except ValueError:
                return _err("maxSnapshotsNum must be a positive integer")
            keep = str(num)
        else:
            keep = value  # daemon parses duration strings like "7d", "24h"

        success, output = _run_ws_ckpt_cmd(
            ["ws-ckpt", "config", "-w", ws, "--enable-auto-cleanup",
             "--auto-cleanup-keep", keep]
        )
        if not success:
            return _err(f"Failed to configure workspace {ws}: {output}")
        return _ok(
            f"Updated workspace policy for {ws}: {key} = {keep} "
            f"(auto-cleanup enabled, keep {keep})"
        )

    # Plugin-level keys: persist to ~/.hermes/config.yaml AND sync the
    # singleton manager's config in-place so the change takes effect this
    # session without re-reading yaml on every hook fire.
    if key == "autoCheckpoint":
        if value is None:
            return _err('autoCheckpoint requires a value: "true" or "false"')
        normalized = str(value).strip().lower()
        # LLM tool callers and shell users emit a wide vocabulary; accept the
        # common bool aliases instead of failing silently for anyone who didn't
        # read stderr. Canonical form remains "true"/"false" in tool descriptions.
        if normalized in ("true", "1", "yes", "on", "enabled"):
            coerced = True
        elif normalized in ("false", "0", "no", "off", "disabled"):
            coerced = False
        else:
            return _err(f'autoCheckpoint must be "true" or "false" (got "{value}")')
        if coerced:
            workspace, ws_err = _require_workspace()
            if ws_err:
                return ws_err
            rejection = _reject_if_cwd_inside_workspace(workspace)
            if rejection:
                return rejection
        err = _persist_plugin_yaml(autoCheckpoint=coerced)
        if err:
            return _err(f"Failed to persist config: {err}")
        get_manager().set_auto_checkpoint(coerced)
        return _ok(f"Config updated: autoCheckpoint = {coerced}")

    if key == "workspace":
        if not value:
            return _err("workspace requires a path value")
        new_path = str(value).strip()
        mgr = get_manager()
        old_path = mgr.config.workspace
        mgr.set_workspace(new_path)
        from .cron import CrontabManager
        cron_map = mgr.config.cron_schedules
        warnings = CrontabManager.migrate(old_path, new_path, cron_map)
        err = _persist_plugin_yaml(workspace=new_path, cronSchedules=cron_map)
        if err:
            return _err(f"Failed to persist config: {err}")
        msg = f"Config updated: workspace = {new_path}"
        if warnings:
            msg += "\n\n" + "\n".join(warnings)
        return _ok(msg)

    if key == "cronSchedules":
        if value is None:
            return _err(
                'cronSchedules requires a value. '
                'Use: add "EXPR", remove "EXPR", or set \'["EXPR"]\''
            )
        from .cron import CrontabManager, parse_schedules_update
        mgr = get_manager()
        ws = mgr.config.workspace
        if not ws:
            return _err(_NO_WORKSPACE_MSG)
        current = list(mgr.config.cron_schedules)
        new_schedules, err_msg = parse_schedules_update(str(value), current)
        if err_msg:
            return _err(err_msg)
        mgr.config.cron_schedules = new_schedules
        err = _persist_plugin_yaml(cronSchedules=new_schedules)
        if err:
            return _err(f"Failed to persist config: {err}")
        cron_note = ""
        if not CrontabManager.sync_with_retry(ws, new_schedules):
            cron_note = (
                "\n\nWARNING: Failed to sync crontab after 3 attempts. "
                "Config saved but cron snapshots will not run until next session start or manual retry."
            )
        return _ok(
            f"cronSchedules updated: {new_schedules or '(disabled)'}"
            + cron_note
        )

    return _err(
        f"Unknown config key: {key}. Available: autoCheckpoint, "
        "workspace, cronSchedules, maxSnapshotsNum, maxSnapshotsDuration."
    )


def _persist_plugin_yaml(**fields: Any) -> str:
    """Write ``plugins.ws-ckpt.<key> = value`` into ~/.hermes/config.yaml.

    Returns an error message on failure, empty string on success.
    Refuses to write when the Hermes installation is managed.
    """
    try:
        from hermes_cli.config import (
            is_managed,
            load_config as hermes_load_config,
            save_config,
        )
    except Exception as e:
        return f"hermes_cli not available: {e}"

    if is_managed():
        return "Hermes installation is managed; config.yaml is read-only"

    try:
        cfg = hermes_load_config()
    except Exception as e:
        return f"failed to load hermes config: {e}"

    plugins = cfg.setdefault("plugins", {})
    if not isinstance(plugins, dict):
        return "plugins section in config.yaml is not a mapping"
    ws_ckpt = plugins.setdefault("ws-ckpt", {})
    if not isinstance(ws_ckpt, dict):
        return "plugins.ws-ckpt section in config.yaml is not a mapping"
    for k, v in fields.items():
        ws_ckpt[k] = v

    try:
        save_config(cfg)
    except Exception as e:
        return f"failed to save config.yaml: {e}"
    return ""


def _resolve_workspace(args: Dict[str, Any]) -> Tuple[str, Optional[str]]:
    """Resolve workspace from args (explicit override) or config (fallback)."""
    explicit = (args.get("workspace") or "").strip()
    if explicit:
        return explicit, None
    return _require_workspace()


def handle_ws_ckpt_checkpoint(args: Dict[str, Any], **_kwargs) -> str:
    """Handle ws-ckpt-checkpoint tool call."""
    snapshot_id = (args.get("id") or "").strip()
    if not snapshot_id:
        return _err("'id' is required")

    workspace, ws_err = _resolve_workspace(args)
    if ws_err:
        return ws_err
    rejection = _reject_if_cwd_inside_workspace(workspace)
    if rejection:
        return rejection

    message = (args.get("message") or "").strip() or "manual checkpoint"

    cmd = ["ws-ckpt", "checkpoint", "-w", workspace, "-s", snapshot_id,
           "-m", message]
    success, output = _run_ws_ckpt_cmd(cmd)
    return _ok(output) if success else _err(output)


def handle_ws_ckpt_rollback(args: Dict[str, Any], **_kwargs) -> str:
    """Handle ws-ckpt-rollback tool call."""
    target = (args.get("target") or "").strip()
    num_ancestors = args.get("num_ancestors")
    preview = args.get("preview") is True

    if target and num_ancestors is not None:
        return _err("'target' and 'num_ancestors' are mutually exclusive")
    if not target and num_ancestors is None:
        return _err("either 'target' or 'num_ancestors' is required")
    if num_ancestors is not None:
        try:
            num_ancestors = int(num_ancestors)
        except (ValueError, TypeError):
            return _err("'num_ancestors' must be an integer >= 1")
    if num_ancestors is not None and num_ancestors < 1:
        return _err("'num_ancestors' must be >= 1")

    workspace, ws_err = _resolve_workspace(args)
    if ws_err:
        return ws_err
    if not preview:
        rejection = _reject_if_cwd_inside_workspace(workspace)
        if rejection:
            return rejection

    if num_ancestors is not None:
        cmd = ["ws-ckpt", "rollback", "-w", workspace, "-n", str(num_ancestors)]
    else:
        cmd = ["ws-ckpt", "rollback", "-w", workspace, "-s", target]
    if preview:
        cmd.append("--preview")
    success, output = _run_ws_ckpt_cmd(cmd)
    if success and not preview:
        get_manager().skip_next_auto_checkpoint = True
    return _ok(output) if success else _err(output)


def handle_ws_ckpt_list(args: Dict[str, Any], **_kwargs) -> str:
    """Handle ws-ckpt-list tool call."""
    workspace, ws_err = _require_workspace()
    if ws_err:
        return ws_err
    cmd = ["ws-ckpt", "list", "-w", workspace, "--format", "table"]
    success, output = _run_ws_ckpt_cmd(cmd)
    return _ok(output) if success else _err(output)


def handle_ws_ckpt_diff(args: Dict[str, Any], **_kwargs) -> str:
    """Handle ws-ckpt-diff tool call."""
    from_id = (args.get("from") or "").strip()
    to_id = (args.get("to") or "").strip()
    if not from_id:
        return _err("'from' is required")

    workspace, ws_err = _require_workspace()
    if ws_err:
        return ws_err
    cmd = ["ws-ckpt", "diff", "-w", workspace, "--from", from_id]
    if to_id:
        cmd.extend(["--to", to_id])
    success, output = _run_ws_ckpt_cmd(cmd)
    return _ok(output) if success else _err(output)


def handle_ws_ckpt_delete(args: Dict[str, Any], **_kwargs) -> str:
    """Handle ws-ckpt-delete tool call."""
    snapshot = (args.get("snapshot") or "").strip()
    if not snapshot:
        return _err("'snapshot' is required")

    workspace, ws_err = _resolve_workspace(args)
    if ws_err:
        return ws_err
    cmd = ["ws-ckpt", "delete", "-s", snapshot, "-w", workspace, "--force"]
    success, output = _run_ws_ckpt_cmd(cmd)
    return _ok(output) if success else _err(output)


def handle_ws_ckpt_status(args: Dict[str, Any], **_kwargs) -> str:
    """Handle ws-ckpt-status tool call."""
    workspace, ws_err = _require_workspace()
    if ws_err:
        return ws_err
    cmd = ["ws-ckpt", "status", "-w", workspace, "--format", "table"]
    success, output = _run_ws_ckpt_cmd(cmd)
    return _ok(output) if success else _err(output)


# ---------------------------------------------------------------------------
# Export tuple: (name, schema, handler, emoji)
# ---------------------------------------------------------------------------

TOOLS = (
    ("ws-ckpt-config", WS_CKPT_CONFIG_SCHEMA, handle_ws_ckpt_config, "⚙️"),
    ("ws-ckpt-checkpoint", WS_CKPT_CHECKPOINT_SCHEMA, handle_ws_ckpt_checkpoint, "📸"),
    ("ws-ckpt-rollback", WS_CKPT_ROLLBACK_SCHEMA, handle_ws_ckpt_rollback, "⏪"),
    ("ws-ckpt-list", WS_CKPT_LIST_SCHEMA, handle_ws_ckpt_list, "📋"),
    ("ws-ckpt-diff", WS_CKPT_DIFF_SCHEMA, handle_ws_ckpt_diff, "🔀"),
    ("ws-ckpt-delete", WS_CKPT_DELETE_SCHEMA, handle_ws_ckpt_delete, "🗑"),
    ("ws-ckpt-status", WS_CKPT_STATUS_SCHEMA, handle_ws_ckpt_status, "📊"),
)
