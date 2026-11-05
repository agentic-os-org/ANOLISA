"""One shared Hook exercises task context and failures in both resident SDKs."""
import json
import os
from pathlib import Path
import sys
import time

event = json.load(sys.stdin)
mode = os.environ["AW_TEST_MODE"]
with Path("events.jsonl").open("a") as stream:
    stream.write(json.dumps({"event": event, "tenant": os.environ["AW_TEST_TENANT"]}) + "\n")
if mode == "cancel":
    time.sleep(0.8)
if mode == ("before-error" if event["name"] == "tool.before" else "after-error"):
    raise SystemExit(9)
if event["name"] == "tool.after" and mode in {"result-mutation", "result-control"}:
    if mode == "result-mutation":
        Path("posttool-entered").touch()
        deadline = time.monotonic() + 3
        while not Path("result-mutated").exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        assert Path("result-mutated").exists(), "PostTool mutation handshake timed out"
    if "UNVERIFIED-NEW-RESULT" in json.dumps(event["tool"]["result"]):
        raise SystemExit(9)
deny = mode == "deny"
if os.environ["AW_TEST_SCENARIO"] == "mcp":
    deny = deny and event["tool"]["name"] == "mcp__local__touch"
print(json.dumps(event["name"] == "tool.before" and deny))
