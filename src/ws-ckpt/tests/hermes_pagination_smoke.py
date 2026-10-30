#!/usr/bin/env python3
"""Exercise installed Hermes discovery/dispatch against a real Btrfs daemon.

Run with the installed Hermes Python environment. Supply two dedicated, already
registered Btrfs workspaces; the first must contain at least five real snapshots.
This script lists existing snapshots and creates only a temporary Hermes profile.
It does not provision a daemon, change workspace data or invoke an LLM.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
from collections.abc import Callable
from pathlib import Path


def stable_table(output: str) -> str:
    """Remove only the CLI's final per-execution timing line."""
    lines = output.strip().splitlines()
    if lines and re.fullmatch(r"Completed in [0-9]+\.[0-9]{3}s", lines[-1]):
        lines.pop()
    return "\n".join(lines)


def check_continuation(
    call: Callable[[dict], dict], cli: Callable[..., str], workspace: Path, cursor: str
) -> None:
    """Compare the same cursor request without assuming count-filled pages."""
    continuation = call({"cursor": cursor})
    expected = json.loads(cli("list", "-w", str(workspace), "--format", "json", "--cursor", cursor))
    assert continuation["success"] is True
    assert continuation["snapshots"] == expected["snapshots"]
    assert continuation["next_cursor"] == expected["next_cursor"]


def main() -> None:
    """Record native host registration, page traversal and explicit failures."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--other-workspace", type=Path, required=True)
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("--ws-ckpt", type=Path, default=shutil.which("ws-ckpt"))
    parser.add_argument("--hermes-bin", type=Path, default=shutil.which("hermes"))
    args = parser.parse_args()
    if args.ws_ckpt is None or args.hermes_bin is None:
        parser.error("installed ws-ckpt and Hermes executables are required")
    args.ws_ckpt = args.ws_ckpt.resolve()
    args.hermes_bin = args.hermes_bin.resolve()
    args.workspace = args.workspace.absolute()
    args.other_workspace = args.other_workspace.absolute()
    args.socket = args.socket.absolute()
    for workspace in [args.workspace, args.other_workspace]:
        fs_type = subprocess.check_output(
            ["stat", "-f", "-c", "%T", str(workspace.resolve())], text=True
        ).strip()
        if fs_type != "btrfs":
            parser.error(f"{workspace} resolves to {fs_type}, not a Btrfs workspace")
    os.environ["PATH"] = str(args.ws_ckpt.parent) + os.pathsep + os.environ["PATH"]
    os.environ["WS_CKPT_SOCKET"] = str(args.socket)
    os.environ["WS_CKPT_WORKSPACE"] = str(args.workspace)
    os.environ["WS_CKPT_AUTO_CHECKPOINT"] = "false"
    os.environ.pop("HERMES_SAFE_MODE", None)
    os.environ.pop("HERMES_ENABLE_PROJECT_PLUGINS", None)

    def cli(*arguments: str) -> str:
        return subprocess.check_output([str(args.ws_ckpt), *arguments], text=True, timeout=60)

    baseline = json.loads(cli("list", "-w", str(args.workspace), "--format", "json"))
    assert isinstance(baseline, list) and len(baseline) >= 5, "prepare five real snapshots first"
    assert args.workspace.resolve() != args.other_workspace.resolve(), "workspaces must differ"
    json.loads(cli("status", "-w", str(args.other_workspace), "--format", "json"))
    component = Path(__file__).resolve().parents[1]
    source = component / "src/plugins/hermes"
    with tempfile.TemporaryDirectory(prefix="hermes-pagination-acceptance-") as temporary:
        os.environ["HERMES_HOME"] = str(Path(temporary) / "profile")
        os.environ["HERMES_BIN"] = str(args.hermes_bin)
        os.environ["ANOLISA_PROJECT_ROOT"] = str(component.parents[1])
        # The normal installer links the plugin and asks the actual Hermes CLI
        # to enable it, using only this test's private profile.
        installation = subprocess.run(
            ["bash", str(component / "scripts/hermes/install-hermes.sh")],
            check=True,
            capture_output=True,
            text=True,
            timeout=60,
        )
        original_cwd = Path.cwd()
        os.chdir(temporary)
        try:
            from hermes_cli.plugins import get_plugin_manager
            from tools.registry import registry

            manager = get_plugin_manager()
            manager.discover_and_load()
            scope = {"scope": manager.scope_key} if hasattr(manager, "scope_key") else {}
            entry = registry.get_entry("ws-ckpt-list", **scope)
            assert entry is not None, "Hermes did not register ws-ckpt-list"
            handler_source = Path(entry.handler.__code__.co_filename).resolve()
            assert handler_source.is_relative_to(
                source
            ), "Hermes loaded a different plugin revision"

            def call(arguments: dict) -> dict:
                raw = registry.dispatch("ws-ckpt-list", arguments, **scope)
                assert isinstance(raw, str), "Hermes dispatch did not return tool JSON"
                return json.loads(raw)

            first = call({"limit": 2})
            assert first["success"] is True and first["next_cursor"]
            collected = list(first["snapshots"])
            page_sizes = [len(collected)]
            cursors = set()
            page = first
            while page["next_cursor"] is not None:
                cursor = page["next_cursor"]
                assert cursor not in cursors, "continuation loop"
                cursors.add(cursor)
                page = call({"limit": 2, "cursor": cursor})
                assert page["success"] is True
                collected.extend(page["snapshots"])
                page_sizes.append(len(page["snapshots"]))
            assert collected == baseline, "native Hermes pages differ from CLI order or objects"
            check_continuation(call, cli, args.workspace, first["next_cursor"])
            table = call({})
            assert table["success"] is True and "snapshots" not in table
            assert stable_table(table["output"]) == stable_table(
                cli("list", "-w", str(args.workspace))
            )
            assert call({"limit": None})["success"] is False
            assert call({"cursor": "invalid"})["success"] is False
            config = entry.handler.__globals__["get_manager"]().config
            config.workspace = str(args.other_workspace)
            try:
                assert call({"cursor": first["next_cursor"]})["success"] is False
            finally:
                config.workspace = str(args.workspace)
            os.environ["WS_CKPT_SOCKET"] = str(Path(temporary) / "absent.sock")
            try:
                assert call({"limit": 2})["success"] is False
            finally:
                os.environ["WS_CKPT_SOCKET"] = str(args.socket)
            assert (
                json.loads(cli("list", "-w", str(args.workspace), "--format", "json")) == baseline
            )
            print(
                json.dumps(
                    {
                        "result": "PASS",
                        "workspace": str(args.workspace),
                        "hermes_executable": str(args.hermes_bin),
                        "hermes_python_module": __import__(
                            "hermes_cli.plugins", fromlist=[""]
                        ).__file__,
                        "ws_ckpt_version": cli("--version").strip(),
                        "plugin_source": str(handler_source),
                        "snapshot_count": len(baseline),
                        "page_sizes": page_sizes,
                        "native_hermes_dispatch": True,
                        "btrfs_workspace": True,
                        "installer_output": installation.stdout,
                        "scope": "native plugin loader/registry; no interactive or LLM session",
                    },
                    indent=2,
                )
            )
        finally:
            os.chdir(original_cwd)


if __name__ == "__main__":
    main()
