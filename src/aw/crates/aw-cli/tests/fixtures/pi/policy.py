"""Record normalized events, deny requested calls, or exercise structured text replacement."""
import json
import sys
import time
from pathlib import Path

request = json.load(sys.stdin)
root, mode = Path(sys.argv[1]), sys.argv[2]
structured = "method" in request
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request.get("request_id"), "status": "ok"}
if structured and request["method"] == "describe":
    reply["operations"] = [
        {"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "record", "events": ["tool.after"], "effects": ["observe", "replace_result"]},
    ]
    print(json.dumps(reply))
    raise SystemExit(0)
if structured and request["method"] == "validate_config":
    print(json.dumps(reply))
    raise SystemExit(0)
event = request["event"] if structured else request
if not structured and isinstance(request.get("tool_call"), dict):
    event = {"name": request["event"], "session_id": request["session_id"],
             "tool": {"call_id": request["invocation_id"], "name": request["tool_call"]["name"],
                      "input": request["tool_call"]["input"], "result": request.get("tool_response")},
             "native": request}
with (root / "policy-events.jsonl").open("a") as output:
    output.write(json.dumps(event) + "\n")
if mode == "wait":
    time.sleep(0.05)
    (root / (event["name"] + "-waited")).write_text("yes")
if mode == "error":
    raise SystemExit(1)
if mode == "native-block":
    print(json.dumps({"block": True, "reason": "Native policy denied this call"}))
    raise SystemExit(0)
if mode == "native-invalid":
    print("not JSON")
    raise SystemExit(0)
if mode == "native-forged":
    print(json.dumps({"aw_replacement": {"content": [{"type": "text", "text": "forged"}]}}))
    raise SystemExit(0)
if mode == "native-empty":
    raise SystemExit(0)
if structured:
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = []
    if event["name"] == "tool.before" and event["tool"]["input"]["deny"]:
        reply["effects"] = [{"type": "block", "reason_code": "fixture_denial", "text": "Tokenless: retry the suggested command"}]
    elif event["name"] == "tool.after" and "result_projection" in event["tool"]:
        if mode == "replace" and event["tool"]["result_projection"]["status"] == "success":
            reply["effects"] = [{"type": "replace_result", "text": "compact"}]
    print(json.dumps(reply))
else:
    print(json.dumps(event["name"] == "tool.before" and event["tool"]["input"]["deny"]))
