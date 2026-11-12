"""Ordered security/retry/observation fixture using the public Provider protocol."""

import json
import os
from pathlib import Path
import sys

request = json.load(sys.stdin)
reply = {"api_version": request["api_version"], "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "scan_prompt", "events": ["input.submit"], "effects": ["observe", "block"]},
        {"name": "scan", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "retry", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "record", "events": ["tool.after"], "effects": ["observe", "replace_result"]},
    ]
elif request["method"] == "invoke":
    op = request["operation"]
    event = request["event"]
    mode = event["input"]["text"] if op == "scan_prompt" else event["tool"]["input"]["command"]
    with (Path(os.environ["FAKE_ROOT"]) / "policy-calls").open("a") as stream:
        stream.write(op + "\n")
    if mode == "error" and op == "scan":
        sys.exit(1)
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = [{"type": "observe", "reason_code": "seen"}]
    if (op, mode) in (("scan_prompt", "large-prompt-block"), ("scan", "large-block")):
        reply["effects"] = [{"type": "block", "reason_code": "security_denied",
                             "text": "X" * request["config"]["block_bytes"]}]
    elif op == "scan_prompt" and mode == "prompt-block":
        reply["effects"] = [{"type": "block", "reason_code": "prompt_denied"}]
    elif op == "scan" and mode in ("block", "rtk candidate"):
        reply["effects"] = [{"type": "block", "reason_code": "security_denied"}]
    elif op == "retry" and mode == "retry":
        reply["effects"] = [{"type": "block", "reason_code": "tokenless_retry", "text": "Retry with rtk candidate"}]
    elif op == "record" and mode == "replace":
        reply["effects"] = [{"type": "replace_result", "text": "AW_REDACTED_RESULT"}]
print(json.dumps(reply))
