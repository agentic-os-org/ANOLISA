"""Boolean Hook fixture; record callback adoption and wait ordering."""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

root = Path(sys.argv[1])
label = sys.argv[2]
event = json.load(sys.stdin)
uid = event["tool"]["input"]["uid"]
mode = event["tool"]["input"]["mode"]
assert uid == event["session_id"]
if event["name"] == "tool.before":
    if label == "before-first":
        (root / f"entered-{uid}-{mode}").write_text("entered")
        deadline = time.monotonic() + 2
        while len(list(root.glob("entered-*"))) < 2:
            assert time.monotonic() < deadline, "native concurrency lost"
            time.sleep(0.005)
    time.sleep(0.025)
    if label == "before-last":
        (root / f"before-complete-{uid}-{mode}").write_text("complete")
else:
    time.sleep(0.025)
with (root / "callbacks.jsonl").open("a") as output:
    output.write(json.dumps({"label": label, "event": event}) + "\n")
if label == "before-first" and mode == "hook-error":
    sys.exit(1)
print(json.dumps(label != "before-last" and (label != "before-first" or mode == "deny")))
