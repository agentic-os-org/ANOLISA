"""Synthetic Post failures for real Worker publication checks."""

import json
import os
from pathlib import Path
import sys
import time

request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [{"name": "check", "events": ["tool.before", "tool.after"], "effects": ["observe"]}]
elif request["method"] == "invoke":
    event = request["event"]
    with Path(os.environ["QODER_TEST_ROOT"], "events.jsonl").open("a") as stream:
        stream.write(json.dumps(event) + "\n")
    if event["name"] == "tool.after":
        mode = os.environ["QODER_POST_CASE"]
        if mode == "timeout":
            time.sleep(60)
        if mode in ("failure", "failure-error", "report"):
            sys.exit(23)
    reply.update(input_digest=request["input_digest"], effects=[])
print(json.dumps(reply))
