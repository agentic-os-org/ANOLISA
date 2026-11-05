"""Deterministic submission Provider, including protocol and timeout faults."""
import json
import sys
import time
from pathlib import Path

root = Path(sys.argv[1])
request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": name, "events": ["input.submit"], "effects": ["observe", "block"]}
        for name in ["scan_prompt", "scan_user_input"]
    ] + [{"name": "tool", "events": ["tool.before", "tool.after"], "effects": ["observe", "block"]}]
elif request["method"] == "invoke":
    event = request["event"]
    with (root / "scans.jsonl").open("a") as out:
        out.write(json.dumps(request) + "\n")
    if event["name"] != "input.submit":
        if event["name"] == "tool.after" and event["tool"]["input"]["mode"] == "strict":
            sys.exit(1)
        reply.update(input_digest=request["input_digest"], effects=[])
        print(json.dumps(reply))
        sys.exit(0)
    text = event["input"]["text"]
    if text == "error":
        sys.exit(1)
    if text == "invalid":
        print("invalid-json")
        sys.exit(0)
    if text in {"timeout", "cancel"}:
        (root / "scan-entered").touch()
        time.sleep(30)
    blocked = (request["operation"] == "scan_prompt" and text == "risk") or (
        request["operation"] == "scan_user_input" and text == "pii"
    )
    reply.update(input_digest=request["input_digest"], effects=[{
        "type": "block" if blocked else "observe", "reason_code": "fixture_scan",
    }])
print(json.dumps(reply))
