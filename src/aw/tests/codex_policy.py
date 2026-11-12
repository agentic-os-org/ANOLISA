"""Public Provider fixture records original results, denials and fresh-call retry."""

import json
from pathlib import Path
import sys

request = json.load(sys.stdin)
mode, path = sys.argv[1:]
reply = {"api_version": request["api_version"], "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "scan_prompt", "events": ["input.submit"], "effects": ["observe", "block"]},
        {"name": "scan", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "retry", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "record", "events": ["tool.after"], "effects": ["observe", "replace_result"]},
    ]
elif request["method"] == "invoke":
    with Path(path).open("a") as stream:
        stream.write(json.dumps(request) + "\n")
    op = request["operation"]
    command = request["event"]["tool"]["input"].get("command", "") if op != "scan_prompt" else ""
    if mode == "offline" or (mode in ("after-failure", "replace-failure") and op == "record"):
        sys.exit(1)
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = [{"type": "observe", "reason_code": "observed"}]
    if op == "scan_prompt" and mode == "prompt-deny":
        reply["effects"] = [{"type": "block", "reason_code": "prompt_denied"}]
    if op == "scan" and (mode == "deny" or "CANDIDATE" in command):
        reply["effects"] = [{"type": "block", "reason_code": "security_denied"}]
    if op == "retry" and mode == "retry" and "ORIGINAL" in command:
        reply["effects"] = [{"type": "block", "reason_code": "tokenless_retry",
                             "text": "Retry with printf CANDIDATE > AW_MUST_NOT_EXIST"}]
    if op == "record" and (mode in ("replace", "replace-apply", "replace-mcp", "replace-parallel")
                           or (mode == "replace-long" and "AW_LONG_DONE" in
                               request["event"]["tool"].get("result_projection", {}).get("text", ""))):
        text = "AW_REDACTED_RESULT"
        if mode == "replace-parallel":
            text = "AW_REDACTED_" + request["event"]["tool"]["call_id"][-1].upper()
        reply["effects"] = [{"type": "replace_result", "text": text}]
print(json.dumps(reply))
