"""Application-owned process fixture for the independent CLI contract.

Read one setup JSON line (aw, socket, cwd, adapter), then action JSON lines.
Each reply is one JSON line. No aw run parent or native SDK is required.
The fixture installs a callable simulated tool boundary before acknowledging Ready.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any

setup = json.loads(sys.stdin.readline())
aw = setup["aw"]
socket = setup["socket"]
cwd = setup["cwd"]
adapter = setup.get("adapter", "qoder-sdk")
start = Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(") ", 1)[1].split()[19]
owner = {"pid": os.getpid(), "start_ticks": start}
registrations: dict[str, Any] = {}
bindings: dict[str, Path] = {}
sequence = 0


def command(argv: list[str], value: Any = None) -> dict[str, Any]:
    completed = subprocess.run([aw, *argv], input=json.dumps(value), text=True,
                               capture_output=True, timeout=12)
    return {"exit": completed.returncode, "stdout": completed.stdout,
            "stderr": completed.stderr}


def lifecycle(value: dict[str, Any], identity: Any = None) -> dict[str, Any]:
    result = command(["instance", "--socket", socket, "--timeout-ms", "10000"],
                     {"api_version": "aw-instance/v1", "identity": identity or service_identity, "command": value})
    return json.loads(result["stdout"])


def raw(value: dict[str, Any]) -> dict[str, Any]:
    result = command(["request", "--socket", socket, "--timeout-ms", "10000"], value)
    if result["exit"]:
        return result
    return json.loads(result["stdout"])["result"]


def scope(tag: str, action: str, value: Any, *extra: str) -> dict[str, Any]:
    return command(["scope", "--adapter", adapter, "--binding", str(bindings[tag]),
                    "--action", action, *extra], value)


def install(tag: str, value: dict[str, Any]) -> None:
    path = Path(cwd) / f"independent-{os.getpid()}-{tag}.json"
    with path.open("w") as output:
        os.chmod(path, 0o600)
        json.dump({"adapter": adapter, "binding": value["handle"]["binding"],
                   "socket": socket, "cwd": cwd, "events": value["routes"]["events"]}, output)
    bindings[tag] = path


def tool(tag: str, scenario: str) -> dict[str, Any]:
    global sequence
    sequence += 1
    opened = scope(tag, "open", {"pid": os.getpid(), "cwd": cwd,
                                  "environment": {"AW_SERVICE_MARKER": tag}})
    if opened["exit"]:
        return opened
    task = json.loads(opened["stdout"])
    try:
        acknowledged = scope(tag, "acknowledge", task)
        assert acknowledged["exit"] == 0, acknowledged
        call_id = f"call-{sequence}"
        if adapter == "qoder-sdk":
            native = {"hook_event_name": "PreToolUse", "cwd": cwd,
                      "session_id": "fixture", "tool_use_id": call_id,
                      "tool_name": "Bash", "tool_input": {"command": "true"},
                      "scenario": scenario}
        else:
            native = {"event": "tool.before", "session_id": "fixture",
                      "tool_call": {"id": call_id, "name": "exec",
                                    "input": json.dumps({"command": "true"})},
                      "scenario": scenario}
        result = scope(tag, "invoke", {"scope": task, "native_input": json.dumps(native)},
                       "--event", "tool.before", "--step", "check", "--on-error", "block")
        # The actual fixture side effect is behind the callback's native veto.
        marker = Path(cwd) / f"effect-{os.getpid()}-{sequence}"
        if result["exit"] == 0:
            marker.write_text("executed")
        result.update(executed=marker.exists(), task=task)
        return result
    finally:
        closed = scope(tag, "close", task)
        assert closed["exit"] == 0, closed


connected = command(["instance", "--socket", socket, "--connect"])
connection = json.loads(connected["stdout"])
assert connection["error"] is None, connection
service_identity = connection["identity"]
print(json.dumps({"owner": owner, "identity": service_identity}), flush=True)
try:
    for line in sys.stdin:
        request = json.loads(line)
        action = request["action"]
        tag = request.get("tag", "first")
        if action == "register":
            version, entrypoint = ("1.0.56", "sdk-worker") if adapter == "qoder-sdk" else ("2.2.2.post1", "app")
            payload = {"action": "register", "key": {"request_id": tag, "owner": owner},
                       "target": "target", "cwd": cwd,
                       "environment": {"AW_SERVICE_MARKER": tag},
                       "capabilities": {"adapter": adapter, "version": version,
                                        "entrypoint": entrypoint,
                                        "events": {"tool.before": ["observe", "block"],
                                                   "tool.after": ["observe"]}}}
            result = lifecycle(payload)
            if result["error"] is None and result["result"]["state"] == "pending_ready":
                registrations[tag] = result["result"]
                install(tag, result["result"])
        elif action == "query":
            result = lifecycle({"action": "query", "key": {"request_id": tag, "owner": owner}})
        elif action in ("ready", "release"):
            payload = {"action": action, "handle": registrations[tag]["handle"]}
            if action == "ready":
                assert bindings[tag].is_file(), "the callable boundary must be installed first"
                payload["policy_revision"] = registrations[tag]["policy_revision"]
            result = lifecycle(payload)
        elif action == "call":
            result = tool(tag, request.get("scenario", "allow"))
        elif action == "raw":
            result = raw(request["operation"])
        elif action == "lifecycle":
            result = lifecycle(request["command"], request.get("identity"))
        elif action == "exit":
            print(json.dumps({"exiting": True}), flush=True)
            break
        else:
            raise ValueError(action)
        print(json.dumps(result), flush=True)
finally:
    for path in bindings.values():
        path.unlink(missing_ok=True)
