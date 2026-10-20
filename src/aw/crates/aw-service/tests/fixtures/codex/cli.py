#!/usr/bin/env python3
"""Deterministic CLI/inventory fixture; real Codex adoption is a separate suite."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(os.environ["FAKE_ROOT"])
home = Path(os.environ["CODEX_HOME"])
if sys.argv[1:] == ["--version"]:
    print("codex-cli " + os.environ.get("FAKE_CODEX_VERSION", "0.162.1"))
    sys.exit(0)
hooks = json.loads((home / "hooks.json").read_text())["hooks"]
if sys.argv[1:] == ["app-server"]:
    if os.environ.get("FAKE_CODEX_PROBE_HOLD") == "1":
        (root / "codex-probe.pid").write_text(str(os.getpid()))
        time.sleep(30)
        sys.exit(1)
    for line in sys.stdin:
        request = json.loads(line)
        method = request["method"]
        if "id" not in request:
            continue
        result = {}
        if method == "hooks/list":
            result = {"data": [{"cwd": os.getcwd(), "errors": [], "warnings": [], "hooks": [
                {"eventName": {"PreToolUse": "preToolUse", "PostToolUse": "postToolUse"}[event],
                 "command": hook["command"], "enabled": True, "async": False,
                 "matcher": group.get("matcher"), "timeoutSec": hook["timeout"],
                 "trustStatus": os.environ.get("FAKE_CODEX_TRUST", "trusted")}
                for event, groups in hooks.items() for group in groups for hook in group["hooks"]
            ]}]}
        if method == "config/read":
            result = {"config": {"features": {"hooks": os.environ.get("FAKE_CODEX_DISABLED") != "1"}}}
        if method == "hooks/list" and os.environ.get("FAKE_CODEX_INVENTORY"):
            mode = os.environ["FAKE_CODEX_INVENTORY"]
            row = result["data"][0]
            if mode == "missing":
                row["hooks"] = []
            elif mode == "duplicate":
                row["hooks"] *= 2
            elif mode == "hook-disabled":
                row["hooks"][0]["enabled"] = False
            elif mode == "async":
                row["hooks"][0]["async"] = True
            elif mode == "inventory-error":
                row["errors"] = [{"message": "invalid native Hook"}]
        print(json.dumps({"id": request["id"], "result": result}), flush=True)
    sys.exit(0)

(root / "codex-started").write_text(json.dumps(sys.argv[1:]))
mode = sys.argv[-1]
if mode == "hold":
    (root / "codex.pid").write_text(str(os.getpid()))
    time.sleep(30)
    sys.exit(1)
native = {"cwd": os.getcwd(), "session_id": "codex-session", "turn_id": "turn",
          "tool_use_id": "call", "tool_name": "Bash", "tool_input": {"command": mode}, "scenario": mode}
results = []
for event in ("PreToolUse", "PostToolUse"):
    native["hook_event_name"] = event
    if event == "PostToolUse":
        native["tool_response"] = {"stdout": "original output", "stderr": "original error", "exit_code": 7}
    blocked = False
    for group in hooks[event]:
        for hook in group["hooks"]:
            result = subprocess.run(hook["command"], shell=True, input=json.dumps(native),
                                    text=True, capture_output=True, timeout=10)
            results.append({"event": event, "code": result.returncode, "stdout": result.stdout,
                            "stderr": result.stderr})
            blocked |= result.returncode == 2
    if blocked:
        break
    if event == "PreToolUse":
        (root / "codex-tool-executed").write_text("yes")
print(json.dumps({"results": results, "response": native.get("tool_response")}))
