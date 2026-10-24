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
print(json.dumps(event["name"] == "tool.before" and mode == "deny"))
