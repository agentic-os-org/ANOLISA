"""Structured Provider failure after a registered resident tool invocation."""

import json
import os
from pathlib import Path
import sys

root = Path(sys.argv[1])
request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "record", "events": ["tool.after"], "effects": ["observe"]},
    ]
elif request["method"] == "invoke":
    event = request["event"]
    with (root / "provider-events.jsonl").open("a") as output:
        output.write(json.dumps(event) + "\n")
    if event["name"] == "tool.after" and os.environ.get("FAKE_QWENPAW_AFTER_FAILURE"):
        raise SystemExit(1)
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = [{"type": "block", "reason_code": "fixture_denial", "text": '[tokenless:rewrite] Retry this tool with arguments: {"command":"rtk git status"}'}] if (
        event["name"] == "tool.before" and event["tool"]["input"]["deny"]
    ) else []
print(json.dumps(reply))
