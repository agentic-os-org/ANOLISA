#!/usr/bin/env python3
"""Deterministic native configuration watcher; real adoption uses pinned acceptance."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

if sys.argv[1:] == ["--version"]:
    print(os.environ.get("FAKE_NATIVE_VERSION", "1.1.73"))
    raise SystemExit(0)
root = Path(os.environ["FAKE_ROOT"])
settings = Path(os.environ["HOME"]) / ".qoder/settings.json"
previous = settings.read_bytes() if settings.exists() else b"{}"
loaded = json.loads(previous)
deadline = time.monotonic() + 40
index = 0


def publish(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value))
    temporary.replace(path)


def event(name):
    payload = {"hook_event_name": name, "cwd": str(root), "session_id": "session",
               "tool_use_id": f"call-{index}", "tool_name": "Bash", "tool_input": {"command": "true"},
               "notification_type": "idle_prompt", "file_path": str(settings), "tool_response": "fixture output"}
    results = []
    for group in loaded.get("hooks", {}).get(name, []):
        if group.get("matcher", "") not in ("", "*", "Bash", "user_settings", "idle_prompt"):
            continue
        for hook in group["hooks"]:
            result = subprocess.run([hook["command"], *hook.get("args", [])],
                                    input=json.dumps(payload).encode(), capture_output=True, timeout=5)
            if any(str(arg).endswith("/detach-confirmation.json") for arg in hook.get("args", [])):
                publish(root / "native-detach.json", {"code": result.returncode, "descriptor": hook["args"][-1]})
            results.append({"code": result.returncode, "stderr": result.stderr.decode()})
    return results


publish(root / "native-pid.json", os.getpid())
while time.monotonic() < deadline:
    current = settings.read_bytes() if settings.exists() else b"{}"
    if current != previous:
        if not os.environ.get("FAKE_SKIP_CONFIRM") or (root / "resume-confirmation").exists():
            event("ConfigChange")
        loaded = json.loads(current)
        previous = current
    command = root / f"attach-command-{index}.json"
    if command.exists():
        action = json.loads(command.read_text())["action"]
        if action == "stop":
            break
        if action == "begin":
            result = event("UserPromptSubmit")
        elif action == "end":
            result = event("Notification")
        elif action == "turn":
            result = event("UserPromptSubmit") + event("PreToolUse")
            if all(item["code"] == 0 for item in result):
                event("PostToolUse")
            event("Notification")
        else:
            raise ValueError(action)
        publish(root / f"attach-result-{index}.json", result)
        index += 1
    time.sleep(0.005)
else:
    raise TimeoutError("attachment fixture lifetime exceeded")
