#!/usr/bin/env python3
"""Native-hook host fixture; this is not evidence of real Qoder adoption."""

from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

if sys.argv[1:] == ["--version"]:
    print(os.environ.get("FAKE_VERSION", "1.1.64"))
    sys.exit(0)

root = Path(os.environ["FAKE_ROOT"])
(root / "environment-bytes-agent.json").write_text(
    json.dumps([(key.hex(), value.hex()) for key, value in sorted(os.environb.items())
                if key.startswith(b"AW_TEST_BYTES")]))
arguments = sys.argv[1:]
separator = arguments.index("--") if "--" in arguments else len(arguments)
query = arguments[separator + 1:]
arguments = arguments[:separator]


def option(name, default=None):
    return arguments[arguments.index(name) + 1] if name in arguments else default


run_id = option("--run-id", "one")
scenario = option("--scenario", "allow")
settings_path = Path(option("--settings"))
settings = json.loads(settings_path.read_text())
(root / f"settings-{run_id}.json").write_text(json.dumps(settings))
(root / f"started-{run_id}").write_text(str(os.getpid()))
results = []


def invoke(hook, payload):
    command = [hook["command"], *hook.get("args", [])]
    environment = {**os.environ, "QODER_PROJECT_DIR": os.getcwd(),
                   "CLAUDE_PROJECT_DIR": os.getcwd(), "FAKE_CALLBACK_ONLY": "callback",
                   "FAKE_SHARED_ENV": "callback", "QODER_SITE": "GLOBAL",
                   "QODER_HOOK_SOURCE": "qoderwork" if os.environ.get(
                       "QODER_WORK_INTEGRATION_MODE") == "1" else "cli",
                   "QODER_HOOK_VERSION": os.environ.get("QODER_CLIENT_VERSION") or "1.1.64"}
    completed = subprocess.run(command, input=payload, capture_output=True, timeout=10,
                               check=False, env=environment)
    arguments = hook.get("args", [])
    step = arguments[arguments.index("--step") + 1] if "--step" in arguments else "existing"
    return {"step": step, "status": completed.returncode,
            "stdout": completed.stdout.hex(), "stderr": completed.stderr.hex()}


def hooks(name):
    payload = {"hook_event_name": name, "cwd": os.getcwd(), "session_id": "fixture-session",
               "tool_use_id": "fixture-call", "tool_name": "custom_tool",
               "tool_input": {"command": option("--tool-command", "echo fixture"), "ratio": 0.125, "标签": "测试"},
               "scenario": scenario, "run_id": run_id}
    if name == "PostToolUse":
        payload["tool_response"] = {"result": "native-output"}
    encoded = (json.dumps(payload, ensure_ascii=False, indent=1) + "\n").encode()
    (root / f"payload-{run_id}-{name}.bin").write_bytes(encoded)
    event_results = []
    for group in settings.get("hooks", {}).get(name, []):
        selected = group.get("hooks", [])
        if group.get("sequential"):
            group_results = [invoke(hook, encoded) for hook in selected]
        else:
            with ThreadPoolExecutor(max_workers=max(1, len(selected))) as executor:
                group_results = list(executor.map(lambda hook: invoke(hook, encoded), selected))
        event_results.extend(group_results)
    results.append({"event": name, "hooks": event_results})
    return any(result["status"] == 2 for result in event_results)


blocked = hooks("PreToolUse")
if not blocked:
    (root / f"tool-{run_id}").write_text("executed")
    hooks("PostToolUse")
if "--hold" in arguments:
    (root / f"ready-{run_id}").write_text("waiting")
    deadline = time.monotonic() + 5
    while not (root / f"release-{run_id}").exists() and time.monotonic() < deadline:
        time.sleep(0.005)
    if not (root / f"release-{run_id}").exists():
        sys.exit(91)
print(json.dumps({"events": results, "tool_executed": not blocked, "query": query}), flush=True)
if "--agent-signal" in arguments:
    os.kill(os.getpid(), signal.SIGTERM)
sys.exit(int(option("--agent-exit", "0")))
