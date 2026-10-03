"""One bounded process per method; faults are selected by fixture-owned inputs."""

import json
import os
from pathlib import Path
import sys
import time


directory = Path(sys.argv[1])
preparation_mode = sys.argv[2]
pid = os.getpid()
started = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
pid_temporary = directory / f"{pid}.pid.tmp"
pid_temporary.write_text(f"{pid} {started}")
pid_temporary.replace(directory / f"{pid}.pid")

raw = sys.stdin.buffer.read()
request = json.loads(raw)
method = request["method"]
record = {
    "request": request,
    "cwd": os.getcwd(),
    "environment": dict(os.environ),
    "arguments": sys.argv[3:],
    "input_bytes": len(raw),
}
temporary = directory / f"{request['request_id']}.json.tmp"
temporary.write_text(json.dumps(record, ensure_ascii=False), encoding="utf-8")
temporary.replace(directory / f"{request['request_id']}.json")

reply = {
    "api_version": "aw-provider/v1alpha1",
    "request_id": request["request_id"],
    "status": "ok",
}
scenario = "normal"
if preparation_mode.startswith(f"{method}-"):
    scenario = preparation_mode[len(method) + 1 :]
if preparation_mode == "slow-preparation":
    time.sleep(0.5 if method == "describe" else 3.0)

if method == "describe":
    reply["operations"] = [
        {
            "name": name,
            "events": ["tool.before", "tool.after"],
            "effects": ["observe", "block"],
        }
        for name in ["check", "second", "record"]
    ]
elif method == "invoke":
    native = request["event"]["native"]
    scenario = native["scenario"]
    delay = request["config"].get("delays_ms", {}).get(request["operation"], 0)
    time.sleep(min(delay, 5000) / 1000)
    if scenario == "wait":
        release = directory / f"release-{native.get('release', 'unused')}"
        deadline = time.monotonic() + 5.0
        while not release.exists() and time.monotonic() < deadline:
            time.sleep(0.005)
        if not release.exists():
            sys.exit(90)
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = []
    if scenario in ["block", "observe", "wait"]:
        effect = "observe" if scenario == "wait" else scenario
        reply["effects"] = [{"type": effect, "reason_code": "fixture_policy"}]

if scenario == "wrong-id":
    reply["request_id"] = "uncorrelated-response"
elif scenario == "wrong-digest":
    reply["input_digest"] = "sha256:" + "0" * 64
elif scenario == "extra-effects":
    reply["effects"] = [{"type": "ask"}]
elif scenario == "provider-error":
    reply = {
        "api_version": "aw-provider/v1alpha1",
        "request_id": request["request_id"],
        "status": "error",
        "error_code": "fixture.private_failure",
    }
elif scenario == "invalid-json":
    sys.stdout.buffer.write(b"{invalid-json private-stdout")
    sys.exit(0)
elif scenario in ["stdout-limit", "stderr-limit"]:
    descriptor = 1 if scenario == "stdout-limit" else 2
    os.write(descriptor, b"x" * 8192)

sys.stderr.buffer.write(b"fixture diagnostic\xff")
sys.stdout.write(json.dumps(reply, ensure_ascii=False))
if scenario == "extra-document":
    sys.stdout.write("\n{}")
sys.exit(17 if scenario == "nonzero" else 0)
