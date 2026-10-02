"""Native command and structured Provider fixtures use only task-owned files."""

import argparse
import json
import os
from pathlib import Path
import signal
import sys
import time

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--protocol", choices=["native", "provider"], required=True)
parser.add_argument("--label", default="policy")
parser.add_argument("--behavior", default="observe")
parser.add_argument("--literal", default="")
args = parser.parse_args()
raw = sys.stdin.buffer.read()
request = json.loads(raw)
(args.root / f"environment-bytes-{args.protocol}.json").write_text(
    json.dumps([(key.hex(), value.hex()) for key, value in sorted(os.environb.items())
                if key.startswith(b"AW_TEST_BYTES")]))
if args.protocol == "provider" and request["method"] == "describe" and os.environ.get("FAKE_PREPARE_BLOCK") == "1":
    (args.root / "preparing.pid").write_text(str(os.getpid()))
    time.sleep(8)
if args.protocol == "provider":
    (args.root / f"provider-{request['request_id']}.json").write_text(json.dumps(request))
    reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
    if request["method"] == "describe":
        reply["operations"] = [
            {"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]},
            {"name": "record", "events": ["tool.after"], "effects": ["observe"]}]
    elif request["method"] == "invoke":
        reply["input_digest"] = request["input_digest"]
        if request["event"]["name"] == "tool.after":
            reply["effects"] = [{"type": "observe", "reason_code": "after_recorded"}]
        elif request["event"]["native"]["scenario"] == "block":
            reply["effects"] = [{"type": "block", "reason_code": "fixture_policy"}]
        else:
            reply["effects"] = []
    print(json.dumps(reply))
    sys.exit(0)

run_id = request["run_id"]
(args.root / f"native-{run_id}-{args.label}.bin").write_bytes(raw)
(args.root / f"argv-{run_id}-{args.label}.json").write_text(json.dumps(args.literal))
if args.behavior == "environment":
    keys = ["QODER_PROJECT_DIR", "CLAUDE_PROJECT_DIR", "FAKE_SHARED_ENV", "FAKE_CALLBACK_ONLY",
            "QODER_HOOK_SOURCE", "QODER_HOOK_VERSION", "QODER_SITE"]
    (args.root / f"environment-{run_id}-{args.label}.json").write_text(
        json.dumps({key: os.environ.get(key) for key in keys}))
elif args.behavior == "barrier":
    (args.root / f"barrier-{run_id}-{args.label}").write_text("ready")
    deadline = time.monotonic() + 4
    while len(list(args.root.glob(f"barrier-{run_id}-*"))) < 2 and time.monotonic() < deadline:
        time.sleep(0.005)
    if len(list(args.root.glob(f"barrier-{run_id}-*"))) != 2:
        sys.exit(92)
    (args.root / f"overlap-{run_id}-{args.label}").write_text("overlap")
elif args.behavior == "ordered":
    if args.label == "second" and not (args.root / f"done-{run_id}-first").exists():
        sys.exit(93)
    time.sleep(0.025)
    (args.root / f"done-{run_id}-{args.label}").write_text("done")
if args.behavior in ["bytes", "block", "signal"]:
    os.write(1, b"native\x00\xff\n")
    os.write(2, b"diagnostic\xfe\n")
else:
    os.write(1, b"{}")
if args.behavior == "signal":
    os.kill(os.getpid(), signal.SIGTERM)
sys.exit(2 if args.behavior == "block" else 0)
