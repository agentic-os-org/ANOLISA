"""Deterministic command and Provider for OpenClaw launcher transport tests."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import time

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--label", required=True)
parser.add_argument("--behavior", default="observe")
args = parser.parse_args()
raw = sys.stdin.buffer.read()
request = json.loads(raw)
if "method" in request:
    (args.root / ("provider-" + request["request_id"] + ".json")).write_text(json.dumps(request))
    (args.root / ("provider-environment-" + request["method"] + ".json")).write_text(json.dumps(os.environ.get("AW_OPENCLAW_NATIVE_ENV_FIXTURE")))
    reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
    if request["method"] == "describe":
        reply["operations"] = [{"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]},
                               {"name": "record", "events": ["tool.after"], "effects": ["observe"]},
                               {"name": "check-final", "events": ["security.violation"], "effects": ["observe", "block"]}]
    elif request["method"] == "invoke":
        if args.behavior == "final-error" and request["operation"] == "check-final":
            sys.exit(3)
        reply["input_digest"] = request["input_digest"]
        command = request["event"]["tool"]["input"].get("command", "")
        if request["operation"] == "check-final":
            blocked = "AW_FINAL_GATE_SECRET_MARKER" in json.dumps(request["event"]["tool"]["result"])
            reply["effects"] = [{"type": "block" if blocked else "observe", "reason_code": "fixture"}]
        else:
            reply["effects"] = [{"type": "block", "reason_code": "fixture"}] if request["event"]["name"] == "tool.before" and "DENY" in command else []
        if args.behavior == "retry" and command == "original":
            corrected = {"command": "rtk git status --short", "cwd": "/work tree"}
            reply["effects"] = [{"type": "block", "reason_code": "tokenless_retry", "text":
                                "[tokenless:rewrite] Retry this tool with arguments: " + json.dumps(corrected)}]
        elif args.behavior == "retry-security" and command == "rtk git status --short":
            reply["effects"] = [{"type": "block", "reason_code": "security_denied", "text": "Corrected call denied by security"}]
    print(json.dumps(reply))
    sys.exit(0)

with (args.root / "raw-calls.jsonl").open("a") as output:
    output.write(json.dumps({"label": args.label, "hook": request["hook"], "digest": hashlib.sha256(raw).hexdigest(), "pid": os.getpid(),
                            "native_environment": os.environ.get("AW_OPENCLAW_NATIVE_ENV_FIXTURE"),
                            "result": request["event"].get("result") if request["hook"] in ("after_tool_call", "final_tool_result")
                            and os.environ.get("AW_FINAL_GATE_ORDER_PROBE") == "1" else None}) + "\n")
if args.behavior == "sleep":
    time.sleep(0.4)
elif args.behavior == "barrier":
    (args.root / f"barrier-{args.label}").write_text("started")
    deadline = time.monotonic() + 3
    while len(list(args.root.glob("barrier-*"))) < 2 and time.monotonic() < deadline:
        time.sleep(0.005)
    if len(list(args.root.glob("barrier-*"))) < 2:
        sys.exit(3)
elif args.behavior == "error":
    sys.exit(3)
print(json.dumps({"block": True, "blockReason": "native policy"}) if args.behavior == "block" else "{}")
