"""Protocol Provider with a recognizable denial reason for real SDK tool requests."""

import json
import os
from pathlib import Path
import sys

request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "check", "events": ["tool.before", "tool.after"], "effects": ["observe", "block"]}
    ]
elif request["method"] == "invoke":
    event = request["event"]
    with Path(os.environ["QODER_TEST_ROOT"], "events.jsonl").open("a") as stream:
        stream.write(json.dumps(event) + "\n")
    if event["name"] == "tool.after" and "AW_POST_FAIL" in json.dumps(event["tool"]["input"]):
        sys.exit(23)
    blocked = event["name"] == "tool.before" and "AW_DENY" in json.dumps(event["tool"]["input"])
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = (
        [
            {
                "type": "block",
                "reason_code": "native_acceptance",
                "text": "AW_NATIVE_RETRY: use an approved command",
            }
        ]
        if blocked
        else []
    )
print(json.dumps(reply))
