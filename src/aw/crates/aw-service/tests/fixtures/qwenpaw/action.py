"""Task-owned raw-command and structured-Provider fixtures."""

import json
import os
import signal
from pathlib import Path
import sys
import time

root, protocol, label = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
raw = sys.stdin.buffer.read()
request = json.loads(raw)
if protocol == "provider":
    reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
    if request["method"] == "describe":
        reply["operations"] = [
            {"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]},
            {"name": "record", "events": ["tool.after"], "effects": ["observe"]},
        ]
    elif request["method"] == "invoke":
        reply["input_digest"] = request["input_digest"]
        event = request["event"]
        (root / (event["name"] + "-provider.json")).write_text(json.dumps(event))
        (root / (event["name"] + "-provider-env.json")).write_text(json.dumps({
            key: os.environ.get(key) for key in ["PROFILE_SECRET", "REMOVED_BY_NATIVE_HOST"]
        }))
        reply["effects"] = [{"type": "block", "reason_code": "fixture_denial"}] if (
            event["name"] == "tool.before" and event["tool"]["input"]["deny"]
        ) else []
    print(json.dumps(reply))
else:
    if label == "environment":
        (root / "native-environment.json").write_text(json.dumps({
            key: os.environ.get(key) for key in ["PROFILE_SECRET", "REMOVED_BY_NATIVE_HOST"]
        }))
    (root / (label + "-raw.json")).write_bytes(raw)
    (root / (label + "-argv.json")).write_text(json.dumps(sys.argv[4:]))
    with (root / "order.jsonl").open("a") as output:
        output.write(json.dumps(label) + "\n")
    if label.startswith("budget-"):
        time.sleep(.35)
        (root / (label + "-completed")).write_text("yes")
    if label == "barrier":
        identifier = request["tool_call"]["id"]
        (root / ("barrier-" + identifier)).write_text("entered")
        deadline = time.monotonic() + 2
        while len(list(root.glob("barrier-call-*"))) < 2 and time.monotonic() < deadline:
            time.sleep(.005)
        if len(list(root.glob("barrier-call-*"))) != 2:
            raise SystemExit(2)
        (root / ("overlap-" + identifier)).write_text("yes")
    if label == "ask":
        print('{"action":"ask"}')
    elif label == "killed":
        os.kill(os.getpid(), signal.SIGTERM)
    elif label == "block":
        sys.stderr.write("native block")
        raise SystemExit(2)
    else:
        sys.stdout.buffer.write(b"native arbitrary bytes\x00\xff")
