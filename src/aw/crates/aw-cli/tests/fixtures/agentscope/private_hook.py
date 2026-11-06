"""Capture only the public native Hook environment and payload."""
import json
import os
from pathlib import Path
import sys

payload = json.load(sys.stdin)
assert "AW_PRIVATE_KEY" not in os.environ
assert os.environ["PUBLIC"] == "visible"
assert "synthetic-task-private-value" not in json.dumps(payload)
with Path(sys.argv[1]).open("a") as output:
    output.write(json.dumps(payload) + "\n")
