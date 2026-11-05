"""Strict post-tool fixture hook: timeout, failure and invalid stdout."""

import json
import sys
import time
from pathlib import Path

root = Path(sys.argv[1])
label = sys.argv[2]
event = json.load(sys.stdin)
mode = event["tool"]["input"]["mode"]
with (root / "strict-callbacks.jsonl").open("a") as output:
    output.write(
        json.dumps(
            {
                "label": label,
                "mode": mode,
                "original": f"complete-{mode}" in json.dumps(event["tool"]["result"]),
            }
        )
        + "\n"
    )
if label == "strict":
    if mode == "timeout":
        time.sleep(30)
    if mode == "error":
        sys.exit(1)
    if mode == "invalid":
        print("not-json")
        sys.exit(0)
print(json.dumps(False))
