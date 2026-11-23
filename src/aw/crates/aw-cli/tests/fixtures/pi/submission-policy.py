"""Deterministic submission policy and failure injection, with normalized event evidence."""
import json
from pathlib import Path
import sys
import time

root = Path(sys.argv[1])
request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [{"name": "submit", "events": ["input.submit"], "effects": ["observe", "block"]}]
elif request["method"] == "invoke":
    event = request["event"]
    with (root / "submissions.jsonl").open("a") as output:
        output.write(json.dumps(event) + "\n")
    text = event["input"]["text"]
    if text == "error":
        sys.exit(7)
    if text == "invalid":
        print("not JSON")
        sys.exit(0)
    if text in ("timeout", "cancel"):
        (root / "scan-started").write_text("yes")
        time.sleep(10)
    reply.update(input_digest=request["input_digest"], effects=[])
    if text in ("risk", "api_key=synthetic-secret"):
        reply["effects"] = [{"type": "block", "reason_code": "input_risk", "text": "Input blocked by AW policy"}]
print(json.dumps(reply))
