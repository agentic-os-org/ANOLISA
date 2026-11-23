"""Finish input rejection after the old native lifecycle timeout has elapsed."""
import json
from pathlib import Path
import sys
import time

request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [{"name": "submit", "events": ["input.submit"], "effects": ["observe", "block"]}]
elif request["method"] == "invoke":
    time.sleep(4.2)
    Path(sys.argv[1], "delayed-submission-finished").write_text("done")
    reply.update(input_digest=request["input_digest"], effects=[{
        "type": "block", "reason_code": "input_risk", "text": "Input blocked after bounded checking",
    }])
print(json.dumps(reply))
