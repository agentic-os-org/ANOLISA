"""Structured AW Provider for the real Hermes Gateway result adoption fixture."""

import json
from pathlib import Path
import sys


request = json.load(sys.stdin)
root = Path(sys.argv[1])
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"],
         "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "record", "events": ["tool.after"], "effects": ["observe", "replace_result"]},
    ]
elif request["method"] == "invoke":
    event = request["event"]
    with (root / "provider.jsonl").open("a") as stream:
        stream.write(json.dumps(event) + "\n")
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = (
        [{"type": "replace_result", "text": "MODEL_VISIBLE_REPLACEMENT"}]
        if event["name"] == "tool.after" and "result_projection" in event["tool"]
        else []
    )
print(json.dumps(reply))
