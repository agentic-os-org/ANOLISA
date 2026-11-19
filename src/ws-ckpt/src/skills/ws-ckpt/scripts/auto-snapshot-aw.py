#!/usr/bin/env python3
"""Manage the workspace binding and snapshot switch for the current AW target."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def run(*command):
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or f"{command[-1]} exited with {result.returncode}")
    return result.stdout


def status(provider):
    data = json.loads(run(provider, "policy", "status"))
    if (
        not isinstance(data, dict)
        or not isinstance(data.get("workspace"), str)
        or not Path(data["workspace"]).is_absolute()
        or not isinstance(data.get("target"), str)
        or not data["target"]
        or not isinstance(data.get("enabled"), bool)
    ):
        raise RuntimeError("AW Provider returned an invalid snapshot status")
    return data


def binding(data):
    return data["workspace"], data["target"]


def registered_workspace_id(workspace):
    try:
        report = json.loads(run("ws-ckpt", "status", "--workspace", workspace, "--format", "json"))
    except RuntimeError as error:
        if str(error).removeprefix("\x1b[31m").startswith("Error [WorkspaceNotFound]:"):
            return None
        raise
    workspaces = report.get("workspaces") if isinstance(report, dict) else None
    if isinstance(workspaces, list) and len(workspaces) == 1 and isinstance(workspaces[0], dict):
        ws_id = workspaces[0].get("ws_id")
        if isinstance(ws_id, str) and ws_id:
            return ws_id
    raise RuntimeError(f"ws-ckpt returned an invalid workspace status for {workspace!r}")


def check_workspace(workspace, requested, *, require_registration):
    different_path = requested is not None and requested != workspace
    if not require_registration and not different_path:
        return

    ws_id = registered_workspace_id(workspace)
    if require_registration and ws_id is None:
        raise RuntimeError(f"Workspace {workspace!r} is not registered; run ws-ckpt init before enabling")
    if different_path and (ws_id is None or ws_id != registered_workspace_id(requested)):
        raise RuntimeError(f"Requested workspace does not match AW binding {workspace!r}")


def write_and_readback(provider, arguments, expected_binding, *, expected_enabled=None):
    workspace, target = expected_binding
    write_error = None
    try:
        run(provider, *arguments)
    except (RuntimeError, OSError) as error:
        write_error = error

    try:
        after = status(provider)
    except (RuntimeError, ValueError, OSError) as error:
        failure = f"write reported an error ({write_error})" if write_error else "write command succeeded"
        raise RuntimeError(f"{failure}, but readback failed; current state is unknown: {error}") from error

    if binding(after) != (workspace, target):
        detail = f": {write_error}" if write_error else ""
        raise RuntimeError(
            f"AW binding differs from the requested binding; active workspace is {after['workspace']!r}{detail}"
        )
    if expected_enabled is not None and after["enabled"] != expected_enabled:
        actual = "enabled" if after["enabled"] else "disabled"
        detail = f": {write_error}" if write_error else ""
        raise RuntimeError(f"Snapshot switch did not reach the requested state; currently {actual}{detail}")
    if write_error:
        after["persistence_confirmed"] = False
        after["write_error"] = str(write_error)
    return after


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("status", "bind", "enable", "disable"))
    parser.add_argument("--workspace", help="Absolute workspace path to bind or match")
    args = parser.parse_args()
    if args.action == "bind" and args.workspace is None:
        parser.error("bind requires --workspace")
    if args.workspace is not None and not Path(args.workspace).is_absolute():
        parser.error("--workspace requires an absolute path")

    try:
        route_script = Path(__file__).absolute().with_name("auto-snapshot-route.sh")
        route = run("sh", str(route_script)).strip()
        if route != "aw_bound":
            raise RuntimeError(f"AW snapshot controls require aw_bound, got {route!r}")
        provider = os.environ["WS_CKPT_AW_PROVIDER"]
        if args.action == "bind":
            target = os.environ["WS_CKPT_AW_TARGET"]
            result = write_and_readback(
                provider,
                ("bind", "--agent", target, "--workspace", args.workspace),
                (args.workspace, target),
            )
            print(json.dumps(result, ensure_ascii=False))
            return 0
        before = status(provider)
        workspace = before["workspace"]
        enabling = args.action == "enable"
        check_workspace(workspace, args.workspace, require_registration=enabling)
        if args.action == "status":
            print(json.dumps(before, ensure_ascii=False))
            return 0

        result = write_and_readback(
            provider,
            ("policy", args.action, "--workspace", workspace),
            binding(before),
            expected_enabled=enabling,
        )
        print(json.dumps(result, ensure_ascii=False))
    except (KeyError, OSError, RuntimeError, ValueError) as error:
        print(f"AW snapshot control failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
