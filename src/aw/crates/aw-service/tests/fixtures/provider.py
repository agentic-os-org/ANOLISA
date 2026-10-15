"""Bounded real Provider fixture; files coordinate calls without timing guesses."""

import json
import os
from pathlib import Path
import sys
import time


directory = Path(sys.argv[1])


def record_pid():
    pid = os.getpid()
    started = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
    temporary = directory / f"{pid}.pid.tmp"
    temporary.write_text(f"{pid} {started}")
    temporary.replace(directory / f"{pid}.pid")


record_pid()
request = json.load(sys.stdin)
method = request["method"]
record = {"request": request, "cwd": os.getcwd(), "environment": dict(os.environ)}
temporary = directory / f"{request['request_id']}.tmp"
temporary.write_text(json.dumps(record, ensure_ascii=False), encoding="utf-8")
temporary.replace(directory / f"{request['request_id']}.call.json")
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if method == "describe":
    reply["operations"] = [
        {"name": operation, "events": ["tool.before", "tool.after"], "effects": ["observe", "block"]}
        for operation in ["check", "second", "record"]
    ]
elif method == "validate_config" and request["config"].get("reject"):
    reply["status"] = "error"
    reply["error_code"] = "fixture.configuration_rejected"
elif method == "invoke":
    scenario = request["event"]["native"]["scenario"]
    if scenario == "descendant":
        child = os.fork()
        if child == 0:
            record_pid()
            (directory / "descendant-ready").write_text("ready")
            time.sleep(5)
            os._exit(0)
        time.sleep(5)
    elif scenario == "wait":
        time.sleep(5)
    elif scenario == "barrier":
        (directory / f"{request['operation']}.ready").write_text("ready")
        deadline = time.monotonic() + 4
        while len(list(directory.glob("*.ready"))) < 2 and time.monotonic() < deadline:
            time.sleep(0.005)
        if len(list(directory.glob("*.ready"))) != 2:
            sys.exit(90)
        (directory / f"{request['operation']}.overlap").write_text("overlap")
    delay = request["config"].get("delays_ms", {}).get(request["operation"], 0)
    time.sleep(min(delay, 5000) / 1000)
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = [] if scenario == "allow" else [{
        "type": "block" if scenario == "block" else "observe", "reason_code": "fixture_policy"
    }]
    if scenario == "malformed":
        sys.stdout.write("private-input-config-output-must-not-be-audited: invalid JSON")
        sys.exit(0)
sys.stderr.write("private-input-config-output-must-not-be-audited")
sys.stdout.write(json.dumps(reply, ensure_ascii=False))
