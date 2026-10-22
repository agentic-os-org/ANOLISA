"""Deterministic AW policy for real SDK tool requests."""

import json
import os
from pathlib import Path
import sys

request = json.load(sys.stdin)
with Path(os.environ["QODER_TEST_ROOT"], "events.jsonl").open("a") as stream:
    stream.write(json.dumps(request) + "\n")
print(
    json.dumps(
        request["name"] == "tool.before" and "AW_DENY" in json.dumps(request["tool"]["input"])
    )
)
