"""An invoke-only user command with task-owned input and process evidence."""

import json
import os
from pathlib import Path
import sys
import time

directory = Path(sys.argv[1])
pid = os.getpid()
started = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
(directory / f"{pid}.pid").write_text(f"{pid} {started}")
event = json.load(sys.stdin)
assert not {"method", "request_id", "input_digest", "allowed_effects"}.intersection(event)
(directory / "hook-event.json").write_text(json.dumps({"request": event, "argv": sys.argv[2:]}))


def contains_12345(value):
    if isinstance(value, str):
        return "12345" in value
    if isinstance(value, list):
        return any(contains_12345(item) for item in value)
    if isinstance(value, dict):
        return any(contains_12345(item) for item in value.values())
    return False


scenario = event["native"]["scenario"]
if scenario == "wait":
    time.sleep(5)
elif scenario in {"null", '"true"', "true false", "X" * 33}:
    print(scenario)
    sys.exit(0)
print(json.dumps(contains_12345(event["tool"]["input"])))
sys.exit(7 if scenario == "nonzero" else 0)
