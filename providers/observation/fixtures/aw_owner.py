"""Real aw instance owner with empty native routes, then exec a synthetic TLS worker.

This uses the existing qoder-sdk/sdk-worker lifecycle contract. It tests ownership
and Scope only; no real Qoder SDK or Native Hook capability is being certified.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

aw, socket, destination, count, *worker = sys.argv[1:]
cwd = str(Path(destination).parent)


def command(action, value=None, extra=()):
    completed = subprocess.run([aw, action, "--socket", socket, *extra],
                               input=json.dumps(value), capture_output=True, text=True, timeout=10)
    if completed.returncode:
        raise RuntimeError(f"{action} failed: {completed.stdout} {completed.stderr}")
    return json.loads(completed.stdout)


connection = command("instance", extra=("--connect",))
identity = connection["identity"]
start = Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(") ", 1)[1].split()[19]
owner = {"pid": os.getpid(), "start_ticks": start}


def lifecycle(value):
    return command("instance", {"api_version": "aw-instance/v1", "identity": identity, "command": value})


registrations = []
for index in range(int(count)):
    registered = lifecycle({"action": "register", "key": {"request_id": f"poc-{index}", "owner": owner},
                            "target": "fixture", "cwd": cwd, "environment": {},
                            "capabilities": {"adapter": "qoder-sdk", "version": "1.0.56",
                                             "entrypoint": "sdk-worker", "events": {}}})
    prepared = registered["result"]
    assert prepared["state"] == "pending_ready", registered
    # No Hook route is configured or advertised. Never acknowledge nonempty routes.
    assert not prepared["routes"]["events"], prepared["routes"]
    ready = lifecycle({"action": "ready", "handle": prepared["handle"],
                       "policy_revision": prepared["policy_revision"]})
    assert ready["result"]["state"] == "ready", ready
    root = prepared["handle"]["binding"]["instance_id"]
    scope = command("request", {"method": "open_scope", "parent_id": root, "pid": os.getpid(),
                                "cwd": cwd, "environment": {}})["result"]
    scope_id = scope["instance_id"]
    policy = command("request", {"method": "scope_policy", "instance_id": scope_id})
    acknowledged = command("request", {"method": "acknowledge_scope", "instance_id": scope_id})
    assert acknowledged["result"]["acknowledged"]
    registrations.append({"registration": registered, "ready": ready, "scope": scope,
                          "scope_policy": policy, "acknowledged": acknowledged})

with open(destination, "x") as output:
    os.chmod(destination, 0o600)
    json.dump({"schema_version": "aw.ptrace-fixture-registration/v0", "owner": owner,
               "identity": identity, "valid_from_ns": str(time.monotonic_ns()),
               "registrations": registrations,
               "status": command("request", {"method": "status"})["result"]}, output)
# exec preserves PID/start_ticks and AW ownership; the importer pins the final executable.
os.execv(worker[0], worker)
