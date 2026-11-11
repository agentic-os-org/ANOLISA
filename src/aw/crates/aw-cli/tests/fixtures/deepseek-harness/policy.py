"""Deterministic public Provider contract used by callback and native tests."""

import json
import sys
from pathlib import Path

root, mode = Path(sys.argv[1]), sys.argv[2]
request = json.load(sys.stdin)
reply = {"api_version": request["api_version"], "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "scan", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "record", "events": ["tool.after"], "effects": ["observe", "replace_result"]},
    ]
elif request["method"] == "invoke":
    event = request["event"]
    with (root / "policy.jsonl").open("a") as stream:
        stream.write(json.dumps(event) + "\n")
    reply.update(input_digest=request["input_digest"], effects=[])
    if event["name"] == "tool.before":
        if mode == "error":
            reply = {**reply, "status": "error", "error_code": "scanner_unavailable"}
            del reply["effects"]
            del reply["input_digest"]
        elif event["tool"]["input"].get("command") == "forbidden":
            reply["effects"] = [{"type": "block", "reason_code": "security_denied"}]
    elif mode == "replace":
        projection = event["tool"].get("result_projection", {})
        if projection.get("status") == "success":
            reply["effects"] = [{"type": "replace_result", "text": "compact"}]
print(json.dumps(reply))
